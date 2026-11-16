# 任务 s2a：入口层去 Kiro（阶段 2 第一步）

## 目标

让 `src/anthropic/` 与 `src/external_pool*` 完全不再依赖 `src/kiro/`：消息入口在完成现有的 raw 探测与入口处理后，**直接**把请求交给 `ExternalPoolManager::forward_with_failover`，删除全部本地 Kiro 调用链、本地/外部分流、回落与救援逻辑、Kiro 能力补偿模块、OpenAI 兼容入口与 Files API。`src/kiro/` 目录本身本任务**不删**（下一任务删），但 `src/anthropic/`、`src/external_pool*` 里不得再出现 `crate::kiro`、`KiroProvider`、`kiro_provider`。

golden 测试（`cargo test --bin kiro-rs -- golden_usage`）与外部池 usage 投影测试必须继续通过，这是行为没变的证据。

## 设计依据

`docs/claude-pool-refactor/02-目标架构与请求链路.md` 第 2 节（14 步链路）、`04-调度.md` 第 8 节（要删的混合调度逻辑）、`01-现状盘点与去留清单.md` 第 2.2 节（anthropic 目录去留）。先读这三节。

## 允许改动的文件

`src/anthropic/**`、`src/external_pool.rs`、`src/external_pool/**`、`src/main.rs`（只改装配 anthropic router 的部分）、`src/admin/service.rs`（只允许改因 anthropic 侧类型删除而编译不过的地方，例如 `UsageRouteSubtype` 变体、`KiroCredentialAttempt`）、`src/model/prompt_injection/mod.rs`（只删 `UpstreamKind::Local` 分支）、`Cargo.toml`（删 `pdf-extract`）。

## 删除清单（整文件删除）

`src/anthropic/`：`converter.rs`、`converter/`、`stream.rs`、`websearch.rs`、`files.rs`、`openai/`、`empty_turn_nudge.rs`、`empty_turn_policy.rs`、`code_execution.rs`、`structured_output.rs`、`stop_sequence.rs`、`tool_use_policy.rs`、`tool_name_restore.rs`、`tool_schema_keys.rs`、`thinking_display.rs`、`signature.rs`、`tool_format_debug.rs`、`control_block_cleaner.rs`、`identity_guard.rs`、`integer_output.rs`、`handlers/local_body_pipeline.rs`、`handlers/model_driven_websearch.rs`、`payload_guard_runtime.rs`（若只服务本地）。

保留模块里引用了上述文件的代码一并删除。`payload_guard.rs` 只删 Kiro 权重（`guard_kiro_request` 等），保留 byte 口径裁剪与 `sanitize_anthropic_messages_for_external_forwarding`。

## `handlers.rs` 的目标形态

现在 15441 行，目标 ≤ 2500 行。保留：

- 五组 `post_messages*`、`count_tokens*`、`get_models*`、`get_model*`、`post_messages_for_endpoint`、`request_runtime_config_from_store`、`RequestRuntimeConfig`（删其中 `kiro_*`、`payload_guard_kiro_max_weight`、`extract_thinking`、`body_conversion`、`expose_proxy_warnings`、`request_capture`、`claude_models_only` 保留到下一任务再删）、`models_list_response`、dfcache 路由解析、`external_route_model_resolution` / `external_request_model_resolution`、`raw_external_route_request_with_hints`（改名 `external_route_request_raw`）、解析后路径的 `ExternalRouteRequest` 构造（现 `:2583` 附近，提成 `external_route_request_parsed`）。
- 新的 `post_messages_inner`：
  1. 取 `state.external_pool_manager`（没有则 503 `api_error`）；取运行配置快照；`external_pools_enabled=false` 则 503。
  2. 若入口阶段 `requires_normalized_body=false`：用 `external_route_request_raw` 构造，`route_subtype = UsageRouteSubtype::External`，直接 `forward_with_failover`。
  3. 否则解析 payload（现有 `parse_messages_payload`），跑现有 `parsed_body_pipeline`（thinking 模型名、thinking 触发、多模态物化）和外部注入（`UpstreamKind::ExternalPool` 的 system 级注入，现 `:8281-8284`、`:8365-8371`），用 `external_route_request_parsed` 构造，`body_mode_filter = Some(Normalized)`，`forward_with_failover`。
  4. 模型解析只做 `external_request_model_resolution(requested)`（透传），不再用 Kiro 目录二次映射。
- `count_tokens_for_endpoint` 保留本地估算，删对 provider 的引用。
- `request_entry.rs`：删本地预检（`maybe_raw_external_preflight_response` 调用与 `:278-296`）、本地快速失败（`:353`、`:408-517`）、直连策略（`:262-276`）；raw 阶段其他处理（缺省 max_tokens、reasoning、历史清洗）保持不变。

删除：`ExternalFallbackContext` 及其 impl、`maybe_raw_external_direct_response`、`maybe_raw_external_preflight_response`、`raw_external_pool_*`、`local_pool_*`、`maybe_forward_external_after_local_error*`、`maybe_external_fallback_*`、`call_stream_local_rescue_*`、`call_non_stream_local_rescue_*`、`external_rescue_preflight`、`handle_stream_request`、`handle_non_stream_request`、`respond_after_first_stream_bytes`、`read_non_stream_body_with_first_output_deadline`、`retry_*empty_turn*`、`retry_stream_before_downstream_commit_inner`、`external_passthrough_stream`、`create_sse_stream`、`decode_complete_eventstream`、`call_api_*`、`RequestUsageContext`、`CredentialUsageContext` 及本地 usage 整形编排（`:3537-4126`、`:4542-5645`）、WebSearch 分支、`map_provider_error`。

## `ExternalRouteRequest` 的字段变化（`external_pool.rs:1050-1110`）

删：`local_attempted`、`local_preflight`、`local_attempts`、`direct_policy_reason`、`fallback_reason`。`UsageRouteSubtype` 只保留一个变体 `External`（其余本地/回落/救援变体删除；`usage.rs` 里按变体分支的代码一并简化）。`external_pool.rs` 内 `:1119-1125`、`:4748-4774`、`:7995-8042`、`:8687-8710` 的"来自本地降级才允许进程内 lease / 直连策略"分支删除：coordinator 熔断打开时一律使用进程内 lease。

## `usage.rs`

删 `KiroCredentialAttempt`、`summarize_attempts` 引用与 `credential_attempts` 字段；`kiro_metering_usage`、`credential_id`、`credential_label`、`sticky_bound`、`fallback_from_sticky` 字段**本任务保留**（存储层下一任务一起改），赋默认值。

## `cache.rs`

把 `use crate::kiro::model::events::MetadataTokenUsage` 换成本文件内定义：

```rust
/// 上游 Anthropic 响应里的 usage 对象。
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

`build_usage_with_simulation_policy`、`usage_from_metadata_or_estimate`（改名 `usage_from_upstream_or_estimate`）、`metadata_usage_has_signal`（改名 `upstream_usage_has_signal`）、`metadata_cache_is_empty` 改用它。字段映射：`uncached_input_tokens → input_tokens`、`cache_write_input_tokens → cache_creation_input_tokens`、`total_tokens` 不再存在（原逻辑 `total_tokens - output` 推 input 的分支删除，input 为 0 时用估算值减去缓存部分）。`external_pool.rs` 里 `cache_usage_from_any_value` 等把 JSON usage 转成这个类型。golden 测试保证行为不变。

## `inference_attempt_budget.rs`

删 `McpCallAttributionSink` 引用与 `InferenceAttemptKind::Local`；只保留 `ExternalPool` 一种 kind（可改名 `Account`，但本任务不强求）。

## `middleware.rs` / `router.rs`

`AppState` 删 `kiro_provider`、`extract_thinking`、`tool_format_debug_recorder`、`file_store`、`with_kiro_provider`；`create_router_with_provider` 改为不接收 provider；路由注册删 `/files*`、`/chat/completions`、`/responses`。`main.rs` 相应改装配调用。

## 测试

- `handlers/tests.rs`（250 个）：删除只覆盖已删功能的测试；保留并修正入口行为测试（鉴权、准入、raw 探测、缺省 max_tokens、reasoning、历史清洗、dfcache 404、models 列表、count_tokens、外部池 503）。不得放宽保留测试的断言。
- `external_pool/tests.rs`：删 `:4-7` 对 kiro 的 use 与依赖它们的测试（这些测试构造 `KiroProvider` 来测混合调度）；其余保留。
- golden 与 usage 投影测试必须通过。

## 自检

```
cargo fmt --all
cargo build -q --bin kiro-rs
grep -rn "crate::kiro\|kiro_provider\|KiroProvider\|MetadataTokenUsage" src/anthropic src/external_pool.rs src/external_pool/   # 应为空
wc -l src/anthropic/handlers.rs   # ≤ 2500
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
cargo test --bin kiro-rs -q 2>&1 | tail -30
git status --short
```
