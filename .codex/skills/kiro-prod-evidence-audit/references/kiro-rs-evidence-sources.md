# kiro.rs Evidence Sources

Use this reference after `SKILL.md` and before any production SSH command. It is derived from the local kiro.rs codebase, so the collection order matches where the application actually persists diagnostics.

## Evidence order

Container stdout is not the primary log source for kiro.rs. Start with persisted business diagnostics and only pull container logs after a specific symptom points there.

1. PostgreSQL business diagnostics: `usage_records`, `runtime_config`, credential runtime/state/event tables, external pool tables, rollups, schema/version tables.
2. Redis live diagnostics: usage summaries, dashboard/top/error caches, recent usage record snapshots, scheduler/cooldown/in-flight/rate-limit state, external pool queue/in-flight state.
3. Project disk diagnostics: `toolFormatDebug.dir`, default `logs/tool-format-debug`, JSONL files written by `src/anthropic/tool_format_debug.rs`.
4. Read-only admin/health endpoints: health/readiness, usage records/summary/dashboard, audit logs, runtime config, credential runtime views.
5. Docker/container/gateway logs: startup crashes, stdout/stderr warnings, reverse-proxy/gateway errors, or missing persisted diagnostics.

## Code-derived map

- `src/anthropic/usage.rs`: `UsageRecord` is the central request diagnostic object. It contains request id, route, model alias/resolution, credential/external-pool attempts, usage fields, latency trace, public/internal error fields, payload breakdown, and payload guard report.
- `src/storage/postgres.rs`: `usage_records` stores indexed columns plus full `data JSONB`. Startup migration intentionally avoids historical `usage_records` backfills and rollup rebuilds because large production tables can be multi-GB.
- `src/storage/redis_cache.rs`: Redis stores `usage:summary:*`, `usage:dashboard:*`, `usage:records:index`, `usage:records:item:*`, scheduler state, local-pool circuit state, and external-pool queue/in-flight keys.
- `src/anthropic/tool_format_debug.rs` and `src/model/config.rs`: tool-format diagnostics write bounded JSONL records under `logs/tool-format-debug` by default. Records include request id, endpoint, model, body hash/size, repair metrics, tool anomalies, samples, sampling fingerprint/group key, and possibly a capped request body sample.
- `src/anthropic/handlers.rs`: `payloadBreakdown` and `payloadGuardReport` are persisted into `usage_records.data` for failures and for successful requests with material payload guard changes or large final bytes.
- `src/external_pool.rs` and `src/external_pool/*`: external pool attempts, public masked errors, internal raw/shaped/reported usage, billing, and pool metadata are represented in usage records and related tables.
- `src/admin/router.rs`: read-only admin routes expose usage records, summaries, dashboard windows/series/top/breakdown, external-pool billing, usage writer stats, audit logs, runtime config, credential runtime views, and system version.

## Phase 1: deployment inventory, no broad app logs

Goal: learn service names, mounts, versions, health, and connection paths without pulling logs or scanning data.

Allowed examples:

```bash
date -Is
hostname -f || hostname
pwd
ls -la
timeout 10s docker compose ps
timeout 10s docker compose ps --format json
timeout 10s docker compose config --services
timeout 10s docker inspect <app-container> --format '{{json .State}}'
timeout 10s docker inspect <app-container> --format '{{json .Mounts}}'
timeout 5s curl -fsS http://127.0.0.1:<port>/healthz
timeout 5s curl -fsS http://127.0.0.1:<port>/readyz
```

If `docker compose config` is needed, treat it as secret-bearing raw evidence and redact before use.

Do not run `docker compose logs` in Phase 1.

## Phase 2: code-defined evidence index

Goal: build a bounded index of what evidence exists. Do not collect full histories.

### PostgreSQL metadata

Always use read-only transactions and short statement timeouts:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select now();
COMMIT;
```

Table sizes:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select relname,
       pg_size_pretty(pg_total_relation_size(relid)) as total_size,
       pg_total_relation_size(relid) as total_bytes
from pg_catalog.pg_statio_user_tables
order by pg_total_relation_size(relid) desc
limit 30;
COMMIT;
```

Indexes for high-risk tables:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select tablename, indexname, indexdef
from pg_indexes
where schemaname = 'public'
  and tablename in (
    'usage_records',
    'admin_audit_logs',
    'credential_events',
    'credential_runtime_state',
    'external_upstream_pools',
    'schema_migrations'
  )
order by tablename, indexname;
COMMIT;
```

Schema/version state:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select table_name, column_name, data_type, is_nullable, column_default
from information_schema.columns
where table_schema = 'public'
  and table_name in (
    'runtime_config',
    'credentials',
    'credential_runtime_state',
    'usage_records',
    'external_upstream_pools',
    'admin_audit_logs',
    'credential_events',
    'schema_migrations'
  )
order by table_name, ordinal_position;
select version, checksum, applied_at
from schema_migrations
order by version desc
limit 20;
COMMIT;
```

### Usage diagnostics

Use `usage_records.created_at`, `status`, `model`, `credential_id`, `conversation_id`, and request id. Avoid broad `SELECT *`. Only select `data` by exact request id or bounded recent samples.

Recent error fingerprints:

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

Exact request lookup:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select data
from usage_records
where id = '<request_id>'
limit 1;
COMMIT;
```

Recent error samples:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select id, created_at, endpoint, model, status, error_type,
       left(coalesce(error_message, error_detail, ''), 220) as error,
       data->>'routeKind' as route_kind,
       data->>'routeSubtype' as route_subtype,
       data->>'upstreamModel' as upstream_model,
       data->>'externalPoolName' as external_pool
from usage_records
where deleted_at is null
  and created_at >= now() - interval '2 hours'
  and status <> 'success'
order by created_at desc
limit 30;
COMMIT;
```

Usage/token anomalies:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select id, created_at, endpoint, model, status,
       total_input_tokens, compat_input_tokens, billable_input_tokens,
       cache_read_input_tokens, cache_creation_input_tokens, output_tokens,
       data->'rawUsage' as raw_usage,
       data->'payloadBreakdown' as payload_breakdown,
       data->'payloadGuardReport' as payload_guard_report,
       data->'externalPoolBilling' as external_pool_billing
from usage_records
where deleted_at is null
  and created_at >= now() - interval '2 hours'
order by total_input_tokens desc
limit 20;
COMMIT;
```

Slow/streaming anomalies:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select id, created_at, endpoint, model, status,
       duration_ms,
       (data->>'firstTokenLatencyMs')::bigint as first_token_latency_ms,
       data->'latencyTrace' as latency_trace,
       left(coalesce(error_message, error_detail, ''), 220) as error
from usage_records
where deleted_at is null
  and created_at >= now() - interval '2 hours'
order by duration_ms desc
limit 20;
COMMIT;
```

### Config/runtime tables

These tables can contain secrets. Capture only redacted shape unless exact fields are needed for a problem.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select updated_at, version,
       config
         - 'apiKey'
         - 'apiKeys'
         - 'adminApiKey'
         - 'requestApiKeys'
         - 'postgres'
         - 'redis'
         - 'credentials'
         - 'externalPools'
       as redacted_config
from runtime_config
where id = 'default';

select id, name, enabled, priority, max_concurrent_requests,
       usage_projection_mode, stream_response_mode,
       request_body_mode, raw_model_mode, auto_disable_policy,
       auto_disabled, auto_disabled_reason, auto_disabled_at, auto_disabled_until,
       auto_disabled_last_error, preserve_path, normalize_model_version_dots,
       model_mapping_mode, model_mapping_require_match, supported_models,
       created_at, updated_at
from external_upstream_pools
where deleted_at is null
order by priority asc, id asc
limit 50;
COMMIT;
```

Credential runtime/event summaries:

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
select credential_id, failure_count, refresh_failure_count, disabled_reason,
       warmup_remaining, generation, revision, updated_at
from credential_runtime_state
order by updated_at desc
limit 50;

select credential_id, event_type, reason, count(*) as count,
       min(created_at) as first_seen,
       max(created_at) as last_seen
from credential_events
where created_at >= now() - interval '2 hours'
group by credential_id, event_type, reason
order by count desc, last_seen desc
limit 50;

select action, object_type, success,
       left(coalesce(error_message, ''), 180) as error,
       count(*) as count,
       max(created_at) as last_seen
from admin_audit_logs
where created_at >= now() - interval '24 hours'
group by action, object_type, success, left(coalesce(error_message, ''), 180)
order by last_seen desc
limit 50;
COMMIT;
```

### Redis diagnostics

Use only metadata and narrow prefixes. Never use `KEYS *`, writes, Lua scripts, deletes, flushes, or expiry changes.

Safe examples:

```bash
timeout 5s redis-cli INFO server
timeout 5s redis-cli INFO memory
timeout 5s redis-cli INFO stats
timeout 5s redis-cli INFO keyspace
timeout 5s redis-cli DBSIZE
timeout 5s redis-cli HGETALL usage:summary:totals
timeout 5s redis-cli HGETALL usage:summary:cache_read
timeout 5s redis-cli ZREVRANGE usage:records:index 0 49 WITHSCORES
timeout 5s redis-cli --scan --pattern 'usage:dashboard:top:*' | head -100
timeout 5s redis-cli --scan --pattern 'scheduler:*' | head -100
timeout 5s redis-cli --scan --pattern 'external_pool:*' | head -100
timeout 5s redis-cli --scan --pattern 'local_pool:*' | head -100
```

If Redis has a configured prefix, prepend it to all keys/patterns.

### Tool-format debug disk diagnostics

Determine configured path from `runtime_config.config.toolFormatDebug.dir` when available; otherwise use `logs/tool-format-debug`.

Index before sampling:

```bash
test -d logs/tool-format-debug && \
find logs/tool-format-debug -type f -name 'tool-format-*.jsonl' \
  -printf '%TY-%Tm-%Td %TH:%TM %s %p\n' | sort | tail -50
```

Sample only selected files:

```bash
head -n 5 logs/tool-format-debug/<file>
tail -n 20 logs/tool-format-debug/<file>
```

If `jq` is available and the file is small enough to sample locally, useful fields are:

```bash
jq -c '{ts,requestId,endpoint,requestedModel,upstreamModel,errorClass,errorReason,errorMessageClass,body,toolCounts,repair,anomalies,sampling}' <file> | tail -20
```

Do not wholesale copy large JSONL files until fingerprints identify a specific window/file. Records may include capped request bodies and must be treated as sensitive.

## Phase 3: targeted issue collection

Choose the smallest source for the problem class:

- Usage/cost/token anomaly: exact `usage_records.data` for request IDs, recent top-token rows, Redis usage summaries, runtime config fields for usage rewrite/cache/output policies.
- Tool/schema/payload error: `usage_records.data.payloadGuardReport`, `payloadBreakdown`, `tool_format_debug_ref`, then matching `tool-format-debug` JSONL samples by request id/fingerprint.
- External pool error: `usage_records` route/external attempts/public/internal error fields, external pool metadata with API keys removed, Redis external-pool in-flight/queue keys, then targeted container logs only if upstream/gateway behavior is not persisted.
- Official Kiro/local upstream error: usage credential attempts, internal/public error fields, credential runtime state/events, official upstream message after redaction.
- Scheduler/capacity/rate-limit: Redis scheduler/global/local-pool keys, credential runtime state/events, usage route/fallback records.
- Startup/migration: Docker state, app container last logs, PostgreSQL schema/migrations/indexes. This is one of the few cases where container logs can be first-class evidence.
- Gateway/edge block: gateway container inspect/log samples, compose service map, request status distribution, then app usage records to see whether the request reached kiro.rs.

## Phase 4: cluster and package

Cluster by probable root cause: component, route, code path, normalized error, public/internal error split, request shape, and remediation. Keep two or three representative redacted samples per problem when variants matter; otherwise keep one sample plus counts.

Run `scripts/package_evidence.py` locally after redaction. Default archive must exclude `raw/`.

## Hard stops

Stop before any step that requires:

- broad `docker compose logs` before persisted diagnostics are checked;
- full `usage_records` scan, full table dump, or unbounded aggregation;
- `SELECT *` on large business tables;
- `KEYS *` or Redis writes;
- copying entire `logs/tool-format-debug` without a specific request/fingerprint/window;
- exposing credentials in commands, files, reports, archives, or final answers.
