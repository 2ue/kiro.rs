#!/usr/bin/env bash
# 监工：codex 执行 → 验收 → 失败则把验收输出回灌给 codex 重做，最多 MAX_ROUNDS 轮。
# 用法：supervise.sh <task-id> [max-rounds]
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TASK_ID="$1"; MAX_ROUNDS="${2:-4}"
RUN_DIR="$ROOT/scripts/refactor/runs/$TASK_ID"
mkdir -p "$RUN_DIR"
STATUS="$RUN_DIR/STATUS"
echo "RUNNING" > "$STATUS"
extra=""
for i in $(seq 1 "$MAX_ROUNDS"); do
  "$ROOT/scripts/refactor/codex-task.sh" "$TASK_ID" "$extra"
  round_dir=$(ls -d "$RUN_DIR"/round-* | sort -V | tail -1)
  # 快照验收脚本后再执行，避免运行中编辑 verify.sh 导致 bash 读到错位内容。
  cp "$ROOT/scripts/refactor/verify.sh" "$round_dir/verify.snapshot.sh"
  (cd "$ROOT" && REFACTOR_ROOT="$ROOT" bash "$round_dir/verify.snapshot.sh" "$TASK_ID") > "$round_dir/verify.log" 2>&1
  if grep -q "^VERIFY PASS" "$round_dir/verify.log"; then
    echo "PASS round=$i" > "$STATUS"; echo "[supervise] $TASK_ID PASS 第 $i 轮"; exit 0
  fi
  echo "[supervise] $TASK_ID 第 $i 轮验收失败，回灌重做"
  extra="$round_dir/verify.log"
done
echo "FAIL rounds=$MAX_ROUNDS" > "$STATUS"; echo "[supervise] $TASK_ID 超过 $MAX_ROUNDS 轮仍失败"; exit 1
