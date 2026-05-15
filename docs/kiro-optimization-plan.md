# Kiro 账号与流式链路优化方案

日期：2026-05-15

本文档只针对当前 `kiro.rs` 项目的 Kiro 相关核心逻辑，目标是把已经分析出的优化点落实为可执行方案。范围包括账号管理、会话调度、代理、Kiro Provider 重试、AWS eventstream/SSE、非流式聚合、usage/token、Admin 管理接口和缓存。

## 2026-05-15 本轮落地记录

本轮优化先落地最贴近当前核心链路的问题：

1. `conversationState.conversationId` 参与账号调度，新增进程内 `conversationId -> credentialId` 粘性绑定。
2. 统一账号可用性判断，避免 priority 模式下 `current_id` 绕过 Opus/Free 过滤。
3. Provider 对 `408/429/5xx` 做 sticky-aware 临时 fallback：优先保持绑定账号，连续软失败后仅在本次请求中排除该账号尝试其他账号。
4. 临时 fallback 账号发生硬失败时，只清理绑定到该失败账号的会话，避免误删原账号绑定。
5. Admin 后端对状态变更清理余额缓存，避免管理页显示旧订阅/旧用量。
6. Admin 前端 mutation 同步清理 `credential-balance` 查询缓存，并修正批量刷新 API Key 账号被计失败的问题。

本轮仍不把会话绑定持久化到文件。原因是当前项目的账号状态、统计和调度都以进程内 `MultiTokenManager` 为核心，服务重启后重新分配会话是可接受边界；如果后续需要跨重启粘性，再单独设计小型持久化索引。

## 2026-05-16 补充落地记录

针对上一轮复查出的真实缺陷，补充落地：

1. 流式请求不再在收到 `2xx eventstream` 响应头时立即上报成功。
2. `call_api_stream` 返回带完成上报器的响应包装，普通流式和 `/cc/v1/messages` 缓冲流式都在上游 body 正常 EOF 后才调用 `report_success_for_session`。
3. 上游流读取错误、idle timeout、Kiro error/exception/invalidState 事件，以及客户端中途断开导致 stream 被 drop，都会按 sticky soft failure 记录，不直接禁用账号。
4. 非流式请求仍在 provider 拿到成功响应后上报成功，因为非流式 body 会在 handler 中一次性读取和解析；后续如需更严格，可把非流式也改为解码成功后再上报。
5. `admin-ui/pnpm-workspace.yaml` 增加单包 `packages` 配置，使 pnpm v9/v10 能正常识别 workspace 并执行类型检查/构建。

## 当前核心链路

当前项目的核心定位是 Anthropic API 兼容代理，而不是通用 Kiro SDK。

主链路如下：

1. `src/main.rs` 启动时加载 `config.json`、`credentials.json`，创建 `MultiTokenManager` 和 `KiroProvider`。
2. `src/anthropic/handlers.rs` 接收 `/v1/messages` 或 `/cc/v1/messages` 请求。
3. `src/anthropic/converter.rs` 将 Anthropic 请求转换为 Kiro `generateAssistantResponse` 请求。
4. `src/kiro/provider.rs` 根据模型、账号、endpoint、proxy 和 token 构造上游请求。
5. `src/kiro/token_manager.rs` 负责账号选择、token 刷新、失败计数、禁用、自愈、Admin 操作和统计。
6. `src/kiro/parser/decoder.rs` 解码 AWS eventstream。
7. `src/anthropic/stream.rs` 将 Kiro event 转换为 Anthropic SSE。
8. Admin API 通过 `src/admin/service.rs` 读取和修改同一个 `MultiTokenManager`。

因此优化必须围绕这条链路做，不做脱离当前项目形态的大重构。

## 已完成的基础优化

当前代码已经完成了以下 Kiro 相关修复和增强：

1. 新增 Kiro 事件解析：`reasoningContentEvent`、`metadataEvent`、`messageMetadataEvent`、`invalidStateEvent`。
2. SSE 转换支持 Kiro 原生 reasoning、redacted thinking、metadata usage。
3. 流式上游读空闲超时从 ping 保活中分离，使用 180 秒 upstream idle timeout。
4. Kiro stream 返回 `2xx` 但非 eventstream 时不再当作成功流。
5. Kiro 上游 error/exception/invalid state 会转换为 SSE error，并避免继续发送正常 `message_delta/message_stop`。
6. Admin 添加账号时保留 `endpoint` 元数据。
7. 非流式响应已使用 `metadataEvent` 的准确 usage，并包含 cache read/write token 字段。

这些优化已经处理了主要的协议兼容问题。后续优化重点应放在“会话粘性调度”和“账号/Admin 状态一致性”上。

## 设计目标

### 必须保证

1. 一个会话尽可能在同一个 Kiro 账号上完成。
2. 新会话可以继续使用当前 `priority` 或 `balanced` 策略分配账号。
3. 硬失败时允许迁移会话，例如额度用尽、账号禁用、refresh token 永久失效、账号不支持目标模型。
4. 软失败时优先保持粘性，例如网络抖动、`408`、`429`、`5xx`、retryable AWS exception。
5. Admin 操作必须同步影响调度状态，例如禁用/删除账号后清理相关会话绑定和缓存。
6. 不引入数据库或复杂资产系统，先使用内存态绑定，贴合当前 JSON 凭据文件和进程内调度模型。

### 暂不做

1. 不把 `credentials.json` 替换成 SQLite。
2. 不照搬其他项目的大型账号资产化系统。
3. 不做 endpoint 插件框架重写。当前 `KiroEndpoint` 抽象已经够用。
4. 不用消息内容 hash 强猜会话。优先使用已存在的 `conversationState.conversationId`。

## P0：会话粘性调度

### 现状

`src/anthropic/converter.rs` 已经从 Claude Code `metadata.user_id` 中提取 `session_id`，并写入 Kiro 请求：

```text
conversationState.conversationId
```

但 `src/kiro/provider.rs` 当前只从请求体中提取 `modelId`，没有提取 `conversationId`。`MultiTokenManager` 也没有 `session -> credential` 绑定表，因此相同会话仍会被当前账号选择策略重新调度。

### 目标行为

调度顺序应改为：

1. Provider 从 Kiro request body 提取：
   - `conversationState.conversationId`
   - `conversationState.currentMessage.userInputMessage.modelId`
2. 如果有 `conversationId`，先查会话绑定。
3. 绑定账号满足以下条件时直接使用：
   - 账号存在
   - 未禁用
   - 支持当前模型，例如 Opus 不能落到 Free 账号
   - 不在本次请求的临时排除集合中
   - token 可用或可刷新
4. 如果没有绑定，或绑定账号不可用，再走现有 `priority/balanced`。
5. 首次成功选中账号后建立绑定。
6. 账号硬失败时清理或迁移绑定。
7. 账号软失败时优先重试同账号，必要时只做本次临时 fallback，默认不永久迁移绑定。

### 建议改动

文件：`src/kiro/token_manager.rs`

新增结构：

```rust
struct SessionBinding {
    credential_id: u64,
    last_used_at: chrono::DateTime<chrono::Utc>,
    soft_failure_count: u32,
}
```

新增字段：

```rust
session_bindings: Mutex<HashMap<String, SessionBinding>>
```

建议常量：

```rust
const SESSION_BINDING_TTL_SECS: i64 = 6 * 60 * 60;
const MAX_SESSION_BINDINGS: usize = 10_000;
const MAX_SESSION_SOFT_FAILURES: u32 = 2;
```

新增或调整方法：

```rust
pub async fn acquire_context_for_session(
    &self,
    model: Option<&str>,
    session_id: Option<&str>,
    excluded_ids: &HashSet<u64>,
) -> anyhow::Result<CallContext>
```

```rust
fn select_next_credential_excluding(
    &self,
    model: Option<&str>,
    excluded_ids: &HashSet<u64>,
) -> Option<(u64, KiroCredentials)>
```

```rust
fn credential_is_usable_for_model(
    entry: &CredentialEntry,
    model: Option<&str>,
) -> bool
```

```rust
pub fn bind_session(&self, session_id: &str, credential_id: u64)
pub fn unbind_session(&self, session_id: &str)
pub fn unbind_sessions_for_credential(&self, credential_id: u64)
pub fn record_session_soft_failure(&self, session_id: &str, credential_id: u64) -> bool
```

### 关键细节

1. 当前 `priority` 模式的 `current_hit` 也必须检查模型支持，否则 Opus 请求可能继续使用 Free 账号。
2. `balanced` 只应用于新会话，不能让老会话每次重新均衡。
3. 会话绑定先做内存态，不持久化。服务重启后重新分配是可以接受的，符合当前项目进程内状态模型。
4. 会话绑定清理要轻量，可以在新增绑定时顺手清理过期项。
5. 如果 `conversationId` 是每次请求随机生成的，粘性不会跨请求生效。这是客户端没有稳定 session 的输入限制，不应由服务端用内容 hash 强行猜测。

## P0：Provider 重试改为 sticky-aware

### 现状

`src/kiro/provider.rs` 的 `call_api_with_retry` 当前对 `408/429/5xx` 只重试，不禁用账号，也不切换账号。这避免了误禁用，但在 `priority` 模式下可能反复打同一个被限流账号。

### 目标行为

引入“粘性优先”的重试：

1. 已绑定会话先重试绑定账号。
2. 软失败达到阈值后，本次请求临时排除绑定账号，尝试其他账号。
3. 本次临时 fallback 成功时，默认不重绑，避免一次上游抖动迁移会话。
4. 软失败连续多次超过阈值时，才考虑重绑。
5. 硬失败直接迁移或清理绑定。

### 失败分类

硬失败：

1. `402` 且 `MONTHLY_REQUEST_COUNT`
2. refresh token `invalid_grant`
3. Admin 手动禁用
4. 删除账号
5. 当前账号不支持目标模型
6. endpoint 配置不存在
7. `401/403` 且强制刷新失败并达到失败阈值

软失败：

1. 请求发送失败
2. `408`
3. `429`
4. `5xx`
5. `2xx` 非 eventstream 且 AWS exception 属于 retryable
6. 上游 eventstream idle timeout
7. 上游读流中断

### 建议改动

文件：`src/kiro/provider.rs`

新增：

```rust
fn extract_conversation_id_from_request(request_body: &str) -> Option<String>
```

修改 `call_api_with_retry`：

1. 提取 `model` 和 `conversation_id`。
2. 增加 `excluded_ids: HashSet<u64>`。
3. 调用 `token_manager.acquire_context_for_session(model, session_id, &excluded_ids)`。
4. 硬失败调用 `report_quota_exhausted`、`report_failure` 后清理绑定。
5. 软失败先 `record_session_soft_failure`，达到阈值后将该账号加入 `excluded_ids`，进行本次临时 fallback。

注意：MCP 请求当前没有明确 conversation id。除非后续能从 MCP request body 中稳定提取会话，否则 `call_mcp_with_retry` 暂不纳入 session sticky，只保留现有账号策略。

## 高缓存场景模拟与验证方案

这里的“高缓存”特指 Kiro `metadataEvent.tokenUsage` 返回大量 cache token 的场景，即：

```json
{
  "tokenUsage": {
    "uncachedInputTokens": 1200,
    "cacheReadInputTokens": 180000,
    "cacheWriteInputTokens": 24000,
    "outputTokens": 900,
    "totalTokens": 206100
  }
}
```

当前项目里 cache token 不由本地缓存系统产生，而是由 Kiro 上游在 `metadataEvent` 中报告。项目只负责：

1. 正确解析 `cacheReadInputTokens` 和 `cacheWriteInputTokens`。
2. 将 `input_tokens` 计算为 `uncachedInputTokens + cacheReadInputTokens`。
3. 在 Anthropic 兼容响应中输出：
   - `cache_read_input_tokens`
   - `cache_creation_input_tokens`
4. 在 `metadataEvent` 缺失时继续回退到 `contextUsageEvent` 或本地估算。

### 单元测试模拟

适合验证转换逻辑，不依赖真实 Kiro 账号。

1. 在 `src/kiro/model/events/additional.rs` 增加大数值 metadata 反序列化测试：
   - `uncachedInputTokens = 1200`
   - `cacheReadInputTokens = 180000`
   - `cacheWriteInputTokens = 24000`
   - 断言 `input_tokens() == 181200`
2. 在 `src/anthropic/stream.rs` 增加流式上下文测试：
   - 构造 `Event::Metadata(MetadataEvent { token_usage: Some(...) })`
   - 调用 `generate_final_events()`
   - 断言 `message_delta.usage.input_tokens == 181200`
   - 断言 `message_delta.usage.output_tokens == 900`
3. 在 `BufferedStreamContext` 上增加测试：
   - 处理 metadata 事件后 finish
   - 断言 `message_start.message.usage.cache_read_input_tokens == 180000`
   - 断言 `message_start.message.usage.cache_creation_input_tokens == 24000`

### 集成测试模拟

适合验证 eventstream 解码到 SSE 的完整路径。当前项目已有 AWS eventstream decoder，但没有 test-only encoder。建议添加测试辅助函数，不进入生产路径：

1. 构造 AWS eventstream frame：
   - header `:message-type = event`
   - header `:event-type = metadataEvent`
   - payload 为上面的 JSON
2. 将 frame bytes 切成多个 chunk，喂给 `EventStreamDecoder`。
3. 断言 decoder 可以解析出 `Event::Metadata`，且 cache token 字段无丢失。
4. 再把事件送入 `StreamContext` 或 `BufferedStreamContext`，验证 SSE usage。

### 本地 mock 上游模拟

适合验证 provider、HTTP content-type、非流式/流式处理以及客户端观测结果。

1. 启动一个本地 mock HTTP 服务，返回：
   - `content-type: application/vnd.amazon.eventstream`
   - body 为若干 AWS eventstream frame
2. frame 顺序：
   - `assistantResponseEvent`
   - `metadataEvent`，包含高 `cacheReadInputTokens/cacheWriteInputTokens`
   - 可选 `messageMetadataEvent`
3. 将测试配置中的 endpoint 指向 mock 服务，或在测试中注入一个 test endpoint。
4. 用 `/v1/messages` 和 `/cc/v1/messages` 分别请求：
   - 普通流式：检查最终 `message_delta.usage`
   - 缓冲流式：检查 `message_start.message.usage`
   - 非流式：检查 JSON 响应 `usage`

### 真实上游近似模拟

真实 Kiro cache hit 由上游决定，本地无法强制生成。但可以用以下方式提高出现高 cache read 的概率：

1. 使用同一个 Claude Code session，也就是保持 `metadata.user_id` 中的 `session_id` 不变。
2. 使用同一个模型和同一个 Kiro 账号，避免切账号导致上游侧会话/cache 不连续。
3. 构造较长、重复的上下文，例如连续多轮发送相同大段代码或同一个仓库摘要。
4. 第二轮及后续请求观察日志和响应 usage：
   - `cache_read_input_tokens` 应明显上升。
   - `cache_creation_input_tokens` 通常在首次建立缓存或上下文变化时上升。
5. 用本轮新增的粘性会话调度保证上述请求尽量固定在同一个账号上，否则无法稳定复现高 cache read。

### 验收标准

1. 高 cache read 场景下，`input_tokens` 不能只显示 uncached tokens。
2. `cache_read_input_tokens` 和 `cache_creation_input_tokens` 不能丢失。
3. 普通流式、缓冲流式、非流式三条路径 usage 语义一致。
4. 同一 `conversationId` 的多轮真实请求应尽量使用同一账号，除非账号被禁用、额度用尽、刷新失败或不支持目标模型。

## P0：账号模型过滤修复

### 问题

`select_next_credential(model)` 会过滤 Free 账号以避免 Opus 请求落到不支持账号，但 `acquire_context` 中的 `current_hit` 只检查 `!disabled`。

### 影响

在 `priority` 模式下，如果当前账号是 Free，Opus 请求可能仍使用该账号，绕过 `supports_opus()`。

### 建议改动

文件：`src/kiro/token_manager.rs`

把模型可用性检查抽为统一函数，并用于：

1. `current_hit`
2. `select_next_credential`
3. `select_next_credential_excluding`
4. session binding 命中判断

## P1：流式成功上报延后

### 现状

`src/kiro/provider.rs` 在 HTTP status 和 content-type 通过后立刻 `report_success(ctx.id)`。但真正的 Kiro 失败可能出现在 eventstream 内，例如：

1. `invalidStateEvent`
2. AWS `error` / `exception`
3. 上游读流错误
4. idle timeout
5. decoder 严重错误

### 风险

账号统计会被污染，`balanced` 调度会失真。粘性会话绑定也可能确认到一个实际失败的账号。

### 建议方案

分两步做：

第一步，轻量方案：

1. Provider 保持返回 `reqwest::Response`。
2. 对于 HTTP 层成功，仍记录成功，但 session binding 的最终确认放到 handler 流结束后。
3. 对 eventstream 内错误，只记录日志和 SSE error，不立刻禁用账号。

第二步，完整方案：

将 Provider 返回类型改为：

```rust
pub struct KiroApiResponse {
    pub response: reqwest::Response,
    pub credential_id: u64,
    pub session_id: Option<String>,
    pub fallback_from_sticky: bool,
}
```

handler 在流结束后根据实际结果调用：

```rust
report_success(credential_id)
report_failure(credential_id)
bind_session(session_id, credential_id)
```

建议先做第一步，避免一次改动过大。待粘性调度稳定后再做完整返回包装。

## P1：非流式 eventstream 防护

### 现状

非流式请求在 `src/anthropic/handlers.rs` 中读取完整 body 后使用 `EventStreamDecoder` 解码。上游非流式实际仍是 eventstream 聚合后的结果，但当前 Provider 只对 stream 请求检查 `2xx` 非 eventstream。

### 风险

如果 Kiro 返回 `2xx application/json` 错误体，非流式可能被解码器 warn 后返回空成功。

### 建议改动

文件：`src/kiro/provider.rs`

把 eventstream content-type 检查扩展到非流式 API 调用，因为上游 `generateAssistantResponse` 对本项目来说都应是 AWS eventstream。

文件：`src/anthropic/handlers.rs`

增加兜底：

1. 统计是否解出任何有效 Kiro event。
2. 如果 body 非空、没有有效 event、且 decoder 出错，返回 `502 api_error`。
3. 如果收到 `invalidStateEvent`，保持当前 `400 invalid_request_error` 行为。

## P1：decoder 严重错误收敛

### 现状

`decoder.feed()` 和 `decode_iter()` 的错误多数只是 warn。解码器内部有恢复能力，这是合理的，但严重错误不应被吞掉。

### 建议策略

轻微解析错误：

1. 继续尝试恢复。
2. 记录 warn。

严重解析错误：

1. `BufferOverflow`
2. `TooManyErrors`
3. 全程没有有效帧但 body 非空

处理方式：

1. 流式：关闭已打开 content block，发送 SSE `error`，不再发送正常 `message_delta/message_stop`。
2. 非流式：返回 `502 api_error`。
3. buffered stream：输出已缓冲事件后追加 SSE `error`，不生成正常 stop。

涉及文件：

1. `src/anthropic/handlers.rs`
2. `src/anthropic/stream.rs`
3. `src/kiro/parser/decoder.rs`

## P1：Admin 账号管理优化

### 代理校验

现状：

1. OAuth 添加账号时会在刷新 token 时触发代理构建。
2. API Key 添加账号时，如果 usage 获取失败只记录 warn，可能把代理配置错误的账号加入池。

建议：

文件：`src/admin/service.rs`

在 `add_credential` 构建 `KiroCredentials` 前先校验 `proxyUrl`：

1. `None`：合法，走全局代理。
2. `direct`：合法，显式禁用代理。
3. `http://...`、`https://...`、`socks5://...`：调用 `reqwest::Proxy::all` 或复用 `build_client` 进行本地格式校验。
4. 其他 scheme 或空字符串：返回 `400 InvalidCredential`。

### 余额缓存失效

现状：

`src/admin/service.rs` 只在删除账号时清理 balance cache。

建议在以下操作后清理对应账号缓存：

1. `set_disabled`
2. `set_priority`
3. `reset_and_enable`
4. `force_refresh_token`
5. `add_credential` 后如果主动获取 usage 成功，写入新缓存；如果失败，确保无旧缓存。

新增 helper：

```rust
fn invalidate_balance_cache(&self, id: u64)
```

### Admin 与 session binding 联动

涉及 `src/kiro/token_manager.rs`：

1. `set_disabled(id, true)`：清理该账号所有 session binding。
2. `delete_credential(id)`：清理该账号所有 session binding。
3. `report_quota_exhausted(id)`：清理该账号所有 session binding。
4. `report_refresh_token_invalid(id)`：清理该账号所有 session binding。
5. `reset_and_enable(id)`：不主动恢复旧 binding，让新请求重新分配。

## P1：token refresh 元数据保护

### 现状

`refresh_token()` 本身是基于 `credentials.clone()` 修改 token 字段，因此大多数元数据会保留。但当前代码中多处直接：

```rust
entry.credentials = new_creds;
```

### 风险

如果未来某个 refresh 实现不再 clone 原凭据，或新 endpoint 返回结构不同，容易丢失 endpoint/proxy/email/priority 等用户输入元数据。`add_credential` 已经显式保留这些字段，说明这是实际关注点。

### 建议

新增 helper：

```rust
fn merge_refreshed_credentials(
    existing: &KiroCredentials,
    refreshed: KiroCredentials,
) -> KiroCredentials
```

只从 refreshed 取：

1. `access_token`
2. `refresh_token`
3. `expires_at`
4. `profile_arn`

其他配置元数据保留 existing：

1. `id`
2. `priority`
3. `auth_method`
4. `region/auth_region/api_region`
5. `machine_id`
6. `email`
7. `subscription_title`
8. `proxy_url/proxy_username/proxy_password`
9. `kiro_api_key`
10. `endpoint`
11. `disabled`

应用位置：

1. `try_ensure_token`
2. `get_usage_limits_for`
3. `force_refresh_token_for`
4. `add_credential` 可继续保留当前显式赋值，也可改用该 helper。

## P2：stream usage cache 字段一致性

### 现状

非流式响应和 `/cc/v1/messages` buffered stream 会暴露 cache read/write token。普通 stream 的最终 `message_delta.usage` 当前只包含：

1. `input_tokens`
2. `output_tokens`

### 建议

先确认 Anthropic streaming `message_delta.usage` 是否兼容：

1. `cache_read_input_tokens`
2. `cache_creation_input_tokens`

如果兼容，再调整 `SseStateManager::generate_final_events` 接收完整 usage struct，而不只是 input/output 两个整数。

该项优先级低于调度和错误处理。

## P2：SSE 循环去重

### 现状

`create_sse_stream` 和 `create_buffered_sse_stream` 里重复了：

1. body chunk 读取
2. idle timeout
3. ping 保活
4. decoder feed/decode
5. read error 转 SSE error

### 建议

等 decoder 错误策略稳定后，再提取小 helper，例如：

```rust
fn decode_chunk_events(...)
```

不要过早抽象整个 stream loop。当前重复是可控的，但后续修错误策略时容易漏改。

## 实施顺序

建议按以下顺序落地，避免一次改动过大：

### 阶段 1：粘性调度基础

1. `Provider` 提取 `conversationId`。
2. `TokenManager` 增加 session binding 内存表。
3. 新增 `acquire_context_for_session`。
4. 修复 `current_hit` 模型过滤绕过。
5. 添加单元测试：
   - 同 session 连续请求命中同账号。
   - 新 session 按 priority 分配。
   - balanced 只影响新 session。
   - Opus session 不绑定 Free 账号。
   - 禁用账号后 session 迁移。

### 阶段 2：sticky-aware retry

1. Provider 增加本次请求 `excluded_ids`。
2. 软失败先重试绑定账号。
3. 超过软失败阈值后临时 fallback。
4. 硬失败清理绑定并迁移。
5. 添加测试：
   - `429` 不立刻永久迁移。
   - `402 MONTHLY_REQUEST_COUNT` 清理绑定。
   - retryable AWS exception 允许临时 fallback。

### 阶段 3：Admin 管理增强

1. Admin 添加账号时校验代理 URL。
2. Admin 状态变更时清理 balance cache。
3. TokenManager 在账号禁用、删除、quota exhausted、invalid refresh token 时清理 session binding。
4. 添加测试：
   - invalid proxy 返回 400。
   - `direct` 合法。
   - force refresh 后缓存失效。
   - delete credential 后 session binding 被清理。

### 阶段 4：eventstream 防护

1. 非流式也检查 eventstream content-type。
2. handler 统计有效 Kiro event。
3. decoder 严重错误转 API/SSE error。
4. 添加测试：
   - 2xx JSON error 不返回空成功。
   - decode TooManyErrors 不生成正常 stop。
   - buffered stream error 不伪装成功。

### 阶段 5：使用量与代码整理

1. 验证 stream final usage 是否应包含 cache fields。
2. 小范围去重 SSE loop。
3. 增加未知 Kiro event 的可观测日志。

## 验证命令

当前本地环境需要显式指定 C 编译器和 linker：

```bash
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test --locked kiro::token_manager -- --nocapture
```

```bash
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test --locked anthropic::stream -- --nocapture
```

```bash
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo test --locked kiro::model::events -- --nocapture
```

```bash
CC=/usr/bin/cc RUSTFLAGS='-C linker=/usr/bin/cc' cargo check --locked
```

如涉及 Admin UI，不建议把当前优化依赖在前端构建上。现有环境下 `pnpm build` 可能受 `crypto.getRandomValues` 环境问题影响，应单独处理。

## 验收标准

1. Claude Code 同一个 `metadata.user_id` session 连续请求应稳定使用同一个 Kiro 账号。
2. 没有 session metadata 的普通请求仍保持现有行为，不引入错误粘性。
3. Free 账号不会承接 Opus 请求，即使它是当前账号或已有绑定账号。
4. 账号硬失败后，会话能迁移，不会卡死在不可用账号。
5. 软失败不会立即污染长期绑定。
6. Admin 禁用、删除、强刷、重置后，余额缓存和 session binding 状态一致。
7. 非流式上游异常不会返回空成功。
8. 流式 decoder 严重错误不会继续发送正常完成事件。
