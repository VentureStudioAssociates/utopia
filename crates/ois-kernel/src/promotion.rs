//! Evidence-aware projection-edge promotion — Rust port of
//! `packages/work/src/promotion.ts` (r1-governance-verification contract v1,
//! projection_edge_promotion concern).
//!
//! Promotion is itemwise and payload-bound: a candidate_knowledge assertion
//! carrying a projection trait family becomes governing only through exact
//! evidence — every binding names the evidence ref, its digest, and the edge
//! endpoint(s) it supports. Exact disposition map (frozen):
//! sufficient_current + authorized => applied/evidence_sufficient;
//! insufficient or binding/endpoint mismatch => rejected;
//! sufficient_but_stale => stale_requires_review/stale_evidence; conflicting
//! => pending_approval/disputed_evidence; permission_limited =>
//! pending_approval/permission_limited_evidence; error => rejected/
//! evaluator_error; sufficient evidence + pending authority =>
//! pending_approval/insufficient_authority; denied authority =>
//! rejected/authority_denied. Evidence sufficiency never grants authority.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::envelope::KernelEnvelope;
use crate::governance::{
    apply_governed_write, evaluate_capability, is_stale_base, CapabilityDecision,
    GovernedWriteError, GovernedWriteOutcome, PrincipalBasis, Profile,
};

/// One evidence record as stored: the ground truth a binding must match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    #[serde(rename = "evidence_ref")]
    pub evidence_ref: String,

    /// The exact content digest of the stored evidence.
    #[serde(rename = "digest")]
    pub digest: String,

    /// A disputed evidence record stays visible as conflicting evidence.
    #[serde(rename = "disputed", default)]
    pub disputed: bool,

    /// The evidence could not be evaluated for the acting principal.
    #[serde(rename = "permission_limited", default)]
    pub permission_limited: bool,

    /// The evaluator failed while assessing this evidence.
    #[serde(rename = "evaluator_error", default)]
    pub evaluator_error: bool,
}

/// The current state of one edge endpoint as resolved from storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointState {
    #[serde(rename = "endpoint_ref")]
    pub endpoint_ref: String,

    /// Digest of the endpoint's current artifact version.
    #[serde(rename = "current_digest")]
    pub current_digest: String,

    /// Refs of declared containing artifacts (implementation containers).
    #[serde(rename = "containing_artifact_refs", default)]
    pub containing_artifact_refs: Vec<String>,
}

/// Inputs for the deterministic analyzer establishment path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyzerOracle {
    #[serde(rename = "analyzer_ref")]
    pub analyzer_ref: String,

    #[serde(rename = "analyzer_version")]
    pub analyzer_version: String,

    /// The declared checkpoint the analyzer ran at.
    #[serde(rename = "checkpoint_ref")]
    pub checkpoint_ref: String,

    /// The ArtifactVersionRef the analyzer read.
    #[serde(rename = "artifact_version_ref")]
    pub artifact_version_ref: String,

    #[serde(rename = "artifact_version_digest")]
    pub artifact_version_digest: String,
}

/// One evidence binding (frozen `R1GovernanceVerification.EvidenceBinding`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceBinding {
    #[serde(rename = "evidence_ref")]
    pub evidence_ref: String,

    #[serde(rename = "evidence_digest")]
    pub evidence_digest: String,

    #[serde(rename = "bound_endpoint_refs")]
    pub bound_endpoint_refs: Vec<String>,
}

/// One promotion item command.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PromotionItemCommand {
    pub assertion_ref: String,
    pub trait_family: String, // 'implementation_edge' | 'impact_dependency_edge' | ...
    /// The candidate assertion's current envelope (candidate_knowledge).
    pub candidate: KernelEnvelope,
    /// The resolver's currentness for the candidate assertion (serialized shape).
    pub candidate_resolved: serde_json::Value,
    pub edge_endpoint_refs: Vec<String>,
    pub evidence_bindings: Vec<EvidenceBinding>,
    pub evidence_policy_ref: String,
    pub confirmation_activity_ref: String,
    pub confirmation_principal_ref: String,
    /// Present only when a deterministic analyzer establishes the edge.
    pub analyzer: Option<AnalyzerOracle>,
    /// A governing Decision/GovernanceEvent establishing the dependency, if any.
    pub dependency_legal_lineage_refs: Vec<String>,
}

/// A promotion batch command.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PromotionBatchCommand {
    pub actor: PrincipalBasis,
    pub actor_activity_ref: String,
    pub profile: Profile,
    pub governance_root_ref: String,
    pub target_scope_refs: Vec<String>,
    pub items: Vec<PromotionItemCommand>,
    /// Evidence catalog: the records bindings cite (ref -> record).
    pub evidence: BTreeMap<String, EvidenceRecord>,
    /// Current endpoint state as resolved by the storage layer.
    pub endpoints: BTreeMap<String, EndpointState>,
    /// The governing record each promotion produces (governing assertion version).
    pub next_by_assertion: BTreeMap<String, KernelEnvelope>,
    /// Per-subject observation start for retroactive classification.
    pub observation_started_at: Option<String>,
    /// OIS acceptance instant for applied promotions.
    pub now: String,
}

/// The itemwise promotion outcome: disposition + reason + applied transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionItemOutcome {
    #[serde(rename = "assertion_ref")]
    pub assertion_ref: String,

    #[serde(rename = "trait_family")]
    pub trait_family: String,

    #[serde(rename = "promotion_disposition")]
    pub promotion_disposition: String, // 'applied' | 'pending_approval' | 'rejected' | 'stale_requires_review'

    #[serde(rename = "reason_code")]
    pub reason_code: String,

    #[serde(rename = "evidence_state")]
    pub evidence_state: String,

    #[serde(rename = "authority_state")]
    pub authority_state: String,

    #[serde(rename = "confirmation_mode")]
    pub confirmation_mode: String,

    /// The applied governed-write outcome (event + churn key), when applied.
    #[serde(rename = "applied", default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<AppliedSummary>,
}

/// Compact summary of an applied promotion's governed write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedSummary {
    #[serde(rename = "churn_key")]
    pub churn_key: String,

    #[serde(
        rename = "change_class",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub change_class: Option<String>,

    #[serde(rename = "event_ref", default, skip_serializing_if = "Option::is_none")]
    pub event_ref: Option<String>,
}

/// Classifies the evidence state for one promotion item deterministically.
/// Ordered so the most informative defect surfaces first; every branch is a
/// frozen-contract state, never a coerced one.
pub fn classify_evidence_state(
    command: &PromotionItemCommand,
    evidence: &BTreeMap<String, EvidenceRecord>,
    endpoints: &BTreeMap<String, EndpointState>,
) -> (String, Option<String>) {
    let bindings = &command.evidence_bindings;

    // Minimum evidence: zero bindings is the evidence-free rejection.
    if bindings.is_empty() {
        return (
            "insufficient".to_string(),
            Some("insufficient_evidence".to_string()),
        );
    }

    // Evaluator failure dominates: the evidence could not be assessed.
    if bindings.iter().any(|b| {
        evidence
            .get(&b.evidence_ref)
            .is_some_and(|r| r.evaluator_error)
    }) {
        return ("error".to_string(), Some("evaluator_error".to_string()));
    }

    // Permission-limited evidence: existence or content withheld for this
    // principal — held for approval, never silently treated as absent.
    if bindings.iter().any(|b| {
        evidence
            .get(&b.evidence_ref)
            .is_some_and(|r| r.permission_limited)
    }) {
        return (
            "permission_limited".to_string(),
            Some("permission_limited_evidence".to_string()),
        );
    }

    // Binding integrity: each binding must cite a known evidence record with
    // its exact digest — a binding that lies about the evidence is a mismatch.
    for binding in bindings {
        match evidence.get(&binding.evidence_ref) {
            None => {
                return (
                    "conflicting".to_string(),
                    Some("evidence_binding_mismatch".to_string()),
                )
            }
            Some(record) => {
                if record.digest != binding.evidence_digest {
                    return (
                        "conflicting".to_string(),
                        Some("evidence_binding_mismatch".to_string()),
                    );
                }
            }
        }
    }

    // Structural endpoint rules per trait family.
    let mut accepted_endpoints: Vec<String> = command.edge_endpoint_refs.clone();
    for (endpoint_ref, state) in endpoints {
        if command.edge_endpoint_refs.contains(endpoint_ref) {
            for container in &state.containing_artifact_refs {
                if !accepted_endpoints.contains(container) {
                    accepted_endpoints.push(container.clone());
                }
            }
        }
    }

    if command.trait_family == "implementation_edge" {
        // Every binding must touch the implementation endpoint or a declared
        // containing Artifact boundary; an unrelated current artifact (for
        // example a README) cannot establish the edge.
        let well_bound = bindings.iter().all(|binding| {
            binding
                .bound_endpoint_refs
                .iter()
                .any(|endpoint| accepted_endpoints.contains(endpoint))
        });
        if !well_bound {
            return (
                "conflicting".to_string(),
                Some("endpoint_binding_mismatch".to_string()),
            );
        }
    }

    if command.trait_family == "impact_dependency_edge" {
        // Bindings must collectively cover both endpoints unless a governing
        // Decision/GovernanceEvent establishes the dependency.
        let established_by_decision = !command.dependency_legal_lineage_refs.is_empty();
        if !established_by_decision {
            let covers = command.edge_endpoint_refs.iter().all(|endpoint| {
                bindings
                    .iter()
                    .any(|b| b.bound_endpoint_refs.contains(endpoint))
            });
            if !covers {
                return (
                    "insufficient".to_string(),
                    Some("insufficient_evidence".to_string()),
                );
            }
        }
    }

    // Disputed evidence: a disputed record keeps the conflict visible.
    if bindings
        .iter()
        .any(|b| evidence.get(&b.evidence_ref).is_some_and(|r| r.disputed))
    {
        return (
            "conflicting".to_string(),
            Some("disputed_evidence".to_string()),
        );
    }

    // Currency: evidence for a prior artifact version is stale — the current
    // version must be re-established, never silently accepted.
    let stale = bindings.iter().any(|binding| {
        binding.bound_endpoint_refs.iter().any(|endpoint| {
            endpoints
                .get(endpoint)
                .is_some_and(|state| state.current_digest != binding.evidence_digest)
        })
    });
    if stale {
        return (
            "sufficient_but_stale".to_string(),
            Some("stale_evidence".to_string()),
        );
    }

    ("sufficient_current".to_string(), None)
}

/// Whether this item attempts deterministic analyzer establishment, and if so
/// whether that attempt is allowed. Permitted only for implementation_edge;
/// any other trait is rejected/oracle_not_allowed_for_trait.
pub fn analyzer_establishment_defect(command: &PromotionItemCommand) -> (bool, Option<String>) {
    match &command.analyzer {
        None => (false, None),
        Some(_) => {
            if command.trait_family == "implementation_edge" {
                (true, None)
            } else {
                (true, Some("oracle_not_allowed_for_trait".to_string()))
            }
        }
    }
}

/// Whether the analyzer's ArtifactVersionRef evidence is current. Oracles
/// establish only over current ArtifactVersionRef evidence at the declared
/// checkpoint; a digest for a prior version is stale evidence.
pub fn analyzer_evidence_stale(
    command: &PromotionItemCommand,
    endpoints: &BTreeMap<String, EndpointState>,
) -> bool {
    let Some(analyzer) = &command.analyzer else {
        return false;
    };
    command.edge_endpoint_refs.iter().any(|endpoint| {
        endpoints
            .get(endpoint)
            .is_some_and(|state| state.current_digest != analyzer.artifact_version_digest)
    })
}

/// Promotes projection edges itemwise. Every item receives its exact frozen
/// outcome; the batch may partially succeed. Applied items emit exactly one
/// GovernanceEvent with a churn key (through the single governed-write path)
/// and a governing_state_change Work Delta. Pure: persistence sits above.
pub fn promote_projection_edges(
    command: &PromotionBatchCommand,
) -> Result<Vec<PromotionItemOutcome>, GovernedWriteError> {
    // Server-derived authority is evaluated once and reported exactly on
    // every outcome: evidence sufficiency never grants authority.
    let decision = evaluate_capability(
        &command.profile,
        &command.actor,
        "establish_governing",
        &command.governance_root_ref,
        &command.target_scope_refs,
    );
    let authorized = matches!(
        &decision,
        CapabilityDecision::Authorized {
            authorized: true,
            ..
        }
    );
    let unauthenticated = matches!(
        &decision,
        CapabilityDecision::Denied { authorized: false, reason }
            if reason == "unauthenticated"
    );
    let authority_state = if authorized {
        "authorized".to_string()
    } else if unauthenticated {
        "denied".to_string()
    } else {
        "pending_required_authority".to_string()
    };

    let mut outcomes = Vec::new();
    for item in &command.items {
        // Zero bindings is structurally impossible: the frozen Promotion payload
        // requires at least one exact evidence binding (minItems 1), so an
        // evidence-free attempt has no disposition record to render — the
        // oracle rejects it as a typed structural error (R1-T09 parity).
        if item.evidence_bindings.is_empty() {
            return Err(GovernedWriteError(
                "promotion item carries no exact evidence binding (minItems 1)".to_string(),
            ));
        }

        let confirmation_mode = confirmation_mode(item);
        // Deterministic analyzer establishment: only for implementation_edge.
        let (attempting, analyzer_defect) = analyzer_establishment_defect(item);
        if let Some(defect) = analyzer_defect {
            let (state, _) = classify_evidence_state(item, &command.evidence, &command.endpoints);
            outcomes.push(PromotionItemOutcome {
                assertion_ref: item.assertion_ref.clone(),
                trait_family: item.trait_family.clone(),
                promotion_disposition: "rejected".to_string(),
                reason_code: defect,
                evidence_state: state,
                authority_state: authority_state.clone(),
                confirmation_mode,
                applied: None,
            });
            continue;
        }

        let (state, defect) = classify_evidence_state(item, &command.evidence, &command.endpoints);
        if let Some(defect) = defect {
            let outcome = match defect.as_str() {
                "insufficient_evidence"
                | "evidence_binding_mismatch"
                | "endpoint_binding_mismatch"
                | "evaluator_error" => PromotionItemOutcome {
                    assertion_ref: item.assertion_ref.clone(),
                    trait_family: item.trait_family.clone(),
                    promotion_disposition: "rejected".to_string(),
                    reason_code: defect,
                    evidence_state: state,
                    authority_state: authority_state.clone(),
                    confirmation_mode,
                    applied: None,
                },
                "stale_evidence" => PromotionItemOutcome {
                    // Stale evidence is review, not rejection: the evidence was
                    // real but no longer current — re-establish against the
                    // current version.
                    assertion_ref: item.assertion_ref.clone(),
                    trait_family: item.trait_family.clone(),
                    promotion_disposition: "stale_requires_review".to_string(),
                    reason_code: "stale_evidence".to_string(),
                    evidence_state: "sufficient_but_stale".to_string(),
                    authority_state: authority_state.clone(),
                    confirmation_mode,
                    applied: None,
                },
                "disputed_evidence" => PromotionItemOutcome {
                    assertion_ref: item.assertion_ref.clone(),
                    trait_family: item.trait_family.clone(),
                    promotion_disposition: "pending_approval".to_string(),
                    reason_code: "disputed_evidence".to_string(),
                    evidence_state: "conflicting".to_string(),
                    authority_state: authority_state.clone(),
                    confirmation_mode,
                    applied: None,
                },
                "permission_limited_evidence" => PromotionItemOutcome {
                    assertion_ref: item.assertion_ref.clone(),
                    trait_family: item.trait_family.clone(),
                    promotion_disposition: "pending_approval".to_string(),
                    reason_code: "permission_limited_evidence".to_string(),
                    evidence_state: "permission_limited".to_string(),
                    authority_state: authority_state.clone(),
                    confirmation_mode,
                    applied: None,
                },
                _ => {
                    return Err(GovernedWriteError(format!(
                        "unmapped promotion defect: {}",
                        defect
                    )))
                }
            };
            outcomes.push(outcome);
            continue;
        }

        // Evidence is sufficient_current. The analyzer path additionally
        // requires its ArtifactVersionRef evidence to be current.
        if attempting && analyzer_evidence_stale(item, &command.endpoints) {
            outcomes.push(PromotionItemOutcome {
                assertion_ref: item.assertion_ref.clone(),
                trait_family: item.trait_family.clone(),
                promotion_disposition: "stale_requires_review".to_string(),
                reason_code: "stale_evidence".to_string(),
                evidence_state: "sufficient_but_stale".to_string(),
                authority_state: authority_state.clone(),
                confirmation_mode,
                applied: None,
            });
            continue;
        }

        // Authority decides the rest — evidence never grants it.
        if !authorized {
            let (disposition, reason) = if unauthenticated {
                ("rejected".to_string(), "authority_denied".to_string())
            } else {
                (
                    "pending_approval".to_string(),
                    "insufficient_authority".to_string(),
                )
            };
            outcomes.push(PromotionItemOutcome {
                assertion_ref: item.assertion_ref.clone(),
                trait_family: item.trait_family.clone(),
                promotion_disposition: disposition,
                reason_code: reason,
                evidence_state: "sufficient_current".to_string(),
                authority_state: authority_state.clone(),
                confirmation_mode,
                applied: None,
            });
            continue;
        }

        // Stale candidate base: promoting over already-resolved (governing,
        // superseded, retired, ...) state is review, never silent rebase. The
        // frozen promotion vocabulary reports base staleness as stale_evidence
        // — the candidate evidence is stale relative to current governing state.
        let (base_stale, _) = is_stale_base(&item.candidate_resolved, None)?;
        if base_stale {
            outcomes.push(PromotionItemOutcome {
                assertion_ref: item.assertion_ref.clone(),
                trait_family: item.trait_family.clone(),
                promotion_disposition: "stale_requires_review".to_string(),
                reason_code: "stale_evidence".to_string(),
                evidence_state: "sufficient_but_stale".to_string(),
                authority_state: authority_state.clone(),
                confirmation_mode,
                applied: None,
            });
            continue;
        }

        // Applied: promote through the single governed-write path.
        let Some(next) = command.next_by_assertion.get(&item.assertion_ref) else {
            return Err(GovernedWriteError(format!(
                "applied promotion requires the governing version envelope for {}",
                item.assertion_ref
            )));
        };
        let outcome: GovernedWriteOutcome = apply_governed_write(
            &crate::governance::GovernedWriteCommand {
                idempotency_key: format!("promote:{}:{}", item.assertion_ref, item.trait_family),
                actor: command.actor.clone(),
                profile: command.profile.clone(),
                resolved: item.candidate_resolved.clone(),
                // Establishing governing state where none exists: the candidate
                // was never governing, so there is no base to declare.
                base_recorded_at: None,
                next: next.clone(),
                originating_decision_ref: None,
                actor_activity_ref: command.actor_activity_ref.clone(),
                now: command.now.clone(),
                observation_started_at: command.observation_started_at.clone(),
                explicit_rule_controls_disputed_basis: true,
                prior_outcome: None,
            },
            &command.governance_root_ref,
            &command.target_scope_refs,
        )?;
        if outcome.disposition != "applied" {
            return Err(GovernedWriteError(format!(
                "authorized promotion over current evidence must apply, got {}",
                outcome.disposition
            )));
        }
        let churn_key = outcome.churn_key.clone().ok_or_else(|| {
            GovernedWriteError("applied promotion produced no churn key".to_string())
        })?;
        outcomes.push(PromotionItemOutcome {
            assertion_ref: item.assertion_ref.clone(),
            trait_family: item.trait_family.clone(),
            promotion_disposition: "applied".to_string(),
            reason_code: "evidence_sufficient".to_string(),
            evidence_state: "sufficient_current".to_string(),
            authority_state: "authorized".to_string(),
            confirmation_mode,
            applied: Some(AppliedSummary {
                churn_key,
                change_class: outcome.change_class.clone(),
                event_ref: outcome.event.as_ref().map(|e| e.kernel_ref()),
            }),
        });
    }
    Ok(outcomes)
}

fn confirmation_mode(item: &PromotionItemCommand) -> String {
    if item.analyzer.is_some() {
        "deterministic_establishment".to_string()
    } else {
        "single_evidence_aware".to_string()
    }
}

/// The Promotion payload record for one item (exactly one concern per payload) —
/// the wire shape the contract freezes.
pub fn promotion_record(item: &PromotionItemCommand) -> serde_json::Value {
    let bindings_json: Vec<serde_json::Value> =
        item.evidence_bindings.iter().map(|b| json!(b)).collect();
    let mut record = json!({
        "assertion_ref": item.assertion_ref,
        "trait_family": item.trait_family,
        "candidate_state": "candidate_knowledge",
        "edge_endpoint_refs": item.edge_endpoint_refs,
        "evidence_bindings": bindings_json,
        "evidence_policy_ref": item.evidence_policy_ref,
        "confirmation_activity_ref": item.confirmation_activity_ref,
        "confirmation_principal_ref": item.confirmation_principal_ref,
    });
    if let Some(analyzer) = &item.analyzer {
        record["deterministic_oracle_ref"] = json!(analyzer.analyzer_ref);
        record["deterministic_oracle_version"] = json!(analyzer.analyzer_version);
        record["artifact_version_ref"] = json!(analyzer.artifact_version_ref);
        record["artifact_version_digest"] = json!(analyzer.artifact_version_digest);
    }
    record
}
