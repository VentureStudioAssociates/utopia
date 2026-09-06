//! Objective records and criterion-level resolution — the Rust port of
//! `packages/core/src/resolver/criterion.ts` plus the `objective` object
//! definition from `packages/contracts/schemas/v1/kernel.schema.json`, at the
//! oracle pin (`second-brain @ frontier/ois-v1 c56f8e5`).
//!
//! Criterion-level resolution is the concrete partial-supersession surface of
//! the frozen contracts: a `criteria_change` may modify one criterion
//! (identity retained, version incremented) while other criteria are
//! untouched. The modified criterion's prior version is superseded; retained
//! criteria are NOT — the criterion set was only partially superseded.
//!
//! Deterministic and pure: the criterion's state is read from the newest
//! governing Objective version, never from the newest record — candidate
//! Objective revisions do not change criterion currentness. The caller passes
//! the objective's full version history in the desired as-known visibility
//! (slice with [`crate::resolver::slice_as_known`] first).

use crate::envelope::KernelEnvelope;
use crate::resolver::{newest_governing_version, ResolverInputError};
use serde::{Deserialize, Serialize};

/// Shape of one criterion summary inside a kernel Objective payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionSummary {
    #[serde(rename = "criterion_id")]
    pub criterion_id: String,

    #[serde(rename = "criterion_version")]
    pub criterion_version: i64,

    /// `active | retired | deprecated` (frozen enum).
    pub lifecycle: String,
}

/// The resolved currentness of one criterion within its Objective's governing
/// version. `superseded_version` is set when a prior version of the same
/// criterion identity was displaced by the current one (partial supersession
/// with retained identity); retained criteria carry null.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionCurrentness {
    /// `governing | retired | deprecated | no_governing_objective |
    /// unknown_criterion` — the TS oracle's state discriminator.
    pub state: String,

    pub criterion_version: Option<i64>,

    pub superseded_version: Option<i64>,

    /// The newest governing Objective version the state was read from.
    pub objective_version: Option<KernelEnvelope>,
}

impl CriterionCurrentness {
    /// The state discriminator as a wire word (matches the TS union tags).
    pub fn state_word(&self) -> &str {
        &self.state
    }
}

/// Narrowly parses the `criteria` array of an Objective payload, failing loud
/// with the same field-naming discipline as the TS oracle (`criterionSummaries`).
pub fn criterion_summaries(
    envelope: &KernelEnvelope,
) -> Result<Vec<CriterionSummary>, ResolverInputError> {
    let raw = envelope
        .object
        .get("criteria")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            ResolverInputError(format!(
                "objective {}: criteria is not an array",
                envelope.object_id
            ))
        })?;
    raw.iter()
        .enumerate()
        .map(|(index, entry)| {
            let record = entry.as_object().ok_or_else(|| {
                ResolverInputError(format!(
                    "objective {}: criteria[{index}] not an object",
                    envelope.object_id
                ))
            })?;
            let criterion_id = record
                .get("criterion_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ResolverInputError(format!(
                        "objective {}: criteria[{index}].criterion_id",
                        envelope.object_id
                    ))
                })?
                .to_string();
            let criterion_version = record
                .get("criterion_version")
                .and_then(|v| v.as_i64())
                .filter(|v| v >= &1)
                .ok_or_else(|| {
                    ResolverInputError(format!(
                        "objective {}: criteria[{index}].criterion_version",
                        envelope.object_id
                    ))
                })?;
            let lifecycle = record
                .get("lifecycle")
                .and_then(|v| v.as_str())
                .filter(|l| matches!(*l, "active" | "retired" | "deprecated"))
                .ok_or_else(|| {
                    ResolverInputError(format!(
                        "objective {}: criteria[{index}].lifecycle",
                        envelope.object_id
                    ))
                })?
                .to_string();
            Ok(CriterionSummary {
                criterion_id,
                criterion_version,
                lifecycle,
            })
        })
        .collect()
}

fn criterion_entry<'a>(
    summaries: &'a [CriterionSummary],
    criterion_id: &str,
) -> Option<&'a CriterionSummary> {
    summaries
        .iter()
        .find(|entry| entry.criterion_id == criterion_id)
}

/// Resolves one criterion's current version from an Objective's version
/// history. Deterministic and pure: the criterion's state is read from the
/// newest governing Objective version, never from the newest record.
pub fn resolve_criterion(
    objective_history: &[KernelEnvelope],
    criterion_id: &str,
) -> Result<CriterionCurrentness, ResolverInputError> {
    let governing = match newest_governing_version(objective_history) {
        Some(governing) => governing,
        None => {
            return Ok(CriterionCurrentness {
                state: "no_governing_objective".to_string(),
                criterion_version: None,
                superseded_version: None,
                objective_version: None,
            })
        }
    };

    let summaries = criterion_summaries(&governing)?;
    let current = match criterion_entry(&summaries, criterion_id) {
        Some(current) => current.clone(),
        None => {
            return Ok(CriterionCurrentness {
                state: "unknown_criterion".to_string(),
                criterion_version: None,
                superseded_version: None,
                objective_version: None,
            })
        }
    };

    // Walk the governing history newest-first for the same criterion identity
    // to find the version this one displaced (if any).
    let mut superseded_version: Option<i64> = None;
    for older in objective_history.iter().rev() {
        if older.recorded_at >= governing.recorded_at {
            continue;
        }
        if let Some(previous) = criterion_entry(&criterion_summaries(older)?, criterion_id) {
            if previous.criterion_version != current.criterion_version {
                superseded_version = Some(previous.criterion_version);
                break;
            }
        }
    }

    // An active criterion on the governing version IS the currentness the
    // oracle calls `governing`; retirement/deprecation pass through as their
    // own lifecycle states — visible, never suppressed.
    let state = match current.lifecycle.as_str() {
        "active" => "governing".to_string(),
        other => other.to_string(),
    };

    Ok(CriterionCurrentness {
        state,
        criterion_version: Some(current.criterion_version),
        superseded_version,
        objective_version: Some(governing),
    })
}

/// The active-criteria denominator of the newest governing Objective version —
/// the readiness count (T12 family: the denominator is the count of active
/// governing criteria). Callers slice the history to the desired as-known
/// window first; a historical slice re-derives the historical denominator and
/// a later retirement never rewrites it. An objective with no governing
/// version has zero active governing criteria.
pub fn active_criteria_count(
    objective_history: &[KernelEnvelope],
) -> Result<usize, ResolverInputError> {
    let governing = match newest_governing_version(objective_history) {
        Some(governing) => governing,
        None => return Ok(0),
    };
    Ok(criterion_summaries(&governing)?
        .iter()
        .filter(|c| c.lifecycle == "active")
        .count())
}

/// The frozen `objective_lifecycle` vocabulary of the `objective` def.
pub const OBJECTIVE_LIFECYCLES: &[&str] =
    &["proposed", "active", "paused", "retired", "deprecated"];

/// The frozen `authority_intent` vocabulary of the `objective` def.
pub const AUTHORITY_INTENTS: &[&str] = &["candidate", "governing"];

/// Validates the `objective` object payload of an envelope against the frozen
/// kernel-contract definition. Fail-loud on every violation, naming the field.
pub fn assert_valid_objective(envelope: &KernelEnvelope) -> Result<(), ResolverInputError> {
    if envelope.object_type != "objective" {
        return Err(ResolverInputError(format!(
            "objective {}: object_type is not 'objective'",
            envelope.object_id
        )));
    }
    let object = &envelope.object;
    let nonempty = |key: &str| -> Result<(), ResolverInputError> {
        let ok = object
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(ResolverInputError(format!(
                "objective {}: {key} is not a nonempty string",
                envelope.object_id
            )))
        }
    };
    nonempty("title")?;
    nonempty("purpose")?;
    let intent = object
        .get("authority_intent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ResolverInputError(format!(
                "objective {}: authority_intent is not a string",
                envelope.object_id
            ))
        })?;
    if !AUTHORITY_INTENTS.contains(&intent) {
        return Err(ResolverInputError(format!(
            "objective {}: authority_intent is not a frozen authority intent: {intent}",
            envelope.object_id
        )));
    }
    let lifecycle = object
        .get("objective_lifecycle")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ResolverInputError(format!(
                "objective {}: objective_lifecycle is not a string",
                envelope.object_id
            ))
        })?;
    if !OBJECTIVE_LIFECYCLES.contains(&lifecycle) {
        return Err(ResolverInputError(format!(
            "objective {}: objective_lifecycle is not a frozen objective lifecycle: {lifecycle}",
            envelope.object_id
        )));
    }
    criterion_summaries(envelope)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn objective_envelope(
        object_id: &str,
        recorded_at: &str,
        lifecycle: &str,
        criteria: serde_json::Value,
    ) -> KernelEnvelope {
        KernelEnvelope {
            schema_version: "1.0.0".to_string(),
            object_type: "objective".to_string(),
            object_id: object_id.to_string(),
            governance_root_ref: "governance_root:gentle-roost".to_string(),
            scope_refs: None,
            authority_class: crate::envelope::AuthorityClass::CurrentOperatingState,
            lifecycle_state: crate::envelope::LifecycleState::Governing,
            valid_at: Some(recorded_at.to_string()),
            as_known_at: recorded_at.to_string(),
            recorded_at: recorded_at.to_string(),
            provenance_refs: vec!["activity:act-1".to_string()],
            object: json!({
                "title": "Keep the line honest",
                "purpose": "Maintain the operating baseline",
                "authority_intent": "governing",
                "objective_lifecycle": lifecycle,
                "criteria": criteria,
            }),
        }
    }

    fn criterion(id: &str, version: i64, lifecycle: &str) -> serde_json::Value {
        json!({"criterion_id": id, "criterion_version": version, "lifecycle": lifecycle})
    }

    #[test]
    fn wording_change_keeps_identity_and_increments_version() {
        // T11: a wording-only clarification preserves the criterion identity;
        // the modified criterion's prior version is superseded.
        let v1 = objective_envelope(
            "o11",
            "2026-09-03T10:00:00Z",
            "active",
            json!([criterion("c11", 1, "active")]),
        );
        let v2 = objective_envelope(
            "o11",
            "2026-09-03T11:00:00Z",
            "active",
            json!([criterion("c11", 2, "active")]),
        );
        let resolved = resolve_criterion(&[v1.clone(), v2.clone()], "c11").unwrap();
        assert_eq!(resolved.state_word(), "governing");
        assert_eq!(resolved.criterion_version, Some(2));
        assert_eq!(resolved.superseded_version, Some(1));
        assert_eq!(
            resolved.objective_version.as_ref().unwrap().recorded_at,
            v2.recorded_at
        );
    }

    #[test]
    fn material_replacement_is_a_new_identity_at_v1() {
        // T11: replacing the testable obligation uses a NEW criterion id at
        // version 1; the old identity's last version stays discoverable.
        let v1 = objective_envelope(
            "o11",
            "2026-09-03T10:00:00Z",
            "active",
            json!([criterion("c11", 1, "active")]),
        );
        let v2 = objective_envelope(
            "o11",
            "2026-09-03T12:00:00Z",
            "active",
            json!([criterion("c11b", 1, "active")]),
        );
        let replaced = resolve_criterion(&[v1.clone(), v2.clone()], "c11b").unwrap();
        assert_eq!(replaced.state_word(), "governing");
        assert_eq!(replaced.criterion_version, Some(1));
        assert_eq!(replaced.superseded_version, None);

        // The displaced identity is no longer on the governing set...
        let old = resolve_criterion(&[v1.clone(), v2.clone()], "c11").unwrap();
        assert_eq!(old.state_word(), "unknown_criterion");
    }

    #[test]
    fn historical_version_stays_reproducible() {
        // T11: querying only the v1 slice re-derives v1 currentness — the
        // historical record is never rewritten.
        let v1 = objective_envelope(
            "o11",
            "2026-09-03T10:00:00Z",
            "active",
            json!([criterion("c11", 1, "active")]),
        );
        let resolved = resolve_criterion(std::slice::from_ref(&v1), "c11").unwrap();
        assert_eq!(resolved.state_word(), "governing");
        assert_eq!(resolved.criterion_version, Some(1));
        assert_eq!(resolved.superseded_version, None);
    }

    #[test]
    fn retired_criterion_state_is_visible_not_suppressed() {
        // T12: retirement is a governed criterion lifecycle state.
        let v1 = objective_envelope(
            "o12",
            "2026-09-03T10:00:00Z",
            "active",
            json!([
                criterion("c12a", 1, "active"),
                criterion("c12c", 1, "active")
            ]),
        );
        let v2 = objective_envelope(
            "o12",
            "2026-09-03T11:00:00Z",
            "active",
            json!([
                criterion("c12a", 1, "active"),
                criterion("c12c", 1, "retired")
            ]),
        );
        let resolved = resolve_criterion(&[v1, v2], "c12c").unwrap();
        assert_eq!(resolved.state_word(), "retired");
        assert_eq!(resolved.criterion_version, Some(1));
    }

    #[test]
    fn readiness_denominator_counts_active_criteria_only() {
        // T12: the denominator before retirement is 3; after retiring one
        // criterion it is 2; the historical slice still re-derives 3.
        let v1 = objective_envelope(
            "o12",
            "2026-09-03T10:00:00Z",
            "active",
            json!([
                criterion("c12a", 1, "active"),
                criterion("c12b", 1, "active"),
                criterion("c12c", 1, "active"),
            ]),
        );
        let v2 = objective_envelope(
            "o12",
            "2026-09-03T11:00:00Z",
            "active",
            json!([
                criterion("c12a", 1, "active"),
                criterion("c12b", 1, "active"),
                criterion("c12c", 1, "retired"),
            ]),
        );
        assert_eq!(active_criteria_count(&[v1.clone(), v2.clone()]).unwrap(), 2);
        // Historical as-known slice (only v1 visible): denominator is 3.
        assert_eq!(active_criteria_count(&[v1]).unwrap(), 3);
    }

    #[test]
    fn candidate_objective_versions_do_not_change_currentness() {
        // The criterion's state is read from the newest GOVERNING version —
        // a newer candidate revision is invisible to criterion currentness.
        let governing = objective_envelope(
            "o11",
            "2026-09-03T10:00:00Z",
            "active",
            json!([criterion("c11", 2, "active")]),
        );
        let candidate = {
            let mut envelope = objective_envelope(
                "o11",
                "2026-09-03T11:00:00Z",
                "active",
                json!([criterion("c11", 3, "active")]),
            );
            envelope.authority_class = crate::envelope::AuthorityClass::CandidateKnowledge;
            envelope.lifecycle_state = crate::envelope::LifecycleState::Candidate;
            envelope
        };
        let resolved = resolve_criterion(&[governing.clone(), candidate.clone()], "c11").unwrap();
        assert_eq!(resolved.state_word(), "governing");
        assert_eq!(resolved.criterion_version, Some(2));
    }

    #[test]
    fn no_governing_objective_and_parse_failures_fail_loud() {
        let candidate_only = {
            let mut envelope = objective_envelope(
                "o11",
                "2026-09-03T10:00:00Z",
                "active",
                json!([criterion("c11", 1, "active")]),
            );
            envelope.authority_class = crate::envelope::AuthorityClass::CandidateKnowledge;
            envelope.lifecycle_state = crate::envelope::LifecycleState::Candidate;
            envelope
        };
        assert_eq!(
            resolve_criterion(&[candidate_only], "c11")
                .unwrap()
                .state_word(),
            "no_governing_objective"
        );

        // criteria not an array
        let bad = objective_envelope("o11", "2026-09-03T10:00:00Z", "active", json!("nope"));
        assert_eq!(
            resolve_criterion(std::slice::from_ref(&bad), "c11")
                .unwrap_err()
                .0,
            "objective o11: criteria is not an array"
        );
        // criterion version below the contract minimum
        let bad = objective_envelope(
            "o11",
            "2026-09-03T10:00:00Z",
            "active",
            json!([criterion("c11", 0, "active")]),
        );
        assert_eq!(
            resolve_criterion(std::slice::from_ref(&bad), "c11")
                .unwrap_err()
                .0,
            "objective o11: criteria[0].criterion_version"
        );
        // lifecycle outside the frozen enum
        let bad = objective_envelope(
            "o11",
            "2026-09-03T10:00:00Z",
            "active",
            json!([criterion("c11", 1, "paused")]),
        );
        assert_eq!(
            resolve_criterion(&[bad], "c11").unwrap_err().0,
            "objective o11: criteria[0].lifecycle"
        );
    }

    #[test]
    fn objective_record_validation_names_every_violation() {
        let good = objective_envelope(
            "o11",
            "2026-09-03T10:00:00Z",
            "active",
            json!([criterion("c11", 1, "active")]),
        );
        assert!(assert_valid_objective(&good).is_ok());

        let mut bad = good.clone();
        bad.object_type = "assertion".to_string();
        assert_eq!(
            assert_valid_objective(&bad).unwrap_err().0,
            "objective o11: object_type is not 'objective'"
        );

        let mut bad = good.clone();
        bad.object["title"] = json!("");
        assert_eq!(
            assert_valid_objective(&bad).unwrap_err().0,
            "objective o11: title is not a nonempty string"
        );

        let mut bad = good.clone();
        bad.object["authority_intent"] = json!("governing_maybe");
        assert!(assert_valid_objective(&bad)
            .unwrap_err()
            .0
            .starts_with("objective o11: authority_intent"));

        let mut bad = good.clone();
        bad.object["objective_lifecycle"] = json!("living");
        assert!(assert_valid_objective(&bad)
            .unwrap_err()
            .0
            .starts_with("objective o11: objective_lifecycle"));
    }
}
