# 最近 24 小时首字超过 10s 请求分析

分析时间：2026-06-30  
现网版本：`0.0.67`, revision `26c3a9a32b91ecf2591473370bc9611577704a6f`  
本地版本：`0.0.70`, HEAD `451602a`，叠加本文记录的本地未提交优化  
范围：仅基于现网最近 24 小时可见 usage 记录和只读日志观察，不改现网、不压测现网。

## 结论摘要

现网 usage 里 `firstTokenLatencyMs > 10000` 的慢请求不是单一原因。按 last-24h 快照分类，主因是上游响应头等待、上游已经出 body 但迟迟没有有效输出、多个中等耗时叠加、账号/上游错误重试链，以及少量首个 body chunk 等待。另有一类重要问题是 `payloadGuardMs` 很大但没有计入 `firstTokenLatencyMs`，导致 usage UI 的“首字”低估了客户端真实等待。

从代码角度看，现网 v0.0.67 和本地 v0.0.70 差异很大。本地版本已经新增流式 phase trace、Redis 调度热路径 75ms 预算、退避降级、独立 Redis 调度连接、并发 lease tombstone、凭据统计增量缓冲、selection failure 结构化错误等能力。这些改动对现网日志里出现的 Redis 热路径超时、并发槽状态竞态、账号选择不可解释有直接改善价值。

但本地 HEAD 仍有几类可改进点：真实客户端首字口径仍未把 payload guard 纳入；usage rollup 仍在 `record_batch` 同一个 Postgres 事务里同步更新；外部池选择仍需要读取 `external_upstream_pools`；后台 best-effort 存储任务没有统一队列、并发上限和隔离池。这些仍可能在高并发或数据库慢时放大首字延迟。

## 本次本地优化落地

本节记录 2026-06-30 已在本地代码实现的两类低风险优化。它们都不改变业务 API 协议，不增加请求并发，不增加后台队列，不引入新的 Redis/PgSQL 热路径，也不依赖生产验证。

### 1. 外部池转发少一次 Postgres 查询

变更文件：`src/external_pool.rs`。

原逻辑在 `ExternalPoolManager::forward()` 开始阶段为了计算默认 retry 上限，会先执行一次 `postgres.list_external_pools(false)` 得到 enabled pool 数；随后真正选择外部池时又会在 `select_pool_with_availability_uncached()` / `scan_pool_availability_uncached()` 内再次执行 `list_external_pools(false)`。也就是说，外部池转发的首个 attempt 前至少有两次同表查询机会。

本次改为：

- 当 `external_pool_retry_max_attempts > 0` 时，继续使用显式配置，不改变语义。
- 当 `external_pool_retry_max_attempts == 0` 时，不再提前查 DB；第一次 uncached selection 已经会得到 `PoolAvailabilitySnapshot`，直接用其中的 `eligible_pools.max(1)` 计算默认 retry 上限。
- `payload_guard_retry_config` 仍额外增加一次 retry 预算，保持旧语义。

预期效果：

- 对外部备用池请求，减少首个 upstream attempt 之前的一次 PgSQL pool acquire 和一次 `external_upstream_pools` 查询。
- 在现网已经观察到 `SELECT external_upstream_pools` 可达约 `9.19s` 的情况下，这个优化能真实减少外部池路径的本地首字前置等待。
- 若数据库正常，这个优化收益较小但不会增加额外成本。

边界：

- 外部池选择本身仍需要读配置。要彻底避免 PgSQL 查询，需要后续做外部池配置内存缓存或版本化缓存。
- 本优化只减少重复读取，不解决 Postgres pool 被 usage rollup 占满的问题。

验证：

- 新增单测 `external_pool_default_retry_attempts_cover_eligible_pools_and_payload_guard_retry`，覆盖 0 个 eligible pool、多个 eligible pool、payload guard retry 追加预算三种情况。

### 2. 瞬态失败后当前请求内临时排除失败账号

变更文件：`src/kiro/token_manager/manager.rs`、`src/kiro/provider.rs`。

现网 `credential_retry_chain` 数量不大，但尾部很长，p95 约 `68.343s`，max 约 `299.281s`。其中一种可由代码改善的情况是：上游 429/408/5xx、网络错误、非 eventstream 的 retryable protocol 错误、未知可重试错误之后，如果当前账号没有被本次请求临时排除，下一轮调度可能又命中同一个刚失败或刚变慢的账号，造成 retry 链首字累加。

本次新增：

- `MultiTokenManager::has_alternate_usable_credential_cached(...)`：只读当前进程内存态判断是否存在其他可调度账号，不触发 Redis/PgSQL 同步。
- `KiroProvider::maybe_exclude_after_transient_failure(...)`：上游瞬态失败写入 cooldown/health 后，如果本机内存态确认还有备选账号，就把当前账号加入当前请求的 `excluded_ids`。
- API 和 MCP 两条 retry 路径都接入该 helper，包括 network send error、retryable non-eventstream/protocol、429/408/5xx、payment-required transient、unknown retryable。

预期效果：

- 对本次请求内的 retry chain，优先换到本机已知可调度账号，避免重复尝试同一失败账号。
- 不增加 Redis/PgSQL I/O；相比原有 `has_alternate_usable_credential(...)`，新增 cached 判断不会在失败路径再做一次 Redis 调度状态同步。
- 不增加内存占用级别：复用每个请求已有的 `excluded_ids: HashSet<u64>`，只可能插入当前失败账号 id。
- 不降低并发上限：账号全局并发、账号并发、RPM、cooldown 仍由原有 scheduler 判断；本次变更只影响当前请求的重试候选集合。
- 如果没有其他可用账号，helper 不排除当前账号，避免“唯一可用账号被排除后请求必然失败”的回归。

边界：

- 这不会减少纯上游首包慢，也不会减少上游已经开始 body 但长时间没有有效输出的情况。
- 这不能替代账号级 cooldown/RPM 策略，只是减少当前请求 retry 链反复命中同账号的概率。
- 分布式多实例下 cached 判断只代表本进程视角；下一轮真正调度仍会走原有 Redis/本地 scheduler 检查，所以不会绕过全局状态。

验证：

- 新增单测 `test_cached_alternate_usable_credential_uses_current_memory_state`，验证当前账号 transient failure 后，本机内存态能发现另一个可调度账号；当唯一备选被本次请求排除后返回 false。
- 新增单测 `test_cached_alternate_usable_credential_is_false_for_single_active_credential`，验证只有一个 active 账号时不会误报 fallback。

## 现网样本

现网只读快照的 usage 可见窗口约为 `2026-06-29 06:40:25Z` 到 `2026-06-29 17:39:28Z`。查询时服务仍在写入，下面数字会有少量漂移。

| 指标 | 数值 |
| --- | ---: |
| usage records | 约 `104,236` |
| 有 `firstTokenLatencyMs` 的记录 | 约 `84,618` |
| `firstTokenLatencyMs > 10s` | 约 `6,685`, 占 `7.90%` |
| 估算 `payloadGuardMs + firstTokenLatencyMs > 10s` | 约 `8,659`, 占 `10.23%` |
| `payloadGuardMs >= 10s` | 约 `880` |
| `firstTokenLatencyMs <= 10s` 但加上 payload guard 后超过 10s | 约 `1,976` |

### 慢首字原因分类

| 分类 | 数量 | 占慢请求 | 典型特征 | 判断 |
| --- | ---: | ---: | --- | --- |
| `dominant_upstream_header_wait` | `4,248` | `63.54%` | p50 `14.292s`, p95 `44.392s`, max `127.257s` | 主要是上游响应头慢；其中约 `1,037` 条有明显本地调度/存储前置开销 |
| `dominant_post_chunk_no_output` | `1,307` | `19.55%` | p50 `33.126s`, p95 `149.369s`, max `496.962s` | 上游 body 已开始，但代理未看到可计为首字的有效输出事件 |
| `combined_moderate_components` | `929` | `13.89%` | p50 `11.797s`, p95 `15.750s` | header 等待、chunk gap、调度等多个中等耗时叠加 |
| `credential_retry_chain` | `194` | `2.90%` | p50 `14.266s`, p95 `68.343s`, max `299.281s` | 429/500/403 等失败后换账号或重试造成累计等待 |
| `dominant_first_body_chunk_wait` | `8` | `0.12%` | p50 `32.463s`, p95 `33.340s` | 响应头到了但首个 body chunk 很慢 |

### 现网日志证据

慢请求集中时段附近观察到以下只读日志信号：

- `PgSQL usage 批量写入耗时较长 elapsed_ms=40167`
- `sqlx::pool::acquire acquired_after_secs=11-13`
- `SELECT credential_id, success_count, selection_count... FROM credential_stats` 约 `8.97s`
- `SELECT external_upstream_pools ...` 约 `9.19s`
- `usage_rollup_totals` upsert 约 `9.17s`
- `Redis 调度热路径不可用，本进程暂时降级为本地调度: 占用 Redis 凭据并发槽超过 75ms`
- `更新 Redis 凭据限流状态超过 75ms`
- 存在 429 后 cooldown 约 `56s` 的账号重试链

这些日志说明：慢首字不是只有上游慢。至少一部分 header-dominant 请求在进入真正 upstream HTTP attempt 之前，已经被本地调度、Postgres pool acquire、Redis 热路径或 background usage 写入挤压。

## usage 口径问题

现网和本地当前代码都存在一个观测口径问题：`payloadGuardMs` 发生在 `RequestUsageContext` 计时基准之后，因此不会进入 `firstTokenLatencyMs` 和 `durationMs`。

本地代码锚点：

- `RequestUsageContext::elapsed_ms()` 使用 `started_at.elapsed()`，见 `src/anthropic/handlers.rs:1123` 到 `src/anthropic/handlers.rs:1131`。
- `mark_payload_guard_latency()` 只把 elapsed 写进 `latency.payload_guard_ms`，见 `src/anthropic/handlers.rs:1133`。
- `/ha/v1/messages` 在 payload guard 后才 `prepare_usage_context(...)`，然后 `mark_payload_guard_latency(payload_guard_elapsed)`，见 `src/anthropic/handlers.rs:3925` 到 `src/anthropic/handlers.rs:3944`。
- `/cc/v1/messages` 同样在 payload guard 后创建 usage context，见 `src/anthropic/handlers.rs:6453` 到 `src/anthropic/handlers.rs:6472`。
- 落库时 `first_token_latency_ms` 直接取 `self.request.first_token_latency_ms()`，`latency_trace` 另存，见 `src/anthropic/handlers.rs:2120` 到 `src/anthropic/handlers.rs:2123`。

因此，`payloadGuardMs > durationMs` 或 `payloadGuardMs + firstTokenLatencyMs` 才接近客户端真实首字等待，并不是数据坏了，而是当前指标语义不完整。这个问题会让真实超过 10s 的请求低估约两千条。

## “处理分片慢”的含义

这里的“分片”通常对应 HTTP streaming body 里的 chunk，或者 chunk 中解出来的 SSE/eventstream event。它不是一个单一耗时代码块，而是从“上游已经开始返回 body”到“代理识别到可以算首字的有效输出”之间的阶段。

在本地代码里可以拆成几层：

- `firstUpstreamChunkMs`：收到上游第一个 body chunk 的时间。代码在本地流式路径收到 `body_stream.next()` 后调用 `mark_first_upstream_chunk()`，见 `src/anthropic/handlers.rs`；外部池流式路径也有同类标记，见 `src/external_pool.rs`。
- `firstOutputDeltaMs`：代理真正识别到第一个有效输出的时间。有效输出包括 thinking delta、visible text、tool/input 等可向下游表达的内容，不是任何 chunk 都算。
- `streamGapToFirstOutputMs = firstOutputDeltaMs - firstUpstreamChunkMs`：上游已经给了 body chunk，但这些 chunk 里还没有有效输出，或者代理还在等待跨 chunk 状态机凑齐语义边界。
- `chunksBeforeFirstOutput` / `eventsBeforeFirstOutput`：第一个有效输出前已经看到多少 chunk/event。

所以“处理分片慢”可能有三种不同含义：

1. **上游首个 body chunk 慢**：HTTP 响应头已经到了，但上游很久才发第一个 body chunk。这个更接近 `dominant_first_body_chunk_wait`。
2. **chunk 到了但没有有效输出**：上游发了 `message_start`、`content_block_start`、context/usage、heartbeat、空 delta、thinking 包装、tool 边界等事件，但代理不能把它们当作首字。这个对应 `dominant_post_chunk_no_output` 或较大的 `streamGapToFirstOutputMs`。
3. **代理跨 chunk 状态机等待更多内容**：例如 `<thinking>`、`</thinking>`、`<invoke>`、stop sequence、工具调用边界被拆在多个 chunk 中，代理必须暂存少量 buffer，等下一个 chunk 才能判断是否输出、过滤或转换。`src/anthropic/stream.rs` 里有很多跨 chunk 处理逻辑和测试。

现网归因里“处理分片慢”不应直接理解为 CPU 处理一个 chunk 很慢。更多时候是“上游已经开始流式返回，但在第一个可计为首字的有效输出前，有很多非输出 event、空 event、thinking/tool 边界或长时间空闲”。本地 v0.0.70 的 trace 已能用 `firstUpstreamChunkMs`、`streamGapToFirstOutputMs`、`chunksBeforeFirstOutput`、`eventsBeforeFirstOutput` 把这些情况拆开。

## 代码差异判断

本地 v0.0.70 相比现网 v0.0.67，相关文件 diff 规模为：

- `src/anthropic/handlers.rs`: 大量新增流式 trace 和兼容处理。
- `src/anthropic/usage.rs`: 新增 latency trace 字段、错误诊断边界处理。
- `src/kiro/provider.rs`: 新增 selection failure、retry 诊断和部分软失败排除。
- `src/storage/redis_cache.rs`: 新增调度/容量独立 Redis 连接、lease tombstone、更多原子脚本。
- `src/storage/postgres.rs`: 主要是 usage 写入日志阈值变化，rollup 同步写入结构仍在。

### 本地已改善的部分

1. 流式 trace 更细。

`UsageLatencyTrace` 已包含 `payloadGuardMs`, `upstreamHeaderMs`, `firstUpstreamChunkMs`, `firstOutputDeltaMs`, `firstThinkingDeltaMs`, `firstVisibleTextDeltaMs`, `streamGapToFirstOutputMs`, `clientDroppedMs`, `terminalReason`，见 `src/anthropic/usage.rs:188` 到 `src/anthropic/usage.rs:210`。

这能把“上游响应头慢”、“body chunk 已来但没有文本”、“thinking 先来但 visible text 慢”、“客户端断开”、“上游 idle timeout”拆开，比现网 v0.0.67 更适合稳定归因。

2. 本地流式处理已记录 header、chunk、output。

- `mark_upstream_header()` 记录上游响应头到达，见 `src/anthropic/handlers.rs:1137`。
- `mark_first_upstream_chunk()` 记录首个 body chunk，见 `src/anthropic/handlers.rs:1147`。
- `mark_stream_events()` 同时识别 thinking、visible text、first output，见 `src/anthropic/handlers.rs:1169`。
- 流式 body 收到 chunk 时调用 `mark_first_upstream_chunk()`，见 `src/anthropic/handlers.rs:4986` 到 `src/anthropic/handlers.rs:4990`。
- 本地/外部池都记录类似 trace，外部池见 `src/external_pool.rs:1371`, `src/external_pool.rs:1423`, `src/external_pool.rs:2456` 到 `src/external_pool.rs:2461`。

3. Redis 调度热路径已有预算和退避。

本地 `MultiTokenManager` 有：

- `SCHEDULER_REDIS_HOT_OP_TIMEOUT = 75ms`，见 `src/kiro/token_manager/manager.rs:302`。
- `SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE = 2s`, max `30s`，见 `src/kiro/token_manager/manager.rs:303` 到 `src/kiro/token_manager/manager.rs:304`。
- 热路径并发探测上限 `SCHEDULER_REDIS_HOT_MAX_PARALLEL_OPS = 2`，见 `src/kiro/token_manager/manager.rs:305`。
- `block_on_scheduler_redis_hot()` 对 Redis future 做 75ms timeout，失败后进入 degraded，见 `src/kiro/token_manager/manager.rs:1292` 到 `src/kiro/token_manager/manager.rs:1324`。
- `RedisStore` 将普通 manager、scheduler manager、scheduler capacity manager 拆开，见 `src/storage/redis_cache.rs:143` 到 `src/storage/redis_cache.rs:150`。

这针对现网日志中的“Redis 调度热路径超过 75ms”是直接改善。预计升级本地版本后，Redis 慢导致每个请求都阻塞的概率会降低。

4. 并发 lease 竞态已有修复。

本地新增 released lease tombstone，避免 Redis release 异步慢、旧 lease 又被读回本地占用容量。代码锚点：

- 本地 tombstone TTL 和硬上限，见 `src/kiro/token_manager/concurrency.rs:18` 到 `src/kiro/token_manager/concurrency.rs:22`。
- release 时先记录 tombstone，再异步释放 Redis lease，见 `src/kiro/token_manager/concurrency.rs:123` 到 `src/kiro/token_manager/concurrency.rs:166`。
- Redis 层新增 `release_in_flight_lease_with_tombstone` 和原子脚本，见 `src/storage/redis_cache.rs` diff 中的 lease tombstone 变更。

这能改善“并发槽状态不准导致排队等待”的一类尾延迟。

5. 凭据统计写入从请求热路径转成增量缓冲。

请求选中账号时，本地先更新内存和 `pending_stats_deltas`，见 `src/kiro/token_manager/manager.rs:3794` 到 `src/kiro/token_manager/manager.rs:3812`。保存时批量写 `credential_stats` delta，见 `src/kiro/token_manager/manager.rs:3324` 到 `src/kiro/token_manager/manager.rs:3355`，Postgres 写入实现见 `src/storage/postgres.rs:1325` 到 `src/storage/postgres.rs:1384`。

这比每次请求同步更新 `credential_stats` 更好，但加载统计仍是同步 `block_on_storage`，见 `src/kiro/token_manager/manager.rs:3292` 到 `src/kiro/token_manager/manager.rs:3305`。

6. Retry 诊断更清楚。

本地 `KiroProvider` 为本地账号调度失败附带 `SelectionFailureSummary`，见 `src/kiro/provider.rs:932` 到 `src/kiro/provider.rs:950`，最后错误包装见 `src/kiro/provider.rs:3090` 到 `src/kiro/provider.rs:3094`。

Transient 错误会写入账号健康/冷却状态，429 使用 `TransientFailureKind::RateLimit`，5xx 使用 `TransientFailureKind::Server`，见 `src/kiro/provider.rs:2929` 到 `src/kiro/provider.rs:2995`。

### 本地仍未解决或仍需优化的部分

1. 真实客户端首字口径仍缺失。

当前只有 `firstTokenLatencyMs` 和 `latencyTrace.payloadGuardMs` 两个分离字段，没有一个稳定的一阶指标表示“请求进入代理到第一个有效输出”。这会继续造成 usage 面板判断偏差。

2. Usage rollup 仍在 Postgres 同事务同步执行。

`PostgresUsageStore::record_batch()` 目前流程是：

- 开启事务。
- 查旧 usage rows。
- upsert usage_records。
- 计算 rollup delta。
- 在同一事务内 apply rollups。
- commit。

代码见 `src/storage/postgres.rs:2253` 到 `src/storage/postgres.rs:2298`。rollup apply 会逐个 upsert `usage_rollup_totals`、`usage_rollup_time_buckets`、cache read、duration、credential summary 等，见 `src/storage/postgres.rs:3429` 到 `src/storage/postgres.rs:3468`。其中 `usage_rollup_totals` upsert 是热点写，见 `src/storage/postgres.rs:3584` 到 `src/storage/postgres.rs:3652`。

现网已经观察到 usage 批量写入 40s、rollup upsert 9s、pool acquire 11-13s。这说明即使本地版本有 Redis 调度保护，Postgres usage writer 仍可能通过共享连接池拖慢 scheduler 读取、external pool 读取和 admin 查询。

3. 外部池选择仍读 Postgres，但已去掉一次重复读取。

本次本地优化已经移除 `ExternalPoolManager::forward()` 为计算默认 retry 上限而提前执行的 `list_external_pools(false)`。现在默认 retry 上限来自第一次 uncached selection 返回的 `PoolAvailabilitySnapshot`。

但可用池扫描仍调用 `postgres.list_external_pools(false)`，见 `src/external_pool.rs` 的 `scan_pool_availability_uncached()`。Postgres 查询本身见 `src/storage/postgres.rs:364` 到 `src/storage/postgres.rs:383`。

现网日志里 `SELECT external_upstream_pools` 曾耗时约 9s。这个路径在本地 HEAD 仍可能导致外部备用池调度慢，尤其当 usage writer 占满同一个 pool 时。

4. 429/5xx retry 已做当前请求内排除，但仍需要真实链路复现。

本次本地优化新增 `maybe_exclude_after_transient_failure()`，在 network/protocol/429/408/5xx 等 transient failure 写入账号状态后，使用本机内存态判断是否存在其他可调度账号；如果存在，就把当前失败账号加入本次请求 `excluded_ids`。这避免了没有 conversation id 或未达到 soft failure 阈值时仍重复尝试同一账号。

剩余风险是：这只降低 retry 链反复命中同账号的概率，不能减少纯上游慢，也不能减少所有账号同时限流/冷却时的等待。需要用 fake upstream 429/500 burst 和本地多账号池复现来量化改善幅度。

5. Background best-effort task 没有统一限流。

`spawn_best_effort_storage_task()` 直接在当前 runtime `tokio::spawn`，没有队列长度、并发上限、超时、单独连接池或拒绝策略，见 `src/kiro/token_manager/storage_task.rs:28` 到 `src/kiro/token_manager/storage_task.rs:56`。

本地很多 Redis 状态更新、审计、凭据事件、lease touch/release 都走这个函数。Redis/Postgres 慢时这些任务可能堆积，并与请求路径共享 runtime 和连接池资源。

## 原因与代码责任判断

| 原因 | 是否代码引起 | 说明 |
| --- | --- | --- |
| 上游响应头慢 | 部分是代码，部分不是 | 纯 upstream header 慢不是代理代码导致；但现网约 `1,037` 条 header-dominant 慢请求存在本地 scheduler/storage overhead，且日志证明 DB/Redis 热路径卡顿 |
| body 已来但无有效输出 | 主要不是代码，但代码可改善体验和观测 | 上游可能先发 context/heartbeat/thinking/tool/input_json，代理必须等 text/thinking/tool 等可输出事件才计首字；本地 v0.0.70 已新增 first thinking/visible text trace |
| payload guard 慢 | 是代码路径和观测口径问题 | 大上下文、工具、图片、压缩/裁剪/统计会在代理内消耗 CPU；且当前不计入 `firstTokenLatencyMs` |
| retry chain | 是调度策略和上游共同导致 | 429/500 是上游/账号状态，但是否继续尝试同账号、重试多少次、冷却多久是代理策略 |
| Postgres usage 写入导致调度慢 | 是代码/架构问题 | usage writer、rollup、scheduler stats、external pool 查询共享 Postgres pool，慢写可反向拖请求路径 |
| Redis 调度热路径慢 | 现网是问题，本地已明显改善 | v0.0.70 的 75ms timeout/退避/独立连接能降低影响，但还要复现验证 |

## 改进方案

### P0: 修正真实首字观测口径

目标：usage 中直接给出客户端视角的首字，不再要求人工把 `payloadGuardMs + firstTokenLatencyMs` 相加。

建议：

- 在 request 进入 handler 时记录 `client_started_at`，早于 payload guard。
- 保留现有 `firstTokenLatencyMs` 兼容字段，但新增 `clientFirstOutputLatencyMs` 或 `requestToFirstOutputMs`。
- `latencyTrace` 增加 `requestReceivedMs = 0`, `payloadGuardEndMs`, `usageContextStartMs`, `schedulerStartMs`, `upstreamHeaderMs`, `firstUpstreamChunkMs`, `firstOutputMs`。
- UI 默认展示客户端视角；详情里继续展示 proxy 内部分段。

验收：

- 大 payload guard 场景中，`clientFirstOutputLatencyMs >= payloadGuardMs + firstOutputDeltaMs`。
- 老字段不破坏现有 API。
- usage 查询能直接筛选真实 `>10s`。

### P0: 先将现网候选升级到本地已修过的调度版本，但必须先本地复现

v0.0.70 已经覆盖现网多个热路径问题。建议先把本地 HEAD 按下文复现矩阵跑完，再决定是否灰度部署。

关键验证点：

- Redis 延迟/超时时，单请求调度额外等待应控制在 `75ms + degraded backoff` 语义内，而不是多秒。
- late Redis lease acquire/release 不应重新占用容量。
- 高并发下 selection failure 能解释“为什么没有账号可用”。
- 429/500 后账号冷却生效，恢复后能重新调度。

### P1: Postgres usage writer 与请求调度解耦

目标：usage 记录和 dashboard rollup 不再拖慢请求调度。

建议：

- 把 `usage_records` 原始写入和 rollup 写入拆成两个阶段。原始 usage 可批量 insert/upsert；rollup delta 放入内存队列或单独表，由后台 worker 合并 flush。
- `usage_rollup_totals` 这类热点 upsert 做按时间窗口/维度合并，避免每个 usage batch 都更新同一批热点行。
- 给 request scheduler/external pool 读路径和 usage writer/admin dashboard 分离 Postgres pool，至少保证 scheduler pool 不被 usage rollup 占满。
- usage writer 增加 queue depth、flush latency、dropped/deferred count 指标。队列过大时优先降级 dashboard rollup，不阻塞请求。
- `record_batch()` 事务内不要同步做所有 rollup；保留幂等补偿任务，允许后续重建 rollup。

预期效果：

- 现网日志里的 `PgSQL usage 批量写入耗时较长 elapsed_ms=40167` 不再反向造成 `sqlx::pool::acquire acquired_after_secs=11-13`。
- header-dominant 中的本地调度/storage overhead 大幅减少。

风险：

- rollup 可能短暂延迟，dashboard 实时性下降。
- 需要补偿任务保证 rollup 最终一致。

### P1: 外部池配置本地缓存

目标：外部池调度不因每次 `list_external_pools` 打 Postgres 而慢。

建议：

- `ExternalPoolManager` 维护 TTL 缓存或 Redis pub/sub invalidation 缓存。
- 管理端修改外部池时发布版本号，request path 只读内存快照。
- `enabled_count` 不需要每次从 DB 查，可从缓存快照得出。

验收：

- Postgres 注入 1-5s 查询延迟时，外部池选择不会把 TTFB 拉到秒级。
- 管理端更新外部池后，缓存能在 TTL 或事件通知内生效。

### P1: 429/5xx retry 当前请求内立即排除当前账号

目标：减少 `credential_retry_chain` 的尾延迟。

建议：

- 对 429/408/5xx，只要有 alternate usable credential，本次请求立即把当前 `ctx.id` 加入 `excluded_ids`。
- 冷却仍按 `Retry-After` 或 EWMA 规则处理，且按 model 维度记录。
- 对无 alternate 的情况，不重复长链路尝试；可以快速返回结构化 503/429，附 retry_after。
- 默认 retry 上限从“账号池覆盖一轮”改成“短链优先”，大账号池不要无上限拉长尾延迟。

验收：

- fake 429/500 混合场景中，`credentialAttempts` 不重复使用刚失败账号，p95 TTFB 不随账号池规模线性增长。

### P1: Payload guard 优化

目标：降低巨大上下文请求的代理内 CPU 时间，并让不可避免的耗时可见。

建议：

- payload guard 起止纳入统一 trace。
- 对超大 history/tool/image 先做 cheap precheck，提前拒绝或提示裁剪，而不是完整转换后才发现超限。
- 避免重复 clone/serialize 大 payload；工具 schema 压缩结果按 hash 缓存。
- 对历史消息裁剪做增量计算，避免每次全量扫描。

验收：

- 构造 1M+ token 或巨大工具 schema 场景，payload guard p95 明显下降。
- 即使仍慢，usage 能把它归为 payload guard，而不是看起来像 upstream 慢。

### P2: body 已到但无有效输出的体验和保护

目标：对 `dominant_post_chunk_no_output` 给出更清楚解释，并避免客户端长时间无感。

建议：

- 保留当前“不把非输出事件算首字”的语义，但在 trace 中记录 `eventsBeforeFirstOutput`, `chunksBeforeFirstOutput`, `terminalReason`。本地 v0.0.70 已基本具备。
- 对长时间没有有效输出但上游仍有 heartbeat/context event 的情况，可选发送标准 SSE heartbeat，改善客户端连接存活感知。
- 增加 `firstEffectiveOutputTimeoutSecs` 可配置项。仅当尚未向客户端发送任何有效输出且协议安全时，才考虑中断并重试；否则只记录 terminal reason。

风险：

- 自动重试流式请求可能造成重复工具调用或重复计费，必须非常保守。

### P2: Background task 隔离与限流

目标：Redis/Postgres 慢时，后台状态更新不会拖垮 runtime。

建议：

- `spawn_best_effort_storage_task` 改为有界队列 worker。
- 按类别隔离：Redis scheduler update、Postgres audit/stats、usage writer。
- 每类配置并发上限、超时、队列长度和丢弃/合并策略。
- 对 lease touch 这类高频事件合并，避免每个 chunk 都生成独立任务。

## 修复后复现与验证计划

原则：不对生产和日常 `9022` 服务压测。使用临时端口、fake upstream、本地 Postgres/Redis，报告写入 `target/loadtest/`。

现有工具：

- `docs/testing/loadtest.md`
- `src/bin/kiro_loadtest.rs`
- `scripts/loadtest/kiro-mock-upstream.mjs`

### L0 静态与单测

建议先跑：

```bash
cargo test --locked --no-default-features --bin kiro_loadtest
cargo test --locked --no-default-features local_latency_trace_records_markers_without_changing_first_output_semantics
cargo test --locked --no-default-features scheduler_redis
cargo test --locked --no-default-features selection_failure
```

如果本机没有 Redis 集成测试环境，Redis 集成测试按当前测试逻辑会跳过；需要在复现阶段用本地 Redis 或 docker-compose 补齐。

本次本地优化已执行的 L0 验证：

```bash
pnpm --dir admin-ui build
pnpm --dir admin-ui-daisy install --frozen-lockfile
pnpm --dir admin-ui-daisy build
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui build
cargo fmt --check
git diff --check
cargo test --locked --no-default-features external_pool_default_retry_attempts_cover_eligible_pools_and_payload_guard_retry
cargo test --locked --no-default-features cached_alternate_usable_credential
cargo test --locked --no-default-features
cargo test --locked --no-default-features -- --skip test_scheduler_state_sync_timeout_does_not_degrade_hot_path --skip response_text_with_body_timeout_expires_after_response_headers
```

结果：

- 合并远端 `origin/main` 到 `451602a` 后无冲突；恢复本地首字优化补丁无冲突。
- 三个前端构建通过，目的是补齐 RustEmbed 需要的 `admin-ui/dist`、`admin-ui-daisy/dist` 和 `ui/dist`，并确保它们与远端新合入的前端源码一致。
- `cargo fmt --check` 通过。
- `git diff --check` 通过，仅有 Git 换行符提示。
- 新增外部池 retry cap 单测通过。
- 新增 cached alternate credential 单测通过。
- 全量 `cargo test --locked --no-default-features` 编译并执行，`778 passed / 2 failed`。失败项单独复跑仍失败：
  - `kiro::token_manager::manager::tests::test_scheduler_state_sync_timeout_does_not_degrade_hot_path`：断言 `result.is_none()` 失败，测试位于未改动的 scheduler Redis timeout helper 附近。
  - `http_client::tests::response_text_with_body_timeout_expires_after_response_headers`：本地 loopback server 已写 response header 但 `send_with_response_header_timeout(..., 1)` 仍报 header timeout，测试位于未改动的 HTTP client helper。
- 跳过上述两个已复现失败项后，其余 `778` 个主二进制测试和 `11` 个 loadtest 二进制测试全部通过。
- 本次没有访问生产服务、没有对生产或 `9022` 发压测。
- `src/model/config.rs` 的本地改动是合并远端后 `cargo fmt` 对新测试断言的格式化，不涉及业务逻辑。

### L1 fake upstream 场景

直接验证 fake server 和 loadtest parser：

```bash
cargo run --bin kiro_loadtest -- \
  --fake-listen 127.0.0.1:19080 \
  --base-url http://127.0.0.1:19080 \
  --route /v1/messages \
  --requests 5 \
  --concurrency 2 \
  --scenario slow-first-byte \
  --fake-delay-ms 12000 \
  --report target/loadtest/direct-slow-first-byte.json
```

然后启动隔离代理，将本地代理 upstream 指向 fake upstream，再跑 `/cc/v1/messages` 和 `/ha/v1/messages`。报告至少包括：

- `ttfbMs.p50/p95/p99`
- `firstThinkingMs.p50/p95/p99`
- `firstTextMs.p50/p95/p99`
- `totalLatencyMs.p50/p95/p99`
- status 分布
- sample request ids / error ids
- proxy 进程 RSS/FD 起止和峰值

本次已执行 direct fake upstream 矩阵，报告位于 `target/loadtest/`：

| 场景 | 报告 | 结果摘要 |
| --- | --- | --- |
| `slow-first-byte` | `target/loadtest/direct-slow-first-byte.json` | 5/5 成功，`ttfbMs.p95=12024`, `firstTextMs.p95=12025`，证明响应头/首字整体慢可稳定复现 |
| `slow-thinking-then-text` | `target/loadtest/direct-slow-thinking-then-text.json` | 5/5 成功，`ttfbMs.p95=9`, `firstThinkingMs.p95=10`, `firstTextMs.p95=12009`，证明 thinking 很快但 visible text 慢必须单独归因 |
| `stream-idle-timeout` | `target/loadtest/direct-stream-idle-timeout.json` | 3/3 以错误结束，HTTP status 仍是 200，`ttfbMs.p95=1516`, `firstTextMs=0`，证明“上游已经开流但没有有效输出”不能只看 HTTP status |
| `rate-limit429` | `target/loadtest/direct-rate-limit429.json` | 5/5 错误，`statusCounts.429=5`, `ttfbMs.p95=5`，直接 fake server 下错误很快返回，真正慢点在代理 retry 链 |
| `server-error500` | `target/loadtest/direct-server-error500.json` | 5/5 错误，`statusCounts.500=5`, `ttfbMs.p95=6`，直接 fake server 下错误很快返回，真正慢点在代理 retry 链 |

隔离代理复现状态：

- 已尝试启动 `docker-compose.local-infra.yml` 提供本地 PgSQL/Redis。
- 当前本机 Docker/Podman 管道不可用，`docker compose` 报错：无法连接 `//./pipe/podman-plus-papay-machine`。
- 因此暂未启动临时代理 `19022`，也没有使用真实 `config.json`/真实凭据绕过该限制。
- 后续容器引擎恢复后，可用临时 config 指向 `127.0.0.1:19090` fake upstream，并用两条假 API key 凭据复现 `recovery-after-burst`，验证 500 后当前请求内是否切到备选账号。

### 场景矩阵

| 场景 | 复现目的 | 命令参数 |
| --- | --- | --- |
| slow response header | 复现 `dominant_upstream_header_wait` | `--scenario slow-first-byte --fake-delay-ms 12000` |
| thinking 先到、text 慢 | 验证 first thinking 和 visible text 拆分 | `--scenario slow-thinking-then-text --fake-delay-ms 12000 --thinking true` |
| body chunk 后无输出 | 复现 `dominant_post_chunk_no_output` | `--scenario stream-idle-timeout` |
| 429 retry chain | 验证 cooldown、排除当前账号、retry 上限 | `--scenario rate-limit429` |
| 500 retry chain | 验证 server transient 策略 | `--scenario server-error500` |
| malformed SSE | 验证 terminal reason 和错误归一化 | `--scenario malformed-sse` |
| client drop | 验证 lease/FD/RSS 清理 | `--scenario client-drop` |
| huge payload guard | 验证 `payloadGuardMs` 与真实首字 | 扩展 loadtest payload 或构造大 tools/history |
| Redis slow | 验证 75ms timeout/degraded | 本地 Redis proxy 注入延迟或临时阻断 Redis |
| Postgres pool contention | 验证 usage writer 解耦效果 | 本地 Postgres 限小 pool，同时制造 usage rollup 压力 |

### L2 修复效果判定

修复后的目标不是让所有慢首字消失，而是让每类慢都有稳定解释，并消除代码可控的尾延迟：

- `payloadGuardMs + firstOutputDeltaMs` 与新增客户端首字指标一致。
- Redis slow 注入时，单请求额外 scheduler 等待不进入多秒级。
- Postgres usage 写慢时，请求调度不出现 `sqlx::pool::acquire` 多秒等待。
- 429/500 burst 后，后续 normal traffic 能恢复，retry chain p95 不超过配置预算。
- `dominant_post_chunk_no_output` 能被 trace 标记为 upstream chunk gap/terminal reason，而不是误判为 DB/Redis。

## 建议实施顺序

1. 先跑本地 v0.0.70 的 fake upstream 复现矩阵，确认本地已有修复对 Redis 调度和 trace 的实际效果。
2. 做 P0 观测口径修正，新增客户端真实首字字段，避免后续优化无法稳定衡量。
3. 做 Postgres usage writer 解耦和 external pool 配置缓存，解决现网日志中最明确的本地资源竞争。
4. 已完成 429/5xx 等 transient retry 的当前请求内排除；下一步用 fake upstream 429/500 burst 量化 retry chain p95 改善，并继续评估 retry 上限收敛。
5. 做 payload guard 性能优化和巨大 payload 可控降级。
6. 最后做 heartbeat/first effective output timeout 等体验项。

## 当前稳定判断

- 现网慢首字中，纯上游慢占多数，但不是全部。
- 代码/架构可改进的重点不是盲目改 stream parser，而是先修观测口径、DB/Redis 热路径隔离、retry 链路和 payload guard。
- 本地 v0.0.70 已经修复或改善了现网 v0.0.67 的一部分调度问题，尤其 Redis 热路径和并发 lease 竞态；本次又减少了外部池一次重复 PgSQL 查询，并让 transient retry 在有备选账号时临时排除当前失败账号；但 usage rollup、外部池配置缓存、真实客户端首字指标仍需要继续改。
- 任何修复后都应先用本地 fake upstream 和隔离代理复现，再考虑灰度到生产；不要在生产机器上做负载验证。
