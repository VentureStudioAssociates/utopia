//! OIS kernel surface persistence for `utopia-store`: typed additive access
//! over the stage-4a side tables (`kernel_decision_refs`,
//! `kernel_objective_criteria`, `kernel_governance_events`,
//! `kernel_manifest_derivations`) declared in migration
//! `0038_ois_kernel_surfaces.sql`.
//!
//! Delta-budget discipline (CONVERGENCE.md): this module is NEW. The kernel's
//! resolution logic stays pure and fail-closed in `ois-kernel`; this adapter
//! only mirrors already-validated kernel objects into indexed side tables and
//! reads them back. Like `ois_kernel.rs`, kernel-side storage is
//! failure-isolated — the ordinary ingest path must never be gated by the
//! shadow tables, and nothing here alters existing facts, ontology, or query
//! behavior.

use chrono::{DateTime, Utc};
use ois_kernel::canonical::canonical_digest;
use ois_kernel::decisions::DecisionRecord;
use ois_kernel::envelope::KernelEnvelope;
use ois_kernel::objectives::CriterionSummary;
use serde_json::Value;

/// The content-addressed envelope id: the canonical digest of the serialized
/// envelope, matching the spike's `kernel_envelopes.id` convention.
fn envelope_id(envelope: &KernelEnvelope) -> Result<String, String> {
    let payload = serde_json::to_value(envelope).map_err(|e| e.to_string())?;
    Ok(canonical_digest(&payload))
}

/// Parses a kernel RFC 3339 instant into a `TIMESTAMPTZ`-bindable value.
/// Fail-closed: an unparseable instant is a store error, never a silent null.
fn ts(instant: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(instant)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| format!("invalid RFC 3339 instant {instant:?}: {e}"))
}

/// The `type` half of a full `type:id` ref. Fail-closed: bare ids are rejected
/// — the surfacing query matches full refs exactly, so a partial ref must not
/// be silently indexed as if it were complete.
fn applied_resource_type(applied_ref: &str) -> Result<&str, String> {
    let (kind, id) = applied_ref
        .split_once(':')
        .ok_or_else(|| format!("applied_ref {applied_ref:?} is not a full type:id ref"))?;
    if kind.is_empty() || id.is_empty() {
        return Err(format!(
            "applied_ref {applied_ref:?} is not a full type:id ref"
        ));
    }
    Ok(kind)
}

/// Async sqlx persistence for the stage-4a surface side tables. Kernel logic
/// is pure and sync; this adapter is concrete rather than a trait impl
/// (runtime queries only; builds need no database).
pub struct PgKernelSurfaceStore<'a> {
    pool: &'a sqlx::PgPool,
}

impl<'a> PgKernelSurfaceStore<'a> {
    pub fn new(pool: &'a sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Mirrors one validated decision into the applicability lookup: one row
    /// per `applies_to` ref. Decision references stay the kernel's exact
    /// `decision:<object_id>` form — no bare-id coercion.
    pub async fn record_decision(
        &self,
        envelope: &KernelEnvelope,
        record: &DecisionRecord,
    ) -> Result<(), String> {
        let recorded_at = ts(&record.recorded_at)?;
        let id = envelope_id(envelope)?;
        let gov_root = &envelope.governance_root_ref;
        let decision_ref = format!("decision:{}", envelope.object_id);
        for applies_to_ref in &record.applies_to_refs {
            sqlx::query(
                "INSERT INTO kernel_decision_refs
                     (envelope_id, gov_root, decision_ref, decision_kind, outcome,
                      applies_to_ref, recorded_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (envelope_id, applies_to_ref) DO NOTHING",
            )
            .bind(&id)
            .bind(gov_root)
            .bind(&decision_ref)
            .bind(&record.decision_kind)
            .bind(&record.outcome)
            .bind(applies_to_ref)
            .bind(recorded_at)
            .execute(self.pool)
            .await
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Mirrors one objective version's criterion summaries. Supersession
    /// between versions is derived by the pure resolver and never stored.
    pub async fn record_objective_criteria(
        &self,
        envelope: &KernelEnvelope,
        criteria: &[CriterionSummary],
    ) -> Result<(), String> {
        let recorded_at = ts(&envelope.recorded_at)?;
        let id = envelope_id(envelope)?;
        for criterion in criteria {
            sqlx::query(
                "INSERT INTO kernel_objective_criteria
                     (envelope_id, gov_root, criterion_id, criterion_version,
                      lifecycle, recorded_at)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (envelope_id, criterion_id) DO NOTHING",
            )
            .bind(&id)
            .bind(&envelope.governance_root_ref)
            .bind(&criterion.criterion_id)
            .bind(criterion.criterion_version)
            .bind(&criterion.lifecycle)
            .bind(recorded_at)
            .execute(self.pool)
            .await
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Mirrors one emitted governance event (decision lineage, touched unit,
    /// churn key) for window queries over inclusive recorded intervals.
    pub async fn record_governance_event(&self, event: &KernelEnvelope) -> Result<(), String> {
        let applied_ref = event
            .object
            .get("applied_ref")
            .and_then(Value::as_str)
            .ok_or("governance event payload is missing applied_ref")?
            .to_string();
        let resource_type = applied_resource_type(&applied_ref)?;
        let recorded_at = ts(&event.recorded_at)?;
        let id = envelope_id(event)?;
        let churn_key = event.object.get("churn_key").and_then(Value::as_str);
        let originating_decision_ref = event
            .object
            .get("originating_decision_ref")
            .and_then(Value::as_str);
        sqlx::query(
            "INSERT INTO kernel_governance_events
                 (envelope_id, gov_root, applied_ref, applied_resource_type,
                  churn_key, originating_decision_ref, recorded_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (envelope_id) DO NOTHING",
        )
        .bind(&id)
        .bind(&event.governance_root_ref)
        .bind(&applied_ref)
        .bind(resource_type)
        .bind(churn_key)
        .bind(originating_decision_ref)
        .bind(recorded_at)
        .execute(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Content-addressed manifest cache: identical derivation keys (principal,
    /// permission basis, governance root, query digest) recompile to the
    /// identical manifest digest, so a repeat derivation is a no-op and a
    /// cached hit can never be stale.
    pub async fn cache_manifest(
        &self,
        derivation_key: &str,
        manifest: &Value,
    ) -> Result<(), String> {
        let manifest_digest = ois_kernel::canonical::canonical_digest(manifest);
        let gov_root = manifest
            .get("governance_root_ref")
            .and_then(Value::as_str)
            .ok_or("manifest payload is missing governance_root_ref")?;
        sqlx::query(
            "INSERT INTO kernel_manifest_derivations
                 (derivation_key, gov_root, manifest_digest, manifest)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (derivation_key) DO NOTHING",
        )
        .bind(derivation_key)
        .bind(gov_root)
        .bind(&manifest_digest)
        .bind(manifest)
        .execute(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Reads a cached manifest by derivation key, if one was derived before.
    pub async fn cached_manifest(&self, derivation_key: &str) -> Result<Option<Value>, String> {
        let row: Option<(Value,)> = sqlx::query_as(
            "SELECT manifest FROM kernel_manifest_derivations WHERE derivation_key = $1",
        )
        .bind(derivation_key)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(row.map(|(manifest,)| manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_instants() {
        let parsed = ts("2026-09-01T00:00:00.000Z").expect("valid instant parses");
        assert_eq!(parsed.to_rfc3339(), "2026-09-01T00:00:00+00:00");
        assert!(ts("not-an-instant").is_err());
    }

    #[test]
    fn resource_type_requires_full_refs() {
        assert_eq!(
            applied_resource_type("legal_document:hub-7").expect("full ref"),
            "legal_document"
        );
        assert!(applied_resource_type("hub-7").is_err());
        assert!(applied_resource_type(":hub-7").is_err());
        assert!(applied_resource_type("legal_document:").is_err());
    }
}
