//! Decision records — the Rust mirror of the frozen `decision` object
//! definition in `packages/contracts/schemas/v1/kernel.schema.json` at the
//! oracle pin (`second-brain @ frontier/ois-v1 c56f8e5`).
//!
//! A Decision is a governed record of a genuine choice: what was asked, what
//! was decided, by whom, and what it applies to. The governed-write path
//! propagates `originating_decision_ref` verbatim (never fabricated —
//! `governance.rs`), so this module owns the record shape itself and its
//! surfacing: which decisions apply to a given ref.
//!
//! Fail-closed discipline: a decision payload that violates the frozen
//! contract is an error, never silently coerced — a malformed outcome
//! vocabulary or an empty `applies_to_refs` cannot masquerade as a record.

use crate::envelope::KernelEnvelope;
use serde::{Deserialize, Serialize};

/// Typed error for incoherent decision records — never silently coerced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionError(pub String);

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "decision record incoherent: {}", self.0)
    }
}

impl std::error::Error for DecisionError {}

/// The frozen outcome vocabulary of the `decision` object definition.
pub const DECISION_OUTCOMES: &[&str] = &["approved", "rejected", "acknowledged"];

fn is_nonempty_string(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// One validated decision record, mirroring the frozen `decision` def exactly.
/// Unknown fields inside `object` are rejected here only where the contract
/// does; the envelope's `object` payload itself is preserved verbatim on the
/// envelope (the kernel's canonical store never rewrites content).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Free-form governed vocabulary for which kind of decision this is.
    #[serde(rename = "decision_kind")]
    pub decision_kind: String,

    /// The question the decision answers (nonempty per contract).
    pub question: String,

    /// `approved | rejected | acknowledged` (frozen enum).
    pub outcome: String,

    /// Server-derived principal ref of the decider (never a payload claim).
    #[serde(rename = "deciding_principal_ref")]
    pub deciding_principal_ref: String,

    /// The decision's own recording instant — the churn-key record instant
    /// for decision-scoped churn (change-dynamics `churnKeyRecordedAt`).
    #[serde(rename = "recorded_at")]
    pub recorded_at: String,

    /// Refs the decision applies to (min 1, unique, nonempty per contract).
    #[serde(rename = "applies_to_refs")]
    pub applies_to_refs: Vec<String>,

    /// Optional human-readable rationale (null or nonempty per contract).
    #[serde(rename = "rationale", default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,

    /// Optional explicit GovernanceRule the decision was made under.
    #[serde(
        rename = "governance_rule_ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub governance_rule_ref: Option<String>,
}

/// Validates and projects the `decision` object payload of an envelope into a
/// `DecisionRecord`. Fail-loud on every contract violation, naming the field.
/// The envelope must be a `decision` kernel object; any other object type is
/// incoherent input.
pub fn decision_record_from_envelope(
    envelope: &KernelEnvelope,
) -> Result<DecisionRecord, DecisionError> {
    if envelope.object_type != "decision" {
        return Err(DecisionError(format!(
            "object_type is not 'decision': {}",
            envelope.object_type
        )));
    }
    let object = &envelope.object;
    let get = |key: &str| object.get(key);

    // decision_kind / question / deciding_principal_ref: nonempty strings.
    if !is_nonempty_string(get("decision_kind")) {
        return Err(DecisionError(
            "decision_kind is not a nonempty string".to_string(),
        ));
    }
    if !is_nonempty_string(get("question")) {
        return Err(DecisionError(
            "question is not a nonempty string".to_string(),
        ));
    }
    if !is_nonempty_string(get("deciding_principal_ref")) {
        return Err(DecisionError(
            "deciding_principal_ref is not a nonempty string".to_string(),
        ));
    }

    // outcome: frozen enum.
    let outcome = get("outcome")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DecisionError("outcome is not a string".to_string()))?;
    if !DECISION_OUTCOMES.contains(&outcome) {
        return Err(DecisionError(format!(
            "outcome is not a frozen decision outcome: {outcome}"
        )));
    }

    // applies_to_refs: min 1, unique, nonempty strings.
    let raw_refs = get("applies_to_refs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| DecisionError("applies_to_refs is not an array".to_string()))?;
    if raw_refs.is_empty() {
        return Err(DecisionError(
            "applies_to_refs is empty (min 1 per contract)".to_string(),
        ));
    }
    let mut applies_to_refs: Vec<String> = Vec::with_capacity(raw_refs.len());
    for (index, entry) in raw_refs.iter().enumerate() {
        let reference = entry.as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
            DecisionError(format!("applies_to_refs[{index}] is not a nonempty string"))
        })?;
        if applies_to_refs.contains(&reference.to_string()) {
            return Err(DecisionError(format!(
                "applies_to_refs[{index}] duplicates an earlier ref: {reference}"
            )));
        }
        applies_to_refs.push(reference.to_string());
    }

    // Optional rationale / governance_rule_ref: null or nonempty strings.
    let optional_ref = |key: &str| -> Result<Option<String>, DecisionError> {
        match get(key) {
            None => Ok(None),
            Some(v) if v.is_null() => Ok(None),
            Some(v) => {
                let s = v.as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
                    DecisionError(format!("{key} is neither null nor a nonempty string"))
                })?;
                Ok(Some(s.to_string()))
            }
        }
    };
    let rationale = optional_ref("rationale")?;
    let governance_rule_ref = optional_ref("governance_rule_ref")?;

    Ok(DecisionRecord {
        decision_kind: get("decision_kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        question: get("question")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        outcome: outcome.to_string(),
        deciding_principal_ref: get("deciding_principal_ref")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        // The envelope is the record's clock carrier (kernel contract); the
        // object payload's recorded_at mirrors it when present.
        recorded_at: envelope.recorded_at.clone(),
        applies_to_refs,
        rationale,
        governance_rule_ref,
    })
}

/// The kernel ref convention for a decision (`decision:{object_id}`) — the
/// churn-key shape decision-scoped events carry.
pub fn decision_ref(envelope: &KernelEnvelope) -> String {
    format!("decision:{}", envelope.object_id)
}

/// Every decision record in a corpus that applies to `target_ref`. Pure
/// surfacing: order follows the input corpus; callers own any windowing.
pub fn decisions_applying_to<'a>(
    decisions: &'a [DecisionRecord],
    target_ref: &str,
) -> Vec<&'a DecisionRecord> {
    decisions
        .iter()
        .filter(|d| d.applies_to_refs.iter().any(|r| r == target_ref))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decision_envelope(object: serde_json::Value) -> KernelEnvelope {
        KernelEnvelope {
            schema_version: "1.0.0".to_string(),
            object_type: "decision".to_string(),
            object_id: "dec-1".to_string(),
            governance_root_ref: "governance_root:gentle-roost".to_string(),
            scope_refs: None,
            authority_class: crate::envelope::AuthorityClass::CurrentOperatingState,
            lifecycle_state: crate::envelope::LifecycleState::Governing,
            valid_at: Some("2026-09-02T10:00:00Z".to_string()),
            as_known_at: "2026-09-02T10:00:00Z".to_string(),
            recorded_at: "2026-09-02T10:00:00Z".to_string(),
            provenance_refs: vec!["activity:act-1".to_string()],
            object,
        }
    }

    fn valid_object() -> serde_json::Value {
        json!({
            "decision_kind": "establishment",
            "question": "Is the line-speed baseline established at 40 u/min?",
            "outcome": "approved",
            "deciding_principal_ref": "principal:owner",
            "recorded_at": "2026-09-02T10:00:00Z",
            "applies_to_refs": ["assertion:asrt-line-speed"],
            "rationale": "Two independent measurements agree."
        })
    }

    #[test]
    fn valid_decision_record_round_trips() {
        let envelope = decision_envelope(valid_object());
        let record = decision_record_from_envelope(&envelope).unwrap();
        assert_eq!(record.outcome, "approved");
        assert_eq!(record.applies_to_refs, vec!["assertion:asrt-line-speed"]);
        assert_eq!(
            record.rationale.as_deref(),
            Some("Two independent measurements agree.")
        );
        assert_eq!(decision_ref(&envelope), "decision:dec-1");
    }

    #[test]
    fn surfacing_finds_decisions_by_applied_ref() {
        let envelope = decision_envelope(valid_object());
        let record = decision_record_from_envelope(&envelope).unwrap();
        let corpus = vec![record];
        assert_eq!(
            decisions_applying_to(&corpus, "assertion:asrt-line-speed").len(),
            1
        );
        assert!(decisions_applying_to(&corpus, "assertion:other").is_empty());
    }

    #[test]
    fn every_contract_violation_fails_loud() {
        // Object-type guard: any non-decision envelope is incoherent input.
        let mut wrong_type = decision_envelope(valid_object());
        wrong_type.object_type = "assertion".to_string();
        assert_eq!(
            decision_record_from_envelope(&wrong_type).unwrap_err().0,
            "object_type is not 'decision': assertion"
        );

        let mut o = valid_object();
        o["outcome"] = json!("maybe");
        assert!(decision_record_from_envelope(&decision_envelope(o))
            .unwrap_err()
            .0
            .starts_with("outcome"));

        let mut o = valid_object();
        o["question"] = json!("");
        assert!(decision_record_from_envelope(&decision_envelope(o))
            .unwrap_err()
            .0
            .starts_with("question"));

        let mut o = valid_object();
        o["applies_to_refs"] = json!([]);
        assert!(decision_record_from_envelope(&decision_envelope(o))
            .unwrap_err()
            .0
            .starts_with("applies_to_refs"));

        let mut o = valid_object();
        o["applies_to_refs"] = json!(["a", "a"]);
        assert!(decision_record_from_envelope(&decision_envelope(o))
            .unwrap_err()
            .0
            .starts_with("applies_to_refs"));

        let mut o = valid_object();
        o["rationale"] = json!("");
        assert!(decision_record_from_envelope(&decision_envelope(o))
            .unwrap_err()
            .0
            .starts_with("rationale"));
    }
}
