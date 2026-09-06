# Stage 5b — Self-Hosted Postgres Deployment Proof (srv1918204)

Topology B of the OIS × Utopia convergence spec: the fork's full compose stack
(Postgres + app) runs on the same self-hosted VPS as the production Gentle
Roost OIS stack, in hard isolation, and the spec's backup → wipe → restore
gate is proven with captured evidence.

Executed 2026-09-06. All commands ran over SSH as `ois-deploy` (uid/gid 1320)
using the rootless Docker context `ois-rootless` (`unix:///run/user/1320/docker.sock`),
Docker 29.7.2, Docker Compose v5.5.0. No sudo, no docker group — rootless
Docker only.

## Production isolation (hard constraint)

The host already runs the production stack — compose project `ois` at
`/opt/ois`, three containers:

```
ois-ois-1          127.0.0.1:46006->8080/tcp   Up 2 days (healthy)
ois-terminusdb-1   127.0.0.1:46363->6363/tcp   Up 2 days
ois-backup-1       8080/tcp                    Up 2 days (unhealthy)
```

The fork stack never touches it:

| | Production | Fork (this proof) |
|---|---|---|
| Compose project | `ois` | `ois-fork` |
| Directory | `/opt/ois` | `/home/ois-deploy/ois-fork` |
| App port | `127.0.0.1:46006` | `127.0.0.1:46007` |
| Datastore port | `127.0.0.1:46363` (TerminusDB) | `127.0.0.1:46008` (Postgres 16 + pgvector) |
| Data | production volumes | `ois-fork_dbdata` volume + `./data` bind mount |

`docker compose -p ois-fork` scopes every lifecycle command to the fork's
containers, volume `ois-fork_dbdata`, and network `ois-fork_default`. The
production containers were verified `Up 2 days` immediately before and after
the wipe step of the gate — the destructive command never named them.

## What was deployed

Image built on the VPS from the fork source (branch `convergence/deploy-selfhosted`,
commit `f6a6fbc`) with the build overlay:

```
docker compose -f docker-compose.yml -f docker-compose.build.yml \
  -p ois-fork --profile app up -d --build
```

Result: both containers healthy, image id `34f9ced33b9e` (built on-host),
loopback-only bindings confirmed by `ss -tln`:

```
LISTEN  127.0.0.1:46007   (app → container 1516)
LISTEN  127.0.0.1:46008   (db → container 5432)
```

`/health` → 200; the SPA is served at `/`; the API is mounted at `/api/v1`.

### Environment (`.env`, mode 0600)

The restricted-role setup recommended in `.env.example` was enabled end-to-end:
the init script (`docker/init-app-role.sh`) created `utopia_app` on first boot,
migrations granted it least privilege (0010/0031), and the app connects as
`utopia_app` while migrations use the owner connection (`UTOPIA_MIGRATION_URL`).

- `UTOPIA_DB_BIND=127.0.0.1:46008` — loopback only, distinct port
- `UTOPIA_APP_BIND=127.0.0.1:46007` — loopback only, distinct port (parameterized in this branch)
- `UTOPIA_DB_PASSWORD`, `UTOPIA_APP_DB_PASSWORD` — random (`openssl rand -hex 16`)
- `UTOPIA_DATABASE_URL` — `utopia_app` restricted role
- `UTOPIA_MIGRATION_URL` — owner role, used only for migration runs

### Migration state

All 34 migrations applied on the app database at first startup (the binary
runs the embedded `sqlx::migrate!` set), including the fork-only kernel
migrations `0033_governed_kernel` and `0038_ois_kernel_surfaces`.

## Deployment fixes (this branch)

1. **App port was hardcoded public.** `docker-compose.yml` bound `"1516:1516"`
   (0.0.0.0) the moment the `app` profile came up. It is now
   `"${UTOPIA_APP_BIND:-127.0.0.1:1516}:1516"` — loopback by default, matching
   the `db` service's existing `UTOPIA_DB_BIND` convention. `.env.example`
   documents it.
2. No other code change was needed for a fresh rootless-Docker host; the
   remaining deployment requirements are documented here and in the runbook.

## DB-backed acceptance on self-hosted Postgres

Run on the VPS itself (8 cores / 32 GB), connecting directly to the fork's
Postgres over loopback — no tunnels:

```
export UTOPIA_DATABASE_URL=postgres://utopia:<pw>@127.0.0.1:46008/utopia_test
export UTOPIA_TEST_REQUIRE_DB=1
cargo test -p utopia-store
```

`utopia_test` is a dedicated database in the same Postgres instance (never the
app's live database), provisioned with the schema: all 34 migration files
applied in filename order via psql (the CI migrations job does the equivalent
with `sqlx-cli`), then `_sqlx_migrations` mirrored from the app database so
checksum-aware migrators see the true bookkeeping.

Result: **105 passed, 0 failed** across 49 test binaries.

Governed fixture suite (in-process; the stage-4b acceptance corpus):

```
cargo test -p ois-kernel
```

Result: **25 passed, 0 failed** (kernel unit tests + parity harness).

Method note: the suite was also attempted from a separate machine through an
SSH tunnel; under connection-pool load the tunnel dropped with
`Connection reset by peer` before the suite could finish. Running the suite
on the host (the realistic operator topology for loopback-only Postgres)
is both the stronger evidence and the only reliable transport.

## Spec gate: backup → wipe → restore → verify

All state that constitutes a deployment's identity is the Postgres database
plus the `data/` directory (original files, full-text index, `secret.key`).
That is exactly what was backed up, wiped, and restored.

### 1. Backup (`20260906T164545Z`)

```
docker compose -p ois-fork exec -T db pg_dump -U utopia -d utopia -Fc > db-utopia-$TS.dump
tar czf data-dir-$TS.tgz -C ~/ois-fork data
sha256sum db-utopia-$TS.dump data-dir-$TS.tgz > SHA256SUMS
```

| Artifact | Size | sha256 |
|---|---|---|
| `db-utopia-20260906T164545Z.dump` | 579 142 B | `69e9e09b214db499cd18ce7677e90f4ff637ca882285ea4550ab14a5bef1fe7c` |
| `data-dir-20260906T164545Z.tgz` | 211 698 B | `27fb6f75f08f526fa414cb3576f9740a692f789aad226c97186f6d2d08c24f8b` |

Stored under `/home/ois-deploy/backups/ois-fork/20260906T164545Z/` — outside
the deployment directory, outside production's `/opt/ois/backups`.

Pre-wipe state was seeded through the real API (register admin → login →
upload two documents), so the gate defends rows that were not planted by SQL:
1 user, 1 organization, 1 workspace, 1 knowledge base, 2 documents (chunked,
indexed, content-addressed on disk), 1 `deployment_settings` row (the
auto-generated JWT secret).

### 2. Wipe

```
docker compose -p ois-fork --profile app down -v --remove-orphans
rm -rf ~/ois-fork/data
```

Verified after: no `ois-fork` containers, no `ois-fork_dbdata` volume, no
data directory. Production containers: still `Up 2 days`, untouched.

### 3. Restore

```
docker compose -p ois-fork --profile app up -d db          # fresh volume, init scripts run
pg_restore --no-owner --exit-on-error -U utopia -d utopia < db-utopia-$TS.dump
tar xzf data-dir-$TS.tgz -C ~/ois-fork
docker compose -p ois-fork --profile app up -d
```

`pg_restore` completed with `--exit-on-error` and no errors.

### 4. Verify

- **Row counts**: all 12 sampled tables identical to pre-wipe
  (users 1, organizations 1, workspaces 1, knowledge_bases 1, documents 2,
  document_versions 0, chunks 2, entities 0, facts 0, kernel_envelopes 0,
  kernel_governance_events 0, deployment_settings 1).
- **App**: `/health` → 200; login with the pre-wipe credentials → 200 —
  the JWT secret in `deployment_settings` and the user rows round-tripped.
- **Documents**: list returns both documents `status: ready` (chunked +
  indexed); the on-disk content-addressed files match the documents' sha256s
  (`a0280773…`, `3bb693eb…`) and the seeded content is byte-identical.
- **Fixtures re-run after restore**: both suites re-ran on the restored
  Postgres with identical results to the pre-wipe runs — governed fixtures
  **25 passed / 0 failed**, DB-backed store suite **105 passed / 0 failed**
  (logs: `/home/ois-deploy/evidence/governed-fixtures-post-restore.log`,
  `db-backed-suite-post-restore.log`).

## Gotchas worth keeping

- **Building from source requires both compose files.** The base
  `docker-compose.yml` alone pulls `ghcr.io/deeplethe/utopia:<tag>` (upstream
  image). The first `up` in this proof did exactly that; the stack was wiped
  (`down -v`) and rebuilt with `-f docker-compose.yml -f docker-compose.build.yml`.
  The runbook bakes the full invocation in.
- **`_sqlx_migrations` is part of the contract.** A database with the schema
  but empty migration bookkeeping makes checksum-aware migrators re-run
  migration 1 and fail on `relation "organizations" already exists`. Restore
  from a proper dump carries the table; provisioning a schema by hand must
  mirror the bookkeeping (the runbook shows how).
- **`down -v` is scoped by `-p`.** Always pass `-p ois-fork` (or run from the
  fork directory with a distinct directory name); volume/network names are
  project-prefixed, which is what keeps production untouchable.
