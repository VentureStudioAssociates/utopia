//! The canonical deterministic currentness resolver — Rust port of
//! `packages/core/src/resolver/resolve.ts` (decision freeze: currentness is
//! resolved, never `latest wins`).
//!
//! Pipeline (invariant 4): permission → governance root → as-known time →
//! valid time → scope → authority/lifecycle resolution with explicit
//! supersession and dispute handling. Every filter that drops records emits a
//! diagnostic; nothing is silently absorbed. The resolver is pure: it never
//! reads a clock or a store, so resolving a corpus at a prior `as_known_at`
//! reproduces the prior state exactly. Identical inputs give identical
//! outputs — the parity contract with the TypeScript oracle.

use serde::{Deserialize, Serialize};

use crate::canonical::canonical_json;
use crate::envelope::{AuthorityClass, KernelEnvelope, LifecycleState};

/// Server-derived permission basis the caller resolved before disclosure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionBasis {
    /// `denied`/`unknown` never disclose content (permission-envelope contract).
    #[serde(rename = "decision")]
    pub decision: String,

    #[serde(rename = "disclosure_state")]
    pub disclosure_state: String,

    /// `null` when existence itself is not disclosable — never a lying zero.
    #[serde(rename = "withheld_count")]
    pub withheld_count: Option<i64>,
}

/// Declared applicability basis for the queried subject (applicability contract).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplicabilityBasis {
    #[serde(rename = "applicability_state")]
    pub applicability_state: String,

    /// Mandatory when `applicability_state` is `undetermined` (never collapses).
    #[serde(rename = "undetermined_reason")]
    pub undetermined_reason: Option<String>,

    #[serde(rename = "effective_from")]
    pub effective_from: Option<String>,

    #[serde(rename = "effective_to")]
    pub effective_to: Option<String>,
}

/// The resolution query: which governance root, which two time axes, which
/// principal scope. `as_known_at` truncates recording visibility; `valid_at`
/// bounds force.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionQuery {
    /// Records outside this governance root never participate (no inherited authority).
    #[serde(rename = "governance_root_ref")]
    pub governance_root_ref: String,

    /// As-known cutoff: records recorded later are invisible to this resolution.
    #[serde(rename = "as_known_at")]
    pub as_known_at: String,

    /// Optional validity cutoff. Records with `valid_at` after this instant
    /// are not yet in force; `valid_at: null` on a record means no asserted
    /// validity instant — unbounded, therefore in force.
    #[serde(rename = "valid_at")]
    pub valid_at: Option<String>,

    /// Optional scope context; records outside it are excluded as context omissions.
    #[serde(rename = "scope_refs")]
    pub scope_refs: Option<Vec<String>>,

    /// Optional server-derived permission basis; defaults to fully visible.
    #[serde(rename = "permission")]
    pub permission: Option<PermissionBasis>,

    #[serde(rename = "applicability")]
    pub applicability: Option<ApplicabilityBasis>,
}

/// The record corpus a resolution runs over (all recorded versions, any time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionCorpus {
    /// Every recorded version of the target object id, any recorded_at.
    #[serde(rename = "target_history")]
    pub target_history: Vec<KernelEnvelope>,

    /// Other kernel records that may declare supersession of the target.
    #[serde(
        rename = "related_records",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub related_records: Option<Vec<KernelEnvelope>>,
}

/// Why the object does not currently govern. States stay distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotEstablishedBasis {
    #[serde(rename = "no_visible_record")]
    NoVisibleRecord,
    #[serde(rename = "candidate_or_working_only")]
    CandidateOrWorkingOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Completeness {
    #[serde(rename = "complete")]
    Complete,
    #[serde(rename = "partial_permissions")]
    PartialPermissions,
    #[serde(rename = "partial_context")]
    PartialContext,
}

/// Resolution diagnostics — explicit omissions, never silently absorbed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ResolutionDiagnostic {
    #[serde(rename = "permission_withheld")]
    PermissionWithheld {
        #[serde(rename = "withheld_count")]
        withheld_count: Option<i64>,
    },

    #[serde(rename = "candidate_not_governing")]
    CandidateNotGoverning {
        #[serde(rename = "object_ids")]
        object_ids: Vec<String>,
    },

    #[serde(rename = "future_valid_excluded")]
    FutureValidExcluded {
        #[serde(rename = "recorded_ats")]
        recorded_ats: Vec<String>,
    },

    #[serde(rename = "out_of_scope_excluded")]
    OutOfScopeExcluded { count: u64 },

    #[serde(rename = "cross_root_excluded")]
    CrossRootExcluded { count: u64 },

    #[serde(rename = "applicability_undetermined")]
    ApplicabilityUndetermined { reason: String },

    #[serde(rename = "not_applicable_entire_window")]
    NotApplicableEntireWindow,
}

/// The resolved currentness of one kernel object. Discriminated on the frozen
/// lifecycle vocabulary plus the distinct permission-limited state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum Currentness {
    #[serde(rename = "governing")]
    Governing {
        envelope: KernelEnvelope,
        /// `protected_constraint` | `current_operating_state`.
        authority: String,
        completeness: Completeness,
        diagnostics: Vec<ResolutionDiagnostic>,
    },

    #[serde(rename = "governing_but_disputed")]
    GoverningButDisputed {
        envelope: KernelEnvelope,
        authority: String,
        completeness: Completeness,
        diagnostics: Vec<ResolutionDiagnostic>,
    },

    #[serde(rename = "not_established")]
    NotEstablished {
        basis: NotEstablishedBasis,
        completeness: Completeness,
        diagnostics: Vec<ResolutionDiagnostic>,
    },

    #[serde(rename = "superseded")]
    Superseded {
        envelope: KernelEnvelope,
        /// Object id of the declared governing successor, when known.
        #[serde(rename = "successor_ref")]
        successor_ref: Option<String>,
        completeness: Completeness,
        diagnostics: Vec<ResolutionDiagnostic>,
    },

    #[serde(rename = "retired")]
    Retired {
        envelope: KernelEnvelope,
        completeness: Completeness,
        diagnostics: Vec<ResolutionDiagnostic>,
    },

    #[serde(rename = "deprecated")]
    Deprecated {
        envelope: KernelEnvelope,
        completeness: Completeness,
        diagnostics: Vec<ResolutionDiagnostic>,
    },

    #[serde(rename = "permission_limited")]
    PermissionLimited {
        completeness: Completeness,
        diagnostics: Vec<ResolutionDiagnostic>,
    },
}

impl Currentness {
    /// The frozen lifecycle/state word this outcome carries — the parity
    /// comparison key between runtimes.
    pub fn state_word(&self) -> &'static str {
        match self {
            Self::Governing { .. } => "governing",
            Self::GoverningButDisputed { .. } => "governing_but_disputed",
            Self::NotEstablished { .. } => "not_established",
            Self::Superseded { .. } => "superseded",
            Self::Retired { .. } => "retired",
            Self::Deprecated { .. } => "deprecated",
            Self::PermissionLimited { .. } => "permission_limited",
        }
    }
}

/// Typed error for incoherent resolver input — contract validation is upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverInputError(pub String);

impl std::fmt::Display for ResolverInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "resolver input incoherent: {}", self.0)
    }
}

impl std::error::Error for ResolverInputError {}

/// Instant comparison over ISO-8601 strings (same-length UTC sorts
/// lexicographically — identical byte order to the TypeScript `<` on the
/// ASCII instants the corpus uses).
pub fn at_or_before(instant: &str, cutoff: &str) -> bool {
    instant <= cutoff
}

/// Total deterministic order over versions: `recorded_at` ascending, then
/// canonical content — stable for exact historical reproduction even when two
/// versions share an acceptance instant.
fn by_recording(a: &KernelEnvelope, b: &KernelEnvelope) -> std::cmp::Ordering {
    if a.recorded_at != b.recorded_at {
        return if a.recorded_at < b.recorded_at {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        };
    }
    let a_key = canonical_json(&a.object);
    let b_key = canonical_json(&b.object);
    // Canonical keys are ASCII JSON; byte order matches the oracle's string order.
    a_key.cmp(&b_key)
}

/// Guards the authority/lifecycle coupling the kernel contract enforces
/// (candidate/working cannot carry governing lifecycle, and vice versa).
/// Contract validation is upstream; this fails loud on incoherent input
/// rather than silently resolving nonsense.
fn assert_coherent_corpus(history: &[KernelEnvelope]) -> Result<String, ResolverInputError> {
    let first = history
        .first()
        .ok_or_else(|| ResolverInputError("empty history".to_string()))?;
    let object_id = first.object_id.clone();
    for record in history {
        // Candidate/working records carry candidate/working authority; every
        // other lifecycle (governing, disputed, and the terminal states)
        // carries governing authority. The coupling is bidirectional.
        let candidate_working = record.lifecycle_state.is_candidate_or_working();
        let candidate_authority = record.authority_class.is_candidate_or_working();
        if candidate_working != candidate_authority {
            return Err(ResolverInputError(format!(
                "object {}: lifecycle {} incompatible with authority {}",
                object_id,
                record.lifecycle_state.as_str(),
                record.authority_class.as_str()
            )));
        }
        if record.object_id != object_id {
            return Err(ResolverInputError(format!(
                "history for {} contains record for {}",
                object_id, record.object_id
            )));
        }
    }
    Ok(object_id)
}

/// Splits a history into in-root versions and a foreign-record count.
fn partition_by_root(history: &[KernelEnvelope], root: &str) -> (Vec<KernelEnvelope>, u64) {
    let mut in_root = Vec::new();
    let mut foreign_count: u64 = 0;
    for record in history {
        if record.governance_root_ref == root {
            in_root.push(record.clone());
        } else {
            foreign_count += 1;
        }
    }
    (in_root, foreign_count)
}

/// As-known truncation: only records with `recorded_at` at or before the
/// cutoff, order preserved. The resolver's only temporal visibility rule —
/// an earlier `as_known_at` never sees a later recording.
pub fn slice_as_known(records: &[KernelEnvelope], as_known_at: &str) -> Vec<KernelEnvelope> {
    records
        .iter()
        .filter(|record| at_or_before(&record.recorded_at, as_known_at))
        .cloned()
        .collect()
}

fn scope_intersects(record: &KernelEnvelope, scopes: &[String]) -> bool {
    let record_scopes = record.scope_refs.clone().unwrap_or_default();
    // A record without scope bindings applies root-wide.
    if record_scopes.is_empty() {
        return true;
    }
    record_scopes.iter().any(|scope| scopes.contains(scope))
}

/// Picks the newest record carrying a governing-capable lifecycle, or None.
/// Candidate/working versions never govern and never displace a governing
/// version: a candidate recorded later does not change currentness.
pub fn newest_governing_version(versions: &[KernelEnvelope]) -> Option<KernelEnvelope> {
    for candidate in versions.iter().rev() {
        if candidate.lifecycle_state.is_governing() {
            return Some(candidate.clone());
        }
    }
    None
}

/// Supersession by declaration: a governing record — the newest version of
/// the target itself, or a related successor — that names the target's
/// identity in `superseded_assertion_ref` displaces it. Related successors
/// must be visible at the as-known cutoff and share the governance root (no
/// inherited authority across roots).
fn declared_supersession(
    newest: &KernelEnvelope,
    target_object_id: &str,
    related: &[KernelEnvelope],
    query: &ResolutionQuery,
) -> Option<KernelEnvelope> {
    let target_ref = format!("{}:{}", newest.object_type, target_object_id);
    let names_target = |ref_value: Option<&str>| -> bool {
        match ref_value {
            Some(r) => r == target_object_id || r == target_ref,
            None => false,
        }
    };
    if names_target(newest.superseded_assertion_ref()) {
        return Some(newest.clone());
    }
    for record in slice_as_known(related, &query.as_known_at) {
        if record.governance_root_ref != query.governance_root_ref {
            continue;
        }
        if !record.lifecycle_state.is_governing() {
            continue;
        }
        if names_target(record.superseded_assertion_ref()) {
            return Some(record.clone());
        }
    }
    None
}

/// Declared successor of a terminal version: the version's own supersession
/// reference, or a governing related record that supersedes this object.
fn declared_successor(envelope: &KernelEnvelope, related: &[KernelEnvelope]) -> Option<String> {
    if let Some(own) = envelope.superseded_assertion_ref() {
        if !own.is_empty() {
            return Some(own.to_string());
        }
    }
    for record in related {
        if record.governance_root_ref != envelope.governance_root_ref {
            continue;
        }
        if !record.lifecycle_state.is_governing() {
            continue;
        }
        if record.superseded_assertion_ref() == Some(envelope.object_id.as_str()) {
            return Some(record.object_id.clone());
        }
    }
    None
}

fn applicability_diagnostics(
    basis: &Option<ApplicabilityBasis>,
) -> Result<Vec<ResolutionDiagnostic>, ResolverInputError> {
    let Some(basis) = basis else {
        return Ok(vec![]);
    };
    if basis.applicability_state == "undetermined" {
        let reason = basis.undetermined_reason.clone().unwrap_or_default();
        if reason.is_empty() {
            return Err(ResolverInputError(
                "undetermined applicability requires undetermined_reason".to_string(),
            ));
        }
        return Ok(vec![ResolutionDiagnostic::ApplicabilityUndetermined {
            reason,
        }]);
    }
    if basis.applicability_state == "not_applicable_entire_window" {
        return Ok(vec![ResolutionDiagnostic::NotApplicableEntireWindow]);
    }
    Ok(vec![])
}

fn finish_governing(
    envelope: KernelEnvelope,
    completeness: Completeness,
    diagnostics: Vec<ResolutionDiagnostic>,
) -> Currentness {
    let authority = if envelope.authority_class == AuthorityClass::ProtectedConstraint {
        "protected_constraint".to_string()
    } else {
        "current_operating_state".to_string()
    };
    if envelope.lifecycle_state == LifecycleState::GoverningButDisputed {
        // Disputes stay visible as their own state — never coerced or suppressed.
        Currentness::GoverningButDisputed {
            envelope,
            authority,
            completeness,
            diagnostics,
        }
    } else {
        Currentness::Governing {
            envelope,
            authority,
            completeness,
            diagnostics,
        }
    }
}

/// Resolves the current state of one kernel object from its append-only
/// version history. Deterministic and pure — the same corpus and query always
/// produce the same result, and a prior `as_known_at` reproduces the prior
/// state exactly (nothing recorded later can participate).
pub fn resolve_current_state(
    corpus: &ResolutionCorpus,
    query: &ResolutionQuery,
) -> Result<Currentness, ResolverInputError> {
    let mut diagnostics: Vec<ResolutionDiagnostic> = Vec::new();
    let permission = &query.permission;

    // Permission precedes disclosure: any non-granted basis limits the
    // outcome itself. An object whose existence is not disclosable is never
    // reported as plainly not-established (no lying zero), and a
    // permission-limited view never masquerades as the underlying state —
    // even when some content is visible (restricted_existence_count_only).
    if let Some(permission) = permission {
        if permission.decision != "granted" {
            diagnostics.push(ResolutionDiagnostic::PermissionWithheld {
                withheld_count: permission.withheld_count,
            });
            return Ok(Currentness::PermissionLimited {
                completeness: Completeness::PartialPermissions,
                diagnostics,
            });
        }
    }

    // An object with no recorded versions at all was never established — a
    // legitimate resolution outcome, not an input error.
    if corpus.target_history.is_empty() {
        return Ok(Currentness::NotEstablished {
            basis: NotEstablishedBasis::NoVisibleRecord,
            completeness: Completeness::Complete,
            diagnostics,
        });
    }

    assert_coherent_corpus(&corpus.target_history)?;
    let (in_root, foreign_count) =
        partition_by_root(&corpus.target_history, &query.governance_root_ref);
    if foreign_count > 0 {
        diagnostics.push(ResolutionDiagnostic::CrossRootExcluded {
            count: foreign_count,
        });
    }
    diagnostics.extend(applicability_diagnostics(&query.applicability)?);

    let visible = slice_as_known(&in_root, &query.as_known_at);

    // Valid-time scoping: versions asserting validity after the queried
    // instant are not yet in force. Null `valid_at` is unbounded — in force.
    let mut in_force: Vec<KernelEnvelope> = Vec::new();
    let mut future_valid: Vec<String> = Vec::new();
    for record in visible {
        let record_valid = record.valid_at.clone();
        if let (Some(query_valid), Some(record_valid)) = (&query.valid_at, record_valid) {
            if !at_or_before(&record_valid, query_valid) {
                future_valid.push(record.recorded_at.clone());
                continue;
            }
        }
        in_force.push(record);
    }
    if !future_valid.is_empty() {
        diagnostics.push(ResolutionDiagnostic::FutureValidExcluded {
            recorded_ats: future_valid,
        });
    }

    // Scope context: out-of-scope versions are context omissions (partial_context).
    let mut in_scope: Vec<KernelEnvelope> = Vec::new();
    let mut out_of_scope_count: u64 = 0;
    let scope_refs_nonempty = query
        .scope_refs
        .as_ref()
        .is_some_and(|scopes| !scopes.is_empty());
    for record in in_force {
        if scope_refs_nonempty && !scope_intersects(&record, query.scope_refs.as_ref().unwrap()) {
            out_of_scope_count += 1;
            continue;
        }
        in_scope.push(record);
    }
    if out_of_scope_count > 0 {
        diagnostics.push(ResolutionDiagnostic::OutOfScopeExcluded {
            count: out_of_scope_count,
        });
    }

    // Completeness: scope omissions are partial_context. (Permission gaps
    // never reach here — they short-circuit above.)
    let completeness = if out_of_scope_count > 0 {
        Completeness::PartialContext
    } else {
        Completeness::Complete
    };

    if in_scope.is_empty() {
        return Ok(Currentness::NotEstablished {
            basis: NotEstablishedBasis::NoVisibleRecord,
            completeness,
            diagnostics,
        });
    }

    let mut ordered = in_scope;
    ordered.sort_by(by_recording);
    let newest = ordered
        .last()
        .ok_or_else(|| {
            ResolverInputError("non-empty history resolved to no newest version".to_string())
        })?
        .clone();

    // A candidate/working version recorded later cannot govern and cannot
    // displace the governing version it was proposed against.
    if newest.lifecycle_state.is_candidate_or_working() {
        let governing = newest_governing_version(&ordered);
        diagnostics.push(ResolutionDiagnostic::CandidateNotGoverning {
            object_ids: ordered
                .iter()
                .filter(|record| record.lifecycle_state.is_candidate_or_working())
                .map(|record| record.object_id.clone())
                .collect(),
        });
        if governing.is_none() {
            return Ok(Currentness::NotEstablished {
                basis: NotEstablishedBasis::CandidateOrWorkingOnly,
                completeness,
                diagnostics,
            });
        }
        return Ok(finish_governing(
            governing.unwrap(),
            completeness,
            diagnostics,
        ));
    }

    if newest.lifecycle_state.is_governing() {
        // A governing record that declares supersession of the target — the
        // newest version itself or a related successor — displaces it.
        let superseding = declared_supersession(
            &newest,
            &newest.object_id,
            corpus.related_records.as_deref().unwrap_or(&[]),
            query,
        );
        if let Some(superseding) = superseding {
            return Ok(Currentness::Superseded {
                successor_ref: Some(superseding.object_id.clone()),
                envelope: superseding,
                completeness,
                diagnostics,
            });
        }
        return Ok(finish_governing(newest, completeness, diagnostics));
    }

    // Newest version is terminal: superseded | retired | deprecated. The
    // object no longer governs; a declared successor is surfaced when known.
    let successor = declared_successor(&newest, corpus.related_records.as_deref().unwrap_or(&[]));
    match newest.lifecycle_state {
        LifecycleState::Superseded => Ok(Currentness::Superseded {
            envelope: newest,
            successor_ref: successor,
            completeness,
            diagnostics,
        }),
        LifecycleState::Retired => Ok(Currentness::Retired {
            envelope: newest,
            completeness,
            diagnostics,
        }),
        _ => Ok(Currentness::Deprecated {
            envelope: newest,
            completeness,
            diagnostics,
        }),
    }
}
