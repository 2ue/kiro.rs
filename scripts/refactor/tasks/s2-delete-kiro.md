# 任务 s2：一次性删除全部 Kiro 代码（阶段 2）

## 为什么是一个任务

`src/kiro/provider.rs` 依赖 `anthropic::converter`、`anthropic::payload_guard::kiro_payload_weight`、`anthropic::inference_attempt_budget` 的本地变体；`storage/postgres.rs` 穷举匹配 `UsageRouteSubtype` 的本地变体；`admin/service.rs` 与 `main.rs` 持有 `MultiTokenManager` / `KiroProvider`。这些互相咬合，无法分步保持编译通过。所以本任务一次性删完，**只要求最终状态**编译并通过测试，中间状态允许编译失败。

## 跨轮次推进

本任务很大。如果你一轮做不完，就把工作区留在"尽可能接近完成"的状态结束（不要回滚），下一轮会把验收日志回灌给你，你从当前工作区继续。每轮结束时的汇报要写清楚：已完成哪些步骤、还剩哪些、当前编译错误数。

## 目标状态

1. `src/kiro/` 目录不存在；`src/debug.rs` 不存在。
2. `grep -rn "crate::kiro\|kiro_provider\|KiroProvider\|MultiTokenManager\|MetadataTokenUsage" src` 为空。
3. 入口 `POST /{route}/v1/messages` 直接把请求交给 `ExternalPoolManager::forward_with_failover`，没有本地/外部分流、回落、救援、直连策略。
4. OpenAI 兼容入口（`/chat/completions`、`/responses`）、Files API、WebSearch、Kiro 能力补偿模块全部删除。
5. `Config` 只剩通用字段与外部池字段；PostgreSQL schema 不再有 Kiro 表；Redis 不再有 `scheduler:*` 等 Kiro key。
6. `cargo build --bins` 通过；golden 测试、外部池 usage 投影测试、全量单测通过。
7. **命名不改**：`external_pool`、`externalPool*`、`kiro_rs_tool`、`KiroRsTool*`、包名 `kiro-rs`、`KIRO_RS_*` 环境变量、`credential_id` 等名字本任务一律保持原样，阶段 5 统一改名。只删功能，不改名。

## 允许改动的文件

`src/**`（整个源码树）、`Cargo.toml`、`Cargo.lock`、`data/`、`config.example.json`。

**但以下文件的函数体不得改动**（只允许改 use 路径、入参类型、删除未被引用的函数）：`src/anthropic/cache.rs`（除下述 `UpstreamUsage` 替换）、`src/anthropic/prompt_cache.rs`、`src/anthropic/prompt_cache_creation_control.rs`、`src/anthropic/bounded_shaping.rs`、`src/anthropic/kiro_plus_shaping.rs`、`src/model/kiro_rs_tool_plus.rs`、`src/external_pool/usage_projection.rs`、`src/external_pool.rs` 里 `project_usage_value` 及其调用链。golden 测试是行为不变的证据。

## 设计依据（先读）

- `docs/claude-pool-refactor/01-现状盘点与去留清单.md` 第 2、3、4 节：逐文件、逐配置、逐表的去留。
- `docs/claude-pool-refactor/02-目标架构与请求链路.md` 第 2 节：目标链路。
- `docs/claude-pool-refactor/04-调度.md` 第 8 节：要删的混合调度逻辑。
- `docs/claude-pool-refactor/11-实施说明与关键逻辑契约.md` 第 4 节红线。

## 建议的内部顺序

从"根"开始删，让依赖链一次断干净，然后集中修编译：

### A. 装配与管理层

1. `src/main.rs`：删 Kiro 装配（endpoints、`KiroProvider`、`MultiTokenManager`、`kiro_version`、`KIRO_API_KEY`、模型 seed 同步、cohort 后台任务、`credentials_changed` 监听、inventory worker）。`RuntimeConfigStore` 的构造与订阅保留；Kiro manager 注册的 validator 不再注册；`anthropic::create_router_with_provider` 改为不带 provider 的构造。
2. `src/admin/`：删 `inventory.rs`、`public_pool.rs`；`router.rs` 删 `/credentials*`、`/inventory*`、`/proxy-resources*`、`/subscription-model-lists*`、`/config/kiro-version/*`、`/config/load-balancing`、`/public-pool`、`/api/public/pool`、`/route-secret`、`/model-capabilities/sync`、`/usage-dashboard/accounts`、`/usage-dashboard/overview/local`；`service.rs` 删对应方法、`token_manager` / `kiro_provider` 字段、`credential_imports_in_flight`、余额缓存；`types.rs` 删 Kiro 凭据类型与 `RuntimeConfigResponse` / `UpdateRuntimeConfigRequest` 里的 Kiro 字段；`route_scenarios.rs` 删 `local`、`local_accounts`；`service_tests.rs` 删只测已删功能的测试。
3. `src/storage/`：删 `credential_inventory.rs`、`kiro_api_keys.rs`；`postgres.rs` 删 `credentials`、`credential_*`（8 张）、`kiro_version_state`、`usage_credential_cost_summary`、`proxy_resources` 的建表/查询/结构体/迁移，`usage_records` 与 rollup 表删 `kiro_metering_usage`、`total_kiro_metering_usage`、`sticky_bound`、`fallback_from_sticky` 列（新 schema 直接不建），`model_capabilities*` 删 `reasoning_cohort_*`，`UsageRouteSubtype` 匹配只剩 `External`；`redis_cache.rs` 删 `scheduler:*`、`refresh_lock`、`refresh_outcome`、`auxiliary:token_refresh`、`balance:*`、`credentials_changed` 频道与对应 Lua（`InFlightKeys` 结构保留，删 `scheduler:` 侧构造）；`account_events.rs` 删 local 分支；`usage_demo_seed.rs` 同步。
4. `src/kiro/token_manager/storage_task.rs` 搬到 `src/common/storage_task.rs`；然后 `rm -rf src/kiro src/debug.rs`。

### B. 入口层

5. 删整文件：`anthropic/converter.rs`、`converter/`、`stream.rs`、`websearch.rs`、`files.rs`、`openai/`、`empty_turn_nudge.rs`、`empty_turn_policy.rs`、`code_execution.rs`、`structured_output.rs`、`stop_sequence.rs`、`tool_use_policy.rs`、`tool_name_restore.rs`、`tool_schema_keys.rs`、`thinking_display.rs`、`signature.rs`、`tool_format_debug.rs`、`control_block_cleaner.rs`、`identity_guard.rs`、`integer_output.rs`、`handlers/local_body_pipeline.rs`、`handlers/model_driven_websearch.rs`、`payload_guard_runtime.rs`（若只服务本地）。`payload_guard.rs` 删 Kiro 权重部分（`guard_kiro_request`、`kiro_payload_weight` 等），保留 byte 口径裁剪与 `sanitize_anthropic_messages_for_external_forwarding`。
6. `handlers.rs` 重写为 ≤ 3000 行。保留：五组 `post_messages*` / `count_tokens*` / `get_models*` / `get_model*`、`post_messages_for_endpoint`、`request_runtime_config`、`RequestRuntimeConfig`（删 `kiro_*`、`payload_guard_kiro_max_weight`、`extract_thinking`、`body_conversion`、`expose_proxy_warnings`、`request_capture`、`claude_models_only`、`local_request_compat` 中的 local 子项）、`models_list_response`（不再按 `claude_models_only` 过滤）、dfcache 路由解析、`external_request_model_resolution`、`raw_external_route_request_with_hints`（改名 `external_route_request_raw`）、解析后路径的 `ExternalRouteRequest` 构造（现约 `:2583`，提成 `external_route_request_parsed`）。新 `post_messages_inner`：取 `state.external_pool_manager`（无则 503）；`external_pools_enabled=false` 则 503；`requires_normalized_body=false` → `external_route_request_raw` + `forward_with_failover`；否则解析 payload → 现有 `parsed_body_pipeline` → `UpstreamKind::ExternalPool` 的 system 级注入 → `external_route_request_parsed`（`body_mode_filter=Some(Normalized)`）→ `forward_with_failover`。`route_subtype` 一律 `UsageRouteSubtype::External`。其余全部删除（`ExternalFallbackContext`、所有 `local_*` / `rescue` / `fallback` / `direct` 函数、`handle_stream_request`、`handle_non_stream_request`、`create_sse_stream`、`decode_complete_eventstream`、`call_api_*`、`RequestUsageContext`、`CredentialUsageContext`、本地 usage 整形编排、WebSearch 分支、`map_provider_error`）。
7. `handlers/request_entry.rs`：删直连策略、本地预检、本地快速失败；raw 阶段其他处理不变。`handlers/tests.rs`：删只覆盖已删功能的测试，保留入口行为测试（鉴权、准入、raw 探测、缺省 max_tokens、reasoning、历史清洗、dfcache 404、models、count_tokens、外部池 503），**不放宽断言**。
8. `middleware.rs` / `router.rs`：`AppState` 删 `kiro_provider`、`extract_thinking`、`tool_format_debug_recorder`、`file_store`、`with_kiro_provider`；`create_router_with_provider` 改为不接收 provider；删 `/files*`、`/chat/completions`、`/responses` 路由。
9. `external_pool.rs`：`ExternalRouteRequest` 删 `local_attempted`、`local_preflight`、`local_attempts`、`direct_policy_reason`、`fallback_reason`；`UsageRouteSubtype` 只留 `External`；删 `:1119-1125`、`:4748-4774`、`:7995-8042`、`:8687-8710` 附近"来自本地降级才允许进程内 lease / 直连策略"分支，coordinator 熔断打开时一律进程内 lease。`external_pool/tests.rs` 删对 `crate::kiro` 的 use 及依赖它们的测试。
10. `usage.rs`：删 `KiroCredentialAttempt`、`summarize_attempts`、`credential_attempts`、`kiro_metering_usage`、`sticky_bound`、`fallback_from_sticky`；`credential_id/label` 保留。`inference_attempt_budget.rs`：删 `McpCallAttributionSink`、`InferenceAttemptKind` 的本地/MCP 变体，只留 `ExternalPool`。`model_capabilities.rs`：删 `sync_from_kiro_*`、reasoning cohort、`KiroReasoningCohortContractMatch`；seed 文件 `data/kiro-upstream-models.seed.json` 改名 `data/anthropic-models.seed.json`，只留 Claude 条目。
11. `cache.rs`：把 `MetadataTokenUsage` 换成本文件定义的

    ```rust
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct UpstreamUsage {
        pub input_tokens: i32,
        pub output_tokens: i32,
        pub cache_read_input_tokens: i32,
        pub cache_creation_input_tokens: i32,
        pub cache_creation_5m_input_tokens: i32,
        pub cache_creation_1h_input_tokens: i32,
    }
    ```

    `build_usage_with_simulation_policy`、`usage_from_metadata_or_estimate`（改名 `usage_from_upstream_or_estimate`）、`metadata_usage_has_signal`、`metadata_cache_is_empty` 改用它。字段映射：`uncached_input_tokens → input_tokens`，`cache_write_input_tokens → cache_creation_input_tokens`；原 `total_tokens - output` 推 input 的分支删除，input 为 0 时用估算值减缓存部分。`external_pool.rs` 里把 JSON usage 转这个类型。**golden 必须继续通过**。
12. `diagnostics/capture.rs`：`record_upstream_frame` 改为记录原始字节，删对 `kiro::parser::frame` 的依赖。`model/prompt_injection/mod.rs`：删 `UpstreamKind::Local` 与 10 个 `ExternalPoolUnsupported` 注入项及其内置文案（保留 `taskQuality`、`languageConstraint`、`custom`、`identityPrompt`），删 `externalPool` 开关（保留项 `enabled` 直接生效）。`model/model_support.rs` 删 `subscription_tier_key`。

### C. 配置

13. `src/model/config.rs`：删 `01` 章第 3.1 节列出的全部 Kiro 顶层字段（`region`、`authRegion`、`apiRegion`、`kiroVersion*`、`machineId*`、`systemVersion`、`nodeVersion`、`kiroUpstream*`、`defaultEndpoint`、`kiroEndpoint*`、`kiroAgentModeStrategy`、`subscriptionModelLists`、`kiroCreditUnitPriceUsd`、`publicPool*`、`tokenRefresh*`、`auxiliaryUpstream*`、`credentialInfoRefreshConcurrency`、全部 `credential*`、`localBerserk*`、`loadBalancingMode`、`scheduler*`、`selectionFailure*`、`compression`、`payloadGuardKiroMaxWeight`、`kiroCachePoint*`、`extractThinking`、`toolFormatDebug`、`promptSteering`、`thinkingTriggerMode`、`dispatchGlobalMaxConcurrentRequests`、`dispatchMaxQueuedRequests`、`weightedCapacity`、`credentialDispatchMaxWaitSecs`、`emptyTurn*`、`exposeProxyWarnings`、`requestCapture`、`streamKeepaliveIntervalSecs`、`streamFirstOutputTimeoutSecs`、`bodyConversion`、`localRequestCompat.zeroUsageOnHoldExpiredMessageStart`、`countTokensApi*`、`claudeModelsOnly`）；`ExternalPoolsConfig` 删 `externalDirectPolicyEnabled`、`directExternal*`、`localPool*`、`fallbackOn*`、`externalPoolLocalRescue*`、`localPoolCircuit*`、`externalPoolMaxInputTokens`、`externalPoolTransientFailureCooldownThreshold`；`cachePolicy.*.cachePoint`、`cacheTtl.local`（`CacheTtlPolicy` 只留一个字段，读取处同步改）。删对应默认值函数、校验、迁移函数（v1–v15 全删，`runtimeConfigMigrationVersion` 重置为 1）。默认值调整：`externalPoolsEnabled` 默认 true、`externalPoolDispatchMaxWaitSecs` 默认 60、`externalPoolCapacityMode` 默认 wait、`externalPoolMaxQueuedRequests` 默认 64。只测已删字段的测试删除。
14. `config.example.json` 按新 Config 重写。`Cargo.toml` 删 `ciborium`、`crc`、`pdf-extract`（确认无引用）。`src/bin/kiro_loadtest.rs` 删假 Kiro 上游与凭据相关代码，保留假 Anthropic 上游与压测逻辑，文件名不改。

### D. 收尾

15. `cargo fmt --all`；`cargo build --bins` 无 error；消除本次改动引入的 `never used` 警告（删未引用函数）。
16. 跑自检。

## 自检

```
cargo fmt --all
cargo build -q --bins
test ! -d src/kiro && test ! -f src/debug.rs && echo "kiro removed"
grep -rn "crate::kiro\|kiro_provider\|KiroProvider\|MultiTokenManager\|MetadataTokenUsage" src   # 应为空
wc -l src/anthropic/handlers.rs   # ≤ 3000
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
cargo test --bin kiro-rs -q 2>&1 | tail -30
git status --short | wc -l
```
