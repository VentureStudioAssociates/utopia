# Stage 4c Report — Agent-Surface Convergence (Governed MCP Reads + Propose-Only Path)

**Branch:** `convergence/agent-surface` (linear on stage-4b head `3e0e652`)
**Oracle pin:** `savvytinker-second-brain @ frontier/ois-v1` `c56f8e5` (READ-ONLY reference)
**Spec:** OIS × Utopia fork program, stage 4 part 3 (`art_cUh17iEH`)

## What was delivered

The eight pre-existing read-only MCP tools stay untouched upstream-facing surface; the
five OIS tools now route exclusively through the governed kernel
(`crates/ois-kernel`), replacing the Phase-1 "propose-only remember path" stub.

### Governed reads (commit `2bd5f4a`)

`ois_resolve_current_state` and `ois_slice_as_known` resolve currentness through
`ois_kernel::resolver` over the kernel side tables via the fork-owned
`PgEnvelopeStore` adapter:

- **Gov-root scoping** — every read is scoped server-side to
  `ois:scope:kb:{kb_id}`; a client-supplied `governance_root_ref` that differs
  is a hard error (`governance_root_mismatch`), never a filter.
- **Temporal axes** — `valid_at` (force bounds) and `as_known_at`
  (historical slices; later recordings invisible) validated RFC 3339 and
  passed into the resolver.
- **Permission-aware fail-closed disclosure** — server-derived permission
  basis (upstream authorize gate -> `DisclosureLimit`); a permission-limited
  principal gets `permission_limited` with an explicit withheld count, never
  suppressed data or a silent empty.
- Raw WQL/query shapes are schema violations (`payload_schema_violation`) —
  fail-closed against query-protocol smuggling.

### Propose-only governed writes (commit `2b6996e`)

The three write tools (`ois_propose_kernel_record`,
`ois_propose_objective_operation`, `ois_attach_evidence`) run the pipeline
behind `tools/call`, mirroring the TypeScript `packages/mcp` propose-only
evaluator semantics:

| Oracle semantic | Fork implementation |
|---|---|
| Payload-derived capabilities (`requiredCapability`) | `required_capability()` — kernel records claiming governing authority/lifecycle map to `establish_governing`; governing transition objective actions map likewise; valid evidence maps to `attach_evidence`. Caller claims of approval/auth stay provenance. |
| Server-side evaluation | `evaluate_capability` over the server-derived profile; the wired agent grant structurally holds only `propose`/`attach_evidence`, never `establish_governing`/`approve`. |
| Rejected envelopes (`OISTypedOperationV1`) | Denials return a typed rejected envelope (`authority_state: denied`, `reason: authority_denied`) — distinct outcomes, never silent no-ops. Nothing is recorded; replaying re-evaluates deterministically (a later grant change is honored). |
| Candidate-only application (`applyCandidate`) | Applied candidates are always `candidate`/`working` knowledge — structural check in `evaluate_write` refuses governing lifecycle. `recorded_at` is the server clock; the server-derived activity ref rides with the client's claimed provenance. |
| Idempotent replay (`replayEnvelope`) | Applied operations record their outcome in the kernel `governed_operations` table (append-only, `op_id` primary key, `ON CONFLICT DO NOTHING`); replay returns the recorded outcome — never a second record. Rejections are not recorded (they never reached the kernel). |
| Cross-root isolation | Payloads targeting another root are rejected before evaluation; store writes are root-scoped. |
| Objective operations | `propose_create`/`propose_revision` land as candidate knowledge ABOUT the objective; the seven governing transition actions map to `establish_governing` and can never apply. |
| Evidence | Lands as a content-addressed candidate record referencing its target; it never mutates the target. Its contract has no authority fields — smuggling them is a schema violation. |

### Fixture evidence — 15 governed fixtures, all green

Read-path (commit `2bd5f4a`):

| Fixture | Asserts |
|---|---|
| `read_resolves_governing_under_server_derived_basis` | governing resolution, complete, no diagnostics |
| `slice_as_known_hides_later_recordings` | historical slice -> `not_established` / no visible record |
| `permission_limited_basis_reports_withheld_count` | `permission_limited` + withheld count 2 |
| `valid_at_axis_bounds_force` | valid_at force semantics |
| `read_rejects_cross_root_queries` | cross-root read -> `governance_root_mismatch` |
| `read_rejects_raw_query_shapes` | raw WQL -> `payload_schema_violation` |
| `wired_agent_grant_is_structurally_propose_only` | no `establish_governing`/`approve` on the agent grant |

Write-path (commit `2b6996e`):

| Fixture | Asserts |
|---|---|
| `governing_claim_is_rejected_fail_closed` | governing claim -> rejected envelope, `authority_denied`, no resources |
| `candidate_proposal_applies_with_server_derived_provenance` | candidate applies; server clock `recorded_at`; activity ref in provenance |
| `objective_operation_is_candidate_never_governing` | `propose_create` -> candidate objective record |
| `cross_root_payload_is_rejected_before_evaluation` | cross-root write -> `governance_root_mismatch` |
| `structurally_no_mcp_write_reaches_governing` | all three write tools: no governing shape applies (evidence smuggling -> schema violation or denial) |
| `replay_returns_the_recorded_outcome` | replay envelope: `replay_of` set, recorded result refs |
| `unauthenticated_principal_is_denied_at_the_kernel` | kernel gate denies `authenticated: false` (`unauthenticated`) |
| `no_grant_profile_is_denied_fail_closed` | grantless agent -> `no_matching_grant` |

These coverage claims from the brief map directly: **unauthenticated**
(`unauthenticated_principal_is_denied_at_the_kernel`), **capability-gap**
(`governing_claim_is_rejected_fail_closed` + `structurally_no_mcp_write_reaches_governing`),
**no-grant** (`no_grant_profile_is_denied_fail_closed`),
**permission-limited withheld count** (`permission_limited_basis_reports_withheld_count`),
and **a proposal never mutates governing state** (structural candidate/working
check + `structurally_no_mcp_write_reaches_governing` + the wired-grant
propose-only invariant).

## Verification (fork-CI fallback)

GitHub Actions is disabled on the fork; local verification is the CI of
record, as ratified in earlier stages. All run at head `2b6996e`:

| Gate | Result |
|---|---|
| `cargo build --workspace` | clean (44.8s) |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` | green — utopia-server 200 passed / 0 failed / 1 ignored; zero failed results across all crates (includes the ois-kernel parity harness) |

Clippy conformance in the pre-existing fork-owned parity fixture harness
(`crates/ois-kernel/tests/fixture_scenarios.rs`, +8/−35) is mechanical
(unused helper removal, borrow/deref lints, `Option::as_slice()`); no parity
semantics changed and the suite stays green.

## Delta budget

Branch base: `3e0e652` (stage-4b merged head) — `git diff --numstat
3e0e652..HEAD`:

| File | +/− | Class | Role |
|---|---|---|---|
| `crates/utopia-server/src/api/mcp_ois.rs` | +1760 | fork-owned (new module) | the entire governed surface: registry, wiring, capability derivation, envelopes, validation, pipeline, fixtures |
| `crates/utopia-store/src/ois_kernel.rs` | +117 | fork-owned (created stage 4a) | `recorded_operation` / `record_mcp_operation` replay persistence |
| `crates/ois-kernel/tests/fixture_scenarios.rs` | +8/−35 | fork-owned (created stage 3) | clippy-only conformance |
| `crates/utopia-server/src/api/mcp.rs` | +26/−9 | **upstream** | integration: `pub(crate)` JSON-RPC helpers, governed dispatcher routing, `tools/list` OIS append |
| `crates/utopia-server/src/api/mod.rs` | +1 | **upstream** | module registration |
| `crates/utopia-server/Cargo.toml` | +1 | **upstream** | `ois-kernel` dependency edge |
| `Cargo.lock` | +1 | **upstream (derived)** | lockfile regeneration from the new dependency edge |

**Four upstream files touched, every change a bounded, mechanical
integration line; all new logic lives in fork-owned modules.** No pervasive
core edits — the kill condition is not triggered. The upstream `mcp.rs`
dispatcher routes `ois_*` tool calls to the governed module before any
upstream exposure check; the eight upstream read-only tools themselves are
byte-identical.

## Design decisions recorded

1. **Rejections are not recorded for replay.** A denial never reached the
   kernel; replaying re-evaluates deterministically, so a later capability
   change is honored rather than frozen into a stale recorded outcome.
   Applied operations (the only kernel-reaching outcomes) record once.
2. **No new migration.** The stage-4a `governed_operations` table already
   carries the replay role (`op_id` PK, `envelope_id` FK, `applied_at`);
   the MCP surface records into it rather than inventing a parallel store.
3. **Server-derived timestamps and provenance** (oracle rule): the client's
   `as_known_at` is a knowledge claim; `recorded_at` is the server clock;
   the server-derived activity ref is appended to (not substituted for)
   the client's claimed provenance refs.
4. **`mcp.rs` helpers made `pub(crate)`** rather than duplicated: one
   JSON-RPC wire format for both surfaces.

## Non-negotiables re-verified

- Propose-only: no MCP invocation reaches governing state (structural check
  + fixtures + grant invariant).
- Fail-closed: cross-root, unauthenticated, grantless, capability-gap, and
  schema-violating inputs all produce explicit errors or rejected envelopes.
- AGPL-3.0-only headers on all OIS-origin fork files.
- Delta budget: bounded integration edits only; additive fork-owned modules.
