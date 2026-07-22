# 生产调度跟进问题与方案草案（2026-07-15）

状态：已完成 2026-07-15 17:15 CST 只读取证；尚未实现修复。

本文记录 2026-07-15 现网排查后确认或高度怀疑的调度/清理问题，并给出修复方向。它不替代已实现的调度设计文档，而是作为生产反馈后的待办入口。

相关现网证据包：

- 历史证据：`tmp/prod-evidence/20260715-125054-152.53.243.159/20260715-125054-152.53.243.159-redacted.tar.gz`。
- 本轮证据：`tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/`。

## P1：本地容量存在时仍可能外部池

结论：确认存在。

现象：本地账号池启用 60 个账号、总并发/RPM 仍有余量时，请求仍可能进入外部池。

已确认路径不是旧的 Redis degraded preflight，而是：

1. 本地请求只尝试到 `credentialRetryMaxAttempts` 上限；
2. 这些尝试都是上游瞬态错误，例如 `500 transient_error`；
3. `fallbackOnLocalTransientExhausted=true`；
4. `classify_local_error_for_external_fallback` 把错误归类为 `local_transient_exhausted`；
5. `fallback_after_local_error_outcome_with_diagnostics` 只检查外部池是否可用，没有重新证明本地池已经不可调度；
6. 请求进入 `external_fallback_after_local_attempts`。

代码证据：

- `src/kiro/provider.rs`：`max_retry_attempts` 中 `credentialRetryMaxAttempts > 0` 时直接使用显式上限。
- `src/anthropic/handlers.rs`：`classify_local_error_for_external_fallback` 允许 transient 错误进入 `local_transient_exhausted`。
- `src/anthropic/handlers.rs`：`fallback_after_local_error_outcome_with_diagnostics` 没有重新检查本地 route state 是否还有 dispatchable 账号。

方案：

1. 把“本地尝试预算耗尽”和“本地池耗尽”拆开。
2. 在 `local_transient_exhausted` 外部 fallback 前，重新计算当前模型的 `local_pool_route_state`。
3. 若本地状态仍是 `Ready` 或 `dispatchable > 0`，禁止外部池：
   - 要么继续尝试本地账号直到本地池不可调度；
   - 要么返回本地瞬态错误，但不能打外部池。
4. 增加严格 local-first 配置，例如 `fallbackOnlyWhenLocalPoolUnavailable`，默认按当前产品期望开启。

验收：

- 构造 60 本地账号、`credentialRetryMaxAttempts=6`、前 6 个账号返回 500 的场景；本地还有 dispatchable 账号时不得外部池。
- 外部池只允许在无账号、全禁用、无模型兼容、全冷却、容量真正满等本地池不可调度状态下触发。

## P2：Redis 调度降级不再外部池，但仍会造成本地错误

结论：确认存在。

现象：升级后 `fallbackOnSchedulerRedisDegraded=false` 阻止了 Redis degraded 直接外部池，但业务仍出现大量：

```text
本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=...）
```

采集窗口中 30 分钟内：

- `local_success=7034`
- `local_error_no_fallback=1184`
- `external_pool=0`
- Redis degraded 是主要本地错误。

方案：

1. 保持 `fallbackOnSchedulerRedisDegraded=false`，避免再次把 Redis 抖动变成外部池流量。
2. 缩短调度热路径 Redis 操作：避免请求路径做大范围状态同步。
3. 对 `scheduler_state_for_credentials` 做分层：热路径只读必要字段；Admin/诊断页再读完整健康详情。
4. 评估单实例降级模式：Redis lease 不可用时允许保守本地内存准入，但必须用配置显式开启，且标明多实例不安全或需要更严格全局限流。
5. 只在 Redis degraded 错误记录中保留 operation 名称，不给所有成功请求增加大字段。

验收：

- Redis 慢/抖动时不会外部池。
- 调度错误量下降，或在单实例降级开关开启时本地请求能继续按保守容量运行。
- Admin/usage/dashboard 查询不应触发调度热路径退避。

## P3：usage 清空是高风险操作

结论：确认存在。

当前 `/api/admin/usage-records/clear` 会：

1. 清内存 usage；
2. Redis 删除 `usage:summary:*`、`usage:dashboard:*`、`usage:records:*`；
3. PostgreSQL 对所有未删除 usage 执行大范围 soft delete。

风险：

- Redis 使用 `SCAN + DEL`，`DEL` 大 key 或批量 key 会阻塞 Redis 主线程；
- PostgreSQL 大范围 UPDATE 会和 usage 写入、索引、autovacuum 竞争；
- 清理后会丢失诊断路由、usage、payload、错误证据，影响后续排查。

方案：

1. 废弃或强限制“清空全部展示记录”。
2. 复用已有后台分批 cleanup job：按 cutoff、batch size、pause、cancel/status 执行。
3. Redis 删除改用 `UNLINK` 或小批量、可让出调度的异步删除。
4. UI 文案必须提示：会删除诊断证据，可能对 Redis/PG 有压力，不只是“清展示”。

验收：

- 清理操作只入队后台任务，不在 Admin 请求中同步执行大范围更新/删除。
- 高基数 Redis usage key 清理不会让调度 Redis 热路径退避。
- 清理前后有审计记录：范围、预估条数、执行策略、任务状态。

## P4：健康均衡模式下部分账号并发被打满，其他账号闲置

状态：高度怀疑存在实现/策略问题，需要补测试复现。

用户现象：

- `loadBalancingMode=health_balanced`；
- 本地账号充足，例如 60 个账号；
- 每个账号并发槽位充足，例如每个 10；
- 没有明显冷却；
- 但某些账号被打到 10 并发，其他账号仍是 0 或 1。

这个现象不一定违反“并发不超限”，但违反“健康均衡下不应在账号充足时过度集中”的产品预期。

### 已确认的可能原因

#### 1. sticky 会话优先于健康均衡

调度流程里，`acquire_context_for_session_with_mode` 会先检查 session sticky：

1. 读取 `bound_credential_id(session_id)`；
2. 如果绑定账号通过硬过滤和并发检查，直接选它；
3. 只有 sticky 不可用时才进入 `select_next_credential_excluding`，也就是健康均衡选择。

因此同一个会话或少数会话的并发请求会优先堆到绑定账号，直到该账号并发槽满，才临时 fallback 到其他账号。当前测试也把这个行为当成预期：绑定账号并发释放后，会话会回到原绑定账号。

影响：

- 如果 Claude Code 同一 session 下发出多轮长流式请求、工具/MCP 相关请求或重试请求，sticky 会导致同账号 in-flight 拉高；
- 健康均衡不是每次都参与选择，所以不能保证全局即时均衡。

方案：

- 增加 load-aware sticky 策略：sticky 只在绑定账号低于某个负载阈值时优先使用，例如 `stickyMaxLoadRatio=0.5` 或 `stickyMaxInFlightSkew=1`。
- 绑定账号超过阈值时，临时走健康均衡，但不一定解除 sticky；请求结束后仍可保留会话亲和。
- 增加配置模式：`strict`（当前行为）、`load_aware`（推荐）、`disabled`。

#### 2. `schedulerTopK=3` 对 60 个账号过小

`health_balanced` 先按 score 取前 `schedulerTopK`，默认只有 3，然后只在这 3 个账号里加权随机。

当大量账号分数相同或接近时，排序 tie-break 使用账号 id，导致最开始只有少数账号进入 topK。并发突发时，这几个账号会先被集中选择。

方案：

- 对大账号池提高默认/推荐 `schedulerTopK`，例如 `min(32, candidate_count)` 或根据账号数自动扩展。
- 同分或近似同分时随机打散，不要固定低 id 优先。
- 增加“最低负载候选池”策略：优先取所有最低 in-flight 桶，再在桶内按健康分采样。

#### 3. 选中账号后并发槽竞争失败时，普通等待模式会等待而不是重选

当前逻辑中，如果选中账号后 `acquire_in_flight_slot` 返回 `None`：

- `FailFastOnCapacity` 会把该账号临时排除，然后重选；
- 普通 `WaitForCapacity` 会进入队列等待。

这会产生一个问题：如果只是被选中的那个账号满了，但其他账号仍有空槽，普通请求也可能在这个账号后面等待，而不是马上排除它重选其他空闲账号。

这在多实例、Redis 状态同步延迟、并发突发时更明显：多个请求可能在本地镜像还没看到 in-flight 变化前选中同一批账号，Redis lease 拒绝后却开始等待，导致其他账号闲置。

方案：

- 对 `acquire_in_flight_slot(id)` 返回“该账号槽位满/竞争失败”的情况，普通模式也应临时排除该账号并重新选择，直到确认所有候选都不可调度才排队等待。
- 需要区分失败原因：单账号满、全局满、Redis degraded、队列满。只有全局满或全候选满才进入等待。

#### 4. 健康分里的 latency 权重可能压过负载权重

默认：

- `schedulerLoadWeight=100`
- `schedulerLatencyWeight=0.01`

负载项按比例计算：账号从 0 到满载，最多增加约 100 分。延迟项按毫秒计算：10 秒延迟差就是 100 分。

所以如果某些账号 latency EWMA 比其他账号低 10 秒以上，即使它们已经很忙，也可能仍然比空闲但慢的账号得分更好，继续被选中直到槽位打满。

方案：

- 降低 latency 权重，或对 latency 使用 log/分段上限，避免它压过负载公平性。
- 增加硬性负载优先层：账号负载超过某阈值时，除非其他账号不可用，否则不继续选它。
- 如果目标是“账号充足时先铺开并发”，可以使用或强化 `weighted_least_inflight`，再叠加健康硬过滤。

#### 5. warmup 分组可能让新账号看起来闲置

如果部分账号仍有 `warmup_remaining > 0`，当前调度会按 warmup 目标份额让预热账号参与，而不是和 ready 账号完全均分。大量账号刚导入时，ready 账号会承担更多流量。

方案：

- 现网判断时需要把 `warmup_remaining` 加入证据。
- 如果不需要预热，允许清空或关闭 warmup。
- UI 上需要明确显示“账号闲置是因为预热策略”，避免误判为调度 bug。

### 建议修复顺序

1. 先补诊断查询/测试，复现即时 in-flight 偏斜：60 账号、每账号 10 并发、health_balanced、长流式占用、同 session 和不同 session 两组。
2. 修复“选中账号槽位竞争失败后普通模式直接等待”的问题：能重选就重选，不能重选才等待。
3. 增加 load-aware sticky，避免一个 session 把绑定账号打满后才扩散。
4. 调整健康均衡候选策略：扩大/自适应 topK、同分随机、最低负载桶优先。
5. 重新校准默认权重：负载公平性优先于 latency 微调。

### 验收标准

- 无 sticky、无冷却、60 账号、每账号并发 10、并发请求数小于总容量时，不应出现少数账号满 10 而大量账号 0/1 的分布。
- 同 session 并发下，`load_aware` sticky 不应把绑定账号打满后才扩散。
- Redis 多实例或模拟 Redis 状态延迟下，单账号 lease 竞争失败应重选其他账号，而不是进入等待。
- 调度分布验证不能只看最终 selection count，还要看峰值 in-flight 分布和持续时间。

## P5：其他负载均衡模式也会出现账号集中打满

状态：代码层面确认存在共因；需要现网 in-flight 分布和 usage attempt 分布继续取证。

用户补充现象：不只是 `health_balanced`，其他模式也会出现某几个账号并发槽位被打满、其他账号仍然 0/1 的情况。

结论：这个现象不能只按某一个负载均衡算法解释。当前调度链路里有几类“模式外”的共享路径，会绕过或削弱任意模式的均衡效果。

### 共因 1：sticky 会话优先于所有模式

`acquire_context_for_session_with_mode` 在进入 `select_next_credential_excluding` 前，会先读取 `conversationId` 对应的绑定账号。只要绑定账号仍可用，就直接选中它，不会进入 `priority`、`balanced`、`health_balanced` 或 `weighted_least_inflight` 的算法分支。

代码证据：

- `src/kiro/provider.rs` 从 Kiro request body 的 `conversationState.conversationId` 提取会话 ID，用于账号粘性调度。
- `src/kiro/token_manager/manager.rs` 的 `acquire_context_for_session_with_mode` 先执行 `bound_credential_id` / `get_bound_credential`，命中后直接 `AcquireDecision::Selected(... sticky_bound=true ...)`。
- `src/anthropic/converter.rs` 在 high-cache 模式下，缺少 `metadata.user_id` 时也会基于稳定请求锚点派生确定性 `conversationId`，这会让没有显式 metadata 的请求也可能进入 sticky。

影响：

- 同一会话下的并发流式请求、工具调用前后重试、客户端重连，都会先压向绑定账号；
- 如果一个下游渠道复用同一个或少数几个会话 ID，会天然造成少数账号被打满；
- 切换负载均衡模式不能解决 sticky 造成的集中，因为算法分支根本没被调用。

方案：

1. 增加 `stickyMode`：`strict`、`load_aware`、`disabled`。
2. 默认建议改成 `load_aware`：绑定账号超过配置阈值时临时绕过 sticky，进入正常模式选择；不立即解除绑定。
3. 增加阈值配置，例如 `stickyMaxLoadRatio`、`stickyMaxInFlightSkew`。
4. 诊断页面显示 sticky 命中率、sticky fallback 次数、按会话维度的集中度。

### 共因 2：普通等待模式在单账号 lease 竞争失败后不会优先重选

当前 `acquire_in_flight_slot(id)` 返回 `None` 时：

- `FailFastOnCapacity`：把该账号加入本次请求的临时排除集合，然后重选；
- `WaitForCapacity`：进入等待队列，等待 dispatch wakeup。

这对所有模式都有影响。只要并发突发下多个请求短时间选中同一个账号，Redis lease 或本地并发槽可能只允许其中一部分成功。普通请求此时应该先排除“刚刚竞争失败的账号”并重选其他有槽账号；当前实现会进入等待，因此其他账号可能仍空闲。

方案：

1. 区分 `acquire_in_flight_slot` 的失败原因：单账号满、全局满、Redis degraded、Redis 错误、未知竞争失败。
2. 对单账号满/竞争失败：普通 `WaitForCapacity` 也先把该账号加入本次临时排除集合并重选。
3. 只有确认全局满、所有候选都满、全部冷却/RPM 限制时，才进入等待队列。
4. 保留最大重选次数，避免极端状态下自旋；达到上限后输出明确诊断。

### 共因 3：`priority` / `balanced` 也有固定 tie-break

`balanced_selection_key` 和 `priority_selection_key` 都以 `entry.id` 作为最后排序键。大量账号初始指标相同或接近时，低 id 账号会先被选中。随着选中计数和 in-flight 增长，理论上会逐步扩散，但在长流式/并发突发/sticky/Redis 状态同步延迟叠加时，会表现为低 id 或少数账号先被打满。

方案：

- tie-break 引入轻量随机盐或轮转游标，不要固定按低 id；
- 或先按最低 in-flight 桶筛选，再在桶内随机/加权选择；
- 对所有模式共享一个“账号充足时禁止过度集中”的硬约束层。

### 共因 4：重试会放大并改变调度形态

一个下游请求不是只占用一个账号一次。当前本地 provider 会在 429、408、5xx、网络错误、部分 400 模型不可用/协议问题、profileArn 问题等情况下换号重试。默认 `credentialRetryMaxAttempts=0` 时，最大尝试次数按账号池规模放大；显式配置时按配置上限执行。

这意味着一次请求可能先打满少数账号，再因为它们进入冷却而扩散到更多账号，最终看起来像“所有账号都被打一遍”。这不是单一模式问题，而是调度 + 重试策略的组合问题。

### 待验证证据

现网应补以下只读证据：

- 最近异常窗口内，每分钟 `usage_records` 请求数 vs `credentialAttempts` 总数；
- 按 `credentialAttempts[].credentialId` 聚合的 429/5xx/timeout 分布；
- usage 记录里的 `stickyBound`、`fallbackFromSticky` 比例；
- Redis 当前 in-flight key 的账号分布；
- 每个账号 `warmup_remaining`、cooldown、model health、recent selection count。

验收：

- 非 sticky 场景、账号健康且容量充足时，各模式都不能出现少量账号满槽而大量账号空闲。
- sticky 场景在 `load_aware` 下可以临时扩散，不能等绑定账号满槽后才扩散。
- 单账号 lease 竞争失败时优先重选其他可调度账号。

## P6：下游渠道低 RPM/20 并发，但系统内上游 RPM 突然暴涨

状态：代码层面确认存在放大风险；2026-07-15 17:15 CST 已完成现网只读取证，确认存在单请求多次上游 credential attempts，同时确认 usage 缺少下游 API key/channel 归因字段。

用户现象：一个下游渠道接入当前服务，对方声称自身 RPM 不高且限制 20 并发，但当前系统里突然出现很高 RPM，导致所有本地账号被打到 429 或其他上游错误。

结论：当前系统没有“按下游渠道/API Key”的强制并发和 RPM 准入控制，也没有把 usage 可靠归因到某个请求 Key。所以下游说“我限了 20 并发”只能作为外部口径，当前服务端不会强制执行，也无法仅靠现有 usage 精确证明是哪一个请求 Key/渠道触发。

### 代码证据 1：请求 API Key 只是鉴权，不是渠道限流实体

当前请求 Key 结构只有 `id/apiKey/maskedApiKey/primary`，没有 `name/rpm/concurrency/modelLimit/routeLimit` 等字段。

运行时 `RequestApiKeyStore` 只是把 key 做 SHA-256 后放进内存 HashSet。请求进入 `auth_middleware` 后只判断 `contains(key)`，通过后直接放行，没有 per-key 并发 lease、per-key RPM 窗口，也没有把 key id 写入 `UsageRecord`。

影响：

- 一个请求 Key 被多个客户端/实例共享时，当前服务不知道它们是同一渠道还是多个渠道；
- 下游自己的限流失败、重连、重试、流式断开重发，当前服务不会在入口阻断；
- usage 只能看到请求总数、模型、会话、账号尝试链，不能直接按“下游渠道”归因。

方案：

1. 把请求 API Key 升级为一等“下游渠道”：`id/name/hash/enabled/rpm/maxConcurrent/modelRules/routeRules`。
2. 鉴权后把 `requestApiKeyId` 或不可逆 hash 前缀写入 request extensions 和 `UsageRecord`，不要保存明文 key。
3. 增加 Redis per-key admission：并发 lease + 60s RPM 滑窗/令牌桶。
4. UI 安全页支持每个请求 Key 配置名称、RPM、并发、启停、备注。
5. 用 429 明确拒绝超过渠道限制的入口请求，避免把压力传给本地账号池。

### 代码证据 2：一个下游请求可以放大成多次上游账号尝试

本地 provider 的 `call_api_with_retry` 会在一个下游请求内循环获取凭据并发槽、调用上游、失败后换账号重试。`credentialRetryMaxAttempts=0` 表示自动按账号池规模放大，至少覆盖一轮可用凭据；显式配置时按配置上限。

此外，流式路径还有“首个下游 SSE 事件提交前”的安全换号重试，payload guard/cachePoint retry、外部 fallback/local rescue 也可能额外发起上游调用。

因此真实上游请求量大致是：

```text
入口请求 RPM × 单请求本地凭据尝试数 × 首输出前流式重试层 × payload/cachePoint retry 层 × 客户端重连/重复请求
```

示例：如果入口只有 80 RPM，但每个请求在 429/5xx 风暴中尝试 6 个本地账号，实际本地上游尝试可到约 480 RPM；如果再叠加客户端超时重连或首输出前重试，峰值会继续放大。20 并发也不能直接等价为低 RPM，因为短请求/失败重试可以在 20 并发下产生很高每分钟请求数。

### 代码证据 3：429 风暴会形成反馈回路

当部分账号开始 429：

1. 当前请求把账号标记为临时冷却并换其他账号；
2. 其他并发请求也在换号；
3. 每个请求都可能继续尝试多个账号；
4. 如果没有入口渠道限流和全局 retry budget，瞬间把 429 从少数账号扩散到更多账号；
5. 最终看起来像“所有账号都被打到 429”。

这解释了“下游看起来不高，但系统里所有账号都 429”的可行路径。是否就是这次现网事实，需要用 `credentialAttempts` 聚合确认。

### 本轮现网证据结论

本轮在 `152.53.243.159` 上完成只读取证，证据文件位于：

- `tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/raw/pg-diagnostics-v3.txt`
- `tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/raw/pg-channel-and-model-diagnostics-v2.txt`
- `tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/raw/pg-minute-and-transient-diagnostics.txt`
- `tmp/prod-evidence/20260715-165347-152.53.243.159-redis-scheduler-channel/raw/pg-transient-and-fields-diagnostics.txt`

确认点：

1. 单请求可以放大为多次本地账号尝试：
   - `req_01VvA1SGFSwGVjDKReyPz4Nu`：`/cc/v1/messages`、`claude-sonnet-5`，一个请求内 13 次本地账号尝试，前 12 次 429/500，最后 200 成功。
   - `req_01xhMNkUuwf9Pkys8dUCfNp6`：`/cc/v1/messages`、`claude-opus-4-8`，一个流式请求内 8 次本地尝试，前 7 次 429/500，最后 200 成功。
2. 高峰分钟有两类形态：
   - Redis degraded storm：例如 15:40 CST 812 条下游记录，其中 801 条 Redis degraded，local attempts 只有 11，因为大部分请求在调度层被快速拒绝。
   - retry amplification：正常成功/重试窗口里，local attempts 明显高于成功请求数，单请求最高 13 次。
3. 当前 usage 不保存 request API key / channel id / channel name。现网 runtime config shape 查询也没有可用于归因的 `requestApiKeys` 结构字段。因此无法仅靠现有 usage 精确裁定“是哪一个下游渠道/API key”。

### 后续只读证据清单

如果再次遇到具体异常窗口，仍建议保留以下最小查询清单。所有查询必须 `BEGIN READ ONLY` + `statement_timeout` + 时间窗口约束。

1. 请求数 vs 上游账号尝试数：

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
WITH recent AS (
  SELECT created_at,
         COALESCE(jsonb_array_length(data->'credentialAttempts'), 0) AS attempt_count,
         status,
         error_type,
         data->>'routeKind' AS route_kind,
         data->>'routeSubtype' AS route_subtype
  FROM usage_records
  WHERE deleted_at IS NULL
    AND created_at >= now() - interval '60 minutes'
)
SELECT date_trunc('minute', created_at) AS minute,
       count(*) AS downstream_requests,
       sum(GREATEST(attempt_count, 1)) AS estimated_upstream_attempts,
       sum(CASE WHEN attempt_count > 1 THEN 1 ELSE 0 END) AS retried_requests,
       max(attempt_count) AS max_attempts,
       count(*) FILTER (WHERE route_kind = 'external_pool') AS external_pool_requests,
       count(*) FILTER (WHERE status <> 'success') AS failed_records
FROM recent
GROUP BY 1
ORDER BY 1 DESC
LIMIT 80;
COMMIT;
```

判读：如果 `estimated_upstream_attempts / downstream_requests` 明显大于 1，说明内部重试放大成立；如果入口请求本身也异常高，则要追入口渠道/客户端。

2. 账号 429/错误扩散：

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
WITH recent AS (
  SELECT created_at, data
  FROM usage_records
  WHERE deleted_at IS NULL
    AND created_at >= now() - interval '30 minutes'
), attempts AS (
  SELECT created_at, jsonb_array_elements(COALESCE(data->'credentialAttempts', '[]'::jsonb)) AS a
  FROM recent
)
SELECT a->>'credentialId' AS credential_id,
       a->>'status' AS status,
       a->>'errorType' AS error_type,
       a->>'action' AS action,
       count(*) AS count,
       min(created_at) AS first_seen,
       max(created_at) AS last_seen
FROM attempts
WHERE (a->>'status') IN ('429','408','500','502','503','504')
   OR COALESCE(a->>'errorType','') <> ''
GROUP BY 1,2,3,4
ORDER BY count DESC, last_seen DESC
LIMIT 100;
COMMIT;
```

判读：如果很多账号在同一窗口出现 429 且单请求 attempt 数高，说明是 retry storm；如果只有少数账号被反复打满，优先看 sticky/调度集中。

3. sticky 与会话集中：

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
SELECT date_trunc('minute', created_at) AS minute,
       count(*) AS requests,
       count(*) FILTER (WHERE (data->>'stickyBound')::boolean IS TRUE) AS sticky_bound,
       count(*) FILTER (WHERE (data->>'fallbackFromSticky')::boolean IS TRUE) AS fallback_from_sticky,
       count(DISTINCT conversation_id) AS conversations
FROM usage_records
WHERE deleted_at IS NULL
  AND created_at >= now() - interval '60 minutes'
GROUP BY 1
ORDER BY 1 DESC
LIMIT 80;
COMMIT;
```

判读：如果请求量集中在少数 conversation 且 sticky 命中高，账号集中是预期实现导致，不是单纯均衡算法问题。

4. 当前 Redis in-flight 分布：

- 禁止 `KEYS *`；只允许窄前缀 `SCAN`，并限制条数。
- 需要先从代码或 Redis keyspace 确认前缀，再读取 scheduler in-flight/count key 的样本。

### 修复方向

1. 先加入口 per-key/channel 限流和并发准入，避免信任下游自称限流。
2. 加单请求全局 retry budget，统一约束 provider retry、stream pre-commit retry、payload/cachePoint retry、external fallback/local rescue，防止层层相乘。
3. 对 429 风暴增加模型/账号池级 breaker：当短窗口内大量账号同模型 429，停止继续扩散换号，按 `Retry-After` 或配置窗口直接对入口返回明确 429/503。
4. `credentialRetryMaxAttempts=0` 在大账号池下不要默认覆盖整池；建议默认上限从“账号数”改为小的安全值，并提供“全池巡检式重试”显式开关。
5. usage 增加渠道维度、attempt amplification 指标、每分钟 upstream attempt RPM，用于证明是入口暴涨还是内部放大。

验收：

- 给某个请求 Key 配 20 并发和固定 RPM 后，超过限制的请求在入口直接 429，不消耗本地账号槽位。
- 构造 429 风暴时，一个下游请求最多消耗配置允许的上游 attempts，不会把 60 个账号全部打一遍。
- Dashboard 能同时显示 downstream request RPM 和 upstream credential-attempt RPM。
- usage 能按请求 Key/渠道聚合，不需要依赖人工猜测。

## P7：Redis 调度热路径脆弱，75ms 超时和共享 Redis 压力会周期性打断本地调度

状态：跨机器证据高度一致，确认是独立问题；不是单纯“账号并发不够”。

这个问题和 P6 不应强行合并。P6 解决的是入口渠道和 retry amplification；P7 解决的是本地账号调度依赖 Redis 的热路径可靠性。两者会互相放大：P6 增加请求和尝试次数，会增加 P7 的 Redis 调度压力；P7 一旦进入退避，又会造成大量请求快速失败或按配置转外部池。

### 现象

在另一台现网机器 `152.53.194.142` 的只读分析里，已观察到：

- 服务版本：`0.0.109`，revision `401473ca1649997bdeccf4468e3add1bdb187248`；
- 本地账号并发容量没有打满：15 个账号、每个 10，总并发 150，异常 usage 里 `globalInFlight=45`；
- errorMetadata.selectionFailure 显示 `stage=dispatch_queue`、`queueDepth=0`、`rejectedAccountCount=0`、`waitableAccountCount=0`、`sampledAccounts=[]`；
- 这说明请求不是“选了一圈账号发现都满了”，而是在 Redis 调度协调层直接失败；
- 6 小时内出现 2146 条 `Redis 调度协调状态不可用`；
- 峰值分钟可到 400+ 条；
- Redis 同时出现 AOF rewrite / RDB save 压力，应用日志出现 `原子写入 Redis 会话绑定超过 75ms`。

在 `152.53.243.159` 的已有本地证据包中，也已观察到同类问题：

- 运行版本同为 `0.0.109` / revision `401473ca1649997bdeccf4468e3add1bdb187248`；
- 本地账号池健康：60 个 enabled，failure/refresh failure 为 0，总 RPM 约 2200，总并发约 625；
- `fallbackOnSchedulerRedisDegraded=false`；
- 30 分钟窗口内：`local_success=7034`、`local_error_no_fallback=1184`、`external_pool=0`；
- 主要错误就是 `本地账号调度容量暂不可用（Redis 调度协调状态不可用...）`；
- 分钟聚合里 13:40-14:07 CST 多个窗口出现每分钟几十到两百多条 Redis degraded 错误；
- 旧证据还显示升级前/重启边界附近 Redis 有 `usage:summary:cache_read` 高基数 HMGET 慢查询，单次约 93ms、触及约 188k 字段。

2026-07-15 17:15 CST 重新登录取证后，进一步确认：

- 当前容器仍是 `0.0.109` / `401473ca1649997bdeccf4468e3add1bdb187248`，app 当前容器 11:58 CST 启动。
- 当前 runtime `redis.keyPrefix=kiro_rs:59137`，不是代码默认的 `kiro_rs:local`。
- 最近 5 分钟窗口内仍出现 82 条 Redis degraded；代表样本 `globalInFlight=63..66`，本地配置总并发约 625，`queueDepth=0`、`sampledAccounts=[]`、`rejectedAccountCount=0`、`waitableAccountCount=0`。
- targeted app logs 在 17:05 CST 捕获到当前版本实际触发点：`原子写入 Redis 会话绑定超过 75ms`、`原子清理 Redis 会话软失败超过 75ms`。
- Redis 当前 prefix 下仍有 `usage:summary:cache_read` 约 30212 bucket、`usage:records:index` 约 72283、scheduler key sample 约 15579，其中 session key 约 14650。
- 旧 slowlog 中 `usage:summary:cache_read HMGET` 的最新时间约 11:55-11:56 CST，早于当前 app 容器启动；所以它是旧版本/清理/重启边界的遗留证据，不能直接证明当前版本仍在执行同一条大 HMGET。
- 当前 Redis `appendonly=no`，但 `save=3600 1 300 100 60 10000`，Redis stats 显示 `rdb_saves=14382`、`latest_fork_usec=8659`、`instantaneous_ops_per_sec≈4624`。这说明当前机器的 Redis 压力并不只来自 AOF，RDB save、keyspace/命令量和 app 热路径设计都需要一起看。

### 代码证据

调度热路径超时是硬编码常量：

```rust
const SCHEDULER_REDIS_HOT_OP_TIMEOUT: StdDuration = StdDuration::from_millis(75);
const SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE: StdDuration = StdDuration::from_secs(2);
const SCHEDULER_REDIS_DEGRADED_BACKOFF_MAX: StdDuration = StdDuration::from_secs(30);
```

只要 Redis 热路径操作超过 75ms，`mark_scheduler_redis_degraded` 会给本进程设置 2 到 30 秒退避窗口。退避窗口内 `scheduler_redis_hot_path_allowed=false`，调度 Redis 准入被跳过。

热路径包括但不限于：

1. 占用凭据并发 lease：`acquire_dispatch_lease`；
2. 读取/写入 sticky 会话绑定：`get_session_binding` / `set_session_binding`；
3. 调度队列 lease；
4. Redis scheduler state 同步；
5. 记录 scheduler selection window / RPM 窗口。

关键失败路径：

```text
Redis hot op > 75ms
→ mark_scheduler_redis_degraded
→ degraded_until = now + 2s/4s/.../30s
→ acquire_in_flight_slot 返回：本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=N）
→ fallbackOnSchedulerRedisDegraded=false 时，不走外部池，记录 local_error_no_fallback
```

### 为什么这是程序设计问题，不只是 Redis 配置问题

Redis 配置和宿主机 IO 会影响触发概率，但当前程序设计有几个放大点：

1. 75ms 对容器化 Redis + AOF/RDB + 高写入业务来说非常紧。AOF rewrite、RDB save、短暂 IO 抖动、CPU 抢占、网络抖动都可能超过。
2. 调度、sticky、RPM selection window、usage summary/dashboard 共用同一个 Redis 压力面。即使 usage writer 是异步，Redis 是单线程，usage 写入/大 key 查询仍能阻塞调度命令执行。
3. 一次 Redis 操作超时会让整个进程进入 2-30s 退避，而不是只降级该操作或只影响对应请求。
4. 退避期间本地账号实际容量仍可能充足，但调度层直接拒绝；这会制造“账号正常但请求不可调度”的错误体验。
5. `fallbackOnSchedulerRedisDegraded=false` 避免了错误打外部池，但没有恢复本地服务能力；只是把问题从“错路由外部池”变成“本地 429/No account ready”。

因此根因应表述为：Redis 抖动是触发条件；调度热路径对 Redis 的强依赖、75ms 硬超时和整进程退避是程序侧脆弱点。

### 不应做的修复

- 不应只把 `fallbackOnSchedulerRedisDegraded=true` 当根修。它只是把错误转外部池，成本和路由都会变；在“本地有容量不应外部池”的产品目标下不合适。
- 不应只把 75ms 调大。调大能减少错误，但会增加请求等待、堆积和尾延迟，并掩盖 Redis 压力。
- 不应继续让 usage/dashboard 的高基数读写与 scheduler lease 共用同一热 Redis，无论是否异步。

### 修复方向

#### 1. 调度 Redis 与 usage/dashboard Redis 分离

最根本的隔离是让 scheduler hot path 使用独立 Redis 或独立 Redis DB/连接池/实例，避免 usage summary、dashboard、usage cleanup、大 key 查询、AOF/RDB 写放大影响调度。

配置建议：

- `schedulerRedisUrl` / `schedulerRedisPrefix`：调度专用；
- `usageRedisUrl` / `usageRedisPrefix`：统计展示专用；
- 未配置时保持当前单 Redis，兼容旧部署；
- UI 明确提示：生产建议拆分，调度 Redis 不建议承载 usage/dashboard 高基数数据。

#### 2. 调度 hot path 超时配置化，并分操作配置

将当前硬编码 `75ms` 改为配置：

- `schedulerRedisHotOpTimeoutMs`，默认可保守提高，例如 150ms；
- `schedulerRedisStateSyncTimeoutMs`；
- `schedulerRedisLeaseTimeoutMs`；
- `schedulerRedisSessionBindingTimeoutMs`。

注意：配置化不是为了无限调大，而是为了让不同部署能按 Redis/磁盘性能校准。

#### 3. 退避策略改为“短路局部操作”，避免整进程长时间停摆

当前一次 hot op 超时会让进程在后续 2-30s 内跳过调度 Redis 准入。建议改造：

- 区分 operation：session binding 超时不应等价于 dispatch lease 超时；
- dispatch lease 超时才影响准入；session binding 超时可退回本地 sticky cache 或直接不 sticky；
- 退避窗口加 jitter，避免多实例同步进入/退出；
- 退避窗口内允许低频探测恢复，不要完全等窗口结束。

#### 4. 单实例安全降级模式

当明确只有一个 app 实例时，可以配置允许 Redis degraded 时使用本地内存 lease 兜底：

- `schedulerRedisDegradedLocalFallbackEnabled`；
- `schedulerRedisDegradedLocalFallbackMode = single_instance_only`；
- 多实例下默认禁止，避免突破全局并发；
- 降级期间强制更保守的全局并发/RPM，例如按配置乘以 0.5。

这能解决“Redis 轻微抖动但本地账号明明有容量”的可用性问题。

#### 5. 降低 scheduler Redis 命令复杂度

当前 `acquire_dispatch_lease` Lua 脚本会在热路径清理过期 lease，并维护 per-credential/global 多个 zset/hash/count key。建议：

- 热路径只做必要 lease CAS；
- 过期 lease 清理移到后台定时任务，或限制每次清理数量；
- selection window/RPM 的 `ZADD/ZCOUNT/ZRANGEBYSCORE` 可用更轻的 rolling bucket 计数替代；
- session binding 写入不要阻塞 dispatch lease 成功路径。

#### 6. 现网 Redis 配置建议作为运维止血，但不是代码根修

对已经触发 AOF/RDB 抖动的部署，可短期评估：

- 调整 RDB `save` 频率，避免高写入时频繁后台保存；
- 提高 `auto-aof-rewrite-percentage` 或 `auto-aof-rewrite-min-size`；
- 评估 `no-appendfsync-on-rewrite yes` 的持久性风险；
- Redis 放到更快磁盘；
- scheduler Redis 使用独立实例并降低持久化强度，因为调度 lease 是可重建运行态，不应和业务事实数据采用同一持久化等级。

### 现网继续取证清单

对 `152.53.243.159` 需要重新取证时，最小只读证据是：

1. 最近 2 小时 Redis degraded 错误分钟聚合；
2. 同窗口 local success / local error / external fallback route 聚合；
3. errorMetadata.selectionFailure 样本，重点看 `stage`、`globalInFlight`、`queueDepth`、`rejectedAccountCount`、`waitableAccountCount`；
4. Redis `INFO persistence`、`INFO commandstats`、`INFO stats`、`SLOWLOG GET 50`；
5. Redis key cardinality：scheduler、usage summary、records index；
6. app 日志只按窗口 grep `Redis 调度热路径`、`超过 75ms`、`调度协调状态不可用`；
7. runtime config 的 `fallbackOnSchedulerRedisDegraded`、账号并发/RPM、外部池配置。

禁止做：`KEYS *`、全表扫描、全量 docker logs、Redis 写命令、重启、清理 usage。

验收：

- Redis AOF/RDB/usage 写入抖动时，调度错误不再成百上千爆发；
- 本地账号有容量时，不因为 session binding 或 usage Redis 抖动整进程拒绝本地调度；
- usage/dashboard 查询和清理不会触发 scheduler Redis degraded；
- 单请求错误 metadata 能清楚区分“账号容量满”和“Redis 调度协调不可用”。
