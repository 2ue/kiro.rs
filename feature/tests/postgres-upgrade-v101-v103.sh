#!/usr/bin/env bash
set -euo pipefail

# Replays real v0.0.101/v0.0.102/v0.0.103 PostgreSQL and Redis state through
# the current release binary. All infrastructure is temporary and uniquely named.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SMOKE_ROOT="${SMOKE_ROOT:-/tmp/kiro-upgrade-smoke-20260716-a}"
RESULT_ROOT="${RESULT_ROOT:-$PROJECT_ROOT/target/validation/f04-upgrade-20260716}"
CURRENT_BINARY="${CURRENT_BINARY:-$SMOKE_ROOT/bin/kiro-rs-current-fixed}"
ROUNDS="${ROUNDS:-3}"
VERSIONS="${VERSIONS:-v101 v102 v103}"
RUN_ID="${RUN_ID:-$$}"
PG_IMAGE="${PG_IMAGE:-postgres:18-alpine}"
REDIS_IMAGE="${REDIS_IMAGE:-redis:7-alpine}"
PG_PASSWORD="upgrade_fixture_pw"
MIGRATION_LOCK_ID=4950531234001

mkdir -p "$RESULT_ROOT"
SUMMARY="$RESULT_ROOT/results.tsv"
printf 'version\tscenario\tround\tphase\tresult\telapsed_ms\ttables\tcolumns\tindexes\tmarkers\tusage\trollups\tbuckets\tpools\tredis_keys\tcredential_revision\truntime_revision\truntime_generation\tmetadata_churn\n' >"$SUMMARY"

CURRENT_PID=""
PG_CONTAINER=""
REDIS_CONTAINER=""

cleanup_process() {
  if [[ -n "$CURRENT_PID" ]] && kill -0 "$CURRENT_PID" 2>/dev/null; then
    kill -TERM "$CURRENT_PID" 2>/dev/null || true
    for _ in $(seq 1 100); do
      kill -0 "$CURRENT_PID" 2>/dev/null || break
      sleep 0.05
    done
  fi
  CURRENT_PID=""
}

cleanup_infra() {
  cleanup_process
  [[ -z "$PG_CONTAINER" ]] || docker rm -f "$PG_CONTAINER" >/dev/null 2>&1 || true
  [[ -z "$REDIS_CONTAINER" ]] || docker rm -f "$REDIS_CONTAINER" >/dev/null 2>&1 || true
  PG_CONTAINER=""
  REDIS_CONTAINER=""
}
trap cleanup_infra EXIT INT TERM

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

require_file() {
  [[ -f "$1" ]] || fail "required file is missing: $1"
}

pg_sql() {
  docker exec "$PG_CONTAINER" psql -v ON_ERROR_STOP=1 -U upgrade_fixture -d postgres "$@"
}

reset_storage() {
  pg_sql -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;' >/dev/null
  docker exec "$REDIS_CONTAINER" redis-cli FLUSHALL >/dev/null
}

make_fixture_files() {
  local version=$1 scenario=$2 round=$3 service_port=$4
  FIXTURE_DIR="$RESULT_ROOT/runtime/${version}-${scenario}-r${round}"
  mkdir -p "$FIXTURE_DIR"
  PREFIX="kiro_f04:${version}:${scenario}:r${round}"

  cat >"$FIXTURE_DIR/config.json" <<EOF
{
  "postgres": {
    "maxConnections": 5,
    "migrateOnStart": true,
    "compressUsageRollupsOnStart": false
  },
  "redis": { "keyPrefix": "$PREFIX" },
  "host": "127.0.0.1",
  "port": $service_port,
  "apiKey": "fixture-request-key-${version}-${scenario}-r${round}",
  "adminApiKey": "fixture-admin-key-${version}-${scenario}-r${round}",
  "externalPools": {
    "externalPoolsEnabled": true,
    "externalPoolCapacityMode": "wait",
    "externalPoolDispatchMaxWaitSecs": 0,
    "fallbackOnLocalCapacityExhausted": true,
    "fallbackOnNoAvailableCredentials": true,
    "fallbackOnLocalTransientExhausted": true
  }
}
EOF

  cat >"$FIXTURE_DIR/credentials.json" <<EOF
{
  "id": 1,
  "accessToken": "fixture-access-token-${version}-${scenario}-r${round}",
  "expiresAt": "2099-01-01T00:00:00Z",
  "authMethod": "social",
  "email": "fixture-${version}-${scenario}-r${round}@example.invalid",
  "disabled": true
}
EOF
}

start_ready() {
  local binary=$1 log_file=$2 service_port=$3
  local started finished response=""
  started=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
  env -u KIRO_API_KEY \
    KIRO_RS_POSTGRES_URL="$POSTGRES_URL" \
    KIRO_RS_REDIS_URL="$REDIS_URL" \
    KIRO_RS_HOST=127.0.0.1 \
    KIRO_RS_PORT="$service_port" \
    KIRO_RS_POSTGRES_MIGRATE_ON_START=true \
    KIRO_RS_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START=false \
    RUST_LOG=info \
    "$binary" -c "$FIXTURE_DIR/config.json" --credentials "$FIXTURE_DIR/credentials.json" >"$log_file" 2>&1 &
  CURRENT_PID=$!

  for _ in $(seq 1 600); do
    if response=$(curl -fsS "http://127.0.0.1:${service_port}/readyz" 2>/dev/null); then
      break
    fi
    kill -0 "$CURRENT_PID" 2>/dev/null || break
    sleep 0.1
  done
  finished=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
  LAST_ELAPSED_MS=$(perl -e "printf \"%.3f\", ($finished - $started) * 1000")
  [[ -n "$response" ]] || {
    tail -n 80 "$log_file" >&2 || true
    fail "service did not become ready: $binary"
  }
}

stop_ready() {
  [[ -n "$CURRENT_PID" ]] || return 0
  kill -TERM "$CURRENT_PID"
  for _ in $(seq 1 200); do
    if ! kill -0 "$CURRENT_PID" 2>/dev/null; then
      CURRENT_PID=""
      return 0
    fi
    sleep 0.05
  done
  fail "service did not stop cleanly: pid=$CURRENT_PID"
}

start_expected_checksum_failure() {
  local log_file=$1 service_port=$2
  local started finished rc
  started=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
  env -u KIRO_API_KEY \
    KIRO_RS_POSTGRES_URL="$POSTGRES_URL" \
    KIRO_RS_REDIS_URL="$REDIS_URL" \
    KIRO_RS_HOST=127.0.0.1 \
    KIRO_RS_PORT="$service_port" \
    KIRO_RS_POSTGRES_MIGRATE_ON_START=true \
    KIRO_RS_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START=false \
    RUST_LOG=info \
    "$CURRENT_BINARY" -c "$FIXTURE_DIR/config.json" --credentials "$FIXTURE_DIR/credentials.json" >"$log_file" 2>&1 &
  CURRENT_PID=$!

  for _ in $(seq 1 200); do
    if curl -fsS "http://127.0.0.1:${service_port}/readyz" >/dev/null 2>&1; then
      cleanup_process
      fail "checksum-corrupt startup unexpectedly became ready"
    fi
    kill -0 "$CURRENT_PID" 2>/dev/null || break
    sleep 0.05
  done
  set +e
  wait "$CURRENT_PID"
  rc=$?
  set -e
  CURRENT_PID=""
  finished=$(perl -MTime::HiRes=time -e 'printf "%.6f", time')
  LAST_ELAPSED_MS=$(perl -e "printf \"%.3f\", ($finished - $started) * 1000")
  [[ $rc -ne 0 ]] || fail "checksum-corrupt startup exited successfully"
  grep -q 'credential-storage-revision-v1 checksum mismatch' "$log_file" || {
    tail -n 80 "$log_file" >&2 || true
    fail "checksum-corrupt startup returned the wrong error"
  }
}

seed_fixture() {
  local version=$1 scenario=$2 round=$3
  local usage_rows=25 rollup_rows=2 bucket_rows=1
  if [[ "$scenario" == large ]]; then
    usage_rows=50000
    rollup_rows=5000
    bucket_rows=1000
  fi

  pg_sql -c "
INSERT INTO runtime_config (id, config, version)
VALUES ('current', jsonb_build_object('fixture','${version}-${scenario}-r${round}'), 17)
ON CONFLICT (id) DO UPDATE SET config=EXCLUDED.config, version=EXCLUDED.version;
INSERT INTO credential_runtime_state (
  credential_id,failure_count,refresh_failure_count,disabled_reason,warmup_remaining,updated_at
)
VALUES (1,3,2,'fixture-sentinel',4,now())
ON CONFLICT (credential_id) DO UPDATE SET
  failure_count=EXCLUDED.failure_count,
  refresh_failure_count=EXCLUDED.refresh_failure_count,
  disabled_reason=EXCLUDED.disabled_reason,
  warmup_remaining=EXCLUDED.warmup_remaining;
INSERT INTO external_upstream_pools (name,base_url,api_key,enabled,priority)
VALUES ('fixture-${version}-${scenario}-r${round}','https://fixture.invalid','fixture-pool-key',true,42);
INSERT INTO usage_records (
  id,created_at,endpoint,stream,model,conversation_id,credential_id,credential_label,
  status,usage_source,total_input_tokens,compat_input_tokens,billable_input_tokens,
  output_tokens,cache_read_input_tokens,cache_creation_input_tokens,
  cache_creation_5m_input_tokens,cache_creation_1h_input_tokens,
  estimated_cost_usd,pricing_available,duration_ms,simulated,sticky_bound,
  fallback_from_sticky,data
)
SELECT '${version}-${scenario}-r${round}-'||g, now()-(g||' milliseconds')::interval,
  '/cc/v1/messages',false,'claude-sonnet-4','conv-'||g,1,'fixture',
  'success','upstream_metadata',100+(g%100),100+(g%100),100+(g%100),20,
  g%1000,5,5,0,0.01,true,50,false,false,false,
  jsonb_build_object('fixture',true,'seq',g)
FROM generate_series(1,${usage_rows}) g;
INSERT INTO usage_rollup_totals (
  dimension,dimension_key,requests,success_requests,total_input_tokens,total_output_tokens
)
SELECT 'fixture','key-'||g,g,g,g*100,g*20 FROM generate_series(1,${rollup_rows}) g;
INSERT INTO usage_rollup_time_buckets (
  bucket_start,dimension,dimension_key,requests,success_requests,total_input_tokens,total_output_tokens
)
SELECT date_trunc('hour',now())-(g||' hours')::interval,'fixture','bucket-'||g,g,g,g*100,g*20
FROM generate_series(1,${bucket_rows}) g;
" >/dev/null

  if [[ "$scenario" == large ]]; then
    for family in usage:summary:cache_read usage:records:item sticky credential:inflight; do
      docker exec "$REDIS_CONTAINER" redis-cli EVAL \
        "for i=1,10000 do redis.call('SET', KEYS[1]..i, '1') end return 10000" \
        1 "${PREFIX}:${family}:" >/dev/null
    done
  else
    docker exec "$REDIS_CONTAINER" redis-cli MSET \
      "${PREFIX}:usage:records:item:1" one \
      "${PREFIX}:usage:records:item:2" two \
      "${PREFIX}:sticky:1" 1 \
      "${PREFIX}:credential:cooldown:1" 1 >/dev/null
    docker exec "$REDIS_CONTAINER" redis-cli HSET \
      "${PREFIX}:usage:summary:cache_read:1" requests 1 tokens 10 >/dev/null
    docker exec "$REDIS_CONTAINER" redis-cli ZADD \
      "${PREFIX}:usage:records:index" 1 one 2 two >/dev/null
  fi
}

snapshot() {
  local output=$1
  local manifest_base=${output%.snapshot}
  pg_sql -At -F '|' -c "
WITH
columns_fp AS (
  SELECT count(*) AS n, md5(COALESCE(string_agg(
    table_name||':'||column_name||':'||data_type||':'||is_nullable||':'||COALESCE(column_default,''),
    ',' ORDER BY table_name,column_name),'')) AS h
  FROM information_schema.columns WHERE table_schema='public'
),
tables_fp AS (
  SELECT count(*) AS n, md5(COALESCE(string_agg(table_name,',' ORDER BY table_name),'')) AS h
  FROM information_schema.tables WHERE table_schema='public'
),
indexes_fp AS (
  SELECT count(*) AS n, md5(COALESCE(string_agg(indexname||':'||indexdef,',' ORDER BY indexname),'')) AS h
  FROM pg_indexes WHERE schemaname='public'
),
markers_fp AS (
  SELECT count(*) AS n,
    md5(COALESCE(string_agg(version||':'||checksum,',' ORDER BY version),'')) AS semantic_h,
    md5(COALESCE(string_agg(version||':'||checksum||':'||applied_at::text,',' ORDER BY version),'')) AS full_h
  FROM schema_migrations
),
runtime_fp AS (
  SELECT md5(COALESCE(string_agg(row_to_json(r)::text,',' ORDER BY id),'')) AS h FROM runtime_config r
),
credential_fp AS (
  SELECT md5(COALESCE(string_agg(row_to_json(c)::text,',' ORDER BY id),'')) AS h FROM credentials c
),
state_fp AS (
  SELECT md5(COALESCE(string_agg(row_to_json(s)::text,',' ORDER BY credential_id),'')) AS h FROM credential_runtime_state s
),
usage_fp AS (
  SELECT count(*) AS n, md5(COALESCE(string_agg(id||':'||total_input_tokens||':'||output_tokens,',' ORDER BY id),'')) AS h FROM usage_records
),
rollup_fp AS (
  SELECT count(*) AS n, md5(COALESCE(string_agg(dimension||':'||dimension_key||':'||requests,',' ORDER BY dimension,dimension_key),'')) AS h FROM usage_rollup_totals
),
bucket_fp AS (
  SELECT count(*) AS n, md5(COALESCE(string_agg(bucket_start::text||':'||dimension||':'||dimension_key||':'||requests,',' ORDER BY bucket_start,dimension,dimension_key),'')) AS h FROM usage_rollup_time_buckets
),
pool_fp AS (
  SELECT count(*) AS n, md5(COALESCE(string_agg(name||':'||enabled||':'||priority,',' ORDER BY name),'')) AS h FROM external_upstream_pools
)
SELECT tables_fp.n,columns_fp.n,indexes_fp.n,
  md5(tables_fp.h||columns_fp.h||indexes_fp.h),
  markers_fp.n,markers_fp.semantic_h,markers_fp.full_h,
  runtime_fp.h,credential_fp.h,state_fp.h,
  usage_fp.n,usage_fp.h,rollup_fp.n,rollup_fp.h,bucket_fp.n,bucket_fp.h,pool_fp.n,pool_fp.h
FROM tables_fp,columns_fp,indexes_fp,markers_fp,runtime_fp,credential_fp,state_fp,usage_fp,rollup_fp,bucket_fp,pool_fp;
" >"$output"

  pg_sql -At -F '|' -c "
SELECT table_name,column_name,ordinal_position,udt_name,is_nullable,COALESCE(column_default,'')
FROM information_schema.columns
WHERE table_schema='public'
ORDER BY table_name,column_name;
" >"${manifest_base}.columns.tsv"
  pg_sql -At -F '|' -c "
SELECT indexname,indexdef FROM pg_indexes WHERE schemaname='public' ORDER BY indexname;
" >"${manifest_base}.indexes.tsv"
  pg_sql -At -F '|' -c "
SELECT version,checksum FROM schema_migrations ORDER BY version;
" >"${manifest_base}.markers.tsv"
}

semantic_snapshot() {
  local input=$1 output=$2
  awk -F '|' 'BEGIN{OFS="|"}{$7=""; print}' "$input" >"$output"
}

assert_current_state() {
  local scenario=$1
  local expected_usage=25 expected_rollups=2 expected_buckets=1 expected_redis=6
  if [[ "$scenario" == large ]]; then
    expected_usage=50000
    expected_rollups=5000
    expected_buckets=1000
    expected_redis=40000
  fi

  local state
  state=$(pg_sql -At -F '|' -c "
SELECT
  (SELECT count(*) FROM schema_migrations),
  (SELECT count(*) FROM usage_records),
  (SELECT count(*) FROM usage_rollup_totals),
  (SELECT count(*) FROM usage_rollup_time_buckets),
  (SELECT count(*) FROM external_upstream_pools),
  (SELECT version FROM runtime_config WHERE id='current'),
  (SELECT revision FROM credentials WHERE id=1),
  (SELECT revision FROM credential_runtime_state WHERE credential_id=1),
  (SELECT generation FROM credential_runtime_state WHERE credential_id=1);
")
  IFS='|' read -r markers usage rollups buckets pools runtime_version CRED_REVISION STATE_REVISION STATE_GENERATION <<<"$state"
  [[ "$markers" == 6 ]] || fail "expected 6 migration markers, got $markers"
  [[ "$usage" == "$expected_usage" ]] || fail "usage row loss: expected $expected_usage, got $usage"
  [[ "$rollups" == "$expected_rollups" ]] || fail "rollup row loss: expected $expected_rollups, got $rollups"
  [[ "$buckets" == "$expected_buckets" ]] || fail "bucket row loss: expected $expected_buckets, got $buckets"
  [[ "$pools" == 1 ]] || fail "external pool row loss: expected 1, got $pools"
  [[ "$runtime_version" == 17 ]] || fail "runtime config sentinel changed: $runtime_version"
  (( CRED_REVISION >= 1 )) || fail "credential revision migration failed: $CRED_REVISION"
  (( STATE_REVISION >= 0 )) || fail "runtime-state revision migration failed: $STATE_REVISION"
  (( STATE_GENERATION >= 0 )) || fail "runtime-state generation migration failed: $STATE_GENERATION"

  REDIS_KEYS=$(docker exec "$REDIS_CONTAINER" redis-cli --scan --pattern "${PREFIX}:*" | wc -l | tr -d ' ')
  (( REDIS_KEYS >= expected_redis )) || fail "Redis fixture keys were lost: expected >=$expected_redis, got $REDIS_KEYS"
}

record_snapshot() {
  local version=$1 scenario=$2 round=$3 phase=$4 result=$5 elapsed=$6 file=$7 churn=$8
  local tables columns indexes markers usage rollups buckets pools
  IFS='|' read -r tables columns indexes _ markers _ _ _ _ _ usage _ rollups _ buckets _ pools _ <"$file"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$version" "$scenario" "$round" "$phase" "$result" "$elapsed" \
    "$tables" "$columns" "$indexes" "$markers" "$usage" "$rollups" "$buckets" "$pools" \
    "${REDIS_KEYS:-}" "${CRED_REVISION:-}" "${STATE_REVISION:-}" "${STATE_GENERATION:-}" "$churn" >>"$SUMMARY"
}

hold_migration_lock() {
  local log_file=$1
  pg_sql -At -c "SELECT pg_advisory_lock($MIGRATION_LOCK_ID); SELECT pg_sleep(0.75); SELECT pg_advisory_unlock($MIGRATION_LOCK_ID);" >"$log_file" 2>&1 &
  LOCK_HOLDER_PID=$!
  for _ in $(seq 1 100); do
    local state
    state=$(pg_sql -At -c "SELECT CASE WHEN pg_try_advisory_lock($MIGRATION_LOCK_ID) THEN CASE WHEN pg_advisory_unlock($MIGRATION_LOCK_ID) THEN 'free' ELSE 'unlock-failed' END ELSE 'held' END;")
    [[ "$state" == held ]] && return 0
    sleep 0.01
  done
  fail "could not confirm advisory migration lock holder"
}

create_infra() {
  local version=$1
  PG_CONTAINER="kiro-f04-${version}-pg-${RUN_ID}"
  REDIS_CONTAINER="kiro-f04-${version}-redis-${RUN_ID}"
  docker run -d --name "$PG_CONTAINER" \
    -e POSTGRES_USER=upgrade_fixture \
    -e POSTGRES_PASSWORD="$PG_PASSWORD" \
    -e POSTGRES_DB=postgres \
    -p 127.0.0.1::5432 "$PG_IMAGE" >/dev/null
  docker run -d --name "$REDIS_CONTAINER" -p 127.0.0.1::6379 "$REDIS_IMAGE" >/dev/null
  # The official image briefly exposes its temporary bootstrap server before
  # restarting into the final server. Do not mistake that transient socket for
  # fixture readiness.
  for _ in $(seq 1 400); do
    if docker logs "$PG_CONTAINER" 2>&1 | grep -q 'PostgreSQL init process complete' && \
      docker exec "$PG_CONTAINER" psql -U upgrade_fixture -d postgres -Atc 'SELECT 1' 2>/dev/null | grep -qx 1; then
      break
    fi
    sleep 0.05
  done
  docker exec "$PG_CONTAINER" psql -U upgrade_fixture -d postgres -Atc 'SELECT 1' 2>/dev/null | grep -qx 1 || fail "PostgreSQL did not become ready"
  for _ in $(seq 1 100); do
    [[ "$(docker exec "$REDIS_CONTAINER" redis-cli ping 2>/dev/null)" == PONG ]] && break
    sleep 0.05
  done
  PG_PORT=$(docker port "$PG_CONTAINER" 5432/tcp | sed 's/.*://')
  REDIS_PORT=$(docker port "$REDIS_CONTAINER" 6379/tcp | sed 's/.*://')
  POSTGRES_URL="postgres://upgrade_fixture:${PG_PASSWORD}@127.0.0.1:${PG_PORT}/postgres"
  REDIS_URL="redis://127.0.0.1:${REDIS_PORT}/0"
}

run_success_round() {
  local version=$1 scenario=$2 round=$3 service_port=$4 old_binary=$5
  reset_storage
  make_fixture_files "$version" "$scenario" "$round" "$service_port"

  start_ready "$old_binary" "$FIXTURE_DIR/old-start.log" "$service_port"
  stop_ready
  seed_fixture "$version" "$scenario" "$round"
  snapshot "$FIXTURE_DIR/before-upgrade.snapshot"

  if [[ "$scenario" == normal && "$round" == 1 ]]; then
    hold_migration_lock "$FIXTURE_DIR/advisory-lock.log"
    start_ready "$CURRENT_BINARY" "$FIXTURE_DIR/current-start.log" "$service_port"
    wait "$LOCK_HOLDER_PID"
    awk -v elapsed="$LAST_ELAPSED_MS" 'BEGIN{exit !(elapsed >= 700)}' || fail "startup did not wait for the held advisory lock: ${LAST_ELAPSED_MS}ms"
  else
    start_ready "$CURRENT_BINARY" "$FIXTURE_DIR/current-start.log" "$service_port"
  fi
  local first_elapsed=$LAST_ELAPSED_MS
  stop_ready
  assert_current_state "$scenario"
  snapshot "$FIXTURE_DIR/after-upgrade.snapshot"
  semantic_snapshot "$FIXTURE_DIR/after-upgrade.snapshot" "$FIXTURE_DIR/after-upgrade.semantic"
  local inline_before
  inline_before=$(pg_sql -At -c "SELECT applied_at::text FROM schema_migrations WHERE version='inline-schema'")

  sleep 0.02
  start_ready "$CURRENT_BINARY" "$FIXTURE_DIR/current-repeat.log" "$service_port"
  local repeat_elapsed=$LAST_ELAPSED_MS
  stop_ready
  assert_current_state "$scenario"
  snapshot "$FIXTURE_DIR/after-repeat.snapshot"
  semantic_snapshot "$FIXTURE_DIR/after-repeat.snapshot" "$FIXTURE_DIR/after-repeat.semantic"
  cmp -s "$FIXTURE_DIR/after-upgrade.semantic" "$FIXTURE_DIR/after-repeat.semantic" || fail "second startup changed schema or business data"
  local inline_after metadata_churn=0
  inline_after=$(pg_sql -At -c "SELECT applied_at::text FROM schema_migrations WHERE version='inline-schema'")
  if [[ "$inline_before" != "$inline_after" ]]; then
    metadata_churn=1
    fail "second startup refreshed inline-schema.applied_at without a checksum change"
  fi
  record_snapshot "$version" "$scenario" "$round" upgrade pass "$first_elapsed" "$FIXTURE_DIR/after-upgrade.snapshot" "$metadata_churn"
  record_snapshot "$version" "$scenario" "$round" repeat pass "$repeat_elapsed" "$FIXTURE_DIR/after-repeat.snapshot" "$metadata_churn"
}

run_failure_round() {
  local version=$1 round=$2 service_port=$3 old_binary=$4
  local scenario=failure
  reset_storage
  make_fixture_files "$version" "$scenario" "$round" "$service_port"

  start_ready "$old_binary" "$FIXTURE_DIR/old-start.log" "$service_port"
  stop_ready
  seed_fixture "$version" normal "$round"
  pg_sql -c "
INSERT INTO schema_migrations(version,checksum,applied_at)
VALUES ('credential-storage-revision-v1','fixture-corrupt-checksum',now())
ON CONFLICT(version) DO UPDATE SET checksum=EXCLUDED.checksum, applied_at=EXCLUDED.applied_at;
" >/dev/null
  snapshot "$FIXTURE_DIR/before-failure.snapshot"
  CRED_REVISION=""
  STATE_REVISION=""
  STATE_GENERATION=""

  for attempt in 1 2 3; do
    start_expected_checksum_failure "$FIXTURE_DIR/failure-${attempt}.log" "$service_port"
    snapshot "$FIXTURE_DIR/after-failure-${attempt}.snapshot"
    cmp -s "$FIXTURE_DIR/before-failure.snapshot" "$FIXTURE_DIR/after-failure-${attempt}.snapshot" || fail "failed migration attempt $attempt left partial state"
    REDIS_KEYS=$(docker exec "$REDIS_CONTAINER" redis-cli --scan --pattern "${PREFIX}:*" | wc -l | tr -d ' ')
    record_snapshot "$version" "$scenario" "$round" "failure-${attempt}" pass "$LAST_ELAPSED_MS" "$FIXTURE_DIR/after-failure-${attempt}.snapshot" 0
  done

  pg_sql -c "DELETE FROM schema_migrations WHERE version='credential-storage-revision-v1'" >/dev/null
  start_ready "$CURRENT_BINARY" "$FIXTURE_DIR/recovery.log" "$service_port"
  local recovery_elapsed=$LAST_ELAPSED_MS
  stop_ready
  assert_current_state normal
  snapshot "$FIXTURE_DIR/after-recovery.snapshot"
  semantic_snapshot "$FIXTURE_DIR/after-recovery.snapshot" "$FIXTURE_DIR/after-recovery.semantic"
  local inline_before
  inline_before=$(pg_sql -At -c "SELECT applied_at::text FROM schema_migrations WHERE version='inline-schema'")

  start_ready "$CURRENT_BINARY" "$FIXTURE_DIR/recovery-repeat.log" "$service_port"
  local repeat_elapsed=$LAST_ELAPSED_MS
  stop_ready
  assert_current_state normal
  snapshot "$FIXTURE_DIR/after-recovery-repeat.snapshot"
  semantic_snapshot "$FIXTURE_DIR/after-recovery-repeat.snapshot" "$FIXTURE_DIR/after-recovery-repeat.semantic"
  cmp -s "$FIXTURE_DIR/after-recovery.semantic" "$FIXTURE_DIR/after-recovery-repeat.semantic" || fail "recovery repeat changed schema or business data"
  local inline_after metadata_churn=0
  inline_after=$(pg_sql -At -c "SELECT applied_at::text FROM schema_migrations WHERE version='inline-schema'")
  if [[ "$inline_before" != "$inline_after" ]]; then
    metadata_churn=1
    fail "recovery repeat refreshed inline-schema.applied_at without a checksum change"
  fi
  record_snapshot "$version" "$scenario" "$round" recovery pass "$recovery_elapsed" "$FIXTURE_DIR/after-recovery.snapshot" "$metadata_churn"
  record_snapshot "$version" "$scenario" "$round" recovery-repeat pass "$repeat_elapsed" "$FIXTURE_DIR/after-recovery-repeat.snapshot" "$metadata_churn"
}

require_file "$CURRENT_BINARY"
for version in $VERSIONS; do
  old_binary="$SMOKE_ROOT/bin/kiro-rs-${version}"
  require_file "$old_binary"
  case "$version" in
    v101) service_port=19131 ;;
    v102) service_port=19132 ;;
    v103) service_port=19133 ;;
    *) fail "unsupported fixture version: $version" ;;
  esac
  lsof -nP -iTCP:"$service_port" -sTCP:LISTEN >/dev/null 2>&1 && fail "service port is already in use: $service_port"
  create_infra "$version"
  for round in $(seq 1 "$ROUNDS"); do
    run_success_round "$version" normal "$round" "$service_port" "$old_binary"
    run_success_round "$version" large "$round" "$service_port" "$old_binary"
    run_failure_round "$version" "$round" "$service_port" "$old_binary"
  done
  cleanup_infra
done

printf 'PASS: results=%s\n' "$SUMMARY"
