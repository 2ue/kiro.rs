# 任务 s3：请求体透传与路由功能化（阶段 3）

## 目标

落实设计 `docs/claude-pool-refactor/02-目标架构与请求链路.md` 第 3 节（透传原则）与第 4 节（路由功能）：

1. 下游请求体**默认按原始字节透传**到账号上游。删除账号级 `requestBodyMode` / `rawModelMode` / `preservePath` 三个字段和 `normalized` 请求体模式，只保留"原字节 + 定点 patch"一条路径。
2. 所有会改写请求体的处理降级为**默认关闭的路由功能**，由 `routeFeatures` 控制；没有任何功能开启时请求体字节与下游完全一致。
3. 请求体改写统一经 `BodyPatch` 机制（JSON Pointer 白名单）。
4. 入口 raw 阶段只读不写。
5. 响应侧只触碰 usage、error 事件、换号后重复 `message_start`；`ToolTranscriptSanitizer` 降为路由功能。
6. 对外错误出口按 `11-实施说明与关键逻辑契约.md` 第 3.10 节：容量类用 529 `overloaded_error`。

golden 测试与 usage 投影测试必须继续通过。**命名不改**（`external_pool`、`routeFeatures` 等名字阶段 5 再改）。

## 先读

- `docs/claude-pool-refactor/02-目标架构与请求链路.md` 第 2、3、4、5 节
- `docs/claude-pool-refactor/11-实施说明与关键逻辑契约.md` 第 3.5、3.6、3.7、3.10 节与第 4 节红线
- 现有代码：`src/anthropic/handlers/request_entry.rs`（raw 阶段改写点：`normalize_raw_client_reasoning_aliases_with_switches`、`normalize_raw_reasoning_protocol_with_probe_and_limit`、`sanitize_raw_request_assistant_history_with_probe`、`apply_missing_max_tokens_policy*`）、`src/anthropic/handlers/parsed_body_pipeline.rs`（thinking 模型名、thinking 触发、多模态物化）、`src/external_pool/body_pipeline.rs`（`prepare_request`、`plan_for_pool`、`prepare_raw_request`、`rewrite_raw_top_level_model_with_probe`、`normalized_request_value`）、`src/anthropic/body_capabilities.rs`（`ExternalBodyPlan`、`ParsedAnthropicBodyPlan`）、`src/model/prompt_injection/mod.rs`（`RouteFeaturesConfig`、`RouteFeatureSet`、`RouteFeatureOverride`）

## 允许改动的文件

`src/anthropic/**`、`src/external_pool.rs`、`src/external_pool/**`、`src/model/config.rs`、`src/model/prompt_injection/mod.rs`、`src/storage/postgres.rs`（只删三列及其读写）、`src/admin/**`（只删三字段的 API 类型与校验）、`src/main.rs`、`Cargo.toml`（给 `serde_json` 加 `preserve_order` feature）、`tests/`（新增 passthrough 测试）。

**函数体不得改动**：`src/anthropic/cache.rs`、`prompt_cache.rs`、`prompt_cache_creation_control.rs`、`bounded_shaping.rs`、`kiro_plus_shaping.rs`、`src/model/kiro_rs_tool_plus.rs`、`src/external_pool/usage_projection.rs`、`external_pool.rs` 中 `project_usage_value` 调用链。

## A. `BodyPatch` 机制

新建 `src/anthropic/body_patch.rs`：

```rust
pub enum PatchOp { Set(serde_json::Value), Remove }
pub struct BodyPatch { pub pointer: String /* RFC 6901 */, pub op: PatchOp, pub source: &'static str /* 产生它的功能名，用于日志 */ }

/// 允许改写的 pointer 前缀白名单。超出白名单：debug 断言失败，release 忽略并 error 日志。
pub const ALLOWED_POINTER_PREFIXES: &[&str] = &["/model", "/max_tokens", "/thinking", "/output_config", "/system", "/messages", "/tools", "/context_management", "/metadata"];

pub struct BodyPatchSet(Vec<BodyPatch>);
impl BodyPatchSet {
    pub fn is_empty(&self) -> bool;
    /// 空集：返回原字节（不解析）。非空：解析 → 按序应用（同 pointer 后者覆盖）→ 序列化。
    /// 解析失败：返回原字节 + warn 日志（下游发的不是合法 JSON，让上游报错）。
    pub fn apply(&self, raw: &Bytes) -> Bytes;
    /// 只有一个 `/model` Set patch 时走现有 `rewrite_raw_top_level_model_with_probe` 字节级快路径。
}
```

`Cargo.toml` 的 `serde_json` features 加 `preserve_order`，保证字段顺序不变。写单测：空集不解析（用一个故意非法的 JSON 字节证明原样返回）、单 `/model` 走快路径、多 patch 顺序、白名单拒绝、解析失败回退。

## B. 路由功能定义

`RouteFeatureSet`（`src/model/prompt_injection/mod.rs:~470`）现有 `thinking_model_name`、`thinking_trigger`、`channel_leak_guard*`（已删或待删）、`model_driven_web_search`（已删）。改为下面这组，**全部默认关闭**：

| 字段 | 类型 | 作用 | 现有实现位置 |
|---|---|---|---|
| `missing_max_tokens` | `Option<MissingMaxTokensConfig>`，None=off | 请求没带 `max_tokens` 时按目录补默认值 | `request_entry.rs` `apply_missing_max_tokens_policy*`；配置 `config.rs` `MissingMaxTokensConfig` |
| `reasoning_compat` | `Option<LocalRequestCompatConfig>`，None=off | reasoning 别名归一、effort 大小写、budget 修复、role/content 归一 | `request_entry.rs` 两个 `normalize_raw_*`；`parse_messages_payload_with_probe` 里的 role/content 归一 |
| `history_sanitize` | `bool` | 请求侧：清洗助手历史中的工具转录污染；响应侧：`ToolTranscriptSanitizer` | `transcript_sanitizer.rs`；`external_pool.rs:7325` 附近 `ExternalAnthropicTranscriptState` |
| `thinking_model_name` | `bool` | `-thinking` 后缀模型名注入 thinking | `parsed_body_pipeline.rs` |
| `thinking_trigger` | `ThinkingTriggerMode`（off/always/real_request），默认 off | 同上 | 同上 |
| `payload_guard` | `Option<PayloadGuardConfig>`，None=off | byte 口径超限裁剪与 prompt-too-long 重试 | `payload_guard.rs`、`body_pipeline.rs` 的 guard 阶段 |
| `payload_shaping` | `Option<PayloadShapingConfig>`，None=off | 历史 tool_result 截断、工具定义压缩、webfetch 裁剪 | `payload_guard.rs` 中 `sanitize_anthropic_messages_for_external_forwarding` 等 |
| `image_processing` | `Option<ImageProcessingConfig>`，None=off | 文件/URL 图片物化、media type 修正、压缩 | `body_processing.rs`、`image_resize.rs` |

- 全局 `Config` 里的 `missing_max_tokens`、`local_request_compat`、`payload_guard_*`（7 项）、`payload_too_long_handling`、`payload_shaping`、`image_processing` 顶层字段**搬进** `route_features.defaults` 作为各功能的默认参数（默认 None/关闭），顶层删除。`compat_profile` 保留在顶层（它决定 strict 模式下是否拒绝而非清洗）。
- `RouteFeatureOverride` 对每个字段用 `Option<...>` 覆盖；合并规则沿用现有 `route_features_for(endpoint)` 的最长前缀逐字段覆盖。
- 内置路由默认值：全部关闭。导入旧配置时（阶段 6 做）会把旧的全局开关映射为对应路由功能。

## C. 入口改造（`request_entry.rs` + `handlers.rs`）

目标链路（设计 02 章第 2 节第 4、5 步）：

1. `probe_raw_messages_body(&raw_body)` 只读探测。探测出 JSON 语法错误仍按现有逻辑返回 400（这是官方也会拒绝的输入，属 C6）。
2. 解析路由 → `features = runtime.route_features_for(endpoint)`。
3. 若 `features` 全部关闭且注入项全部关闭：**不解析 JSON**，`requires_normalized_body=false`，直接构造 `external_route_request_raw` 转发。
4. 否则解析为 `serde_json::Value`（不是 `MessagesRequest`），依次让每个开启的功能产出 `BodyPatch`：
   - `missing_max_tokens` → `/max_tokens` Set
   - `reasoning_compat` → `/thinking/*`、`/output_config/*`、`/messages/N/role`、`/messages/N/content` Set/Remove（把现有基于字节的 `normalize_raw_*` 改为基于 Value 产出 patch；逻辑不变）
   - `history_sanitize` → `/messages/N/content` Set
   - `thinking_model_name` / `thinking_trigger` → `/model` Set、`/thinking` Set
   - `image_processing` → `/messages/N/content/M/source` Set（物化后的 base64）
   - 注入 → `/system` Set（现有 `apply_to_system` 的结果）
   - `payload_shaping` → `/messages/*`、`/tools/*` Set
   - `payload_guard`（preemptive 模式）→ `/messages` Set（裁剪后的历史）
5. `BodyPatchSet::apply(&raw_body)` 得到最终字节，`requires_normalized_body = !patches.is_empty()`。
6. 构造 `ExternalRouteRequest`：`effective_raw_body` = 最终字节，`payload = None`（不再携带类型化 payload），`effective_raw_probe` 重新探测最终字节。
7. `forward_with_failover`。

`post_messages_inner` 只剩这一条路径；删除 `external_route_request_parsed` 与 `body_mode_filter`。`ExternalRouteRequest.payload: Option<MessagesRequest>` 字段：若 usage 投影链路（`usage_projection.rs::build_template`）需要 `payload` 来算 `request_input_tokens` 和 `stable_conversation_id`，保留该字段但改为在入口按最终字节 `serde_json::from_slice::<MessagesRequest>` 填充（失败则 None，投影退化为 pass_through）。**不要改 `build_template` 的函数体**。

策略 `compat_profile.is_strict()` 且 `history_sanitize` 开启时发现污染：沿用现有行为返回 400（`strict_request_protocol_contamination`）。

## D. 账号池侧（`external_pool.rs` + `body_pipeline.rs`）

- 删除 `ExternalPool.request_body_mode`、`raw_model_mode`、`preserve_path` 及 `ExternalPoolRequestBodyMode`、`ExternalPoolRawModelMode` 枚举、`ExternalBodyPlan::normalized`、`ExternalBodyBytesPlan::Normalized`、`normalized_request_value` 及其 overlay 系列函数、`prepare_normalized_payload`、`sanitize_normalized_external_*`。`storage/postgres.rs` 删三列（`EXTERNAL_POOL_SELECT_SQL`、INSERT、UPDATE、结构体、迁移守卫）；`admin/types.rs` 与 `service.rs` 删三字段。
- `prepare_request` 简化为：取 `route.effective_raw_body`；`model_pipeline::outbound_model_for_raw` 算出站模型；若与请求模型不同 → 一个 `/model` patch 经 `BodyPatchSet::apply`（快路径）；`anthropic_passthrough`/`claude_code_mimic` 画像且 beta 不含 context-management → `/context_management` Remove patch（现 `sanitize_anthropic_body_for_beta_header` 改为产出 patch）。两处是设计认可的"必要处理"。
- `external_pool.rs:6812` 附近 prompt-too-long 的裁剪重试：只在路由 `payload_guard` 开启（`route.payload_guard_retry_config.is_some()`）时执行，裁剪结果同样经 `BodyPatchSet`（`/messages` Set）。
- 响应侧：`ExternalAnthropicTranscriptState` 只在 `route.history_sanitize_enabled` 为 true 时构造；否则 SSE 事件只经 usage 改写与 error 脱敏。`ExternalRouteRequest` 新增 `history_sanitize_enabled: bool`。
- `ExternalPoolFinalError::public_status`：容量类（无候选、排队超时、全局并发满，即现有 `is_capacity_like()` 或 `status == SERVICE_UNAVAILABLE` 分支）改为 `529` + `overloaded_error`；其余不变。

## E. 测试

新建 `src/anthropic/passthrough_tests.rs`（`#[cfg(test)] mod`）：

1. **字节一致**：用 `external_pool/tests.rs` 里现有的 mock Anthropic 上游（搜 `spawn_` / `axum::serve` 辅助函数），对一组覆盖 thinking、tools、cache_control、metadata、未知顶层字段、非标准 role、字段顺序打乱的请求体，在全部路由功能关闭时断言上游收到的字节与下游发送的字节**逐字节相同**；请求头除鉴权、`anthropic-version`、hop-by-hop 外一致。
2. **每个功能成对测试**：开启某一功能时只有白名单内对应 pointer 的值变化，其余 JSON 节点（用 `serde_json::Value` 比较）不变；关闭时字节一致。
3. **模型映射只改顶层 model**：账号映射命中时只有 `/model` 变，其余字节一致（用快路径）。
4. **529**：无可用账号时响应为 529 `overloaded_error`。
5. 现有 `handlers/tests.rs` 中 thinking_suffix_*、thinking_trigger_*、payload_guard_*、on_too_long_*、each_prompt_injection_item_* 等测试按新的功能开关方式适配（默认关 → 测试里显式开启），断言不变。

## 自检

```
cargo fmt --all
cargo build -q --bins
grep -rn "request_body_mode\|raw_model_mode\|preserve_path\|ExternalPoolRequestBodyMode\|ExternalPoolRawModelMode\|normalized_request_value\|body_mode_filter" src   # 应为空
grep -rn "local_request_compat\|missing_max_tokens\b" src/model/config.rs | grep "pub " # 顶层应为空（已搬入 route_features）
cargo test --bin kiro-rs -q -- golden_usage
cargo test --bin kiro-rs -q -- external_pool::tests::usage_projection
cargo test --bin kiro-rs -q -- passthrough
cargo test --bin kiro-rs -q 2>&1 | tail -30
```
