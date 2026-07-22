# Redis Usage Writer 原子性、基数与 Scheduler 隔离

Status: `implementation-focused-pass / isolated-redis-dynamic-and-single-instance-joint-fault-pass / multi-instance-and-production-cardinality-pending / NO-GO`

Severity: `P1 correctness + P1 availability`

Last updated: 2026-07-20

## 问题范围

本专题只处理 Redis usage detail/derived summary 写入对 scheduler 热路径的干扰，以及写入在超时、断连、WRONGTYPE 和高基数下的正确性。PostgreSQL usage writer、soft/hard cleanup 产品合同和 Dashboard P95 分别由其他专题负责。

生产现象中，usage summary 的高基数 `HMGET/HGETALL`、大批清理和写入压力与 scheduler 75 ms 热路径超时同窗出现。它不证明每次 scheduler degraded 都由 usage 引起，但当前实现确实让 usage 与 scheduler 共享 Redis 单线程压力面，因此必须同时验证数据正确性和调度延迟，不能只测 usage 最终结果。

## 已确认缺陷

### 1. 写 gate 进入太晚

旧流程对一个最多 64 条的 Redis usage batch 使用 `join_all`。每条记录在取得默认单 permit gate 之前，已经并发发送 snapshot EVAL 和独立 `SET NX seen`；因此一批请求最多先制造 128 个并发 Redis 往返，再在 aggregate 前排队。usage writer 是异步的并不意味着它不会阻塞同一 Redis 上的 scheduler。

### 2. seen marker 早于 aggregate 成功

旧流程先独立写 `seen`，再提交 totals/dashboard/top/realtime aggregate。seen 成功而 aggregate 失败时，同一 request ID 在一小时内会被当作已经处理，导致 derived totals 永久少记，而且没有可靠的失败标记要求读路径回退 PostgreSQL。

### 3. 命令级错误留下部分 aggregate

aggregate 包含多个 hash/zset/expiry 命令。Redis 脚本不会自动回滚 `pcall` 前已经成功的写入。如果后续命令遇到 WRONGTYPE，只返回错误但继续允许读取 derived cache，会得到内部不一致的 totals、Dashboard 和 top breakdown。

### 4. 两 RTT 中间修复仍有取消窗口

第一次修复把 gate 前移，并把 seen 放到 aggregate Lua 的最后，使 accepted path 从 3 RTT 降为 2 RTT。但外层 2 秒 timeout 从等待 semaphore 时就开始；64 个 future 同时等待时，timeout 可以发生在 snapshot 已成功、aggregate 尚未确认之间。该中间版本减少了 Redis fanout，却仍可能留下没有 invalidation 的 detail/summary 缺口，因此没有作为最终修复保留。

### 5. cache-read exact-token 字段无硬上限

全局和逐小时 Dashboard 使用精确 `cache_read_input_tokens` 作为 hash field。生产证据曾出现约 18.8 万参数的 cache-read summary 命令。无限增长会增加内存、序列化、Redis 单线程执行时间和 scheduler 75 ms 超时概率。

### 6. batch fanout 与 timeout 语义不一致

默认 gate 只有一个 permit，但旧 batch 同时创建 64 个 future 和 64 个 timer。它没有增加实际 aggregate 并发，却增加等待对象，并让后排记录的 timeout 在真正开始操作前已经消耗。高延迟时大量 future 会同一时刻超时并形成日志/计数突发。

## 根因

根因不是单独的 `SET NX`、某个 Hash 指纹或某次 Redis 慢查询，而是 usage 写入缺少一个覆盖 detail、derived aggregate 和幂等 marker 的提交所有权边界。网络阶段被拆成多个可独立超时的 RTT，批次并发模型又与单 permit gate 相互矛盾；同时 exact-token 维度没有基数预算。三个设计缺口叠加后，正常流量会制造不必要的 Redis fanout，异常流量则可能留下半提交状态，并与 scheduler 共用 Redis 单线程形成可用性共因。

## 当前选定修复

### 单 Lua 提交单元

`record_usage_summary` 现在在取得 gate 后构造一个 `GUARDED_IDEMPOTENT_USAGE_PIPELINE_SCRIPT`，同一个 EVAL 内按以下顺序执行：

1. 读取并推进 cleanup watermark；旧记录直接拒绝。
2. 检查 derived-cache invalidation 和 request-ID seen marker。
3. 在任何写入前检查全局与当前小时 cache-read hash 的类型和字段上限。
4. 写 detail snapshot、records index、TTL，并执行有界过期/overflow trim。
5. 写 totals、realtime、Dashboard、top dimensions 和 external billing aggregate。
6. 所有命令成功后，最后写 seen marker。

accepted path 由 3 RTT 降为 1 RTT。客户端 timeout 或断连不能把 Redis 脚本拆成 snapshot/aggregate 两个可取消阶段；Redis 端要么未收到 EVAL，要么原子执行该脚本。命令级 `pcall` 错误仍可能发生在部分写入之后，但脚本会设置 `usage:cleanup:derived_cache_invalidated`，所有 summary/dashboard/detail cache 读路径必须回退 PostgreSQL，且 seen 不会写入。

### 有界 cache-read 基数

全局和当前小时 cache-read hash 的默认硬上限均为 4096 个字段。新 bucket 在任一 hash 已达上限时不再写入，立即 invalidation 并回退 PostgreSQL。已有 bucket 仍可增加计数，不会因达到上限而丢失已存在维度。读路径在发现历史 bucket 超过上限时也拒绝展开。

### 单批共享 deadline

Redis batch 仍最多 64 条、总 deadline 仍为 2 秒，但不再用 `join_all` 同时创建 64 个等待 future。记录按顺序进入操作，共享同一 batch deadline；deadline 用尽后，剩余记录直接记为未开始超时。由于 RedisStore gate 本来就是单 permit，这不会降低该流程原有的有效 Redis 并发，却移除了等待者 fanout。

## 复现与验证程序

### R1：命令级部分失败

在随机 Redis namespace 中把某个 late-stage realtime key 预置为 string，触发 `HINCRBY` WRONGTYPE。连续 5 轮断言：

- `record_usage_summary` 返回错误；
- invalidated key 存在；
- seen key 不存在；
- 至少一个 earlier aggregate 已写，证明失败位置确实在中途；
- Redis summary 返回 `None`，不能读取半成品；
- 清理 invalidated cache 后重试，同一 ID 最终恰好计数一次。

程序：`redis_usage_summary_partial_command_error_never_sets_seen_for_five_rounds`。

### R2：cache-read 基数上限

使用测试上限 8，连续 5 轮写入 8 个唯一 exact-token bucket，再写第 9 个。断言全局和小时 hash 均保持 8 个字段、invalidated 存在、summary 回退 PostgreSQL。

程序：`redis_usage_summary_cache_read_cardinality_is_hard_capped_for_five_rounds`。

非 Docker runner：`feature/tests/run-redis-usage-writer-validation.sh`。它必须显式接收隔离 Redis URL 和 `KIRO_RS_TEST_REDIS_ISOLATED=1`；默认执行 3 个 outer rounds，缺环境时在 Cargo 前 fail closed。

### R3：无 Redis 的提交顺序合同

连续 5 轮静态断言 cardinality guard 位于所有写入前，snapshot 位于 aggregate 前，seen 是唯一且最后的 commit marker，seen 前错误路径包含 invalidation。

程序：`guarded_usage_script_commits_snapshot_aggregate_and_seen_in_order_for_five_rounds`。

### R4：batch 无 waiter fanout

连续 5 轮对 8 个延迟操作记录最大同时执行数，必须严格为 1；再用 pending future 证明共享 20 ms deadline 后全部有限返回错误。

程序：`bounded_usage_batch_uses_one_shared_deadline_without_waiter_fanout`。

### R5：scheduler 联合压力

在隔离非 Docker Redis 上同时执行 64-record usage batch 与 scheduler reserve/release/session-binding。分别注入 0/25/50/74/75/90/150/500 ms 延迟、WRONGTYPE、连接中断和恢复，每格至少 5 轮，三次 soak。记录 usage throughput/drop、scheduler p50/p95/p99、degraded 次数、恢复时间、RSS 和 FD。

## 当前证据

- `usage-summary-atomic-c0-r2`：`cargo fmt --all + git diff --check + cargo check --all-targets` 通过；scope `447380 KiB`，`removed=true / reservation_released=true`。
- `usage-summary-atomic-c0-r3`：全目标 check 通过；R3、R4 各 `running 1 test / 1 passed`，测试内部各 5 轮；scope `2016696 KiB`，`removed=true / reservation_released=true`。
- R1、R2 已在当前仓库专属隔离 Redis 中完成两批动态验证。2026-07-18 `redis-usage-writer-real-20260718-rerun` 为 3 outer rounds × 2 tests；2026-07-19 `redis-usage-writer-real-20260719` 再次为 3 outer rounds × 2 tests。两个 exact filters 均为五轮程序，覆盖 cache-read cardinality hard cap 和 partial command error never sets seen。最新 scope `1691768 KiB removed=true reservation_released=true`，日志哈希见 [2026-07-19 storage evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。
- R5 已纳入 `run-scheduler-redis-chaos-validation.mjs`。2026-07-20 `scheduler-redis-joint-fault-r4` 为 8 tests × 3 outer，即 24/24 exact invocation；联合故障测试本身每次再跑 3 internal rounds，所以 `25/50/74/75/90/150/500ms`、WRONGTYPE、disconnect/recovery 各有 9 轮。2026-07-21 重跑 `scheduler-redis-joint-chaos-20260721-r5` 暴露真实红项：WRONGTYPE 是确定性 Redis response/type 错误，本不应按 commit-unknown 释放远端 lease；旧分类无谓入队 release/tombstone，干扰后续 5/5 recovery。修复 failure `commit_unknown` 分类后，`scheduler-redis-joint-chaos-20260721-r6` 再次 8 tests × 3 outer，即 24/24 exact invocation。低延迟场景 usage 16/16 且严格 1 write = 1 Redis RTT；500ms 在精确 3 次 Redis failure 后打开 breaker，后续 128 次全部本地 fail-fast、无 Redis counter 增长；WRONGTYPE 不误报 `AllDisabled` 且恢复 5/5；disconnect 在 writer RTT 已在途后注入且无内部 retry；每类硬故障移除后连续 5/5 恢复。RSS 增量约 10--12 MiB，FD +4，最新 scope `1710316 KiB removed=true reservation_released=true`。详见 [联合故障证据](../evidence/scheduler-redis-chaos-nondocker-20260720.md)。
- `usage-summary-atomic-c0-r1` 被工具层 1 秒超时强杀，只产生 16 KiB stale scope 和空 reservation temp；两者经 ownership 核对后定点回收。该轮不计编译或行为证据。

详细命令和证据边界见 [2026-07-18 evidence](../evidence/redis-usage-writer-atomicity-20260718.md)。

## 发布验收

- R1、R2 在明确隔离的非 Docker Redis 中各至少 5 轮通过；缺环境时必须 fail-closed。
- accepted、duplicate、watermark reject、invalidation、WRONGTYPE 和 cardinality overflow 均严格为 1 RTT。
- 64-record batch 不产生超过 1 个本进程 usage Redis in-flight 操作，不产生等待 future fanout。
- scheduler 联合压力每格至少 5 轮；正常负载不能因 usage writer 触发 degraded，故障移除后 5/5 恢复。
- 记录 p50/p95/p99、吞吐、drop、RSS、FD 和 Redis command latency；不能用单次本地延迟代替生产规模结论。
- 所有 scoped Cargo target 必须 `removed=true / reservation_released=true`。

## 残余风险

- 单 Lua 脚本减少网络 RTT 和取消窗口，但脚本内仍包含多条 Redis 命令并占用 Redis 单线程；当前单实例、受控故障矩阵已通过，生产高基数/cleanup 同窗与跨实例总并发仍需最终证明。
- 单 permit 只约束一个进程。多实例共享 Redis 时总并发等于实例数；单实例 simultaneous fault 已通过，跨实例 usage-writer + scheduler fault 和生产高基数仍需独立验证。
- Redis command-level 错误依靠 invalidation 回退 PostgreSQL，而不是回滚已经执行的 Redis 命令；这是刻意的 fail-closed 设计。
- Redis 整体不可达时可能无法写 invalidation marker。当前 disconnect/recovery 证明本轮 writer 没有隐藏重试并可恢复，但 Redis restart 后旧 derived cache 的跨实例 generation fence 仍需独立验证。
- cache-read 4096 是保护上限，不是对生产分布的性能证明；生产规模可能需要更低上限或移除 exact-token Redis 物化。
