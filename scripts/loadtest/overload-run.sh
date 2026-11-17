#!/usr/bin/env bash
# Overload reproduction runs on an isolated claude-pool with mock upstreams.
#
# Each case starts claude-pool on a fresh empty database and an empty Redis db,
# seeds ACCOUNTS mock accounts, applies production-like dispatch settings (queue 200, 60 s wait,
# global 1000, priority mode, no per-key admission), sets the mock upstream shape, runs
# overload-probe.mjs with the case profile, then stops everything.
#
# Usage: BIN=/path/to/claude-pool CASES="steady_ok spike_sub2api" scripts/loadtest/overload-run.sh
# Env: ACCOUNTS (10), PER_CONC (4, or a range like 3-5), PER_RPM (ignored: accounts have no RPM limit), QUEUE (200), WAIT_SECS (60), OUT_DIR (/tmp/overload-e2e/results), KEEP=1 to keep
#      the database after the run. A range spreads the limit over the accounts in turn
#      (3,4,5,3,...), so the pool mixes account sizes like production. MAX_SOCKETS (300) caps
#      the client's connections. GLOBAL_MAX (1000) is accounts.globalMaxConcurrentRequests;
#      IDLE_SECS (120), MAX_DURATION_SECS (3600) are the stream idle
#      timeout and the response duration cap.
set -uo pipefail
REPO=$(cd "$(dirname "$0")/../.." && pwd)
D=${D:-$(mktemp -d /tmp/overload-instance.XXXXXX)}
OUT_DIR=${OUT_DIR:-/tmp/overload-e2e/results}
CASES=${CASES:-steady_ok}
# shellcheck source=lib/isolated-instance.sh
source "$REPO/scripts/loadtest/lib/isolated-instance.sh"
mkdir -p "$OUT_DIR"
trap '[ "${REUSE_INSTANCE:-0}" = 1 ] || cleanup_instance' EXIT
trap 'exit 130' INT TERM
FAILED=0

prepare() {
  if [ "${REUSE_INSTANCE:-0}" = 1 ]; then
    return 0
  fi
  stop_instance
  start_instance "$1"
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
    # One call in twenty hangs after its first chunk until claude-pool gives up on it (stream idle
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

exec 3>&1 4>&2
for case_name in $CASES; do
  exec 1>&3 2>&4
  exec > >(tee "$OUT_DIR/$case_name.log") 2>&1
  case "$case_name" in
    rpm_bound|pool_rpm_*)
      echo "$case_name: not_applicable (no per-account RPM)"
      printf '{"case":"%s","verdict":"not_applicable"}\n' "$case_name" > "$OUT_DIR/$case_name.json"
      continue ;;
  esac
  def=$(case_def "$case_name")
  [ -z "$def" ] && { echo "unknown case $case_name"; FAILED=1; continue; }
  mock_json=${def%%|*}; rest=${def#*|}; profile=${rest%%|*}; rest=${rest#*|}; flags=${rest%%|*}
  plan=""; [ "$rest" != "$flags" ] && plan=${rest#*|}
  # TIME_WAIT holding ephemeral ports (see overload-probe.mjs).
  # The host runs other work. Above this load the mock and claude-pool both run seconds late and a
  # case measures the host, not the scheduler: wait (up to 20 min) for it to settle.
  for _ in $(seq 1 40); do
    load=$(uptime | awk -F'load averages: ' '{print $2}' | awk '{print int($1)}')
    [ "$load" -le "${MAX_LOAD:-60}" ] && break
    echo "host load $load > ${MAX_LOAD:-60}, waiting before $case_name"; sleep 30
  done
  timewait=$(netstat -an -p tcp 2>/dev/null | awk '$6=="TIME_WAIT"{n=split($4,a,"."); if (a[n]+0>=49152) c++} END{print c+0}')
  if [ "$timewait" -gt 2000 ]; then echo "TIME_WAIT $timewait > 2000, waiting before $case_name"; sleep 60; fi
  # Rates in the case table are for the production pool (57 accounts x 4 = 228 slots); scale
  # them to the pool under test so every pool size sees the same relative load.
  slots=$(python3 -c "import sys;lo,_,hi=sys.argv[2].partition('-');lo=int(lo);hi=int(hi or lo);print(sum(lo+i%(hi-lo+1) for i in range(int(sys.argv[1]))))" "$ACCOUNTS" "$PER_CONC")
  profile=$(python3 "$REPO/scripts/loadtest/scale-profile.py" "$profile" "$slots")
  profile=${PROFILE_OVERRIDE:-$profile}
  printf '\n== %s  accounts=%s conc=%s rpm=%s queue=%s wait=%ss global=%s slots=%s profile=%s %s\n' "$case_name" "$ACCOUNTS" "$PER_CONC" "$PER_RPM" "$QUEUE" "$WAIT_SECS" "$GLOBAL_MAX" "$slots" "$profile" "$flags"
  prepare "$case_name" || { echo "prepare failed"; FAILED=1; continue; }
  curl --fail-with-body -sS -m 5 -X POST "http://127.0.0.1:$MOCK_PORT/__control" -H 'content-type: application/json' \
    -d "$(echo "$mock_json" | python3 -c 'import json,sys;d=json.load(sys.stdin);d={"errorRate":0,"failTokens":[],"slowTokens":[],"hangRate":0,"trickleRate":0,"midStreamResetRate":0,**d};d["resetCounters"]=True;print(json.dumps(d))')" >/dev/null || { FAILED=1; continue; }
  # Process CPU and RSS of the server for the whole case: a fault must not pin CPU, and the
  # pool must come back to idle CPU after it. One line per second: epoch utime stime rss_kb.
  (
    pid=$SERVER_PID
    [ -z "$pid" ] && exit 0
    while kill -0 "$pid" 2>/dev/null; do
      # macOS 没有 /proc；ps 的 utime/stime 是「[[dd-]hh:]mm:ss.ff」，统一转成秒。
      read -r u s rss < <(ps -o utime=,stime=,rss= -p "$pid" 2>/dev/null)
      secs() { python3 -c 'import sys;v=sys.argv[1].split(":");t=0.0
for p in v: t=t*60+float(p)
print(t)' "$1" 2>/dev/null || echo 0; }
      echo "$(date +%s) $(secs "${u:-0}") $(secs "${s:-0}") ${rss:-0}"
      sleep 1
    done
  ) > "$OUT_DIR/$case_name.cpu.txt" 2>/dev/null &
  SAMPLER=$!
  sleep 3
  # shellcheck disable=SC2086
  $NODE "$REPO/scripts/loadtest/overload-probe.mjs" --base-url "http://127.0.0.1:$PORT" --api-key "$API_KEY" --admin-key "$ADMIN_KEY" \
    --pool-mock "http://127.0.0.1:$MOCK_PORT" --accounts "$ACCOUNTS" --account-limits "$D/limits.json" \
    --profile "$profile" --max-sockets "${MAX_SOCKETS:-300}" --global-max "$GLOBAL_MAX" --target-pid "$SERVER_PID" $flags ${plan:+--upstream-plan "$plan"} --out "$OUT_DIR/$case_name.json" 2>&1 | tail -12
  [ "${PIPESTATUS[0]}" = 0 ] || FAILED=1
  echo "load: $(uptime | awk -F'load averages: ' '{print $2}')  rss_mb: $(ps -o rss= -p "$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t | head -1)" 2>/dev/null | awk '{print int($1/1024)}')"
  kill "$SAMPLER" 2>/dev/null || true
  python3 - "$OUT_DIR/$case_name.cpu.txt" <<'PY'
import sys
rows = [l.split() for l in open(sys.argv[1]) if l.strip()]
if len(rows) > 2:
    t0 = int(rows[0][0])
    prev = [float(rows[0][1]), float(rows[0][2])]
    cpu = []
    for row in rows[1:]:
        t, u, s = int(row[0]), float(row[1]), float(row[2])
        if t > t0:
            cpu.append((u - prev[0] + s - prev[1]) * 100.0 / (t - t0))
        prev = [u, s]
        t0 = t
    rss = [int(row[3]) // 1024 for row in rows]
    cpu.sort()
    n = len(cpu)
    print(f"cpu_total_pct: p50={cpu[n//2]:.0f} p95={cpu[int(n*0.95) if int(n*0.95) < n else n-1]:.0f} max={cpu[-1]:.0f} "
          f"secs_over_400={sum(1 for c in cpu if c > 400)}/{n} rss_end_mb={rss[-1]} rss_peak_mb={max(rss)}")
PY
  echo "redis leases after: $(docker exec "$REDIS_CONTAINER" redis-cli -n "$REDIS_DB" ZCARD "$KEY_PREFIX:account:global:inflight:last_seen")"
  sed -E 's/\x1b\[[0-9;]*m//g' "${SERVER_LOG:-$D/server-$case_name.log}" | grep -E " (WARN|ERROR) " \
    | grep -vE "machine_id|native reasoning|capability cohort|slow interaction diagnostic" \
    | sed -E 's/^[^ ]+ +//; s/req_[A-Za-z0-9]+/req_X/g; s/request_api_key_id=[0-9a-f]+/key=X/; s/(account_id|pool_id)=[0-9]+/account=N/g; s/#[0-9]+/#N/g; s/[0-9]+/N/g' \
    | cut -c1-180 | sort | uniq -c | sort -rn | head -8
  # Shutdown must finish well inside docker's 10 s grace period.
  pid=$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t 2>/dev/null | head -1)
  t0=$(python3 -c 'import time;print(time.time())')
  [ "${REUSE_INSTANCE:-0}" = 1 ] || stop_instance
  echo "shutdown_secs: $(python3 -c "import time;print(round(time.time()-$t0,1))")"
done
exit "$FAILED"
