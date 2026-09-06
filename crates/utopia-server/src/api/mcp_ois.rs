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
use chrono::{DateTime, Utc};
use ois_kernel::canonical::canonical_digest;
use ois_kernel::envelope::{AuthorityClass, KernelEnvelope, LifecycleState};
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

/// Server clock in the oracle's instant format (millisecond UTC, `Z`).
/// The resolver compares instants byte-wise; every instant this surface
/// writes must match the fork's stored corpora.
fn iso_now(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
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
    pub operation_family: &'static str,
    pub fixed_capability: Option<&'static str>,
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
        operation_family: "kernel_write",
        fixed_capability: None,
    },
    OisTool {
        name: TOOL_PROPOSE_OBJECTIVE_OPERATION,
        description: "Submit one objective operation (proposal actions apply as candidate knowledge; governing transition actions map to establish_governing and are denied for propose-only agents).",
        kind: ToolKind::Write,
        operation_family: "objective_operation",
        fixed_capability: None,
    },
    OisTool {
        name: TOOL_ATTACH_EVIDENCE,
        description: "Attach evidence by appending a new candidate kernel version carrying provenance. Evidence that itself establishes governing state is a governing operation and is denied for propose-only agents.",
        kind: ToolKind::Write,
        operation_family: "kernel_write",
        fixed_capability: None,
    },
    OisTool {
        name: TOOL_RESOLVE_CURRENT_STATE,
        description: "Resolve the canonical currentness of one kernel object under the server-derived permission basis. States stay distinct: governing, governing_but_disputed, not_established, superseded, retired, deprecated, permission_limited.",
        kind: ToolKind::Read,
        operation_family: "kernel_write",
        fixed_capability: Some("propose"),
    },
    OisTool {
        name: TOOL_SLICE_AS_KNOWN,
        description: "Slice one kernel object as known at an as_known_at instant (later recordings are invisible) and resolve its currentness in that historical slice.",
        kind: ToolKind::Read,
        operation_family: "kernel_write",
        fixed_capability: Some("propose"),
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
    pub actor_activity_ref: String,
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

    let actor_activity_ref = format!("activity:utopia-mcp-token-{token_id}");

    let wired = WiredEvaluator {
        profile,
        principal,
        actor_activity_ref,
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

// ----------------------------------------------------- capability derivation

/// Reads the required capability off an *already validated* payload. The
/// payload's own authority claims decide what the operation constitutes;
/// server-side evaluation then decides whether this Principal holds it
/// (oracle `requiredCapability`). Caller claims of approval or
/// authentication stay provenance — never authority.
fn required_capability(tool: &OisTool, payload: &Value) -> Result<&'static str, OisBoundaryError> {
    if let Some(fixed) = tool.fixed_capability {
        return Ok(fixed);
    }

    /// The governing claim rule (oracle: authority class in the governing pair
    /// or lifecycle `governing` — contract coherence ties the rest to these).
    fn claims_governing(payload: &Value) -> bool {
        let authority = payload.get("authority_class").and_then(Value::as_str);
        let lifecycle = payload.get("lifecycle_state").and_then(Value::as_str);
        matches!(
            authority,
            Some("protected_constraint") | Some("current_operating_state")
        ) || lifecycle == Some("governing")
    }

    match tool.name {
        TOOL_PROPOSE_KERNEL_RECORD => Ok(if claims_governing(payload) {
            "establish_governing"
        } else {
            "propose"
        }),
        TOOL_ATTACH_EVIDENCE => Ok(if claims_governing(payload) {
            // Evidence that itself establishes governing state is a governing
            // operation, not an evidential one — same rule as propose.
            "establish_governing"
        } else {
            "attach_evidence"
        }),
        TOOL_PROPOSE_OBJECTIVE_OPERATION => {
            let action = payload.get("action").and_then(Value::as_str);
            Ok(match action {
                Some("propose_create") | Some("propose_revision") => "propose",
                _ => "establish_governing",
            })
        }
        other => Err(OisBoundaryError::new(
            "family_not_wired",
            format!("tool '{other}' has no capability mapping — registry is misconfigured"),
        )),
    }
}

// ------------------------------------------------------------------ envelopes

/// The typed-operation envelope returned for every write invocation — the
/// oracle's `OISTypedOperationV1` shape. Denials are envelopes too: distinct
/// outcomes, never silent no-ops.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TypedOperationEnvelope {
    pub schema_version: String,
    pub operation_id: String,
    pub operation_family: &'static str,
    pub actor_principal_ref: String,
    pub actor_activity_ref: String,
    pub idempotency_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_of_operation_id: Option<String>,
    pub target_refs: Vec<String>,
    pub base_state: BaseState,
    pub authority_state: &'static str,
    pub disposition: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_resource_refs: Option<Vec<String>>,
    pub submitted_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BaseState {
    pub base_ref: String,
}

fn operation_id(tool: &OisTool, idempotency_key: &str) -> String {
    format!("mcp:{}:{}", tool.name, idempotency_key)
}

/// The well-formed, contract-valid invocation whose capability the wired
/// Principal does not hold. Nothing is recorded — the denial is not a silent
/// write either; replaying re-evaluates deterministically (a later grant
/// change is honored).
fn rejected_envelope(
    tool: &OisTool,
    idempotency_key: &str,
    wired: &WiredEvaluator,
    now: &str,
) -> TypedOperationEnvelope {
    TypedOperationEnvelope {
        schema_version: "1.0.0".to_string(),
        base_state: BaseState {
            // No kernel state was read as base; the ref names the boundary
            // denial itself so base_ref stays meaningful and non-empty.
            base_ref: format!("ois-denied:{}:{}", tool.name, idempotency_key),
        },
        operation_id: operation_id(tool, idempotency_key),
        operation_family: tool.operation_family,
        actor_principal_ref: wired.principal.principal_ref.clone(),
        actor_activity_ref: wired.actor_activity_ref.clone(),
        idempotency_key: idempotency_key.to_string(),
        replay_of_operation_id: None,
        target_refs: vec![],
        authority_state: "denied",
        disposition: "rejected",
        reason_code: Some("authority_denied".to_string()),
        result_resource_refs: None,
        submitted_at: now.to_string(),
        resolved_at: None,
    }
}

/// The applied envelope for a candidate that just landed. `base_ref` names
/// the record itself — the propose-only boundary holds no prior governing
/// state to conflict with (oracle `applyCandidate`).
fn applied_envelope(
    tool: &OisTool,
    idempotency_key: &str,
    record_ref: &str,
    wired: &WiredEvaluator,
    now: &str,
) -> TypedOperationEnvelope {
    TypedOperationEnvelope {
        schema_version: "1.0.0".to_string(),
        base_state: BaseState {
            base_ref: record_ref.to_string(),
        },
        operation_id: operation_id(tool, idempotency_key),
        operation_family: tool.operation_family,
        actor_principal_ref: wired.principal.principal_ref.clone(),
        actor_activity_ref: wired.actor_activity_ref.clone(),
        idempotency_key: idempotency_key.to_string(),
        replay_of_operation_id: None,
        target_refs: vec![record_ref.to_string()],
        authority_state: "authorized",
        disposition: "applied",
        reason_code: Some("evidence_sufficient".to_string()),
        result_resource_refs: Some(vec![record_ref.to_string()]),
        submitted_at: now.to_string(),
        resolved_at: Some(now.to_string()),
    }
}

/// Idempotent replay: the recorded outcome returned, never a second record
/// (oracle `replayEnvelope`).
fn replay_envelope(
    tool: &OisTool,
    idempotency_key: &str,
    prior: &utopia_store::ois_kernel::RecordedOperation,
    wired: &WiredEvaluator,
    now: &str,
) -> TypedOperationEnvelope {
    TypedOperationEnvelope {
        schema_version: "1.0.0".to_string(),
        base_state: BaseState {
            base_ref: prior.envelope_id.clone(),
        },
        operation_id: prior.op_id.clone(),
        operation_family: tool.operation_family,
        actor_principal_ref: wired.principal.principal_ref.clone(),
        actor_activity_ref: wired.actor_activity_ref.clone(),
        idempotency_key: idempotency_key.to_string(),
        replay_of_operation_id: Some(prior.op_id.clone()),
        target_refs: vec![prior.envelope_id.clone()],
        authority_state: "authorized",
        disposition: "applied",
        reason_code: Some("replay_acknowledged".to_string()),
        result_resource_refs: Some(vec![prior.envelope_id.clone()]),
        submitted_at: now.to_string(),
        resolved_at: Some(prior.applied_at.to_rfc3339()),
    }
}

// -------------------------------------------------------- write validation

/// Write invocation shape: `{ idempotency_key, payload }` — nothing else may
/// ride along (the oracle compiles exactly this wrapper around each payload
/// contract).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInvocation {
    idempotency_key: String,
    payload: Value,
}

/// The kernel record payload, validated against the fork's envelope contract
/// (the 11 canonical fields; unknown top-level fields rejected fail-closed).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KernelPayload {
    schema_version: String,
    object_type: String,
    object_id: String,
    governance_root_ref: String,
    #[serde(default)]
    scope_refs: Option<Vec<String>>,
    authority_class: AuthorityClass,
    lifecycle_state: LifecycleState,
    valid_at: Option<String>,
    as_known_at: String,
    recorded_at: String,
    provenance_refs: Vec<String>,
    object: Value,
}

/// The objective-operation payload (frozen action vocabulary; the inner
/// payload stays a free object the record builder projects).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectiveOperationPayload {
    schema_version: String,
    action: String,
    #[serde(default)]
    objective_ref: Option<String>,
    #[serde(default)]
    base_objective_version: Option<i64>,
    payload: Value,
}

/// The MCP-local evidence payload: what the evidence is about, and the
/// evidence itself. It lands as a candidate record that references its
/// target — it never mutates the target.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct EvidencePayload {
    target_object_id: String,
    evidence: Value,
}

const OBJECTIVE_ACTIONS: [&str; 8] = [
    "propose_create",
    "propose_revision",
    "activate_version",
    "pause",
    "retire",
    "deprecate",
    "criteria_change",
    "review_policy_change",
];

fn validate_write_invocation(args: &Value) -> Result<WriteInvocation, OisBoundaryError> {
    let invocation: WriteInvocation = serde_json::from_value(args.clone()).map_err(|e| {
        schema_violation(format!(
            "write invocation must be {{ idempotency_key, payload }}: {e}"
        ))
    })?;
    if invocation.idempotency_key.is_empty() {
        return Err(schema_violation(
            "idempotency_key must be a non-empty string",
        ));
    }
    Ok(invocation)
}

fn validate_kernel_payload(payload: &Value) -> Result<KernelPayload, OisBoundaryError> {
    let parsed: KernelPayload = serde_json::from_value(payload.clone()).map_err(|e| {
        schema_violation(format!("payload does not satisfy the kernel contract: {e}"))
    })?;
    if parsed.schema_version != "1.0.0" {
        return Err(schema_violation(format!(
            "payload schema_version '{}' is not '1.0.0'",
            parsed.schema_version
        )));
    }
    if parsed.object_type.is_empty() || parsed.object_id.is_empty() {
        return Err(schema_violation(
            "object_type and object_id must be non-empty",
        ));
    }
    if parsed.provenance_refs.is_empty() {
        return Err(schema_violation(
            "provenance_refs must carry at least one ref",
        ));
    }
    // The authority/lifecycle coupling the kernel contract enforces
    // (candidate/working cannot carry governing lifecycle, and vice versa).
    // Incoherent input fails loud here rather than resolving nonsense later.
    if parsed.lifecycle_state.is_candidate_or_working()
        != parsed.authority_class.is_candidate_or_working()
    {
        return Err(schema_violation(format!(
            "lifecycle '{}' is incompatible with authority '{}'",
            parsed.lifecycle_state.as_str(),
            parsed.authority_class.as_str()
        )));
    }
    require_rfc3339(&parsed.as_known_at, "as_known_at")?;
    require_rfc3339(&parsed.recorded_at, "recorded_at")?;
    if let Some(valid_at) = &parsed.valid_at {
        require_rfc3339(valid_at, "valid_at")?;
    }
    Ok(parsed)
}

fn validate_objective_payload(
    payload: &Value,
) -> Result<ObjectiveOperationPayload, OisBoundaryError> {
    let parsed: ObjectiveOperationPayload =
        serde_json::from_value(payload.clone()).map_err(|e| {
            schema_violation(format!(
                "payload does not satisfy the objective-operations contract: {e}"
            ))
        })?;
    if parsed.schema_version != "1.0.0" {
        return Err(schema_violation(format!(
            "payload schema_version '{}' is not '1.0.0'",
            parsed.schema_version
        )));
    }
    if !OBJECTIVE_ACTIONS.contains(&parsed.action.as_str()) {
        return Err(schema_violation(format!(
            "action '{}' is not in the frozen objective-operation vocabulary",
            parsed.action
        )));
    }
    Ok(parsed)
}

fn validate_evidence_payload(payload: &Value) -> Result<EvidencePayload, OisBoundaryError> {
    let parsed: EvidencePayload = serde_json::from_value(payload.clone()).map_err(|e| {
        schema_violation(format!(
            "payload does not satisfy the evidence contract: {e}"
        ))
    })?;
    if parsed.target_object_id.is_empty() {
        return Err(schema_violation("target_object_id must be non-empty"));
    }
    if !parsed.evidence.is_object() {
        return Err(schema_violation("evidence must be an object"));
    }
    Ok(parsed)
}

// ----------------------------------------------------------- write pipeline

/// The outcome of one governed write invocation, decided purely (no storage):
/// the boundary then persists exactly what was decided — nothing more.
#[derive(Debug)]
pub enum WriteOutcome {
    /// Capability denied. Nothing recorded, nothing written; replaying
    /// re-evaluates (a later grant change is honored).
    Rejected(TypedOperationEnvelope),
    /// The candidate landed: the kernel envelope to persist, the record ref
    /// it will be stored under, and the response envelope reporting it.
    Applied {
        kernel: Box<KernelEnvelope>,
        record_ref: String,
        response: TypedOperationEnvelope,
    },
}

/// Builds the CANDIDATE kernel envelope for a validated kernel-record
/// payload. Timestamps and provenance are server-derived (the oracle's
/// rule): the client's as_known_at is a knowledge claim, recorded_at is the
/// server clock, and the server-derived activity ref rides with the client's
/// claimed provenance.
fn candidate_kernel_envelope(
    wired: &WiredEvaluator,
    payload: KernelPayload,
    now: &str,
) -> KernelEnvelope {
    let mut provenance_refs = payload.provenance_refs;
    provenance_refs.push(wired.actor_activity_ref.clone());
    KernelEnvelope {
        schema_version: "1.0.0".to_string(),
        object_type: payload.object_type,
        object_id: payload.object_id,
        governance_root_ref: wired.governance_root_ref.clone(),
        scope_refs: payload.scope_refs,
        authority_class: payload.authority_class,
        lifecycle_state: payload.lifecycle_state,
        valid_at: payload.valid_at,
        as_known_at: payload.as_known_at,
        recorded_at: now.to_string(),
        provenance_refs,
        object: payload.object,
    }
}

/// Objective operations from a propose-only agent land as candidate
/// knowledge ABOUT the objective. Governing transition actions never even
/// reach this builder — the capability gate maps them to
/// establish_governing, which no wired agent grant holds.
fn candidate_objective_envelope(
    wired: &WiredEvaluator,
    payload: ObjectiveOperationPayload,
    now: &str,
) -> KernelEnvelope {
    let object_id = payload
        .objective_ref
        .unwrap_or_else(|| canonical_digest(&payload.payload));
    KernelEnvelope {
        schema_version: "1.0.0".to_string(),
        object_type: "objective".to_string(),
        object_id,
        governance_root_ref: wired.governance_root_ref.clone(),
        scope_refs: None,
        authority_class: AuthorityClass::CandidateKnowledge,
        lifecycle_state: LifecycleState::Candidate,
        valid_at: None,
        as_known_at: now.to_string(),
        recorded_at: now.to_string(),
        provenance_refs: vec![wired.actor_activity_ref.clone()],
        object: json!({
            "action": payload.action,
            "base_objective_version": payload.base_objective_version,
            "payload": payload.payload,
        }),
    }
}

/// Evidence lands as a content-addressed candidate record that references
/// its target. It never mutates the target.
fn candidate_evidence_envelope(
    wired: &WiredEvaluator,
    payload: EvidencePayload,
    now: &str,
) -> Result<KernelEnvelope, OisBoundaryError> {
    let payload_json = serde_json::to_value(&payload).map_err(|e| {
        OisBoundaryError::new("internal_error", format!("evidence serialize failed: {e}"))
    })?;
    Ok(KernelEnvelope {
        schema_version: "1.0.0".to_string(),
        object_type: "evidence".to_string(),
        object_id: canonical_digest(&payload_json),
        governance_root_ref: wired.governance_root_ref.clone(),
        scope_refs: None,
        authority_class: AuthorityClass::CandidateKnowledge,
        lifecycle_state: LifecycleState::Candidate,
        valid_at: None,
        as_known_at: now.to_string(),
        recorded_at: now.to_string(),
        provenance_refs: vec![wired.actor_activity_ref.clone()],
        object: json!({
            "target_object_id": payload.target_object_id,
            "evidence": payload.evidence,
        }),
    })
}

/// The propose-only evaluation: what the payload constitutes → whether the
/// wired Principal may do it → the candidate that lands or the rejection
/// envelope that reports the denial.
fn evaluate_write(
    wired: &WiredEvaluator,
    tool: &OisTool,
    invocation: &WriteInvocation,
    now: &str,
) -> Result<WriteOutcome, OisBoundaryError> {
    let capability = required_capability(tool, &invocation.payload)?;

    if let CapabilityDecision::Denied { .. } = evaluate_capability(
        &wired.profile,
        &wired.principal,
        capability,
        &wired.governance_root_ref,
        &[],
    ) {
        return Ok(WriteOutcome::Rejected(rejected_envelope(
            tool,
            &invocation.idempotency_key,
            wired,
            now,
        )));
    }

    let kernel = match tool.name {
        TOOL_PROPOSE_KERNEL_RECORD => {
            let payload = validate_kernel_payload(&invocation.payload)?;
            // Cross-root submissions never record — profiles do not cross
            // governance roots (the same isolation the reads enforce).
            if payload.governance_root_ref != wired.governance_root_ref {
                return Err(OisBoundaryError::new(
                    "governance_root_mismatch",
                    format!(
                        "payload targets '{}'; the wired profile owns '{}'",
                        payload.governance_root_ref, wired.governance_root_ref
                    ),
                ));
            }
            candidate_kernel_envelope(wired, payload, now)
        }
        TOOL_PROPOSE_OBJECTIVE_OPERATION => candidate_objective_envelope(
            wired,
            validate_objective_payload(&invocation.payload)?,
            now,
        ),
        TOOL_ATTACH_EVIDENCE => candidate_evidence_envelope(
            wired,
            validate_evidence_payload(&invocation.payload)?,
            now,
        )?,
        other => return Err(OisBoundaryError::unknown_tool(other)),
    };

    // Structural propose-only invariant, checked where it matters: what
    // lands is candidate/working knowledge. No MCP invocation reaches
    // governing state — governing writes belong to the governed promotion
    // path, which this surface does not expose.
    if !kernel.lifecycle_state.is_candidate_or_working() {
        return Err(OisBoundaryError::new(
            "internal_error",
            "refusing to record a governing lifecycle from the propose-only boundary",
        ));
    }

    let envelope_json = serde_json::to_value(&kernel).map_err(|e| {
        OisBoundaryError::new("internal_error", format!("envelope serialize failed: {e}"))
    })?;
    let record_ref = canonical_digest(&envelope_json);
    let response = applied_envelope(tool, &invocation.idempotency_key, &record_ref, wired, now);
    Ok(WriteOutcome::Applied {
        kernel: Box::new(kernel),
        record_ref,
        response,
    })
}

/// The propose-only write pipeline behind `tools/call`: replay lookup →
/// invocation validation → capability evaluation → candidate application.
/// Applied candidates are the only writes; rejections record nothing.
async fn handle_write(
    store: &utopia_store::ois_kernel::PgEnvelopeStore<'_>,
    wired: &WiredEvaluator,
    name: &str,
    args: &Value,
    now: &str,
) -> Result<Value, OisBoundaryError> {
    let tool = wired_tool(name).ok_or_else(|| OisBoundaryError::unknown_tool(name))?;
    let invocation = validate_write_invocation(args)?;
    let op_id = operation_id(tool, &invocation.idempotency_key);

    // Idempotent replay: a recorded operation returns its recorded outcome —
    // never a second record.
    if let Some(prior) = store.recorded_operation(&op_id).await.map_err(|e| {
        OisBoundaryError::new("internal_error", format!("kernel store read failed: {e}"))
    })? {
        let envelope = replay_envelope(tool, &invocation.idempotency_key, &prior, wired, now);
        return Ok(operation_response(&envelope));
    }

    match evaluate_write(wired, tool, &invocation, now)? {
        WriteOutcome::Rejected(envelope) => Ok(operation_response(&envelope)),
        WriteOutcome::Applied {
            kernel,
            record_ref,
            response,
        } => {
            // Persist the candidate first (FK order), then the operation row
            // that enables replay. Both are idempotent on conflict.
            store.record_envelope(&kernel).await.map_err(|e| {
                OisBoundaryError::new("internal_error", format!("kernel write failed: {e}"))
            })?;
            store
                .record_mcp_operation(&op_id, &record_ref, name, &wired.principal.principal_ref)
                .await
                .map_err(|e| {
                    OisBoundaryError::new("internal_error", format!("operation record failed: {e}"))
                })?;
            Ok(operation_response(&response))
        }
    }
}

/// The MCP content response for a typed-operation envelope.
fn operation_response(envelope: &TypedOperationEnvelope) -> Value {
    let body = serde_json::to_string(envelope).unwrap_or_default();
    json!({
        "content": [{ "type": "text", "text": body }],
        "isError": false,
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
                    let store = utopia_store::ois_kernel::PgEnvelopeStore::new(&state.pool);
                    match handle_write(&store, &wired, name, args, &iso_now(Utc::now())).await {
                        Ok(result) => super::mcp::ok(id, result),
                        Err(err) => super::mcp::rpc_err(id, err.rpc_code(), &err.message),
                    }
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

    // ------------------------------------------------ write-path fixtures

    fn kernel_payload(authority: &str, lifecycle: &str, object_id: &str) -> Value {
        json!({
            "schema_version": "1.0.0",
            "object_type": "assertion",
            "object_id": object_id,
            "governance_root_ref": root(),
            "scope_refs": Value::Null,
            "authority_class": authority,
            "lifecycle_state": lifecycle,
            "valid_at": Value::Null,
            "as_known_at": "2026-09-05T00:00:00.000Z",
            "recorded_at": "2026-09-05T00:00:00.000Z",
            "provenance_refs": ["activity:client"],
            "object": { "statement": "line speed is 130" },
        })
    }

    fn objective_payload(action: &str) -> Value {
        json!({
            "schema_version": "1.0.0",
            "action": action,
            "payload": { "title": "hold the line" },
        })
    }

    fn evidence_payload() -> Value {
        json!({
            "target_object_id": "asrt-line-speed",
            "evidence": { "digest": "sha256:abc", "source_ref": "notion:page-9" },
        })
    }

    fn write_args(payload: Value) -> Value {
        json!({ "idempotency_key": "idem-1", "payload": payload })
    }

    #[test]
    fn governing_claim_is_rejected_fail_closed() {
        let session = wired();
        let tool = wired_tool(TOOL_PROPOSE_KERNEL_RECORD).expect("tool present");
        let invocation = validate_write_invocation(&write_args(kernel_payload(
            "current_operating_state",
            "governing",
            "asrt-line-speed",
        )))
        .expect("invocation validates");
        let outcome = evaluate_write(&session, tool, &invocation, "2026-09-06T00:00:00.000Z")
            .expect("evaluation succeeds");
        match outcome {
            WriteOutcome::Rejected(envelope) => {
                assert_eq!(envelope.disposition, "rejected");
                assert_eq!(envelope.authority_state, "denied");
                assert_eq!(envelope.reason_code.as_deref(), Some("authority_denied"));
                assert!(envelope.result_resource_refs.is_none());
            }
            WriteOutcome::Applied { .. } => {
                panic!("a governing claim must never apply through the MCP boundary")
            }
        }
    }

    #[test]
    fn candidate_proposal_applies_with_server_derived_provenance() {
        let session = wired();
        let tool = wired_tool(TOOL_PROPOSE_KERNEL_RECORD).expect("tool present");
        let invocation = validate_write_invocation(&write_args(kernel_payload(
            "candidate_knowledge",
            "candidate",
            "asrt-line-speed",
        )))
        .expect("invocation validates");
        match evaluate_write(&session, tool, &invocation, "2026-09-06T00:00:00.000Z")
            .expect("candidate applies")
        {
            WriteOutcome::Applied {
                kernel, response, ..
            } => {
                assert!(kernel.lifecycle_state.is_candidate_or_working());
                assert_eq!(kernel.authority_class.as_str(), "candidate_knowledge");
                // recorded_at is the server clock, not the client's claim.
                assert_eq!(kernel.recorded_at, "2026-09-06T00:00:00.000Z");
                // The server-derived activity ref rides with the client's
                // claimed provenance.
                assert!(kernel
                    .provenance_refs
                    .iter()
                    .any(|r| r.starts_with("activity:utopia-mcp-token-")));
                assert_eq!(response.disposition, "applied");
            }
            WriteOutcome::Rejected(e) => {
                panic!("a candidate must apply, got {:?} ({e:?})", e.disposition)
            }
        }
    }

    #[test]
    fn objective_operation_is_candidate_never_governing() {
        let session = wired();
        let tool = wired_tool(TOOL_PROPOSE_OBJECTIVE_OPERATION).expect("tool present");
        let invocation =
            validate_write_invocation(&write_args(objective_payload("propose_create")))
                .expect("invocation validates");
        match evaluate_write(&session, tool, &invocation, "2026-09-06T00:00:00.000Z")
            .expect("proposal applies")
        {
            WriteOutcome::Applied { kernel, .. } => {
                assert_eq!(kernel.object_type, "objective");
                assert!(kernel.lifecycle_state.is_candidate_or_working());
            }
            WriteOutcome::Rejected(_) => {
                panic!("propose_create is a proposal, not a transition")
            }
        }
    }

    #[test]
    fn cross_root_payload_is_rejected_before_evaluation() {
        let session = wired();
        let tool = wired_tool(TOOL_PROPOSE_KERNEL_RECORD).expect("tool present");
        let mut payload = kernel_payload("candidate_knowledge", "candidate", "asrt-x");
        payload["governance_root_ref"] = json!("ois:scope:kb:other-root");
        let invocation =
            validate_write_invocation(&write_args(payload)).expect("invocation validates");
        let err = evaluate_write(&session, tool, &invocation, "2026-09-06T00:00:00.000Z")
            .expect_err("cross-root submissions never record");
        assert_eq!(err.reason, "governance_root_mismatch");
    }

    #[test]
    fn structurally_no_mcp_write_reaches_governing() {
        let session = wired();
        // The two valid ways a payload can claim governing: a governing
        // authority/lifecycle pair on a kernel record, or a governing
        // transition action on an objective operation. Both must reject.
        let governing_shapes: Vec<(&str, Value)> = vec![
            (
                TOOL_PROPOSE_KERNEL_RECORD,
                kernel_payload("protected_constraint", "governing", "asrt-x"),
            ),
            (
                TOOL_PROPOSE_OBJECTIVE_OPERATION,
                objective_payload("activate_version"),
            ),
        ];
        for (name, payload) in governing_shapes {
            let tool = wired_tool(name).expect("tool present");
            let invocation =
                validate_write_invocation(&write_args(payload)).expect("invocation validates");
            match evaluate_write(&session, tool, &invocation, "2026-09-06T00:00:00.000Z")
                .expect("evaluation succeeds")
            {
                WriteOutcome::Rejected(_) => {}
                WriteOutcome::Applied { .. } => panic!("{name} applied a governing claim"),
            }
        }
        // Evidence cannot claim governing by construction: its contract has
        // no authority fields, and smuggling them in is a schema violation.
        // Either the payload contract rejects it (fail-closed) or the
        // capability gate denies the governing claim.
        let mut smuggled = evidence_payload();
        smuggled["authority_class"] = json!("protected_constraint");
        let tool = wired_tool(TOOL_ATTACH_EVIDENCE).expect("tool present");
        let invocation =
            validate_write_invocation(&write_args(smuggled)).expect("invocation validates");
        match evaluate_write(&session, tool, &invocation, "2026-09-06T00:00:00.000Z") {
            Ok(WriteOutcome::Rejected(_)) => {}
            Ok(WriteOutcome::Applied { .. }) => panic!("smuggled governing evidence applied"),
            Err(err) => assert_eq!(err.reason, "payload_schema_violation"),
        }
    }

    #[test]
    fn replay_returns_the_recorded_outcome() {
        let session = wired();
        let tool = wired_tool(TOOL_PROPOSE_KERNEL_RECORD).expect("tool present");
        let prior = utopia_store::ois_kernel::RecordedOperation {
            op_id: operation_id(tool, "idem-1"),
            envelope_id: "env-123".to_string(),
            op_kind: TOOL_PROPOSE_KERNEL_RECORD.to_string(),
            actor: "agent:utopia-mcp-token-3".to_string(),
            applied_at: chrono::DateTime::parse_from_rfc3339("2026-09-05T00:00:00Z")
                .expect("parses")
                .with_timezone(&Utc),
        };
        let envelope =
            replay_envelope(tool, "idem-1", &prior, &session, "2026-09-06T00:00:00.000Z");
        assert_eq!(envelope.operation_id, prior.op_id);
        assert_eq!(
            envelope.replay_of_operation_id.as_deref(),
            Some(prior.op_id.as_str())
        );
        assert_eq!(envelope.disposition, "applied");
        assert_eq!(
            envelope.result_resource_refs,
            Some(vec!["env-123".to_string()])
        );
    }

    #[test]
    fn unauthenticated_principal_is_denied_at_the_kernel() {
        let session = wired();
        // The transport always authenticates before wiring; this asserts the
        // kernel gate underneath it fails closed if that ever regressed.
        let unauthenticated = PrincipalBasis {
            principal_ref: session.principal.principal_ref.clone(),
            principal_kind: "agent".to_string(),
            authenticated: false,
        };
        let decision =
            evaluate_capability(&session.profile, &unauthenticated, "propose", &root(), &[]);
        match decision {
            CapabilityDecision::Denied { reason, .. } => {
                assert_eq!(reason, "unauthenticated");
            }
            CapabilityDecision::Authorized { .. } => {
                panic!("an unauthenticated principal must never be authorized")
            }
        }
    }

    #[test]
    fn no_grant_profile_is_denied_fail_closed() {
        let mut session = wired();
        // A token wired without a matching agent grant row.
        session
            .profile
            .grants
            .retain(|g| g.principal_kind != "agent");
        let decision = evaluate_capability(
            &session.profile,
            &session.principal,
            "propose",
            &root(),
            &[],
        );
        match decision {
            CapabilityDecision::Denied { reason, .. } => {
                assert_eq!(reason, "no_matching_grant");
            }
            CapabilityDecision::Authorized { .. } => {
                panic!("a grantless agent must be denied")
            }
        }
    }
}
