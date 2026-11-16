# 任务 s5：去 Kiro 命名（阶段 5）

## 目标

落实设计 `docs/claude-pool-refactor/01-现状盘点与去留清单.md` 第 6 节与 `11-实施说明与关键逻辑契约.md` 第 1 节术语表。改名完成后，`grep -ri kiro` 在 `src/`、`ui/src/`、`Cargo.toml`、`Dockerfile`、`docker-compose*.yml`、`scripts/`、`config.example.json`、`data/`、`tests/` 范围内为 **0**（`docs/`、`README.md`、`feature/` 在阶段 8 处理）。

这是**纯改名**任务：不改任何逻辑、不删任何功能、不改任何断言内容（断言里的字符串字面量随名字同步改）。整个任务一个 commit 便于 diff 审查，所以请一次性做完。

## 命名映射（必须严格按此表）

| 类别 | 现名 | 新名 |
|---|---|---|
| Cargo 包名 / 主二进制 | `kiro-rs` | `claude-pool` |
| 压测二进制 | `kiro_loadtest`（`src/bin/kiro_loadtest.rs`） | `pool_loadtest`（`src/bin/pool_loadtest.rs`） |
| 版本号 | `0.0.252` | `1.0.0` |
| 环境变量前缀 | `KIRO_RS_*` | `CLAUDE_POOL_*`（如 `CLAUDE_POOL_HOST`、`CLAUDE_POOL_PORT`、`CLAUDE_POOL_POSTGRES_URL`、`CLAUDE_POOL_TEST_REDIS_URL`、`CLAUDE_POOL_HTTP_DRAIN_SECS`） |
| 其他环境变量 | `KIRO_CAPTURE_DIR`、`KIRO_TLS_KEYLOG_FILE`、`KIRO_BODY_PERF_*` | `CLAUDE_POOL_CAPTURE_DIR`、`CLAUDE_POOL_TLS_KEYLOG_FILE`、`CLAUDE_POOL_BODY_PERF_*` |
| Redis 默认 key 前缀 | `kiro_rs` | `claude_pool` |
| Redis 业务前缀 | `external_pool:` | `account:` |
| Redis 观测前缀默认 | `kiro_rs:observability` | `claude_pool:observability` |
| 会话 id 哈希前缀 | `kiro.rs:anthropic:conversation-id:v1:` | `claude-pool:anthropic:conversation-id:v1:` |
| Rust 模块 | `src/external_pool.rs` + `src/external_pool/` | `src/account/mod.rs` + `src/account/`（`external_pool.rs` 的内容变成 `account/mod.rs`；子模块路径跟着走） |
| Rust 类型 | `ExternalPool` → `Account`；`ExternalPoolManager` → `AccountManager`；`ExternalPoolsConfig` → `AccountsConfig`；`ExternalRouteRequest` → `AccountRouteRequest`；`ExternalPoolError` → `AccountError`；`ExternalPoolFinalError` → `AccountFinalError`；`ExternalPoolBilling` → `AccountBilling`；`ExternalPool*Mode`/`*Profile`/`*Type`/`*Policy` 枚举去掉 `ExternalPool` 前缀（如 `ExternalPoolAuthType` → `AccountAuthType`、`ExternalPoolRouteMode` → `RouteRuleMode`、`ExternalPoolHeaderProfile` → `AccountHeaderProfile`、`ExternalPoolUsageProjectionMode` → `UsageProjectionMode`、`ExternalPoolStreamResponseMode` → `StreamResponseMode`、`ExternalPoolCapacityMode` → `CapacityMode`）；`UsageRouteKind::ExternalPool` → `UsageRouteKind::Account`；`UsageRouteSubtype::External` → `UsageRouteSubtype::Account`；`UpstreamKind::ExternalPool` → 删除枚举改为无参（注入不再区分上游） |
| 配置键（JSON camelCase） | `externalPools` → `accounts`；其内 `externalPool*` 字段去前缀（`externalPoolRetryMaxAttempts` → `retryMaxAttempts`、`externalPoolsEnabled` → `enabled`、`externalPoolGlobalMaxConcurrentRequests` → `globalMaxConcurrentRequests`，以此类推）；`externalPoolUsageProjection*` → `usageProjection*`；`externalPoolUsageDebug*`/`externalPoolWireDebug*` → `usageDebug*`/`wireDebug*` |
| 配置键 | `cachePolicy.kiroRsTool` → `prefixTool`；`cachePolicy.kiroRsToolPlus` → `prefixToolPlus`；路由补丁里的 `kiroRsTool`/`kiroRsToolPlus` 同 |
| 策略类型 serde 名 | `kiro_rs_tool` → `prefix_tool`；`kiro_rs_tool_plus` → `prefix_tool_plus` |
| Rust 类型 | `KiroRsToolCachePolicy` → `PrefixToolCachePolicy`；`KiroRsToolCachePolicyPatch` → `PrefixToolCachePolicyPatch`；`KiroRsToolPlusCachePolicy` → `PrefixToolPlusCachePolicy`；`KiroRsToolPromptCachePlan` → `PrefixToolPromptCachePlan`；`PromptCacheStrategyType::KiroRsTool(Plus)` → `PrefixTool(Plus)`；`KiroPlusShaper` → `PrefixToolPlusShaper`；函数名里的 `kiro_rs_tool` → `prefix_tool` |
| 文件 | `src/anthropic/kiro_plus_shaping.rs` → `prefix_tool_plus_shaping.rs`；`src/model/kiro_rs_tool_plus.rs` → `prefix_tool_plus_policy.rs` |
| 表名 | `external_upstream_pools` → `accounts`；`usage_records.credential_id/credential_label` → `account_id/account_label`；rollup 表 `external_pool_*_cost_usd` → `account_*_cost_usd`；usage 记录 JSON 里 `externalPoolId/externalPoolName/externalPoolBilling` → `accountId/accountName/accountBilling`；索引名同步 |
| `runtime_config` 协调 epoch 行 id | `external_pool_redis_coordination_epoch` | `account_redis_coordination_epoch` |
| Redis 频道 | `events:external_pool_data_changed` | `events:account_data_changed` |
| Admin API 路径 | `/external-pools*` → `/accounts*`；`/usage-dashboard/external-pool-billing` → `/usage-dashboard/account-billing`；`/usage-dashboard/external-pool-risk` → `/usage-dashboard/account-risk`；`/usage-dashboard/overview/external` → `/usage-dashboard/overview` |
| Admin 类型 | `ExternalPoolItem`/`CreateExternalPoolRequest`/`UpdateExternalPoolRequest`/… → `AccountItem`/`CreateAccountRequest`/`UpdateAccountRequest`/… |
| 诊断目录默认 | `/tmp/kiro-rs/` | `/tmp/claude-pool/` |
| 日志与注释文案 | 「外部池」→「账号」/「账号池」；「Kiro」相关描述删除或改写；英文注释里的 `external pool` → `account` |
| 前端 | `ui/src/features/external-pools/` → `accounts/`；`external-pool-risk/` → `account-risk/`；路由 `/external-pools` → `/accounts`、`/external-pool-risk` → `/account-risk`；API 路径与类型随后端改；`kiroRsTool*` → `prefixTool*`；文案「外部池」→「账号」、「Kiro-RS Tool」→「前缀追踪」 |
| 前端 Kiro 残留 | `kiroMeteringUsage`、`KiroClientVersion`、`KiroApiKey*`、`kiroEndpointFallback*`、`kiroUpstream*`、`kiroCreditUnitPriceUsd`、`KiroVersionLifecycle*` 等对应后端已删功能的类型与字段：**删除**（它们引用的 API 已不存在）；相关页面若只剩这些字段也一并删（`credentials/`、`inventory/`、`proxies/`、`public-pool/` 四个目录已在阶段 2 后无后端支持，整体删除，并从 router 与导航移除） |
| scripts | `scripts/loadtest/kiro-mock-upstream.mjs` → `anthropic-mock-upstream.mjs`（内容只保留 Anthropic 协议 mock）；`kiro-load-runner.mjs`/`kiro-conversation-load-runner.mjs` → `pool-load-runner.mjs`/`pool-conversation-load-runner.mjs`；`scripts/kiro_version_evidence.py`、`scripts/kiro_capture_schema_diff.py` 删除；`scripts/ci/clippy-baseline.json` 重新生成（按 README 的 `check-clippy-baseline.mjs --update`，用 rustc 1.92.0：`PATH="$(dirname $(rustup which --toolchain 1.92.0 rustc)):$PATH"`） |
| Docker | `Dockerfile`、`docker-compose*.yml` 中二进制名、镜像名、容器名、数据库名、用户名 `kiro_rs` → `claude_pool`，环境变量按前缀表 |
| 数据 | `data/anthropic-models.seed.json` 已改名，检查内容无 Kiro 字样 |

**允许保留 Kiro 字样的唯一位置**：无。阶段 6 的导入子命令另行加入。

## 允许改动的文件

`src/**`、`ui/src/**`、`ui/tests/**`、`Cargo.toml`、`Cargo.lock`、`Dockerfile`、`docker-compose*.yml`、`scripts/**`（`scripts/refactor/` 除外）、`config.example.json`、`data/**`、`tests/golden/**`（JSON 里的 `strategy` 值 `kiro_rs_tool(_plus)` → `prefix_tool(_plus)`，文件名同步改；`expected` 数值不变）、`.github/**`（CI 里的二进制名与环境变量）。

golden 测试的 `expected` 数值**一个都不能变**：改名后 `cargo test -- golden_usage` 必须直接通过，不允许 `GOLDEN_UPDATE=1` 重新生成。

## 做法建议

1. 先改 Rust：用 `cargo` 编译驱动，逐类改名；`external_pool.rs` 移动到 `account/mod.rs` 用 `git mv`。
2. 再改 serde 名与配置键，更新 `config.example.json` 与 golden 文件名/strategy 值。
3. 再改 Postgres schema 与 Redis key 字符串。
4. 再改 Admin 路径与类型。
5. 最后改前端：`pnpm --dir ui install --frozen-lockfile`（若 node_modules 不存在可软链 `../2ue_kiro.rs/ui/node_modules`），改完跑 `pnpm --dir ui check && pnpm --dir ui build`（这个前端没有 lint/test 脚本，`check` 是 tsc 类型检查）。
6. Docker、scripts、CI。
7. 全局 grep 清零。

## 自检

```
cargo fmt --all
cargo build -q --bins
grep -rni "kiro" src ui/src ui/tests Cargo.toml Dockerfile docker-compose*.yml scripts config.example.json data tests .github 2>/dev/null | grep -v "scripts/refactor/"   # 应为空
cargo test --bin claude-pool -q -- golden_usage      # 注意二进制名已改
cargo test --bin claude-pool -q 2>&1 | tail -30
pnpm --dir ui check && pnpm --dir ui build
ls src/account/ && test ! -f src/external_pool.rs && echo "moved"
```
