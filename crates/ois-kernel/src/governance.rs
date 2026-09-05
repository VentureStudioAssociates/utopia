//! Governed writes — Rust port of `packages/core/src/governance/{profile,events,write}.ts`.
//!
//! Governed write application is the single path through which kernel state
//! becomes governing. Session F R4 semantics, all vocabulary frozen:
//! - Replay of an idempotency key returns the original outcome
//!   (`replay_acknowledged`) and can never emit a second GovernanceEvent.
//! - Authority is server-derived (profile + authenticated Principal); caller
//!   or agent claims of human approval are provenance, never authority.
//! - Stale governing writes return `stale_requires_review` naming the
//!   resolved base — never silently dropped, never silently rebased.
//! - Consequential governing application on a disputed basis defaults to
//!   `pending_approval` with reason `disputed_basis` unless an explicit rule
//!   controls otherwise; the disputed state stays visible throughout.
//! - Every applied transition emits exactly one GovernanceEvent with a churn
//!   key, propagating `originating_decision_ref` when a genuine Decision
//!   exists. No Decision is ever fabricated here.
//!
//! Pure and deterministic: same command, same outcome — including the emitted
//! event's identity.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::canonical::canonical_digest;
use crate::envelope::KernelEnvelope;

/// The acting Principal as derived server-side at operation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalBasis {
    #[serde(rename = "principal_ref")]
    pub principal_ref: String,

    #[serde(rename = "principal_kind")]
    pub principal_kind: String, // 'human' | 'agent'

    /// Server-derived. A claim inside a submitted record never sets this.
    #[serde(rename = "authenticated")]
    pub authenticated: bool,
}

/// One profile grant (frozen `GovernanceProfiles.Grant` shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    #[serde(rename = "principal_kind")]
    pub principal_kind: String, // 'human' | 'agent'

    #[serde(rename = "principal_ref")]
    pub principal_ref: Option<String>,

    #[serde(rename = "capabilities")]
    pub capabilities: Vec<String>,

    #[serde(rename = "governing_authority")]
    pub governing_authority: bool,

    #[serde(
        rename = "delegation_scope_refs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub delegation_scope_refs: Option<Vec<String>>,
}

/// The governance profile binding capabilities to a root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(rename = "schema_version")]
    pub schema_version: String,

    #[serde(rename = "profile_id")]
    pub profile_id: String,

    #[serde(rename = "profile_version")]
    pub profile_version: i64,

    #[serde(rename = "governance_root_ref")]
    pub governance_root_ref: String,

    #[serde(rename = "root_scope_owner_kind")]
    pub root_scope_owner_kind: String,

    #[serde(rename = "grants")]
    pub grants: Vec<Grant>,

    #[serde(rename = "disputed_basis_policy")]
    pub disputed_basis_policy: String, // 'default_pending_approval' | 'explicit_rule_controlled'
}

/// Why a capability evaluation failed. Internal diagnostic — never a wire reason code.
pub type CapabilityDenialReason = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CapabilityDecision {
    Authorized {
        authorized: bool,
        grant: Grant,
    },
    Denied {
        authorized: bool,
        reason: CapabilityDenialReason,
    },
}

/// Capabilities that themselves constitute governing authority when exercised.
pub const GOVERNING_CAPABILITIES: &[&str] = &["establish_governing", "approve"];

fn is_governing_capability(capability: &str) -> bool {
    GOVERNING_CAPABILITIES.contains(&capability)
}

fn matching_grants(profile: &Profile, principal: &PrincipalBasis) -> Vec<Grant> {
    profile
        .grants
        .iter()
        .filter(|grant| {
            (grant.principal_ref.as_deref() == Some(principal.principal_ref.as_str())
                || grant.principal_ref.is_none())
                && grant.principal_kind == principal.principal_kind
        })
        .cloned()
        .collect()
}

/// Delegation coverage: an agent's governing authority applies when the
/// delegation names a scope that covers the target — any target scope ref, or
/// the governance root itself for a root-wide delegation.
pub fn delegation_covers(
    grant: &Grant,
    governance_root_ref: &str,
    target_scope_refs: &[String],
) -> bool {
    let delegation_scopes = grant.delegation_scope_refs.clone().unwrap_or_default();
    if delegation_scopes.iter().any(|s| s == governance_root_ref) {
        return true;
    }
    target_scope_refs
        .iter()
        .any(|scope| delegation_scopes.contains(scope))
}

/// Evaluates whether the server-derived Principal holds `capability` over the
/// target scopes under the profile. Deterministic and pure.
pub fn evaluate_capability(
    profile: &Profile,
    principal: &PrincipalBasis,
    capability: &str,
    governance_root_ref: &str,
    target_scope_refs: &[String],
) -> CapabilityDecision {
    if !principal.authenticated {
        return CapabilityDecision::Denied {
            authorized: false,
            reason: "unauthenticated".to_string(),
        };
    }
    if profile.governance_root_ref != governance_root_ref {
        return CapabilityDecision::Denied {
            authorized: false,
            reason: "profile_root_mismatch".to_string(),
        };
    }
    let grants = matching_grants(profile, principal);
    if grants.is_empty() {
        return CapabilityDecision::Denied {
            authorized: false,
            reason: "no_matching_grant".to_string(),
        };
    }
    for grant in grants {
        if !grant.capabilities.iter().any(|c| c == capability) {
            continue;
        }
        if is_governing_capability(capability) && !grant.governing_authority {
            return CapabilityDecision::Denied {
                authorized: false,
                reason: "governing_authority_required".to_string(),
            };
        }
        if principal.principal_kind == "agent"
            && grant.governing_authority
            && !delegation_covers(&grant, governance_root_ref, target_scope_refs)
        {
            return CapabilityDecision::Denied {
                authorized: false,
                reason: "delegation_scope_mismatch".to_string(),
            };
        }
        return CapabilityDecision::Authorized {
            authorized: true,
            grant,
        };
    }
    CapabilityDecision::Denied {
        authorized: false,
        reason: "capability_not_granted".to_string(),
    }
}

/// Whether consequential governing application on a disputed basis defaults
/// to pending_approval: the profile uses the default policy and no explicit
/// GovernanceRule controls this case (decision freeze).
pub fn disputed_basis_pending(profile: &Profile, explicit_rule_controls: bool) -> bool {
    profile.disputed_basis_policy == "default_pending_approval" && !explicit_rule_controls
}

/// Which governing axis a transition moves (frozen change-dynamics vocabulary).
pub type TransitionAxis = &'static str;

/// The four frozen change classes a GovernanceEvent may carry.
pub type GovernanceChangeClass = String;

/// Classifies an applied transition. Before the subject's observation start,
/// establishment is retroactive knowledge — `knowledge_establishment`, never
/// Change Coverage (acceptance invariant 2). On/after observation start, the
/// transition is governing change on the recorded axis, with recording lag
/// when `recorded_at` exceeds `valid_from`.
pub fn classify_change_class(
    axis: TransitionAxis,
    valid_from: Option<&str>,
    observation_started_at: Option<&str>,
) -> GovernanceChangeClass {
    if let (Some(observation_start), Some(valid_from)) = (observation_started_at, valid_from) {
        if valid_from < observation_start {
            return "knowledge_establishment".to_string();
        }
    }
    match axis {
        "governance_rule" => "governance_rule_change".to_string(),
        "objective_criterion" => "objective_criterion_change".to_string(),
        "kernel_lineage" | "assertion" => "governing_state_change".to_string(),
        _ => "governing_state_change".to_string(),
    }
}

/// Deterministic event object id: content-derived so replaying the same
/// transition derives the same identity.
fn event_id_for(input: &GovernanceEventInput) -> String {
    let digest = canonical_digest(&json!({
        "governance_root_ref": input.governance_root_ref,
        "event_kind": input.event_kind,
        "change_class": input.change_class,
        "applied_ref": input.applied_ref,
        "applied_resource_type": input.applied_resource_type,
        "originating_decision_ref": input.originating_decision_ref,
        "origin_governance_root_ref": input.origin_governance_root_ref,
        "now": input.now,
    }));
    format!("gevt-{}", &digest[..24])
}

/// Input for building the governance_event kernel envelope of an applied
/// transition.
pub struct GovernanceEventInput {
    pub governance_root_ref: String,
    pub event_kind: String, // 'governing_transition' | 'cross_scope_acceptance'
    pub change_class: String,
    /// Ref of the governing resource the event applied (kernel ref convention).
    pub applied_ref: String,
    pub applied_resource_type: String,
    /// Propagated from the Decision result application when one exists.
    pub originating_decision_ref: Option<String>,
    /// Required for cross_scope_acceptance: the foreign origin root.
    pub origin_governance_root_ref: Option<String>,
    pub actor_activity_ref: String,
    /// OIS acceptance instant — the event's recorded_at.
    pub now: String,
    /// Scope bindings carried onto the event record.
    pub scope_refs: Vec<String>,
}

/// Builds the governance_event kernel envelope for an applied transition.
/// The event is a governing record (`current_operating_state` + `governing`)
/// whose `valid_at` is its acceptance instant. `churn_key` is always set —
/// the originating Decision ref when a genuine Decision exists, else the
/// event's own ref. No approval Decision is fabricated here.
pub fn build_governance_event(input: &GovernanceEventInput) -> KernelEnvelope {
    let object_id = event_id_for(input);
    let mut provenance = vec![input.applied_ref.clone(), input.actor_activity_ref.clone()];
    if let Some(decision) = &input.originating_decision_ref {
        provenance.push(decision.clone());
    }
    let mut object = json!({
        "event_kind": input.event_kind,
        "change_class": input.change_class,
        "applied_ref": input.applied_ref,
        "applied_resource_type": input.applied_resource_type,
        "originating_decision_ref": input.originating_decision_ref,
        "churn_key": input
            .originating_decision_ref
            .clone()
            .unwrap_or_else(|| format!("governance_event:{}", object_id)),
        "recorded_at": input.now,
    });
    if input.event_kind == "cross_scope_acceptance" {
        object["origin_governance_root_ref"] = json!(input.origin_governance_root_ref.clone());
    }
    KernelEnvelope {
        schema_version: "1.0.0".to_string(),
        object_type: "governance_event".to_string(),
        object_id,
        governance_root_ref: input.governance_root_ref.clone(),
        scope_refs: if input.scope_refs.is_empty() {
            None
        } else {
            Some(input.scope_refs.clone())
        },
        authority_class: crate::envelope::AuthorityClass::CurrentOperatingState,
        lifecycle_state: crate::envelope::LifecycleState::Governing,
        valid_at: Some(input.now.clone()),
        as_known_at: input.now.clone(),
        recorded_at: input.now.clone(),
        provenance_refs: provenance,
        object,
    }
}

/// Frozen operation dispositions (typed-operation contract).
pub type Disposition = String;

/// A previously recorded operation outcome, found by idempotency key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedOutcome {
    #[serde(rename = "idempotency_key")]
    pub idempotency_key: String,

    #[serde(rename = "disposition")]
    pub disposition: String,

    #[serde(
        rename = "reason_code",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reason_code: Option<String>,

    #[serde(
        rename = "record_ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub record_ref: Option<String>,

    #[serde(
        rename = "governance_event_ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub governance_event_ref: Option<String>,
}

/// The governed-write command: resolver currentness + declared base + the
/// contract-validated next governing version.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct GovernedWriteCommand {
    pub idempotency_key: String,
    pub actor: PrincipalBasis,
    pub profile: Profile,
    /// The resolver's currentness for the target as of the write (serialized shape).
    pub resolved: serde_json::Value,
    /// Declared base: the exact `recorded_at` of the resolved version this
    /// write was built against, or null to establish governing state where
    /// none exists. Optimistic concurrency — never silently rebased.
    pub base_recorded_at: Option<String>,
    /// The contract-validated new governing version.
    pub next: KernelEnvelope,
    /// Ref of a genuine approving Decision, or null. Never fabricated.
    pub originating_decision_ref: Option<String>,
    pub actor_activity_ref: String,
    /// OIS acceptance instant for the new version and its event.
    pub now: String,
    /// Per-subject observation start for retroactive classification.
    pub observation_started_at: Option<String>,
    /// True when an explicit GovernanceRule controls the disputed-basis case.
    pub explicit_rule_controls_disputed_basis: bool,
    /// Prior outcome for this idempotency key, when the service found one.
    pub prior_outcome: Option<RecordedOutcome>,
}

/// The outcome of one governed write application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernedWriteOutcome {
    #[serde(rename = "disposition")]
    pub disposition: String,

    #[serde(
        rename = "reason_code",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reason_code: Option<String>,

    #[serde(
        rename = "transition_class",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub transition_class: Option<String>,

    #[serde(
        rename = "change_class",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub change_class: Option<String>,

    /// The governing record written. Null for evidence refresh — none is written.
    #[serde(rename = "record", default, skip_serializing_if = "Option::is_none")]
    pub record: Option<KernelEnvelope>,

    #[serde(rename = "event", default, skip_serializing_if = "Option::is_none")]
    pub event: Option<KernelEnvelope>,

    #[serde(rename = "churn_key", default, skip_serializing_if = "Option::is_none")]
    pub churn_key: Option<String>,

    /// The base the resolver actually found (stale writes) — review and
    /// resubmit against it.
    #[serde(
        rename = "resolved_base_recorded_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub resolved_base_recorded_at: Option<String>,

    /// The original outcome for a replayed idempotency key.
    #[serde(rename = "original", default, skip_serializing_if = "Option::is_none")]
    pub original: Option<RecordedOutcome>,
}

/// Kernel ref convention for an envelope (`object_type:object_id`).
pub fn kernel_ref(envelope: &KernelEnvelope) -> String {
    envelope.kernel_ref()
}

fn resource_type_for(object_type: &str) -> &'static str {
    match object_type {
        "assertion" => "assertion",
        "objective" => "objective",
        "governance_rule" => "governance_rule",
        "scope" => "scope",
        _ => "kernel_lineage",
    }
}

fn axis_for(object_type: &str) -> TransitionAxis {
    match object_type {
        "governance_rule" => "governance_rule",
        "objective" => "objective_criterion",
        "assertion" => "assertion",
        _ => "kernel_lineage",
    }
}

/// The resolver's currentness state word extracted from the serialized shape.
fn resolved_state_word(resolved: &serde_json::Value) -> String {
    resolved
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// The resolved envelope's recorded_at, when the currentness carries one.
fn resolved_recorded_at(resolved: &serde_json::Value) -> Option<String> {
    resolved
        .get("envelope")
        .and_then(|e| e.get("recorded_at"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Typed error for structurally impossible commands — never silently coerced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedWriteError(pub String);

impl std::fmt::Display for GovernedWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "governed write rejected: {}", self.0)
    }
}

impl std::error::Error for GovernedWriteError {}

/// Guard: the new version must be a coherent governing record of this root.
fn assert_governing_candidate(
    next: &KernelEnvelope,
    governance_root_ref: &str,
    now: &str,
) -> Result<(), GovernedWriteError> {
    if next.governance_root_ref != governance_root_ref {
        return Err(GovernedWriteError(format!(
            "root mismatch: {}",
            next.governance_root_ref
        )));
    }
    if next.recorded_at != now {
        return Err(GovernedWriteError(format!(
            "recorded_at {} does not match acceptance instant {}",
            next.recorded_at, now
        )));
    }
    let governing_lifecycle = next.lifecycle_state.is_governing();
    let governing_authority = next.authority_class.is_governing();
    if !governing_lifecycle || !governing_authority {
        return Err(GovernedWriteError(format!(
            "candidate is not a governing record: {}/{}",
            next.lifecycle_state.as_str(),
            next.authority_class.as_str()
        )));
    }
    Ok(())
}

/// Stale-base detection: the declared base must re-resolve to exactly the
/// resolved current version. Establishing where state exists, or transitioning
/// against a version that no longer re-resolves, is `stale_requires_review` —
/// the outcome names the resolved base so the caller can review and resubmit.
/// Never silently dropped, never silently rebased.
pub fn is_stale_base(
    resolved: &serde_json::Value,
    base_recorded_at: Option<&str>,
) -> Result<(bool, Option<String>), GovernedWriteError> {
    if resolved_state_word(resolved) == "permission_limited" {
        // Cannot verify the base against withheld state — a permission gap is
        // pending review, not staleness (states stay distinct).
        return Ok((false, None));
    }
    let Some(base_recorded_at) = base_recorded_at else {
        return if resolved_state_word(resolved) == "not_established" {
            Ok((false, None))
        } else {
            Ok((true, resolved_recorded_at(resolved)))
        };
    };
    if resolved_state_word(resolved) == "not_established" {
        return Ok((true, None));
    }
    let resolved_at = resolved_recorded_at(resolved);
    let stale = resolved_at.as_deref() != Some(base_recorded_at);
    if stale {
        Ok((true, resolved_at))
    } else {
        // TS returns `{stale: false}` — no base disclosure on the non-stale path.
        Ok((false, None))
    }
}

/// Applies one governed kernel transition. Pure and deterministic: same
/// command, same outcome — including the emitted event's identity.
pub fn apply_governed_write(
    command: &GovernedWriteCommand,
    governance_root_ref: &str,
    target_scope_refs: &[String],
) -> Result<GovernedWriteOutcome, GovernedWriteError> {
    // 1. Idempotent replay: return the original outcome, emit nothing.
    if let Some(prior) = &command.prior_outcome {
        return Ok(GovernedWriteOutcome {
            disposition: "replayed".to_string(),
            reason_code: Some("replay_acknowledged".to_string()),
            transition_class: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: None,
            original: Some(prior.clone()),
        });
    }

    assert_governing_candidate(&command.next, governance_root_ref, &command.now)?;

    // 2. Server-derived authority. Claims of approval are provenance.
    let decision = evaluate_capability(
        &command.profile,
        &command.actor,
        "establish_governing",
        governance_root_ref,
        target_scope_refs,
    );
    let authorized = matches!(
        &decision,
        CapabilityDecision::Authorized {
            authorized: true,
            ..
        }
    );
    if !authorized {
        return Ok(GovernedWriteOutcome {
            disposition: "rejected".to_string(),
            reason_code: Some("insufficient_authority".to_string()),
            transition_class: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: None,
            original: None,
        });
    }

    // 3. Stale base: review, never silent rebase.
    let (stale, resolved_base) =
        is_stale_base(&command.resolved, command.base_recorded_at.as_deref())?;
    if stale {
        return Ok(GovernedWriteOutcome {
            disposition: "stale_requires_review".to_string(),
            reason_code: Some("stale_base_state".to_string()),
            transition_class: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: resolved_base,
            original: None,
        });
    }

    // 4. Permission-limited resolution: withheld state may govern — the write
    //    cannot verify its basis, which is pending review, never a silent apply.
    if resolved_state_word(&command.resolved) == "permission_limited" {
        return Ok(GovernedWriteOutcome {
            disposition: "pending_approval".to_string(),
            reason_code: Some("permission_limited_evidence".to_string()),
            transition_class: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: None,
            original: None,
        });
    }

    // 5. Disputed basis: consequential governing application on disputed state
    //    defaults to pending_approval unless an explicit rule controls it.
    if resolved_state_word(&command.resolved) == "governing_but_disputed"
        && disputed_basis_pending(
            &command.profile,
            command.explicit_rule_controls_disputed_basis,
        )
    {
        return Ok(GovernedWriteOutcome {
            disposition: "pending_approval".to_string(),
            reason_code: Some("disputed_basis".to_string()),
            transition_class: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: None,
            original: None,
        });
    }

    // 6. Applied: the record cites its GovernanceEvent; the event carries the
    //    churn key and any propagated originating Decision ref.
    let axis = axis_for(&command.next.object_type);
    let change_class = classify_change_class(
        axis,
        command.next.valid_at.as_deref(),
        command.observation_started_at.as_deref(),
    );
    let event = build_governance_event(&GovernanceEventInput {
        governance_root_ref: governance_root_ref.to_string(),
        event_kind: "governing_transition".to_string(),
        change_class: change_class.clone(),
        applied_ref: kernel_ref(&command.next),
        applied_resource_type: resource_type_for(&command.next.object_type).to_string(),
        originating_decision_ref: command.originating_decision_ref.clone(),
        origin_governance_root_ref: None,
        actor_activity_ref: command.actor_activity_ref.clone(),
        now: command.now.clone(),
        scope_refs: target_scope_refs.to_vec(),
    });
    let event_ref = kernel_ref(&event);
    let mut record = command.next.clone();
    record.provenance_refs.push(event_ref);
    let churn_key = event
        .object
        .get("churn_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if churn_key.is_empty() {
        return Err(GovernedWriteError(
            "applied transition produced no churn key".to_string(),
        ));
    }
    Ok(GovernedWriteOutcome {
        disposition: "applied".to_string(),
        reason_code: None,
        transition_class: Some("governing_transition".to_string()),
        change_class: Some(change_class),
        record: Some(record),
        event: Some(event),
        churn_key: Some(churn_key),
        resolved_base_recorded_at: None,
        original: None,
    })
}

/// The cross-root acceptance command.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct CrossScopeAcceptanceCommand {
    pub idempotency_key: String,
    pub actor: PrincipalBasis,
    pub profile: Profile,
    /// The foreign snapshot being accepted (foreign root binding).
    pub origin_snapshot: KernelEnvelope,
    pub origin_governance_root_ref: String,
    /// Canonical digest of the previously accepted origin snapshot, if any.
    pub prior_accepted_origin_digest: Option<String>,
    pub originating_decision_ref: Option<String>,
    pub actor_activity_ref: String,
    pub now: String,
    pub observation_started_at: Option<String>,
    pub prior_outcome: Option<RecordedOutcome>,
}

/// Accepts cross-root information into local governance. Cross-root
/// information never inherits local authority: acceptance creates a LOCAL
/// governing record and emits a LOCAL cross_scope_acceptance GovernanceEvent
/// with a churn key. A refresh of the same accepted origin snapshot is
/// evidence refresh — no new governing transition, no event, no churn.
pub fn accept_cross_scope(
    command: &CrossScopeAcceptanceCommand,
    governance_root_ref: &str,
    target_scope_refs: &[String],
) -> Result<GovernedWriteOutcome, GovernedWriteError> {
    if let Some(prior) = &command.prior_outcome {
        return Ok(GovernedWriteOutcome {
            disposition: "replayed".to_string(),
            reason_code: Some("replay_acknowledged".to_string()),
            transition_class: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: None,
            original: Some(prior.clone()),
        });
    }
    if command.origin_snapshot.governance_root_ref != command.origin_governance_root_ref {
        return Err(GovernedWriteError(
            "origin snapshot is not bound to the declared origin root".to_string(),
        ));
    }
    if command.origin_governance_root_ref == governance_root_ref {
        return Err(GovernedWriteError(
            "cross-scope acceptance requires a foreign origin root".to_string(),
        ));
    }

    // Acceptance is a governing act — the local approver's authority applies.
    let decision = evaluate_capability(
        &command.profile,
        &command.actor,
        "approve",
        governance_root_ref,
        target_scope_refs,
    );
    let authorized = matches!(
        &decision,
        CapabilityDecision::Authorized {
            authorized: true,
            ..
        }
    );
    if !authorized {
        return Ok(GovernedWriteOutcome {
            disposition: "rejected".to_string(),
            reason_code: Some("insufficient_authority".to_string()),
            transition_class: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: None,
            original: None,
        });
    }

    let origin_digest = canonical_digest(
        &serde_json::to_value(&command.origin_snapshot)
            .map_err(|_| GovernedWriteError("origin snapshot is not serializable".to_string()))?,
    );
    if command.prior_accepted_origin_digest.as_deref() == Some(origin_digest.as_str()) {
        // Refresh of the same accepted origin snapshot: evidence refresh only —
        // no new governing record, no event, no churn.
        return Ok(GovernedWriteOutcome {
            disposition: "applied".to_string(),
            transition_class: Some("evidence_refresh".to_string()),
            reason_code: None,
            change_class: None,
            record: None,
            event: None,
            churn_key: None,
            resolved_base_recorded_at: None,
            original: None,
        });
    }

    let change_class = classify_change_class(
        "kernel_lineage",
        Some(command.now.as_str()),
        command.observation_started_at.as_deref(),
    );
    let mut local_record = command.origin_snapshot.clone();
    local_record.governance_root_ref = governance_root_ref.to_string();
    if !target_scope_refs.is_empty() {
        local_record.scope_refs = Some(target_scope_refs.to_vec());
    }
    local_record.authority_class = crate::envelope::AuthorityClass::CurrentOperatingState;
    local_record.lifecycle_state = crate::envelope::LifecycleState::Governing;
    local_record.valid_at = Some(command.now.clone());
    local_record.as_known_at = command.now.clone();
    local_record.recorded_at = command.now.clone();
    let mut provenance = command.origin_snapshot.provenance_refs.clone();
    provenance.push(format!(
        "governance_root:{}",
        command.origin_governance_root_ref
    ));
    provenance.push(command.actor_activity_ref.clone());
    local_record.provenance_refs = provenance;

    let event = build_governance_event(&GovernanceEventInput {
        governance_root_ref: governance_root_ref.to_string(),
        event_kind: "cross_scope_acceptance".to_string(),
        change_class: change_class.clone(),
        applied_ref: kernel_ref(&local_record),
        applied_resource_type: resource_type_for(&local_record.object_type).to_string(),
        originating_decision_ref: command.originating_decision_ref.clone(),
        origin_governance_root_ref: Some(command.origin_governance_root_ref.clone()),
        actor_activity_ref: command.actor_activity_ref.clone(),
        now: command.now.clone(),
        scope_refs: target_scope_refs.to_vec(),
    });
    let event_ref = kernel_ref(&event);
    local_record.provenance_refs.push(event_ref);
    let churn_key = event
        .object
        .get("churn_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if churn_key.is_empty() {
        return Err(GovernedWriteError(
            "cross-scope acceptance produced no churn key".to_string(),
        ));
    }
    Ok(GovernedWriteOutcome {
        disposition: "applied".to_string(),
        reason_code: None,
        transition_class: Some("cross_scope_acceptance".to_string()),
        change_class: Some(change_class),
        record: Some(local_record),
        event: Some(event),
        churn_key: Some(churn_key),
        resolved_base_recorded_at: None,
        original: None,
    })
}
