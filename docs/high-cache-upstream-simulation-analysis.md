# Kiro 高缓存上游模拟与使用记录方案

本文档针对当前 `kiro.rs` 项目作为“被其他服务调用的上游服务”这一定位，分析 Kiro 账号管理、代理、调度、SSE/eventstream、usage/cache token、Admin 管理，以及如何模拟和查看高 prompt-cache 使用记录。

分析对象：

1. 当前仓库：`/Users/yuanfeijie/Desktop/procode/kiro.rs`
2. 参考 Rust 项目：用户提到的 `../kiro.rs_new` 在本机不存在，实际可对比路径为 `../kiro.rs-new.rs`
3. 参考 Go 项目：`~/Desktop/procode/Kiro-Go`

结论先行：

1. 当前项目核心链路已经具备高缓存场景的基础：会话粘性调度、Kiro `metadataEvent.tokenUsage` 解析、普通流/缓冲流/非流式 usage 透出方向都已经有基础。
2. 当前项目最大的缺口不是“能不能返回 cache 字段”，而是“作为上游服务时，能不能稳定模拟高缓存，并能在 Admin 里追踪每次请求的高缓存记录”。
3. `../kiro.rs-new.rs` 的价值是轻量级 `cache_control` 启发式模拟，适合 P0/P1 快速兜底。
4. `Kiro-Go` 的价值是更真实的 prompt cache tracker：按账号记录缓存指纹，首轮 creation、后续 read，TTL、阈值、规范化 fingerprint 都更接近真实上游行为，适合作为当前项目 P2 的主要学习对象。
5. 当前项目的 sticky session 方向是正确的，而且比简单轮询更符合高缓存场景：同一个下游会话应该尽可能固定在同一个 Kiro 账号上完成，只有硬失败或连续软失败才 fallback。

## 当前项目核心链路

当前项目的请求链路是：

1. `src/main.rs` 初始化配置、凭据、`MultiTokenManager`、`KiroProvider`、Anthropic 路由和 Admin 路由。
2. `src/anthropic/handlers.rs` 接收 `/v1/messages` 和 `/cc/v1/messages` 请求。
3. `src/anthropic/converter.rs` 将 Anthropic-compatible 请求转成 Kiro 请求，并把 Claude Code session 信息转为 `conversationState.conversationId`。
4. `src/kiro/provider.rs` 从 Kiro request body 中提取模型和 `conversationId`，通过 `MultiTokenManager` 选择账号并调用 Kiro 上游。
5. `src/kiro/parser/decoder.rs` 解 AWS eventstream。
6. `src/anthropic/stream.rs` 将 Kiro event 转成 Anthropic SSE。
7. `src/admin/*` 通过同一个 `MultiTokenManager` 做账号管理、余额查询和负载模式管理。

这个链路的核心设计要点是：调度发生在 Provider 层，SSE/非流式 usage 转换发生在 Anthropic adapter 层，Admin 只管理账号状态。后续高缓存记录需要跨这三层：请求进入时要知道原始 Anthropic prompt，Provider 选中账号后要知道 credential id，流结束后要知道最终 usage。

## 当前已有能力

### 账号与会话粘性

`src/kiro/token_manager.rs` 已有 `conversationId -> credential id` 的内存绑定：

1. `session_bindings` 存在于 `MultiTokenManager` 中，见 `src/kiro/token_manager.rs:533`。
2. 绑定 TTL 是 6 小时，上限是 10000 条，见 `src/kiro/token_manager.rs:541-546`。
3. `get_bound_credential` 会优先使用已有会话绑定，见 `src/kiro/token_manager.rs:784-807`。
4. `bind_session_to_credential` 会在成功获取上下文后建立或刷新绑定，见 `src/kiro/token_manager.rs:828-846`。
5. 禁用账号时会清理该账号关联的会话绑定，见 `src/kiro/token_manager.rs:1696-1699`。
6. 成功请求会清理 sticky 软失败计数，见 `src/kiro/token_manager.rs:1330-1335`。

`src/kiro/provider.rs` 已在调用 Kiro 前提取 conversation id：

1. 提取位置：`src/kiro/provider.rs:460-463`
2. 传入账号选择：`src/kiro/provider.rs:467-473`
3. 解析函数：`src/kiro/provider.rs:790-802`

这说明当前项目的核心调度已经符合“同一会话尽可能在同一账号完成”的方向。高缓存模拟不应该绕开这个机制，而应该复用 `conversationId + credentialId` 作为缓存记录和模拟的主键。

### sticky-aware retry

Provider 对错误做了区分：

1. 网络错误、408、429、5xx 被视作软失败，不直接禁用账号，见 `src/kiro/provider.rs:517-540` 和 `src/kiro/provider.rs:702-729`。
2. 连续软失败达到阈值后，本次请求可临时 fallback 到其他账号，见 `src/kiro/token_manager.rs:871-882`。
3. 402 monthly limit 会禁用/切换并清理会话绑定，见 `src/kiro/provider.rs:605-643`。
4. 401/403 会按凭据问题处理，见 `src/kiro/provider.rs:651-699`。
5. 流式成功不是收到响应头就算成功，而是在 EOF 时由 SSE 处理链路回报成功，见 `src/kiro/provider.rs:60-67` 和 `src/anthropic/handlers.rs:479-486`。
6. 流读取错误、idle timeout、drop 会按软失败处理，见 `src/anthropic/handlers.rs:467-477`、`src/anthropic/handlers.rs:495-507` 和 `src/kiro/provider.rs:106-112`。

这对高缓存很重要：如果瞬态错误就频繁切账号，下游服务连续会话就很难观察到稳定 cache read。

### Kiro metadata usage 解析

当前项目已经能解析 Kiro 新事件中的 token usage：

1. `MetadataTokenUsage` 包含 `uncached_input_tokens`、`output_tokens`、`total_tokens`、`cache_read_input_tokens`、`cache_write_input_tokens`，见 `src/kiro/model/events/additional.rs:33-47`。
2. `input_tokens()` 当前定义为 `uncached_input_tokens + cache_read_input_tokens`，见 `src/kiro/model/events/additional.rs:49-52`。
3. 已有高缓存解析测试，见 `src/kiro/model/events/additional.rs:137-156`。

这部分应该作为权威来源。任何本地模拟都不应该覆盖真实 Kiro metadata，除非明确进入开发/测试强制模拟模式。

### SSE 与非流式 usage

当前工作区已经有一组未提交/未最终验证的高缓存相关改动：

1. `src/anthropic/cache.rs` 是未跟踪文件，提供 `CacheUsage`、`estimate_cached_message_tokens` 和 `build_usage`。
2. `src/anthropic/mod.rs` 已把 `mod cache;` 接入。
3. `src/anthropic/handlers.rs` 在 messages 被 move 前计算 `cached_msg_tokens`，见 `src/anthropic/handlers.rs:289-298` 和 `src/anthropic/handlers.rs:931-940`。
4. 非流式响应通过 `super::cache::build_usage(...)` 输出 usage，见 `src/anthropic/handlers.rs:743-773`。
5. 普通流的 `message_delta.usage` 已改为完整 usage 对象，见 `src/anthropic/stream.rs:1326-1349`。
6. 缓冲流在完成后回填 `message_start.message.usage`，见 `src/anthropic/stream.rs:1463-1490`。
7. `SseStateManager` 已新增 `generate_final_events_with_usage`，见 `src/anthropic/stream.rs:470-500`。

这些改动方向正确，但目前还不能视为完整方案，因为缺少：

1. usage 记录存储。
2. Admin 查询页面/API。
3. 稳定的 per-account/per-session prompt cache tracker。
4. 运行时开关和测试模式边界。
5. 对当前 `cache.rs` 语义的进一步校正和测试。

### Admin 账号管理

当前 Admin 后端能力主要是账号和余额：

1. 获取账号列表：`src/admin/service.rs:70-110`
2. 禁用/启用账号：`src/admin/service.rs:112-127`
3. 设置优先级：`src/admin/service.rs:130-136`
4. 重置失败计数：`src/admin/service.rs:139-145`
5. 查询余额并做 5 分钟缓存：`src/admin/service.rs:148-178`
6. 添加账号时校验 endpoint 并主动查询订阅：`src/admin/service.rs:209-264`
7. 删除账号时清理余额缓存：`src/admin/service.rs:274-287`
8. 管理负载均衡模式：`src/admin/service.rs:290-314`
9. 强刷 token：`src/admin/service.rs:316-323`
10. Admin 路由只有 credentials/balance/load-balancing，见 `src/admin/router.rs:35-50`。

缺口是：没有请求 usage 记录、没有高缓存记录列表、没有按会话/账号/请求来源筛选的 Admin API，也没有管理页视图。

## 当前项目缺口

### 缺口 1：没有高缓存使用记录

现在服务可以把某次响应的 cache 字段返回给下游，但服务端自己没有记录：

1. 哪个下游请求出现了高 cache read。
2. 哪个 Kiro credential 产生了 cache read。
3. 哪个 `conversationId`/session 维持了高缓存。
4. 是真实 Kiro metadata，还是本地模拟。
5. 流式请求是否完整 EOF，还是因为中断/idle timeout 失败。
6. 某个账号、某个会话、某段时间的 cache read 总量。

这会导致“作为其他服务上游”的验证不闭环：下游看到字段不等于本服务可运维、可审计、可复盘。

### 缺口 2：没有可控高缓存模拟

真实 Kiro cache hit 由上游决定，本地不能强制真实上游一定返回高 `cacheReadInputTokens`。如果要让其他服务把当前服务当上游做集成测试，就需要本地可控模拟。

当前工作区的 `src/anthropic/cache.rs` 是启发式模拟：发现 `cache_control` 后按估算切分 usage。它能让响应字段“长得像高缓存”，但它不是状态化缓存：

1. 不知道这是同一账号的第几次请求。
2. 不区分首次 cache creation 和后续 cache read。
3. 不存 fingerprint，不支持 TTL。
4. 不考虑 Claude Code billing header 这种每次变化但不应破坏缓存的内容。
5. 不支持 Admin 查看“模拟缓存命中记录”。

所以它适合做临时 fallback，不适合作为最终高缓存模拟模型。

### 缺口 3：cache usage 语义需要统一

当前 `src/kiro/model/events/additional.rs:49-52` 把 metadata 的 `input_tokens` 定义成 `uncached + cache_read`。这符合之前项目测试里“上下文总输入”的理解。

但 Anthropic API 的 usage 通常把：

1. `input_tokens` 理解为未缓存/计费输入。
2. `cache_creation_input_tokens` 表示写入缓存 token。
3. `cache_read_input_tokens` 表示从缓存读取 token。

`Kiro-Go` 明确使用 `billedClaudeInputTokens(inputTokens, usage)`，即 `input_tokens = total - cache_creation - cache_read`，见 `Kiro-Go/proxy/cache_tracker.go:509-516`。

当前项目如果继续对真实 metadata 返回 `input_tokens = uncached + cache_read`，优点是保留上下文总输入视角；缺点是和 Anthropic 兼容 usage 语义不完全一致。这个点不能随意改，因为会影响现有下游。建议先在使用记录里同时记录：

1. `total_input_tokens`
2. `billable_input_tokens`
3. `cache_creation_input_tokens`
4. `cache_read_input_tokens`
5. `compat_input_tokens`

响应字段是否调整要作为兼容性变更单独决策。

### 缺口 4：Provider 没有把选中账号信息暴露给 usage 记录层

流式路径中 `KiroStreamCompletion` 内部有 `credential_id` 和 `session_id`，见 `src/kiro/provider.rs:62-67`，但当前 Anthropic handler 只能用它 report success/failure，无法拿到 credential id 写 usage record。

非流式路径 `call_api` 成功后会立即 report success，调用者也拿不到 `credential_id`。

如果要做高缓存记录，至少需要一种方式把本次请求选中的 `credential_id/session_id` 暴露给 handler 或 recorder：

1. 给 `KiroStreamCompletion` 增加只读 accessor。
2. 非流式返回一个带 `response + credential_id + session_id` 的结构，或在 Provider 内部记录基础调用信息。
3. 更推荐新增“请求上下文/usage recorder”贯穿 handler 和 provider，避免 handler 反向解析 Provider 内部状态。

## `../kiro.rs-new.rs` 可借鉴点

实际路径是 `/Users/yuanfeijie/Desktop/procode/kiro.rs-new.rs`。

### 轻量 cache_control 检测

`kiro.rs-new.rs/src/anthropic/cache.rs` 提供：

1. `estimate_cached_message_tokens`：从 messages 反向找最后一条带非 null `cache_control` 的 message content，并估算 token，见 `kiro.rs-new.rs/src/anthropic/cache.rs:10-28`。
2. `split_cache_tokens`：按 `total = input + cache_creation + cache_read` 切分，见 `kiro.rs-new.rs/src/anthropic/cache.rs:30-73`。

它的优点：

1. 简单。
2. 不需要状态存储。
3. 对下游最小侵入。
4. 可以快速让响应出现 `cache_creation_input_tokens` 和 `cache_read_input_tokens`。

它的局限：

1. 不是状态化缓存，首次请求也可能直接出现大量 read。
2. 只看最后一条带 `cache_control` 的 message，不处理 system/tools。
3. 不知道账号、会话、TTL。
4. 不适合用于 Admin 高缓存记录和可重复验证。

### 接入点

`kiro.rs-new.rs` 的接入方式：

1. 在 messages move 前计算缓存 token，见 `kiro.rs-new.rs/src/anthropic/handlers.rs:286-295`。
2. 普通流最终 `message_delta.usage` 加 cache 字段，见 `kiro.rs-new.rs/src/anthropic/stream.rs:456-498`。
3. 缓冲流完成后回填 `message_start.message.usage`，见 `kiro.rs-new.rs/src/anthropic/stream.rs:1220-1237`。

当前工作区的 `src/anthropic/cache.rs` 基本已经吸收这部分思路，并额外加了 metadata 优先逻辑。后续建议保留它作为 `heuristic_cache_control` 模式，而不是把它当最终真实模拟。

## `Kiro-Go` 可借鉴点

`Kiro-Go` 里最值得学习的是 `proxy/cache_tracker.go` 的 prompt cache tracker。

### 状态化 prompt cache tracker

核心结构：

1. `promptCacheTracker` 按 account ID 保存 fingerprint entry，见 `Kiro-Go/proxy/cache_tracker.go:55-69`。
2. `BuildClaudeProfile` 将 Claude request 展开成 cacheable blocks，并生成累计 token 的 breakpoints，见 `Kiro-Go/proxy/cache_tracker.go:71-126`。
3. `Compute` 在请求前计算本次 usage：首次 creation，后续 read，见 `Kiro-Go/proxy/cache_tracker.go:128-194`。
4. `Update` 在上游请求成功后写入缓存 entry，见 `Kiro-Go/proxy/cache_tracker.go:196-223`。
5. 过期 entry 会被清理，见 `Kiro-Go/proxy/cache_tracker.go:225-235`。

这比 `kiro.rs-new.rs` 更符合“当前服务作为上游给其他服务压测/集成”的需求，因为它能产生稳定的第一轮写缓存、第二轮读缓存行为。

### 更真实的缓存 profile

`Kiro-Go` 不只是找最后一条 message：

1. 会把 tools、system、messages 都展平，见 `Kiro-Go/proxy/cache_tracker.go:245-271`。
2. tools 会纳入 fingerprint，见 `Kiro-Go/proxy/cache_tracker.go:249-263`。
3. system 支持 string、数组、字符串数组，见 `Kiro-Go/proxy/cache_tracker.go:286-317`。
4. messages 支持 string、数组和其他内容，见 `Kiro-Go/proxy/cache_tracker.go:319-355`。
5. 一旦出现显式 `cache_control`，后续 message-end 也可以成为隐式 breakpoint，见 `Kiro-Go/proxy/cache_tracker.go:87-98`。

这对 Claude Code 多轮会话很重要：第一轮 system/prefix 被缓存后，后续用户消息即使没有重复显式 `cache_control`，也能命中之前 prefix。

### fingerprint 稳定性

`Kiro-Go` 做了几个关键处理：

1. 忽略 Claude Code `x-anthropic-billing-header` 这类每次变化但不影响语义的文本块，见 `Kiro-Go/proxy/cache_tracker.go:357-366` 和 `Kiro-Go/proxy/cache_tracker.go:389-407`。
2. canonical JSON 会剔除 `cache_control` 字段，见 `Kiro-Go/proxy/cache_tracker.go:530-585`。
3. 忽略外层位置 key，见 `Kiro-Go/proxy/cache_tracker.go:587-593`。
4. 通过长度分隔写入 hash chunk，避免拼接歧义，见 `Kiro-Go/proxy/cache_tracker.go:596-602`。

这些处理适合移植到当前项目，因为当前服务的典型下游很可能是 Claude Code 类客户端，而这类客户端会带动态 billing/header 内容。如果不规范化 fingerprint，高缓存模拟会频繁 miss。

### TTL、阈值、现实约束

`Kiro-Go` 的现实约束：

1. 默认 cache TTL 是 5 分钟，见 `Kiro-Go/proxy/cache_tracker.go:14`。
2. 默认最小 cacheable token 是 1024，Opus 是 4096，见 `Kiro-Go/proxy/cache_tracker.go:16-21` 和 `Kiro-Go/proxy/cache_tracker.go:42-48`。
3. TTL 归一化到 5 分钟或 1 小时，见 `Kiro-Go/proxy/cache_tracker.go:409-483`。
4. cacheable tokens 最多按 total input 的 85% 计算，避免 100% cache hit 不真实，见 `Kiro-Go/proxy/cache_tracker.go:158-164`。
5. usage map 支持 nested cache creation breakdown，见 `Kiro-Go/proxy/cache_tracker.go:513-528`。

这些规则可以作为当前项目本地模拟模式的默认值。

### 请求链路集成

`Kiro-Go` 的集成方式：

1. 账号选中后计算 `estimatedInputTokens`、`cacheProfile`、`cacheUsage`，见 `Kiro-Go/proxy/handler.go:805-808`。
2. 流式 `message_start` 带 usage，见 `Kiro-Go/proxy/handler.go:1096-1108`。
3. 上游成功后 `promptCache.Update(account.ID, cacheProfile)`，见 `Kiro-Go/proxy/handler.go:1213-1217`。
4. 流式最终 `message_delta.usage` 使用同一份 usage，见 `Kiro-Go/proxy/handler.go:1224-1230`。
5. 非流式响应 patch usage 和 nested cache creation，见 `Kiro-Go/proxy/handler.go:1394-1403`。

当前 Rust 项目需要注意差异：当前账号选择在 Provider 内，handler 在 Provider 调用前不知道 credential id。因此 Rust 不能照搬 Go handler 写法，需要调整 Provider 返回上下文或引入 recorder。

### 测试值得学习

`Kiro-Go/proxy/cache_tracker_test.go` 覆盖了高缓存模拟最关键的行为：

1. 首次请求 creation、第二次 read，见 `Kiro-Go/proxy/cache_tracker_test.go:9-47`。
2. usage map 字段，见 `Kiro-Go/proxy/cache_tracker_test.go:49-75`。
3. billing header 变化不破坏 cache hit，见 `Kiro-Go/proxy/cache_tracker_test.go:77-167`。
4. 位置 key 规范化，见 `Kiro-Go/proxy/cache_tracker_test.go:169-211`。
5. 多轮会话的 implicit message-end breakpoint，见 `Kiro-Go/proxy/cache_tracker_test.go:213-264`。

当前项目后续移植时应建立同等测试集。

### 账号池与 Admin 管理可借鉴但不应照搬

`Kiro-Go/pool/account.go` 有：

1. 加权轮询，见 `Kiro-Go/pool/account.go:45-66`。
2. 按模型过滤账号，见 `Kiro-Go/pool/account.go:133-172`。
3. 冷却、token 过期跳过、额度跳过，见 `Kiro-Go/pool/account.go:181-209`。
4. 错误冷却策略，见 `Kiro-Go/pool/account.go:247-269`。
5. 请求数、token、credits、lastUsed 统计落盘，见 `Kiro-Go/pool/account.go:321-354`。
6. `config.Account` 里包含更完整的账号信息和运行统计，见 `Kiro-Go/config/config.go:34-96`。

这些值得参考，但不建议现在直接把当前项目的 priority/balanced/sticky 调度替换成 Go 的 weighted round-robin。当前项目的核心目标是“粘性会话保证高缓存连续性”，直接引入加权轮询可能反而破坏同会话固定账号的确定性。

可先学习的部分：

1. 按模型能力/订阅能力刷新账号模型列表。
2. Admin 展示更多账号运行统计。
3. 对 quota/overage 账号做更细粒度状态。
4. usage 统计持久化。

不建议当前阶段引入：

1. 全局加权轮询替代 sticky。
2. 无会话维度的纯 account-level prompt cache 命中模型。

## 当前项目推荐设计

推荐拆成两层：先记录，再模拟。

### 第一层：UsageRecordStore

新增一个轻量记录组件，例如：

1. `src/anthropic/usage_record.rs`
2. 或 `src/kiro/usage_recorder.rs`

职责：

1. 记录每次请求的最终 usage。
2. 支持内存 ring buffer，避免无限增长。
3. 可选 JSONL 落盘，路径可放在 `token_manager.cache_dir()/kiro_usage_records.jsonl`。
4. Admin 查询时从内存读取，服务重启后可加载最近 N 条 JSONL。

建议字段：

1. `id`：本地 request id。
2. `created_at`：UTC 时间。
3. `endpoint`：`/v1/messages`、`/cc/v1/messages`、`/v1/messages/count_tokens` 等。
4. `stream`：是否流式。
5. `model`。
6. `conversation_id`：从 Kiro request 或 Anthropic metadata 中解析到的会话 id。
7. `credential_id`。
8. `credential_email` 或脱敏账号展示字段。
9. `api_key_hash` 或调用方标识，若当前服务启用了 API key。
10. `status`：`success`、`error`、`stream_error`、`client_drop`、`upstream_timeout`。
11. `usage_source`：
    - `upstream_metadata`
    - `context_estimate`
    - `local_prompt_cache`
    - `heuristic_cache_control`
    - `none`
12. `total_input_tokens`。
13. `billable_input_tokens`。
14. `compat_input_tokens`：实际返回给下游的 `usage.input_tokens`。
15. `output_tokens`。
16. `cache_read_input_tokens`。
17. `cache_creation_input_tokens`。
18. `cache_creation_5m_input_tokens`。
19. `cache_creation_1h_input_tokens`。
20. `duration_ms`。
21. `error_type`。
22. `error_message`。
23. `sticky_bound`：是否命中已有会话绑定。
24. `fallback_from_sticky`：是否因为连续软失败临时 fallback。
25. `simulated`：是否本地模拟。

这个记录结构能回答用户真正关心的问题：哪个下游会话、哪个账号、哪次请求出现了高缓存。

### 第二层：Admin Usage API

建议新增 Admin API：

1. `GET /api/admin/usage-records`
2. `GET /api/admin/usage-summary`
3. `POST /api/admin/usage-records/clear`

`GET /api/admin/usage-records` 查询参数建议：

1. `limit`
2. `offset` 或 `cursor`
3. `conversationId`
4. `credentialId`
5. `model`
6. `status`
7. `source`
8. `stream`
9. `minCacheRead`
10. `since`
11. `until`

`GET /api/admin/usage-summary` 返回：

1. 最近 N 分钟请求数。
2. 成功/失败/流中断数量。
3. `cache_read_input_tokens` 总量。
4. `cache_creation_input_tokens` 总量。
5. 高缓存请求数量，例如 `cache_read_input_tokens >= 10000`。
6. Top credential by cache read。
7. Top conversation by cache read。
8. simulated vs upstream metadata 比例。

### 第三层：Admin UI

当前 Admin UI 是账号管理面板，后续建议增加一个“缓存记录”视图或 Tab：

列表字段：

1. 时间。
2. 模型。
3. endpoint。
4. stream。
5. 账号。
6. conversationId。
7. total input。
8. billable input。
9. output。
10. cache read。
11. cache creation。
12. source。
13. status。
14. duration。

筛选：

1. 最小 cache read。
2. source。
3. status。
4. credential。
5. conversationId。
6. 最近 5/15/60 分钟。

摘要卡：

1. 请求总数。
2. 高缓存请求数。
3. cache read 总量。
4. cache creation 总量。
5. 最高缓存会话。
6. 最高缓存账号。

Admin 页面不应该只显示余额，因为余额是账号级，而高缓存记录是请求级/会话级。

## 高缓存模拟模式

建议明确配置开关，默认关闭本地模拟，真实 metadata 优先。

### 模式 0：disabled

默认模式。

行为：

1. 只使用 Kiro `metadataEvent.tokenUsage`。
2. 没有 metadata 时只返回估算 input/output，不制造 cache read。
3. 仍然记录 usage record，source 是 `upstream_metadata` 或 `context_estimate`。

适用：

1. 生产默认。
2. 不希望本地制造 usage 字段的环境。

### 模式 1：heuristic_cache_control

基于当前 `src/anthropic/cache.rs` 和 `kiro.rs-new.rs` 的轻量模拟。

行为：

1. Kiro metadata 存在时，metadata 优先。
2. metadata 缺失且请求里存在非 null `cache_control` 时，根据 token 估算切分 cache usage。
3. source 记为 `heuristic_cache_control`。

优点：

1. 实现成本低。
2. 可快速让下游看到高缓存字段。
3. 与当前工作区改动吻合。

缺点：

1. 不是真正的 first creation/second read。
2. 不按账号/会话维护缓存。
3. 不适合验证长期粘性会话效果。

建议定位：

1. P1 fallback。
2. 兼容 `../kiro.rs-new.rs` 的行为。

### 模式 2：local_prompt_cache

移植 `Kiro-Go` 的 promptCacheTracker 思路。

行为：

1. 在原始 Anthropic payload 上构建 cache profile。
2. 使用 `credential_id + conversation_id + model` 作为缓存域。
3. 首次请求：命中 cache_control breakpoint 后返回 cache creation。
4. 上游成功后：写入本地 prompt cache tracker。
5. 后续同账号、同会话、同模型、同稳定 prefix：返回 cache read。
6. source 记为 `local_prompt_cache`。
7. 真实 Kiro metadata 存在时仍以 metadata 为准，除非显式开启 override。

为什么建议比 `Kiro-Go` 多加 `conversation_id`：

1. `Kiro-Go` 只按 account ID 分区，这适合它自己的代理模型。
2. 当前项目已经有 sticky session，用户明确要求一个会话尽量在同账号完成。
3. 对“作为其他服务上游的高缓存模拟”来说，下游通常希望同一 session 重复请求才命中，而不是不同下游会话共享同账号缓存。
4. 因此推荐 key 为 `credential_id + conversation_id + model`，必要时配置允许降级为 account-level。

### 模式 3：force_high_cache

仅限开发/集成测试。

行为：

1. 不依赖 prompt fingerprint。
2. 按配置固定制造高 cache read，比如 `cache_read_input_tokens = min(total_input_tokens * 0.8, 180000)`。
3. source 记为 `forced_high_cache`。
4. 必须通过 env/config/admin 明确开启，生产默认永远关闭。

用途：

1. 下游服务联调：验证它能解析和展示高缓存字段。
2. Admin UI 压测：快速生成大量高缓存记录。

风险：

1. 最不真实。
2. 容易误导成本/usage 判断。
3. 必须在 UI 和 usage record 中明确标记 `simulated=true`。

## 高缓存模拟请求方法

当前服务作为上游时，下游服务可以这样模拟：

1. 启用 `local_prompt_cache` 或 `heuristic_cache_control`。
2. 使用同一个下游 session，让请求最终转成稳定的 `conversationState.conversationId`。
3. 使用同一个模型。
4. 让 sticky session 绑定到同一个 Kiro credential。
5. 在 system 或早期 message block 上放一个足够长的稳定 prefix，并加：

```json
{
  "type": "ephemeral"
}
```

示例 Anthropic request 片段：

```json
{
  "model": "claude-sonnet-4-5-20250929",
  "stream": true,
  "metadata": {
    "user_id": "downstream-service/session-high-cache-001"
  },
  "system": [
    {
      "type": "text",
      "text": "这里放很长且稳定的系统提示词，建议超过 1024 tokens，用于触发本地 prompt cache profile。",
      "cache_control": {
        "type": "ephemeral"
      }
    }
  ],
  "messages": [
    {
      "role": "user",
      "content": "第一轮问题"
    }
  ],
  "max_tokens": 1024
}
```

期望行为：

1. 第一轮：`cache_creation_input_tokens > 0`，`cache_read_input_tokens = 0`。
2. 第二轮：同 session、同账号、同模型、同 stable prefix，`cache_read_input_tokens` 明显上升。
3. Admin usage records 可以按 `conversationId=session-high-cache-001` 或 `minCacheRead=10000` 查到记录。
4. 如果发生连续软失败 fallback，usage record 应标记 `fallback_from_sticky=true`，这样能解释 cache read 降低。

## 与 sticky session 的关系

用户希望“尽可能保证一个会话在同一个账号上完成”。这个优化方向不仅符合高缓存，而且是高缓存模拟的前提。

推荐规则：

1. 新会话按当前 `priority` 或 `balanced` 选择账号。
2. 一旦选中账号，绑定 `conversationId -> credentialId`。
3. 同一会话后续请求优先使用绑定账号。
4. 软失败只累计，不立即解绑。
5. 连续软失败达到阈值后，本次请求临时 fallback，但记录 `fallback_from_sticky=true`。
6. 硬失败、quota、手动禁用、删除账号时清理绑定。
7. prompt cache tracker 的缓存域默认绑定 `credentialId + conversationId + model`。

这样能最大程度保证：

1. 真实 Kiro cache 有机会连续命中。
2. 本地模拟 cache 行为可重复。
3. Admin 上可以解释为什么某次 cache read 变低。

## 具体落地优先级

### P0：把 usage 记录能力补上

目标：

1. 先让服务端知道每次请求发生了什么。
2. 不先追求复杂模拟。

任务：

1. 新增 usage record 类型和 ring buffer。
2. 非流式成功/失败后记录。
3. 普通流在 EOF/error/idle timeout/drop 时记录。
4. 缓冲流完成后记录。
5. 记录真实 metadata usage 和 context estimate source。
6. Admin 增加 `GET /usage-records` 和 `GET /usage-summary`。

验收：

1. 真实 Kiro 返回 metadata 时，record 中能看到 cache read/write。
2. 流式中断时，record status 不是 success。
3. 可以按 credential/conversation/minCacheRead 查询。

### P1：稳定当前轻量 cache_control 模拟

目标：

1. 把当前工作区 `src/anthropic/cache.rs` 改动稳定成明确模式。
2. 不让生产默认悄悄制造 cache usage。

任务：

1. 增加配置项，例如 `prompt_cache_simulation_mode`。
2. 默认 `disabled`。
3. 开启 `heuristic_cache_control` 时才用当前 `estimate_cached_message_tokens`。
4. usage record 标记 source。
5. 普通流、缓冲流、非流式都保持字段一致。

验收：

1. 默认不模拟。
2. 开启后请求带 `cache_control` 会出现 cache fields。
3. metadata 存在时 metadata 永远优先。
4. 测试覆盖普通流 `message_delta`、缓冲流 `message_start`、非流式 JSON。

### P2：移植 Kiro-Go 风格 local_prompt_cache

目标：

1. 支持可重复的 first creation / second read。
2. 支持高缓存记录查看。
3. 更接近真实 Anthropic/Kiro prompt cache 行为。

任务：

1. 新增 Rust `PromptCacheTracker`。
2. 从原始 Anthropic payload 构建 cache profile。
3. 支持 tools/system/messages 展平。
4. canonical JSON 剔除 `cache_control`。
5. 忽略 Claude Code billing header。
6. 支持 TTL 5m/1h。
7. 支持最小 cacheable tokens：Sonnet/Haiku 1024，Opus 4096。
8. cacheable cap 默认 85%。
9. cache key 使用 `credentialId + conversationId + model`。
10. 上游成功后 update，失败不 update。
11. usage record 标记 source `local_prompt_cache`。

验收：

1. 第一轮同 session 请求 creation > 0, read = 0。
2. 第二轮同 session 请求 read > 0。
3. billing header 改变不破坏 hit。
4. 无 `cache_control` 且没有历史 breakpoint 时不模拟。
5. Admin 可看到高缓存记录。

### P3：Admin UI 高缓存视图

目标：

1. 让高缓存使用记录可视化。

任务：

1. 增加 usage records API client。
2. 增加缓存记录 Tab。
3. 增加 summary cards。
4. 增加筛选和清空。
5. 高亮 `simulated=true` 和 `source`。

验收：

1. 管理页能看到最近请求。
2. 能筛选高缓存请求。
3. 能看到每个账号/会话的 cache read 总量。
4. 能区分真实 metadata 和本地模拟。

### P4：账号管理增强

目标：

1. 学习 `Kiro-Go` 的账号统计，但不破坏当前 sticky 调度。

任务：

1. 展示 per-account request count、tokens、credits、last used。
2. 增加模型列表刷新/展示。
3. 增加 quota/overage 状态。
4. 增加账号级 proxy 健康检查。

不建议当前阶段：

1. 用 weighted round-robin 替代当前 sticky-aware priority/balanced。
2. 在没有明确需求前引入复杂 overage 调度。

## 实现注意点

### 不要让模拟覆盖真实 metadata

优先级必须是：

1. Kiro metadata usage。
2. local_prompt_cache。
3. heuristic_cache_control。
4. context estimate。
5. request estimate。

只有 `force_high_cache` 可以覆盖，但必须显式配置并标记。

### 不要在失败请求后更新 prompt cache

local prompt cache 应该只在上游成功后 update：

1. 非流式：完整 decode 成功并生成响应后 update。
2. 普通流：EOF 且无 stream_error 后 update。
3. 缓冲流：完整 finish 后 update。
4. read error、idle timeout、client drop 不 update。

否则会让失败请求也“创建缓存”，导致后续 read 不真实。

### 需要让 handler 知道 credential_id

当前 Provider 内部知道 credential id，但 handler 不知道。建议：

1. `KiroStreamCompletion` 增加 `credential_id()`、`session_id()` accessor。
2. 非流式新增 `call_api_with_context` 或返回 `KiroApiResponse { response, credential_id, session_id }`。
3. 保持旧 `call_api` 作为兼容 wrapper。

这样 usage recorder 才能写出“哪个账号产生了高缓存”。

### 原始 Anthropic payload 要在 move 前提取

`token::count_all_tokens` 会 move `payload.system/messages/tools`。构建 prompt cache profile 必须在 move 前完成，类似当前 `src/anthropic/handlers.rs:289-298` 的顺序。

### usage 语义要避免破坏下游

当前项目已有下游可能依赖 `input_tokens = uncached + cache_read`。如果要改成 Anthropic 更标准的 billable input，需要：

1. 先在 usage record 中记录两套值。
2. 加配置开关。
3. 保持默认兼容当前行为。
4. 文档中明确字段语义。

## 验收标准

最终优化完成后，应该满足：

1. 同一个下游 session 在无硬失败时稳定使用同一个 Kiro credential。
2. 真实 Kiro `metadataEvent.tokenUsage` 中的高 cache read/write 不丢失。
3. 普通流、缓冲流、非流式三条路径 usage 字段一致。
4. Admin 可以查看每次请求的 usage record。
5. Admin 可以筛选 `cache_read_input_tokens >= N` 的高缓存记录。
6. 本地模拟模式可以让下游服务稳定复现高 cache read。
7. 第一轮请求和第二轮请求的模拟结果符合 creation/read 语义。
8. 流式失败、idle timeout、client drop 不会被误记为成功高缓存。
9. 模拟记录和真实 metadata 记录可以区分。
10. 所有本地模拟默认关闭，生产不会无意制造 cache usage。

## 推荐下一步

按当前项目实际情况，下一步不应该先继续堆启发式 cache 字段，而应该：

1. 先实现 `UsageRecordStore` 和 Admin 查询 API。
2. 再把当前 `src/anthropic/cache.rs` 纳入配置化 `heuristic_cache_control` 模式。
3. 然后移植 `Kiro-Go` 的 `PromptCacheTracker`，形成真正适合上游集成测试的 `local_prompt_cache`。
4. 最后补 Admin UI 的高缓存记录页面。

这样顺序最贴合当前服务核心逻辑：先可观测，再可控模拟，再可视化管理。
