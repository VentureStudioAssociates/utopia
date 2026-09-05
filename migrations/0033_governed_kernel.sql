-- OIS governed kernel side tables (Phase-2 spike, spec art_cUh17iEH §'The spike
-- that decides it'). Purely additive: no existing table, column, or constraint
-- is altered. These tables shadow the ordinary `facts` path — a Utopia fact
-- ingests as a CANDIDATE kernel envelope (no auto-promotion); governed
-- transitions are recorded for replay detection and the recording-axis rewind.

-- One kernel envelope version (append-only; never rewritten in place).
-- `id` is content-addressed (canonical digest of the envelope), so re-recording
-- the same version is a no-op. `object_id` is the per-object history key the
-- resolver queries; `payload` carries the complete canonical envelope JSON.
CREATE TABLE kernel_envelopes (
    id              TEXT PRIMARY KEY,
    gov_root        TEXT NOT NULL,
    object_type     TEXT NOT NULL,
    object_id       TEXT NOT NULL,
    authority_class TEXT NOT NULL DEFAULT 'candidate',
    lifecycle       TEXT NOT NULL,
    valid_at        TIMESTAMPTZ,
    as_known_at     TIMESTAMPTZ NOT NULL,
    recorded_at     TIMESTAMPTZ NOT NULL,
    payload         JSONB NOT NULL,
    evidence_refs   TEXT[] NOT NULL DEFAULT '{}'
);

-- Spec-sketched access paths: scope/type/authority lookups and the
-- recording-axis rewind (as-known windows).
CREATE INDEX kernel_envelopes_root_type_authority_idx
    ON kernel_envelopes (gov_root, object_type, authority_class);
CREATE INDEX kernel_envelopes_as_known_at_idx
    ON kernel_envelopes (as_known_at);
-- History lookup per object (history_for_object / related_records).
CREATE INDEX kernel_envelopes_object_idx
    ON kernel_envelopes (object_id);

-- One governed operation attempt (any disposition) per idempotency key.
-- `replay_of` links a replayed attempt to the original outcome; `envelope_id`
-- names the governing record written, when one was written.
CREATE TABLE governed_operations (
    op_id           TEXT PRIMARY KEY,
    envelope_id     TEXT REFERENCES kernel_envelopes(id),
    op_kind         TEXT NOT NULL,
    actor           TEXT NOT NULL,
    evidence_digest TEXT,
    applied_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    replay_of       TEXT REFERENCES governed_operations(op_id)
);
