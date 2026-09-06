# SPIKE-REPORT — OIS governed kernel as additive Utopia side tables

**Program:** OIS × Utopia convergence, Phase-2 substrate spike (spec `art_cUh17iEH` §“The spike that decides it”; Phase-1 matrix `art_ngTAo4uR` §3).
**Branch:** `spike/governed-kernel` (open PR to `dev`, held for stage-3 ratification review — do not merge).
**Oracle pin:** `VentureStudioAssociates/savvytinker-second-brain` @ `frontier/ois-v1` `c56f8e5` (read-only scratch checkout).
**Verdict: NO kill condition triggered. Parity holds — 74/74 frozen cases pass in the Rust port, with 0 cross-engine divergences against the TypeScript oracle.**

---

## 1. Method

The TypeScript testkit freezes expected outcomes but not complete envelope corpora, so parity inputs were
**derived, documented, and frozen** per case: `gen_corpus.py` (scratch driver) emits `parity-corpus.json`
with 74 cases — each carrying its oracle derivation (test file @ pin) and the R1 / Gentle Roost fixture
references it exercises. The same corpus drives both engines:

- **TypeScript oracle:** `parity-driver.ts` (scratch) invokes the pinned TS resolver / governed-write /
  promotion APIs on identical inputs and normalizes outputs to a fixed JSON shape → `ts-results.json`.
  All 74 pass against frozen expectations (the TS implementation already passes the oracle suite at `c56f8e5`).
- **Rust port:** `crates/ois-kernel` (pure: resolver, governed writes, authority, promotion; no I/O) +
  `tests/parity.rs` (integration harness, `include_str!` corpus). Runs the identical inputs through the
  Rust engines, checks the frozen expectations, and — when `TS_RESULTS_PATH` is set — compares every
  normalized result 1:1 with the TS oracle's results.

Reproduction (repo root):

```
cargo test --workspace                                        # no-TS mode: frozen expectations only
TS_RESULTS_PATH=<scratch>/driver/ts-results.json \
  cargo test -p ois-kernel --test parity -- --nocapture       # full differential vs oracle
```

## 2. Differential result

```
rust-oracle: 74 pass, 0 fail of 74          (frozen expectations, both modes)
rust-vs-ts differential: 0 cross-engine divergences
```

Full workspace: `cargo test --workspace` → **438 passed, 0 failed**. `cargo fmt --all --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean.

### 2a. Per-fixture table

**Resolver (deterministic currentness)** (20 cases)

| Case | Oracle source | Expected (frozen) | Rust result | Parity |
|---|---|---|---|---|
| `resolver.r01_empty_history` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="not_established"; basis="no_visible_record"; completeness="complete"` | match | yes |
| `resolver.r02_candidate_only` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="not_established"; basis="candidate_or_working_only"; diagnostic_kinds=["candidate_not_governing"]` | match | yes |
| `resolver.r03_newest_governing` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="governing"; envelope_recorded_at="2026-08-05T00:00:00.000Z"; authority="current_operating_state"` | match | yes |
| `resolver.r04_order_invariant` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="governing"; shuffled_equal=true` | match | yes |
| `resolver.r05_supersession_related` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="superseded"; successor_ref="asrt-line-speed-v2"` | match | yes |
| `resolver.r06_supersession_in_history` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="superseded"` | match | yes |
| `resolver.r07_disputed_distinct` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="governing_but_disputed"; authority="current_operating_state"` | match | yes |
| `resolver.r08_terminal_distinct` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="retired"; state_b="deprecated"` | match | yes |
| `resolver.r09_future_valid_excluded` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="not_established"; basis="no_visible_record"; diagnostic_kinds=["future_valid_excluded"]` | match | yes |
| `resolver.r10_valid_at_null_unbounded` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="governing"` | match | yes |
| `resolver.r11_out_of_scope_excluded` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="not_established"; diagnostic_kinds=["out_of_scope_excluded"]` | match | yes |
| `resolver.r12_foreign_root_excluded` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="not_established"; basis="no_visible_record"` | match | yes |
| `resolver.r13_permission_count_only` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="permission_limited"; completeness="partial_permissions"; diagnostic_kinds=["permission_withheld"]; withheld_count=1` | match | yes |
| `resolver.r14_existence_not_disclosable` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="permission_limited"; withheld_count=null` | match | yes |
| `resolver.t01_prior_as_known_reproduces` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="governing"; envelope_recorded_at="2026-08-01T00:00:00.000Z"` | match | yes |
| `resolver.t02_truncated_history_equivalent` | `packages/core/test/resolver.test.ts @ c56f8e5` | `equal_to_b=true` | match | yes |
| `resolver.t03_backdated_late_recording` | `packages/core/test/resolver.test.ts @ c56f8e5` | `state="governing"; envelope_recorded_at="2026-08-01T00:00:00.000Z"; state_b="superseded"` | match | yes |
| `resolver.t05_repeat_determinism` | `packages/core/test/resolver.test.ts @ c56f8e5` | `repeat_equal=true` | match | yes |
| `scenario.t01_ten_lineages` | `fixtures/adversarial/r1-contract-closure/oracles.json R1-T01` | `all_states="governing"; count=10` | match | yes |
| `scenario.gr_e01_candidate_not_governing` | `Gentle Roost flagship GR-E01 + non-governance must-have; helper mirrors work candidateEnvelope` | `state="not_established"; basis="candidate_or_working_only"; diagnostic_kinds=["candidate_not_governing"]` | match | yes |

**Capability evaluation** (8 cases)

| Case | Oracle source | Expected (frozen) | Rust result | Parity |
|---|---|---|---|---|
| `capability.c01_unauthenticated` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=false; reason="unauthenticated"` | match | yes |
| `capability.c02_agent_capability_gap` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=false; reason="capability_not_granted"` | match | yes |
| `capability.c03_no_matching_grant` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=false; reason="no_matching_grant"` | match | yes |
| `capability.c04_claimed_approval_insufficient` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=false; reason="capability_not_granted"` | match | yes |
| `capability.c05_profile_root_mismatch` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=false; reason="profile_root_mismatch"` | match | yes |
| `capability.c06a_delegation_in_scope` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=true` | match | yes |
| `capability.c06b_delegation_out_of_scope` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=false` | match | yes |
| `capability.c06c_delegation_root_wide` | `packages/core/test/governance.test.ts @ c56f8e5` | `authorized=true` | match | yes |

**Governed writes** (17 cases)

| Case | Oracle source | Expected (frozen) | Rust result | Parity |
|---|---|---|---|---|
| `write.w01_solo_establishment` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; event_object_type="governance_event"; event_lifecycle="governing"; event_kind="governing_transition"; event_decision_ref=null; churn_key_nonempty=true` | match | yes |
| `write.w02_event_cited_in_provenance` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; record_cites_event=true` | match | yes |
| `write.w03a_decision_churn_key` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; churn_key="decision:dec-1"; event_decision_ref="decision:dec-1"` | match | yes |
| `write.w03b_no_decision_churn_key` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; churn_key_equals_event_ref=true; event_decision_ref=null` | match | yes |
| `write.w04_event_identity_deterministic` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; event_equal=true; churn_key_equal=true` | match | yes |
| `write.w05_backdated_knowledge_establishment` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; change_class="knowledge_establishment"` | match | yes |
| `write.w07_stale_base` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="stale_requires_review"; resolved_base_recorded_at="2026-08-02T00:00:00.000Z"` | match | yes |
| `write.w08_establishment_over_existing` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="stale_requires_review"` | match | yes |
| `write.w09_matching_base_ok` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition_not="stale_requires_review"` | match | yes |
| `write.w11_disputed_basis_default` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="pending_approval"; reason_code="disputed_basis"` | match | yes |
| `write.w12_disputed_explicit_rule` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"` | match | yes |
| `write.w13_permission_limited_pending` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="pending_approval"; reason_code="permission_limited_evidence"` | match | yes |
| `write.w14_no_capability_rejected` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="rejected"; reason_code="insufficient_authority"` | match | yes |
| `write.w15_replay` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="replayed"; original{idempotency_key="idem-1", disposition="applied", reason_code=null, record_ref="assertion:asrt-line-speed", governance_event_ref="governance_event:016f0000-0000-7000-8000-000000000001"}; no_event=true; no_churn_key=true` | match | yes |
| `write.w16_foreign_next_error` | `packages/core/test/governance.test.ts @ c56f8e5` | `error=true` | match | yes |
| `scenario.t27_recording_lag` | `fixtures/adversarial/r1-contract-closure/r3-fixtures.json R1-T27` | `disposition="applied"; change_class="governing_state_change"; recording_lag_seconds=97200` | match | yes |
| `scenario.t29_decision_churn` | `fixtures/adversarial/r1-contract-closure/r3-fixtures.json R1-T29` | `disposition="applied"; churn_key="decision:d29"; churn_key_equal=true` | match | yes |

**Change classification** (4 cases)

| Case | Oracle source | Expected (frozen) | Rust result | Parity |
|---|---|---|---|---|
| `classify.assertion_on_time` | `packages/core/test/governance.test.ts @ c56f8e5` | `change_class="governing_state_change"` | match | yes |
| `classify.governance_rule` | `packages/core/test/governance.test.ts @ c56f8e5` | `change_class="governance_rule_change"` | match | yes |
| `classify.objective_criterion` | `packages/core/test/governance.test.ts @ c56f8e5` | `change_class="objective_criterion_change"` | match | yes |
| `classify.backdated` | `packages/core/test/governance.test.ts @ c56f8e5` | `change_class="knowledge_establishment"` | match | yes |

**Stale-base detection** (4 cases)

| Case | Oracle source | Expected (frozen) | Rust result | Parity |
|---|---|---|---|---|
| `stale_base.exact_match` | `packages/core/test/governance.test.ts @ c56f8e5` | `stale=false` | match | yes |
| `stale_base.moved_state` | `packages/core/test/governance.test.ts @ c56f8e5` | `stale=true; resolved_base_recorded_at="2026-08-01T00:00:00.000Z"` | match | yes |
| `stale_base.none_no_record` | `packages/core/test/governance.test.ts @ c56f8e5` | `stale=false` | match | yes |
| `stale_base.base_without_record` | `packages/core/test/governance.test.ts @ c56f8e5` | `stale=true; resolved_base_recorded_at=null` | match | yes |

**Cross-scope acceptance** (5 cases)

| Case | Oracle source | Expected (frozen) | Rust result | Parity |
|---|---|---|---|---|
| `accept.cr01_local_event` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; event_kind="cross_scope_acceptance"; event_origin_root="governance_root:acme-ops"; churn_key_nonempty=true; record_root="governance_root:gentle-roost"; record_cites_event=true; record_provenance_contains=true` | match | yes |
| `accept.cr02_refresh` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="applied"; transition_class="evidence_refresh"; no_event=true; no_churn_key=true; no_record=true` | match | yes |
| `accept.cr03_agent_rejected` | `packages/core/test/governance.test.ts @ c56f8e5` | `disposition="rejected"; reason_code="insufficient_authority"` | match | yes |
| `accept.cr04_snapshot_root_mismatch` | `packages/core/test/governance.test.ts @ c56f8e5` | `error=true` | match | yes |
| `accept.cr05_same_root_error` | `packages/core/test/governance.test.ts @ c56f8e5` | `error=true` | match | yes |

**Evidence-aware promotion** (16 cases)

| Case | Oracle source | Expected (frozen) | Rust result | Parity |
|---|---|---|---|---|
| `promo.p01_zero_bindings_error` | `packages/work/test/promotion.test.ts @ c56f8e5` | `error=true` | match | yes |
| `promo.p02_itemwise_partial` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "applied", "reason_code": "evidence_sufficient", "evidence_state": "sufficient_current", "authority_state": "authorized", "delta_change_class": "governing_state_change", "has_churn_key": true}, {"promotion_disposition": "pending_approval", "reason_code": "disputed_evidence", "evidence_state": "conflicting"}]` | match | yes |
| `promo.p03_wrong_endpoint` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "rejected", "reason_code": "endpoint_binding_mismatch"}]` | match | yes |
| `promo.p04_binding_mismatch` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "rejected", "reason_code": "evidence_binding_mismatch", "evidence_state": "conflicting"}]` | match | yes |
| `promo.p05_stale_evidence` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "stale_requires_review", "reason_code": "stale_evidence", "evidence_state": "sufficient_but_stale"}]` | match | yes |
| `promo.p06_permission_limited` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "pending_approval", "reason_code": "permission_limited_evidence", "evidence_state": "permission_limited"}]` | match | yes |
| `promo.p07_evaluator_error` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "rejected", "reason_code": "evaluator_error", "evidence_state": "error"}]` | match | yes |
| `promo.p08_analyzer_wrong_trait` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "rejected", "reason_code": "oracle_not_allowed_for_trait"}]` | match | yes |
| `promo.p09_analyzer_applies` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "applied", "confirmation_mode": "deterministic_establishment"}]` | match | yes |
| `promo.p10_analyzer_stale` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "stale_requires_review", "reason_code": "stale_evidence", "evidence_state": "sufficient_but_stale"}]` | match | yes |
| `promo.p11_agent_pending` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "pending_approval", "reason_code": "insufficient_authority", "authority_state": "pending_required_authority", "evidence_state": "sufficient_current"}]` | match | yes |
| `promo.p12_ghost_denied` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "rejected", "reason_code": "authority_denied", "authority_state": "denied"}]` | match | yes |
| `promo.p13_over_resolved` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "stale_requires_review", "reason_code": "stale_evidence"}]` | match | yes |
| `promo.p14a_impact_one_sided` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "rejected", "reason_code": "insufficient_evidence"}]` | match | yes |
| `promo.p14b_impact_decision` | `packages/work/test/promotion.test.ts @ c56f8e5` | `items=[{"promotion_disposition": "applied"}]` | match | yes |
| `scenario.gr_e3b_promotion` | `Gentle Roost flagship GR-E3B` | `items=[{"promotion_disposition": "applied", "reason_code": "evidence_sufficient", "evidence_state": "sufficient_current", "authority_state": "authorized", "delta_change_class": "governing_state_change", "has_churn_key": true}]` | match | yes |

### 2b. Non-governance fixture (the Phase-1 pivotal finding)

`scenario.gr_e01_candidate_not_governing` (and unit-level twin `resolver.r02_candidate_only`): a
bulk-extracted fact flows through Utopia's ordinary path, lands as a CANDIDATE envelope
(`candidate_knowledge` authority × `candidate` lifecycle, coupled bidirectionally and frozen), and the
resolver reports `state=not_established`, `basis=candidate_or_working_only`,
`diagnostic_kinds=[candidate_not_governing]`. It cannot change resolved governing state without a
governed operation. The hook (`crates/utopia-store/src/graph.rs`, failure-isolated) is the only
ordinary-path touch: no auto-promotion, no inferred authority. **Result: PASS (yes).**

## 3. Divergence list

Two kernel-level semantic divergences were found during the port and **fixed**; the remainder were
harness projection normalizations. After fixes: **0 unexplained divergences** (a kill condition if any
remained).

| # | Where | Divergence found | Resolution |
|---|---|---|---|
| D1 | `is_stale_base` (governed writes) | Rust returned `resolved_base_recorded_at` even when the base was *not* stale; the TS oracle emits no field on the non-stale path. | Fixed in the Rust port: the non-stale path returns `None` (no field), mirroring TS. |
| D2 | Promotion, structurally invalid evidence | Zero evidence bindings: Rust initially returned a `rejected` disposition; the TS oracle raises `WorkRecordError` for this structural case (R1-T09). | Fixed: the Rust promotion path returns a typed `GovernedWriteError` (fail-closed, identical observable behavior). |
| D3 | Harness projections | TS `JSON.stringify` drops `undefined` keys and derives some projection fields differently (e.g. `withheld_count` is not top-level on `permission_limited`; it is read from the `permission_withheld` diagnostic; `envelope_recorded_at` is emitted as `null` when absent; replay `original.reason_code` normalized to `null`). | Fixed in the Rust harness projections to mirror the TS driver's exact output shape. No kernel semantic change. |
| D4 | Harness input shape | Capability corpus key is `actor` (not `principal`); ten-lineage scenario must resolve each lineage as its own one-object history (the oracle rejects mixed object IDs in a single history). | Fixed in the harness/driver; recorded in the corpus. |

## 4. Delta budget (upstream files touched)

Every pre-existing Utopia file touched — **5 files, 42 insertions, 0 deletions**, all additive.
No existing facts, ontology, or query table is altered; no existing behavior changed.

| Upstream file | Impact | Purpose |
|---|---|---|
| `Cargo.toml` | +2 lines | workspace member + `[workspace.dependencies]` entry for `ois-kernel` |
| `Cargo.lock` | +10 lines | generated lockfile entries for the new crate |
| `crates/utopia-store/Cargo.toml` | +1 line | `ois-kernel.workspace = true` dependency |
| `crates/utopia-store/src/lib.rs` | +1 line | `pub mod ois_kernel;` registration |
| `crates/utopia-store/src/graph.rs` | +28 lines | failure-isolated candidate-envelope hook at the end of `insert_fact_inner` (log-and-continue; the ordinary path can never be gated by the kernel side-record) |

New files (not upstream edits): `crates/ois-kernel/` (8 sources + tests + fixtures),
`crates/utopia-store/src/ois_kernel.rs` (sqlx adapter, async inherent methods over the kernel's sync
store seam), `migrations/0033_governed_kernel.sql` (additive `kernel_envelopes` + `governed_operations`
side tables with the spec-sketched indexes; nothing existing is altered).

**Kill-condition check (pervasive core edits): NOT triggered.** All kernel logic lives in a new crate;
storage is side tables; the only ordinary-path touch is the isolated hook above. AGPL-3.0-only applies
to all new OIS-origin code; no upstream file content was modified (append-only lines), so Apache
headers/NOTICE in upstream files are untouched.

## 5. Revised risk notes

- **Parity risk (was: the decisive question).** The port achieves 1:1 behavioral parity on the frozen
  contract surface (resolver, capability, governed writes, classification, stale-base, cross-root
  acceptance, promotion incl. itemwise dispositions, temporal/adversarial cases, Gentle Roost flagship).
  The dual-runtime parity harness (same corpus → both engines → normalized diff) is itself the
  regression gate going forward; it runs in CI without a database.
- **Dual-runtime window (spec open question 3).** Recommendation: keep the OIS surface on the
  TypeScript/TerminusDB runtime through stage 3; the Utopia kernel stays **candidate-only** until
  ratification. The parity evidence de-risks the eventual cutover, but the TS runtime remains the
  authority until the Utopia side proves deployment + live-DB behavior. Concretely: projections and the
  proof surface stay TS; Utopia ingests CANDIDATE envelopes beside `facts` (shadow, failure-isolated).
- **Live-database gap.** The sqlx adapter compiles and is wired, but CI does not yet run the side tables
  against a live Postgres (the crate builds with runtime queries, no DB). Stage-3 work: an
  integration test that exercises `record_envelope`/history queries and the hook against Postgres.
- **Corpus provenance.** Frozen fixture JSONs contain scenario references + expected outcomes, not
  complete corpora; the parity corpus therefore records per-case oracle derivation explicitly
  (`derivation` + `fixture_refs` on every case). The oracle TS suite itself passes at the pin
  (`c56f8e5`), and the differential compares both engines on identical inputs.
- **Idempotency/replay.** Replay detection is behavioral in the kernel (prior-outcome lookup by
  idempotency key); the side-table shape supports it (`governed_operations.op_id` PK, `replay_of`).

## 6. Conclusion

The gated-fork path survives its decisive experiment: OIS governing semantics port cleanly beside
Utopia's fact model as additive side tables with full fixture parity and a 5-file/42-line additive
upstream delta. Proceed to stage-3 ratification with the PR held open (unmerged) for review.
