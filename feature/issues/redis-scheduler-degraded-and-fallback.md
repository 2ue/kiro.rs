# Redis Scheduler Degraded And Fallback

Status: `handler-fallback + r8-L3-L5 + non-Docker single-instance chaos and simultaneous single-instance fault pass + external-takeover-runner-contract-pass / remaining-external-takeover-dynamic-two-instance-native-cli-ui-upgrade`

Severity: P0

## 用户可见现象

账号并发/RPM 有余量且 external pool 可用，仍快速返回 `No account is ready... Retry after 1 second`。给定生产请求样本的 usage 显示 `routeSubtype=local_error_no_fallback`、`selectionFailure.stage=dispatch_queue`、`globalInFlight=39`、`durationMs=3`，说明请求没有打到模型，而是在本地协调阶段失败。

## 已确认演化

- 75 ms hot-path timeout 首次可追溯到 `09033fd4`，已包含在 v0.0.57；它不是 v0.0.108 才出现的新常量。
- 跨实例 fail-closed 在 v0.0.102 的 `835ab49d` 引入；Redis acquire/queue 失败不再 local-memory fail-open。
- v0.0.108 将 `SchedulerRedisDegraded` 从普通 capacity fallback 拆成独立开关，默认 false，且旧配置没有迁移；因此已有 capacity fallback 配置升级后可变成 `local_error_no_fallback`。
- sticky read/write/delete/soft-failure 等非容量 Redis 操作共用进程级 breaker，可让整池进入 2/4/8/16/30 秒 fail-closed backoff。
- usage 使用独立 connection 但仍共享 Redis 单线程；当前高基数读取已有 guard，旧 18.8 万参数 HMGET 只能作为历史证据，不能宣称当前仍持续发生；同步 clear 的 `SCAN + DEL` 风险仍在。
- external pool 同样依赖共享 Redis，部分 Redis 调用没有统一 bounded timeout；in-flight lease release 的异常路径会同步等待 Redis 最多约 2 秒。

## 根因

直接生产失败由两个条件叠加：scheduler 协调 Redis 操作超过 75 ms 后进入 `SchedulerRedisDegraded`，而升级后的独立 fallback 开关默认/持久化为 false，导致 `local_error_no_fallback`。后续复现又确认 fresh-state guard 将 degraded 时的本地内存 `dispatchable` 估计误当成可取得 Redis 分布式 lease 的证据，即使开关 true 也会压制 external。共享 breaker、usage Redis 单线程竞争和 external coordinator 的多次串行 Redis 往返进一步放大失败窗口与 TTFB。

## 当前本地复核证据

- `redis_backed_` 聚焦组：3 轮，每轮 4/4 通过。
- `redis_dispatch_queue_` 聚焦组：3 轮，每轮 6/6 通过。
- scheduler fallback toggle：3 轮，每轮 2/2 通过。
- high-cardinality guard：3 轮，每轮 1/1 通过。
- fail-closed 与 queue-degraded 单项：各 3 轮；backoff 单项 1 轮。
- 隔离 Redis blocking Lua 令另一连接的 PING 分别约 100/150/280 ms，均可越过 75 ms hot-path budget。

这些结果证明当前 fail-closed 语义和共享 Redis 共因确实存在，不证明生产可用性已经达标。隔离 Redis 已清理，46379 无残留监听。

2026-07-16 又完成真实 handler + Toxiproxy 红绿：150 ms Redis 延迟令 route state 成为 `SchedulerRedisDegraded`，但仍带本地内存估计 `dispatchable=1`。修复前 fresh-state guard 因该计数错误压制 external，真实返回 429、local/external 均 0 hit；修复后 3 轮 x 5 请求全部 external 1 hit/HTTP 200，移除延迟后 3/3 先恢复 local 路由。详见 [strict local-first 与 scheduler chaos 证据](../evidence/strict-local-first-and-scheduler-chaos-20260716.md)。

2026-07-18 追加 development focused rerun：`external_fallback_classifier_respects_scheduler_fallback_toggles` 与 `local_pool_preflight_reason_respects_scheduler_fallback_toggles` 均 `running 1 / passed 1`。同一 scoped batch 还复测 runtime backlog 不假禁用、finite queue lease、deadline freeze 和 40x15/global-500，六个精确 filter 全部通过；scope `scheduler-focused-rerun-20260718-dev6g` 清理 `1699916 KiB` 后 `removed=true / reservation_released=true`，后续根 target/flycheck 可再生产物也已删除为 `target=0 KiB`。该结果仍是 development/focused 证据，不替代真实 Redis/PG 动态或冻结负载。

2026-07-19 冻结 fake-upstream 负载复核补齐了 L3/L4/L5：历史 frozen product `e16df13a...` 在 L3 burst/recovery 9/9、L4 restart/failure/client-drop/mixed-chaos 12/12 通过，但 L5 900 秒 long-stream soak 红灯，`l5_long_stream_soak_900s_c20` 为 `5949` success / `94051` 429，post-soak recovery 仍有 `2/12` 429；300 秒诊断同样红，`2203` success / `12361` 429。raw proxy log 证明根因是 capacity hot-path breaker：`占用 Redis 凭据并发槽超过共享总期限 75ms`，随后 `Redis 调度协调状态不可用` 约 24722 次。该证据见 [frozen L3-L5 evidence](../evidence/frozen-loadtest-l3-l5-chaos-20260719.md)。这说明 breaker/fallback focused pass 不能关闭发布门禁；必须修复 sustained long-stream 下的 scheduler hot-path 可用性。

同日 r6 candidate `d75c102...` 追加了普通本地请求在 `SchedulerRedisUnavailableError` 下的有界本地等待。`WaitForCapacity` / `WaitForCapacityMax` 不再在 Redis capacity breaker/open/local scheduler overload 时立即形成 429 storm；`FailFastOnCapacity` 仍快速返回 degraded，以保留 external fallback preflight 合同。r6 的 120 秒 run 请求层 921/921 但短 idle RSS gate 未过；第一轮 300 秒把 429 从历史 12,361 个压到 158 个；第二轮 300 秒 keep-raw 为 2281/2281、0 个 429、TTFB p95 361 ms、RSS/FD gate 通过，raw log 中 capacity/snapshot/affinity breaker `failures=0`。

r6 随后的 900 秒 rerun 在约 4.5 分钟时提前复现红项并被停止：PgSQL usage batch 慢写与 `usage_rollup_totals` 4.97s slow SQL 后，Redis affinity/capacity 分别超过 75 ms，出现 46 次 `Redis 调度协调状态不可用`、23 次返回 429，capacity breaker recovery 记录 `suppressed_requests=901086`。这说明 r6 的有界等待仍会被普通 capacity signal 频繁唤醒，导致 breaker-open 窗口内形成内部重试风暴，符合用户描述的“下游 RPM 不高但系统内部 RPM 很高”。

r7 candidate `58f465...` 将 Redis degraded 分支改为专用 sleep，不再监听普通 capacity signal；真实 Redis focused test 加入 1 ms noisy notifier 后仍 1/1 通过，并断言 `suppressed` 有界。r7 的 300 秒 keep-raw 为 2121/2121、0 个 429、TTFB p95 961 ms、RSS/FD gate 通过；raw 中 Redis degraded、capacity timeout、queue timeout、returned 429、suppressed 均为 0，capacity/snapshot/affinity breaker failures 均为 0。PgSQL usage 慢写仍出现 4 次、最高 1688 ms，但未传播成 scheduler breaker。r7 900 秒 rerun 又在 2-3 分钟内复现 22 次 Redis degraded、11 次 429 和 6 次 capacity slot timeout，说明“去 spin”不够，单次 capacity 75ms timeout 仍会打开 breaker。

r8 candidate `131696bd...` 将 capacity/queue Redis budget 调整为 250ms，affinity/sticky 保持独立 75ms，并要求 capacity timeout 连续达到阈值才打开 capacity breaker；成功会重置 timeout streak。`cargo fmt --check && cargo check -q --all-targets` 通过，`scheduler_redis_` 聚焦 5/5、`scheduler_redis_capacity_` 聚焦 2/2、`redis_backed_in_flight_limit_does_not_fail_open_while_degraded` 1/1 通过，scoped targets 均已清理。Toxiproxy 延迟注入在本机未配置，因此只算编译/skip，不计动态通过。

r8 frozen L5 300 秒为 2281/2281、0 个 429、TTFB p95 388 ms、RSS/FD gate 通过；r8 frozen L5 900 秒为 6821/6821、0 个 429、TTFB p95 354 ms、RSS/FD gate 通过。900 秒 raw 中 `Redis 调度协调状态不可用=0`、capacity breaker `admitted=6833 failures=0 suppressed=0`、snapshot `failures=0`；affinity breaker 有 2 次 75ms sticky/session cleanup failure 并自愈，未影响容量准入。随后 r8 L3 burst/recovery 9/9 与 r8 L4 restart/failure/client-drop/mixed-chaos 12/12 通过。完整证据见 [frozen L3-L5 evidence](../evidence/frozen-loadtest-l3-l5-chaos-20260719.md)。

2026-07-20 使用当前项目 Redis 空 DB15 和非 Docker loopback chaos proxy 补齐单实例动态注入：7 exact tests x 3 outer，21/21 通过。50ms capacity 正常通过；500ms 在约 250ms hot deadline fail closed，连续达到阈值才打开 breaker；affinity 500ms 不污染 capacity；disconnect/reconnect、300 lease 异步 release、cancel/commit-unknown 均恢复且无残留，任何故障均不报告 `AllDisabled`。两个 smoke 红项确认是旧测试等待合同未跟随指数 backoff/ConnectionManager transport shape，最终 oracle 保留指数退避与阈值而非放宽产品。

2026-07-21 追加完整 joint-fault rerun 时，`scheduler-redis-joint-chaos-20260721-r5` 在 WRONGTYPE recovery 真实失败：outer round 2 的 `wrongtype-round-2` 首次 recovery 仍返回 `Redis 调度协调状态不可用，retry_after_secs=4`。根因是 hot-path Redis 调用开始后所有失败都按 commit-unknown 处理；对 timeout/连接中断这是保守必要的，但对确定性 Redis response/type/script/server 错误不成立，因为这类错误没有创建 lease。错误分类导致 release/tombstone reconciliation 被无谓入队，增加异常期 Redis 写并干扰 breaker half-open recovery。修复后 `SchedulerRedisExecutionOutcome::Failed` 与 `SchedulerRedisHotOutcome::Failed` 都携带 `commit_unknown`，确定性 response 错误走 `confirm_redis_not_acquired()`，不再释放不存在的远端 lease。`scheduler-redis-joint-chaos-20260721-r6` 使用空 DB5 通过 8 exact × 3 outer（24/24），并再次覆盖 usage-writer/scheduler 同窗 latency、500ms、WRONGTYPE、disconnect 和 5/5 recovery。详见 [非 Docker chaos evidence](../evidence/scheduler-redis-chaos-nondocker-20260720.md)。

这关闭单实例 usage 与 scheduler 同时故障子项。两实例 fault/fallback、external takeover 动态服务验证、真实上游/CLI 其他能力、UI/browser、upgrade 和 final inventory 仍 open。

2026-07-21 追加 external takeover 代码路径与 runner 合同复核：

- 源码复核确认 `local_pool_route_fallback_reason` 只有在 `fallbackOnSchedulerRedisDegraded=true` 时返回 `local_scheduler_redis_degraded`；`local_pool_fallback_reason_for_fresh_state` 对 `SchedulerRedisDegraded` 特判，不再让本地内存 `dispatchable > 0` 估计压制 fallback；`fallback_after_local_error_outcome_with_diagnostics` 会重新读取 fresh local state 并检查 external pool eligibility。
- `external-takeover-focused-20260721-r2` 跑过四个 exact tests：`external_fallback_classifier_respects_scheduler_fallback_toggles`、`local_pool_preflight_reason_respects_scheduler_fallback_toggles`、`fresh_local_pool_state_blocks_external_while_any_local_account_is_dispatchable`、`all_parsed_external_fallback_entrypoints_share_model_and_body_mode_eligibility`，全部通过；scoped target `1708372 KiB removed=true reservation_released=true`。
- 新增 `feature/tests/external-takeover-scheduler-degraded-nondocker.mjs` 与 contract test。contract 8/8 通过，证明 runner 不调用 Docker/Cargo、不探测既有 `9022`、拒绝 DB0/非 loopback/不安全 database/prefix，并只接受仓库外冻结 binary 与 owned artifact root。
- 2026-07-22 该动态 runner 已使用仓库外冻结 r12 binary `eca8ce4eb1ebb4c1657d1894dc69d0624313b6ff28e0cba095bf845c0914d13e` 执行产品服务路径。enabled 三个 clean-DB 轮次均为 5/5 degraded HTTP 200 external takeover + 5/5 recovery local 200；disabled 一个 clean-DB 轮次为 5/5 degraded HTTP 429、local/external hits 均 0、公开错误脱敏，移除延迟后 5/5 recovery local 200。runner 未用 Docker/Cargo，未探测或触碰 `9022`，Redis owned prefix 最终 remaining 0。详见 [external takeover evidence](../evidence/external-takeover-scheduler-degraded-20260721.md)。

## 本地复现

隔离 Redis 中用 bounded blocking Lua 施加 50/75/100/150/300 ms 延迟，分别触发 sticky、lease、queue、binding 操作；并行发送 local/external 矩阵请求，记录 breaker、route state、fallback、attempt、Redis op latency 和恢复时间。每档 3 轮，两实例另跑 lease/recovery。

## 已实现：兼容迁移与有界 fallback

2026-07-16 当前工作树已实现第一批 P0 修复：

- `fallbackOnSchedulerRedisDegraded` 的 serde 缺省值和新配置默认值改为 `true`，直接从 v0.0.107 或更早版本升级时恢复原先由 capacity fallback 隐式覆盖 Redis degraded 的行为。
- runtime config migration marker 从 4 升到 5。旧 marker 配置同时满足 external pool 已启用、capacity/no-credential/transient 三类 fallback 全部启用且 scheduler flag 为 false 时，一次性迁移为 true。这覆盖 v0.0.108/v0.0.109 已把缺省 false 写回 PgSQL 的升级链。
- v5 迁移后管理员再次显式设置 false 不会被重写。旧 marker 中只启用部分 fallback、external pool 关闭或显式 true 的配置不会被错误扩大。
- external capacity `wait` 模式不再把 `externalPoolDispatchMaxWaitSecs=0` 解释为无限等待；旧值迁移为 30 秒，运行时即使再次收到 0 也按 30 秒有效上限执行。
- 两套 UI 的缺省值同步为 true，并在保存时把最大等待限制为至少 1 秒；external pool 总开关关闭时仍不会构造 fallback context 或发起外部请求。

行为合同如下：

| 输入状态 | v5 后 scheduler degraded 行为 |
| --- | --- |
| 字段缺失 | 使用兼容默认 true；仅在 external pool 启用且有 eligible pool 时 fallback |
| 旧 marker、external enabled、三类旧 fallback 全为 true、scheduler=false | 一次性迁移为 true |
| 旧 marker、external disabled | 不扩大策略，不路由外部池 |
| 旧 marker、任一旧 fallback=false | 保留 scheduler=false，不扩大管理员的窄策略 |
| 当前 marker、显式 false | 永久保留 false，直接走原本的本地规范错误 |
| 显式 true | 保留 true |
| external pool 无 eligible pool | 有界返回最终错误，不回到本地形成循环 |
| external capacity wait=0 | 运行时按 30 秒截止，不无限排队 |

迁移取舍：v0.0.108/v0.0.109 已将“旧字段缺失”与“管理员显式 false”都持久化为相同的 false，历史来源无法再从 JSON 无损区分。v5 只对具有完整广泛 fallback 意图的旧 marker 做一次恢复；极少数在旧版本主动关闭 scheduler fallback、但同时保留其余三类 fallback 全开的配置会被迁移为 true。回滚方式是在升级完成后通过任一管理 UI 将“调度 Redis 降级时使用外部账号”关闭；marker 已为 5，后续启动不会再次改写。

本批证据见 [`../evidence/redis-scheduler-fallback-v5-20260716.md`](../evidence/redis-scheduler-fallback-v5-20260716.md)。

## 修复方向

2026-07-17 源码修复进一步收敛了 scheduler coordination 合同，但本批按要求没有运行 Cargo 或 Redis chaos，不能据此关闭验收项：

- capacity/affinity breaker 使用显式 `Closed / Open / HalfOpen` 与 failure generation fencing；旧 generation 的 success 不能清除更新 failure，退避到期到唯一 probe 成功前 route 仍为 degraded。
- semaphore 等待与 Redis 操作共享 75ms 总 deadline；本地 permit 饱和分类为 `LocalSchedulerOverloaded / NotStarted`，只计本地饱和指标，不打开 Redis breaker，也不登记 commit-unknown tombstone。
- in-flight/dispatch-queue acquire 明确区分 `NotStarted / CommitUnknown / Definitive`。只有 Redis 调用真正开始后才 arm commit-unknown；明确拒绝不释放远端 lease，确认成功使用普通 release。
- release 使用 manager-owned、65,536 intent 硬上限、单 worker、每批最多 64 个 Redis 操作的独立 reconciliation lane；Drop 不再同步等待 Redis，也不再回退到共享 storage task queue。重试退避和 breaker 退避都使用稳定、上界内 jitter，并保留饱和、failure、probe、stale-success、pending/retry 指标。
- r6 进一步把普通本地请求的 degraded capacity acquire 从 quick-fail 改为本进程有界等待；等待期间不创建 Redis queue admission 占位，避免 Redis 已 degraded 时继续放大 Redis queue 操作。
- r7 将上述 degraded wait 从 capacity-signal wait 改为专用 recovery sleep，避免 stream release、usage writer 或本地 capacity 变化在 Redis breaker open 时把请求循环提前唤醒成内部 retry storm。
- r8 将 capacity/queue Redis budget 与 affinity budget 拆开：capacity/queue 为 250ms 且连续 timeout 才打开 breaker；affinity/sticky 保持 75ms 且只影响 sticky 缓存，不冻结本地账号池。
- r9 当前 dirty-tree 进一步区分 hot-path Redis failure 的 commit-unknown 语义：确定性 Redis response/type/script/server 错误不创建远端 lease，不入队 release/tombstone；timeout、连接丢失、I/O 和未知非 Redis 错误仍保守处理为 commit-unknown。这直接降低 WRONGTYPE/脚本错误/类型污染下的内部 Redis 写放大和恢复竞争。

对应源码测试包含 c255/c256/c257/c512 本地 admission 边界、10,000 次 stale-success fencing 等价交错、Open/HalfOpen 单 probe、稳定 backoff 边界，以及 NotStarted/CommitUnknown/Definitive cleanup 分类。它们尚未执行，最终仍需按下方故障矩阵在冻结候选上验证。

- [x] 对旧 external+capacity fallback 配置做显式一次性迁移与兼容默认。
- [x] 将 external capacity wait 的 0 秒旧语义改成运行时 30 秒安全上限。
- 按 capacity/queue、sticky affinity、state-sync 拆 breaker；sticky 失败降级为无 sticky，不能冻结整个账号池。
- degraded fallback 前后都重查 route state，并避免 local/external 循环。
- [x] degraded fallback 使用 fresh route state；普通 Ready/dispatchable 保持 strict local-first，但 `SchedulerRedisDegraded` 不再把本地内存 `dispatchable` 误当成可取得分布式 lease。
- 所有 external Redis 操作设置 bounded timeout；lease release 改为异步幂等 reconciliation，不在请求完成路径同步等待 2 秒。
- scheduler 热路径和 usage/cleanup 工作负载最终做 Redis 实例隔离，并使用 `UNLINK`/小批清理；先测再决定是否调整 75 ms。
- 用结构化 failure enum 代替错误文本分类，并迁移 v0.0.107 的 fallback 语义。

## 验收、回滚与残余风险

E03-E05、F03。故障注入覆盖 50/74/75/90/150/500 ms、connection reset、restart 和 commit-unknown。Redis 故障期路由必须符合配置，公开错误不泄漏 Redis/credential/pool 内情；恢复不超过 60 秒且 5/5 normal，双实例无超卖/残留，usage cleanup 压力不产生 scheduler degraded。

性能门槛：proxy p95 <= 25 ms、p99 <= 75 ms；Redis script p95 <= 5 ms、p99 <= 10 ms；相对基线吞吐/p95/p99 回退分别不超过 5%/10%/15%。

当前 150 ms Redis degraded handler fixture 虽 15/15 可用，但 TTFB p50 `1507.82 ms`、p95 `1991.59 ms`。这是明确未关闭的异常性能项：external pool 的 runtime snapshot/cooldown/lease 协调仍串行依赖同一慢 Redis。必须在 burst/soak 下证明 admission、队列和资源有界，并评估合并 Redis round trip 或独立 coordinator；不能只凭状态 200 关闭本专题。

回滚可以由管理员显式关闭 degraded fallback，但不得恢复 Redis 故障时 local-memory fail-open 或无限 external wait。剩余风险包括 external 原子热路径尚在最终验证、breaker 作用域仍需故障矩阵、usage cleanup 共因和双实例 commit-unknown；这些都必须由 E03-E05/F03/L3-L5 关闭。

## 2026-07-22 当前工作树复核

本轮重新执行了两组真实 Redis 动态门：

- `scheduler-redis-chaos-20260722-r1`：空 Redis DB7，3 outer × 8 exact，24/24 通过。覆盖 50/500ms latency、capacity 连续 timeout 后 breaker、affinity 500ms 不污染 capacity、disconnect/reconnect、WRONGTYPE/commit-unknown、300 lease release 和 usage-writer/scheduler joint fault。结果 `databaseEmpty=true`，scoped target `removed=true / reservation_released=true`。
- `redis-fault-domain-product-20260722-r1`：business Redis `<loopback>:26379/db8`、observability Redis `<loopback>:50892/db2`，3 outer × 1 exact × 内部 3 轮通过。结果 `dockerUsed=false`、`flushDbUsed=false`、`protected9022ProbeSkipped=true`，证明 usage/observability 故障域不会把 scheduler 拉入 degraded 或 `AllDisabled`。

这把“单实例 Redis chaos + usage 同窗故障 + business/observability Redis 分离 + SchedulerRedisDegraded external takeover 正/负向动态路径”的当前工作树证据更新到 2026-07-22。剩余发布门仍包括两实例组合故障、真实上游/全能力 CLI、UI、upgrade 和最终 inventory。完整证据见 [final-regression-rerun-20260722](../evidence/final-regression-rerun-20260722.md) 与 [external takeover evidence](../evidence/external-takeover-scheduler-degraded-20260721.md)。

## 2026-07-23 最终候选复核

v0.0.117 冻结候选通过 load/chaos L3/L4：突发错误、429/500、invalid tool、client-drop、proxy restart 后恢复流量均 12/12 成功；未出现“账号全部禁用”或调度退化后无法恢复的表现。L3/L4 使用 fake upstream 和独立 PG/Redis，不是生产 9022 或 Docker 动态验证。SchedulerRedisDegraded external takeover 的正/负向动态证据仍以 2026-07-22 的专门 runner 为准；最终门禁补充证明该修复未被后续改动破坏。详见 [最终发布门禁证据](../evidence/final-release-gate-20260723.md)。
