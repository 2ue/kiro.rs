# Kiro 最终改造实施方案

本文档是基于当前仓库实际结构的最终落地实施规格。它必须能在没有聊天上下文、没有额外口头说明的情况下，指导开发者完成代码改造、测试和验收。

当前项目路径：

```text
/Users/yuanfeijie/Desktop/procode/kiro.rs
```

参考项目路径：

```text
/Users/yuanfeijie/Desktop/procode/kiro.rs-new.rs
/Users/yuanfeijie/Desktop/procode/Kiro-Go
```

注意：用户曾提到 `../kiro.rs_new`，本机实际存在的是 `../kiro.rs-new.rs`。

## 背景

当前项目是一个 Anthropic-compatible Kiro 代理服务。服务接收其他服务或客户端发来的 Anthropic `/v1/messages` 请求，将其转换为 Kiro 请求，调用 Kiro 上游，再把 Kiro eventstream 转回 Anthropic JSON 或 SSE。

本次改造的核心目标不是单独“返回高缓存字段”，而是把当前服务作为其他服务上游时需要的闭环能力补齐：

1. 同一会话尽可能固定在同一个 Kiro 账号上完成。
2. 真实 Kiro metadata usage 不丢失。
3. 本地可控模拟高缓存，便于下游服务联调。
4. 每次请求都有 usage 记录，可以在 Admin API/UI 查看。
5. Admin 账号管理和运行观测能解释账号调度、失败、fallback、高缓存变化。

最终交付后，调用方应能完成以下验证：

1. 发送同一个 `conversationId/session` 的连续请求。
2. 服务尽量保持这些请求使用同一个 Kiro credential。
3. 若真实 Kiro 返回高 `cacheReadInputTokens`，响应和 Admin 记录都能看到。
4. 若真实 Kiro 没有返回高缓存，也可以开启本地模拟，让第一轮请求产生 cache creation，第二轮请求产生 cache read。
5. 管理员可以通过 Admin UI/API 看到每条请求的 credential、conversation、source、cache read/cache creation、状态和错误。

## 术语

### Kiro credential

一个 Kiro 账号或 Kiro API Key 凭据。当前项目由 `src/kiro/token_manager.rs` 的 `MultiTokenManager` 管理多个 credential。

### conversationId / session

当前服务用于粘性调度的会话 ID。它最终写入 Kiro request body：

```json
{
  "conversationState": {
    "conversationId": "..."
  }
}
```

Provider 层从 Kiro request body 解析该字段，并用它绑定 `conversationId -> credentialId`。

### sticky session

同一个 `conversationId` 的请求优先使用同一个 Kiro credential。只有 credential 硬失败、额度用尽、被禁用、被删除，或者连续软失败达到阈值时，才允许 fallback。

### upstream metadata

Kiro 上游 eventstream 中的 `metadataEvent.tokenUsage`，包含真实 token usage。例如：

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

该数据是真实来源，优先级最高。

### cache read / cache creation

Anthropic-compatible usage 字段：

```json
{
  "cache_read_input_tokens": 180000,
  "cache_creation_input_tokens": 24000
}
```

`cache_read_input_tokens` 表示读缓存 token，`cache_creation_input_tokens` 表示写缓存 token。Kiro metadata 里的 `cacheWriteInputTokens` 映射为 Anthropic 响应里的 `cache_creation_input_tokens`。

### totalInputTokens

完整输入 token 数，用于服务端记录和聚合。它不一定等于响应给下游的 `usage.input_tokens`。

### compatInputTokens

当前服务实际返回给下游的 `usage.input_tokens`。为了兼容现有行为，第一版不要默认改变响应语义。

### billableInputTokens

更接近 Anthropic 计费语义的输入 token，即：

```text
max(totalInputTokens - cacheReadInputTokens - cacheCreationInputTokens, 0)
```

第一版只记录，不默认替换响应里的 `usage.input_tokens`。

### UsageRecord

服务端记录的一条请求级 usage。它记录请求的模型、endpoint、stream、credential、conversation、source、usage token、状态、错误和耗时。

### UsageSource

usage 数据来源。固定枚举值：

1. `upstream_metadata`
2. `local_prompt_cache`
3. `heuristic_cache_control`
4. `forced_high_cache`
5. `context_estimate`
6. `request_estimate`
7. `none`

### high cache request

高缓存请求。默认定义：

```text
cacheReadInputTokens >= highCacheThreshold
```

默认 `highCacheThreshold = 10000`。

## 非目标

以下内容不在本次最终改造第一阶段范围内：

1. 不引入数据库。
2. 不替换现有 `priority/balanced + sticky session` 调度为 weighted round-robin。
3. 不默认改变下游响应中 `usage.input_tokens` 的兼容语义。
4. 不把本地模拟当作真实 Kiro 收费数据。
5. 不做长期历史报表系统，只做最近请求的 ring buffer 和可选 JSONL。
6. 不新增复杂 overage 调度策略。
7. 不为了高缓存模拟绕过真实 Kiro Provider 调用。

## 当前基础

当前仓库已经具备的基础：

1. `src/kiro/token_manager.rs` 已实现 `conversationId -> credentialId` 粘性会话绑定。
2. `src/kiro/provider.rs` 已能从 Kiro request body 中提取 `conversationId` 并传给 `MultiTokenManager`。
3. `src/kiro/model/events/additional.rs` 已能解析 Kiro `metadataEvent.tokenUsage`，包含 cache read/write token。
4. `src/anthropic/stream.rs` 已有流式状态机和 stream EOF/错误完成判定。
5. `src/admin/service.rs` 已有账号 CRUD、余额缓存、优先级、负载模式管理。
6. 当前工作区已有未最终稳定的 `src/anthropic/cache.rs`，以及 `handlers.rs/stream.rs/mod.rs` 的 cache usage 接入改动。

后续实现不需要推翻这些能力，而是补足记录、配置、Admin 和本地 prompt cache tracker。

## 改造原则

1. 真实上游 metadata 永远优先。
2. 本地模拟默认关闭，生产不会无意制造 cache usage。
3. prompt cache 模拟必须和 sticky session 绑定，不做纯随机高缓存。
4. 失败请求不更新本地 prompt cache。
5. usage record 记录的是服务端事实，必须能区分 usage source。
6. 不引入数据库，先用内存 ring buffer 加可选 JSONL 持久化。
7. Admin UI 增加请求级观测，不替换现有账号管理。
8. 不用 `Kiro-Go` 的 weighted round-robin 替代当前 sticky-aware priority/balanced 调度。

## 固定技术决策

以下是实现时必须遵守的决策，不再作为开放问题处理。

1. Usage 记录模块放在 `src/anthropic/usage.rs`。
2. 本地 prompt cache tracker 放在 `src/anthropic/prompt_cache.rs`。
3. `src/anthropic/cache.rs` 保留为 usage 构建和轻量 heuristic helper，不承担状态化缓存。
4. UsageRecord 由 Anthropic handler/stream 完成时写入，因为最终 usage 在 Anthropic adapter 层形成。
5. Provider 必须暴露本次请求使用的 `credential_id/session_id`，否则 UsageRecord 无法解释账号维度。
6. UsageRecorder 用内存 `VecDeque` ring buffer，默认最多 5000 条。
7. UsageRecorder 可选 JSONL 持久化，默认开启。
8. JSONL 写失败不影响主请求，只记录 warn。
9. 本地模拟默认关闭，配置字段为 `promptCacheSimulationMode`。
10. 本地 prompt cache 的默认 scope 是 `credentialId + conversationId + model`。
11. 真实 metadata 存在时，本地模拟不能覆盖它。
12. `force-high-cache` 仅用于开发/联调；第一版也不覆盖真实 metadata。
13. 失败请求不更新 `PromptCacheTracker`。
14. Admin API 路径必须挂在现有 `/api/admin` 下。
15. Admin UI 在现有 dashboard 增加“缓存记录”视图，不新建独立登录体系。

## 文件改动总览

必须新增：

1. `src/anthropic/usage.rs`
2. `src/anthropic/prompt_cache.rs`
3. `admin-ui/src/api/usage.ts`
4. `admin-ui/src/hooks/use-usage-records.ts`
5. `admin-ui/src/components/usage-records-panel.tsx`

必须修改：

1. `src/anthropic/mod.rs`
2. `src/anthropic/cache.rs`
3. `src/anthropic/middleware.rs`
4. `src/anthropic/handlers.rs`
5. `src/anthropic/stream.rs`
6. `src/model/config.rs`
7. `src/main.rs`
8. `src/kiro/provider.rs`
9. `src/admin/types.rs`
10. `src/admin/service.rs`
11. `src/admin/handlers.rs`
12. `src/admin/router.rs`
13. `admin-ui/src/types/api.ts`
14. `admin-ui/src/components/dashboard.tsx`

可选修改：

1. `src/kiro/token_manager.rs`：如果需要暴露 sticky hit/fallback 状态，可新增只读辅助方法；不要破坏现有选择逻辑。
2. `src/admin/service.rs`：账号禁用/删除时清理 `PromptCacheTracker` 中对应 credential 的缓存，如果 tracker 被注入 AdminService。

## 阶段 0：稳定当前 cache helper

### 目标

把当前已有高缓存辅助改动整理到可维护状态，避免后续实现建立在半接入代码上。

### 文件

1. `src/anthropic/cache.rs`
2. `src/anthropic/mod.rs`
3. `src/anthropic/handlers.rs`
4. `src/anthropic/stream.rs`

### 任务

1. 保留 `CacheUsage`，但补充 source 语义，不只返回数值。
2. 保留 `estimate_cached_message_tokens` 作为 heuristic 模式工具函数。
3. 将 `build_usage` 拆分为更明确的层次：
   - `usage_from_metadata(metadata)`
   - `usage_from_heuristic(total_input_tokens, output_tokens, cached_msg_tokens)`
   - `usage_from_prompt_cache(total_input_tokens, output_tokens, prompt_cache_usage)`
4. 明确字段语义：
   - `total_input_tokens`：用于记录/统计的完整输入估算。
   - `compat_input_tokens`：实际返回给下游的 `usage.input_tokens`。
   - `billable_input_tokens`：更接近 Anthropic 计费语义的输入 token，先只记录，不默认改变响应。
5. 普通流、缓冲流、非流式的 usage 输出保持一致。

### 验收

1. `cargo test anthropic::cache` 通过。
2. 普通 stream final `message_delta.usage` 包含 cache fields。
3. `/cc/v1/messages` buffered stream 的 `message_start.message.usage` 包含最终 cache fields。
4. 非流式 JSON `usage` 包含 cache fields。

## 阶段 1：配置项和共享状态

### 目标

让高缓存模拟和 usage 记录可配置，默认保持生产安全。

### 文件

1. `src/model/config.rs`
2. `src/main.rs`
3. `src/anthropic/middleware.rs`

### Config 新增字段

在 `Config` 增加以下字段，serde JSON 名称使用 camelCase：

```rust
pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
pub usage_record_limit: usize,
pub usage_record_persist: bool,
pub high_cache_threshold: i32,
```

字段对应 JSON：

```json
{
  "promptCacheSimulationMode": "disabled",
  "usageRecordLimit": 5000,
  "usageRecordPersist": true,
  "highCacheThreshold": 10000
}
```

枚举定义：

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PromptCacheSimulationMode {
    Disabled,
    HeuristicCacheControl,
    LocalPromptCache,
    ForceHighCache,
}
```

JSON 允许值固定为：

1. `disabled`
2. `heuristic-cache-control`
3. `local-prompt-cache`
4. `force-high-cache`

默认值：

1. `prompt_cache_simulation_mode = Disabled`
2. `usage_record_limit = 5000`
3. `usage_record_persist = true`
4. `high_cache_threshold = 10000`

必须增加 default 函数：

```rust
fn default_prompt_cache_simulation_mode() -> PromptCacheSimulationMode {
    PromptCacheSimulationMode::Disabled
}

fn default_usage_record_limit() -> usize {
    5000
}

fn default_usage_record_persist() -> bool {
    true
}

fn default_high_cache_threshold() -> i32 {
    10_000
}
```

### AppState 扩展

`src/anthropic/middleware.rs` 的 `AppState` 增加：

1. `usage_recorder: Arc<UsageRecorder>`
2. `prompt_cache: Arc<PromptCacheTracker>`
3. `prompt_cache_simulation_mode: PromptCacheSimulationMode`
4. `high_cache_threshold: i32`

现有 `AppState::new` 和 `with_kiro_provider` 需要调整签名。调用点主要在 `src/main.rs` 和测试。

### main.rs 接入

推荐初始化路径：

```rust
let usage_recorder = Arc::new(UsageRecorder::new(
    config.usage_record_limit,
    if config.usage_record_persist {
        token_manager.cache_dir().map(|d| d.join("kiro_usage_records.jsonl"))
    } else {
        None
    },
));

let prompt_cache = Arc::new(PromptCacheTracker::default());
```

如果 `token_manager.cache_dir()` 不可用，`persist_path` 使用 `None`，不阻断启动。

创建 Anthropic router 时传入：

1. `usage_recorder`
2. `prompt_cache`
3. `prompt_cache_simulation_mode`
4. `high_cache_threshold`

创建 AdminService 时传入同一个 `usage_recorder`，保证 API 请求记录和 Admin 查询来自同一个实例。

### 验收

1. 旧配置文件不加新字段也能启动。
2. 新字段通过 serde default 生效。
3. Admin API 和 Anthropic API 使用同一个 recorder 实例。
4. 默认模式不会启用本地 cache 模拟。

## 阶段 2：UsageRecorder

### 目标

把每次请求的 usage、账号、会话、状态记录下来，支持 Admin 查询。

### 新增文件

```text
src/anthropic/usage.rs
```

### 核心结构

```rust
pub struct UsageRecorder {
    records: Mutex<VecDeque<UsageRecord>>,
    limit: usize,
    persist_path: Option<PathBuf>,
}
```

### UsageRecord

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: String,
    pub created_at: String,
    pub endpoint: String,
    pub stream: bool,
    pub model: String,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub credential_label: Option<String>,
    pub status: UsageRecordStatus,
    pub usage_source: UsageSource,
    pub total_input_tokens: i32,
    pub compat_input_tokens: i32,
    pub billable_input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
    pub duration_ms: u64,
    pub simulated: bool,
    pub sticky_bound: bool,
    pub fallback_from_sticky: bool,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
}
```

说明：

1. `created_at` 使用 RFC3339 字符串。
2. `duration_ms` 输出 `u64`，避免前端 number 语义问题。
3. `credential_label` 优先使用 email；没有 email 时使用 `#credential_id`；禁止输出 refresh token/access token/API key。
4. `sticky_bound` 第一版如果无法可靠获取，可默认 false，但字段必须保留。
5. `fallback_from_sticky` 第一版如果无法可靠获取，可默认 false，但字段必须保留。

### UsageRecordStatus

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageRecordStatus {
    Success,
    Error,
    StreamError,
    UpstreamTimeout,
    ClientDropped,
}
```

JSON 值：

1. `success`
2. `error`
3. `stream_error`
4. `upstream_timeout`
5. `client_dropped`

### UsageSource

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    UpstreamMetadata,
    LocalPromptCache,
    HeuristicCacheControl,
    ForcedHighCache,
    ContextEstimate,
    RequestEstimate,
    None,
}
```

JSON 值：

1. `upstream_metadata`
2. `local_prompt_cache`
3. `heuristic_cache_control`
4. `forced_high_cache`
5. `context_estimate`
6. `request_estimate`
7. `none`

### UsageRecordQuery

```rust
pub struct UsageRecordQuery {
    pub limit: usize,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub model: Option<String>,
    pub status: Option<UsageRecordStatus>,
    pub source: Option<UsageSource>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<String>,
    pub until: Option<String>,
}
```

查询默认值：

1. `limit` 默认 100。
2. `limit` 最大 1000，超过时截断到 1000。
3. `min_cache_read` 缺省表示不过滤。
4. 字符串过滤第一版使用精确匹配，避免歧义。
5. `since/until` 使用 RFC3339 字符串，解析失败由 Admin API 返回 400。

### UsageSummary

```rust
pub struct UsageSummary {
    pub total_requests: usize,
    pub success_requests: usize,
    pub error_requests: usize,
    pub high_cache_requests: usize,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_input_tokens: i64,
    pub total_cache_creation_input_tokens: i64,
    pub simulated_requests: usize,
    pub upstream_metadata_requests: usize,
    pub top_credentials: Vec<UsageAggregate>,
    pub top_conversations: Vec<UsageAggregate>,
}

pub struct UsageAggregate {
    pub key: String,
    pub label: Option<String>,
    pub requests: usize,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
}
```

### 持久化

1. 内存 ring buffer 是主路径。
2. 如果 `usage_record_persist = true`，每条记录 append 到 JSONL。
3. JSONL 每行是一条完整 `UsageRecord` JSON。
4. 启动时加载 JSONL 最后 `usage_record_limit` 条。
5. JSONL 写失败只 warn，不影响请求。
6. JSONL 损坏行跳过并 warn。
7. clear records 时必须清空内存，并 truncate 当前 JSONL 文件。

### 验收

1. recorder 能按 limit 截断。
2. query 能按 credential/conversation/source/status/minCacheRead 过滤。
3. summary 能正确聚合高缓存请求。
4. JSONL 损坏行会被跳过并 warn。
5. clear 能清空内存和 JSONL。

## 阶段 3：Provider 暴露调用上下文

### 目标

让 UsageRecord 知道本次请求实际用了哪个 Kiro 账号。

### 文件

1. `src/kiro/provider.rs`
2. `src/anthropic/handlers.rs`

### 流式改造

`KiroStreamCompletion` 增加：

```rust
pub fn credential_id(&self) -> u64
pub fn session_id(&self) -> Option<&str>
```

`handle_stream_request` 从 completion 读取 credential/session 写入 record context。

### 非流式改造

新增结构：

```rust
pub struct KiroApiResponse {
    pub response: reqwest::Response,
    pub credential_id: u64,
    pub session_id: Option<String>,
}
```

新增方法：

```rust
pub async fn call_api_with_context(&self, request_body: &str) -> anyhow::Result<KiroApiResponse>
```

保留旧方法：

```rust
pub async fn call_api(&self, request_body: &str) -> anyhow::Result<reqwest::Response>
```

旧方法内部调用 `call_api_with_context` 并只返回 response，降低破坏面。

### 验收

1. 非流式 success record 有 credential_id。
2. 流式 EOF success record 有 credential_id。
3. 发生上游调用前错误时，record 可没有 credential_id，但 status 必须是 error。
4. 现有 provider 调用方仍能使用 `call_api`。

## 阶段 4：PromptCacheTracker

### 目标

实现可重复的本地高缓存模拟：同账号、同会话、同模型、同稳定 prefix，第一轮 creation，第二轮 read。

### 新增文件

```text
src/anthropic/prompt_cache.rs
```

### 借鉴来源

主要参考：

1. `~/Desktop/procode/Kiro-Go/proxy/cache_tracker.go`
2. `~/Desktop/procode/Kiro-Go/proxy/cache_tracker_test.go`

### 核心结构

```rust
pub struct PromptCacheTracker {
    entries: Mutex<HashMap<PromptCacheScope, HashMap<[u8; 32], PromptCacheEntry>>>,
    max_supported_ttl: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PromptCacheScope {
    pub credential_id: u64,
    pub conversation_id: String,
    pub model: String,
}

pub struct PromptCacheEntry {
    pub expires_at: DateTime<Utc>,
    pub ttl: Duration,
}
```

Scope 规则：

1. 有 `conversation_id`：scope = `credential_id + conversation_id + model`。
2. 无 `conversation_id`：第一版不启用 `local_prompt_cache`，回退到 `heuristic_cache_control` 或 request estimate，避免无会话请求互相污染。

### PromptCacheUsage

```rust
pub struct PromptCacheUsage {
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}
```

### Profile 构建

从原始 `MessagesRequest` 构建：

1. request prelude：model、tool_choice 等稳定信息。
2. tools。
3. system。
4. messages。

规则：

1. 显式 `cache_control: {"type":"ephemeral"}` 形成 breakpoint。
2. 一旦出现显式 breakpoint，后续 message-end 形成隐式 breakpoint。
3. canonical JSON 剔除 `cache_control`。
4. 忽略 Claude Code `x-anthropic-billing-header:` 文本块。
5. 忽略外层 `tool_index/system_index/message_index/block_index` 位置字段，但不忽略 block 内语义字段。
6. 使用 SHA-256 fingerprint。

### Compute 规则

1. 默认最小缓存 token：1024。
2. Opus 最小缓存 token：4096。
3. TTL 归一化：`<=5m` 记 5m，`>5m` 记 1h，最高 1h。
4. 最多 cache total input 的 85%，避免 100% cache hit。
5. 首次请求：creation > 0，read = 0。
6. 后续请求：命中 fingerprint 后 read > 0，未命中的新 prefix 部分 creation > 0。

`billableInputTokens` 计算：

```text
max(totalInputTokens - cacheCreationInputTokens - cacheReadInputTokens, 0)
```

### Update 规则

只在请求成功后 update：

1. 非流式成功。
2. 普通流 EOF 且无 stream_error。
3. 缓冲流成功完成。

不 update：

1. 上游请求失败。
2. decode 失败导致 error。
3. read error。
4. idle timeout。
5. client drop。

### 清理规则

1. 每次 compute/update 前清理过期 entry。
2. Admin 禁用/删除 credential 时，如果 PromptCacheTracker 被注入 AdminService，应清理该 credential 的所有 scope。

### 验收

1. first request creation，second request read。
2. 不同 credential 不共享缓存。
3. 不同 conversation 默认不共享缓存。
4. 同 conversation 切账号后不命中。
5. billing header 变化不破坏命中。
6. TTL 过期后不命中。
7. Opus 低于 4096 token 不产生缓存。

## 阶段 5：Anthropic handler 接入

### 目标

让普通流、缓冲流、非流式都能生成一致 usage，并记录 UsageRecord。

### 文件

1. `src/anthropic/handlers.rs`
2. `src/anthropic/stream.rs`
3. `src/anthropic/cache.rs`
4. `src/anthropic/prompt_cache.rs`
5. `src/anthropic/usage.rs`

### 请求进入时

在 `payload.system/messages/tools` 被 move 前：

1. 生成 request id。
2. 记录 start time。
3. 估算 input tokens。
4. 构建 heuristic cache info。
5. 构建 local prompt cache profile。
6. 从 conversion result 或 Kiro request body 取得 conversation id。

### Provider 返回后

拿到 credential id：

1. local prompt cache 模式下 compute usage。
2. 存入 stream/non-stream context。
3. 后续 metadata 如果出现，覆盖模拟 usage。

### source 优先级

最终 usage source：

1. `upstream_metadata`
2. `local_prompt_cache`
3. `heuristic_cache_control`
4. `context_estimate`
5. `request_estimate`
6. `none`

特殊规则：

1. `force-high-cache` 模式仅在 `promptCacheSimulationMode = "force-high-cache"` 时启用。
2. 即使开启 `force-high-cache`，第一版也不覆盖真实 `upstream_metadata`，避免污染真实上游数据。
3. 如后续确实需要覆盖 metadata，必须新增单独配置字段，例如 `forceHighCacheOverrideMetadata`，默认 false。本次不实现。

### 非流式流程

1. 调用 `call_api_with_context`。
2. decode Kiro eventstream。
3. metadata/context/output 聚合。
4. 生成最终 usage。
5. 成功时 update local prompt cache。
6. 写 UsageRecord。

失败路径：

1. Provider error：写 error record。
2. body read error：写 error record。
3. invalidState：写 error record。
4. decode 部分失败但能完成时，按当前行为处理，同时记录 warn。

### 普通流流程

1. 调用 `call_api_stream`。
2. 从 completion 读取 credential/session。
3. 创建 `StreamContext` 时带入 usage context。
4. metadata 到达后覆盖模拟 usage。
5. EOF 无错误：report success，update prompt cache，写 success record。
6. read error：report soft failure，写 `stream_error` record。
7. idle timeout：report soft failure，写 `upstream_timeout` record。
8. client drop：`KiroStreamCompletion::drop` 已 soft failure；第一版如果难以精确写 UsageRecord，可先记录明确 read error/timeout/upstream error，client drop 作为后续补强。

### 缓冲流流程

1. 和普通流一样处理 event。
2. finish 后回填 message_start usage。
3. 成功时 update prompt cache。
4. 写 UsageRecord。

### 验收

1. 三条路径都能产生 usage record。
2. 三条路径真实 metadata 都优先。
3. 三条路径本地模拟字段一致。
4. 流错误不会写 success record。
5. 失败请求不会 update prompt cache。

## 阶段 6：Admin API

### 目标

让外部管理端可以查询和清理 usage records。

### 文件

1. `src/admin/types.rs`
2. `src/admin/service.rs`
3. `src/admin/handlers.rs`
4. `src/admin/router.rs`

### AdminService 改造

`AdminService` 增加：

```rust
usage_recorder: Arc<UsageRecorder>,
high_cache_threshold: i32,
```

构造函数调整：

```rust
AdminService::new(token_manager, endpoint_names, usage_recorder, high_cache_threshold)
```

### 新增 API

在 `src/admin/router.rs` 注册以下路由。因为 router nest 在 `/api/admin` 下，代码中的 route path 不带 `/api/admin` 前缀。

1. `GET /usage-records`
2. `GET /usage-summary`
3. `POST /usage-records/clear`

对外完整路径：

1. `GET /api/admin/usage-records`
2. `GET /api/admin/usage-summary`
3. `POST /api/admin/usage-records/clear`

### 查询参数契约

`GET /api/admin/usage-records` 支持：

```text
limit: number, default 100, max 1000
conversationId: string
credentialId: number
model: string
status: success|error|stream_error|upstream_timeout|client_dropped
source: upstream_metadata|local_prompt_cache|heuristic_cache_control|forced_high_cache|context_estimate|request_estimate|none
stream: true|false
minCacheRead: number
since: RFC3339 string
until: RFC3339 string
```

无效查询参数处理：

1. enum 值非法：返回 400。
2. 数字解析失败：返回 400。
3. `since/until` 时间解析失败：返回 400。
4. `limit <= 0`：使用默认 100。

### UsageRecordsResponse

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsResponse {
    pub total: usize,
    pub records: Vec<UsageRecordItem>,
}
```

`UsageRecordItem` 使用 camelCase JSON。

返回示例：

```json
{
  "total": 1,
  "records": [
    {
      "id": "req_01h",
      "createdAt": "2026-05-16T01:00:00Z",
      "endpoint": "/v1/messages",
      "stream": true,
      "model": "claude-sonnet-4-5-20250929",
      "conversationId": "session-high-cache-001",
      "credentialId": 3,
      "credentialLabel": "user@example.com",
      "status": "success",
      "usageSource": "local_prompt_cache",
      "totalInputTokens": 200000,
      "compatInputTokens": 20000,
      "billableInputTokens": 20000,
      "outputTokens": 900,
      "cacheReadInputTokens": 160000,
      "cacheCreationInputTokens": 20000,
      "cacheCreation5mInputTokens": 20000,
      "cacheCreation1hInputTokens": 0,
      "durationMs": 1200,
      "simulated": true,
      "stickyBound": true,
      "fallbackFromSticky": false,
      "errorType": null,
      "errorMessage": null
    }
  ]
}
```

### UsageSummaryResponse

字段：

1. `totalRequests`
2. `successRequests`
3. `errorRequests`
4. `highCacheRequests`
5. `totalInputTokens`
6. `totalOutputTokens`
7. `totalCacheReadInputTokens`
8. `totalCacheCreationInputTokens`
9. `simulatedRequests`
10. `upstreamMetadataRequests`
11. `topCredentials`
12. `topConversations`

返回示例：

```json
{
  "totalRequests": 10,
  "successRequests": 8,
  "errorRequests": 2,
  "highCacheRequests": 3,
  "totalInputTokens": 1000000,
  "totalOutputTokens": 9000,
  "totalCacheReadInputTokens": 480000,
  "totalCacheCreationInputTokens": 60000,
  "simulatedRequests": 4,
  "upstreamMetadataRequests": 6,
  "topCredentials": [
    {
      "key": "3",
      "label": "user@example.com",
      "requests": 5,
      "cacheReadInputTokens": 300000,
      "cacheCreationInputTokens": 40000
    }
  ],
  "topConversations": [
    {
      "key": "session-high-cache-001",
      "label": null,
      "requests": 3,
      "cacheReadInputTokens": 250000,
      "cacheCreationInputTokens": 20000
    }
  ]
}
```

### 验收

1. 不带参数默认返回最近 100 条。
2. `minCacheRead` 能筛高缓存。
3. `credentialId`、`conversationId`、`source`、`status` 能过滤。
4. clear 后列表为空，summary 归零。
5. 认证仍复用现有 admin auth。

## 阶段 7：Admin UI

### 目标

在管理页面里直接查看高缓存使用记录和汇总。

### 文件

新增：

1. `admin-ui/src/api/usage.ts`
2. `admin-ui/src/hooks/use-usage-records.ts`
3. `admin-ui/src/components/usage-records-panel.tsx`

修改：

1. `admin-ui/src/types/api.ts`
2. `admin-ui/src/components/dashboard.tsx`

### 页面结构

在 dashboard 顶部增加 tab/segmented control：

1. 账号管理
2. 缓存记录

缓存记录视图包括：

1. Summary cards。
2. 筛选栏。
3. usage record table。
4. 清空记录按钮。

### 表格字段

1. 时间。
2. 状态。
3. source。
4. simulated。
5. model。
6. stream。
7. credential。
8. conversationId。
9. input/output。
10. cache read。
11. cache creation。
12. duration。
13. error message。

### UI 约束

1. 不做营销式页面。
2. 表格信息密度优先。
3. 高 cache read 用 badge 高亮。
4. `simulated=true` 必须明显标识。
5. 错误行用状态 badge，不弹大量 toast。
6. 请求失败展示后端错误 message。

### 前端类型契约

`admin-ui/src/types/api.ts` 新增类型必须与 Admin API camelCase 返回一致：

```ts
export type UsageSource =
  | 'upstream_metadata'
  | 'local_prompt_cache'
  | 'heuristic_cache_control'
  | 'forced_high_cache'
  | 'context_estimate'
  | 'request_estimate'
  | 'none'

export type UsageRecordStatus =
  | 'success'
  | 'error'
  | 'stream_error'
  | 'upstream_timeout'
  | 'client_dropped'

export interface UsageRecordItem {
  id: string
  createdAt: string
  endpoint: string
  stream: boolean
  model: string
  conversationId?: string
  credentialId?: number
  credentialLabel?: string
  status: UsageRecordStatus
  usageSource: UsageSource
  totalInputTokens: number
  compatInputTokens: number
  billableInputTokens: number
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  cacheCreation5mInputTokens: number
  cacheCreation1hInputTokens: number
  durationMs: number
  simulated: boolean
  stickyBound: boolean
  fallbackFromSticky: boolean
  errorType?: string
  errorMessage?: string
}
```

### 验收

1. 管理页能切到缓存记录。
2. 能按 min cache read 筛选。
3. 能按 source/status 筛选。
4. summary 和列表刷新一致。
5. 清空记录会同步刷新 summary 和 table。

## 阶段 8：账号管理增强

### 目标

补足和当前核心逻辑相关的账号管理能力，不引入过度复杂调度。

### 后端

优先优化：

1. Admin credential item 增加 usage record 聚合字段：
   - `recentRequests`
   - `recentCacheReadTokens`
   - `recentErrors`
2. 账号禁用/删除时清理 prompt cache tracker 中该 credential 的 entries。
3. 手动切换 load balancing mode 时不清空 session binding。
4. summary 中要能看到 sticky 命中和 fallback 字段，为后续展示预留。

暂不做：

1. weighted round-robin。
2. 复杂 overage 策略。
3. 数据库级历史统计。

### 前端

优先优化：

1. 账号卡展示最近 cache read。
2. 账号卡展示最近错误数量。
3. 删除/禁用账号后刷新 usage summary。

## 阶段 9：测试计划

### Rust 单元测试

新增/补充：

1. `anthropic::cache`：
   - metadata 优先。
   - heuristic 总和一致。
   - cache_control null 忽略。
2. `anthropic::prompt_cache`：
   - first creation / second read。
   - credential 隔离。
   - conversation 隔离。
   - billing header drift 不破坏命中。
   - TTL 过期。
   - Opus 4096 阈值。
3. `anthropic::usage`：
   - ring buffer limit。
   - query filters。
   - summary aggregation。
   - JSONL bad line 跳过。
4. `kiro::provider`：
   - stream completion accessor。
   - non-stream context response credential id。
5. `admin`：
   - usage records query。
   - usage summary。
   - clear records。

### 集成测试

建议补 mock eventstream：

1. metadata 高 cache read。
2. 无 metadata 但 local prompt cache enabled。
3. stream read error。
4. idle timeout 可用更小 timeout 或抽象 clock 后测。

### Admin UI 测试

最低要求：

1. `pnpm typecheck`
2. `pnpm build`

如果项目没有测试框架，先不引入新框架。

### 必跑命令

实现完成后至少运行：

```bash
cargo fmt --check
cargo test
cd admin-ui && pnpm typecheck && pnpm build
```

如果 `cargo fmt --check` 因既有格式问题失败，允许先运行 `cargo fmt`，但必须确认没有无关大规模重排。

## 高缓存手工验收流程

该流程是最终手工验收标准之一。

### 配置

示例配置片段：

```json
{
  "promptCacheSimulationMode": "local-prompt-cache",
  "usageRecordLimit": 5000,
  "usageRecordPersist": true,
  "highCacheThreshold": 10000
}
```

### 请求要求

1. 两次请求使用同一个 API key。
2. 两次请求使用同一个 model。
3. 两次请求使用同一个 `metadata.user_id` 或其他当前 converter 能稳定转成 `conversationId` 的字段。
4. system 或早期 message block 带长文本和 `cache_control: {"type":"ephemeral"}`。
5. 第二次请求保留第一轮稳定 prefix，只追加新用户问题。

### 验收步骤

1. 启动服务。
2. 发送第一轮长 system + `cache_control` 请求。
3. 查询 `/api/admin/usage-records?conversationId=...`。
4. 断言第一轮 `cacheCreationInputTokens > 0` 且 `cacheReadInputTokens = 0`。
5. 发送第二轮同 session 请求。
6. 断言第二轮 `cacheReadInputTokens > 0`。
7. 查询 `/api/admin/usage-summary`，断言 high cache count 增加。

## 风险与边界

### input_tokens 兼容性

当前项目已有测试和行为把 metadata 的 `input_tokens` 视为 `uncached + cache_read`。Anthropic 标准语义更偏 billable input。短期不要默认改响应语义，先在 UsageRecord 里同时保存：

1. `totalInputTokens`
2. `compatInputTokens`
3. `billableInputTokens`

如果后续要切换响应语义，必须加配置开关并单独测试。

### client drop 精确记录

普通 SSE 流如果客户端中断，当前 `KiroStreamCompletion::drop` 能 soft failure，但 UsageRecord 如果只在生成最终事件时写，可能捕捉不到 drop。第一版可以先记录明确 read error/timeout/upstream error，client drop 作为后续补强。

### JSONL 持久化大小

JSONL append 会增长。第一版只加载最后 N 条，不做自动 truncate。后续可增加轮转：

1. 超过 50MB 轮转。
2. 保留最近 3 个文件。

### prompt cache 真实度

本地 prompt cache 是模拟，不代表 Kiro 真实收费。Admin UI 必须明确标记 source/simulated，避免误解。

## Definition of Done

以下验收标准是最终改造完成的 Definition of Done。所有必须项通过后，才能认为最终方案落地完成。

### 功能验收

1. 默认配置下不制造本地 cache usage。
2. 真实 metadata 高缓存被响应和 UsageRecord 保留。
3. `heuristic-cache-control` 模式能快速生成 cache fields。
4. `local-prompt-cache` 模式能 first creation / second read。
5. 同会话 sticky 账号不被破坏。
6. 失败请求不 update prompt cache。
7. Admin API 能查 usage records 和 summary。
8. Admin UI 能查看、筛选、清空缓存记录。
9. UsageRecord 中可以看到 `credentialId`、`conversationId`、`usageSource`、`simulated`。
10. `local-prompt-cache` 模式下，同 credential、同 conversation、同 model 的第二轮请求能产生 `cacheReadInputTokens > 0`。
11. 切换 credential 或 conversation 后不误命中上一会话缓存。
12. Admin summary 中 high cache count 与 `highCacheThreshold` 一致。

### 质量验收

1. `cargo fmt --check` 通过。
2. `cargo test` 通过。
3. Admin UI typecheck/build 通过。
4. 不引入数据库或复杂外部依赖。
5. 不破坏现有账号 CRUD、余额查询、负载模式管理。
6. 不破坏现有 `/v1/messages`、`/cc/v1/messages`、`/v1/messages/count_tokens` 路由。
7. 不输出敏感 credential 字段到 Admin API。
8. JSONL 持久化失败不影响正常请求。

### API 验收

1. `GET /api/admin/usage-records` 未认证时返回 401。
2. `GET /api/admin/usage-records` 认证后默认返回最近记录。
3. `GET /api/admin/usage-records?minCacheRead=10000` 只返回高缓存记录。
4. `GET /api/admin/usage-records?source=local_prompt_cache` 只返回本地 prompt cache 模拟记录。
5. `GET /api/admin/usage-summary` 返回聚合值。
6. `POST /api/admin/usage-records/clear` 清空记录，并使 summary 归零。

### SSE 验收

1. 普通流成功结束时，最终 `message_delta.usage` 包含 `cache_read_input_tokens` 和 `cache_creation_input_tokens`。
2. 缓冲流成功结束时，`message_start.message.usage` 被回填为最终 usage。
3. 流式 read error/idle timeout 不写 success record。
4. 流式 EOF 无错误才 update prompt cache。

## 推荐实施顺序

建议按以下顺序提交：

1. `UsageRecorder` 基础结构和测试。
2. Provider 暴露 credential/session context。
3. handler/stream/non-stream 写 usage records。
4. Admin usage records/summary API。
5. prompt cache simulation mode 配置化。
6. local prompt cache tracker。
7. Admin UI 缓存记录页面。
8. 账号管理聚合增强。

这样每一步都能独立验证，且不会一次性把调度、流式、Admin UI 全部搅在一起。

## 本地实施落地记录

本节记录当前仓库已经落地的最终方案，供后续没有上下文的维护者直接接手。

### 已落地文件

1. `src/model/config.rs`
   - 增加 `PromptCacheSimulationMode`。
   - 增加 `promptCacheSimulationMode`、`usageRecordLimit`、`usageRecordPersist`、`highCacheThreshold`。
   - 默认 `promptCacheSimulationMode = disabled`，不改变生产行为。

2. `src/anthropic/cache.rs`
   - 增加 `CacheSimulation` 和扩展后的 `CacheUsage`。
   - 真实 Kiro `metadataEvent.tokenUsage` 始终优先。
   - 本地模拟只在没有 metadata 时生效。
   - usage JSON 同时输出 `cache_creation_input_tokens`、`cache_read_input_tokens`、`cache_creation_5m_input_tokens`、`cache_creation_1h_input_tokens`。

3. `src/anthropic/prompt_cache.rs`
   - 实现本地 prompt cache tracker。
   - scope 为 `credential_id + conversation_id + model`，与 sticky session 保持一致。
   - 支持 first creation / second read。
   - 支持 5m/1h TTL、模型最小 cache token、canonical JSON、忽略 `cache_control` 和 billing header。
   - `SystemMessage` 已保留 `cache_control`，system block 可参与本地 prompt-cache 模拟。

4. `src/anthropic/usage.rs`
   - 实现 `UsageRecorder` 内存 ring buffer。
   - 支持 JSONL append 持久化，启动时加载最近 N 条。
   - 支持 query/filter/summary/clear。

5. `src/kiro/provider.rs`
   - 新增 `KiroApiResponse`。
   - 新增 `call_api_with_context`，非流式也能拿到实际 `credential_id/session_id`。
   - `KiroStreamCompletion` 暴露 `credential_id()` 和 `session_id()`。
   - 新增 `credential_label()`，仅返回脱敏 label。

6. `src/anthropic/handlers.rs`
   - `/v1/messages` 和 `/cc/v1/messages` 都会构造 request usage context。
   - 非流式成功/失败写 UsageRecord。
   - 普通流和缓冲流在 EOF/error/timeout 后写 UsageRecord。
   - 只有成功且 source 为 `local_prompt_cache` 时才 update tracker。
   - 本地模拟按配置生效，默认关闭。

7. `src/anthropic/stream.rs`
   - `StreamContext`/`BufferedStreamContext` 支持 `CacheSimulation` 注入。
   - 保存最终 usage 快照，供 handler 记录。
   - 缓冲流会继续回填 `message_start.message.usage`。

8. `src/admin/service.rs`
   - 注入 `UsageRecorder`、`PromptCacheTracker` 和 `highCacheThreshold`。
   - 禁用/删除 credential 时清理该 credential 的本地 prompt-cache scope。
   - 增加 usage records/summary/clear service 方法。

9. `src/admin/handlers.rs`、`src/admin/router.rs`
   - 新增 `GET /api/admin/usage-records`。
   - 新增 `GET /api/admin/usage-summary`。
   - 新增 `POST /api/admin/usage-records/clear`。
   - 复用现有 Admin API key 鉴权。

10. `admin-ui/src/*`
    - 增加 usage API、React Query hooks、Usage 管理面板。
    - Dashboard 增加 `凭据 / Usage` 页签。
    - Usage 面板支持汇总、筛选、刷新、清空、记录表格展示。

### 当前行为约束

1. 默认配置不会制造本地高缓存 usage。
2. 如果上游返回 metadata usage，响应和 UsageRecord 都以 metadata 为准。
3. `local-prompt-cache` 只在同 credential、同 conversation、同 model 下命中。
4. 没有可从 `metadata.user_id` 提取的 stable conversation id 时，`local-prompt-cache` 不跨会话猜测命中；会退回启发式或普通估算。
5. 失败请求不更新 prompt cache。
6. 流式 read error/idle timeout 不写 success record。
7. JSONL 持久化失败只记录 warn，不阻断 API 请求。

### 高缓存模拟配置

推荐用于上游服务联调的配置：

```json
{
  "promptCacheSimulationMode": "local-prompt-cache",
  "usageRecordLimit": 5000,
  "usageRecordPersist": true,
  "highCacheThreshold": 10000
}
```

快速造数可使用：

```json
{
  "promptCacheSimulationMode": "heuristic-cache-control"
}
```

压测 Admin 高缓存展示可使用：

```json
{
  "promptCacheSimulationMode": "force-high-cache"
}
```

`force-high-cache` 仅用于模拟和展示链路验证，不应作为生产默认值。

### 实际验收命令

本地环境 PATH 中存在 Volta 的 `cc`，会导致 Rust 链接阶段失败。验证 Rust 时需要临时指定 macOS SDK 和 clang：

```bash
SDKROOT=$(xcrun --sdk macosx --show-sdk-path) \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=$(xcrun --find clang) \
CC=$(xcrun --find clang) \
cargo check
```

```bash
SDKROOT=$(xcrun --sdk macosx --show-sdk-path) \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=$(xcrun --find clang) \
CC=$(xcrun --find clang) \
cargo test
```

Admin UI 验证：

```bash
cd admin-ui
pnpm build
```

已通过的本地验收结果：

1. `cargo check` 通过。
2. `cargo test` 通过，230 个测试全部通过。
3. `admin-ui pnpm build` 通过。
4. `git diff --check` 通过。

### 手工联调步骤

1. 设置 `promptCacheSimulationMode` 为 `local-prompt-cache`。
2. 启动服务。
3. 下游服务用固定 `metadata.user_id` 发第一轮请求，长 system 或早期 message block 带 `cache_control`。
4. 查询 `GET /api/admin/usage-records?conversationId=<session>`。
5. 第一轮应看到 `usageSource = local_prompt_cache` 且 `cacheCreationInputTokens > 0`。
6. 同一 session、同一 model、同一账号 sticky 下发第二轮请求。
7. 第二轮应看到 `cacheReadInputTokens > 0`。
8. 查询 `GET /api/admin/usage-summary`，确认 `highCacheRequests` 按 `highCacheThreshold` 增加。
