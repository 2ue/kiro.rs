#!/usr/bin/env bash
# 任务验收：按任务 id 执行对应检查，输出到 stdout，失败返回非 0。
set -uo pipefail
ROOT="${REFACTOR_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || cd "$(dirname "$0")/../.." && pwd)}"
cd "$ROOT" || exit 2
echo "仓库根: $ROOT"
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
  s4-routes-and-overrides)
    step "编译"
    run cargo build -q --bins
    step "pathOverrides 已收口"
    hits=$(grep -rn "path_overrides\|defined_cache_routes\|external_pool_route_mode\|external_pool_route_rules" src --include=*.rs 2>/dev/null | grep -v "migrate\|legacy\|tests.rs" || true)
    [ -z "$hits" ] || { echo "!! 仍有残留:"; echo "$hits" | head -30; fail=1; }
    [ -f src/model/routes.rs ] || { echo "!! 缺 src/model/routes.rs"; fail=1; }
    step "golden"
    run cargo test --bin kiro-rs -q -- golden_usage
    step "usage 投影"
    run cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
    step "routes 单测"
    run cargo test --bin kiro-rs -q -- model::routes
    step "全量单测"
    run cargo test --bin kiro-rs -q
    ;;
  s5-rename)
    step "编译"
    run cargo build -q --bins
    step "Kiro 字样清零（src/ui/构建/脚本/配置）"
    hits=$(grep -rni "kiro" src ui/src ui/tests Cargo.toml Dockerfile docker-compose*.yml scripts config.example.json data tests .github 2>/dev/null | grep -v "scripts/refactor/" || true)
    [ -z "$hits" ] || { echo "!! 仍有 Kiro 字样:"; echo "$hits" | head -40; echo "共 $(echo "$hits" | wc -l) 处"; fail=1; }
    step "模块已移动"
    [ -d src/account ] && [ ! -f src/external_pool.rs ] || { echo "!! external_pool 未移动到 account/"; fail=1; }
    step "包名"
    grep -q '^name = "claude-pool"' Cargo.toml || { echo "!! Cargo 包名未改"; fail=1; }
    step "golden（expected 不得改变）"
    changed=$(git diff --name-only HEAD -- tests/golden | xargs -I{} sh -c 'git diff HEAD -- {} | grep -E "^[-+]\s+\"(input_tokens|output_tokens|cache_read_input_tokens|cache_creation_input_tokens|ephemeral_5m_input_tokens|ephemeral_1h_input_tokens|total_input_tokens)\"" | head -1' 2>/dev/null || true)
    [ -z "$changed" ] || { echo "!! golden expected 数值被改动"; fail=1; }
    run cargo test --bin claude-pool -q -- golden_usage
    step "全量单测"
    run cargo test --bin claude-pool -q
    step "前端"
    run pnpm --dir ui check
    run pnpm --dir ui build
    ;;
  s6-migration-tool)
    step "编译"
    run cargo build -q --bins
    step "旧名只允许出现在 migration 模块"
    hits=$(grep -rn "external_upstream_pools\|kiroRsTool\|externalPool" src --include=*.rs 2>/dev/null | grep -v "src/migration/" | grep -v "tests" || true)
    [ -z "$hits" ] || { echo "!! 旧名泄漏到 migration 之外:"; echo "$hits" | head -20; fail=1; }
    [ -f src/migration/mod.rs ] || { echo "!! 缺 src/migration/mod.rs"; fail=1; }
    [ -f tests/migration/fixtures/legacy-sample.json ] || { echo "!! 缺 fixture"; fail=1; }
    step "dry-run 报告可生成"
    run cargo run -q --bin claude-pool -- import-legacy tests/migration/fixtures/legacy-sample.json
    step "migration 单测"
    run cargo test --bin claude-pool -q -- migration
    step "golden"
    run cargo test --bin claude-pool -q -- golden_usage
    step "全量单测"
    run cargo test --bin claude-pool -q
    ;;
  s7-admin-and-console)
    step "编译"
    run cargo build -q --bins
    step "admin 单测"
    run cargo test --bin claude-pool -q -- admin
    step "golden"
    run cargo test --bin claude-pool -q -- golden_usage
    step "全量单测"
    run cargo test --bin claude-pool -q
    step "前端"
    run pnpm --dir ui check
    run pnpm --dir ui build
    step "前端无旧页面残留"
    hits=$(grep -rn "credentials\|Credential\|inventory\|Inventory\|public-pool\|PublicPool\|proxy-resources" ui/src --include=*.ts --include=*.tsx 2>/dev/null | grep -v "request_api_key\|requestApiKey\|RequestApiKey" || true)
    [ -z "$hits" ] || { echo "!! 前端仍有旧页面引用:"; echo "$hits" | head -20; fail=1; }
    [ ! -d ui/src/features/credentials ] && [ -d ui/src/features/routes ] || { echo "!! 页面目录不符"; fail=1; }
    ;;
  s8-docs-and-release|s8b-e2e-fixes)
    step "编译"
    run cargo build -q --bins
    step "共享 store 回归测试"
    run cargo test --bin claude-pool -q -- admin_route_update_is_visible_to_request_path
    step "pools 键已改名"
    # 只查对外 JSON 键与前端类型/访问：后端 json!({"pools"/"pool"}) 与 serde 字段 `pub pool:`；前端 `.pools`/`pools:`/`pool:` 类型声明与 `.pool.`/`.pool)` 访问。
    hits=$( { grep -rn '"pools"\|"pool":\|pub pool:' src/admin src/account/mod.rs --include='*.rs' 2>/dev/null; grep -rnE '\.pools\b|\bpools:|\bpool: Account|\.pool\.|\.pool\)' ui/src --include='*.ts' --include='*.tsx' 2>/dev/null; } | grep -v "UsageOverviewPool\|poolRow\|/usage/\|types/usage.ts" || true)
    [ -z "$hits" ] || { echo "!! 仍有 pools/pool 键:"; echo "$hits" | head -10; fail=1; }
    step "Account 不再省略空集合"
    n=$(grep -n "skip_serializing_if" src/account/mod.rs | grep -c "Vec::is_empty\|HashMap::is_empty"); [ "$n" -eq 0 ] || { echo "!! 仍有 $n 处省略空集合"; fail=1; }
    step "全仓 Kiro 字样清零（设计文档、migration、.vscode 除外）"
    # worktree 下 .git 是文件，需同时 --exclude=.git
    hits=$(grep -rni "kiro" . --exclude-dir=.git --exclude=.git --exclude-dir=target --exclude-dir=node_modules --exclude-dir=claude-pool-refactor --exclude-dir=refactor --exclude-dir=.vscode --exclude-dir=migration --exclude=config.toml 2>/dev/null | grep -v "src/migration/" | grep -v "^./ui/node_modules" | grep -v "^./.git:" || true)
    [ -z "$hits" ] || { echo "!! 仍有 Kiro 字样:"; echo "$hits" | head -40; echo "共 $(echo "$hits" | wc -l) 处"; fail=1; }
    step "文档目录已清理"
    for d in feature docs/refactor-plan docs/plantree docs/analysis docs/archive; do [ -d "$d" ] && { echo "!! 仍存在 $d"; fail=1; }; done
    step "release 构建与版本"
    run cargo build -q --release --locked --bin claude-pool
    v=$(/tmp/kiro-rs-rust-analyzer-target/release/claude-pool --version 2>/dev/null); echo "$v"; echo "$v" | grep -q "claude-pool 1.0.0" || { echo "!! 版本输出不符"; fail=1; }
    step "全量单测"
    run cargo test --bin claude-pool -q
    step "前端"
    run pnpm --dir ui check
    run pnpm --dir ui build
    ;;
  s9-load-chaos-tooling)
    step "编译（release）"
    run cargo build -q --release --bins
    step "readyz 单测"
    run cargo test --bin claude-pool -q -- readyz
    step "golden"
    run cargo test --bin claude-pool -q -- golden_usage
    step "全量单测"
    run cargo test --bin claude-pool -q
    step "脚本语法"
    run node --check scripts/loadtest/overload-probe.mjs
    run node --check scripts/loadtest/scheduler-scenarios.mjs
    run bash -n scripts/loadtest/overload-run.sh
    run bash -n scripts/loadtest/run-acceptance-matrix.sh
    run bash -n scripts/loadtest/lib/isolated-instance.sh
    step "受保护文件未改"
    for f in src/anthropic/cache.rs src/anthropic/prompt_cache.rs src/anthropic/bounded_shaping.rs src/anthropic/prefix_tool_plus_shaping.rs src/account/usage_projection.rs src/account/golden_tests.rs; do
      git diff --quiet HEAD -- "$f" || { echo "!! 受保护文件被改: $f"; fail=1; }
    done
    step "smoke 矩阵"
    run env KEEP=0 scripts/loadtest/run-acceptance-matrix.sh smoke
    ;;
  s10-mid-stream-terminal-error)
    step "编译（release）"
    run cargo build -q --release --bins
    step "回归单测"
    run cargo test --bin claude-pool -q -- after_content_start
    step "golden"
    run cargo test --bin claude-pool -q -- golden_usage
    step "全量单测"
    run cargo test --bin claude-pool -q
    step "受保护文件未改"
    for f in src/anthropic/cache.rs src/anthropic/prompt_cache.rs src/anthropic/bounded_shaping.rs src/anthropic/prefix_tool_plus_shaping.rs src/account/usage_projection.rs src/account/golden_tests.rs; do
      git diff --quiet HEAD -- "$f" || { echo "!! 受保护文件被改: $f"; fail=1; }
    done
    step "单场景矩阵"
    run bash -n scripts/loadtest/run-acceptance-matrix.sh
    run env KEEP=0 scripts/loadtest/run-acceptance-matrix.sh scenarios mid_stream_resets long_stream_burst connection_resets
    ;;
  s11-coordinator-saturation-not-degraded)
    step "编译（release）"
    run cargo build -q --release --bins
    step "coordinator 与 readyz 单测"
    run cargo test --bin claude-pool -q -- coordinator
    run cargo test --bin claude-pool -q -- readyz
    step "golden"
    run cargo test --bin claude-pool -q -- golden_usage
    step "全量单测"
    run cargo test --bin claude-pool -q
    step "受保护文件未改"
    for f in src/anthropic/cache.rs src/anthropic/prompt_cache.rs src/anthropic/bounded_shaping.rs src/anthropic/prefix_tool_plus_shaping.rs src/account/usage_projection.rs src/account/golden_tests.rs; do
      git diff --quiet HEAD -- "$f" || { echo "!! 受保护文件被改: $f"; fail=1; }
    done
    step "开环单用例矩阵"
    run env KEEP=0 scripts/loadtest/run-acceptance-matrix.sh overload queue_overflow spike_sub2api
    ;;
  *)
    echo "未知任务 $TASK_ID"; exit 2;;
esac

echo; [ $fail -eq 0 ] && echo "VERIFY PASS" || echo "VERIFY FAIL"
exit $fail
