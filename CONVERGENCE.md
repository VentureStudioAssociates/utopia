# CONVERGENCE

This fork carries the **OIS governing-semantics convergence program**: a gated effort
to evaluate and converge [Utopia](https://github.com/deeplethe/utopia) as the substrate
for [OIS](https://github.com/VentureStudioAssociates) — storage, ingestion, retrieval,
entity resolution, search, and MCP transport — under explicit delta budgets, stage
gates, and licensing rules. Upstream continues to move; this document is the working
agreement that keeps this fork honest about the difference between what upstream is,
what was reviewed, and what this program adds.

Operated by Savvy Tinker (Matthew Burris).

## Program provenance

- Governing spec: **OIS × Utopia Convergence Spec — Gated Fork Program** (Obvious artifact `art_cUh17iEH`).
- Phase-1 baseline: **Utopia × OIS — Differential Architecture Review and Migration Matrix, Phase 1** (Obvious artifact `art_ngTAo4uR`), reviewed against `deeplethe/utopia @ 11cc418` ("One search box on the ontology page, in the graph's corner (#371)") — the **review pin**.

## Fork start pin

- Fork: `VentureStudioAssociates/utopia`, forked from `deeplethe/utopia`.
- Start commit: `28db351e2f0d728daa7e22a14ba7a70960db75f7` ("Add guarded full-content hydration for new RSS items (#326)") on `dev`.
- Upstream had moved past the review pin before the fork was cut (28 commits ahead at bootstrap time, 2026-09-05). The Phase-1 matrix is therefore a review record, not a live diff: **the spike re-checks any matrix disposition that looks stale against the current upstream tip before relying on it.**

## Sync cadence

- Upstream remote (already configured in this checkout):
  ```bash
  git remote add upstream https://github.com/deeplethe/utopia.git
  git fetch upstream
  ```
- The fork's working base tracks upstream `dev`. **Rebase onto `upstream/dev` before each program stage gate** so gates are always evaluated against current substrate behavior, never a frozen snapshot.
- **Never rebase across a fixture-parity claim without re-running the oracle suite** (the Phase-1 differential fixtures defined in the migration matrix). A rebase can silently change substrate behavior a parity claim depends on; the oracle suite is the only evidence that survives one.

## Delta budget discipline

- Every PR must **enumerate and bound** its changes to existing upstream files. Unbounded drift into upstream code is a program kill condition.
- New program logic lives in **new modules/directories** (e.g. `crates/ois-kernel/`), never as inline rewrites of upstream core.
- Upstream behavior is preserved; convergence code runs alongside it until a stage gate explicitly ratifies a replacement.

## Licensing

- **Upstream code remains Apache-2.0.** The upstream `LICENSE` file is retained verbatim and is never modified. Upstream ships no separate `NOTICE` file; upstream source headers are retained. Apache-2.0 §4(d) attribution is satisfied by LICENSE retention plus the in-repo provenance above.
- **All OIS-origin semantic code added by this program is AGPL-3.0-only** (governed kernel, resolver and governed-write ports, governance side tables, oracle fixtures). New files carrying OIS-origin code declare `SPDX-License-Identifier: AGPL-3.0-only`; upstream files are never re-headered.
- Apache-2.0 → AGPL-3.0 is one-way compatible. Combined derivative works therefore distribute as AGPL-3.0-only with the Apache portions attributed per §4 — never the reverse, and never OIS-origin code inside an Apache-licensed upstream file.

## Toolchain (bootstrap verification, 2026-09-05)

| Tool | Version | Note |
|---|---|---|
| Rust (stable) | rustc 1.98.1 (48a229cea 2026-09-01) | via rustup, minimal profile — README requires 1.85+ |
| Cargo | 1.98.1 (797e8a9bc 2026-08-05) | same install |
| Node.js | v20.20.2 | README requires 20+ |
| pnpm | 10.2.1 | pinned by `web/package.json` `packageManager`; activated via corepack |

## Build verification (bootstrap, 2026-09-05)

Commands and results from the fork sandbox, against the start commit:

- `cargo build --workspace` — clean compile, exit 0 (1m 19s on the bootstrap sandbox).
- `cargo fmt --all --check` — clean.
- `cargo clippy --workspace --all-targets -- -D warnings` — 0 warnings.
- `cargo test --workspace` — **435 passed, 0 failed, 1 ignored.** Database-backed `utopia-store` integration tests skip in a database-less sandbox, matching the upstream `backend` CI job's documented behavior. Note: **GitHub Actions is disabled by default on forks** (verified at bootstrap: zero workflow runs since fork creation), so the database-backed `migrations` job and the `web` build have not run on this PR; enable Actions on the fork (owner setting) to restore full CI coverage.
- `docker compose --profile app up -d` — **not run: Docker is unavailable in this sandbox.** The documented compose quick start therefore has no automated coverage yet — locally (no Docker) and in CI (Actions disabled on the fork). Its components are covered indirectly: the migration sequence by the `migrations` job (once Actions is enabled), and the UI by `pnpm build`. Bootstrap merge is based on the local verification above, per the plan's fork-CI fallback.
