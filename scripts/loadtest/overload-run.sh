#!/usr/bin/env bash
# Overload reproduction runs on an isolated kiro-rs with mock upstreams.
#
# Each case starts kiro-rs on a fresh copy of the template database and an empty Redis db,
# seeds ACCOUNTS mock accounts, applies production-like dispatch settings (queue 200, 60 s wait,
# global 1000, priority mode, no per-key admission), sets the mock upstream shape, runs
# overload-probe.mjs with the case profile, then stops everything.
#
# Usage: BIN=/path/to/kiro-rs CASES="steady_ok spike_sub2api" scripts/loadtest/overload-run.sh
# Env: ACCOUNTS (10), PER_CONC (4, or a range like 3-5), PER_RPM (0 = unlimited, or a range like
#      20-30), QUEUE (200), WAIT_SECS (60), OUT_DIR (/tmp/overload-e2e/results), KEEP=1 to keep
#      the database after the run. A range spreads the limit over the accounts in turn
#      (3,4,5,3,...), so the pool mixes account sizes like production. MAX_SOCKETS (300) caps
#      the client's connections. GLOBAL_MAX (1000) is dispatchGlobalMaxConcurrentRequests;
#      IDLE_SECS (120), MAX_DURATION_SECS (3600) and LEASE_MAX_SECS (900) are the stream idle
#      timeout, the response duration cap and the in-flight lease max age.
set -uo pipefail

D=${D:-/tmp/overload-e2e}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
BIN=${BIN:-/tmp/kiro-rs-rust-analyzer-target/release/kiro-rs}
DB=${DB:-kiro_overload_probe}
PORT=${PORT:-19381}
MOCK_PORT=${MOCK_PORT:-39181}
REDIS_DB=${REDIS_DB:-13}
# A small pool by default: the case table expresses load relative to capacity, so 10 x 4 slots
# reproduce the same overload ratios as production's 57 x 4 with a fraction of the connections.
ACCOUNTS=${ACCOUNTS:-10}
PER_CONC=${PER_CONC:-4}
PER_RPM=${PER_RPM:-0}
QUEUE=${QUEUE:-200}
WAIT_SECS=${WAIT_SECS:-60}
GLOBAL_MAX=${GLOBAL_MAX:-1000}
IDLE_SECS=${IDLE_SECS:-120}
MAX_DURATION_SECS=${MAX_DURATION_SECS:-3600}
LEASE_MAX_SECS=${LEASE_MAX_SECS:-900}
CASES=${CASES:-steady_ok}
OUT_DIR=${OUT_DIR:-$D/results}
PGPW=$(docker exec kiro-rs-postgres-local printenv POSTGRES_PASSWORD)
NODE=${NODE:-"/Users/yuanfeijie/.volta/bin/volta run --node 22.23.0 node"}
mkdir -p "$D" "$OUT_DIR" && chmod 700 "$D"
cp "$BIN" "$D/kiro-rs"

stop_all() {
  pkill -f "kiro-mock-upstream.mjs --overload-probe" 2>/dev/null || true
  pkill -f "overload-probe.mjs" 2>/dev/null || true
  local pid
  pid=$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t 2>/dev/null | head -1)
  [ -n "$pid" ] && kill "$pid" 2>/dev/null
  for _ in $(seq 1 60); do [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && { echo "kiro-rs did not exit in 30 s, killing"; kill -9 "$pid"; }
  return 0
}

wait_port() {
  for _ in $(seq 1 120); do lsof -nP -iTCP:$1 -sTCP:LISTEN >/dev/null 2>&1 && return 0; sleep 0.5; done
  echo "port $1 did not open"; return 1
}

prepare() {
  docker exec kiro-rs-postgres-local psql -U kiro_rs -d postgres -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" -c "CREATE DATABASE $DB TEMPLATE kiro_identity_e2e" 2>&1 | grep -v NOTICE
  docker exec kiro-rs-redis-local redis-cli -n $REDIS_DB FLUSHDB >/dev/null
  python3 - "$D" "$PGPW" "$ACCOUNTS" "$PER_CONC" "$PER_RPM" "$QUEUE" "$WAIT_SECS" "$PORT" "$MOCK_PORT" "$DB" "$REDIS_DB" "$1" "$GLOBAL_MAX" "$IDLE_SECS" "$MAX_DURATION_SECS" "$LEASE_MAX_SECS" <<'PY'
import json, hashlib, secrets, datetime, sys
d, pgpw, accounts, per_conc, per_rpm, queue, wait_secs, port, mock_port, db, redis_db, case = sys.argv[1:13]
global_max, idle_secs, max_duration_secs, lease_max_secs = (int(v) for v in sys.argv[13:17])
accounts = int(accounts)
def spread(spec, i):
    # "3-5" -> 3,4,5,3,4,5,... over the accounts; "4" -> 4 for every account.
    low, _, high = spec.partition("-")
    low = int(low); high = int(high or low)
    return low + (i - 1) % (high - low + 1)
limits = {}
keys = open(f"{d}/keys").read().split()
api_key, admin_key = keys[0], keys[1]
exp = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=6)).isoformat()
q = lambda v: "'" + str(v).replace("'", "''") + "'"
sql = ["BEGIN;", "UPDATE external_upstream_pools SET deleted_at=now(), enabled=false WHERE deleted_at IS NULL;",
       "DELETE FROM credential_runtime_state;", "DELETE FROM credential_account_info;", "DELETE FROM credentials;", "DELETE FROM proxy_resources;"]
for i in range(1, accounts + 1):
    cid = 7000 + i
    # Production has a priority-0 tier and a priority-10 tier; "tiers" cases model that.
    priority = (0 if i <= accounts // 4 else 10) if case.startswith("tiers") else 0
    conc, rpm = spread(per_conc, i), spread(per_rpm, i)
    limits[f"mock-access-{cid}"] = {"conc": conc, "rpm": rpm}
    rt = "mock-refresh-" + secrets.token_hex(16)
    c = {"id": cid, "accessToken": f"mock-access-{cid}", "refreshToken": rt,
         "profileArn": f"arn:aws:codewhisperer:us-east-1:111111111111:profile/MOCK{cid}",
         "expiresAt": exp, "authMethod": "social", "region": "us-east-1", "priority": priority, "disabled": False,
         "rateLimitAutoDisableEnabled": False, "maxConcurrentRequests": conc}
    if rpm > 0:
        c["rpm"] = rpm
    sql.append(f"INSERT INTO credentials (id,priority,disabled,auth_kind,refresh_token_hash,data,created_at,updated_at,revision) VALUES ({cid},{priority},false,'social',{q(hashlib.sha256(rt.encode()).hexdigest())},{q(json.dumps(c))}::jsonb,now(),now(),1);")
    sql.append(f"INSERT INTO credential_runtime_state (credential_id,failure_count,refresh_failure_count,disabled_reason,warmup_remaining,generation,revision,updated_at) VALUES ({cid},0,0,NULL,0,0,1,now());")
sql.append("COMMIT;")
json.dump(limits, open(f"{d}/limits.json", "w"))
max_conc = max(v["conc"] for v in limits.values())
open(f"{d}/seed.sql", "w").write("\n".join(sql) + "\n")
patch = {"port": int(port), "host": "127.0.0.1", "apiKey": api_key, "adminApiKey": admin_key,
         "tokenRefreshBackgroundEnabled": False, "kiroUpstreamBaseUrl": f"http://127.0.0.1:{mock_port}",
         "loadBalancingMode": "priority", "credentialMaxConcurrentRequests": max_conc,
         "credentialRpm": None,
         "credentialWarmupRequests": 0, "dispatchMaxQueuedRequests": int(queue),
         "dispatchGlobalMaxConcurrentRequests": global_max, "credentialDispatchMaxWaitSecs": int(wait_secs),
         "kiroUpstreamResponseTimeoutSecs": 300, "kiroUpstreamStreamIdleTimeoutSecs": idle_secs,
         "kiroUpstreamStreamMaxDurationSecs": max_duration_secs, "credentialInFlightLeaseMaxSecs": lease_max_secs,
         "requestAdmission": {"rpm": 0, "maxConcurrentRequests": 0, "maxQueuedRequests": 0, "queueTimeoutMs": 0},
         "externalPools": {"externalPoolsEnabled": False, "localPoolCircuitEnabled": False}}
open(f"{d}/patch.json", "w").write(json.dumps(patch))
json.dump({"host": "127.0.0.1", "port": int(port), "apiKey": api_key, "adminApiKey": admin_key,
           "postgres": {"url": f"postgres://kiro_rs:{pgpw}@127.0.0.1:25432/{db}", "maxConnections": 10},
           "redis": {"url": f"redis://127.0.0.1:26379/{redis_db}", "keyPrefix": "kiro-overload-probe"}},
          open(f"{d}/config.json", "w"))
json.dump([], open(f"{d}/credentials.json", "w"))
PY
  chmod 600 "$D"/*.json "$D/seed.sql"
  docker exec -i kiro-rs-postgres-local psql -U kiro_rs -d $DB -v ON_ERROR_STOP=1 -q < "$D/seed.sql" || return 1
  docker exec kiro-rs-postgres-local psql -U kiro_rs -d $DB -qAt -c "select config::text from runtime_config where id='default'" > "$D/base-config.json"
  python3 - "$D" <<'PY'
import json, sys
d = sys.argv[1]
cfg = json.load(open(f"{d}/base-config.json")); patch = json.load(open(f"{d}/patch.json"))
for k, v in patch.items():
    if k == "externalPools":
        ep = dict(cfg.get("externalPools") or {}); ep.update(v); cfg["externalPools"] = ep
    elif v is None:
        cfg.pop(k, None)
    else:
        cfg[k] = v
open(f"{d}/apply.sql", "w").write("update runtime_config set config = $cfg$" + json.dumps(cfg) + "$cfg$::jsonb where id='default';\n")
PY
  docker exec -i kiro-rs-postgres-local psql -U kiro_rs -d $DB -v ON_ERROR_STOP=1 -q < "$D/apply.sql"
  rm -f "$D/seed.sql" "$D/apply.sql" "$D/base-config.json" "$D/patch.json"
}

# Case table: mock upstream shape | probe profile | extra probe flags.
# Rates are arrivals per second. With 57 accounts x 4 = 228 slots, a 5 s stream holds a slot 5 s,
# so ~45 arrivals/s fills the pool; production ran 2.5-3.5 arrivals/s with 5-60 s streams.
case_def() {
  case "$1" in
    # Normal load that fits: everything must succeed, no queueing.
    steady_ok)        echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:90|--retry sub2api' ;;
    # Slow ramp from fitting to 2x capacity and back.
    ramp_sub2api)     echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|ramp:10:90:120,steady:90:60,ramp:90:10:60,steady:10:60|--retry sub2api' ;;
    # Production shape: steady, sudden spike far over capacity, back to steady.
    spike_sub2api)    echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:45,steady:150:40,steady:20:120|--retry sub2api' ;;
    spike_honor)      echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:45,steady:150:40,steady:20:120|--retry honor' ;;
    spike_noretry)    echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:45,steady:150:40,steady:20:120|--retry none' ;;
    # Long streams (30-60 s) like production opus traffic: few arrivals, slots held long.
    long_spike)       echo '{"headerDelayMs":2000,"jitterMs":3000,"streamChunks":200,"chunkDelayMs":200}|steady:3:60,steady:15:40,steady:3:150|--retry sub2api --stream-share 1' ;;
    # Repeated bursts: overload, recover, overload.
    square_sub2api)   echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|square:15:120:20:160,steady:15:60|--retry sub2api' ;;
    # Sustained 3x overload for minutes.
    sustained_sub2api) echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:30,steady:140:180,steady:20:120|--retry sub2api' ;;
    # Large request bodies (256 KB) on top of the spike: parse cost on the reject path.
    bigbody_spike)    echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:45,steady:150:40,steady:20:120|--retry sub2api --body-kb 256' ;;
    # Sticky sessions concentrated on 40 conversations.
    sticky_spike)     echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:45,steady:150:40,steady:20:120|--retry sub2api --sessions 40' ;;
    # Priority tiers: a quarter at priority 0, the rest at 10.
    tiers_spike)      echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:45,steady:150:40,steady:20:120|--retry sub2api' ;;
    # All accounts fail with 5xx for 30 s under normal load, then recover: must resume at once, no account left cooled down.
    storm_5xx)        echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:20:30,steady:20:60|--retry sub2api|[null,{"errorRate":1,"errorStatus":500},{"errorRate":0}]' ;;
    # Upstream throttles every account (429) for 30 s.
    storm_429)        echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:20:30,steady:20:60|--retry sub2api|[null,{"errorRate":1,"errorStatus":429},{"errorRate":0}]' ;;
    # Upstream throttles every account for 30 s without a Retry-After header: local cooldowns
    # fall back to their own back-off and must not keep the pool idle long after it recovers.
    storm_429_noretry) echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:20:30,steady:20:90|--retry sub2api|[null,{"errorRate":1,"errorStatus":429,"retryAfter":null},{"errorRate":0}]' ;;
    # Connections reset before the first byte on every account for 30 s.
    storm_reset)      echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:20:30,steady:20:60|--retry sub2api|[null,{"errorRate":1,"errorStatus":"reset"},{"errorRate":0}]' ;;
    # Half of the streams are cut mid-way for 30 s.
    storm_midstream)  echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:20:30,steady:20:60|--retry sub2api|[null,{"midStreamResetRate":0.5},{"midStreamResetRate":0}]' ;;
    # Upstream errors (30% 5xx) during an overload spike: errors and overload at the same time.
    spike_with_errors) echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:150:40,steady:20:90|--retry sub2api|[null,{"errorRate":0.3,"errorStatus":500},{"errorRate":0}]' ;;
    # Upstream turns very slow (header 20-40 s) for 60 s while loaded: slots held long, queue times out.
    slow_wave)        echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:20:60,steady:20:90|--retry sub2api|[null,{"headerDelayMs":20000,"jitterMs":20000},{"headerDelayMs":800,"jitterMs":1500}]' ;;
    # A third of the accounts become very slow; the rest must carry the load without stalls.
    slow_accounts_spike) echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:30,steady:100:40,steady:20:60|--retry sub2api|[null,{"slowTokens":["mock-access-7001","mock-access-7002","mock-access-7003"],"slowExtraMs":30000},{"slowTokens":[]}]' ;;
    # Spike far above the queue limit with a tiny queue (QUEUE env); the cheap rejection path must hold.
    queue_overflow)   echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:20,steady:300:40,steady:20:60|--retry sub2api' ;;
    # Overload with clients giving up after 10 s (cancels in the queue and mid-stream).
    spike_impatient)  echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:20:20,steady:150:40,steady:20:60|--retry sub2api --client-timeout-ms 10000' ;;
    # Sustained 2x overload for 4 minutes, then a long quiet period: memory and queues must come back.
    long_overload)    echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0,"slowTokens":[],"failTokens":[],"midStreamResetRate":0}|steady:90:240,steady:10:90|--retry sub2api' ;;
    # Short probe for connection-level errors during a spike.
    conn_probe)       echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:20:20,steady:150:30,steady:20:30|--retry sub2api' ;;
    # Close to full but fitting (about 85% of the slots): everything must succeed, no queueing.
    near_full)        echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:38:120|--retry sub2api' ;;
    # Per-account RPM is the bottleneck (run with PER_RPM=20): short calls, sub2api retries.
    rpm_bound)        echo '{"headerDelayMs":300,"jitterMs":300,"streamChunks":0,"chunkDelayMs":0}|steady:10:30,steady:60:60,steady:10:60|--retry sub2api --stream-share 0.3' ;;
    # Upstream slows down 5x while loaded (production 10-29 trigger).
    slow_upstream)    echo '{"headerDelayMs":4000,"jitterMs":6000,"streamChunks":30,"chunkDelayMs":300}|steady:20:180|--retry sub2api' ;;
    # ---- Large pools with mixed limits (run with PER_CONC=3-5 PER_RPM=20-30). Rates are per
    # 228 slots like the rest of the table. With 5 s calls and 20-30 RPM per account the RPM
    # windows bind first: about 24 arrivals/s per 228 slots use every account's RPM.
    # Normal load inside the RPM budget (60%): everything succeeds, nothing waits.
    pool_steady)      echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:14:120|--retry sub2api' ;;
    # 1.5x the RPM budget for 60 s, then back: waiting requests must get every RPM slot that
    # frees up, and the queue must drain afterwards.
    pool_rpm_spike)   echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:14:30,steady:36:60,steady:14:120|--retry sub2api' ;;
    # 2x the RPM budget held for 3 minutes (a long queue), then a quiet period.
    pool_rpm_long)    echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:14:20,steady:30:180,steady:5:90|--retry sub2api' ;;
    # Quiet, then a sudden burst at 4x the RPM budget for 20 s.
    pool_burst)       echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:5:30,steady:60:20,steady:5:120|--retry sub2api' ;;
    # Normal, then every account fails (5xx) for 30 s, then sudden recovery.
    pool_5xx_recover) echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0}|steady:14:30,steady:14:30,steady:14:90|--retry sub2api|[null,{"errorRate":1,"errorStatus":500},{"errorRate":0}]' ;;
    # Same with upstream 429 and no Retry-After: cooldowns must not keep the pool idle after.
    rpm_storm_429)    echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0}|steady:14:30,steady:14:30,steady:14:90|--retry sub2api|[null,{"errorRate":1,"errorStatus":429,"retryAfter":null},{"errorRate":0}]' ;;
    # The queue is full (2x RPM budget) when upstream starts resetting connections for 20 s.
    pool_queue_fault) echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"errorRate":0}|steady:30:30,steady:30:20,steady:30:30,steady:14:90|--retry sub2api|[null,{"errorRate":1,"errorStatus":"reset"},{"errorRate":0}]' ;;
    # Queue under overload with clients that give up after 10 s.
    pool_queue_impatient) echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100}|steady:14:20,steady:36:60,steady:14:90|--retry sub2api --client-timeout-ms 10000' ;;
    # Every call is a 60 s stream (one chunk per 5 s): concurrency binds, not RPM. Half the
    # slots, then 2x arrivals so every slot is held by a long stream and requests queue.
    pool_long_full)   echo '{"headerDelayMs":800,"jitterMs":0,"streamChunks":30,"chunkDelayMs":100,"trickleRate":1,"trickleChunks":13,"trickleIntervalMs":5000}|steady:1.9:60,steady:7.6:120,steady:1.9:150|--retry sub2api --stream-share 1 --drain-secs 240' ;;
    # One call in twenty hangs after its first chunk until kiro-rs gives up on it (stream idle
    # timeout, 120 s here): about a third of the slots end up hung, the rest keeps being served,
    # and the hung slots come back.
    pool_hang)        echo '{"headerDelayMs":800,"jitterMs":1500,"streamChunks":30,"chunkDelayMs":100,"hangRate":0}|steady:14:60,steady:14:200|--retry sub2api --stream-share 1 --drain-secs 200|[{"hangRate":0.05},{"hangRate":0}]' ;;
    # Every call trickles one chunk per 20 s for 10 minutes (under the idle timeout, never
    # finishing). Run with MAX_DURATION_SECS=60: slots must come back at the cap, queued
    # requests must get them.
    pool_trickle)     echo '{"headerDelayMs":800,"jitterMs":0,"streamChunks":30,"chunkDelayMs":100,"trickleRate":0,"trickleChunks":30,"trickleIntervalMs":20000}|steady:7.6:60,steady:3.8:120|--retry sub2api --stream-share 1 --drain-secs 120|[{"trickleRate":0.5},{"trickleRate":0}]' ;;
    *) echo "" ;;
  esac
}

[ -f "$D/keys" ] || { printf 'sk-ovl-%s\nsk-ovl-admin-%s\n' "$(openssl rand -hex 12)" "$(openssl rand -hex 12)" > "$D/keys"; chmod 600 "$D/keys"; }
API_KEY=$(sed -n 1p "$D/keys"); ADMIN_KEY=$(sed -n 2p "$D/keys")

for case_name in $CASES; do
  def=$(case_def "$case_name")
  [ -z "$def" ] && { echo "unknown case $case_name"; continue; }
  mock_json=${def%%|*}; rest=${def#*|}; profile=${rest%%|*}; rest=${rest#*|}; flags=${rest%%|*}
  plan=""; [ "$rest" != "$flags" ] && plan=${rest#*|}
  timewait=$(netstat -an -p tcp 2>/dev/null | grep -c TIME_WAIT)
  if [ "$timewait" -gt 2000 ]; then echo "TIME_WAIT $timewait > 2000, waiting before $case_name"; sleep 60; fi
  # Rates in the case table are for the production pool (57 accounts x 4 = 228 slots); scale
  # them to the pool under test so every pool size sees the same relative load.
  slots=$(python3 -c "import sys;lo,_,hi=sys.argv[2].partition('-');lo=int(lo);hi=int(hi or lo);print(sum(lo+i%(hi-lo+1) for i in range(int(sys.argv[1]))))" "$ACCOUNTS" "$PER_CONC")
  profile=$(python3 "$REPO/scripts/loadtest/scale-profile.py" "$profile" "$slots")
  printf '\n== %s  accounts=%s conc=%s rpm=%s queue=%s wait=%ss global=%s slots=%s profile=%s %s\n' "$case_name" "$ACCOUNTS" "$PER_CONC" "$PER_RPM" "$QUEUE" "$WAIT_SECS" "$GLOBAL_MAX" "$slots" "$profile" "$flags"
  stop_all
  prepare "$case_name" || { echo "prepare failed"; continue; }
  (cd "$REPO" && PORT=$MOCK_PORT nohup $NODE scripts/loadtest/kiro-mock-upstream.mjs --overload-probe > "$D/mock.log" 2>&1 < /dev/null &)
  wait_port $MOCK_PORT || continue
  curl -sS -m 5 -X POST "http://127.0.0.1:$MOCK_PORT/__control" -H 'content-type: application/json' \
    -d "$(echo "$mock_json" | python3 -c 'import json,sys;d=json.load(sys.stdin);d["resetCounters"]=True;print(json.dumps(d))')" >/dev/null
  (cd "$D" && RUST_LOG=info nohup ./kiro-rs -c config.json --credentials credentials.json > "$D/server-$case_name.log" 2>&1 < /dev/null &)
  wait_port $PORT || { tail -20 "$D/server-$case_name.log"; stop_all; continue; }
  sleep 3
  # shellcheck disable=SC2086
  $NODE "$REPO/scripts/loadtest/overload-probe.mjs" --base-url "http://127.0.0.1:$PORT" --api-key "$API_KEY" --admin-key "$ADMIN_KEY" \
    --kiro-mock "http://127.0.0.1:$MOCK_PORT" --accounts "$ACCOUNTS" --account-limits "$D/limits.json" \
    --profile "$profile" --max-sockets "${MAX_SOCKETS:-300}" --global-max "$GLOBAL_MAX" $flags ${plan:+--upstream-plan "$plan"} --out "$OUT_DIR/$case_name.json" 2>&1 | tail -12
  echo "load: $(uptime | awk -F'load averages: ' '{print $2}')  rss_mb: $(ps -o rss= -p "$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t | head -1)" 2>/dev/null | awk '{print int($1/1024)}')"
  echo "redis leases after: $(docker exec kiro-rs-redis-local redis-cli -n $REDIS_DB GET kiro-overload-probe:scheduler:global:inflight:weighted_count)"
  sed -E 's/\x1b\[[0-9;]*m//g' "$D/server-$case_name.log" | grep -E " (WARN|ERROR) " \
    | grep -vE "machine_id|native reasoning|capability cohort|slow interaction diagnostic" \
    | sed -E 's/^[^ ]+ +//; s/req_[A-Za-z0-9]+/req_X/g; s/request_api_key_id=[0-9a-f]+/key=X/; s/credential_id=[0-9]+/cid=N/g; s/#[0-9]+/#N/g; s/[0-9]+/N/g' \
    | cut -c1-180 | sort | uniq -c | sort -rn | head -8
  # Shutdown must finish well inside docker's 10 s grace period.
  pid=$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t 2>/dev/null | head -1)
  t0=$(python3 -c 'import time;print(time.time())')
  stop_all
  echo "shutdown_secs: $(python3 -c "import time;print(round(time.time()-$t0,1))")"
done
stop_all
if [ "${KEEP:-0}" != "1" ]; then
  docker exec kiro-rs-postgres-local psql -U kiro_rs -d postgres -qc "DROP DATABASE IF EXISTS $DB WITH (FORCE)" 2>&1 | grep -v NOTICE
  docker exec kiro-rs-redis-local redis-cli -n $REDIS_DB FLUSHDB >/dev/null
  rm -f "$D"/config.json "$D"/credentials.json "$D/kiro-rs" "$D/limits.json"
fi
