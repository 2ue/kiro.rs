# 调度队列租约续租导致内部 Redis RPM 放大

Date: 2026-07-21

Status: `reproduced / fixed-in-dirty-tree / focused-isolated-redis-and-e03-two-process-pass / remaining-simultaneous-fault-external-native`

Severity: P1 performance and availability amplifier; P0 when it contributes to scheduler-wide degraded admission

## 现象与影响

系统可能同时出现“下游 RPM 不高、Kiro inference RPM 也不高，但进程/Redis 内部操作很高”。在本地凭据容量满后，每个等待请求都会持有一个 Redis queue lease。旧实现即使初始 lease TTL 已经覆盖整个有限等待期，仍按每个 waiter 最长 20 秒续租一次；waiter 越多，越会在 Redis 已拥塞时继续制造 scheduler capacity 写操作。

默认参数下，本地最大等待 120 秒、初始 TTL 180 秒。500 个持续 waiter 的 renewal 量约为 25 ops/s，即 1250-1500 ops/min。external 默认等待 30 秒、TTL 60 秒，也会为每个持续 waiter 在第 20 秒额外写一次。该操作不应被误计为用户请求或模型调用，但会增加 Redis 单线程尾延迟，间接提高 75 ms scheduler hot-path timeout、breaker open、local fallback/429 和慢首字概率。

本问题与 [高并发低 RPM/runtime quarantine](high-concurrency-low-rpm-runtime-quarantine.md) 紧密耦合，但不是 PgSQL 假禁用的同一状态机；它是独立的压力放大源。

## 源码链与根因

本地路径：

```text
MultiTokenManager::try_enter_dispatch_queue
  -> RedisStore::try_enter_dispatch_queue
  -> DispatchQueueGuard::confirm_redis_acquired
  -> next_redis_renew_at = now + min(ttl/3, 20s)
  -> acquire loop 每秒醒来
  -> renew_dispatch_queue_lease_if_needed
  -> RedisStore::renew_dispatch_queue
```

旧 TTL 为 `credentialDispatchMaxWaitSecs + 60`，默认 180 秒，已经长于请求最多 120 秒的 queue 生命周期；周期 renewal 没有增加正确性，只增加写操作。external 使用固定 60 秒 TTL/20 秒 renewal，默认 30 秒有限等待也有同一问题。

另一个隐藏边界是 `WaitForCapacityMax`：旧 lease TTL 只看全局配置，不看 request override。override 更长时，若简单删除所有 renewal 会让 lease 在请求仍合法等待时过期。runtime config 还可能在 waiter 已 admission 后变更；若超时判断动态采用新配置而 TTL 使用旧快照，也会产生分叉。

## 复现方法

### 单点复现

构造 500 个已确认 Redis admission 的 `DispatchQueueGuard`，使用默认 finite policy。修复前每个 guard 都设置 `next_redis_renew_at`；20 秒后 acquisition loop 可进入 Redis renewal。修复后 500 个 guard 必须全部不 arm renewal，drop 后 local queued 必须为 0。unlimited 对照必须继续 arm。

### 真实 Redis 复现

占用唯一 local capacity，令第二个请求进入 Redis queue，最大等待设置为 30 秒。读取 queue ZSET deadline，保持 waiter 22 秒跨过旧 20 秒 renewal 点，再次读取 score：

- 修复前 score 会向后移动约 20 秒；
- 修复后 score 必须逐值相等；
- 初始剩余 TTL 应约 90 秒；
- 取消 waiter 后 local/Redis queue 都在 2 秒内归零。

真实程序由 `feature/tests/run-runtime-quarantine-storage-validation.sh` 执行，默认 3 个 outer rounds；缺确认隔离 Redis、非 loopback 未 opt-in、TCP 不可达或端口为 9022 时必须在 Cargo 前失败。

### 配置变化复现

waiter 以 1 秒最大等待 admission 后，将 runtime config 上调到 5 秒。修复后该 waiter 连续 5 轮都按原 1 秒 deadline 超时；后续新请求才使用 5 秒。该合同保证 request TTL、超时和 remaining wait 使用同一快照。

## 选定修复与优化

- finite local TTL = `ceil(request max wait) + 60s`，renewal disabled。
- finite external TTL 使用相同规则，renewal disabled。
- unlimited local wait 使用 60 秒 TTL，renewal enabled，最长间隔仍为 20 秒。
- `WaitForCapacityMax` 按 override 本身计算，不再借用全局配置。
- request 进入 acquire loop 时冻结 max wait；runtime config 更新只影响新请求。
- admission/release/cancel、Redis fail-closed、queue limit 和 crash TTL 回收不变。

该实现不读取或改写 Messages body、prompt、tool、thinking、image、search 或 stream 数据。正常无排队请求不执行新分支；排队 admission 只多一次常量时间的 `Duration` 舍入/加法，减少而不增加网络 I/O。

未采用“把 TTL 设为一天后全部取消 renewal”：这会使进程崩溃后的 stale queue slot 长期占用。也未采用“只调大 Redis 75 ms timeout”：它不会消除内部 operation amplification。

## 验证结果与证据

`queue-lease-amplification-r2`：六个精确测试实际 6/6 通过，覆盖 500 finite guard、unlimited、local/external policy、40x15/global-500 和 runtime backlog，各适用合同内部 5 轮；scope `1684700 KiB` 后 `removed=true / reservation_released=true`。

`queue-lease-refresh-provider-r1`：当前实现七个精确测试实际 7/7 通过；deadline config、local policy、500 guard、external policy、40x15/global-500 和 Redis 静态合同各内部 5 轮，API/MCP real loopback fixture 也内部 5 轮保持每路径 inference=1/OAuth=0。Rust 1.92.0 `cargo check --all-targets` 无 warning；scope `2030352 KiB` 后完整回收。

2026-07-18 追加 `scheduler-focused-rerun-20260718-dev6g`，在 development reservation 6 GiB 下复跑 finite queue lease、deadline freeze 和 40x15/global-500 相关 filter，连同 scheduler fallback 与 runtime backlog 共 6 个精确 filter 全部 `running 1 / passed 1`。scope `size_kib=1699916 removed=true reservation_released=true`，后续根 target/flycheck 可再生产物也已删除为 `target=0 KiB`。这仍是 development/focused 证据，不替代真实 Redis 22 秒动态和冻结 L3-L5。

2026-07-19 使用当前仓库专属隔离 PostgreSQL/Redis 执行合批 storage suite。Redis queue 部分三轮动态通过：`finite_redis_dispatch_queue_lease_deadline_does_not_move_after_renew_interval`、`redis_dispatch_queue_waiter_fails_closed_after_coordination_degrades`、`redis_dispatch_queue_cancelled_waiter_releases_local_and_remote_lease` 均为 3 outer rounds 通过。该结果关闭“真实 Redis 22 秒动态未执行”的缺口，但不替代两实例、usage+scheduler 联合压力和冻结 L1-L5。完整命令和 artifact gate 见 [2026-07-19 storage evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。

2026-07-21 追加跨实例 RPM 同步预约修复与 E03 真实双进程验证。旧 `record_scheduler_selection()` 把 Redis selection/rate-limit 写入放在异步 best-effort 任务里，导致两个进程连续快速请求时第三次可能在同步到 `scheduler:rate_limit:<id>` 前继续 local `200`。当前新增 `RedisStore::try_record_scheduler_selection()`，在一个 Redis Lua 脚本内清理窗口、检查已有 shared deadline、统计 60 秒 selection、记录本次 selection，并在达到 RPM 时同步设置 shared deadline。manager 在有 Redis 且 `rpm>0` 时先做该同步 reservation；成功后本地记录不再重复写 Redis，rate-limited 时释放 provisional in-flight lease 并进入有界等待/明确错误。

验证：

```text
storage::redis_cache::tests::redis_scheduler_cooldown_and_rate_limit_round_trip = 1 passed
kiro::token_manager::manager::tests::redis_backed_rpm_reservation_blocks_third_cross_instance_selection = 1 passed
cargo fmt --check = pass
cargo check --all-targets = pass
E03 true two-process runtime outerRounds=3 = pass
```

E03 frozen candidate sha256 `98e0f79328b49925dc940faaa3b1e8b0c8ae8ef7b9975725eb219635c8957ee7`。三轮中 RPM first statuses 都为 `[200,200]`，post-restart statuses 都为 `[429,429]`，`externalHits=0`，`disabled=0`，Redis prefix 清理为空。详见 [E03 证据](../evidence/e03-real-two-process-scheduler-runner-20260720.md)。

## 性能、兼容性与回滚风险

预期性能变化是有限 waiter 的 Redis 写入严格减少；正常请求不变。请求级 deadline 冻结意味着 Admin 将 wait 配置从旧值改为新值时，已 admission waiter 保留旧值，新请求采用新值。这比中途延长/缩短已有请求更可预测，但属于需要保留的显式兼容合同。

若真实 Redis gate 暴露 lease 提前过期，只能回滚“finite 不续租”这一层，同时保留按实际 override 计算 TTL 和 request deadline 一致性；不得回滚到所有 waiter 无条件 20 秒续租而不记录放大量。若 unlimited renewal 失败，继续按 scheduler coordination unavailable fail closed，不能在本地猜测跨实例 queue 状态。

## 残余风险与发布边界

- 真实 Redis 三轮、单实例 latency/disconnect/recovery、单实例 usage-writer+Redis fault 和 E03 两实例 scheduler/RPM 已执行；external takeover focused/runner contract 已执行，但 dynamic service run 与两实例 fault/fallback 组合仍未执行。
- 500 并发、100 秒慢流与 usage writer 联合压力已有 fake-upstream/正常联合压力证据，并已补单实例 simultaneous fault；仍未与 external takeover dynamic、两实例 fault/fallback 和真实上游绑定同一最终候选。
- admission 和 release 本身仍各需要 Redis 操作；本修复只删除无必要的周期 renewal，不声称 scheduler Redis RPM 已归零。
- OAuth refresh、profile discovery、usage writer 和 inference retry 是其他独立 amplification channel，必须继续分别计数。
- 发布仍为 NO-GO；本专题 focused/storage/E03/single-instance-fault pass 与 external takeover runner contract 不能替代真实上游、external takeover dynamic service、两实例 fault/fallback、UI、upgrade 和最终 inventory。
