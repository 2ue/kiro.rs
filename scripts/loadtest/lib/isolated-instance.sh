#!/usr/bin/env bash
# Shared lifecycle for disposable load-test instances. Source from the repository scripts.
REPO=${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}
BIN=${BIN:-/tmp/kiro-rs-rust-analyzer-target/release/claude-pool}
PG_CONTAINER=${PG_CONTAINER:-kiro-rs-postgres-local}
REDIS_CONTAINER=${REDIS_CONTAINER:-kiro-rs-redis-local}
PG_USER=${PG_USER:-kiro_rs}
PG_PORT=${PG_PORT:-25432}
REDIS_PORT=${REDIS_PORT:-26379}
PORT=${PORT:-19381}
MOCK_PORT=${MOCK_PORT:-39181}
REDIS_DB=${REDIS_DB:-13}
KEY_PREFIX=${KEY_PREFIX:-pool-overload-probe}
DB=${DB:-claude_pool_acceptance}
NODE=${NODE:-node}
# Version-manager shims can fork a child; launch the actual Node binary so $! owns the mock.
NODE=$($NODE -p 'process.execPath')
ACCOUNTS=${ACCOUNTS:-10}
PER_CONC=${PER_CONC:-4}
# FD leak caps for the scenario verdict: upstream sockets can never exceed the slots that were
# concurrently in flight; database sockets are bounded by the pools in the generated config.json
# (postgres maxConnections=10, usage pool default 6) plus the Redis client and its three
# connection managers / pub-sub (8 with slack).
UPSTREAM_SOCKET_CAP=${UPSTREAM_SOCKET_CAP:-$((ACCOUNTS * PER_CONC))}
DB_SOCKET_CAP=${DB_SOCKET_CAP:-$((10 + 6 + 8))}
PER_RPM=0
QUEUE=${QUEUE:-200}
WAIT_SECS=${WAIT_SECS:-60}
GLOBAL_MAX=${GLOBAL_MAX:-1000}
IDLE_SECS=${IDLE_SECS:-120}
# 首输出超时要明显短于客户端 120s 超时，否则挂起流的换号与客户端放弃撞在同一刻。
FIRST_OUTPUT_SECS=${FIRST_OUTPUT_SECS:-30}
MAX_DURATION_SECS=${MAX_DURATION_SECS:-3600}
SERVER_PID=${SERVER_PID:-}
MOCK_PID=${MOCK_PID:-}
INSTANCE_CREATED=0

admin_api() {
  curl --fail-with-body -sS --max-time 30 -H "x-api-key: $ADMIN_KEY" \
    -H 'content-type: application/json' "http://127.0.0.1:$PORT/api/admin/$@"
}

wait_url() {
  local url=$1 pid=$2
  for _ in $(seq 1 120); do
    kill -0 "$pid" 2>/dev/null || return 1
    curl -fsS --max-time 1 "$url" >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "did not become ready: $url" >&2
  return 1
}

seed_accounts() {
  local case_name=${1:-normal}
  echo 'PER_RPM=0: the account scheduler has no per-account RPM limit.'
  python3 - "$D" "$ACCOUNTS" "$PER_CONC" "$MOCK_PORT" "$case_name" <<'PY'
import json, sys
from pathlib import Path
d, count, spec, port, case = sys.argv[1:]
lo, _, hi = spec.partition('-'); lo, hi = int(lo), int(hi or lo)
limits = {}
for i in range(1, int(count) + 1):
    token = f'mock-access-{7000+i}'
    conc = lo + (i-1) % (hi-lo+1)
    limits[token] = {'conc': conc, 'rpm': 0}
    account = dict(name=token, baseUrl=f'http://127.0.0.1:{port}', apiKey=token,
                   authType='x_api_key', enabled=True, maxConcurrentRequests=conc,
                   priority=(0 if i <= int(count)//4 else 10) if case.startswith('tiers_') else 0,
                   usageProjectionMode='pass_through')
    Path(d, f'account-{i}.json').write_text(json.dumps(account))
Path(d, 'limits.json').write_text(json.dumps(limits))
PY
  local i
  for i in $(seq 1 "$ACCOUNTS"); do
    admin_api accounts -X POST --data-binary "@$D/account-$i.json" > "$D/account-response.json" || return 1
    rm -f "$D/account-$i.json"
  done
}

apply_dispatch_settings() {
  admin_api config/runtime > "$D/runtime.json" || return 1
  if [ "${USE_CODE_DEFAULTS:-0}" = 1 ]; then
    # 只放开下游准入（压测客户端直打），账号池调度参数保持代码默认，验证开箱配置。
    python3 - "$D" <<'PY'
import json, sys
from pathlib import Path
d = Path(sys.argv[1])
accounts = json.loads((d/'runtime.json').read_text())['accounts']
print('code defaults:', {k: accounts.get(k) for k in ('globalMaxConcurrentRequests','maxQueuedRequests','dispatchMaxWaitSecs','streamFirstOutputTimeoutSecs','streamIdleTimeoutSecs','requestTimeoutSecs')})
(d/'dispatch-settings.json').write_text(json.dumps(dict(
    requestAdmission=dict(rpm=0, maxConcurrentRequests=0, maxQueuedRequests=0, queueTimeoutMs=0))))
PY
    admin_api config/runtime -X PUT --data-binary "@$D/dispatch-settings.json" > "$D/runtime-applied.json"
    return
  fi
  python3 - "$D" "$QUEUE" "$WAIT_SECS" "$GLOBAL_MAX" "$IDLE_SECS" "$MAX_DURATION_SECS" "$FIRST_OUTPUT_SECS" <<'PY'
import json, sys
from pathlib import Path
d = Path(sys.argv[1]); queue, wait, maximum, idle, duration, first_output = map(int, sys.argv[2:])
accounts = json.loads((d/'runtime.json').read_text())['accounts']
accounts.update(enabled=True, maxQueuedRequests=queue, dispatchMaxWaitSecs=wait,
                globalMaxConcurrentRequests=maximum, capacityMode='wait',
                streamIdleTimeoutSecs=idle, requestTimeoutSecs=duration,
                streamRequestTimeoutSecs=duration, streamFirstOutputTimeoutSecs=first_output)
(d/'dispatch-settings.json').write_text(json.dumps(dict(accounts=accounts,
    requestAdmission=dict(rpm=0, maxConcurrentRequests=0, maxQueuedRequests=0, queueTimeoutMs=0))))
PY
  admin_api config/runtime -X PUT --data-binary "@$D/dispatch-settings.json" > "$D/runtime-applied.json"
}

start_instance() {
  local case_name=${1:-normal}
  [[ "$DB" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]] || { echo 'invalid isolated DB name' >&2; return 1; }
  [[ "$REDIS_DB" =~ ^[0-9]+$ ]] && [ "$REDIS_DB" -gt 0 ] || { echo 'use a dedicated nonzero Redis db' >&2; return 1; }
  mkdir -p "$D"
  chmod 700 "$D"
  if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN -t >/dev/null 2>&1 || lsof -nP -iTCP:"$MOCK_PORT" -sTCP:LISTEN -t >/dev/null 2>&1; then
    echo 'isolated instance ports already in use' >&2; return 1
  fi
  local pgpw
  pgpw=$(docker exec "$PG_CONTAINER" printenv POSTGRES_PASSWORD) || return 1
  API_KEY=${API_KEY:-sk-load-$(openssl rand -hex 12)}
  ADMIN_KEY=${ADMIN_KEY:-sk-load-admin-$(openssl rand -hex 12)}
  docker exec "$PG_CONTAINER" psql -U "$PG_USER" -d postgres -v ON_ERROR_STOP=1 \
    -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" -c "CREATE DATABASE $DB" || return 1
  INSTANCE_CREATED=1
  docker exec "$REDIS_CONTAINER" redis-cli -n "$REDIS_DB" FLUSHDB >/dev/null || return 1
  python3 - "$D" "$PG_USER" "$pgpw" "$PG_PORT" "$DB" "$PORT" "$REDIS_PORT" "$REDIS_DB" "$KEY_PREFIX" "$API_KEY" "$ADMIN_KEY" <<'PY'
import json, sys
from pathlib import Path
from urllib.parse import quote
d, user, password, pgport, db, port, rport, rdb, prefix, key, admin = sys.argv[1:]
Path(d, 'config.json').write_text(json.dumps(dict(host='127.0.0.1', port=int(port), apiKey=key, adminApiKey=admin,
    postgres=dict(url=f'postgres://{quote(user, safe="")}:{quote(password, safe="")}@127.0.0.1:{pgport}/{db}', maxConnections=10),
    redis=dict(url=f'redis://127.0.0.1:{rport}/{rdb}', keyPrefix=prefix))))
PY
  chmod 600 "$D/config.json"
  cp "$BIN" "$D/claude-pool" || return 1
  PORT=$MOCK_PORT $NODE "$REPO/scripts/loadtest/anthropic-mock-upstream.mjs" > "$D/mock.log" 2>&1 &
  MOCK_PID=$!
  wait_url "http://127.0.0.1:$MOCK_PORT/__control" "$MOCK_PID" || return 1
  (cd "$D" && exec env RUST_LOG="${RUST_LOG:-info}" ./claude-pool -c config.json) > "$D/server-$case_name.log" 2>&1 &
  SERVER_PID=$!
  wait_url "http://127.0.0.1:$PORT/readyz" "$SERVER_PID" || { tail -25 "$D/server-$case_name.log"; return 1; }
  seed_accounts "$case_name" && apply_dispatch_settings || return 1
}

stop_instance() {
  local pid
  for pid in "${SERVER_PID:-}" "${MOCK_PID:-}"; do
    [ -n "$pid" ] || continue
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
    if kill -0 "$pid" 2>/dev/null; then echo "process $pid exceeded shutdown budget" >&2; kill -9 "$pid" 2>/dev/null || true; fi
    wait "$pid" 2>/dev/null || true
  done
  SERVER_PID= MOCK_PID=
}

cleanup_instance() {
  stop_instance
  if [ "${KEEP:-0}" != 1 ]; then
    if [ "$INSTANCE_CREATED" = 1 ]; then
      docker exec "$PG_CONTAINER" psql -U "$PG_USER" -d postgres -v ON_ERROR_STOP=1 -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" || return 1
      docker exec "$REDIS_CONTAINER" redis-cli -n "$REDIS_DB" FLUSHDB >/dev/null || return 1
    fi
    rm -rf "$D"
  fi
}
