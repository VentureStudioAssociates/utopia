//! OIS governed kernel persistence for `utopia-store`: the additive side
//! tables (`kernel_envelopes`, `governed_operations`) and the minimal
//! ingestion hook that turns a Utopia fact into a CANDIDATE envelope.
//!
//! Spike discipline (CONVERGENCE.md delta budget): this module is NEW — the
//! only edits to existing upstream files are the module registration, the
//! one-call hook in `graph::insert_fact_inner`, and the workspace dependency
//! on `ois-kernel`. Nothing here alters existing facts, ontology, or query
//! tables; failures in the kernel side-record are deliberately failure-
//! isolated (logged, never propagated) so the ordinary ingest path cannot be
//! gated by the spike's shadow storage.

use ois_kernel::canonical::canonical_digest;
use ois_kernel::envelope::KernelEnvelope;
use ois_kernel::ingest::{candidate_envelope_from_fact, FactRef};
use serde_json::Value;

/// Async sqlx persistence for the governed-kernel side tables. Exposes the
/// same operations as the kernel's sync `EnvelopeStore` seam — sqlx is async,
/// the kernel logic is pure and sync, so this adapter is concrete rather than
/// a trait impl (runtime queries only; builds need no database).
pub struct PgEnvelopeStore<'a> {
    pool: &'a sqlx::PgPool,
}

impl<'a> PgEnvelopeStore<'a> {
    pub fn new(pool: &'a sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Appends one kernel envelope version. Append-only: content-addressed id,
    /// re-recording the same version is a no-op, never rewritten in place.
    pub async fn record_envelope(&self, envelope: &KernelEnvelope) -> Result<(), String> {
        let payload = serde_json::to_value(envelope).map_err(|e| e.to_string())?;
        let id = canonical_digest(&payload);
        let evidence_refs: Vec<String> = envelope.provenance_refs.clone();
        // The kernel vocabularies are serde enums, not sqlx types — bind their
        // serialized string form (matches the migration's TEXT columns).
        let authority = serde_string(&envelope.authority_class)?;
        let lifecycle = serde_string(&envelope.lifecycle_state)?;
        sqlx::query(
            "INSERT INTO kernel_envelopes
                 (id, gov_root, object_type, object_id, authority_class, lifecycle,
                  valid_at, as_known_at, recorded_at, payload, evidence_refs)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&id)
        .bind(&envelope.governance_root_ref)
        .bind(&envelope.object_type)
        .bind(&envelope.object_id)
        .bind(&authority)
        .bind(&lifecycle)
        .bind(&envelope.valid_at)
        .bind(&envelope.as_known_at)
        .bind(&envelope.recorded_at)
        .bind(&payload)
        .bind(&evidence_refs)
        .execute(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Every recorded version of one object (any recorded_at), any order —
    /// the resolver owns ordering.
    pub async fn history_for_object(&self, object_id: &str) -> Result<Vec<KernelEnvelope>, String> {
        let rows: Vec<(Value,)> =
            sqlx::query_as("SELECT payload FROM kernel_envelopes WHERE object_id = $1")
                .bind(object_id)
                .fetch_all(self.pool)
                .await
                .map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|(payload,)| serde_json::from_value(payload).map_err(|e| e.to_string()))
            .collect()
    }

    /// Root-scoped variant of `history_for_object`: governance-root filtering
    /// happens in the query as well as the resolver — the side tables may hold
    /// envelopes from several roots, and the read surface never lets one
    /// session's loader scan another's.
    pub async fn history_for_object_in_root(
        &self,
        gov_root: &str,
        object_id: &str,
    ) -> Result<Vec<KernelEnvelope>, String> {
        let rows: Vec<(Value,)> = sqlx::query_as(
            "SELECT payload FROM kernel_envelopes WHERE object_id = $1 AND gov_root = $2",
        )
        .bind(object_id)
        .bind(gov_root)
        .fetch_all(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|(payload,)| serde_json::from_value(payload).map_err(|e| e.to_string()))
            .collect()
    }

    /// Root-scoped variant of `related_records` (same supersession shape,
    /// confined to the session's governance root).
    pub async fn related_records_in_root(
        &self,
        gov_root: &str,
        object_id: &str,
    ) -> Result<Vec<KernelEnvelope>, String> {
        let rows: Vec<(Value,)> = sqlx::query_as(
            "SELECT payload FROM kernel_envelopes
             WHERE object_id <> $1
               AND gov_root = $2
               AND payload->'object'->>'superseded_assertion_ref' IS NOT NULL
               AND payload->'object'->>'superseded_assertion_ref' LIKE '%' || $1 || '%'",
        )
        .bind(object_id)
        .bind(gov_root)
        .fetch_all(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|(payload,)| serde_json::from_value(payload).map_err(|e| e.to_string()))
            .collect()
    }

    /// Records that may declare supersession of the target: a different
    /// object whose payload object names the target as its predecessor.
    pub async fn related_records(&self, object_id: &str) -> Result<Vec<KernelEnvelope>, String> {
        let rows: Vec<(Value,)> = sqlx::query_as(
            "SELECT payload FROM kernel_envelopes
             WHERE object_id <> $1
               AND payload->'object'->>'superseded_assertion_ref' IS NOT NULL
               AND payload->'object'->>'superseded_assertion_ref' LIKE '%' || $1 || '%'",
        )
        .bind(object_id)
        .fetch_all(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|(payload,)| serde_json::from_value(payload).map_err(|e| e.to_string()))
            .collect()
    }

    /// The recorded outcome of one applied governed MCP operation — the
    /// `governed_operations` row (oracle `PortRecordedOutcome`). Rejections
    /// are not recorded: they never reached the kernel, and replaying
    /// re-evaluates them deterministically.
    pub async fn recorded_operation(
        &self,
        op_id: &str,
    ) -> Result<Option<RecordedOperation>, String> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            chrono::DateTime<chrono::Utc>,
        )> = sqlx::query_as(
            "SELECT op_id, envelope_id, op_kind, actor, applied_at
                 FROM governed_operations WHERE op_id = $1",
        )
        .bind(op_id)
        .fetch_all(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .map(
                |(op_id, envelope_id, op_kind, actor, applied_at)| RecordedOperation {
                    op_id,
                    envelope_id,
                    op_kind,
                    actor,
                    applied_at,
                },
            )
            .next())
    }

    /// Records one applied governed MCP operation for idempotent replay.
    /// Append-only: the operation id is the primary key, so re-recording the
    /// same operation is a no-op — never a rewrite in place.
    pub async fn record_mcp_operation(
        &self,
        op_id: &str,
        envelope_id: &str,
        op_kind: &str,
        actor: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO governed_operations (op_id, envelope_id, op_kind, actor)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (op_id) DO NOTHING",
        )
        .bind(op_id)
        .bind(envelope_id)
        .bind(op_kind)
        .bind(actor)
        .execute(self.pool)
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// The kernel-side record of one applied governed MCP operation, as read
/// back from `governed_operations` for replay.
pub struct RecordedOperation {
    pub op_id: String,
    pub envelope_id: String,
    pub op_kind: String,
    pub actor: String,
    pub applied_at: chrono::DateTime<chrono::Utc>,
}

/// Arguments for the candidate-envelope hook, projected from the fact row the
/// ordinary ingest path just committed. Pure data — no SQL knowledge.
pub struct CandidateFact<'a> {
    pub fact_id: uuid::Uuid,
    pub kb_id: uuid::Uuid,
    pub subject_id: uuid::Uuid,
    pub predicate_id: Option<uuid::Uuid>,
    pub object_entity_id: Option<uuid::Uuid>,
    pub object_value: Option<&'a Value>,
    pub valid_from: Option<chrono::DateTime<chrono::Utc>>,
}

/// Minimal ingestion hook: a Utopia fact becomes a CANDIDATE envelope —
/// nothing more, no auto-promotion. The same fact always yields the same
/// envelope (content-addressed id, ON CONFLICT no-op).
pub async fn record_candidate_for_fact(
    pool: &sqlx::PgPool,
    fact: &CandidateFact<'_>,
) -> Result<(), String> {
    let fact_ref = FactRef {
        id: fact.fact_id.to_string(),
        subject_id: fact.subject_id.to_string(),
        predicate_ref: fact.predicate_id.map(|p| p.to_string()),
        object_id: fact.object_entity_id.map(|o| o.to_string()),
        object_value: fact.object_value.cloned(),
        valid_from: fact.valid_from.map(|t| t.to_rfc3339()),
        recorded_at: chrono::Utc::now().to_rfc3339(),
        governance_root_ref: format!("ois:scope:kb:{}", fact.kb_id),
        scope_refs: vec![],
    };
    let envelope = candidate_envelope_from_fact(&fact_ref);
    PgEnvelopeStore::new(pool).record_envelope(&envelope).await
}

/// Serialized string form of a kernel vocabulary value (serde enum), for
/// binding into TEXT columns without a sqlx::Type impl.
fn serde_string<T: serde::Serialize>(value: &T) -> Result<String, String> {
    let raw = serde_json::to_string(value).map_err(|e| e.to_string())?;
    Ok(raw.trim_matches('"').to_string())
}
