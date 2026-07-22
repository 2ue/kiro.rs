# Usage Cleanup Safety And Redis Isolation

Role: Usage 明细清理、Redis 单线程隔离、持久任务恢复与 F03 验收权威

Status: `cleanup-suite-and-writer-lock-order-focused-pass / dynamic-multi-instance-recheck-pending`

Severity: P1

Last updated: 2026-07-16

## 结论

旧实现存在两条独立危险路径，不能只通过把 `DEL` 改成 `UNLINK` 修复：

1. `POST /api/admin/usage-records/clear` 在请求线程同步清内存、Redis summary/dashboard/snapshot，并对 PostgreSQL 全表执行 soft delete。
2. 已有后台 `/cleanup/start` 在提交前同步执行 `COUNT/MIN/MAX`，任务结束后再做一次完整 preview；任务状态和 cancel 只在进程内，重启后无法判断是否完成，最后仍一次性清 Redis snapshot。

本轮已移除生产同步全表 clear 数据面。旧 URL 保留兼容，但现在返回 `202 Accepted` 和持久化 queued job。最终产品口径已经明确：soft cleanup 删除范围内明细时，必须在同一 PostgreSQL 事务中同步扣减对应累计统计、费用、credential summary、Dashboard rollup、cache-read 和 duration histogram；hard cleanup 只物理删除已经 soft-delete 的 tombstone，不得再次扣减 rollup。PostgreSQL 使用小批短事务，Redis 使用多 pass 小 SCAN 和小批 UNLINK，任务通过 PostgreSQL lease/checkpoint 支持取消、进程恢复和显式 resume。

这不等于 F03 已通过。隔离 PostgreSQL 与 Redis 的持久化、批量、rollup 一致性、晚到重放拒绝、零残留和取消集成已完成多轮验证。一次针对旧测试合同的补充运行暴露了 soft clear 后同 ID 恢复期望与 tombstone 设计不一致；当前源码已把合同固化为“soft tombstone 存在期间，同 ID 即使新 `created_at` 晚于 cutoff 也不复活；cutoff 后的新 ID 可以写入”。主线还增加了 usage writer shared / cleanup exclusive transaction advisory lock，关闭已开始但未提交的旧 usage 在 watermark 之后提交的竞态。编译恢复后，round-trip 1/1 通过；完整 `cargo test cleanup -- --nocapture --test-threads=1` 又连续三次外层执行，每次 36/36，合计 108/108、0 ignored。三次运行中的 PostgreSQL fallback summary p95 为 `4.952-9.945 ms`、Dashboard p95 为 `16.645-49.070 ms`。这些结果仍不等于完整 PostgreSQL/全树，也没有量化 advisory-lock writer 争用；Redis 慢断、完整进程恢复、pass-limit 恢复、生产规模 scheduler 压力和 UI browser 仍未关闭，发布总门禁必须保持未通过。

2026-07-16 的双实例 request-admission runner 又确认一条与 cleanup 独立但同属 usage writer 的发布阻断：旧 binary 在每轮出现 1-4 次 `40P01 deadlock detected`，即使 `record_count=1` 也会发生，部分 batch 因 deadlock detector 等待约 1 秒。根因是 `record_batch` 和 `UsageRollupBatchDelta::apply` 直接消费进程随机 seed 的 `HashMap`：两个实例会以不同顺序锁定相同 global/status/model/endpoint rollup 行；shared advisory lock 只排斥 cleanup，不会串行 writer，因此不能阻止 writer/writer 锁序反转。当前源码已按 request ID、total/time/cache/duration/credential 完整 key 排序，并给 detail `SELECT ... FOR UPDATE` 加 `ORDER BY id`。隔离 PostgreSQL 回归连续 3 次外层通过，每次 64 对并发 writer、128 条记录，总计 192 对/384 条写入且 0 deadlock。它仍是 focused source 证据；新 binary 的双实例 admission 五轮必须显示 `usageDeadlockRetries=0` 后才能关闭。

增强 same-ID 测试随后暴露 missing-row gap：两个事务首次并发写同一 ID 时，前置 `SELECT ... FOR UPDATE` 都可能看不到尚未提交的行，第二个事务虽在 `ON CONFLICT` 等待并覆盖 detail，却仍按“旧值不存在”再次增加 rollup。当前源码在读取旧行前按 request ID 的稳定 SHA-256 派生 64-bit transaction advisory lock，并按 lock ID 排序，三轮 focused 测试最终 global request 严格为 130。该协议只对所有 writer 都运行新代码时成立；旧/新实例混跑仍可能重复累计，因此本版升级必须全实例停写、排空、统一切换，不支持 mixed-version rolling writer。

独立审计还确认两项运维边界。第一，soft watermark 原先在设置 `lock_timeout=250ms` 之前取得 exclusive guard，长 writer 可令任务无界等待；当前源码已把 transaction timeout 前移，待真实 PostgreSQL 验证 250ms 失败和释放后恢复。第二，compression/backfill 持 exclusive guard 可保证事务一致，却会阻塞在线 writer 超过其三次 5 秒重试并导致整批丢弃。当前源码增加数据库生命周期 fence：服务实例持 shared session lock，离线 maintenance 必须取得 exclusive session lock；检测到当前版本在线实例时 fail-fast，并拒绝 `compressUsageRollupsOnStart=true`。首次从不认识该 fence 的旧版本升级仍必须先停止并排空所有旧实例。

## 用户现象与影响

- Admin 点击“清空”时请求长时间占用，失败时无法知道 PostgreSQL 和 Redis 分别执行到哪里。
- 高基数 usage key 清理可在 Redis 单线程执行 25-90ms 甚至更久的命令，与 scheduler 75ms 热路径直接竞争。
- scheduler 因 Redis 热路径超时进入 degraded/backoff，向用户表现为 `No account is ready...` 429；账号和外部池本身可能仍有容量。
- 旧后台任务在服务重启后状态回到 idle，但数据库可能已处理一部分，操作员只能重复提交并猜测结果。
- `max_batches` 到达时旧状态为 completed，无法区分“全部完成”和“仅达到安全上限”。
- Redis 清理失败只写 warning，任务仍可能显示 completed，Redis snapshot 可继续展示已 soft-delete 的明细。
- 两套 UI 源码已改为明确说明 soft cleanup 会同步扣除命中记录对应的累计统计、费用和 Dashboard rollup，hard cleanup 只物理删除 tombstone；最终浏览器确认文本和交互仍需在冻结候选上验证。

这些表现没有 tool hash 等协议指纹；它们属于存储和调度资源竞争问题。

## 修复前源码链

```text
POST /api/admin/usage-records/clear
  -> src/admin/handlers.rs::clear_usage_records
  -> src/admin/service.rs::AdminService::clear_usage_records
  -> src/anthropic/usage.rs::UsageRecorder::clear
     -> records.lock().clear()
     -> block_on RedisStore::clear_usage_summary()
     -> block_on PostgresUsageStore::clear()
        -> UPDATE usage_records ... WHERE deleted_at IS NULL
```

```text
POST /api/admin/usage-records/cleanup/start
  -> normalize request
  -> synchronous preview COUNT/MIN/MAX
  -> in-process UsageCleanupRuntime
  -> tokio::spawn batch loop
  -> final synchronous-equivalent preview COUNT/MIN/MAX
  -> one-shot Redis snapshot pattern delete
```

旧 Redis pattern 删除为 `SCAN COUNT 1000`，随后把该次返回的全部 key 交给一条 `DEL`。即使 SCAN 本身渐进，单条 DEL 的参数和主线程释放工作仍无硬上限。

## 根因

1. 旧 clear 把“Admin 命令提交”“明细数据库清理”“聚合清理”“缓存失效”合成一个同步操作。
2. 后台 job 没有持久化权威、worker lease、phase checkpoint 或恢复入口。
3. PostgreSQL batch 数量过大（默认 1000、最大 5000），无 lock/statement timeout，也未跳过被其他事务持锁的行。
4. Redis 删除只限制 SCAN hint，没有限制单条删除命令 key 数，也没有主动 yield。
5. SCAN 同时删除会改变 keyspace；只扫一遍不能证明零残留。
6. Redis 阶段可能超过 worker lease；无 heartbeat 时第二实例可能在 lease 过期后并发接管。
7. cancel 只改本实例 AtomicBool，跨实例 worker 看不到；Redis 阶段也没有检查 cancel。
8. 达到 max_batches、真正完成、用户取消和失败没有不同的状态/恢复语义。

## 选定方案

### API 与语义

- `/usage-records/clear` 保留兼容 URL，但只提交 `olderThanDays=0` 的 soft-delete 后台 job并返回 202。
- `/cleanup/start` 只做校验和单行 job INSERT；不隐式 preview。显式 `/cleanup/preview` 仍作为操作员主动选择的只读操作。
- 新增 `/cleanup/resume`，只接受 paused、failed 或 cancelled job。
- soft cleanup 同步删除范围内明细对 summary/dashboard/费用/credential/cache/duration rollup 的贡献；UI 必须明确展示这一影响，不得承诺保留累计统计。
- hard cleanup 只物理删除 tombstone。正常 soft cleanup 已将 `rollup_active` 置为 false，因此后续 hard cleanup 不重复扣减；只有历史 tombstone 仍带 `rollup_active=true` 时才兼容扣减一次。
- queued/running 是活动状态；达到每次 max_batches 后为 paused，不伪装 completed。

### PostgreSQL

- 新表 `usage_cleanup_jobs` 持久化 plan、status、phase、progress、cancel、Redis 删除统计、lease 和错误。
- partial unique index 保证全局只有一个 queued/running job。
- worker 原子 claim；30 秒 lease，每 10 秒 heartbeat。续租失败或 lease 丢失后当前 worker停止，不再写 final。
- cancel 写 PostgreSQL 权威字段；heartbeat 和每批 progress 都读取该字段，同实例 AtomicBool 只用于更快停止。
- 默认 batch 250，硬上限 500。
- 每批独立事务，`lock_timeout=250ms`、`statement_timeout=2s`、`FOR UPDATE SKIP LOCKED`。
- `usage_cleanup_watermarks` 保存单调 soft cutoff；`created_at < watermark` 的晚到记录和 replay 被拒绝，避免清理后旧数据重新进入 rollup。
- usage writer transaction 在读 watermark、写 detail/rollup 到 commit 期间持有 `pg_advisory_xact_lock_shared`；cleanup 推进 watermark 前取得同一 key 的 exclusive transaction advisory lock。这样 cleanup 会等待所有已开始的 writer commit，再推进 cutoff 并处理其旧记录；推进后的 writer 则读取新 watermark。
- 明细 soft-delete、`rollup_active=false` 和 rollup 负增量位于同一事务。负增量后只按本批受影响的维度键删除 `requests <= 0` 行，不做全表修复扫描。
- global total 和 global time bucket 的 duration max 从精确 duration histogram 重算；其他维度的 max 当前不是外部消费权威。
- 每批持久化 checkpoint 并主动 `yield_now()`；配置 pause 为 0 时仍会 yield。
- processed/batches 是累计值；max_batches 是每次 worker run 的预算。resume 从当前累计数开始获得新的单次预算。

### Redis

- 每次 `SCAN COUNT 128`。
- 每条删除命令最多 64 keys，优先 `UNLINK`。
- 只有错误明确包含 unknown command + UNLINK 时，才对同一小批回退 `DEL`；连接错误、超时和其他 ERR 不回退。
- 每条删除命令和每个 cursor 段后主动 yield。
- 从 cursor 0 重复全量 pass，直到完整 pass 删除数为 0；最多 8 pass。达到上限标记 failed，不能静默 completed。
- soft cleanup 后持久化 Redis derived-cache invalidation 标记；summary/dashboard/snapshot 写入拒绝继续更新旧聚合，读取返回 miss 并回退 PostgreSQL，避免清理与并发旧 writer 竞态重新污染结果。
- 记录 deleted keys、commands、max command keys、scan passes、DEL fallback 和 pass-limit。
- cancel 最多在当前 64-key command 后生效。snapshot index 在 pattern 前后各做一次 O(1) UNLINK；取消时 item 可暂时残留，但 index 已不可达，resume 会再次从 cursor 0 幂等清理。
- Admin usage cache 本地 shadow 立即失效；Redis Admin cache TTL 为 2 秒。取消时允许停止 pattern 清理，避免为了清缓存拖延 cancel。

## 幂等、取消和恢复合同

- Soft delete 只匹配 `deleted_at IS NULL` 且只对 `rollup_active=true` 的贡献扣减一次；重复 batch 不会重复处理。
- Hard delete 只匹配已 soft-delete 且早于 cutoff 的行；`rollup_active=false` 的正常 tombstone 不再扣减，历史 active tombstone 只兼容扣减一次。
- cutoff 之前的晚到/重放由 PostgreSQL、Redis 和进程内 watermark 共同拒绝；cutoff 之后的新 ID 可以写入。soft tombstone 仍存在时，同 ID 即使新事件的 `created_at` 晚于 cutoff 也不复活。hard cleanup 物理删除 tombstone 后，watermark 仍能拒绝保留原 `created_at` 的正常 replay，但数据库已没有 ID 证据，无法识别伪造了更新 `created_at` 的同 ID；这是明确残余边界，不能描述成永久 ID 防重。
- Redis UNLINK/DEL 对不存在 key 返回 0；resume 从 cursor 0 重扫。
- cancelled/paused/failed resume 保留累计 processed/batches 和 phase。
- pass-limit 是本轮收敛结果；requeue 清除 `redis_pass_limit_reached`，但保留累计 scan pass 统计。
- PostgreSQL 阶段未确认完成时，即使做过缓存失效，最终 phase 仍回到 postgres。
- PostgreSQL 已确认无剩余但 Redis 未完成时，phase 保留在 `redis_admin_cache` 或 `redis_snapshots`。
- 只有 PostgreSQL 确认无剩余且 Redis 失效完整收敛时，状态才是 completed、phase 才是 complete、remaining 才是 0。

## 复现方案

### R1：证明旧同步链已移除

```bash
rg -n "UsageRecorder::clear|usage_recorder\.clear|UPDATE usage_records SET deleted_at.*WHERE deleted_at IS NULL" src
```

验收：生产代码没有同步 clear 调用或无界全表 update；仅允许 `cfg(test)` fixture helper。

### R2：PostgreSQL job 与 batch

设置隔离测试库，不得指向生产：

```bash
KIRO_RS_TEST_POSTGRES_URL='postgres://.../isolated-test' \
  cargo test postgres_usage_cleanup_job_is_persistent_exclusive_and_recoverable -- --nocapture

KIRO_RS_TEST_POSTGRES_URL='postgres://.../isolated-test' \
  cargo test postgres_usage_cleanup_batches_are_bounded_idempotent_and_skip_locked -- --nocapture

KIRO_RS_TEST_POSTGRES_URL='postgres://.../isolated-test' \
  cargo test cleanup -- --nocapture --test-threads=1

KIRO_RS_TEST_POSTGRES_URL='postgres://.../isolated-test' \
  cargo test postgres_persists_runtime_config_credentials_stats_usage_and_pricing \
  -- --nocapture --test-threads=1
```

连续执行 3 轮。验收：单 active、未过期 lease 不可抢、过期可接管、cancel 持久、requeue 保留 checkpoint、pass-limit flag 重置；持锁最旧行被跳过；soft/hard 重复执行最终均返回 0；soft cleanup 后明细与所有权威 rollup 一致，hard cleanup 不双扣；既有同 ID 恢复合同与最终选择一致且测试通过。

### R3：Redis 有界删除与零残留

```bash
KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:<isolated-port>/' \
  cargo test redis_pattern_delete_is_bounded_and_cancellable -- --nocapture
```

单次测试内部包含 321、338、355 keys 三轮。每轮在删除后重新 SCAN，必须为 0；max command keys 必须不超过 64，scan passes 至少 2，pass-limit=false。取消分支必须 index 不存在、item 仍存在，随后恢复清理归零。

### R4：故障注入与 scheduler 并发

在隔离服务、隔离 Redis/PG 和假上游执行，不触碰 9022：

1. 固定 scheduler 请求负载并记录成功、degraded、dispatch 429、Redis command p95/p99。
2. 分别运行正常 cleanup、cancel、pause/resume、Redis 50/75/100/150/300ms、Redis reset、PG lock/statement timeout。
3. 每个故障点 3 轮；每轮结束重新 SCAN usage/admin pattern 并查询 job row。
4. 记录 scheduler routeSubtype、selectionFailure、globalInFlight 和 cleanup command stats。
5. 并行记录 PostgreSQL usage writer 在无 cleanup、等待 exclusive cleanup lock、cleanup 完成后三个阶段的 p50/p95/p99、queue time 和失败数。

验收：清理不诱发新的 SchedulerRedisDegraded 或假容量 429；advisory lock 不死锁/饥饿，writer 等待有界且恢复后无持续延迟；单请求 cancel 在一个 PG batch 或一个 Redis 64-key command 后停止；失败 job 可 resume；恢复后正常请求 5/5 成功。

## 当前验证结果

| 验证 | 结果 | 说明 |
| --- | --- | --- |
| `cargo check --tests` | PASS | 2026-07-16；仅有并行 attempt-budget provider 旧 wrapper dead-code warning |
| cleanup 请求默认/边界 | PASS | 4 tests；默认 250、最大 500、cutoff/0-day/非法上限 |
| resume 单次 batch budget | PASS | 1 test；累计 batches 不令 resumed worker 立即再次 paused |
| UNLINK fallback classifier | PASS | 1 test；只接受明确 unknown UNLINK command |
| PostgreSQL persistent/lease/cancel/requeue | PASS after fix | 隔离 PostgreSQL 外层 3 轮；首次运行发现 `INT4` 按 `i64/INT8` 解码失败，修正 job row 类型映射后 3/3 通过 |
| PostgreSQL batch/idempotent/SKIP LOCKED | PASS | 与上项同一外层命令，修复后 3/3 通过；每轮 soft/hard 最终为 0，锁行被跳过 |
| cleanup 一致性总组 | PASS (3 outer runs) | 隔离 PostgreSQL/Redis，`cargo test cleanup -- --nocapture --test-threads=1` 每次 36/36，三次合计 108/108、0 ignored；覆盖 watermark、in-flight commit、并发旧写、soft/hard rollup、duration max、legacy cost、零计数定点删除、lease/cancel/recovery 和 Redis guarded commit |
| historical cost + same-ID update | PASS | 本地 estimated fallback 与 external raw-cost fallback 各内部 3 轮；同 ID 降费用/降 duration 后只保留新值 |
| 高基数 rollup pruning | PASS | 每轮 48 个唯一 conversation/credential/cache/duration，以 batch 7 多批清理，内部 3 轮；六类表无零计数残留，新记录恰好一次 |
| PostgreSQL fallback 性能 | PASS (isolated cleanup runs) | 三次外层 cleanup 运行中的 summary p95 `4.952-9.945 ms`，dashboard p95 `16.645-49.070 ms`；只证明隔离 fixture，不代表 writer advisory-lock 或生产规模 |
| Redis usage writer + scheduler burst | PASS (isolated) | 三轮 loaded scheduler p95 `3.027-4.406 ms`、p99 `4.319-15.936 ms`，低于 75 ms 预算 |
| 两实例 usage writer 锁序 | FOCUSED PASS / dynamic pending | 旧双服务 binary 每轮 1-4 次 `40P01`；排序修复后真实 PostgreSQL 3 外轮 x 64 并发对，192 对/384 records 全通过、deadlock 0；待新 binary 双服务五轮 |
| missing-ID same-ID exactly-once | FOCUSED PASS / six-authority pending | per-ID xact advisory lock 后三外轮均通过，每轮含 32 对反向 same-ID batch，global requests=130；仍待六类 rollup 全字段 oracle 和新 binary 多实例 |
| watermark lock timeout | IMPLEMENTED / runtime pending | timeout 已前移到 exclusive guard 之前；待真实 PG 断言约250ms失败、释放后恢复 |
| offline maintenance lifecycle fence | IMPLEMENTED / runtime pending | 当前版本 service shared / maintenance exclusive；startup compression true fail-fast；旧版本不认识 fence，因此首次升级仍禁止 mixed-version rolling |
| soft clear 后同 ID 合同回归 | PASS (focused) | 旧合同运行曾在 `src/storage/postgres.rs:10691` 暴露漂移；更新后的 `postgres_persists_runtime_config_credentials_stats_usage_and_pricing` 1/1 通过，断言 soft tombstone 不复活且 cutoff-newer 新 ID 可写 |
| 1000 external billing cleanup | PASS (focused 1 + cleanup outer 3) | `postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup` 先聚焦 1/1，再随 cleanup 组三次外层通过；summary/Dashboard external billing 贡献归零 |
| replay / cutoff-newer new ID | PASS (focused + cleanup outer 3, each internal 3) | `postgres_cleanup_rejects_late_replay_but_accepts_newer_records_for_three_rounds` 聚焦内部三轮通过，并随 cleanup 组三次外层各执行内部三轮 |
| in-flight usage commit / watermark 竞态 | PASS (cleanup outer 3, each internal 3) | `record_batch` 持 shared xact advisory lock，watermark advance 持 exclusive lock；`postgres_cleanup_watermark_waits_for_inflight_usage_commit_for_three_rounds` 随 cleanup 组三次外层各执行内部三轮，watermark 等待旧 writer commit 后再清理 detail/rollup |
| Redis 321/338/355 零残留与 cancel index | PASS | 隔离 Redis 外层 3/3 通过；每次内部三组 key 数，合计 9 组零残留；cancel index 分支 3/3 |
| Redis latency/reset + scheduler pressure | NOT RUN | 必须由隔离 load/chaos gate 执行 |
| 两套 UI production build/browser | NOT RUN | 由总 UI gate 执行 |

本批隔离端口为 PostgreSQL `47432`、Redis `47379`；均不属于现有 `9022` 服务或开发 Redis。详细命令、合同漂移发现、源码合同更新、四项聚焦通过和剩余门禁见 [Usage cleanup storage integration](../evidence/usage-cleanup-storage-integration-20260716.md)。

## 发布门禁

对应 [重新验证矩阵](../tests/reverification-matrix.md) F03，并与 E04 共同验收。以下全部完成前不得将本文状态改为 `verified-fixed`：

- PostgreSQL 两个集成测试各 3 轮通过。
- soft tombstone 存在期间同 ID 不复活的 round-trip 已聚焦 1/1 通过；仍须纳入完整 PostgreSQL/全树，不能只运行 cleanup 过滤组。
- in-flight writer / watermark advisory-lock 已随 cleanup 组完成三次外层执行，且每次内部三轮；仍须在 writer 并发负载下量化等待、吞吐与恢复。
- PostgreSQL writer 基线与 cleanup 并发压力下 p95/p99、等待时间和吞吐回退满足总性能预算；不能只以竞态测试通过证明热路径成本可接受。
- 新 binary 双实例 writer/admission 至少 5 轮 `usageDeadlockRetries=0`；不得把 retry 后最终成功当成无 deadlock。
- 六类 rollup authority 必须与最终 active detail 独立重算一致；不能只断言 global requests。
- watermark exclusive guard 的 250ms timeout、释放后恢复和无饥饿至少三外轮通过。
- service shared / maintenance exclusive lifecycle fence 至少三轮通过；在线 service 必须拒绝 maintenance，maintenance 必须拒绝 service startup，释放后双方恢复。
- 当前版本首次部署采用全实例停写、usage drain、停止旧实例、维护/升级、统一启动新实例；mixed old/new writer 不在支持范围。
- soft cleanup 后 detail、summary、Dashboard、credential cost、cache/duration histogram 均只含 cutoff 后贡献；hard cleanup 不重复扣减。
- Redis 集成测试 3 次外层运行通过，且每次内含三组零残留断言。
- Redis 慢/断/恢复与 scheduler 并发压力每个故障点 3 轮通过。
- cancel、paused resume、Redis pass-limit resume 和进程重启恢复均有运行证据。
- 两套 UI build/browser gate 通过。
- 证据记录构建 hash、隔离端口、命令延迟、scheduler degraded/429 数、残留 key 数和 job final row。

## 残余风险与回滚

- usage 与 scheduler 仍可能共用同一个 Redis 实例；小命令和 yield 降低阻塞上限，但不能替代生产容量规划和 Redis workload 隔离。
- Redis derived-cache invalidation 当前没有有界重建路径；首次 cleanup 后 summary/dashboard 会长期使用 PostgreSQL 权威。隔离延迟可接受不等于生产数据规模已验证。
- 每个 PostgreSQL `record_batch` 新增一次 shared advisory-lock SQL 往返；正常无争用时仍有固定延迟，cleanup exclusive lock 期间会有意阻塞 writer commit。当前没有最终候选的 writer burst/p95/p99 证据，必须在 F03/L3-L5 量化并验证恢复。
- lifecycle fence 只能约束实现该锁协议的当前及后续版本；旧 binary 在线时无法被数据库自动识别，首次升级仍依赖发布编排全停全起。绕过该步骤会重新暴露 same-ID double count 和 maintenance/write race。
- hard cleanup 删除 tombstone 后没有永久 request-ID ledger；旧 `created_at` replay 仍由 watermark 拒绝，但伪造 newer `created_at` 的同 ID 可能重新写入。若业务要求永久 ID 防重，需要独立有界/持久 dedupe authority，不能依赖已经物理删除的 detail 行。
- 显式 preview 仍执行 COUNT/MIN/MAX；它不再是 start 的隐式步骤，但生产超大表上仍应由操作员谨慎使用并观测数据库。
- Admin Redis cache 在取消后最多依赖 2 秒 TTL 收敛；本地 shadow 会立即失效。
- 旧 clear API 响应从 200 SuccessResponse 改为 202 UsageCleanupStatusResponse；仓库内两套 UI 已同步响应类型和 soft/hard 删除说明，仍需最终浏览器验证。外部自建 Admin 客户端也需要按新响应与数据删除合同迁移。
- 回滚代码不能回滚 `usage_cleanup_jobs` 表；该表为向前兼容附加表，旧二进制会忽略它。回滚后不得重新启用旧同步 clear，除非明确接受原 P1 风险。
