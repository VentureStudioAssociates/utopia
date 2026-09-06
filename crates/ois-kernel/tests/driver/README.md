# Stage-4b acceptance-corpus tooling

Reproduction tooling for the fork's parity acceptance suite
(`cargo test -p ois-kernel --test parity`).

- `gen_stage4_corpus.py` — regenerates the 41 `fixture`-engine cases
  (R1-T01..T29 + GR-E01..E10/E3B/B00) appended to
  `tests/fixtures/parity-corpus.json` from the pinned oracle JSONs.
  Both corpus copies (`tests/fixtures/` and `fixtures/`) must stay
  byte-identical (MD5-verified after every regeneration).
- `run_ts.mts` — the TypeScript differential driver: runs all 135 corpus
  cases through the pinned oracle implementation and emits the
  `rows` JSON consumed by the harness via `TS_RESULTS_PATH`.

## Oracle pin

All oracle content is read from the second-brain checkout pinned at:

    savvytinker-second-brain @ frontier/ois-v1
    commit c56f8e5cb7a4ad70f237e43a095ff2369de14b2d

The checkout is treated as strictly read-only (`ORACLE_ROOT` env var
overrides the default path). The driver resolves the oracle's own
`node_modules` for its tsx runner; nothing inside the checkout is
written or modified.

## Reproducing the TS-differential run

    git clone https://github.com/VentureStudioAssociates/savvytinker-second-brain oracle-scratch
    git -C oracle-scratch checkout c56f8e5cb7a4ad70f237e43a095ff2369de14b2d
    cd oracle-scratch && pnpm install --frozen-lockfile && cd ..
    ORACLE_ROOT=$PWD/oracle-scratch \
      npx tsx crates/ois-kernel/tests/driver/run_ts.mts \
      crates/ois-kernel/tests/fixtures/parity-corpus.json ts-results.json
    TS_RESULTS_PATH=$PWD/ts-results.json cargo test -p ois-kernel --test parity

Without `TS_RESULTS_PATH` the harness runs frozen-expectation mode.

The single explained divergence (`fixture.gr_e04` disposition) is
documented in `STAGE4-REPORT.md` and allowlisted in `tests/parity.rs`;
any other divergence fails the suite.
