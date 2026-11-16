# 任务 s2c：删除 `src/kiro/`、配置与存储去 Kiro（阶段 2 第三步）

## 目标

删除 `src/kiro/` 目录，裁剪 `Config`、`ExternalPoolsConfig`、PostgreSQL schema、Redis key、`main.rs` 装配、压测工具与 Cargo 依赖。完成后 `cargo build` 通过，golden 与全量单测通过。**命名改造不在本任务**（`externalPool*` 等名字保持不动，阶段 5 统一改）。

## 设计依据

`docs/claude-pool-refactor/01-现状盘点与去留清单.md` 第 2.1、2.4、2.5、3、4 节；`07-存储与迁移.md` 第 2、3 节。

## 允许改动的文件

`src/kiro/**`（删除）、`src/model/**`、`src/storage/**`、`src/main.rs`、`src/common/**`、`src/diagnostics/**`、`src/bin/**`、`src/debug.rs`（删）、`src/anthropic/model_capabilities.rs`、`src/anthropic/usage.rs`、`src/anthropic/handlers.rs`（只删上一任务暂留的 `RequestRuntimeConfig` Kiro 字段）、`src/admin/types.rs`（只删暂留的五个必填字段）、`src/admin/service.rs`（只改因 Config 字段删除而编译不过的地方）、`Cargo.toml`、`data/`、`config.example.json`。

## 步骤（每步编译一次）

1. `src/kiro/token_manager/storage_task.rs` 搬到 `src/common/storage_task.rs`，更新 `external_pool.rs` 与 `storage/account_events.rs` 的 use。
2. `src/model/config.rs`：删 `01` 章第 3.1 节列出的全部 Kiro 字段（`region`、`authRegion`、`apiRegion`、`kiroVersion*`、`machineId*`、`systemVersion`、`nodeVersion`、`kiroUpstream*`、`defaultEndpoint`、`kiroEndpoint*`、`kiroAgentModeStrategy`、`subscriptionModelLists`、`kiroCreditUnitPriceUsd`、`publicPool*`、`tokenRefresh*`、`auxiliaryUpstream*`、`credentialInfoRefreshConcurrency`、全部 `credential*`、`localBerserk*`、`loadBalancingMode`、`scheduler*`、`selectionFailure*`、`compression`、`payloadGuardKiroMaxWeight`、`kiroCachePoint*`、`extractThinking`、`toolFormatDebug`、`promptSteering`、`thinkingTriggerMode`、`dispatchGlobalMaxConcurrentRequests`、`dispatchMaxQueuedRequests`、`weightedCapacity`、`credentialDispatchMaxWaitSecs`、`emptyTurn*`、`exposeProxyWarnings`、`requestCapture`、`streamKeepaliveIntervalSecs`、`streamFirstOutputTimeoutSecs`、`bodyConversion`、`localRequestCompat.zeroUsageOnHoldExpiredMessageStart`、`countTokensApi*`、`claudeModelsOnly`）；`ExternalPoolsConfig` 删 `externalDirectPolicyEnabled`、`directExternal*`、`localPool*`、`fallbackOn*`、`externalPoolLocalRescue*`、`localPoolCircuit*`、`externalPoolMaxInputTokens`、`externalPoolTransientFailureCooldownThreshold`；`cachePolicy.*.cachePoint`、`cacheTtl.local`（`CacheTtlPolicy` 只留一个字段，`external_pool.rs` 读取处同步改）。删对应默认值函数、校验、迁移函数（v1–v15 全删，`runtimeConfigMigrationVersion` 重置为 1，迁移入口只保留"旧版本号 → 直接接受"）。**暂不**改 `claudeModelsOnly` 对应的列表过滤为 `listed` 字段（阶段 4 做），直接删开关并让 `/v1/models` 不再过滤。
   - 删除默认值调整：`externalPoolsEnabled` 默认 true、`externalPoolDispatchMaxWaitSecs` 默认 60、`externalPoolCapacityMode` 默认 wait、`externalPoolMaxQueuedRequests` 默认 64。
   - `model/kiro_rs_tool_plus.rs` 本任务不改名。`model/model_support.rs` 删 `subscription_tier_key`。`model/prompt_injection/mod.rs` 删 10 个 `ExternalPoolUnsupported` 注入项及其内置文案（保留 `taskQuality`、`languageConstraint`、`custom`、`identityPrompt`）与 `externalPool` 开关（保留项 `enabled` 直接生效）。
3. `src/storage/postgres.rs`：删 `credentials`、`credential_*`（8 张）、`kiro_version_state`、`usage_credential_cost_summary`、`proxy_resources` 的建表、查询、结构体、迁移；`usage_records` 与 rollup 表的 `kiro_metering_usage` / `total_kiro_metering_usage` 列、`sticky_bound`、`fallback_from_sticky` 列删除（新 schema 直接不建；不写 ALTER 兼容）；`model_capabilities*` 删 `reasoning_cohort_*`。`redis_cache.rs`：删 `scheduler:*`、`refresh_lock`、`refresh_outcome`、`auxiliary:token_refresh`、`balance:*`、`credentials_changed` 频道及对应 Lua；保留 `external_pool:*`、`usage:*`、`request_key_limits:*`、`admin_cache:*`、`runtime_config_changed`、`dispatch_wakeup`、`external_pool_data_changed`。`InFlightKeys` 结构保留，删 `scheduler:` 侧构造。
4. `src/anthropic/usage.rs`：删 `kiro_metering_usage`、`sticky_bound`、`fallback_from_sticky`、`credential_attempts` 残留；`credential_id/label` 本任务保留（阶段 5 改名 account）。`src/anthropic/model_capabilities.rs`：删 `sync_from_kiro_catalog`、`sync_from_kiro_models*`、reasoning cohort、`KiroReasoningCohortContractMatch`；保留手工录入、静态别名、解析、列表。seed 文件 `data/kiro-upstream-models.seed.json` 改名 `data/anthropic-models.seed.json`，内容只保留 Claude 条目。
5. `src/diagnostics/capture.rs`：删 `record_upstream_frame` 对 `kiro::parser::frame` 的依赖，改为记录原始字节。
6. `src/main.rs`：删 Kiro 装配（endpoints、KiroProvider、MultiTokenManager、kiro_version、KIRO_API_KEY 环境变量、模型 seed 同步、cohort 后台任务、credentials_changed 监听）。
7. `src/bin/kiro_loadtest.rs`：删假 Kiro 上游与 Kiro 凭据相关代码，保留假 Anthropic 上游、压测与混沌逻辑；文件名本任务不改。
8. 删 `src/kiro/`、`src/debug.rs`；`Cargo.toml` 删 `ciborium`、`crc`（确认 `bin` 不再用）；`config.example.json` 按新 Config 重写（保留的字段用现有默认值）。
9. 测试：`storage/postgres.rs`、`model/config.rs` 中只测已删内容的测试删除；其余保留。

## 自检

```
cargo fmt --all
cargo build -q --bin kiro-rs --bins
test ! -d src/kiro && echo "kiro dir removed"
grep -rn "crate::kiro" src   # 应为空
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q 2>&1 | tail -30
grep -c "" src/model/config.rs    # 参考：应明显小于 11161
git status --short | head -50
```
