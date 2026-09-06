# Backup / Restore Runbook — `ois-fork` self-hosted stack

Operator procedures for the fork's compose stack (`ois-fork` project: app on
`127.0.0.1:46007`, Postgres 16 + pgvector on `127.0.0.1:46008`). These are the
steps proven in stage 5b (see `DEPLOY-SELFHOSTED.md` for the evidence trail).

They scope every `docker compose` call to the fork's compose project. The
production OIS stack on this host uses project `ois` at `/opt/ois` — none of
these commands name it.

## 0. Layout and assumptions

```
~/ois-fork/                  deployment directory (source + compose + .env)
~/ois-fork/data/             app state on disk: files/, index/, secret.key
volume ois-fork_dbdata       Postgres data (named volume)
~/backups/ois-fork/<TS>/     one directory per backup: dump + tgz + SHA256SUMS
```

A deployment's identity = **the database + the `data/` directory**. Both must
be captured together; restoring only the database leaves `documents` rows
pointing at missing files, and restoring only `data/` leaves the ledger empty.

Commands below run on the host, from `~/ois-fork`, as `ois-deploy`:

```
cd ~/ois-fork
```

## 1. Backup

```bash
TS=$(date -u +%Y%m%dT%H%M%SZ)
B=~/backups/ois-fork/$TS
mkdir -p "$B"

# 1a. Database, custom format (portable, compressed, pg_restore-ready)
docker compose -p ois-fork exec -T db pg_dump -U utopia -d utopia -Fc > "$B/db-utopia-$TS.dump"

# 1b. App data directory (original files, full-text index, secret.key)
tar czf "$B/data-dir-$TS.tgz" -C ~/ois-fork data

# 1c. Integrity manifest
sha256sum "$B/db-utopia-$TS.dump" "$B/data-dir-$TS.tgz" > "$B/SHA256SUMS"

# 1d. Restore manifest — proves the dump parses before you ever need it
docker compose -p ois-fork exec -T db pg_restore --list "$B/db-utopia-$TS.dump" > "$B/restore-manifest.txt"

echo "$TS" > ~/backups/ois-fork/LATEST
```

Check the dump size is nonzero before trusting it. `pg_restore --list` exiting
0 is the cheapest "this backup is real" signal available.

## 2. Restore (into a wiped or fresh stack)

This destroys the current fork stack. The compose project scoping is what
keeps everything else on the host safe.

```bash
TS=$(cat ~/backups/ois-fork/LATEST)
B=~/backups/ois-fork/$TS
sha256sum -c "$B/SHA256SUMS"        # verify the backup first

# 2a. Wipe (fork only)
docker compose -p ois-fork --profile app down -v --remove-orphans
rm -rf ~/ois-fork/data

# 2b. Fresh database volume; init scripts recreate the utopia_app role
docker compose -p ois-fork --profile app up -d db
timeout 60 bash -c 'until docker compose -p ois-fork exec -T db pg_isready -U utopia -q; do sleep 2; done'

# 2c. Restore the database (strict: stop on first error)
docker compose -p ois-fork exec -T db pg_restore --no-owner --exit-on-error \
  -U utopia -d utopia < "$B/db-utopia-$TS.dump"

# 2d. Restore the data directory and bring the app up
tar xzf "$B/data-dir-$TS.tgz" -C ~/ois-fork
docker compose -p ois-fork --profile app up -d
```

## 3. Verify a restore

```bash
# 3a. Health
curl -fsS http://127.0.0.1:46007/health

# 3b. Row counts (compare against your last captured baseline)
docker compose -p ois-fork exec -T db psql -U utopia -d utopia -t -A <<'SQL'
SELECT 'users: '||count(*) FROM users
UNION ALL SELECT 'organizations: '||count(*) FROM organizations
UNION ALL SELECT 'workspaces: '||count(*) FROM workspaces
UNION ALL SELECT 'knowledge_bases: '||count(*) FROM knowledge_bases
UNION ALL SELECT 'documents: '||count(*) FROM documents
UNION ALL SELECT 'chunks: '||count(*) FROM chunks
UNION ALL SELECT 'deployment_settings: '||count(*) FROM deployment_settings
ORDER BY 1;
SQL

# 3c. Login with a pre-wipe account — proves users + JWT secret round-tripped
curl -fsS -X POST http://127.0.0.1:46007/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  -d '{"email":"<admin>","password":"<pw>"}' -o /dev/null -w '%{http_code}\n'

# 3d. Documents readable and files content-addressed on disk
curl -fsS -H "Authorization: Bearer <token>" \
  http://127.0.0.1:46007/api/v1/workspaces/<ws>/kbs | grep -o '"name":"[^"]*"'
find ~/ois-fork/data/files -type f | wc -l
```

A login that returns 200 after a restore is the strongest single check: it
means `deployment_settings` (the auto-generated JWT secret), `users`, and
`memberships` all survived, and the restored `secret.key` still decrypts the
sealed credentials.

## 4. Provisioning `utopia_test` (for the DB-backed suite)

The DB-backed acceptance suite (`cargo test -p utopia-store` with
`UTOPIA_TEST_REQUIRE_DB=1`) needs a database with the schema AND the
`_sqlx_migrations` bookkeeping (some tests run the embedded migrator, which
validates checksums). Never point it at the app database.

```bash
export PGPASSWORD='<UTOPIA_DB_PASSWORD from ~/ois-fork/.env>'

psql -h 127.0.0.1 -p 46008 -U utopia -d postgres -c "CREATE DATABASE utopia_test OWNER utopia;"
for f in $(ls migrations/*.sql | sort); do
  psql -h 127.0.0.1 -p 46008 -U utopia -d utopia_test -q -v ON_ERROR_STOP=1 -f "$f"
done

# Mirror the bookkeeping from the app database (true checksums/descriptions)
psql -h 127.0.0.1 -p 46008 -U utopia -d utopia_test -q -c \
  "CREATE TABLE IF NOT EXISTS _sqlx_migrations (version BIGINT PRIMARY KEY,
   description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
   success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL);"
pg_dump -h 127.0.0.1 -p 46008 -U utopia -d utopia -t _sqlx_migrations --data-only \
  | psql -h 127.0.0.1 -p 46008 -U utopia -d utopia_test -q -f -
```

Then run the suite on the host (direct loopback connection — an SSH tunnel to
the loopback-only port collapses under connection-pool load):

```bash
UTOPIA_DATABASE_URL="postgres://utopia:<pw>@127.0.0.1:46008/utopia_test" \
UTOPIA_TEST_REQUIRE_DB=1 cargo test -p utopia-store
```

## 5. Failure modes seen in practice

| Symptom | Cause | Fix |
|---|---|---|
| `up --build` runs but the app is the *upstream* image | only the base compose file was passed; compose pulled `ghcr.io/deeplethe/utopia:<tag>` | rebuild with `-f docker-compose.yml -f docker-compose.build.yml`, then `down -v` and re-up from scratch |
| `relation "organizations" already exists` during tests | schema exists but `_sqlx_migrations` is empty → migrator re-runs migration 1 | mirror `_sqlx_migrations` (section 4) or restore from a real dump |
| `pg_restore` fails on GRANT/role | fresh volume never ran the init script | `up -d db` first, wait for `pg_isready`, then restore |
| Wiped the wrong project | `-p` not passed / wrong directory | never skip `-p ois-fork`; check `docker volume ls` before `down -v` |
