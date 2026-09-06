//! Differential parity harness for the OIS kernel port.
//!
//! Runs the shared parity corpus (74 frozen cases derived, with recorded
//! provenance, from the TypeScript oracle's R1-T01–T15, R1-T22/T27/T28/T29 and
//! Gentle Roost GR-E01/GR-E3B fixtures) through THIS Rust port and:
//!
//! 1. asserts each case's frozen expected outcome, and
//! 2. when `TS_RESULTS_PATH` points at the TypeScript oracle driver's
//!    `ts-results.json` (same corpus, same normalization), compares the
//!    normalized Rust results 1:1 against the oracle's results.
//!
//! Fail-closed discipline: an unexpected Rust error is a failure; divergence
//! from the oracle without a recorded explanation is a kill condition.

use ois_kernel::governance::{
    accept_cross_scope, apply_governed_write, classify_change_class, evaluate_capability,
    is_stale_base, CrossScopeAcceptanceCommand, GovernedWriteCommand,
};
use ois_kernel::promotion::{promote_projection_edges, PromotionBatchCommand};
use ois_kernel::resolver::resolve_current_state;
use ois_kernel::{ResolutionCorpus, ResolutionQuery};
use serde_json::{json, Map, Value};
use std::fs;

const CORPUS: &str = include_str!("fixtures/parity-corpus.json");

/// Fixture-level scenario engine for the stage-4b acceptance corpus
/// (R1-T01..T29 + GR flagship). Child module so it reuses this file's
/// `epoch_seconds` port.
mod fixture_scenarios;

/// Days-from-civil (Hinnant) — epoch seconds for ISO `YYYY-MM-DDTHH:MM:SS(.fff)Z`
/// without pulling a date dependency into the kernel crate.
fn epoch_seconds(iso: &str) -> i64 {
    let (date, time) = iso.split_once('T').unwrap_or((iso, "00:00:00"));
    let dp: Vec<i64> = date
        .split('-')
        .filter_map(|p| p.parse::<i64>().ok())
        .collect();
    let (y, m, d) = (dp[0], dp[1], dp[2]);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let tp: Vec<i64> = time
        .trim_end_matches('Z')
        .split(':')
        .filter_map(|p| p.split('.').next().and_then(|q| q.parse::<i64>().ok()))
        .collect();
    let (hh, mm, ss) = (tp[0], tp[1], tp[2]);
    days * 86400 + hh * 3600 + mm * 60 + ss
}

fn opt_str(v: &Value, key: &str) -> Option<Value> {
    v.get(key).cloned().filter(|x| !x.is_null())
}

// ---------------------------------------------------------------------------
// Per-engine runners: normalized to the TypeScript driver's projection shapes.
// ---------------------------------------------------------------------------

fn project_resolver(c: &Value) -> Value {
    let mut out = Map::new();
    out.insert("state".into(), c["state"].clone());
    // TS JSON.stringify drops `completeness` when the state object lacks it
    // (the resolve_each placeholder), so the key is conditional here too.
    if let Some(k) = c.get("completeness").cloned() {
        out.insert("completeness".into(), k);
    }
    out.insert(
        "diagnostic_kinds".into(),
        c.get("diagnostics")
            .and_then(|d| d.as_array())
            .map(|a| Value::Array(a.iter().map(|d| d["kind"].clone()).collect()))
            .unwrap_or(Value::Array(vec![])),
    );
    out.insert(
        "envelope_recorded_at".into(),
        c.get("envelope")
            .and_then(|e| e.get("recorded_at"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    if let Some(basis) = c.get("basis") {
        out.insert("basis".into(), basis.clone());
    }
    if let Some(authority) = c.get("authority") {
        out.insert("authority".into(), authority.clone());
    }
    if c["state"] == "superseded" {
        out.insert(
            "successor_ref".into(),
            c.get("successor_ref").cloned().unwrap_or(Value::Null),
        );
    }
    if c["state"] == "permission_limited" {
        let wc = c
            .get("diagnostics")
            .and_then(|d| d.as_array())
            .and_then(|a| {
                a.iter()
                    .find(|d| d["kind"] == "permission_withheld")
                    .and_then(|d| d.get("withheld_count"))
            })
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::Null);
        out.insert("withheld_count".into(), wc);
    }
    Value::Object(out)
}

fn run_resolver(input: &Value) -> Result<Value, String> {
    let corpus_a: ResolutionCorpus = serde_json::from_value(
        input
            .get("corpus")
            .or_else(|| input.get("corpus_a"))
            .cloned()
            .ok_or("missing corpus")?,
    )
    .map_err(|e| e.to_string())?;
    let query: ResolutionQuery =
        serde_json::from_value(input["query"].clone()).map_err(|e| e.to_string())?;

    let resolve_each = input.get("resolve_each").is_some();
    let combined: Value = if resolve_each {
        json!({ "state": "not_established" })
    } else {
        serde_json::to_value(resolve_current_state(&corpus_a, &query).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
    };
    let a = project_resolver(&combined);
    let mut result = a.clone();

    if let Some(corpus_b) = input.get("corpus_b") {
        let cb: ResolutionCorpus =
            serde_json::from_value(corpus_b.clone()).map_err(|e| e.to_string())?;
        let qb: ResolutionQuery = serde_json::from_value(
            input
                .get("query_b")
                .cloned()
                .unwrap_or_else(|| input["query"].clone()),
        )
        .map_err(|e| e.to_string())?;
        let b = project_resolver(
            &serde_json::to_value(resolve_current_state(&cb, &qb).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
        if input.get("assert_truncated_equivalence").is_some() {
            result["equal_to_b"] = Value::Bool(a == b);
        }
        result["state_b"] = b["state"].clone();
    } else if let Some(qb) = input.get("query_b") {
        // Same corpus, later as-known instant (temporal rewinds use this).
        // Mutually exclusive with the corpus_b branch above: t02 carries BOTH
        // keys, and its B resolution is the truncated-corpus one — an
        // unconditional branch here overwrote the corpus_b result.
        let qb: ResolutionQuery = serde_json::from_value(qb.clone()).map_err(|e| e.to_string())?;
        let b = project_resolver(
            &serde_json::to_value(
                resolve_current_state(&corpus_a, &qb).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
        );
        result["state_b"] = b["state"].clone();
    }

    if input.get("also_run_shuffled").is_some() {
        let mut history = corpus_a.target_history.clone();
        history.reverse();
        let shuffled_corpus = ResolutionCorpus {
            target_history: history,
            related_records: corpus_a.related_records.clone(),
        };
        let shuffled = project_resolver(
            &serde_json::to_value(
                resolve_current_state(&shuffled_corpus, &query).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
        );
        result["shuffled_equal"] = Value::Bool(a == shuffled);
    }

    if input.get("repeat").is_some() {
        let again = project_resolver(
            &serde_json::to_value(
                resolve_current_state(&corpus_a, &query).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
        );
        result["repeat_equal"] = Value::Bool(a == again);
    }

    if resolve_each {
        let states: Vec<String> = corpus_a
            .target_history
            .iter()
            .map(|env| {
                let one = ResolutionCorpus {
                    target_history: vec![env.clone()],
                    related_records: None,
                };
                let c = serde_json::to_value(
                    resolve_current_state(&one, &query).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                Ok(c["state"].as_str().unwrap_or_default().to_string())
            })
            .collect::<Result<Vec<String>, String>>()?;
        let mut unique = states.clone();
        unique.sort();
        unique.dedup();
        result["all_states"] = if unique.len() == 1 {
            Value::String(unique[0].clone())
        } else {
            Value::String(states.join(","))
        };
        result["count"] = json!(states.len());
    }

    Ok(result)
}

fn run_capability(input: &Value) -> Result<Value, String> {
    let profile: ois_kernel::Profile =
        serde_json::from_value(input["profile"].clone()).map_err(|e| e.to_string())?;
    let principal: ois_kernel::PrincipalBasis =
        serde_json::from_value(input["actor"].clone()).map_err(|e| e.to_string())?;
    let scopes: Vec<String> = input["target_scope_refs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let decision = evaluate_capability(
        &profile,
        &principal,
        input["capability"].as_str().ok_or("capability")?,
        input["governance_root_ref"].as_str().ok_or("root")?,
        &scopes,
    );
    Ok(match decision {
        ois_kernel::governance::CapabilityDecision::Authorized { authorized, grant } => json!({
            "authorized": authorized,
            "reason": Value::Null,
            "grant_principal": grant.principal_ref,
        }),
        ois_kernel::governance::CapabilityDecision::Denied { authorized, reason } => json!({
            "authorized": authorized,
            "reason": serde_json::to_value(reason).unwrap_or(json!(null)),
            "grant_principal": Value::Null,
        }),
    })
}

fn project_write(
    outcome: &ois_kernel::GovernedWriteOutcome,
    input: &Value,
    command: &GovernedWriteCommand,
) -> Result<Value, String> {
    let outcome_v = serde_json::to_value(outcome).map_err(|e| e.to_string())?;
    let mut out = Map::new();
    for key in [
        "disposition",
        "reason_code",
        "transition_class",
        "change_class",
        "churn_key",
        "resolved_base_recorded_at",
    ] {
        out.insert(key.into(), opt_str(&outcome_v, key).unwrap_or(Value::Null));
    }
    if let Some(record) = outcome.record.as_ref() {
        out.insert(
            "record_root".into(),
            record.governance_root_ref.clone().into(),
        );
    }
    if let Some(event) = outcome.event.as_ref() {
        out.insert("event_object_type".into(), event.object_type.clone().into());
        out.insert(
            "event_lifecycle".into(),
            serde_json::to_value(event.lifecycle_state).unwrap_or(Value::Null),
        );
        out.insert(
            "event_kind".into(),
            event
                .object
                .get("event_kind")
                .cloned()
                .unwrap_or(Value::Null),
        );
        out.insert(
            "event_decision_ref".into(),
            event
                .object
                .get("originating_decision_ref")
                .cloned()
                .unwrap_or(Value::Null),
        );
        out.insert(
            "event_origin_root".into(),
            event
                .object
                .get("origin_governance_root_ref")
                .cloned()
                .unwrap_or(Value::Null),
        );
        out.insert(
            "event_ref".into(),
            json!(format!("{}:{}", event.object_type, event.object_id)),
        );
    }
    if let (Some(record), Some(event)) = (outcome.record.as_ref(), outcome.event.as_ref()) {
        let event_ref = format!("{}:{}", event.object_type, event.object_id);
        let cites = record.provenance_refs.iter().any(|r| r == &event_ref);
        out.insert("record_cites_event".into(), Value::Bool(cites));
    }

    if let Some(original) = outcome.original.as_ref() {
        let mut orig = serde_json::to_value(original).map_err(|e| e.to_string())?;
        if orig.get("reason_code").is_none() {
            orig["reason_code"] = Value::Null;
        }
        out.insert("original".into(), orig);
    }
    // Derived projections (pure functions of the outcome, asserted where set).
    out.insert(
        "churn_key_nonempty".into(),
        Value::Bool(outcome.churn_key.is_some()),
    );
    out.insert("no_event".into(), Value::Bool(outcome.event.is_none()));
    out.insert(
        "no_churn_key".into(),
        Value::Bool(outcome.churn_key.is_none()),
    );
    out.insert("no_record".into(), Value::Bool(outcome.record.is_none()));
    if let Some(event) = outcome.event.as_ref() {
        out.insert(
            "churn_key_equals_event_ref".into(),
            Value::Bool(
                outcome.churn_key.as_deref()
                    == Some(&format!("{}:{}", event.object_type, event.object_id)),
            ),
        );
    }
    if let Some(needles) = input.get("provenance_contains").and_then(|n| n.as_array()) {
        let refs: Vec<String> = outcome
            .record
            .as_ref()
            .map(|r| r.provenance_refs.clone())
            .unwrap_or_default();
        out.insert(
            "record_provenance_contains".into(),
            Value::Bool(
                needles
                    .iter()
                    .filter_map(|n| n.as_str())
                    .all(|n| refs.iter().any(|r| r == n)),
            ),
        );
    }
    if input.get("compute_recording_lag").is_some() {
        let next = command.next.clone();
        let recorded = epoch_seconds(&next.recorded_at);
        let valid = next
            .valid_at
            .as_deref()
            .map(epoch_seconds)
            .unwrap_or(recorded);
        out.insert("recording_lag_seconds".into(), json!(recorded - valid));
    }
    Ok(Value::Object(out))
}

fn run_write(input: &Value) -> Result<Value, String> {
    let command: GovernedWriteCommand =
        serde_json::from_value(input["command"].clone()).map_err(|e| e.to_string())?;
    let outcome = apply_governed_write(
        &command,
        input["governance_root_ref"].as_str().ok_or("root")?,
        &scopes_of(input),
    )
    .map_err(|e| e.to_string())?;
    let mut result = project_write(&outcome, input, &command)?;
    if input.get("run_twice").is_some() {
        let second = apply_governed_write(
            &command,
            input["governance_root_ref"].as_str().unwrap(),
            &scopes_of(input),
        )
        .map_err(|e| e.to_string())?;
        let second_v = serde_json::to_value(&second).map_err(|e| e.to_string())?;
        result["event_equal"] = Value::Bool(
            second
                .event
                .as_ref()
                .map(|e| format!("{}:{}", e.object_type, e.object_id))
                == outcome
                    .event
                    .as_ref()
                    .map(|e| format!("{}:{}", e.object_type, e.object_id)),
        );
        result["churn_key_equal"] = Value::Bool(
            second_v["churn_key"].as_str().map(String::from)
                == serde_json::to_value(&outcome).map_err(|e| e.to_string())?["churn_key"]
                    .as_str()
                    .map(String::from),
        );
    }
    Ok(result)
}

fn scopes_of(input: &Value) -> Vec<String> {
    input["target_scope_refs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn run_accept(input: &Value) -> Result<Value, String> {
    let mut command: CrossScopeAcceptanceCommand =
        serde_json::from_value(input["command"].clone()).map_err(|e| e.to_string())?;
    if input.get("prior_digest_of").and_then(|v| v.as_str()) == Some("origin_snapshot") {
        command.prior_accepted_origin_digest = Some(ois_kernel::canonical::canonical_digest(
            &serde_json::to_value(&command.origin_snapshot).map_err(|e| e.to_string())?,
        ));
    }
    let outcome = apply_accept(&command, input)?;
    project_write_outcome(&outcome, input, &command)
}

fn apply_accept(
    command: &CrossScopeAcceptanceCommand,
    input: &Value,
) -> Result<ois_kernel::GovernedWriteOutcome, String> {
    accept_cross_scope(
        command,
        input["governance_root_ref"].as_str().ok_or("root")?,
        &scopes_of(input),
    )
    .map_err(|e| e.to_string())
}

fn project_write_outcome(
    outcome: &ois_kernel::GovernedWriteOutcome,
    input: &Value,
    command: &CrossScopeAcceptanceCommand,
) -> Result<Value, String> {
    let outcome_v = serde_json::to_value(outcome).map_err(|e| e.to_string())?;
    let mut out = Map::new();
    for key in [
        "disposition",
        "reason_code",
        "transition_class",
        "change_class",
        "churn_key",
        "resolved_base_recorded_at",
    ] {
        out.insert(key.into(), opt_str(&outcome_v, key).unwrap_or(Value::Null));
    }
    if let Some(record) = outcome.record.as_ref() {
        out.insert(
            "record_root".into(),
            record.governance_root_ref.clone().into(),
        );
    }
    if let Some(event) = outcome.event.as_ref() {
        out.insert("event_object_type".into(), event.object_type.clone().into());
        out.insert(
            "event_lifecycle".into(),
            serde_json::to_value(event.lifecycle_state).unwrap_or(Value::Null),
        );
        out.insert(
            "event_kind".into(),
            event
                .object
                .get("event_kind")
                .cloned()
                .unwrap_or(Value::Null),
        );
        out.insert(
            "event_decision_ref".into(),
            event
                .object
                .get("originating_decision_ref")
                .cloned()
                .unwrap_or(Value::Null),
        );
        out.insert(
            "event_origin_root".into(),
            event
                .object
                .get("origin_governance_root_ref")
                .cloned()
                .unwrap_or(Value::Null),
        );
        out.insert(
            "event_ref".into(),
            json!(format!("{}:{}", event.object_type, event.object_id)),
        );
    }
    if let (Some(record), Some(event)) = (outcome.record.as_ref(), outcome.event.as_ref()) {
        let event_ref = format!("{}:{}", event.object_type, event.object_id);
        let cites = record.provenance_refs.iter().any(|r| r == &event_ref);
        out.insert("record_cites_event".into(), Value::Bool(cites));
    }
    if let Some(original) = outcome.original.as_ref() {
        let mut orig = serde_json::to_value(original).map_err(|e| e.to_string())?;
        if orig.get("reason_code").is_none() {
            orig["reason_code"] = Value::Null;
        }
        out.insert("original".into(), orig);
    }
    out.insert(
        "churn_key_nonempty".into(),
        Value::Bool(outcome.churn_key.is_some()),
    );
    out.insert("no_event".into(), Value::Bool(outcome.event.is_none()));
    out.insert(
        "no_churn_key".into(),
        Value::Bool(outcome.churn_key.is_none()),
    );
    out.insert("no_record".into(), Value::Bool(outcome.record.is_none()));
    if let Some(event) = outcome.event.as_ref() {
        out.insert(
            "churn_key_equals_event_ref".into(),
            Value::Bool(
                outcome.churn_key.as_deref()
                    == Some(&format!("{}:{}", event.object_type, event.object_id)),
            ),
        );
    }
    if let Some(needles) = input.get("provenance_contains").and_then(|n| n.as_array()) {
        let refs: Vec<String> = outcome
            .record
            .as_ref()
            .map(|r| r.provenance_refs.clone())
            .unwrap_or_default();
        out.insert(
            "record_provenance_contains".into(),
            Value::Bool(
                needles
                    .iter()
                    .filter_map(|n| n.as_str())
                    .all(|n| refs.iter().any(|r| r == n)),
            ),
        );
    }
    if input.get("compute_recording_lag").is_some() {
        let recorded = epoch_seconds(&command.origin_snapshot.recorded_at);
        let valid = command
            .origin_snapshot
            .valid_at
            .as_deref()
            .map(epoch_seconds)
            .unwrap_or(recorded);
        out.insert("recording_lag_seconds".into(), json!(recorded - valid));
    }
    Ok(Value::Object(out))
}

fn run_classify(input: &Value) -> Result<Value, String> {
    let axis = match input["axis"].as_str().ok_or("axis")? {
        "assertion" => "assertion",
        "governance_rule" => "governance_rule",
        "objective_criterion" => "objective_criterion",
        other => return Err(format!("unknown axis {other}")),
    };
    let valid_from = input["valid_from"].as_str();
    let observation = input["observation_started_at"].as_str();
    Ok(json!({
        "change_class": classify_change_class(axis, valid_from, observation),
    }))
}

fn run_stale_base(input: &Value) -> Result<Value, String> {
    let (stale, resolved_base) =
        is_stale_base(&input["resolved"], input["base_recorded_at"].as_str())
            .map_err(|e| e.to_string())?;
    Ok(json!({
        "stale": stale,
        "resolved_base_recorded_at": resolved_base,
    }))
}

fn run_promotion(input: &Value) -> Result<Value, String> {
    let command: PromotionBatchCommand =
        serde_json::from_value(input["command"].clone()).map_err(|e| e.to_string())?;
    let outcomes = promote_projection_edges(&command).map_err(|e| e.to_string())?;
    let items: Vec<Value> = outcomes
        .iter()
        .map(|o| {
            let mut m = Map::new();
            m.insert(
                "promotion_disposition".into(),
                o.promotion_disposition.clone().into(),
            );
            m.insert("reason_code".into(), o.reason_code.clone().into());
            m.insert("evidence_state".into(), o.evidence_state.clone().into());
            m.insert("authority_state".into(), o.authority_state.clone().into());
            m.insert(
                "confirmation_mode".into(),
                o.confirmation_mode.clone().into(),
            );
            m.insert(
                "has_churn_key".into(),
                Value::Bool(
                    o.applied
                        .as_ref()
                        .map(|a| !a.churn_key.is_empty())
                        .unwrap_or(false),
                ),
            );
            m.insert(
                "delta_change_class".into(),
                o.applied
                    .as_ref()
                    .and_then(|a| a.change_class.clone())
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            Value::Object(m)
        })
        .collect();
    Ok(json!({ "items": items }))
}

// ---------------------------------------------------------------------------
// Stage-4a kernel-surface runners: decision surfacing, criterion
// currentness, governance-event churn origins, and context manifests.
// Projections are normalized to the TypeScript driver's shapes 1:1.
// ---------------------------------------------------------------------------

fn kernel_envelopes(value: &Value) -> Result<Vec<ois_kernel::KernelEnvelope>, String> {
    serde_json::from_value::<Vec<ois_kernel::KernelEnvelope>>(value.clone())
        .map_err(|e| format!("envelope parse: {e}"))
}

/// Decisions that apply to a resource (exact full-ref match on
/// `applies_to_refs`, input order preserved), projected to the driver's
/// normalized shape.
fn run_decision_surface(input: &Value) -> Result<Value, String> {
    let decisions: Vec<ois_kernel::KernelEnvelope> =
        kernel_envelopes(input.get("decisions").ok_or("missing decisions")?)?;
    let resource_ref = input["resource_ref"]
        .as_str()
        .ok_or("missing resource_ref")?
        .to_string();
    let records: Vec<ois_kernel::DecisionRecord> = decisions
        .iter()
        .map(ois_kernel::decision_record_from_envelope)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.0)?;
    // The DecisionRecord is envelope-free; walk (envelope, record) pairs in
    // input order (oldest-first) so each ref comes from its own envelope.
    let items: Vec<Value> = decisions
        .iter()
        .zip(records.iter())
        .filter(|(_, record)| record.applies_to_refs.iter().any(|r| r == &resource_ref))
        .map(|(envelope, record)| {
            json!({
                "decision_ref": ois_kernel::decisions::decision_ref(envelope),
                "decision_kind": record.decision_kind,
                "outcome": record.outcome,
                "recorded_at": record.recorded_at,
            })
        })
        .collect();
    let count = items.len();
    Ok(json!({ "items": items, "count": count }))
}

/// Criterion currentness from an objective's version history.
fn run_criterion(input: &Value) -> Result<Value, String> {
    let history = kernel_envelopes(
        input
            .get("objective_history")
            .ok_or("missing objective_history")?,
    )?;
    let criterion_id = input["criterion_id"]
        .as_str()
        .ok_or("missing criterion_id")?;
    let resolved = ois_kernel::resolve_criterion(&history, criterion_id).map_err(|e| e.0)?;
    Ok(json!({
        "state": resolved.state_word(),
        "criterion_version": resolved.criterion_version,
        "superseded_version": resolved.superseded_version,
        "objective_version_recorded_at": resolved
            .objective_version
            .as_ref()
            .map(|e| e.recorded_at.clone()),
    }))
}

/// Per-window unique churn origins and distinct touched units.
fn run_churn(input: &Value) -> Result<Value, String> {
    let decisions = kernel_envelopes(input.get("decisions").ok_or("missing decisions")?)?;
    let events = kernel_envelopes(
        input
            .get("governance_events")
            .ok_or("missing governance_events")?,
    )?;
    let keys: Vec<String> = serde_json::from_value(
        input
            .get("in_window_churn_keys")
            .ok_or("missing in_window_churn_keys")?
            .clone(),
    )
    .map_err(|e| e.to_string())?;
    let window = input.get("window").ok_or("missing window")?;
    let start = window["start"].as_str().ok_or("window.start")?;
    let end = window["end"].as_str().ok_or("window.end")?;
    let unique = ois_kernel::unique_churn_origins(&keys, &decisions, &events, start, end)
        .map_err(|e| e.0)?;
    let touched = ois_kernel::events_surface::touched_units(&events, start, end);
    Ok(json!({ "unique_churn_origins": unique, "touched_units": touched }))
}

/// Context-manifest compilation per the frozen schema contract.
fn run_manifest(input: &Value) -> Result<Value, String> {
    let compile = input.get("compile").ok_or("missing compile")?;
    let omits = compile
        .get("omissions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let omissions: Vec<ois_kernel::Omission> = omits
        .iter()
        .map(|o| {
            serde_json::from_value::<ois_kernel::Omission>(o.clone())
                .map_err(|e| format!("omission parse: {e}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let str_array = |key: &str| -> Vec<String> {
        compile
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let manifest = ois_kernel::compile_manifest(&ois_kernel::context_manifest::CompileInput {
        manifest_id: compile["manifest_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        governance_root_ref: compile["governance_root_ref"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        scope_refs: str_array("scope_refs"),
        activity_ref: compile["activity_ref"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        depth: compile["depth"].as_str().unwrap_or_default().to_string(),
        included_resource_refs: str_array("included_resource_refs"),
        included_checkpoint_refs: str_array("included_checkpoint_refs"),
        included_activity_refs: str_array("included_activity_refs"),
        omissions,
        basis_checkpoint_refs: str_array("basis_checkpoint_refs"),
        basis_as_known_at: compile
            .get("basis_as_known_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        compiled_at: compile["compiled_at"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    })
    .map_err(|e| e.0)?;
    let omitted_kinds: Vec<String> = manifest
        .omitted
        .as_ref()
        .map(|o| o.iter().map(|x| x.omission_kind.clone()).collect())
        .unwrap_or_default();
    let omitted_count = omitted_kinds.len();
    Ok(json!({
        "completeness": manifest.completeness,
        "permission_envelope_ref": manifest.permission_envelope_ref,
        "omitted_count": omitted_count,
        "omitted_kinds": omitted_kinds,
        "manifest_digest": manifest.manifest_digest,
    }))
}

fn run_case(tc: &Value) -> Result<Value, String> {
    let input = &tc["input"];
    match tc["engine"].as_str().unwrap_or_default() {
        "resolver" => run_resolver(input),
        "capability" => run_capability(input),
        "governed_write" => run_write(input),
        "cross_scope_acceptance" => run_accept(input),
        "classify" => run_classify(input),
        "stale_base" => run_stale_base(input),
        "promotion" => run_promotion(input),
        "decision_surface" => run_decision_surface(input),
        "criterion" => run_criterion(input),
        "churn_origins" => run_churn(input),
        "manifest" => run_manifest(input),
        "fixture" => fixture_scenarios::run_fixture_scenario(input),
        other => Err(format!("unknown engine {other}")),
    }
}

// ---------------------------------------------------------------------------
// Frozen-expectation checking (mirrors the TypeScript driver's checker).
// ---------------------------------------------------------------------------

fn check_expectations(tc: &Value, run: &Result<Value, String>) -> Vec<String> {
    let mut failures = Vec::new();
    let a = &tc["assert"];
    if a.get("error").and_then(|v| v.as_bool()) == Some(true) {
        if run.is_ok() {
            failures.push("expected error, got success".into());
        }
        return failures;
    }
    let r = match run {
        Ok(r) => r,
        Err(e) => {
            failures.push(format!("unexpected error: {e}"));
            return failures;
        }
    };
    if let Some(expected_items) = a.get("items").and_then(|v| v.as_array()) {
        // Per-item SUBSET match: the oracle tests assert specific fields per
        // promotion outcome, not whole-record equality.
        let items = r["items"].as_array().cloned().unwrap_or_default();
        if items.len() != expected_items.len() {
            failures.push(format!(
                "item count: expected {}, got {}",
                expected_items.len(),
                items.len()
            ));
        }
        for (i, exp) in expected_items.iter().enumerate() {
            let act = items.get(i).cloned().unwrap_or(json!({}));
            for (k, v) in exp.as_object().unwrap() {
                if &act[k] != v {
                    failures.push(format!(
                        "item {i} {k}: expected {v}, got {}",
                        act.get(k).cloned().unwrap_or(Value::Null)
                    ));
                }
            }
        }
        return failures;
    }
    for (key, expected) in a.as_object().unwrap() {
        let actual = r.get(key).cloned();
        if let Some(real_key) = key.strip_suffix("_not") {
            if r.get(real_key) == Some(expected) {
                failures.push(format!("{real_key}: should not be {expected}"));
            }
            continue;
        }
        if actual.as_ref() != Some(expected) {
            failures.push(format!(
                "{key}: expected {expected}, got {}",
                actual.unwrap_or(Value::Null)
            ));
        }
    }
    failures
}

/// Cross-engine comparison against the TypeScript oracle's normalized results.
/// Error messages are engine-internal vocabulary, so error equality is judged
/// by PRESENCE on both sides, never by string.
fn compare_with_ts(row: &Value, run: &Result<Value, String>, is_fixture: bool) -> Vec<String> {
    let mut failures = Vec::new();
    let id = row["id"].as_str().unwrap_or("?");
    match (&row["error"], run) {
        (e, Err(mine)) if !e.is_null() => {} // both errored: presence-level parity
        (e, _) if !e.is_null() => failures.push(format!("{id}: TS errored, Rust succeeded")),
        (_, Err(mine)) => failures.push(format!("{id}: Rust errored where TS did not: {mine}")),
        (_, Ok(mine)) => {
            let ts = &row["result"];
            if is_fixture {
                // The fixture TS reference emits the partition-bound projection
                // (assert-shaped): every bound field must appear in the Rust
                // result with an equal value. Rust items may carry additional
                // kernel-derived fields beyond the oracle's binding surface.
                let diverges = match (
                    ts.get("items").and_then(|v| v.as_array()),
                    mine.get("items").and_then(|v| v.as_array()),
                ) {
                    (Some(ts_items), Some(rust_items)) => {
                        for (index, ts_item) in ts_items.iter().enumerate() {
                            let Some(ts_obj) = ts_item.as_object() else {
                                continue;
                            };
                            let Some(rust_item) = rust_items.get(index).and_then(|v| v.as_object())
                            else {
                                failures
                                    .push(format!("{id}: TS item {index} has no Rust counterpart"));
                                continue;
                            };
                            for (key, ts_value) in ts_obj {
                                if rust_item.get(key) != Some(ts_value) {
                                    failures.push(format!(
                                        "{id}: item {index} field {key} diverges: ts {ts_value} vs rust {}",
                                        rust_item
                                            .get(key)
                                            .map(|v| v.to_string())
                                            .unwrap_or_else(|| "<absent>".into())
                                    ));
                                }
                            }
                        }
                        false
                    }
                    _ => ts != mine,
                };
                if diverges {
                    failures.push(format!(
                        "{id}: normalized result diverges\n  ts:   {}\n  rust: {}",
                        serde_json::to_string(ts).unwrap_or_default(),
                        serde_json::to_string(mine).unwrap_or_default(),
                    ));
                }
            } else if ts != mine {
                failures.push(format!(
                    "{id}: normalized result diverges\n  ts:   {}\n  rust: {}",
                    serde_json::to_string(ts).unwrap_or_default(),
                    serde_json::to_string(mine).unwrap_or_default(),
                ));
            }
        }
    }
    failures
}
/// Divergences ratified as documented conflicts in STAGE4-REPORT.md. Each
/// entry is (case id, failure substring): a failure only counts as explained
/// when BOTH match, so any new field drift on these cases — or any divergence
/// on any other case — still fails the suite. The single entry is the
/// GR-E04 disposition conflict: the authored Gentle Roost oracle requires
/// pending_approval for a candidate dependency edge promoted with one-sided
/// evidence, while the ratified work-engine p14a semantics (shared kernel
/// mapping, pinned TS work engine included) return rejected for the same
/// input shape. The kernel mapping was not bent for either side.
const EXPLAINED_DIVERGENCES: &[(&str, &str)] = &[("fixture.gr_e04", "disposition")];

fn is_explained_divergence(id: &str, failure: &str) -> bool {
    EXPLAINED_DIVERGENCES
        .iter()
        .any(|(cid, sub)| id == *cid && failure.contains(sub))
}

#[test]
fn differential_parity() {
    let corpus: Value = serde_json::from_str(CORPUS).expect("parity corpus must parse");
    let cases = corpus["cases"].as_array().expect("cases array");
    let ts_rows: Option<Map<String, Value>> = std::env::var("TS_RESULTS_PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .map(|p| {
            let raw = fs::read_to_string(&p).expect("TS_RESULTS_PATH readable");
            let parsed: Value = serde_json::from_str(&raw).expect("ts-results.json parses");
            parsed["rows"]
                .as_array()
                .expect("rows array")
                .iter()
                .map(|r| (r["id"].as_str().unwrap_or_default().to_string(), r.clone()))
                .collect::<Map<String, Value>>()
        });

    let mut pass = 0usize;
    let mut total = 0usize;
    let mut all_failures: Vec<String> = Vec::new();
    let mut ts_divergences = 0usize;
    let mut explained_count = 0usize;

    for tc in cases {
        let id = tc["id"].as_str().unwrap_or_default().to_string();
        total += 1;
        let run = run_case(tc);
        let mut failures = check_expectations(tc, &run);
        if let (Some(rows), Ok(_)) = (&ts_rows, &run) {
            if let Some(row) = rows.get(&id) {
                let is_fixture = tc["engine"] == "fixture";
                let mut ts_fail = compare_with_ts(row, &run, is_fixture);
                ts_divergences += ts_fail.len();
                failures.append(&mut ts_fail);
            }
        }
        let (explained, unexplained): (Vec<String>, Vec<String>) = failures
            .into_iter()
            .partition(|f| is_explained_divergence(&id, f));
        for f in &explained {
            println!("EXPLAINED divergence (documented in STAGE4-REPORT.md) [{id}]: {f}");
        }
        explained_count += explained.len();
        if unexplained.is_empty() {
            pass += 1;
        } else {
            for f in &unexplained {
                all_failures.push(format!("FAIL {id}\n   - {f}"));
            }
        }
    }

    if let Some(_rows) = &ts_rows {
        println!("rust-vs-ts differential: {ts_divergences} cross-engine divergences ({explained_count} explained)");
    } else {
        println!(
            "TS_RESULTS_PATH not set \u{2014} frozen expectations asserted; cross-engine diff skipped"
        );
    }
    println!(
        "rust-oracle: {pass} pass, {} fail of {total} ({explained_count} explained divergences)",
        total - pass
    );
    for f in &all_failures {
        println!("{f}");
    }
    assert!(
        all_failures.is_empty(),
        "parity failures: {} of {total} cases failed",
        total - pass
    );
}
