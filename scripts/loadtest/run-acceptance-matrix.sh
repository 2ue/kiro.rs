#!/usr/bin/env bash
# Usage: scripts/loadtest/run-acceptance-matrix.sh [smoke|quick|full]
set -uo pipefail
MODE=${1:-quick}
REPO=$(cd "$(dirname "$0")/../.." && pwd)
OUT_DIR=${OUT_DIR:-/tmp/claude-pool-acceptance/$(date +%Y%m%d-%H%M%S)-$MODE}
D=${D:-$(mktemp -d /tmp/claude-pool-acceptance-instance.XXXXXX)}
DB=${DB:-claude_pool_acceptance}
PORT=${PORT:-19381}
MOCK_PORT=${MOCK_PORT:-39181}
REDIS_DB=${REDIS_DB:-13}
source "$REPO/scripts/loadtest/lib/isolated-instance.sh"
case "$MODE" in
  smoke)
    SCHEDULER_CASES='normal'
    OVERLOAD_CASES='steady_ok'
    export PROFILE_OVERRIDE=steady:5:20 ;;
  quick|full)
    SCHEDULER_CASES='normal sudden_burst sustained_overload burst_with_errors_recovering burst_with_short_outage burst_with_long_outage single_account_failing connection_resets long_stream_burst mid_stream_resets client_timeouts burst_errors_then_recover burst_errors_never_recover sudden_burst_sustained'
    OVERLOAD_CASES='steady_ok spike_sub2api storm_5xx storm_429 queue_overflow'
    if [ "$MODE" = full ]; then
      SCHEDULER_CASES="$SCHEDULER_CASES long_stream_sessions mixed_chaos_soak"
      OVERLOAD_CASES="$OVERLOAD_CASES sustained_sub2api long_spike square_sub2api storm_reset storm_midstream spike_with_errors slow_wave slow_accounts_spike spike_impatient long_overload pool_hang pool_trickle"
    fi ;;
  *) echo "Usage: $0 [smoke|quick|full]" >&2; exit 2 ;;
esac
mkdir -p "$OUT_DIR"
trap 'cleanup_instance' EXIT
trap 'exit 130' INT TERM
FAILED=0
start_instance matrix > "$OUT_DIR/instance.log" 2>&1 || { cat "$OUT_DIR/instance.log"; exit 1; }
SERVER_LOG=$D/server-matrix.log
# Assert the idle observation contract on the real endpoint before applying load.
curl -fsS "http://127.0.0.1:$PORT/readyz" > "$D/idle-ready.json" || exit 1
python3 - "$D/idle-ready.json" <<'PY'
import json,sys
d = json.load(open(sys.argv[1]))['dispatch']
assert d['queuedRequests'] == 0 and d['progressing'] is True, d
assert d['dispatchedTotal'] == 0 and d['secsSinceLastDispatch'] is None, d
PY
[ "$?" = 0 ] || exit 1
for name in $SCHEDULER_CASES; do
  echo "== scheduler $name"
  $NODE "$REPO/scripts/loadtest/scheduler-scenarios.mjs" --base-url "http://127.0.0.1:$PORT" \
    --api-key "$API_KEY" --pool-mock "http://127.0.0.1:$MOCK_PORT" --scenario "$name" \
    --target-pid "$SERVER_PID" --dispatch-max-wait-secs "$WAIT_SECS" \
    --out "$OUT_DIR/$name.json" > "$OUT_DIR/$name.log" 2>&1 || FAILED=1
  if [ ! -s "$OUT_DIR/$name.json" ]; then
    printf '{"scenario":"%s","verdict":"FAIL","problems":["场景进程未产出报告"]}\n' "$name" > "$OUT_DIR/$name.json"
  fi
  tail -3 "$OUT_DIR/$name.log"
done
for name in $OVERLOAD_CASES; do
  echo "== overload $name"
  # Preserve one service and mock for the entire matrix. Only settings and counters change.
  case "$name" in
    queue_overflow) QUEUE=8 ;;
    *) QUEUE=${MATRIX_QUEUE:-200} ;;
  esac
  case "$name" in
    pool_trickle) MAX_DURATION_SECS=60 ;;
    *) MAX_DURATION_SECS=${MATRIX_MAX_DURATION_SECS:-3600} ;;
  esac
  apply_dispatch_settings > "$OUT_DIR/$name.settings.log" 2>&1 || FAILED=1
  BIN=$BIN PG_CONTAINER=$PG_CONTAINER REDIS_CONTAINER=$REDIS_CONTAINER PG_USER=$PG_USER \
    PG_PORT=$PG_PORT REDIS_PORT=$REDIS_PORT DB=$DB PORT=$PORT MOCK_PORT=$MOCK_PORT \
    REDIS_DB=$REDIS_DB KEY_PREFIX=$KEY_PREFIX NODE="$NODE" ACCOUNTS=$ACCOUNTS PER_CONC=$PER_CONC \
    WAIT_SECS=$WAIT_SECS GLOBAL_MAX=$GLOBAL_MAX IDLE_SECS=$IDLE_SECS \
    REUSE_INSTANCE=1 CASES=$name D=$D OUT_DIR=$OUT_DIR SERVER_PID=$SERVER_PID MOCK_PID=$MOCK_PID \
    API_KEY=$API_KEY ADMIN_KEY=$ADMIN_KEY SERVER_LOG=$SERVER_LOG \
    QUEUE=$QUEUE MAX_DURATION_SECS=$MAX_DURATION_SECS \
    bash "$REPO/scripts/loadtest/overload-run.sh" >> "$OUT_DIR/matrix.log" 2>&1 || FAILED=1
done
python3 "$REPO/scripts/loadtest/overload-verdict.py" "$OUT_DIR" > "$OUT_DIR/overload-verdict.log" 2>&1 || FAILED=1
cat "$OUT_DIR/overload-verdict.log"
python3 "$REPO/scripts/loadtest/acceptance-summary.py" "$OUT_DIR" $SCHEDULER_CASES $OVERLOAD_CASES || FAILED=1
# Keep the service and mock logs as reviewable artifacts before removing the instance directory.
cp "$SERVER_LOG" "$OUT_DIR/server.log"
cp "$D/mock.log" "$OUT_DIR/mock.log"
shutdown_started=$(python3 -c 'import time;print(time.monotonic())')
stop_instance
python3 - "$shutdown_started" <<'PYTIME' | tee -a "$OUT_DIR/matrix.log"
import sys,time
print(f"shutdown_secs: {time.monotonic()-float(sys.argv[1]):.1f}")
PYTIME
echo "SUMMARY: $OUT_DIR/SUMMARY.md"
exit "$FAILED"
