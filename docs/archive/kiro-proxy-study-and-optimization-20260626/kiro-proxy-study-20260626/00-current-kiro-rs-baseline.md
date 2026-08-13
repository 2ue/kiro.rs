# 当前项目 `kiro.rs` 基线

本篇先定义当前项目的现状。后续所有外部项目分析都以这个基线为参照：哪些能力当前已经更强，哪些外部实现值得学习，哪些不能为了“借鉴”而退化。

## 关键代码位置

| 能力 | 当前项目文件 |
| --- | --- |
| 本地账号调度、RPM、并发、冷却、粘性 | `src/kiro/token_manager.rs` |
| Kiro 上游调用、多账号重试、完成 guard | `src/kiro/provider.rs` |
| Kiro IDE endpoint、headers、profileArn 注入 | `src/kiro/endpoint/ide.rs` |
| profileArn、Builder ID、Social、External IdP 规则 | `src/kiro/protocol.rs` |
| Anthropic 请求转换、tool pair 校验 | `src/anthropic/converter.rs` |
| payload guard、tool-use 修复 | `src/anthropic/payload_guard.rs` |
| SSE/streaming、thinking、tool event 转换 | `src/anthropic/stream.rs` |
| prompt cache / high-cache 模拟 | `src/anthropic/prompt_cache.rs`、`src/anthropic/cache.rs` |
| usage 记录、dashboard、latency trace | `src/anthropic/usage.rs` |
| 对外错误 envelope、request-id | `src/anthropic/envelope.rs` |
| 外部账号池调度、容量、错误归一化、usage projection | `src/external_pool.rs` |
| Kiro 尝试链路记录 | `src/kiro/call_trace.rs` |

## 当前项目强项

### 1. 运行态比大部分样本完整

当前项目不是单进程内存轮询，已经有 PgSQL + Redis：

- PgSQL 存配置、账号、usage、pricing、审计。
- Redis 存调度运行态、session binding、并发 lease、refresh lock、summary cache。
- usage 写入走队列，队列满时保护请求热路径。
- 本地账号和外部账号池都能记录 route subtype、attempts、first token、latency trace、error id。

这部分明显强于 `Kiro-Go`、`Kiro-account-manager`、`kiroxy`、`dntproxy` 这类更偏单机或轻量的项目。后续学习外部项目时不能把状态模型退回内存或 JSON 文件。

### 2. 调度能力已经比较生产化

`src/kiro/token_manager.rs` 里已经包含：

- 账号级 `rpm`，通过 `rate_limit_available_at` 控制最小请求间隔。
- 账号级 `max_concurrent_requests` override。
- 全局 `dispatch_global_max_concurrent_requests`。
- `dispatch_max_queued_requests` 队列控制。
- in-flight lease 自动释放和过期清理。
- Redis 热路径超时降级，避免 Redis 卡住直接拖死接口。
- session sticky，且支持软失败后 fallback。
- 代理资源绑定校验，防止账号绑定代理不可用时偷跑直连。
- `priority`、`balanced`、`health_balanced` 三种调度模式。

当前调度的实质强项是：它不只是在请求结束后记账，而是在请求进入上游前就做容量判断和 lease 占用。

### 3. 高并发保护比轻量项目强

当前项目有几层保护：

- local + Redis 双层并发 lease。
- `InFlightLeaseGuard` drop 自动释放。
- `QueueGuard` drop 自动退出队列。
- 非流式 `KiroApiCompletion` 用 Drop 兜底，避免 body 读取中途失败导致账号长期占槽。
- `cleanup_expired_in_flight_leases_local_first` 避免异常请求永久占槽。
- 外部账号池也有 lease touch、idle deadline、capacity snapshot、cooldown。

这个方向必须保留。外部项目里很多“简单轮询”的实现不能直接照搬。

### 4. 对外错误归一化已经有基础

`src/anthropic/envelope.rs` 提供：

- Anthropic 兼容 request id。
- `request-id` 和 `anthropic-request-id` header。
- 对外统一文案。
- 错误 ID 能给下游定位，同时内部保留原始错误。

`src/external_pool.rs` 也已经避免把外部账号池原始错误直接透给下游，并把 status、headers、response body、retryable、cooldown、auto-disable reason 等诊断存到 usage/error diagnostics。

### 5. 协议兼容已经覆盖不少复杂场景

当前项目已有：

- thinking 输出，包括 XML thinking tag 和 reasoningContentEvent。
- tool-use / tool-result 配对校验。
- payload guard 修复重复 tool_use_id、孤立 tool_result、空 tool use。
- 高缓存路由 `/cc`、`/ha`、`/na` 和安全自定义 `/dfcache/*`。
- 外部池 thinking/model mapping normalize。

## 当前项目短板

### 1. `token_manager.rs` 过大，边界不清

调度、粘性、容量、Redis、冷却、refresh、proxy resource、审计、配置更新都在一个文件里。当前文件里的逻辑能力强，但维护成本高。

典型问题：

- selector 策略不是独立接口。
- capacity lease 和选择逻辑交织。
- session sticky 和 fallback 判断嵌在 manager 内部。
- route preflight 和真正 acquire 的失败原因模型不完全统一。
- 管理端想解释“为什么不可用”需要从多个状态推导。

对比 `kirocc-prox`，它的 `Selector` / `Scheduler` / `Conductor` / `RuntimeStateStore` 分层更清晰，当前项目应学习这种拆分，但保留自身更强的 Redis/PgSQL 和健康评分能力。

### 2. 调度失败原因需要结构化

当前 `compute_local_pool_route_state` 能统计：

- no credentials
- all disabled
- no model compatible
- proxy blocked
- all cooling down
- capacity full

但这更多是 route-state 预检聚合。真实 acquire 过程里的失败、fallback、排除账号、Redis 降级、RPM、proxy resource、model cooldown 没有统一结构化原因对象。

可以学习 `dntproxy` 的 `AccountSelectionErrorKind`，但不能把内部字段直接暴露给下游。建议内部形成：

- `SelectionFailureReason::NoAccount`
- `SelectionFailureReason::Disabled`
- `SelectionFailureReason::ModelUnsupported`
- `SelectionFailureReason::ProxyUnavailable`
- `SelectionFailureReason::RpmLimited`
- `SelectionFailureReason::ConcurrencyFull`
- `SelectionFailureReason::GlobalConcurrencyFull`
- `SelectionFailureReason::Cooldown`
- `SelectionFailureReason::RedisDegraded`
- `SelectionFailureReason::StickyUnavailable`

对外仍然保持统一 “No account is currently available...” 这类文案；对内日志、usage、管理端使用结构化详情。

### 3. 策略语义需要更可解释

当前 `priority_selection_key` 和 `scheduler_score_with_config` 都是“数值越小越优先”。这和 `kirocc-prox` 里的 Priority descending 不同，后续文档和 UI 必须明确：当前项目里优先级数字更小表示更高优先级。

`health_balanced` 的 score 包含：

- priority
- load
- recent error rate
- latency ewma
- probation
- selection pressure
- total selection count

能力强，但用户很难理解每个配置项会怎样影响调度。建议后续把 score 拆成可展示的 breakdown，用于管理端解释“为什么选了这个账号”。

### 4. 高缓存仍偏 usage 模拟

当前 `PromptCacheTracker` 通过本地 fingerprint 模拟 cache creation/read usage。这对计费和 dashboard 有用，但不等于真实向 Kiro upstream 发送 `cachePoint`。

外部 `kiro2api` 和 `kiroxy` 的 `ApplyToolCachePoints` 提供了真实 Kiro request shape 方向。当前项目可以在 feature flag 下尝试，但必须经过长会话、tool-use、Claude Code CLI、Kiro、OpenCode 的真实验证。

### 5. 压测/回归工具还不够体系化

当前单测很多，但缺一个面向当前项目的真实压测脚本，能同时观察：

- TTFB / first upstream chunk / first output delta。
- 大并发下 `rpm` 和 `max_concurrent_requests` 是否生效。
- 账号失败、恢复、冷却、外部池 fallback 是否符合预期。
- Redis 降级是否不拖慢接口。
- SSE idle、client drop、upstream 200 JSON exception 是否不会卡死。
- 内存是否稳定。

`kiroxy/scripts/loadtest` 和 `cp-coder9/kiro-gateway/tests` 的测试组织值得学习。

## 必须保留的当前优势

- PgSQL/Redis 的生产状态模型。
- 账号级 RPM 和并发 lease。
- dispatch wait / queue。
- session sticky + soft failure fallback。
- 外部账号池和本地账号池统一 route 口径。
- usage 异步写入、dashboard summary、latency trace。
- 对外错误归一化和 request id。
- payload guard、tool-use 修复、thinking 兼容。
- `/dfcache/*` 的安全边界。

## 对后续学习的判断

当前项目不缺“功能”，主要缺三件事：

1. 模块边界更清晰。
2. 调度与错误更可解释。
3. 真实压测和协议回归更体系化。

外部项目的价值应该围绕这三点吸收，而不是把当前项目改成某个外部项目的简单架构。

