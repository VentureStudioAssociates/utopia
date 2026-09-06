//! Minimal ingestion hook: a Utopia fact becomes a CANDIDATE envelope.
//!
//! The Phase-1 pivotal finding, encoded as code: bulk-extracted facts flow
//! through Utopia's ordinary path and land in the governed kernel as
//! `candidate_knowledge` / `candidate` — nothing more. No auto-promotion, no
//! inferred authority: promoting a candidate is a governed transition that
//! must go through the single governed-write path with server-derived
//! authority and evidence. Until then the resolver reports the object
//! `candidate_not_governing` — it cannot change resolved governing state.

use crate::envelope::{AuthorityClass, KernelEnvelope, LifecycleState};

/// A minimal projection of the Utopia fact row the hook needs. Pure data —
/// the caller (utopia-store) fills it from the inserted row.
pub struct FactRef {
    pub id: String,
    pub subject_id: String,
    pub predicate_ref: Option<String>,
    pub object_id: Option<String>,
    pub object_value: Option<serde_json::Value>,
    pub valid_from: Option<String>,
    pub recorded_at: String,
    pub governance_root_ref: String,
    pub scope_refs: Vec<String>,
}

/// Builds the CANDIDATE envelope for an ingested fact. Deterministic and
/// pure: the same fact row always yields the same envelope. The envelope is
/// the fact's identity in the governed kernel — `candidate_knowledge` +
/// `candidate` authority coupling, valid_at from the fact's validity start
/// (null = unbounded, in force), recorded_at from the fact's recording.
pub fn candidate_envelope_from_fact(fact: &FactRef) -> KernelEnvelope {
    let mut object = serde_json::json!({
        "subject": fact.subject_id,
        "source": "utopia_ingest",
    });
    if let Some(predicate) = &fact.predicate_ref {
        object["predicate"] = serde_json::json!(predicate);
    }
    match (&fact.object_id, &fact.object_value) {
        (Some(oid), _) => {
            object["object"] = serde_json::json!(oid);
        }
        (None, Some(value)) => {
            object["object"] = value.clone();
        }
        (None, None) => {
            object["object"] = serde_json::json!(null);
        }
    }
    KernelEnvelope {
        schema_version: "1.0.0".to_string(),
        // The assertion family is the natural kernel home for a subject-
        // predicate-object fact row.
        object_type: "assertion".to_string(),
        object_id: fact.id.clone(),
        governance_root_ref: fact.governance_root_ref.clone(),
        scope_refs: if fact.scope_refs.is_empty() {
            None
        } else {
            Some(fact.scope_refs.clone())
        },
        // The coupling is bidirectional and frozen: candidate lifecycle
        // carries candidate authority. Nothing extracted from a document
        // governs by itself.
        authority_class: AuthorityClass::CandidateKnowledge,
        lifecycle_state: LifecycleState::Candidate,
        valid_at: fact.valid_from.clone(),
        as_known_at: fact.recorded_at.clone(),
        recorded_at: fact.recorded_at.clone(),
        provenance_refs: vec![format!("utopia-fact:{}", fact.id)],
        object,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact() -> FactRef {
        FactRef {
            id: "0195f1a2-0000-7000-8000-000000000001".to_string(),
            subject_id: "ent-subject".to_string(),
            predicate_ref: Some("rel-works-at".to_string()),
            object_id: Some("ent-object".to_string()),
            object_value: None,
            valid_from: Some("2026-01-01T00:00:00Z".to_string()),
            recorded_at: "2026-09-05T10:00:00Z".to_string(),
            governance_root_ref: "ois:scope:utopia-root".to_string(),
            scope_refs: vec![],
        }
    }

    #[test]
    fn ingested_fact_is_candidate_and_cannot_govern() {
        let envelope = candidate_envelope_from_fact(&fact());
        assert_eq!(envelope.lifecycle_state, LifecycleState::Candidate);
        assert_eq!(envelope.authority_class, AuthorityClass::CandidateKnowledge);
        assert!(envelope.lifecycle_state.is_candidate_or_working());
        // The resolver's coherence guard accepts the coupling...
        let corpus = crate::resolver::ResolutionCorpus {
            target_history: vec![envelope.clone()],
            related_records: None,
        };
        let query = crate::resolver::ResolutionQuery {
            governance_root_ref: envelope.governance_root_ref.clone(),
            as_known_at: "2026-09-05T16:00:00Z".to_string(),
            valid_at: None,
            scope_refs: None,
            permission: None,
            applicability: None,
        };
        let resolved = crate::resolver::resolve_current_state(&corpus, &query).unwrap();
        // ...and the resolved state can never be governing.
        assert_eq!(resolved.state_word(), "not_established");
    }
}
