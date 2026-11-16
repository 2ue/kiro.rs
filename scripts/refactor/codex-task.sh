#!/usr/bin/env bash
# 用 codex（yolo 模式）执行一个任务说明文件。只跑一次，不做验收。
# 用法：codex-task.sh <task-id> [附加上下文文件]
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TASK_ID="$1"; EXTRA="${2:-}"
TASK_FILE="$ROOT/scripts/refactor/tasks/$TASK_ID.md"
RUN_DIR="$ROOT/scripts/refactor/runs/$TASK_ID"
mkdir -p "$RUN_DIR"
ROUND=$(( $(ls "$RUN_DIR" | grep -c '^round-') + 1 ))
OUT="$RUN_DIR/round-$ROUND"
mkdir -p "$OUT"
[ -f "$TASK_FILE" ] || { echo "任务文件不存在: $TASK_FILE" >&2; exit 2; }

PROMPT="$OUT/prompt.md"
{
  cat "$ROOT/scripts/refactor/CODEX-RULES.md"
  echo; echo "---"; echo
  cat "$TASK_FILE"
  if [ -n "$EXTRA" ] && [ -f "$EXTRA" ]; then
    echo; echo "---"; echo "# 上一轮验收失败的输出（必须先解决）"; echo; echo '```'; tail -c 20000 "$EXTRA"; echo '```'
  fi
} > "$PROMPT"

echo "[codex-task] $TASK_ID round $ROUND 开始 $(date '+%m/%d %H:%M:%S')" | tee "$OUT/meta.txt"
codex exec \
  --dangerously-bypass-approvals-and-sandbox \
  --skip-git-repo-check \
  -C "$ROOT" \
  --color never \
  -o "$OUT/last-message.md" \
  - < "$PROMPT" > "$OUT/codex.log" 2>&1
CODE=$?
echo "[codex-task] $TASK_ID round $ROUND 结束 exit=$CODE $(date '+%m/%d %H:%M:%S')" | tee -a "$OUT/meta.txt"
exit $CODE
