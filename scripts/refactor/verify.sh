#!/usr/bin/env bash
# 任务验收：按任务 id 执行对应检查，输出到 stdout，失败返回非 0。
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
TASK_ID="$1"
fail=0
step() { echo; echo "== $1"; }
run() { echo "\$ $*"; "$@"; local c=$?; [ $c -ne 0 ] && { echo "!! 失败 exit=$c"; fail=1; }; return $c; }

step "格式"
run cargo fmt --all -- --check

case "$TASK_ID" in
  s0-golden)
    step "golden 用例数量 ≥ 48"
    n=$(ls tests/golden/usage/*.json 2>/dev/null | wc -l | tr -d ' ')
    echo "用例数: $n"; [ "$n" -ge 48 ] || { echo "!! 不足 48"; fail=1; }
    step "golden 测试（跑两遍确认确定性）"
    run cargo test --bin kiro-rs -q -- golden_usage
    run cargo test --bin kiro-rs -q -- golden_usage
    step "外部池 usage 投影回归"
    run cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
    step "不得改动既有源码（只允许新增）"
    changed=$(git diff --name-only HEAD -- src | grep -v '^src/external_pool/golden_tests.rs$' | grep -v '^src/external_pool/tests.rs$' | grep -v '^src/external_pool.rs$' | grep -v '^src/external_pool/usage_projection.rs$' || true)
    [ -z "$changed" ] || { echo "!! 改动了不该改的文件:"; echo "$changed"; fail=1; }
    ;;
  s1-runtime-store)
    step "编译"
    run cargo build -q --bin kiro-rs
    step "配置读写不再经过 token_manager（handlers/admin/main）"
    hits=$(grep -n "token_manager\.runtime_config\|provider\.runtime_config_snapshot\|kiro_provider.*runtime_config" src/anthropic/handlers.rs src/anthropic/handlers/*.rs src/admin/*.rs src/main.rs | grep -v "^.*//" || true)
    [ -z "$hits" ] || { echo "!! 仍有直接经 Kiro 组件读配置的调用:"; echo "$hits"; fail=1; }
    step "新模块存在"
    [ -f src/model/runtime.rs ] || { echo "!! 缺 src/model/runtime.rs"; fail=1; }
    step "全量单测（非集成）"
    run cargo test --bin kiro-rs -q
    ;;
  *)
    echo "未知任务 $TASK_ID"; exit 2;;
esac

echo; [ $fail -eq 0 ] && echo "VERIFY PASS" || echo "VERIFY FAIL"
exit $fail
