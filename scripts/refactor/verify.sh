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
  s2-delete-kiro)
    step "编译（含 bins）"
    run cargo build -q --bins
    step "src/kiro 与 debug.rs 已删除"
    [ -d src/kiro ] && { echo "!! src/kiro 仍存在"; fail=1; }
    [ -f src/debug.rs ] && { echo "!! src/debug.rs 仍存在"; fail=1; }
    step "无 Kiro 引用"
    hits=$(grep -rn "crate::kiro\|kiro_provider\|KiroProvider\|MultiTokenManager\|MetadataTokenUsage" src || true)
    [ -z "$hits" ] || { echo "!! 仍有引用:"; echo "$hits" | head -40; fail=1; }
    step "删除清单文件不存在"
    for f in src/anthropic/converter.rs src/anthropic/stream.rs src/anthropic/websearch.rs src/anthropic/files.rs src/anthropic/openai src/anthropic/handlers/local_body_pipeline.rs src/anthropic/handlers/model_driven_websearch.rs src/anthropic/signature.rs src/anthropic/structured_output.rs src/admin/inventory.rs src/admin/public_pool.rs src/storage/credential_inventory.rs src/storage/kiro_api_keys.rs; do [ -e "$f" ] && { echo "!! 仍存在 $f"; fail=1; }; done
    step "handlers.rs 行数 ≤ 3000"
    n=$(wc -l < src/anthropic/handlers.rs | tr -d ' '); echo "行数: $n"; [ "$n" -le 3000 ] || { echo "!! 超过 3000"; fail=1; }
    step "golden"
    run cargo test --bin kiro-rs -q -- golden_usage
    step "外部池 usage 投影"
    run cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
    step "全量单测"
    run cargo test --bin kiro-rs -q
    ;;
  s2b-restore-tests)
    step "编译"
    run cargo build -q --bins
    step "测试数量回到基线附近"
    n1=$(grep -c "#\[test\]\|#\[tokio::test" src/external_pool/tests.rs); n2=$(grep -c "#\[test\]\|#\[tokio::test" src/anthropic/handlers/tests.rs)
    echo "external_pool/tests.rs: $n1  handlers/tests.rs: $n2"
    [ "$n1" -ge 380 ] || { echo "!! external_pool 测试数不足 380"; fail=1; }
    [ "$n2" -ge 40 ] || { echo "!! handlers 测试数不足 40"; fail=1; }
    step "golden"
    run cargo test --bin kiro-rs -q -- golden_usage
    step "全量单测"
    run cargo test --bin kiro-rs -q
    ;;
  s3-passthrough-features)
    step "编译"
    run cargo build -q --bins
    step "normalized 模式与三字段已删"
    hits=$(grep -rn "request_body_mode\|raw_model_mode\|preserve_path\|ExternalPoolRequestBodyMode\|ExternalPoolRawModelMode\|normalized_request_value\|body_mode_filter" src || true)
    [ -z "$hits" ] || { echo "!! 仍有残留:"; echo "$hits" | head -30; fail=1; }
    step "body_patch 模块存在"
    [ -f src/anthropic/body_patch.rs ] || { echo "!! 缺 src/anthropic/body_patch.rs"; fail=1; }
    step "serde_json preserve_order"
    grep -q 'preserve_order' Cargo.toml || { echo "!! Cargo.toml 未启用 preserve_order"; fail=1; }
    step "golden"
    run cargo test --bin kiro-rs -q -- golden_usage
    step "usage 投影"
    run cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
    step "passthrough 测试"
    run cargo test --bin kiro-rs -q -- passthrough
    step "全量单测"
    run cargo test --bin kiro-rs -q
    ;;
  *)
    echo "未知任务 $TASK_ID"; exit 2;;
esac

echo; [ $fail -eq 0 ] && echo "VERIFY PASS" || echo "VERIFY FAIL"
exit $fail
