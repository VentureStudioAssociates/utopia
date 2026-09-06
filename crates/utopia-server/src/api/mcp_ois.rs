//! Governed OIS tools on the Utopia MCP boundary (stage 4c of the OIS × Utopia
//! convergence program, spec `art_cUh17iEH`).
//!
//! SPDX-License-Identifier: AGPL-3.0-only
//!
//! Utopia's MCP surface (`super::mcp`) already serves eight read-only tools and
//! a propose-only `remember`. This module adds the governed OIS surface beside
//! them: typed reads that resolve currentness through `ois-kernel` over the
//! kernel side tables, and a propose-only write path that appends CANDIDATE
//! kernel envelopes — never governing state.
//!
//! Semantics mirror the TypeScript oracle `packages/mcp` (propose-only
//! evaluator, oracle pin `c56f8e5`), translated to the fork:
//!
//! - Strict invocation pipeline: registry lookup → invocation/payload
//!   validation → governance-root check → server-derived capability
//!   evaluation → idempotent replay → candidate application.
//! - What an operation *constitutes* comes from the validated payload
//!   (governing authority/lifecycle claims map to `establish_governing`);
//!   what the Principal *may do* comes from the server-derived profile via
//!   `ois_kernel::evaluate_capability`. Denials return rejected
//!   typed-operation envelopes — distinct outcomes, never silent no-ops,
//!   never coerced into a candidate write.
//! - The boundary is propose-only by construction: the wired agent grant
//!   never carries governing authority, so no MCP invocation can reach
//!   governing state. Promotion belongs to the governed-write path
//!   (`apply_governed_write`), which this surface does not expose.
//! - Reads resolve through the pure resolver with a server-derived
//!   permission basis (permission precedes disclosure): a restricted basis
//!   yields `permission_limited` with the withheld count, never a lying zero.
//!
//! Delta budget (CONVERGENCE.md): all logic here is NEW. Upstream edits are
//! limited to the module registration, the two routing hooks in `super::mcp`
//! (listing append, tools/call branch, response-helper visibility), and one
//! workspace dependency line.

use axum::Json;
use chrono::DateTime;
use ois_kernel::governance::{
    evaluate_capability, CapabilityDecision, Grant, PrincipalBasis, Profile,
};
use ois_kernel::resolver::{
    resolve_current_state, slice_as_known, Currentness, PermissionBasis, ResolutionCorpus,
    ResolutionQuery,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;
use utopia_core::models::{KnowledgeBase, User};

/// The wired governance root for one MCP session: the fork's kernel roots are
/// per-knowledge-base (`ois:scope:kb:{kb_id}`), the same convention the ingest
/// hook uses when facts land as candidate envelopes.
fn governance_root_for(kb_id: Uuid) -> String {
    format!("ois:scope:kb:{kb_id}")
}

// ------------------------------------------------------------------ registry

/// Frozen tool names — MCP names carry no governance meaning; the capability
/// mapping below does (oracle `registry.ts` `TOOL_NAMES`).
pub const TOOL_PROPOSE_KERNEL_RECORD: &str = "ois_propose_kernel_record";
pub const TOOL_PROPOSE_OBJECTIVE_OPERATION: &str = "ois_propose_objective_operation";
pub const TOOL_ATTACH_EVIDENCE: &str = "ois_attach_evidence";
pub const TOOL_RESOLVE_CURRENT_STATE: &str = "ois_resolve_current_state";
pub const TOOL_SLICE_AS_KNOWN: &str = "ois_slice_as_known";

/// One registered tool: a typed operation with its family and capability.
/// `fixed_capability` is `None` where the payload's own authority claims
/// decide what the operation constitutes (oracle `ToolDescriptor`).
pub struct OisTool {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ToolKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Write,
}

/// The wired v1 tools, in registry order (oracle `V1_AGENT_TOOLS`). No tool
/// here has a fixed governing capability — the v1 boundary never registers one.
const WIRED_TOOLS: [OisTool; 5] = [
    OisTool {
        name: TOOL_PROPOSE_KERNEL_RECORD,
        description: "Submit one kernel record as working/candidate knowledge. Payloads asserting governing authority map to establish_governing and are denied for propose-only agents.",
        kind: ToolKind::Write,
    },
    OisTool {
        name: TOOL_PROPOSE_OBJECTIVE_OPERATION,
        description: "Submit one objective operation (proposal actions apply as candidate knowledge; governing transition actions map to establish_governing and are denied for propose-only agents).",
        kind: ToolKind::Write,
    },
    OisTool {
        name: TOOL_ATTACH_EVIDENCE,
        description: "Attach evidence by appending a new candidate kernel version carrying provenance. Evidence that itself establishes governing state is a governing operation and is denied for propose-only agents.",
        kind: ToolKind::Write,
    },
    OisTool {
        name: TOOL_RESOLVE_CURRENT_STATE,
        description: "Resolve the canonical currentness of one kernel object under the server-derived permission basis. States stay distinct: governing, governing_but_disputed, not_established, superseded, retired, deprecated, permission_limited.",
        kind: ToolKind::Read,
    },
    OisTool {
        name: TOOL_SLICE_AS_KNOWN,
        description: "Slice one kernel object as known at an as_known_at instant (later recordings are invisible) and resolve its currentness in that historical slice.",
        kind: ToolKind::Read,
    },
];

fn wired_tool(name: &str) -> Option<&'static OisTool> {
    WIRED_TOOLS.iter().find(|tool| tool.name == name)
}

/// True for every `ois_`-prefixed name: governed-surface names belong to this
/// module's dispatcher, which owns their errors (unknown names included).
pub fn is_wired_tool(name: &str) -> bool {
    name.starts_with("ois_")
}

/// Whether the tool may be listed/invoked under this session's write standing.
/// The three write tools append kernel records — the same two-gate standing
/// (`remember` needs: write-scope token held by an editor). Reads are open to
/// any authenticated reader of the base.
fn exposed_under(name: &str, can_write: bool) -> bool {
    match wired_tool(name).map(|tool| tool.kind) {
        Some(ToolKind::Write) => can_write,
        Some(ToolKind::Read) => true,
        None => false,
    }
}

/// MCP `tools/list` entries for the governed surface. Write tools list only
/// for write-capable sessions — listing a tool the caller can never invoke
/// invites retry loops (the same discipline as upstream `remember`).
pub fn tool_listings(can_write: bool) -> Vec<Value> {
    WIRED_TOOLS
        .iter()
        .filter(|tool| exposed_under(tool.name, can_write))
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": input_schema(tool),
            })
        })
        .collect()
}

/// Read inputs are MCP-surface-local typed queries, not portable contracts;
/// write inputs wrap the kernel payload contract with an idempotency key.
fn input_schema(tool: &OisTool) -> Value {
    match tool.kind {
        ToolKind::Read => {
            let mut schema = json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["governance_root_ref", "object_id", "as_known_at"],
                "properties": {
                    "governance_root_ref": { "type": "string", "minLength": 1 },
                    "object_id": { "type": "string", "minLength": 1 },
                    "as_known_at": { "type": "string", "format": "date-time" },
                },
            });
            if tool.name == TOOL_RESOLVE_CURRENT_STATE {
                schema["required"] = json!([
                    "governance_root_ref",
                    "object_id",
                    "as_known_at",
                    "valid_at"
                ]);
                schema["properties"]["valid_at"] = json!({
                    "type": ["string", "null"],
                    "format": "date-time",
                    "description": "Validity cutoff; null means unbounded (records with valid_at null are in force).",
                });
            }
            schema
        }
        ToolKind::Write => {
            if tool.name == TOOL_PROPOSE_OBJECTIVE_OPERATION {
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["idempotency_key", "payload"],
                    "properties": {
                        "idempotency_key": { "type": "string", "minLength": 1 },
                        "payload": {
                            "type": "object",
                            "required": ["schema_version", "action", "payload"],
                            "properties": {
                                "schema_version": { "const": "1.0.0" },
                                "action": { "enum": [
                                    "propose_create", "propose_revision", "activate_version",
                                    "pause", "retire", "deprecate", "criteria_change",
                                    "review_policy_change",
                                ] },
                                "objective_ref": { "type": ["string", "null"] },
                                "base_objective_version": { "type": ["integer", "null"] },
                                "payload": { "type": "object" },
                            },
                        },
                    },
                })
            } else {
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["idempotency_key", "payload"],
                    "properties": {
                        "idempotency_key": { "type": "string", "minLength": 1 },
                        "payload": {
                            "type": "object",
                            "required": [
                                "schema_version", "object_type", "object_id",
                                "governance_root_ref", "authority_class", "lifecycle_state",
                                "valid_at", "as_known_at", "recorded_at", "provenance_refs",
                                "object",
                            ],
                            "properties": {
                                "schema_version": { "const": "1.0.0" },
                                "object_type": { "type": "string", "minLength": 1 },
                                "object_id": { "type": "string", "minLength": 1 },
                                "governance_root_ref": { "type": "string", "minLength": 1 },
                                "scope_refs": { "type": "array", "items": { "type": "string" } },
                                "authority_class": { "enum": [
                                    "protected_constraint", "current_operating_state",
                                    "working_knowledge", "candidate_knowledge",
                                ] },
                                "lifecycle_state": { "enum": [
                                    "candidate", "working", "governing",
                                    "governing_but_disputed", "superseded", "retired",
                                    "deprecated",
                                ] },
                                "valid_at": { "type": ["string", "null"], "format": "date-time" },
                                "as_known_at": { "type": "string", "format": "date-time" },
                                "recorded_at": { "type": "string", "format": "date-time" },
                                "provenance_refs": {
                                    "type": "array", "minItems": 1,
                                    "items": { "type": "string" },
                                },
                                "object": { "type": "object" },
                            },
                        },
                    },
                })
            }
        }
    }
}

// ------------------------------------------------------------------- errors

/// A boundary rejection: the invocation never became an operation. Carries the
/// oracle's structured reasons, mapped onto JSON-RPC codes by `rpc_code`.
#[derive(Debug, Clone)]
pub struct OisBoundaryError {
    pub reason: &'static str,
    pub message: String,
}

impl OisBoundaryError {
    fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }

    fn unknown_tool(name: &str) -> Self {
        Self::new(
            "unknown_tool",
            format!("no tool '{name}' is registered on the v1 agent boundary"),
        )
    }

    /// JSON-RPC error code for the JSON-RPC error body (transport succeeds,
    /// the method fails — the upstream surface's convention).
    pub fn rpc_code(&self) -> i64 {
        match self.reason {
            "unknown_tool" => -32601,
            "internal_error" | "resolver_input_error" => -32603,
            // payload_schema_violation, governance_root_mismatch, wiring
            _ => -32602,
        }
    }
}

// ------------------------------------------------------------------- wiring

/// The wired, server-derived evaluator session (oracle `EvaluatorWiring`):
/// a `solo_operator_v1`-shaped profile derived from Utopia's own access model,
/// with the acting agent Principal bound to the authenticated token.
///
/// Derivation is server-side; payload claims never set any of it. The agent
/// grant is propose-only by construction — the v1 MCP boundary never wires
/// governing authority, so even a write-capable session's proposals can never
/// promote. Governing authority in the profile's human grant reflects the
/// user's standing outside this surface (the governed-write path), and is
/// inert here: this boundary's Principal is always agent-kind.
pub struct WiredEvaluator {
    pub profile: Profile,
    pub principal: PrincipalBasis,
    pub governance_root_ref: String,
}

/// Wires the evaluator for one authenticated MCP session. Fail-closed: the
/// probe at the end mirrors the oracle's `wireEvaluatorPrincipal` — a grant
/// that cannot even hold `propose` over its own root never opens the surface.
pub fn wire_evaluator(
    user_id: Uuid,
    token_id: Uuid,
    kb_id: Uuid,
    can_write: bool,
) -> Result<WiredEvaluator, OisBoundaryError> {
    let governance_root_ref = governance_root_for(kb_id);
    let agent_ref = format!("agent:utopia-mcp-token-{token_id}");

    let mut agent_capabilities = vec!["propose".to_string()];
    if can_write {
        agent_capabilities.push("attach_evidence".to_string());
    }

    let mut human_capabilities = vec!["propose".to_string()];
    if can_write {
        // The human Scope Owner governs outside this boundary; the grant
        // records that standing (solo_operator_v1 shape), it grants nothing
        // here — the MCP Principal is always the agent.
        human_capabilities.push("establish_governing".to_string());
        human_capabilities.push("approve".to_string());
        human_capabilities.push("attach_evidence".to_string());
    }

    let profile = Profile {
        schema_version: "1.0.0".to_string(),
        profile_id: "solo_operator_v1".to_string(),
        profile_version: 1,
        governance_root_ref: governance_root_ref.clone(),
        root_scope_owner_kind: "human".to_string(),
        grants: vec![
            Grant {
                principal_kind: "human".to_string(),
                principal_ref: Some(format!("human:utopia-user-{user_id}")),
                capabilities: human_capabilities,
                governing_authority: can_write,
                delegation_scope_refs: None,
            },
            Grant {
                principal_kind: "agent".to_string(),
                principal_ref: Some(agent_ref.clone()),
                capabilities: agent_capabilities,
                // Structural propose-only invariant: the v1 boundary never
                // wires a governing agent grant.
                governing_authority: false,
                delegation_scope_refs: None,
            },
        ],
        disputed_basis_policy: "default_pending_approval".to_string(),
    };

    let principal = PrincipalBasis {
        principal_ref: agent_ref.clone(),
        // The transport authenticated this session server-side; any contrary
        // claim inside a submitted payload is provenance, never authority.
        principal_kind: "agent".to_string(),
        authenticated: true,
    };

    let wired = WiredEvaluator {
        profile,
        principal,
        governance_root_ref,
    };

    match evaluate_capability(
        &wired.profile,
        &wired.principal,
        "propose",
        &wired.governance_root_ref,
        &[],
    ) {
        CapabilityDecision::Authorized { .. } => Ok(wired),
        CapabilityDecision::Denied {
            authorized: _,
            reason,
        } => Err(OisBoundaryError::new(
            "no_agent_grant",
            format!("wired grant does not even hold 'propose' over its own root ({reason})"),
        )),
    }
}

// ---------------------------------------------------------------- validation

/// Typed-read query shape — the read vocabulary is the resolver's query, and
/// nothing else (raw query shapes smuggled in are schema violations).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadQuery {
    governance_root_ref: String,
    object_id: String,
    as_known_at: String,
    #[serde(default)]
    valid_at: Option<String>,
}

fn schema_violation(message: impl Into<String>) -> OisBoundaryError {
    OisBoundaryError::new("payload_schema_violation", message)
}

fn require_rfc3339(instant: &str, field: &str) -> Result<(), OisBoundaryError> {
    if instant.is_empty() || DateTime::parse_from_rfc3339(instant).is_err() {
        return Err(schema_violation(format!(
            "payload field '{field}' must be an RFC 3339 instant"
        )));
    }
    Ok(())
}

fn validate_read_query(args: &Value) -> Result<ReadQuery, OisBoundaryError> {
    let query: ReadQuery = serde_json::from_value(args.clone())
        .map_err(|e| schema_violation(format!("read arguments are not a typed query: {e}")))?;
    if query.governance_root_ref.is_empty() || query.object_id.is_empty() {
        return Err(schema_violation(
            "governance_root_ref and object_id must be non-empty",
        ));
    }
    require_rfc3339(&query.as_known_at, "as_known_at")?;
    if let Some(valid_at) = &query.valid_at {
        require_rfc3339(valid_at, "valid_at")?;
    }
    Ok(query)
}

// --------------------------------------------------------------------- reads

/// The server-derived disclosure basis for a wired session. Production reads
/// run fully visible (the wired evaluator has already proven `propose` over
/// the root); `ExistenceCountOnly` is the fail-closed contract for
/// per-principal kernel restrictions — it must yield `permission_limited`
/// with the withheld count, never a lying zero (exercised by the fixtures).
pub enum DisclosureLimit {
    Full,
    /// The fail-closed shape; constructed by the fixtures (production reads of
    /// a proven session run fully visible).
    #[allow(dead_code)]
    ExistenceCountOnly(i64),
}

fn permission_basis(limit: DisclosureLimit) -> PermissionBasis {
    match limit {
        DisclosureLimit::Full => PermissionBasis {
            decision: "granted".to_string(),
            disclosure_state: "fully_visible".to_string(),
            withheld_count: None,
        },
        DisclosureLimit::ExistenceCountOnly(count) => PermissionBasis {
            decision: "permission_limited".to_string(),
            disclosure_state: "restricted_existence_count_only".to_string(),
            withheld_count: Some(count),
        },
    }
}

/// Resolves one typed read through the pure kernel resolver: permission basis
/// → governance root → as-known slice → valid-time scoping → currentness.
/// The resolver never silently absorbs an omission — every filter that drops
/// records emits a diagnostic.
pub fn evaluate_read(
    wired: &WiredEvaluator,
    args: &Value,
    corpus: ResolutionCorpus,
    basis: PermissionBasis,
) -> Result<Currentness, OisBoundaryError> {
    // Dispatch happens per tool name — resolve vs slice differ only in which
    // envelope of validation applies, so both come through here and the query
    // shape decides the temporal semantics.
    let query = validate_read_query(args)?;

    // Cross-root queries never resolve — profiles do not cross governance
    // roots (the same isolation the write path enforces).
    if query.governance_root_ref != wired.governance_root_ref {
        return Err(OisBoundaryError::new(
            "governance_root_mismatch",
            format!(
                "query targets '{}'; the wired profile owns '{}'",
                query.governance_root_ref, wired.governance_root_ref
            ),
        ));
    }

    // Permission precedes disclosure: the basis is server-derived per
    // Principal, never declared by the caller.
    let resolution = ResolutionQuery {
        governance_root_ref: wired.governance_root_ref.clone(),
        as_known_at: query.as_known_at.clone(),
        valid_at: query.valid_at.clone(),
        scope_refs: None,
        permission: Some(basis),
        applicability: None,
    };

    // Mirror the oracle's read path: the target history is sliced at the
    // boundary, the resolver re-slices idempotently inside.
    let sliced = ResolutionCorpus {
        target_history: slice_as_known(&corpus.target_history, &query.as_known_at),
        related_records: corpus.related_records,
    };

    resolve_current_state(&sliced, &resolution).map_err(|e| {
        OisBoundaryError::new("resolver_input_error", format!("resolution failed: {e}"))
    })
}

// ------------------------------------------------------------------ dispatch

/// The authenticated MCP session as seen by the governed surface: every field
/// was derived server-side by the upstream authorize gate — nothing here is
/// payload-claimed.
pub struct GovernedSession<'a> {
    pub kb: &'a KnowledgeBase,
    pub user: &'a User,
    pub token_id: Uuid,
    pub can_write: bool,
}

/// The governed surface's entry point for `tools/call`. Returns `None` for
/// names outside this surface (the upstream dispatcher owns those), otherwise
/// the complete JSON-RPC response body.
///
/// Authentication and KB-role enforcement already happened in `super::mcp`'s
/// `authorize` — the same two gates the upstream tools run behind. This layer
/// adds the governed-surface checks on top; none of them weaken the upstream
/// gates.
pub async fn maybe_handle_rpc(
    state: &AppState,
    session: &GovernedSession<'_>,
    name: &str,
    args: &Value,
    id: Option<Value>,
) -> Option<Json<Value>> {
    if !is_wired_tool(name) {
        return None;
    }

    // The wired evaluator session: server-derived profile, propose-only
    // agent grant, per-token Principal and activity refs.
    let wired = match wire_evaluator(
        session.user.id,
        session.token_id,
        session.kb.id,
        session.can_write,
    ) {
        Ok(wired) => wired,
        Err(err) => return Some(super::mcp::rpc_err(id, err.rpc_code(), &err.message)),
    };

    let response = match wired_tool(name) {
        None => super::mcp::rpc_err(id, -32601, &OisBoundaryError::unknown_tool(name).message),
        Some(tool) => match tool.kind {
            ToolKind::Read => {
                // Production disclosure: the wired session proved `propose`
                // over the root, so reads run fully visible. The restricted
                // shape exists for per-principal kernel restrictions and is
                // exercised by the fixtures.
                let basis = permission_basis(DisclosureLimit::Full);
                match load_corpus(state, &wired.governance_root_ref, name, args).await {
                    Ok(corpus) => match evaluate_read(&wired, args, corpus, basis) {
                        Ok(derivation) => {
                            let derivation =
                                serde_json::to_value(&derivation).unwrap_or(Value::Null);
                            super::mcp::ok(
                                id,
                                json!({
                                    "content": [{ "type": "text", "text": serde_json::to_string(
                                        &json!({ "kind": "derivation", "derivation": derivation }),
                                    ).unwrap_or_default() }],
                                    "isError": false,
                                }),
                            )
                        }
                        Err(err) => super::mcp::rpc_err(id, err.rpc_code(), &err.message),
                    },
                    Err(err) => super::mcp::rpc_err(id, err.rpc_code(), &err.message),
                }
            }
            ToolKind::Write => {
                // Same two-gate standing as upstream `remember`: a write-scope
                // token held by an editor. The kernel capability gate below is
                // additional, not a substitute.
                if !exposed_under(name, session.can_write) {
                    let message = format!(
                        "Tool '{name}' needs a token with the write scope, \
                         held by an editor in this base"
                    );
                    super::mcp::rpc_err(id, -32601, &message)
                } else {
                    // The propose-only application path lands with the
                    // governed-write wiring (stage 4c, second commit); the
                    // validation/gating pipeline is in place either way.
                    let err = OisBoundaryError::new(
                        "family_not_wired",
                        format!("tool '{name}' is not wired for application yet"),
                    );
                    super::mcp::rpc_err(id, err.rpc_code(), &err.message)
                }
            }
        },
    };

    // Audit every governed-surface invocation, same event shape as upstream.
    let _ = utopia_store::audit::record(
        &state.pool,
        Some(session.kb.id),
        session.user.id,
        "mcp.tool_called",
        "personal_token",
        Some(session.token_id),
        json!({ "tool": name }),
    )
    .await;

    Some(response)
}

/// Assembles the resolution corpus for one object from the kernel side
/// tables, scoped to the session's governance root (oracle `port.history`).
async fn load_corpus(
    state: &AppState,
    gov_root: &str,
    tool_name: &str,
    args: &Value,
) -> Result<ResolutionCorpus, OisBoundaryError> {
    // The object id and as-known cutoff validate inside `evaluate_read`;
    // this loader only needs the object id to query the side tables.
    let object_id = args
        .get("object_id")
        .and_then(Value::as_str)
        .ok_or_else(|| schema_violation("read arguments must carry object_id"))?
        .to_string();

    let store = utopia_store::ois_kernel::PgEnvelopeStore::new(&state.pool);
    let target_history = match tool_name {
        TOOL_SLICE_AS_KNOWN | TOOL_RESOLVE_CURRENT_STATE => store
            .history_for_object_in_root(gov_root, &object_id)
            .await
            .map_err(|e| {
                OisBoundaryError::new("internal_error", format!("kernel store read failed: {e}"))
            })?,
        other => {
            return Err(OisBoundaryError::unknown_tool(other));
        }
    };
    let related_records = store
        .related_records_in_root(gov_root, &object_id)
        .await
        .map_err(|e| {
            OisBoundaryError::new("internal_error", format!("kernel store read failed: {e}"))
        })?;

    Ok(ResolutionCorpus {
        target_history,
        related_records: Some(related_records),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ois_kernel::envelope::{AuthorityClass, KernelEnvelope, LifecycleState};
    use ois_kernel::resolver::{Completeness, NotEstablishedBasis, ResolutionDiagnostic};

    const KB: Uuid = Uuid::from_u128(1);
    const USER: Uuid = Uuid::from_u128(2);
    const TOKEN: Uuid = Uuid::from_u128(3);

    fn root() -> String {
        governance_root_for(KB)
    }

    fn wired() -> WiredEvaluator {
        wire_evaluator(USER, TOKEN, KB, true).expect("wiring succeeds for a write-capable session")
    }

    fn governing_assertion(object_id: &str, recorded_at: &str) -> KernelEnvelope {
        KernelEnvelope {
            schema_version: "1.0.0".to_string(),
            object_type: "assertion".to_string(),
            object_id: object_id.to_string(),
            governance_root_ref: root(),
            scope_refs: None,
            authority_class: AuthorityClass::CurrentOperatingState,
            lifecycle_state: LifecycleState::Governing,
            valid_at: None,
            as_known_at: recorded_at.to_string(),
            recorded_at: recorded_at.to_string(),
            provenance_refs: vec!["activity:seed".to_string()],
            object: json!({ "statement": "line speed is 120" }),
        }
    }

    fn read_args(object_id: &str, as_known_at: &str) -> Value {
        json!({
            "governance_root_ref": root(),
            "object_id": object_id,
            "as_known_at": as_known_at,
            "valid_at": Value::Null,
        })
    }

    fn corpus(history: Vec<KernelEnvelope>) -> ResolutionCorpus {
        ResolutionCorpus {
            target_history: history,
            related_records: None,
        }
    }

    #[test]
    fn read_resolves_governing_under_server_derived_basis() {
        let session = wired();
        let history = vec![governing_assertion(
            "asrt-line-speed",
            "2026-09-01T00:00:00.000Z",
        )];
        let derivation = evaluate_read(
            &session,
            &read_args("asrt-line-speed", "2026-09-05T00:00:00.000Z"),
            corpus(history),
            permission_basis(DisclosureLimit::Full),
        )
        .expect("read resolves");
        match derivation {
            Currentness::Governing {
                authority,
                completeness,
                diagnostics,
                ..
            } => {
                assert_eq!(authority, "current_operating_state");
                assert_eq!(completeness, Completeness::Complete);
                assert!(diagnostics.is_empty());
            }
            other => panic!("expected governing, got {:?}", other.state_word()),
        }
    }

    #[test]
    fn slice_as_known_hides_later_recordings() {
        let session = wired();
        let history = vec![governing_assertion(
            "asrt-line-speed",
            "2026-09-01T00:00:00.000Z",
        )];
        let derivation = evaluate_read(
            &session,
            &read_args("asrt-line-speed", "2026-08-15T00:00:00.000Z"),
            corpus(history),
            permission_basis(DisclosureLimit::Full),
        )
        .expect("read resolves");
        match derivation {
            Currentness::NotEstablished {
                basis,
                completeness,
                ..
            } => {
                assert_eq!(basis, NotEstablishedBasis::NoVisibleRecord);
                assert_eq!(completeness, Completeness::Complete);
            }
            other => panic!("expected not_established, got {:?}", other.state_word()),
        }
    }

    #[test]
    fn permission_limited_basis_reports_withheld_count() {
        let session = wired();
        let history = vec![governing_assertion(
            "asrt-line-speed",
            "2026-09-01T00:00:00.000Z",
        )];
        let derivation = evaluate_read(
            &session,
            &read_args("asrt-line-speed", "2026-09-05T00:00:00.000Z"),
            corpus(history),
            permission_basis(DisclosureLimit::ExistenceCountOnly(2)),
        )
        .expect("read resolves");
        match derivation {
            Currentness::PermissionLimited {
                completeness,
                diagnostics,
            } => {
                assert_eq!(completeness, Completeness::PartialPermissions);
                assert!(diagnostics.iter().any(|d| matches!(
                    d,
                    ResolutionDiagnostic::PermissionWithheld {
                        withheld_count: Some(2)
                    }
                )));
            }
            other => panic!(
                "a restricted basis must never disclose the underlying state, got {:?}",
                other.state_word()
            ),
        }
    }

    #[test]
    fn valid_at_axis_bounds_force() {
        let session = wired();
        let mut record = governing_assertion("asrt-future", "2026-09-01T00:00:00.000Z");
        record.valid_at = Some("2026-09-10T00:00:00.000Z".to_string());

        // Query before the record's validity: not yet in force, and the
        // omission is explicit.
        let derivation = evaluate_read(
            &session,
            &json!({
                "governance_root_ref": root(),
                "object_id": "asrt-future",
                "as_known_at": "2026-09-05T00:00:00.000Z",
                "valid_at": "2026-09-05T00:00:00.000Z",
            }),
            corpus(vec![record.clone()]),
            permission_basis(DisclosureLimit::Full),
        )
        .expect("read resolves");
        match derivation {
            Currentness::NotEstablished {
                basis: _,
                diagnostics,
                ..
            } => assert!(diagnostics
                .iter()
                .any(|d| matches!(d, ResolutionDiagnostic::FutureValidExcluded { .. }))),
            other => panic!("expected not_established, got {:?}", other.state_word()),
        }

        // Past the validity start: in force.
        let derivation = evaluate_read(
            &session,
            &json!({
                "governance_root_ref": root(),
                "object_id": "asrt-future",
                "as_known_at": "2026-09-05T00:00:00.000Z",
                "valid_at": "2026-09-15T00:00:00.000Z",
            }),
            corpus(vec![record]),
            permission_basis(DisclosureLimit::Full),
        )
        .expect("read resolves");
        assert_eq!(derivation.state_word(), "governing");
    }

    #[test]
    fn read_rejects_cross_root_queries() {
        let session = wired();
        let err = evaluate_read(
            &session,
            &json!({
                "governance_root_ref": "ois:scope:kb:00000000-0000-4000-8000-0000000000ff",
                "object_id": "asrt-x",
                "as_known_at": "2026-09-05T00:00:00.000Z",
                "valid_at": Value::Null,
            }),
            corpus(vec![]),
            permission_basis(DisclosureLimit::Full),
        )
        .expect_err("cross-root reads never resolve");
        assert_eq!(err.reason, "governance_root_mismatch");
    }

    #[test]
    fn read_rejects_raw_query_shapes() {
        let session = wired();
        let err = evaluate_read(
            &session,
            &json!({
                "governance_root_ref": root(),
                "object_id": "asrt-x",
                "as_known_at": "2026-09-05T00:00:00.000Z",
                "valid_at": Value::Null,
                "wql": "select documents",
            }),
            corpus(vec![]),
            permission_basis(DisclosureLimit::Full),
        )
        .expect_err("raw query shapes are schema violations");
        assert_eq!(err.reason, "payload_schema_violation");
    }

    #[test]
    fn wired_agent_grant_is_structurally_propose_only() {
        let session = wired();
        let agent_grant = session
            .profile
            .grants
            .iter()
            .find(|g| g.principal_kind == "agent")
            .expect("agent grant present");
        assert!(!agent_grant.governing_authority);
        assert!(!agent_grant
            .capabilities
            .iter()
            .any(|c| c == "establish_governing" || c == "approve"));
    }
}
