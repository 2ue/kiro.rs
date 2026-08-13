# 有限调度队列租约与内部 Redis RPM 放大证据

Date: 2026-07-18

Source: `401473c` (`v0.0.109`) 上的未发布 dirty tree

Status: `focused deterministic pass / all-target check pass / real Redis and PostgreSQL execution pending`

## 结论

高并发、低下游 RPM 时，内部高 RPM 不只可能来自 inference retry、OAuth refresh 或 profile discovery。旧本地调度队列为每个 Redis waiter 建立一个租约；默认最大等待为 120 秒、初始 TTL 为 180 秒，初始 TTL 已覆盖整个有限等待期，但 guard 仍每 20 秒续租。500 个持续排队 waiter 会产生约 25 次续租/秒；考虑超时检查先于最后一次续租，完整 120 秒窗口约为 1250-1500 次 Redis renewal/分钟。它不是下游请求 RPM，也不是 Kiro inference RPM，却会在调度拥塞时反向增加 Redis 单线程压力。

外部池存在同类路径：默认等待 30 秒、旧 TTL 60 秒，本来足以覆盖请求，但第 20 秒仍会续租一次。旧本地实现还有一个相反的正确性缺口：`WaitForCapacityMax` 大于全局配置时，TTL 仍按全局配置计算，只能依赖续租避免合法长等待提前丢 lease。

## 修复合同

- 有限本地等待的 TTL 为本请求实际最大等待向上取整后加 60 秒，且不安排周期续租。
- 有限 external 等待使用同一合同；默认 30 秒等待得到 90 秒 TTL，20 秒处不再续租。
- `WaitForCapacityMax` 使用 override 本身计算 TTL；300 秒 override 得到 360 秒 TTL。
- 无限等待保持 60 秒 TTL 和最长 20 秒续租周期，不能用优化牺牲 crash-safe lease。
- 每个请求在进入调度时冻结自己的最大等待。运行时配置上调或下调只影响后续请求，不会令已 admission 的 TTL 与超时判断分叉。
- admission、release、取消清理、Redis fail-closed 和全局 queue limit 不变；热路径只增加一次固定整数计算，不增加 I/O、任务或正文处理。

## 当前确定性验证

`queue-lease-amplification-r2` 在 Rust 1.92.0 下实际执行六个精确测试：

- 500 个 finite guard 全部 `next_renew_at=None`，0 个 guard 可进入 renewal 分支；drop 后 local queued 归零。
- unlimited guard 仍正确 arm renewal。
- local default、300 秒 override、1501 ms 舍入和 unlimited policy 各内部 5 轮。
- external default、120 秒、1501 ms 和 unlimited policy 各内部 5 轮。
- 40 凭据、每凭据 15 并发、60 RPM、global 500 的 queue/release/unlimited 对照内部 5 轮。
- 40 凭据非终态 runtime mutation backlog 内部 5 轮，保持 available 40/40。

六个过滤器均为 `running 1 test / 1 passed`；scope `size_kib=1684700`，退出 `removed=true / reservation_released=true`。

动态配置边界加入后，`queue-lease-refresh-provider-r1` 再次执行并通过：

- 已 admission 请求保持原 1 秒 deadline，即使 runtime config 上调为 5 秒；内部 5 轮，合计约 5.02 秒。
- local policy、500 finite guard、external policy 和 40x15/global-500 均再次实际运行 1 项并通过。
- API/MCP final-attempt 真实 loopback 请求 fixture 内部 5 轮通过：每轮 API inference=1、MCP inference=1、OAuth refresh=0。
- token-refresh Redis secret-free/bounded 静态合同内部 5 轮通过。
- `cargo check --all-targets` 通过且无 warning。

该 scope 使用 10 GiB reservation，实际 `size_kib=2030352`，退出 `removed=true / reservation_released=true`。

## 真实存储验证程序

新增 `feature/tests/run-runtime-quarantine-storage-validation.sh`。它要求独立 PostgreSQL 和 Redis URL、两个显式 isolated 标志，默认执行 3 个 outer rounds，并在 Cargo 前完成 scheme、loopback/opt-in、端口、TCP 可达性检查。任何 URL 指向 9022 都直接 exit 64，且不会探测该端口。每轮执行：

- 真实 PostgreSQL 两连接池全部占用，使普通 success mutation 命中 5 秒 acquire/write deadline；测试内部 5 轮，要求只进入 FIFO backlog、不 quarantine、不 disabled，释放连接后 revision 顺序推进并恢复 Ready。
- PostgreSQL FIFO replay/unquarantine 与 generation reset fencing。
- 真实 Redis finite queue：保持 waiter 22 秒跨过旧 20 秒 renewal 点，要求 ZSET deadline 完全不移动，取消后 queue lease 释放。
- Redis coordination degraded fail-closed 与取消释放。

缺 URL 的 runner 在 Cargo 前 exit 64；PostgreSQL URL 指向 9022 的 runner 同样在 Cargo 前 exit 64。两个负向 gate 均未创建 scoped target。

当前主机没有用户确认的隔离非 Docker PostgreSQL/Redis。`runtime-storage-validator-compile-r3` 的 `cargo check --all-targets` 通过；两个新 storage test 被 test harness 编译，但正文明确打印缺 URL 并提前返回，因此只能计为 compile-only，不能计动态 PASS。该 scope `size_kib=2036280`，退出 `removed=true / reservation_released=true`。

## 红项与无效证据

- `queue-lease-amplification-r1` 被命令执行器短超时杀死，未形成测试结果；遗留的 14448 KiB owned target 和 reservation 随后由 `run-cargo-scoped.sh --reap-stale` 校验 owner/PGID 后删除。它只证明 stale cleanup，不计行为证据。
- `runtime-storage-validator-compile-r1` 因取消测试的 `unwrap_err()` 要求 `CallContext: Debug` 而编译失败；改为显式 match。scope `447300 KiB`，已回收。
- `runtime-storage-validator-compile-r2` 因默认 12 GiB reservation 与 20 GiB floor 相差约 203 MiB，在编译前 fail closed；16 KiB 元数据已回收。后续使用仍保留 20 GiB floor 的 10 GiB reservation。

## 未关闭门禁

- 在调用方确认的隔离 PostgreSQL/Redis 上执行上述 runner 三轮；不得借用未知 listener，也不得把缺依赖早退算通过。
- 冻结候选上执行 500 并发、100 秒慢流、Redis latency/disconnect/recovery、PgSQL writer pressure、local/external takeover 与 RSS/FD/TTFB 三次 soak。
- 将 Redis queue admission/renew/release、scheduler snapshot、usage writer、OAuth/profile/inference 分通道计数带入同一负载报告，确认内部放大不再转移到其他通道。
- 发布 inventory 当前仍被编辑器拥有的根 `target/` 和引用进程阻断；本证据没有删除或干扰该目录。
