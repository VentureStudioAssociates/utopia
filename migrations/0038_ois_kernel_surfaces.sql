-- OIS governed kernel surface tables (stage-4a, spec art_cUh17iEH §"The spike
-- that decides it", part 1). Purely additive: no existing table, column, or
-- constraint is altered. These shadow tables give the stage-3 kernel surfaces
-- (decision applicability, objective criteria, governance-event lineage and
-- churn, context-manifest derivations) direct access paths over the append-
-- only `kernel_envelopes` store; resolution logic stays in the pure kernel.
-- Numbering note: the fork's next free number past the spike's 0033 — upstream
-- deeplethe/utopia has since consumed 0033–0037, so the fork-owned sequence
-- continues at 0038 (CONVERGENCE.md renumbering is a stage-gate decision).

-- Decision applicability: one row per (decision envelope, applies_to ref).
-- The surfacing query matches FULL type:id refs exactly (no bare-id coercion).
CREATE TABLE kernel_decision_refs (
    envelope_id    TEXT NOT NULL REFERENCES kernel_envelopes(id),
    gov_root       TEXT NOT NULL,
    decision_ref   TEXT NOT NULL,
    decision_kind  TEXT NOT NULL,
    outcome        TEXT NOT NULL,
    applies_to_ref TEXT NOT NULL,
    recorded_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (envelope_id, applies_to_ref)
);
CREATE INDEX kernel_decision_refs_lookup_idx
    ON kernel_decision_refs (gov_root, applies_to_ref);

-- Objective criteria: one row per (objective envelope version, criterion
-- identity). Supersession between versions is derived by the pure resolver,
-- never stored.
CREATE TABLE kernel_objective_criteria (
    envelope_id       TEXT NOT NULL REFERENCES kernel_envelopes(id),
    gov_root          TEXT NOT NULL,
    criterion_id      TEXT NOT NULL,
    criterion_version INTEGER NOT NULL,
    lifecycle         TEXT NOT NULL,
    recorded_at       TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (envelope_id, criterion_id)
);
CREATE INDEX kernel_objective_criteria_root_idx
    ON kernel_objective_criteria (gov_root, criterion_id);

-- Governance-event surfacing: one row per emitted event envelope — decision
-- lineage, touched units, and churn keys over inclusive recorded windows.
CREATE TABLE kernel_governance_events (
    envelope_id              TEXT PRIMARY KEY REFERENCES kernel_envelopes(id),
    gov_root                 TEXT NOT NULL,
    applied_ref              TEXT NOT NULL,
    applied_resource_type    TEXT NOT NULL,
    churn_key                TEXT,
    originating_decision_ref TEXT,
    recorded_at              TIMESTAMPTZ NOT NULL
);
CREATE INDEX kernel_governance_events_decision_idx
    ON kernel_governance_events (gov_root, originating_decision_ref);
CREATE INDEX kernel_governance_events_applied_idx
    ON kernel_governance_events (gov_root, applied_ref);
CREATE INDEX kernel_governance_events_recorded_idx
    ON kernel_governance_events (gov_root, recorded_at);

-- Context-manifest derivation cache: content-addressed by the kernel's
-- derivation key (principal_ref, permission_digest, governance_root,
-- query_digest). Identical inputs recompile to the identical digest, so
-- caching is a no-op on repeats and can never serve a stale manifest.
CREATE TABLE kernel_manifest_derivations (
    derivation_key  TEXT PRIMARY KEY,
    gov_root        TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    manifest        JSONB NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
