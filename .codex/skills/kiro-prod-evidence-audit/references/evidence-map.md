# Production Evidence Map

Read this before running production SSH commands. All commands must be adapted to the user's host, SSH user, compose directory, container names, and requested time window.

## Command Discipline

- Record every command in `commands.md` before or immediately after running it.
- Wrap remote commands with `timeout` where possible.
- Prefer `--no-color`, `--tail`, `--since`, and `LIMIT`.
- Redirect output to local files under the evidence root; avoid remote temp files.
- Redact before moving snippets into `problems/*/evidence/`.

Example local capture pattern:

```bash
mkdir -p "$ROOT/raw/docker" "$ROOT/raw/app" "$ROOT/raw/db" "$ROOT/raw/host"
ssh -o BatchMode=yes -o ConnectTimeout=10 "$SSH_TARGET" \
  "cd '$REMOTE_DIR' && timeout 20s docker compose ps --format json" \
  > "$ROOT/raw/docker/compose-ps.json"
```

Use key/agent access when available. If the user explicitly authorizes password auth, use a local hidden prompt or in-memory pty/expect handoff. Do not place the password in command arguments, evidence files, reports, archives, or persistent files.

## Host and Docker Inventory

Collect only deployment and health inventory in this phase. Do not pull application/container logs until a specific issue class points to them.

```bash
date -Is
hostname -f || hostname
uname -a
uptime
df -h
free -m || vmstat 1 3
docker version
docker compose version
```

In the compose directory:

```bash
pwd
ls -la
docker compose ps
docker compose ps --format json
docker compose config --services
docker compose config
```

Notes:

- `docker compose config` may contain environment secrets. Save raw locally, then redact before using it as evidence.
- Do not run `docker compose pull`, `up`, `restart`, `down`, or `exec` commands that write state.
- Do not run `docker compose logs` as inventory. First index persisted diagnostics (`usage_records`, Redis, tool-format debug files) and only then collect targeted logs if needed.

## Code-Defined Business Diagnostics First

Before container logs, collect a bounded evidence index from the sources defined by the code:

- PostgreSQL: `usage_records`, `runtime_config`, `credential_runtime_state`, `credential_events`, `admin_audit_logs`, `external_upstream_pools`, rollup tables, `schema_migrations`.
- Redis: `usage:summary:*`, `usage:dashboard:*`, `usage:records:index`, `usage:records:item:*`, `scheduler:*`, `local_pool:*`, `external_pool:*`.
- Disk: configured `toolFormatDebug.dir`, default `logs/tool-format-debug`, sampled by metadata/fingerprint/request id.
- Admin API: read-only usage/runtime/audit/health endpoints when an admin key is available and safe to use.

See `kiro-rs-evidence-sources.md` for project-specific query examples.

## App Container Evidence, Targeted Only

Identify the app container from `docker compose ps`. Collect:

```bash
docker inspect <app-container>
docker stats --no-stream <app-container>
```

Only after persisted diagnostics identify a relevant time window or when investigating startup/crash/stdout-only symptoms, collect bounded logs:

```bash
docker logs --no-color --tail 80 <app-container>
docker logs --no-color --since <window> --tail <bounded-tail> <app-container>
```

Search locally in targeted captured logs for:

- `ERROR`, `WARN`, `panic`, `thread '`, `backtrace`, `failed`, `timeout`, `invalid`, `schema`, `migration`, `revision`, `usage`, `gateway`, `blocked`, `rate limit`, `429`, `500`, `502`, `503`, `504`.
- request IDs, session IDs, route names, model aliases, upstream account IDs, cache/write/read token fields, and parse/stream errors.

## Startup Crash and Migration Evidence

For repeated restarts, capture:

```bash
docker inspect <app-container> --format '{{json .State}}'
docker compose ps
docker logs --no-color --tail 500 <app-container>
```

Then check PostgreSQL schema metadata for the missing column/table/index cited by logs. Do not run migrations.

## PostgreSQL Read-Only Evidence

Only run through `docker compose exec -T postgres ...` or another known read-only access path. Do not dump whole tables.

Safe metadata examples:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select now();
select table_name, column_name, data_type, is_nullable, column_default
from information_schema.columns
where table_schema = 'public'
order by table_name, ordinal_position;
COMMIT;
```

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select schemaname, relname, n_live_tup, n_dead_tup, last_vacuum, last_autovacuum, last_analyze, last_autoanalyze
from pg_stat_user_tables
order by relname;
COMMIT;
```

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select relname, pg_size_pretty(pg_total_relation_size(relid)) as total_size
from pg_catalog.pg_statio_user_tables
order by pg_total_relation_size(relid) desc
limit 20;
COMMIT;
```

For `usage_records`, first capture table size, schema, and indexes. Only sample explicit columns by time or exact request IDs. Do not use `SELECT *` on this table.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select id, created_at, endpoint, model, status, error_type,
       left(coalesce(error_message, error_detail, ''), 220) as error,
       total_input_tokens, compat_input_tokens, billable_input_tokens,
       output_tokens, cache_read_input_tokens, cache_creation_input_tokens,
       data->>'routeKind' as route_kind,
       data->>'routeSubtype' as route_subtype,
       data->>'upstreamModel' as upstream_model,
       data->>'externalPoolName' as external_pool
from usage_records
where deleted_at is null
  and created_at >= now() - interval '2 hours'
order by created_at desc
limit 20;
COMMIT;
```

For exact request IDs, it is safe to fetch the full JSON diagnostic row:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select data
from usage_records
where id = '<request_id>'
limit 1;
COMMIT;
```

For grouped recent error fingerprints:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select status,
       coalesce(error_type, '') as error_type,
       left(coalesce(error_message, error_detail, ''), 180) as message_prefix,
       count(*) as count,
       min(created_at) as first_seen,
       max(created_at) as last_seen
from usage_records
where deleted_at is null
  and created_at >= now() - interval '2 hours'
  and status <> 'success'
group by status, coalesce(error_type, ''), left(coalesce(error_message, error_detail, ''), 180)
order by count desc, last_seen desc
limit 50;
COMMIT;
```

If the table is very large or lacks a useful time index, avoid the row sample and record the limitation.

## Redis Read-Only Evidence

Use read-only metadata:

```bash
redis-cli INFO server
redis-cli INFO memory
redis-cli INFO stats
redis-cli INFO keyspace
redis-cli DBSIZE
```

Avoid `KEYS *`. If keys are needed, use bounded `SCAN` only:

```bash
redis-cli --scan --pattern '<narrow-prefix>*' | head -100
```

Do not run `DEL`, `FLUSH*`, `SET`, `EXPIRE`, or script commands.

## Gateway and Edge Evidence

Inspect compose services for reverse proxies such as nginx, caddy, traefik, cloudflared, or custom gateway containers.

Collect bounded logs only for gateway/edge symptoms or when app diagnostics show the request never reached kiro.rs:

```bash
docker logs --no-color --since <window> --tail <bounded-tail> <gateway-container>
docker inspect <gateway-container>
```

Look for:

- blocked request messages;
- body size limits;
- request timeout / upstream timeout;
- `413`, `429`, `499`, `500`, `502`, `503`, `504`;
- WAF/security module messages;
- route mismatches and header stripping.

## Usage and Cost Anomaly Evidence

Correlate app logs, usage records, and request IDs. Keep evidence around:

- request ID, route, model alias and resolved upstream model;
- reported `input_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`, `output_tokens`;
- local estimated tokens and payload guard breakdown;
- first-byte latency and total latency;
- upstream status and error message;
- whether the request was local credential or external pool;
- config that controls usage rewrite, cache simulation, output token rewrite, and final output limits.

Do not infer a bug from one high token value alone. Preserve the payload guard breakdown and the final reported usage fields together.

## External Pool and Upstream Error Evidence

For external pool failures, preserve:

- which pool/provider was selected;
- HTTP status;
- sanitized response body shape;
- whether the body looked like a real upstream API error or non-API content such as ads/promo/HTML/login pages;
- retry/fallback chain.

For official Kiro/upstream errors, preserve the upstream message verbatim after redacting secrets and account identifiers.

## Local Problem Folder Template

Each `problem.md` should use this structure:

```markdown
# P### short title

## Status

open | likely-fixed | needs-repro | informational

## Impact

Who/what is affected, route, model, accounts, and approximate time range.

## Evidence

- `evidence/<file>`: why this file matters.

## Normalized signature

Component, route, error code/message pattern, and fingerprint hash.

## Analysis

What the evidence proves, what is inferred, and what is still unknown.

## Reproduction hints

Local config/data/request shape needed to reproduce without production writes.

## Next checks

Smallest safe checks or code locations to inspect next.
```
