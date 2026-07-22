# External Pool Redis Coordination And Release

Status: `coordinator-focused-pass / eligibility-hot-path-remediation-in-progress / release-candidate-gates-pending`

Severity: P0

## 问题、现象与影响面

external pool 的 Redis 协调同时承担池级/全局并发、cooldown、active lease heartbeat、进程间 fencing 和请求结束后的 lease 释放。旧实现有三类独立风险：

- 单个 pool 的 cooldown key 被写成 Redis list/hash/set 时，批量 snapshot 中的普通 `GET` 返回 `WRONGTYPE`，整批 60 个 pool 全部失败，健康的 59 个 pool 也不可选。
- Redis restart 或数据丢失后，新 manager 只看到空的 in-flight 集合，可能在旧 confirmed lease 的上游响应尚未被 heartbeat fencing 前取得第二个 lease，造成同一容量被两个上游请求同时占用。
- lease Drop 先进入容量 256 的全局 critical storage queue，拒绝后再进入容量 64 的 fallback task semaphore。10,000 个同时 Drop 最多约 320 个能立即登记，其余只能等待 Redis TTL，生产默认最长可能约 720 秒，恢复期会出现大面积假满。

这些问题与本地账号 scheduler 的 75ms Redis degraded/fallback 问题不同，但共享同一个 Redis 单线程和连接资源时会互相放大。external release 堵塞不会增加真实下游 RPM，却会保留虚假的 in-flight 容量；coordinator fail-closed 又会让请求等待、fallback 或快速失败。

## 根因与代码链

1. `src/storage/redis_cache.rs` 的 external snapshot Lua 原先把所有 cooldown `GET` 视为同一批事务的硬成功条件，没有 pool 级错误隔离。
2. Redis 中的 lease 集合本身不能证明 Redis 进程世代。restart/data loss 后，空集合既可能表示没有请求，也可能表示协调状态丢失；旧 lease 仍可能在另一个进程继续输出。
3. 旧 release 路径以“每个 Drop 一个异步 task”为模型，task admission 与真实已取得 lease 的数量没有一一预留关系。队列满时不能在 Drop 中阻塞，因此只能丢弃 intent 并依赖 TTL。
4. Redis `INFO server` 的 `run_id` 可以标识进程世代，但如果放入每个 snapshot/acquire Lua，会给所有正常请求增加昂贵操作。独立微基准中 guard-only 约 131k-168k rps，`INFO + guard` 约 77k-101k rps，吞吐下降约 35%-41%。
5. PostgreSQL `runtime_config` 中的 epoch row 和 advisory lock 是 authority。它只能协调共享同一 PostgreSQL database/schema 的实例；不同 PG authority 若写同一 Redis key prefix，会形成 split authority 并轮流旋转 guard。

主要实现位置：

- `src/external_pool.rs`：epoch reconciliation、5 秒 run-id probe、35 秒 recovery barrier、heartbeat fencing、release reservation/dispatcher 和 shutdown drain API。
- `src/storage/redis_cache.rs`：standalone Redis guard、snapshot/acquire/touch Lua、pool 级 WRONGTYPE 隔离和批量 release Lua。
- `src/main.rs`：HTTP 停止接收并排空后，将 external release intent 纳入后台 10 秒 drain 阶段。

## 稳定复现

### WRONGTYPE 批次污染

创建 60 个 external pool，只将一个 pool 的 cooldown key 依次写成非法 JSON string、list、hash、set。每种类型连续选择和 status 5 轮。修复前 list 首轮即可让整批不可用；修复后异常 pool 必须 `coordinator_state_invalid`，其余 59 个持续 dispatchable。

### Restart 与 active lease 竞争

在专用 Redis 上取得 confirmed lease，保持 heartbeat 活跃，然后 `docker kill --signal KILL` 并启动同一容器，删除 in-flight keys 模拟数据丢失。使用同一 PG/prefix 的 fresh manager 在旧 heartbeat fencing 前尝试 acquire。修复前可以取得第二个 lease；修复后 recovery barrier 内必须返回 `coordinator_restart_recovery`，旧 epoch heartbeat 必须先 lost，barrier 结束后才可取得新 lease。

变体至少覆盖：无 active lease、1 个 active、同 manager 2 个加 peer manager 2 个、连续 restart 5 轮、普通 `reset_peer` 但 Redis 未 restart、clean startup。

### 10k release backlog

在 Redis 中真实写入 10,000 个 confirmed lease，给每个 lease 预留 release permit。通过 Toxiproxy downstream `reset_peer` 制造 commit-unknown，再同时 Drop 10,000 个 lease。故障期显式 drain 必须超时但保留全部 intent；移除 toxic 后要求单 worker 分批重试，最终 dispatcher pending、pool/global in-flight 和 confirmed tombstone 全部为 0。

可重复命令和隔离端口见 [external pool Redis 协调证据](../evidence/external-pool-redis-coordination-release-20260716.md) 与 [执行矩阵](../tests/external-pool-redis-coordination-matrix-20260716.md)。

## 方案比较

| 方案 | 优点 | 限制或拒绝原因 |
| --- | --- | --- |
| 扩大 critical/fallback task queue | 改动小 | 仍是每 lease 一个 task；任意固定 queue 都会在更大 burst 丢 intent，不建立 acquire/release 容量不变量 |
| 每个热路径执行 `INFO server` | restart 检测直接 | 微基准吞吐下降 35%-41%，不可接受 |
| Redis 异常时继续使用本地内存容量 | 可用性表面更高 | 多实例会超卖；明确拒绝 fail-open |
| 低频 run-id probe + PG epoch + recovery barrier | 正常热路径只保留 snapshot/acquire RTT，restart 可 fencing | 需要共享 PG authority；存在最多约 5 秒低频探测窗口 |
| 单 worker + 去重 map + 批量 Lua + acquire-time permit | Drop O(1)，task 数固定，commit-unknown 幂等，队列容量与已获 lease 一一对应 | 需要显式 hard cap 和 shutdown drain；进程崩溃仍只能由 TTL 回收 |

## 已实现修复与优化

- snapshot Lua 对每个 cooldown 使用 `redis.pcall`；WRONGTYPE 只产生该 pool 的 invalid sentinel，不再终止整批。
- PostgreSQL 使用独立 row `external_pool_redis_coordination_epoch` 保存 `redisRunId` 与 `coordinationEpoch`，并用 advisory lock 串行化同 authority 的 reconciliation。
- clean first startup 直接安装 epoch，`recovery_grace=0`；已有 authority 检测到 run-id/guard 不一致时旋转 epoch，并用 Redis `TIME + PEXPIREAT` 安装生产默认 35 秒 barrier。
- snapshot/acquire/touch Lua 只读 epoch 和 recovery key，不执行 `INFO`。每个 manager 以 5 秒间隔单飞 probe `INFO server/cluster`；任一 coordinator Redis error 会要求下一次先 probe。
- confirmed lease 保存 acquisition epoch；heartbeat touch 同时检查 manager 当前 epoch和 Redis guard。epoch 旋转后旧 heartbeat 永久返回 false，不能在 barrier 后复活。
- release dispatcher 采用每 manager 最多一个运行 worker、HashMap O(1) 去重、每批 256 个 intent 的 Lua。单项 Redis 类型错误返回逐 intent status，健康 intent 可继续完成。
- 固定 release capacity 为 65,536。每次 Redis acquire 前必须先取得 `OwnedSemaphorePermit`；permit 随 pending/confirmed lease 和 release intent 一直保留到批处理明确完成。容量耗尽时在访问 Redis 前 fail closed 为 `release_backlog_saturated`，不存在“queue 满后静默丢弃”。
- commit-unknown 重试即使得到 `removed=false`，只要 Lua 明确完成，也会清除 intent 并归还 permit。
- 主关闭流程复用现有剩余 10 秒后台 drain 预算等待 dispatcher idle，并记录 pending/enqueued/completed/retries/worker starts/spawn failures。

## 修复后验证与证据

- WRONGTYPE：非法 JSON/list/hash/set，各 5 轮，59 个健康 pool 全部保持可选。
- clean startup：5 个独立 schema/prefix，5/5 无 35 秒 barrier；peer manager 复用同 epoch。
- 普通断连：`reset_peer` 5/5 fail closed/recover，run-id、epoch、PG authority version 不变，无 recovery barrier。
- restart：无 active lease 当前最终 5/5；单 active fresh-manager fencing 5/5；同/跨 manager 共 4 个 active heartbeat fencing 5/5。
- release：10,000 个真实 lease x 5 轮全部恢复；每轮一个 worker，Drop 11-18ms，恢复 1.406-1.549s，最终 pending/pool/global/tombstone 为 0。
- Redis 原语回归：atomic acquire、commit-unknown tombstone、10k confirmed release、pending tombstone TTL、queue lease 与双 manager/60 pool 竞争共 10/10。
- 21-cell：Redis 延迟 `0/50/74/75/90/150/500ms` x 并发 `64/16/1`，每格 5 manager、60 pool、200 warmup、1,000 measured、5 recovery；全部 1,000/1,000、recovery 5/5、admission rejection 0。

完整数字、首轮红灯和构建身份见证据文档。当前结论来自 HEAD `401473ca1649` 上的并行 dirty worktree/debug test binary，不是冻结 release candidate，也没有触碰 `127.0.0.1:9022` 或真实 credential。

## 2026-07-17 静态资格与 fallback 热路径复核

本轮继续复核了“健康本地请求为什么仍产生 external 内部 RPM”以及“本地失败后 external 为什么仍可能接不住”。当前 dirty tree 的第一版优化把完整 external pool 列表按 generation 缓存 5 秒，并把 model/body mode 在内存过滤；它能把健康本地 eligibility 的 Redis RTT 降为 0，也能把同一 generation 的 PostgreSQL list 查询合并为 1 次。但独立审查确认该版本尚不能作为完成态：

- TTL 到期后一个请求持全局 async refresh lock 跨 PostgreSQL await，其余请求全部排队。慢查询或表锁会让健康本地请求每 5 秒形成一次 head-of-line blocking，消除 RPM 扇出却引入整波尾延迟。
- PostgreSQL 错误被转换成空列表并按正常 5 秒 TTL 缓存，调用方无法区分“确认没有 pool”和“资格状态未知”。这会在故障恢复后继续短时禁止 fallback。
- local attempt、local preflight 和 local-error fallback 的 eligibility 必须使用与最终 route 完全相同的 model/body-mode query。normalized 请求只存在 raw pool 时，model-only hint 会错误启用 fail-fast，真实 selection 随后却没有候选。
- 静态 cache 只能作为 hint。真实 external dispatch 仍必须使用 fresh PostgreSQL selection snapshot、Redis runtime snapshot 和 atomic lease，Redis/PG 异常继续 fail closed；不得用 stale pool/body/key 直接授权发送。
- 真实 selection 自身仍直接 await PostgreSQL list 且无显式 timeout。错误 burst 会继续形成每请求 PostgreSQL 查询和无界等待；它与 eligibility SWR 是两个独立问题。
- cross-instance Admin/auto-disable 目前没有 pool revision/epoch 参与 atomic acquire。TTL 内的 stale hint 通常会被 fresh selection 拦住，但 selection 后到 HTTP send 前发生 disable/key/URL 更新时，旧 pool record 仍存在一次 TOCTOU 风险。该问题需独立的 revision authority 或发送前 revision gate，不能由 TTL cache 宣称关闭。

选定的当前修复方向是：fresh 5 秒、同 generation last-good stale 最多 30 秒、单个 500ms bounded background refresh、失败后 1 秒重试；cold start 或 generation mismatch 禁止读 stale，短超时后 fail closed。真实 PostgreSQL 表锁下执行 `c32/c128 x 3`，要求 stale wave 全部低延迟、每 generation PG load=1、eligibility Redis RTT=0；invalidation wave 不得返回旧 generation，解锁后必须恢复。authoritative selection 另加有界 timeout 和结构化 coordinator-unavailable 证据。上述实现与动态门禁完成前，本专题继续阻断发布。

## 性能风险、边界、回滚与残余问题

- 支持拓扑是 standalone Redis、所有协调同一 external namespace 的实例共享同一 PostgreSQL database/schema authority 和同一 Redis key prefix。Redis Cluster/CROSSSLOT 未支持、未验证。
- 不同 PG authority 使用同一 Redis prefix 是已观察到的错误拓扑：隔离 runner 出现约 34 秒 recovering。需要启动配置校验，或设计 Redis CAS authority 后才可声明支持；当前应改为共享 PG 或不同 Redis prefix。
- 35 秒来自最大 heartbeat interval 30 秒、Redis operation timeout 2 秒和 margin，不等于 360 秒 lease max age。进程或 runtime stop-the-world 超过 margin 时不是绝对 fencing 保证。
- 如果共用 connection 上的其他操作先完成 reconnect，run-id 最迟依赖 5 秒低频 probe；当前不能承诺 restart 后绝对零探测窗口。
- 10k 测试 RSS 从 21.8MiB 到峰值约 43.0MiB、结束约 44.1MiB；FD 20 到 21。21-cell RSS 24.3MiB 到峰值55.8MiB、结束29.5MiB；FD 44 到45。资源有界，但 allocator RSS 不保证立即回到初值。
- debug 21-cell 在 0ms/聚合并发320 的 p95 为 1.232s，0ms/聚合并发5 的 p95 为 28.256ms；这证明正确性和有界性，不满足最终 release 性能结论。必须用冻结 release binary 建立同机基线，解释高并发下 PG pool listing/Redis serialization，并执行 L3/L5。
- 正常进程 shutdown 会显式 drain；进程崩溃、runtime 已不可用或 Redis 故障超过 shutdown budget 时，残余 lease 仍依赖 Redis TTL。不能承诺 crash 后即时释放。
- dispatch queue lease 仍使用独立的 critical/fallback cleanup 与 60 秒 TTL，本批 dispatcher 只覆盖 external pool in-flight lease；queue Drop storm 需要单独评审。
- 回滚 dispatcher 会重新引入 256+64 admission 丢失，不是安全回滚。紧急回滚只能关闭 external pool 或降低并发，并保留 fail-closed；不得恢复本地内存 fail-open。

发布验收仍需：冻结 commit/release binary SHA、完整 `cargo test --all-targets`、release-mode 21-cell 对照、双实例共享 authority soak、Redis/PG/usage cleanup 联合 chaos、graceful shutdown 与强制 crash 对照、Redis namespace/PG row 启动配置校验。
