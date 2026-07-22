# 高并发低 RPM、排队与凭据运行态假禁用

Date: 2026-07-21

Status: `reproduced-defects / focused-storage-r8-load-single-instance-chaos-pass / e03-two-process-scheduler-pass / remaining-simultaneous-fault-external-native-ui-upgrade`

Severity: P0 for false local-pool loss; P1 for queue latency and misleading observability

## 现象

生产窗口中有 40 个活跃凭据，每个配置 `15` 并发、`60 RPM`，持久化状态和当前管理 API 都显示 `disabled=0`。异常时并发约 500、下游 RPM 约 300，请求大量等待、首字明显变慢，随后先出现 `local_scheduler_redis_degraded`，再出现约 1056 次 `local_all_disabled`，并最终大量转入 external pool。窗口结束后 40 个凭据又全部恢复正常。

这个现象由三个相互放大的状态组成，不能用“账号真的被禁用”或“RPM 没满所以并发一定有余量”单独解释。

## 已确认事实

1. `credentials.disabled=true`、`credential_runtime_state.disabled_reason` 和自动禁用事件均没有持久化证据；当前管理视图也显示 40/40 可用。
2. 当前源码只有内存中全部 `CredentialEntry.disabled=true` 时才产生 `LocalPoolRouteStateKind::AllDisabled`。单纯 Redis capacity breaker 打开应产生 `SchedulerRedisDegraded`，二者不是同义词。
3. Redis scheduler 热路径超过 75 ms 会打开 capacity breaker；当前工作树已把 snapshot、affinity 和 capacity breaker 拆开，但原子 lease/queue 仍必须在 Redis 协调不可用时 fail closed。
4. 任意 PgSQL runtime mutation 写入失败时，旧实现都会进入 `enqueue_pending_runtime_mutation()`，并无条件设置 `runtime_persistence_degraded=true` 和 `disabled=true`。这包括第一次普通 API failure、refresh failure，甚至 success reset，远未达到连续失败阈值也会隔离账号。
5. 生产同窗口有大量 `sqlx::pool::acquire` 慢获取和 usage/统计写入超时。若多个账号的 runtime mutation 同时无法取得 PgSQL 连接，它们会逐个只在本进程内变成 disabled；数据库事务没有成功，所以事后查询仍全部正常。后台 FIFO 重放成功后内存态又恢复，完全符合短窗口特征。
6. Redis-backed queue waiter 至少每秒唤醒一次；snapshot 同步已有 debounce/singleflight，但旧 queue guard 仍为每个 waiter 每 20 秒续租。默认本地 120 秒等待的初始 180 秒 TTL 已覆盖整个有限生命周期，500 waiter 仍会额外产生约 1250-1500 次 Redis renewal/分钟。external 默认 30 秒等待、60 秒 TTL 也会在第 20 秒做一次不必要 renewal。这是与 inference/OAuth RPM 不同的内部 Redis operation amplification。

## 为什么 500 并发与 300 RPM 可以同时成立

RPM 是请求开始速率，并发是仍未完成的存量，二者不是同一个上限。按 Little 定律：

```text
平均并发 L = 到达率 lambda * 平均占用时间 W
500 = (300 / 60 requests/s) * W
W ~= 100 seconds
```

只要流式或长任务平均占用本地 lease 约 100 秒，300 RPM 就能维持约 500 in-flight。若另有 `dispatchGlobalMaxConcurrentRequests=500`，第 501 个请求必然排队，即使 40 个账号的理论逐账号容量总和为 600、每账号 60 RPM 也远未用完。若全局上限为 0、weighted capacity 关闭、40 个账号都支持该模型且没有单账号覆盖/冷却/sticky 集中，则 500 个 weight=1 请求后应仍有约 100 个容量单位。

因此验收必须同时记录：

- 物理请求数与 weighted capacity units；
- 全局并发上限和逐账号有效并发覆盖；
- model-compatible / proxy / cooldown / RPM / concurrency blocked 数；
- queue depth、queue wait、lease age 和上游服务时间；
- downstream request RPM、local inference attempt RPM、auxiliary RPM；
- 持久化 disabled、runtime disabled reason、persistence backlog 和 quarantine。

## 根因状态机

```text
高并发长请求
  -> 本地 lease 长时间占用，排队与 TTFB 增长
  -> usage/统计与 runtime mutation 竞争 PgSQL；scheduler/usage 竞争 Redis
  -> Redis capacity operation > 75 ms
  -> SchedulerRedisDegraded，按配置外部 fallback 或本地 429
  -> 某次需要记录的账号失败遇到 PgSQL acquire/写入超时
  -> mutation 进入本地 FIFO
  -> 旧实现无条件把该账号 runtime_persistence_degraded 当成 disabled
  -> 多账号依次进入同状态，最终 available=0 / local_all_disabled
  -> external preflight/fallback 接管全部流量
  -> PgSQL 恢复后 FIFO 重放，内存账号重新可用，数据库事后看不到临时假禁用
```

Redis degraded 是第一条协调故障；PgSQL backlog 无条件 quarantine 是第二条独立缺陷。只开启 external fallback 会转移流量和成本，不能根治。只调大 75 ms 会增加尾延迟，也不能修复假禁用。

有限 queue lease 的周期续租是第三条压力放大链：它本身不产生下游或 inference 请求，但会在 waiter 最多、Redis 已经变慢时增加 scheduler capacity Redis 操作，扩大超过 75 ms 和 breaker open 的概率。

## 选定修复

将两个原本混在一个 bool 里的概念拆开：

- `runtime_persistence_degraded`：存在必须按 operation ID、generation 和 FIFO 顺序重放的 PgSQL mutation。
- `runtime_persistence_quarantined`：待重放 mutation 包含未确认前不能继续调度的显式状态转换。

分类合同：

| Mutation | 有 backlog | 立即 quarantine |
| --- | --- | --- |
| Success | 是 | 否 |
| ApiFailure | 是 | 否；本地 failure count 达真实阈值时仍按阈值禁用 |
| RefreshFailure | 是 | 否；本地 refresh failure count 达真实阈值时仍按阈值禁用 |
| 健康/统计 Patch | 是 | 否；只更新 failure/refresh/warmup/last-used 等字段 |
| Disable | 是 | 是 |
| 调度状态 Patch | 是 | 是；包含 credential disabled、禁用原因或 generation 推进时保持 fail closed |

FIFO 重放、operation ID 幂等、generation fencing、显式禁用和管理变更的安全边界不变。普通健康统计/失败计数的存储瞬态故障不再把整个本地池逐账号伪装成 disabled。

队列租约同时改为生命周期驱动：有限等待一次 admission 的 TTL 覆盖“本请求最大等待 + 60 秒安全余量”，不安排 renewal；无限等待继续使用 60 秒 TTL 和最长 20 秒 renewal。`WaitForCapacityMax` 按实际 override 计算 TTL。每个已 admission 请求冻结自己的等待期限，runtime config 更新只影响后续请求，避免 TTL 与动态超时口径分叉。external 有限等待采用相同规则。

## 确定性复现与验证程序

### R1：40 账号非终态 mutation backlog

对 40 个账号分别排入 Success、ApiFailure、RefreshFailure 和自动 `refresh_failure_count=0` 健康 Patch，连续 5 轮。旧实现得到 `available=0 / AllDisabled`；修复后必须保持 `available=40 / Ready`，同时每个账号仍标记 persistence degraded。再给两个账号排入 Disable 和包含禁用/generation 变更的调度状态 Patch，只有这两个账号必须 quarantine，池仍为 `available=38 / Ready`。

### R2：40 x 15、60 RPM、global 500

连续 5 轮为 40 个账号均匀持有 500 个 weight=1 lease：

- `globalInFlight=500`；
- 40 个账号仍 available，disabled=0；
- RPM blocked=0；
- global=500 时 route state 必须为 CapacityFull，第 501 个请求进入 bounded queue；
- 释放一个 lease 后等待者在 1 秒内获得容量；
- global=0 时同样持有 500 个 lease，route state 仍 Ready，第 501 个请求在 500 ms 内获得本地容量且 queue=0。

### R3：PgSQL 故障程序

`feature/tests/run-runtime-quarantine-storage-validation.sh` 要求调用方提供专用 PostgreSQL/Redis URL 和两个 isolated 确认，在 Cargo 前校验 scheme、loopback/显式 opt-in、TCP 可达性并按数值拒绝 9022。真实 PostgreSQL 测试占满两条 pool connection，使普通 success mutation 命中 5 秒 acquire/write deadline；内部 5 轮必须保持 Ready、不 quarantine、不 disabled，释放连接后 FIFO/revision 恢复。另验证重复 operation ID、generation reset、显式 Disable/调度状态 Patch quarantine 和普通/健康 Patch 非 quarantine。默认 3 个 outer rounds；没有依赖时 runner exit 64，普通 `cargo test` 的 skip 不能冒充通过。按用户要求不启动 Docker。

### R4：Redis queue operation 放大

本地纯状态测试创建 500 个 finite guard，要求 0 个 arm renewal；无限等待 guard 必须继续 arm。真实 Redis 程序让一个 finite waiter 保持 22 秒，跨过旧 20 秒 renewal 点，要求 queue ZSET deadline 完全不移动，取消后 lease 和 namespace 可清理。local default、长 override、亚秒舍入、unlimited 与 external 对照均各内部 5 轮；runtime config 在排队中从 1 秒改为 5 秒时，已 admission waiter 必须仍按原 1 秒 deadline 退出。

### R5：冻结候选假上游负载

用假上游持有 100 秒等价慢流，逐级升到 500 并发，另注入 Redis 50/74/75/90/150/500 ms 和 PgSQL writer backpressure。每类至少 5 轮，三次 soak；记录 p50/p95/p99 TTFB、queue wait、first thinking/text、RSS、FD、local/external route、账号状态和恢复时间。不得压测 `127.0.0.1:9022`。

## 性能与恢复验收

- 健康路径不增加 PgSQL 或 Redis round trip；mutation 分类是本地 enum match 和队列扫描，队列仍受 per-credential/total 软预算约束。
- 500 个本地 lease 的确定性测试不得产生 disabled、cooldown 或 external route。
- 500 个有限 queue waiter 不得产生周期 Redis renewal；无限等待仍必须续租，取消/release 后无 queue slot 泄漏。
- Redis/PgSQL 故障恢复后 5/5 正常请求回到本地；pending mutation 最终归零。
- 错误 burst 下内部 inference attempts 和 auxiliary requests 均受共享预算，不随账号数或等待者数增长。
- capacity full、scheduler degraded、runtime persistence quarantined、manual/threshold disabled 必须是不同 reason，不得再用 `local_all_disabled` 覆盖协调故障。
- 公开错误不得暴露 Redis、PgSQL、credential 或 external pool 内部信息；详细原因只进入脱敏 usage/admin 诊断。

## 残余风险

普通 mutation backlog 期间，多实例看到的 failure count 可能暂时不同；FIFO、operation ID 和 generation fencing保证最终一致，但不能把它描述为强同步。显式账号禁用、quota/risk/invalid-grant，以及包含 disabled/reason/generation 的调度状态 patch 仍必须 quarantine。单个请求的 queue deadline 现在在 admission 时冻结，运行时 wait 配置只影响新请求；这是有意的稳定性合同。是否进一步把 scheduler Redis 与 usage Redis、credential runtime PgSQL 与 usage writer pool 做物理隔离，需要由非 Docker 故障程序和冻结候选负载数据决定。

## 当前修复后结果

2026-07-17 的 `runtime-quarantine-focused-r3` 使用 Rust 1.92.0 精确执行 5 个 filter，全部为 `running 1 test / 1 passed`。两条新用例各自内部连续 5 轮：40 账号普通 backlog 保持 40/40 available；Disable/当时被整体分类的 Patch 只 quarantine 2 个账号；global 500 时第 501 个请求进入 bounded queue 并在释放后 1 秒内获批；global unlimited 对照中第 501 个请求在 500 ms 内直接获批。scope `size_kib=1676672`，退出 `removed=true / reservation_released=true`。Patch 的过宽分类由下述 2026-07-18 补充修复取代。

此前 `runtime-quarantine-c0-focused-r1` 和 `runtime-quarantine-focused-r2` 的 filter 路径错误，均为 `running 0 tests`，只算编译/链接证据，不计行为通过。完整命令、计数和未关闭项见 [聚焦证据](../evidence/high-concurrency-low-rpm-runtime-quarantine-20260717.md)。

2026-07-18 的后续审计发现自动刷新成功后的 `refresh_failure_count=0` 也使用 `PendingCredentialRuntimeMutation::Patch`，旧拆分仍会把这种健康 Patch 错判为 quarantine。当前已改为字段级判定：只在 Patch 包含 credential disabled、禁用原因变更或 generation 推进时 quarantine。`runtime-quarantine-patch-semantics-r4` 的字段矩阵、40 账号四类 backlog 和 40x15/global-500 三条精确测试均 `running 1 / passed 1`，各内部 5 轮；scope `2017328 KiB` 并完整回收。

同日继续确认并修复 finite queue renewal amplification。`queue-lease-amplification-r2` 六个精确过滤器实际 6/6 通过；500 finite guard 为 0 renewal-armed，unlimited guard 保留 renewal，local/external TTL 与 40x15/global-500 各内部 5 轮。加入动态配置期限冻结后，`queue-lease-refresh-provider-r1` 的七个精确过滤器实际 7/7 通过：1 秒 admission deadline 在 runtime config 上调为 5 秒后仍连续 5 轮于原期限退出；API/MCP 真实 loopback final-attempt fixture 内部 5 轮严格每路径 inference=1、OAuth refresh=0；`cargo check --all-targets` 无 warning。两个 scope 均 `removed=true / reservation_released=true`，详见 [queue lease/RPM evidence](../evidence/queue-lease-rpm-amplification-20260718.md)。

2026-07-18 追加一轮 development reservation 6 GiB 复测，scope `scheduler-focused-rerun-20260718-dev6g`。六个精确 filter 均为 `running 1 / passed 1`：scheduler degraded fallback toggles、preflight scheduler fallback toggles、non-terminal runtime backlog 不假禁用、finite queue lease policy、queued deadline freeze、40x15/global-500。相关测试内部继续执行 5 轮关键循环。scope 清理 `size_kib=1699916 removed=true reservation_released=true`；随后 rust-analyzer/flycheck 重建的根 `target` 已确认无活跃引用并删除，复核 `target=0 KiB`。该复测仍不替代真实 PgSQL/Redis 动态和冻结负载。

2026-07-19 使用当前仓库专属隔离 PostgreSQL/Redis 执行合批 storage suite。runtime quarantine 部分为 3 个 outer rounds × 6 个 exact filters，即 18 个 exact invocations 全部通过，覆盖真实 PostgreSQL pool pressure 不假 quarantine、pending mutation FIFO/revision replay、generation reset fence、Redis queue deadline 22 秒不移动、coordination degraded waiter fail-closed、cancelled waiter release。scope `storage-suite-real-20260719` 清理 `size_kib=1690164 removed=true reservation_released=true`。完整命令、日志哈希和 artifact gate 见 [2026-07-19 storage evidence](../evidence/storage-integration-and-artifact-gate-20260719.md)。

2026-07-19 r6 candidate 追加冻结假上游 L5 复测：120 秒请求层 921/921 但短 idle RSS gate 未过；第一轮 300 秒仍有 158 个 429；第二轮 300 秒 keep-raw 为 2281/2281、0 个 429、TTFB p95 361 ms、RSS/FD gate 通过，raw log 无 Redis degraded / queue timeout / 429 / `local_all_disabled`，capacity/snapshot/affinity breaker failures 均为 0。但 r6 900 秒 rerun 在约 4.5 分钟复现内部放大：PgSQL usage 慢写与 4.97s rollup SQL 后，Redis affinity/capacity 75 ms breaker 打开，23 次 429，capacity breaker recovery 记录 `suppressed_requests=901086`。这精确命中了“下游 RPM 不高但系统内部 RPM 很高”的问题。

2026-07-19 r7 candidate 将 Redis degraded 等待从普通 capacity-signal wait 改为专用 sleep。真实 Redis focused test 加入 1ms noisy notifier 后通过并断言 suppressed 有界；r7 300 秒 keep-raw 为 2121/2121、0 个 429、TTFB p95 961 ms、RSS/FD gate 通过。raw 中 Redis degraded / capacity timeout / queue timeout / returned 429 / `local_all_disabled` / suppressed 全为 0，breaker failures 全为 0。PgSQL usage 慢写仍出现 4 次、最高 1688 ms，因此 usage writer 性能仍是 open 项，但本轮未再传播成 scheduler storm。

同日 r7 900 秒 rerun 在 2-3 分钟内再次复现红项：22 次 Redis degraded、11 次返回 429、6 次 capacity slot timeout。r8 candidate 随后拆分 capacity/queue 与 affinity timeout：capacity/queue 使用 250ms budget 且连续 timeout 才打开 breaker，affinity/sticky 仍使用独立 75ms budget。r8 focused scheduler tests 通过；r8 L5 300 秒为 2281/2281、0 个 429、TTFB p95 388 ms；r8 L5 900 秒为 6821/6821、0 个 429、TTFB p95 354 ms，capacity breaker `failures=0 / suppressed=0`，RSS/FD gate 通过。affinity breaker 在 900 秒里有 2 次 sticky cleanup 75ms failure 并自愈，没有传播为 capacity degraded。随后 r8 L3 burst/recovery 9/9 与 r8 L4 restart/failure/client-drop/mixed-chaos 12/12 通过。

2026-07-20 又执行正常 usage/scheduler 联合 burst 9 个测量轮：writer throughput `449.03..617.72/s`，writer p99 最大 `31.482ms`，scheduler loaded/recovery p99 最大 `55.785/69.204ms`，RSS 增量小于 9MiB、FD 恒为 15；40 账号 backlog、40x15/global-500、500 finite waiter 三项各 3 outer x 5 internal 通过，0 假禁用。随后用非 Docker loopback chaos proxy 完成 7 scheduler Redis exact tests x 3 outer，覆盖 50/500ms、disconnect/reconnect、指数 breaker、300 lease release、cancel/commit-unknown，21/21 通过且所有错误都不是 `AllDisabled`。详见 [联合压力证据](../evidence/high-concurrency-low-rpm-runtime-quarantine-20260717.md) 和 [Redis chaos](../evidence/scheduler-redis-chaos-nondocker-20260720.md)。

2026-07-21 在 E03 真实双进程 scheduler gate 中追加关闭跨实例 shared RPM fail-open。旧实现的第三次快速跨实例请求可能在另一个进程尚未同步到 Redis selection/rate-limit 前继续 local `200`；新实现通过 Redis Lua `try_record_scheduler_selection()` 同步/原子记录 selection 并设置 `scheduler:rate_limit:<id>` deadline。聚焦测试：

```text
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379/15
RUSTUP_TOOLCHAIN=1.92.0
feature/tests/run-cargo-scoped.sh rpm-reservation-focused-20260721-r4 -- \
  cargo fmt --check
  cargo test storage::redis_cache::tests::redis_scheduler_cooldown_and_rate_limit_round_trip -- --exact
  cargo test kiro::token_manager::manager::tests::redis_backed_rpm_reservation_blocks_third_cross_instance_selection -- --exact
```

结果为两个 exact tests 均 `1 passed`，`cargo fmt --check` 通过，scope `removed=true / reservation_released=true`。随后 `rpm-reservation-check-all-20260721-r4` 的 `cargo check --all-targets` 通过并清理 target。

同日用仓库外冻结候选 `/tmp/kiro-e03-candidate.T2iG7N/kiro-rs`（sha256 `98e0f79328b49925dc940faaa3b1e8b0c8ae8ef7b9975725eb219635c8957ee7`）执行 E03 三轮真实双进程：

```text
runId=e03-20260721013242272-88844-36667d
outerRounds=3
rpm.firstStatuses=[200,200] in every round
rpm.postRestartStatuses=[429,429] in every round
externalHits=0 in every round
disabled=0 in every round
cleanup.redisPrefixKeysRemaining=[]
cleanup.occupiedPorts=[]
```

这个结果说明“账号没禁用但第三次跨实例 RPM 仍打本地”的缺口已关闭；同时候选阶段 RPM-only 等待现在返回 `凭据 RPM 限制`，selectionFailure 会归到 `RpmLimit/RpmLimited`，不再把该场景伪装成 account concurrency 或 all-disabled。

剩余 open 项从“真实存储/单实例 chaos 未执行/真实两进程 scheduler 未执行”收敛为：external takeover 动态服务跑、两实例 usage/fault/external 接管组合、真实上游/Claude CLI 下的长流与工具/search/image/MCP 场景、UI/browser、upgrade 和 final inventory。2026-07-21 external takeover focused handler tests 4/4 与非 Docker runner contract 8/8 已通过，但没有独占空 PostgreSQL database URL，因此不计为动态产品 PASS。

2026-07-22 当前工作树再次复核：

- `runtime-quarantine-storage-20260722-r1` 使用当前项目 loopback PostgreSQL/Redis 执行 3 outer × 6 exact，18/18 通过；覆盖真实 PostgreSQL pool pressure 不假 quarantine、pending mutation FIFO/revision replay、generation reset fence、Redis finite queue deadline、coordination degraded waiter fail-closed 和 cancelled waiter release。scope `size_kib=1711220 removed=true reservation_released=true`。
- `scheduler-redis-chaos-20260722-r1` 使用空 Redis DB7 与非 Docker loopback proxy，3 outer × 8 exact，24/24 通过；覆盖 affinity/capacity 分离、50/500ms latency、连续 timeout breaker、300 lease release、disconnect/reconnect、usage writer + scheduler 联合故障、cancel 和 commit-unknown cleanup；结束 `databaseEmpty=true`。
- `redis-fault-domain-product-20260722-r1` 使用 business Redis `<loopback>:26379/db8` 与 observability Redis `<loopback>:50892/db2`，3 outer × 1 exact × 内部 3 轮通过；证明 usage/observability Redis 故障不影响 business scheduler，business fault fail-closed 且不伪装 `AllDisabled`。
- 当前复核证据集中在 [2026-07-22 回归证据](../evidence/final-regression-rerun-20260722.md)。
