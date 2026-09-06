//! Governance event surfacing — per-window churn origins and decision
//! lineage over recorded `governance_event` envelopes. Pure port of the
//! kernel-level semantics of the oracle's change-dynamics modules
//! (`packages/projections/src/change-dynamics/{churn,model}.ts` and
//! `time.ts`) at the pin (`second-brain @ frontier/ois-v1 c56f8e5`).
//!
//! `unique_churn_origins` counts distinct churn keys whose churn-key RECORD
//! itself was recorded inside the requested window — `Decision.recorded_at`
//! when a Decision is the key, otherwise the GovernanceEvent's `recorded_at`.
//! Propagation events recorded in a later window may touch units there while
//! contributing zero later-window unique churn origins when their Decision
//! was recorded earlier (R1-T29). Later propagation never creates repeated
//! Decision churn.
//!
//! Fail-closed discipline: a churn key without its Decision or GovernanceEvent
//! record is incoherent input — an error, never a silent zero (a false zero
//! hides churn; permission may make counts null upstream, never here).

use crate::envelope::KernelEnvelope;
use serde_json::Value;

/// Typed error for incoherent event corpora — never silently zeroed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSurfaceError(pub String);

impl std::fmt::Display for EventSurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event surface input incoherent: {}", self.0)
    }
}

impl std::error::Error for EventSurfaceError {}

/// Kernel envelope ref in the `object_type:object_id` convention.
pub fn envelope_ref(envelope: &KernelEnvelope) -> String {
    envelope.kernel_ref()
}

/// True when `ref` names the envelope (bare id or `type:id` convention) —
/// mirrors `envelopeMatchesRef` in the oracle's change-dynamics model.
pub fn envelope_matches_ref(envelope: &KernelEnvelope, reference: &str) -> bool {
    reference == envelope.object_id || reference == envelope_ref(envelope)
}

/// True when `start <= instant <= end` (inclusive interval, as
/// `inInclusiveInterval` in the oracle's time module). Instants are ISO-8601
/// UTC strings; the frozen corpus uses uniform-format instants where
/// lexicographic order is chronological order (the port's established
/// convention, see `resolver::at_or_before`).
pub fn in_inclusive_interval(instant: &str, start: &str, end: &str) -> bool {
    start <= instant && instant <= end
}

/// The churn-key origin record's own recording instant: `Decision.recorded_at`
/// when a Decision is the churn key, otherwise the GovernanceEvent's own
/// `recorded_at` (the key IS the event). An event that merely DECLARES a
/// churn key (a propagation pointing at its decision) is never a substitute
/// for the key's origin record. A churn key without its origin record is
/// incoherent input — an error, never a silently zeroed origin.
pub fn churn_key_recorded_at(
    churn_key: &str,
    decisions: &[KernelEnvelope],
    governance_events: &[KernelEnvelope],
) -> Result<String, EventSurfaceError> {
    // Exact port of `churnKeyRecordedAt` (change-dynamics model.ts): a
    // Decision match wins; otherwise an event satisfies the lookup when its
    // own ref matches OR it DECLARES the churn key — a propagation event
    // carrying its decision's key is a legitimate record carrier. A key with
    // neither record is incoherent input and throws, never a silent zero.
    if let Some(decision) = decisions
        .iter()
        .find(|record| record.object_type == "decision" && envelope_matches_ref(record, churn_key))
    {
        return Ok(decision.recorded_at.clone());
    }
    let event = governance_events.iter().find(|record| {
        if envelope_matches_ref(record, churn_key) {
            return true;
        }
        let declared = record.object.get("churn_key").and_then(|v| v.as_str());
        declared == Some(churn_key)
    });
    if let Some(event) = event {
        return Ok(event.recorded_at.clone());
    }
    Err(EventSurfaceError(format!(
        "churn key {churn_key} has no recorded Decision or GovernanceEvent record"
    )))
}

/// Distinct churn keys among the in-window event set whose churn-key record
/// was itself recorded inside the inclusive window. Mirrors the
/// `unique_churn_origins` derivation of the oracle's `computeChurn` over the
/// kernel's event corpus (the Work Delta decomposition lives in the work
/// layer; the origin-counting rule is this one).
pub fn unique_churn_origins(
    in_window_churn_keys: &[String],
    decisions: &[KernelEnvelope],
    governance_events: &[KernelEnvelope],
    window_start: &str,
    window_end: &str,
) -> Result<usize, EventSurfaceError> {
    let mut unique_keys: Vec<&String> = Vec::new();
    for key in in_window_churn_keys {
        if unique_keys.contains(&key) {
            continue;
        }
        unique_keys.push(key);
    }
    let mut unique = 0;
    for key in unique_keys {
        let recorded_at = churn_key_recorded_at(key, decisions, governance_events)?;
        if in_inclusive_interval(&recorded_at, window_start, window_end) {
            unique += 1;
        }
    }
    Ok(unique)
}

/// Distinct `applied_ref`s of events recorded inside the inclusive window —
/// the kernel-level "touched units" of the change-dynamics surface (R1-T29
/// window B: two propagation events touch two units).
pub fn touched_units(
    governance_events: &[KernelEnvelope],
    window_start: &str,
    window_end: &str,
) -> usize {
    let mut touched: Vec<String> = Vec::new();
    for event in governance_events {
        if !in_inclusive_interval(&event.recorded_at, window_start, window_end) {
            continue;
        }
        let applied = event
            .object
            .get("applied_ref")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(applied) = applied {
            if !touched.contains(&applied) {
                touched.push(applied);
            }
        }
    }
    touched.len()
}

/// Events whose lineage belongs to a Decision: `originating_decision_ref`
/// equals the ref, or the decision ref appears in the event's provenance.
/// Order follows the input corpus; callers own any windowing.
pub fn events_for_decision<'a>(
    governance_events: &'a [KernelEnvelope],
    decision_ref: &str,
) -> Vec<&'a KernelEnvelope> {
    governance_events
        .iter()
        .filter(|event| {
            let declared = event
                .object
                .get("originating_decision_ref")
                .and_then(|v| v.as_str())
                .map(|s| s == decision_ref)
                .unwrap_or(false);
            declared || event.provenance_refs.iter().any(|p| p == decision_ref)
        })
        .collect()
}

/// Every distinct churn key declared on the given events (the input to
/// [`unique_churn_origins`]). Events without a churn key are skipped here;
/// the governing-write path guarantees one on applied transitions.
pub fn declared_churn_keys(governance_events: &[KernelEnvelope]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for event in governance_events {
        if let Some(key) = event.object.get("churn_key").and_then(|v| v.as_str()) {
            if !keys.iter().any(|k| k == key) {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

/// Convenience projection: the event's frozen fields as JSON, the shared
/// normalized shape the parity harness compares across engines.
pub fn project_event(event: &KernelEnvelope) -> Value {
    serde_json::json!({
        "event_ref": envelope_ref(event),
        "event_kind": event.object.get("event_kind"),
        "change_class": event.object.get("change_class"),
        "applied_ref": event.object.get("applied_ref"),
        "originating_decision_ref": event.object.get("originating_decision_ref"),
        "churn_key": event.object.get("churn_key"),
        "recorded_at": event.recorded_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decision(recorded_at: &str, object_id: &str) -> KernelEnvelope {
        KernelEnvelope {
            schema_version: "1.0.0".to_string(),
            object_type: "decision".to_string(),
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
                "decision_kind": "establishment",
                "question": "q",
                "outcome": "approved",
                "deciding_principal_ref": "principal:owner",
                "applies_to_refs": ["assertion:u29a"],
            }),
        }
    }

    fn event(
        object_id: &str,
        recorded_at: &str,
        decision_ref: Option<&str>,
        applied_ref: &str,
    ) -> KernelEnvelope {
        let churn_key = decision_ref
            .map(|d| d.to_string())
            .unwrap_or_else(|| format!("governance_event:{object_id}"));
        KernelEnvelope {
            schema_version: "1.0.0".to_string(),
            object_type: "governance_event".to_string(),
            object_id: object_id.to_string(),
            governance_root_ref: "governance_root:gentle-roost".to_string(),
            scope_refs: None,
            authority_class: crate::envelope::AuthorityClass::CurrentOperatingState,
            lifecycle_state: crate::envelope::LifecycleState::Governing,
            valid_at: Some(recorded_at.to_string()),
            as_known_at: recorded_at.to_string(),
            recorded_at: recorded_at.to_string(),
            provenance_refs: vec![applied_ref.to_string(), "activity:act-1".to_string()],
            object: json!({
                "event_kind": "governing_transition",
                "change_class": "governing_state_change",
                "applied_ref": applied_ref,
                "applied_resource_type": "assertion",
                "originating_decision_ref": decision_ref,
                "churn_key": churn_key,
                "recorded_at": recorded_at,
            }),
        }
    }

    #[test]
    fn t29_window_a_counts_the_decision_as_one_churn_origin() {
        let decisions = vec![decision("2026-09-02T10:00:00Z", "d29")];
        let events = vec![event(
            "ge29a",
            "2026-09-02T10:00:00Z",
            Some("decision:d29"),
            "assertion:u29a",
        )];
        let keys = declared_churn_keys(&events);
        assert_eq!(keys, vec!["decision:d29"]);
        // Window A: the decision day — the key's record (the Decision) is inside.
        let unique = unique_churn_origins(
            &keys,
            &decisions,
            &events,
            "2026-09-02T00:00:00Z",
            "2026-09-02T23:59:59Z",
        )
        .unwrap();
        assert_eq!(unique, 1);
    }

    #[test]
    fn t29_window_b_propagation_adds_zero_new_churn_origins() {
        let decisions = vec![decision("2026-09-02T10:00:00Z", "d29")];
        let events = vec![
            event(
                "ge29a",
                "2026-09-02T10:00:00Z",
                Some("decision:d29"),
                "assertion:u29a",
            ),
            event(
                "ge29b1",
                "2026-09-03T10:00:00Z",
                Some("decision:d29"),
                "assertion:u29b",
            ),
            event(
                "ge29b2",
                "2026-09-03T11:00:00Z",
                Some("decision:d29"),
                "assertion:u29c",
            ),
        ];
        let keys = declared_churn_keys(&events);
        // Window B: two propagation events touch two units, but the shared
        // decision key was recorded in window A — zero NEW churn origins.
        let unique = unique_churn_origins(
            &keys,
            &decisions,
            &events,
            "2026-09-03T00:00:00Z",
            "2026-09-03T16:00:00Z",
        )
        .unwrap();
        assert_eq!(unique, 0);
        assert_eq!(
            touched_units(&events, "2026-09-03T00:00:00Z", "2026-09-03T16:00:00Z"),
            2
        );
        // All propagation events stay visible under the decision's lineage.
        assert_eq!(events_for_decision(&events, "decision:d29").len(), 3);
    }

    #[test]
    fn self_keyed_events_count_in_their_own_window() {
        // No decision: the event's own ref is its churn key; the record
        // instant is the event's own recorded_at.
        let events = vec![event(
            "ge01",
            "2026-09-04T08:00:00Z",
            None,
            "assertion:asrt-x",
        )];
        let keys = declared_churn_keys(&events);
        assert_eq!(keys, vec!["governance_event:ge01"]);
        let unique = unique_churn_origins(
            &keys,
            &[],
            &events,
            "2026-09-04T00:00:00Z",
            "2026-09-04T23:59:59Z",
        )
        .unwrap();
        assert_eq!(unique, 1);
    }

    #[test]
    fn declaring_event_carries_the_key_and_truly_missing_keys_fail_loud() {
        // Oracle parity: the propagation event declaring the churn key IS a
        // legitimate record carrier even when no Decision record exists —
        // the key's origin instant is the declaring event's recorded_at.
        let events = vec![event(
            "ge09",
            "2026-09-04T08:00:00Z",
            Some("decision:missing"),
            "assertion:asrt-x",
        )];
        let keys = declared_churn_keys(&events);
        let unique = unique_churn_origins(
            &keys,
            &[],
            &events,
            "2026-09-04T00:00:00Z",
            "2026-09-04T23:59:59Z",
        )
        .unwrap();
        assert_eq!(unique, 1);

        // A churn key with no Decision record and no event (matching or
        // declaring) is incoherent input — the oracle throws; so do we.
        let error = unique_churn_origins(
            &["decision:nowhere".to_string()],
            &[],
            &events,
            "2026-09-04T00:00:00Z",
            "2026-09-04T23:59:59Z",
        )
        .unwrap_err();
        assert_eq!(
            error.0,
            "churn key decision:nowhere has no recorded Decision or GovernanceEvent record"
        );
    }

    #[test]
    fn ref_matching_covers_bare_ids_and_type_prefixed_refs() {
        let decisions = vec![decision("2026-09-02T10:00:00Z", "d29")];
        assert_eq!(
            churn_key_recorded_at("d29", &decisions, &[]).unwrap(),
            "2026-09-02T10:00:00Z"
        );
        assert_eq!(
            churn_key_recorded_at("decision:d29", &decisions, &[]).unwrap(),
            "2026-09-02T10:00:00Z"
        );
    }
}
