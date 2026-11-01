# 有可用账号仍进入调度队列：线上调查与代码修复方案

日期：2026-11-01
范围：`/root/docker-compose/kiro-rs-2ue-59137` 线上实例；源码：`/root/code/kiro.rs`
状态：线上故障已复发（Redis 调度超时、调度队列满、/readyz 超时）；本地已完成 Redis degraded 与错误排队路径修复，并加入隔离 Redis 延迟回归测试；远端未修改

## 结论

这次现象不是账号池真的没有可用账号。请求的本地账号快照仍显示有数十个可调度账号，但调度请求在确认 Redis 并发租约、记录 Redis 调度选择等协调操作时超过热路径 deadline，触发 scheduler breaker。调度器无法确认跨实例容量租约后，普通请求进入等待/重试，最后表现为账号并发明明足够却排队、队列满或返回 429。

临时把调度等待从几十/几百秒收紧到 5 秒、缩小队列并重启，清除了旧请求和进程内熔断状态。随后 `/healthz`、`/readyz` 恢复，队列保持为 0，业务仍持续拿到 Kiro 上游 200 响应。**这证明临时处理能恢复服务，但不能证明 Redis 热路径已经根治**：重启后的最初约一分钟内仍出现过 Redis affinity、capacity 和 snapshot deadline 超时，之后才停止复发。

可以确定的直接根因是“Redis 调度协调操作未能在应用设置的 deadline 内完成，且请求层把协调失败转成等待”。无法仅凭本次证据断言 Redis 服务端是最终根因。Redis `PING` 正常、无 slowlog、客户端数少且无连接拒绝；故障窗口同时有高应用 CPU/内存和 PostgreSQL usage 写入变慢的迹象。最终触发因素还可能是应用 runtime/CPU 调度延迟、Redis 客户端等待、网络抖动或共享存储/连接池争用，需要补充分段延迟指标后才能定论。

## 用户可见问题

- 账号没有耗尽，调用仍提示排队、等待超时或调度队列已满。
- 有些请求等到数分钟后才返回 429；大量等待请求又占满 dispatch queue，进一步影响新请求。
- Docker 显示容器 `healthy`，但应用 HTTP 健康检查曾经超时。
- 重启后服务短暂恢复，说明进程内等待/熔断/请求堆积被清空；若下游延迟再出现，问题可能复发。

## 现场证据

### 重启前

- 应用日志反复出现：
  - `读取 Redis 会话绑定超过共享总期限 75ms`；
  - `占用 Redis 凭据并发槽超过共享总期限 250ms`；
  - `原子记录 Redis 调度选中次数超过共享总期限 250ms`；
  - 后台 scheduler snapshot 超过 500ms；随后打开 capacity/affinity breaker。
- 同一时间日志明确报出本地池 `local_total=64`，`local_available=48`，`local_dispatchable` 仍在 31–48 之间，但请求因 `Redis 调度协调状态不可用` 等待。这是“账号存在且本地看到容量，但无法完成权威 Redis 协调”的直接证据。
- 出现 `queued=200, max_queued_requests=200, global_max_concurrent_requests=1000`，以及等待约 300 秒后超时的请求。
- 故障前查询到的 runtime config（保存在 PostgreSQL）为：

  | 配置 | 故障前值 |
  | --- | ---: |
  | `credentialMaxConcurrentRequests` | 30 |
  | `credentialRpm` | 200 |
  | `dispatchGlobalMaxConcurrentRequests` | 1000 |
  | `dispatchMaxQueuedRequests` | 200 |
  | `credentialDispatchMaxWaitSecs` | 60 |
  | `externalPools.externalPoolLocalRescueMaxWaitSecs` | 300 |
  | `externalPools.externalPoolMaxQueuedRequests` | 300 |

- 日志出现 `waited_secs≈300, max_wait_secs=299`。故障快照中 `externalPoolLocalRescueMaxWaitSecs=300` 是相关的长等待配置，但不能据此断定这条错误一定由该字段单独控制：`credentialDispatchMaxWaitSecs`、`externalPoolDispatchMaxWaitSecs`、请求是否处于 external local-rescue 路径，以及调用时加载的 runtime config 都会影响实际 acquire deadline。线上应同时记录请求 route/route subtype 和 acquire mode 才能精确归因。故障前 file config 与数据库 runtime config 数值并不一致；线上有效运行值应以 runtime config 为准，单改挂载 JSON 不能可靠地改变当前运行设置。
- 故障窗口一次采样中应用容器使用约 5.4 CPU 核、RSS 约 7.6 GiB；宿主机 8 vCPU，load average 约 11.8。`/healthz` 和 `/readyz` 3 秒内都没有响应，但 Docker 健康检查仍是 TCP `nc -z`，所以显示 healthy。
- Redis 当时 `PING` 为 `PONG`，没有连接拒绝；后续检查 `INFO` 显示客户端数很少、无 blocked clients、slowlog 为 0。它排除了“Redis 完全不可达”，但不能排除应用侧 Redis 请求排队或瞬时 deadline 超时。
- PostgreSQL 的 `usage_records` 表约 4.7 GiB。日志曾记录小批量 usage 写入耗时约 3.3 秒、4.9 秒，以及其他数百毫秒写入。它是存储压力/运行时抖动的佐证和可能的放大器，但没有证据证明它单独导致 Redis scheduler timeout。

### 临时处理与重启后监控

2026-11-01 14:28:13 UTC 重启应用前，更新 PostgreSQL runtime config（版本更新到 680），再重启应用：

| 配置 | 处理前 | 临时值 |
| --- | ---: | ---: |
| `credentialDispatchMaxWaitSecs` | 60 | 5 |
| `dispatchMaxQueuedRequests` | 200 | 50 |
| `externalPools.externalPoolLocalRescueMaxWaitSecs` | 300 | 5 |
| `externalPools.externalPoolMaxQueuedRequests` | 300 | 100 |

观察至 2026-11-01 14:35:54 UTC（重启后约 7 分 41 秒）：

- `/healthz` 返回 200，约 11ms；`/readyz` 返回 ready，约 10ms。
- 最后六次间隔 10 秒的 `/readyz` 检查均为 ready，报告 `dispatchProgressing=true`、`queuedRequests=0`、`secsSinceLastDispatch=0`；观测的 dispatchRatePerSec 约 19.5–39/s。
- 重启后约 5 分钟，应用未再次重启。流量持续进入 Kiro 上游并收到 `response_headers_received ... (200)`；最后 60 秒采样到 184 条成功收到上游响应头的调用链、4 条失败调用链。该统计不能替代完整成功率/usage 分析，但确认请求在持续工作。
- 重启后的最初约一分钟内，仍看到 affinity 75ms、capacity 250ms、snapshot 500ms 超时和两条快速调度等待超时；截至 14:35:54 UTC 最近 60 秒未再看到 scheduler timeout、queue full 或 PG usage 慢写日志。
- 应用 RSS 在稳定后约 1.1–1.3 GiB，CPU 样本约 12–45%，期间有瞬时高峰。相较重启前 7.6 GiB / 538% CPU，进程资源压力明显下降。
- Redis 最终 `PING=PONG`、客户端 6、blocked/rejected 为 0、slowlog 0。当前没有证据表明 Redis server 自身持续高负载。
- 运行容器健康检查仍是 `nc -z 127.0.0.1 8990`，不能检查 handler 是否能及时处理 HTTP。源码仓库的 deploy compose 已有 `/readyz` 检查，但这台线上容器实际采用的 compose 配置仍是 TCP 检查。

### 后续监控：故障复发

2026-11-01 14:38–14:39 UTC 再次只读检查线上实例，发现前述“重启后暂时恢复”没有持续：

- 容器仍为 running、Docker health 为 `healthy`、重启次数为 0；但宿主机映射端口实际是 `59137`（不是容器端口 `8990`）。
- Redis scheduler 日志再次出现 75ms affinity、250ms capacity 和 500ms snapshot deadline 超时。capacity breaker 样例记录到连续失败 5 次、抑制 119 个请求；本地快照仍为 64 个账号、44 个 available、42–44 个 dispatchable。
- 日志显示有可用本地账号时仍大量转向 external fallback/rescue；随后 dispatch queue 达到临时上限 `queued=50, max_queued_requests=50`，多个请求返回队列已满 429，`Retry-After=60`。最近两分钟对该错误文本的日志匹配有 1,312 行；同一失败可能同时记录“请求失败”和“返回 429”，因此不能把该行数直接当作独立请求数。
- 14:39 UTC 的 `/healthz` 返回 200，但耗时约 2.20 秒；紧接着 `/readyz` 在 5 秒客户端 deadline 内没有返回任何响应。同期采样容器 CPU 约 664%（6.6 核），RSS 约 2.3 GiB。Docker `healthy` 仍只代表 TCP 端口可连接。
- 同一时间窗口仍可见 PostgreSQL 查询慢语句告警（外部池查询约 1.8 秒、维护任务查询约 2.0 秒）。这与应用负载/存储压力同时出现，但仍不能单独证明其导致 Redis scheduler timeout。

**更新后的运行结论**：缩短等待和队列仅限制了故障放大规模，没有消除 Redis 协调 deadline 超时；重启清除了当时的进程状态并短暂降低资源使用，之后问题随负载再次出现。故障已再次造成调度队列满和 429，不能再将“线上已恢复”作为当前结论。应以 HTTP `/readyz` 能在短 deadline 内返回、scheduler deadline 不再连续超时、可用账号不因 Redis degraded 被挡住、queue 不持续增长作为恢复判据。

14:43 UTC 再次抽样：`/readyz` 仍在 5 秒内无响应；最近 60 秒有 8 条 Redis scheduler breaker 日志和约 1,120 行队列满相关日志（日志会对同一请求重复记录，行数不等于独立 429 请求数）。应用 CPU 约 638%、RSS 约 4.38 GiB，Docker 仍显示 `healthy` 且重启次数为 0。与 14:39 UTC 样本相比，队列错误持续且 RSS 继续上升，说明这不是瞬时恢复过程。

### 用户再次报告“服务又死了”后的现场与临时恢复

2026-11-01 约 14:50 UTC 用户再次报告服务不可用。只读检查和恢复记录如下：

- 容器不是 exited，也未 OOM：状态仍 `running`、`OOMKilled=false`、Docker health `healthy`、重启计数 0。宿主机 `/healthz` 和 `/readyz` 均连续 3 秒无响应；这与 TCP healthcheck 仍绿再次不符。
- 最近日志继续出现 `Redis 调度协调状态不可用` 等待超时（日志样例 `waited_secs=14/24, max_wait_secs=4`）、队列达到 50 后返回 429。Redis scheduler deadline/breaker 仍有告警；另出现 critical storage task 5 秒超时。
- 故障时应用 CPU 约 537%（5.4 核），RSS 约 6.5 GiB；宿主机可用内存约 6.3 GiB、swap 已用约 955 MiB。PostgreSQL `pg_isready` 报告 accepting connections。Redis CLI 未提供认证参数，返回 `NOAUTH`，因此本次不能据此评价 Redis 命令健康。
- 在确认没有重启数据库/Redis后，仅重启应用容器 `kiro-rs-2ue-59137-app`。启动后 HTTP `/healthz`、`/readyz` 恢复至约 12–15ms，ready 检查中 PostgreSQL、Redis、Redis runtime events 均为 true，dispatch queue 为 0；调度随后恢复推进。
- 重启后 CPU/RSS 一度降至约 2.5%/30 MiB；约一分钟后的采样为 CPU 292%、RSS 1.49 GiB，`/readyz` 仍返回 200（约 199ms），`dispatchRatePerSec≈16.6`、`queuedRequests=0`。
- 但启动约 5 秒后日志已再次出现 Redis 后台 snapshot 500ms deadline 和 affinity 75ms deadline 超时。随后的最近 60 秒暂未见 queue-full 日志，但观察时间不足，不能判定根因已解决或稳定恢复。

**本次处置结论**：应用进程在高 CPU/RSS 和 Redis 调度错误、队列积压下仍存活，但 HTTP handler 已无响应；单独重启应用能清空进程内积压并短暂恢复 HTTP/dispatch。Redis deadline 超时在新进程启动数秒后复现，说明重启是应急恢复手段，不是修复。恢复判定需要继续观察 `/readyz` 延迟、dispatch rate/queue、CPU/RSS 和 Redis deadline 错误，且 Docker TCP health 不能作为唯一依据。

## 根因分析与确定性

### 已确认的故障机制

1. 本地选号快照发现账号有容量，但有 Redis 配置时，调度器还要确认 Redis in-flight lease；启用 `credentialRpm` 时还会执行 Redis 选择/RPM 原子操作。
2. Redis affinity/scheduler 操作分别使用 75ms、250ms、500ms deadline。操作超时会进入 breaker/degraded 分支。
3. 在普通等待模式，Redis协调失败会进入本地有界队列并等恢复/重试；若调用链有更长的 local rescue deadline，等待可能接近 300 秒。等待请求累积到 200 后，新请求看到 queue full。
4. 因此，“dispatchable > 0”并不代表本次请求已经拿到权威租约。这个等待是控制面协调失败，不是凭据并发槽确实用完。

### 根因判断

- **直接根因（确定）**：Redis scheduler 热路径未能在设定 deadline 内成功，应用将控制面故障转成排队/重试；等待 deadline 过长使单次故障扩大成 300 秒请求堆积。
- **触发因素（高概率但尚未定论）**：故障窗口应用 CPU/RSS 显著上升、PostgreSQL usage 写入变慢；这些压力可能让 Tokio 执行、连接池/Redis 客户端调度或进程资源出现尾延迟。重启后旧队列被清除，资源恢复，热路径超时短暂出现后停止。
- **未确认**：Redis server 命令执行慢、Redis 网络慢、redis-rs 客户端连接池/Multiplexed connection 排队、进程 runtime 被 CPU/同步工作拖慢，或 PgSQL/usage writer 共享资源争用，哪个是尾延迟的最终来源。本次 Redis slowlog 为 0、PING 正常，不能把根因写成“Redis 服务挂了”。
- **确定的放大器**：`externalPoolLocalRescueMaxWaitSecs=300`、dispatch queue 200、global dispatch 1000，以及 TCP-only healthcheck。增加账号并发上限或把队列再放大，不能修复 Redis 协调超时，还可能提高资源峰值。

## 已实施的本地代码修复

本次修复只修改本地仓库，未修改现场机器、远端代码、Redis、PostgreSQL 或任何远端服务。

### 1. Redis 协调失败不再伪装成容量满

`prepare_in_flight_slot` 原来用 `Option<PreparedInFlightSlot>` 表示结果。`None` 同时表示真实本地槽位已满、release reconciliation 许可不足、以及内部准入失败；调度主循环看到 `None` 后会把请求放入 dispatch queue。高并发 Redis 延迟时，这会产生“账号快照仍有空槽，但请求排队”的现象。

现在使用明确的结果分类：

- `Ready`：已完成本地 provisional reservation，可以继续确认 Redis lease；
- `CapacityFull`：只有真实本地/全局容量竞争才允许走容量队列；
- `RedisReleaseAdmissionUnavailable`：release reconciliation 无法为这次 Redis lease 保留回收名额，直接返回 `CapacityUnavailable`，不进入容量队列。

### 2. Redis degraded 等待与容量队列分离

- Redis hot operation 失败会唤醒已经等待的调度请求，让它们重新观察 coordinator 状态；
- Redis 后台 snapshot 准入/执行失败也会发出 capacity signal，避免请求继续睡在旧快照造成的普通容量队列里；
- 一旦 capacity 或 scheduler snapshot breaker 进入 degraded，普通请求丢弃已有 queue guard，只给 Redis 独立的最多 250ms 恢复窗口；
- Redis degraded 不再使用普通的几十秒/几百秒 dispatch deadline，也不创建 local-only queue lease；窗口结束返回带 Redis coordination 原因的 `CapacityUnavailable`。

这样保留了 Redis 作为跨实例并发权威，避免在多实例场景下 fail-open；同时不会把 Redis 控制面故障扩大成“账号容量耗尽”的长队列。

### 3. 回归测试与本地 mock 复现

新增 `redis_capacity_latency_with_idle_credentials_does_not_enter_dispatch_queue`：

- 64 个兼容账号，每账号并发上限 30，全局上限 1000；
- 128 个并发请求同时进入；
- 通过本机隔离 Redis + TCP 延迟代理注入 500ms response latency，超过 Redis capacity hot-path 250ms deadline；
- 断言所有失败均归类为 Redis coordination unavailable；
- 断言 dispatch queue 峰值为 0、本地 `queued_requests` 最终为 0、所有 provisional local slots 均回滚。

该测试已在本地隔离 Redis 上通过（128 并发、64 个空闲账号、500ms Redis 延迟，耗时约 7.6 秒）。测试不连接 `152.53.243.159`，也不使用生产 Redis 或真实账号。

## 后续代码修复方案

目标是：**本地还有容量时，Redis 临时抖动不应让单实例请求一直排队；多实例情况下，不能为了可用性而破坏跨实例并发上限。** 不应承诺在真实容量饱和或上游不可用时“永远不排队/永远成功”。

### P0：明确 Redis degraded 时的调度策略

增加显式调度故障模式，例如 `schedulerRedisDegradedMode`：

- `single_instance_local`：仅在明确保证单实例时启用。Redis breaker 打开后，原子地切到进程内 per-credential/global semaphore 和本地 RPM bucket；已有本地可用容量就直接派发，不进入 Redis degraded wait queue。Redis 状态恢复时使用 generation/fencing token、幂等 lease id 和有序 reconcile，避免旧的 commit-unknown lease 或重复释放影响新 lease。
- `multi_instance_fail_fast`：多副本保持全局并发正确性。Redis 权威协调不可用时，不假装本地容量就是集群容量；停止长排队，在很短的 grace 后走已配置的 external fallback，否则返回带 `Retry-After` 的 503/429。恢复后由 breaker probe 重新开放派发。
- 不根据 Redis 心跳/节点记录“猜测目前只有一个实例”：协调服务不可用时，无法安全证明其他节点不存在。`single_instance_local` 必须由部署明确配置并在启动校验副本数/运营声明。

该策略直接处理本次需求：单实例且账号容量足够时，Redis 超时不再阻断本地推理；多实例仍保留严格的集群并发限制。

### P0：只对真实容量不足排队

- 将等待原因严格拆开：`credential_capacity_full`、`global_capacity_full`、`rpm_limited`、`scheduler_redis_degraded`、`affinity_unavailable`。
- 只有真实账号/global capacity full 或明确 RPM 窗口才进入 capacity queue；Redis command timeout、breaker open、连接池等待不得伪装成容量满并加入长队列。
- 对 Redis degraded 等待设置独立 deadline（建议几百毫秒级），到期立即执行上述单实例本地模式或多实例 fail-fast/fallback。`credentialDispatchMaxWaitSecs` 不应覆盖控制面故障。
- 队列保留硬上限和短 deadline。队列满快速返回明确 Retry-After，不允许 0/无限制值形成无界请求堆积。

### P1：缩小请求关键路径的 Redis 操作和故障域

- Sticky/session affinity 是调度偏好，不应成为可用账号派发的硬门槛。Redis affinity timeout 时优先使用本地 sticky cache；cache miss/degraded 时直接按普通策略选另一个 dispatchable 凭据。
- 盘点一次成功派发需要的 Redis round trip。并发租约与 RPM reservation 的正确性操作保留原子性；选择次数/健康统计/非关键快照合并或后台批量写，避免每个请求多次独立等待 Redis。
- 若 scheduler Redis 当前还承载 token refresh、runtime event 或其他热工作负载，拆分为物理 Redis 故障域；不同 DB/key prefix 不算隔离。配置校验应比较 Redis server `run_id`，拒绝同一 server 冒充独立故障域。
- 记录 Redis pool wait、写入请求到达 Redis 的时间、命令执行 RTT、返回时间。不能只用 `PING`/slowlog 诊断整个调用链。

### P1：隔离 PostgreSQL usage writer

- usage_records 约 4.7 GiB，故障前后存在小批量写入数秒的记录。usage 写入必须继续异步、有界、主请求不等待；为 usage writer 使用独立有界连接池/资源预算，避免长事务或大表写入挤占 credential/runtime critical storage pool。
- 监控 usage queue depth/drop count、PG pool acquire wait、transaction duration、锁等待和 WAL/fsync latency。大表索引/维护在低峰执行，不在请求故障处理中在线做大规模改表。
- 这项是隔离风险，不是已证实的唯一根因；不能把写入变慢直接等同 Redis timeout 的原因。

### P1：准确反映服务状态

- 线上 healthcheck 改为 HTTP `/readyz` 或轻量 `/healthz`，不要仅检查 TCP 端口。健康探针必须有短 timeout；readyz 显示最近派发停止或 scheduler breaker 持续打开时应变为 not-ready。
- `/readyz` 分别报告 PostgreSQL、Redis、Redis runtime event、dispatch progress 和 queue depth；Redis 不可用但单实例 local mode 正常工作时，不应仅因 Redis down 而错误下线所有业务。

### P2：补齐运行指标和日志

增加低基数指标：

- `scheduler_dispatch_wait_seconds{reason}` histogram；
- `scheduler_redis_operation_seconds{operation,outcome}` histogram；
- Redis admission semaphore wait 与 Redis command RTT 分开计时；
- breaker state/duration/open count、commit-unknown count；
- dispatch queue depth/reject count、可用/dispatchable credential count、local/global in-flight 数；
- local fallback mode 当前状态及进入/退出次数；
- usage writer queue depth/drop count、PG pool wait。

日志按请求记录 `dispatchWaitReason`、`redisOperation`、`queueWaitMs`、`available/dispatchable`、`breakerGeneration`，避免只记录最终“调度不可用”。不要记录 API key、token、prompt 或完整请求体。

## 验收标准

1. **单实例、账号有容量、Redis 注入 250ms/500ms 延迟**：进入 local degraded mode 后，账号仍可派发；请求不进入长队列；本地每账号/global concurrency 和 RPM 仍有效。
2. **单实例、Redis 完全断连**：现有本地容量继续服务；readyz 反映降级状态；恢复 Redis 后有 fencing/reconcile，不出现永久占槽或双重释放。
3. **多实例、Redis 断连/延迟/commit unknown**：不走进程本地 fail-open；请求在短 deadline 内 external fallback 或 fail-fast，跨实例并发上限不被突破。
4. **真实容量满**：只在该场景排队；queue 有上限和 deadline；槽释放后等待者公平唤醒。
5. **affinity Redis 超时**：账号有容量时请求可改为非 sticky 派发；不能因为会话绑定不可读而等待 75ms 后继续无效重试。
6. **PG usage 延迟和大表压力**：usage queue 有界，业务 scheduler/health 仍正常，usage writer 失败计数可见。
7. 长流并发 + 账号池 64 + Redis/PG latency chaos 持续至少 30 分钟；`/healthz`/`/readyz` p99 < 100ms、无持续 queue growth、无 false all-disabled、无跨实例并发超发；记录调度延迟直方图和上游成功率。

## 本次临时配置的回滚信息

以下是现场记录中的临时 PostgreSQL runtime config 变更（不是本次本地代码修改；本次没有写远端配置），不涉及账号和凭据：

- `credentialDispatchMaxWaitSecs: 5 -> 60`
- `dispatchMaxQueuedRequests: 50 -> 200`
- `externalPools.externalPoolLocalRescueMaxWaitSecs: 5 -> 300`
- `externalPools.externalPoolMaxQueuedRequests: 100 -> 300`

如果要恢复原行为，在管理后台 runtime config 中恢复以上值；数据库是 runtime authority，不能只改容器挂载的 `config/config.json`。当前配置版本为 680。临时等待值降低后，Redis 再次故障时请求会更快 fallback 或返回临时错误，这是为了阻止分钟级请求堆积；它不会让 Redis 操作本身变快。

## 复现所需的最小配置

以下配置是**非生产隔离环境**中的复现起点，不是生产推荐值。它只包含调度机制相关字段；账号 token、API key 和 Redis 密码必须从测试 fixture/安全环境变量注入，不写进文档或命令历史。

### A. 精确复现线上长排队形状

先使用与线上相同的大致容量比例，让本地快照有大量空闲账号，再通过 Redis 延迟 proxy 让 lease/RPM 请求超过当前 deadline。为了确定性看到本地排队，测试 endpoint 要走 local-only 路由（或测试 manager 直接使用 `AcquireMode::WaitForCapacity`），不要让 external fallback 抢先接管请求。

```json
{
  "credentialRpm": 200,
  "credentialMaxConcurrentRequests": 30,
  "credentialDispatchMaxWaitSecs": 300,
  "credentialSessionMaxConcurrentRequests": 0,
  "credentialInFlightLeaseMaxSecs": 900,
  "dispatchGlobalMaxConcurrentRequests": 1000,
  "dispatchMaxQueuedRequests": 200,
  "requestAdmission": {
    "rpm": 0,
    "maxConcurrentRequests": 0,
    "maxQueuedRequests": 0,
    "queueTimeoutMs": 0
  },
  "externalPools": {
    "externalPoolsEnabled": false,
    "localPoolPreflightEnabled": false,
    "fallbackOnSchedulerRedisDegraded": false,
    "externalPoolGlobalMaxConcurrentRequests": 1000,
    "externalPoolMaxQueuedRequests": 300,
    "externalPoolDispatchMaxWaitSecs": 300,
    "externalPoolLocalRescueEnabled": false,
    "externalPoolLocalRescueMaxWaitSecs": 300
  }
}
```

测试数据建议创建 64 个兼容该模型、启用且没有冷却/额度耗尽的凭据；至少 48 个应保持当前空闲。配置中的每账号并发 30 和全局并发 1000，意味着测试请求应明显低于本地可用容量。`credentialRpm=200` 用来覆盖 Redis RPM reservation；另跑一轮将它设为 `0`，单独覆盖 Redis 并发 lease 路径。

将 Redis URL 指向隔离测试 Redis 前的 loopback chaos proxy，并使用独立非零 DB/key prefix。给 proxy 下游响应加 300–500ms 延迟（超过当前 250ms capacity deadline），并以并发突发提交 local-only 请求。**修复前**预期会看到 Redis capacity breaker/degraded 日志，虽然账号仍 dispatchable，但请求逐步进入 dispatch queue；高并发时可观察到 queue depth 增长、queue full 或等待超时。**修复后**同一场景应在短暂独立 degraded grace 后返回 Redis coordination unavailable，queue depth 保持为 0，且 provisional local slots 回滚。要精确覆盖旧实现“等待约 300 秒”的放大形状，可在隔离环境中使用长 deadline 运行完整 300 秒测试；快速 CI 不应真的等 5 分钟，可通过测试时钟/可注入 deadline 缩短等价等待。

注意：线上观测到的 `max_wait_secs=299` 与快照中的多个长 deadline 同时存在；在复现日志中必须同时打印 route subtype、`AcquireMode`、有效 dispatch deadline 和 runtime config version，不能只靠 `externalPoolLocalRescueMaxWaitSecs=300` 推断来源。

### B. 复现“容量其实已满”的对照组

保持 Redis 正常，将 64 个凭据的有效并发槽占满，或用一个测试凭据且设 `credentialMaxConcurrentRequests=1` 并占住它。应观察到 `credential_capacity_full`（或对应的 capacity wait reason），等待 lease 释放后请求继续派发。该组是对照：它验证测试能够区分“真实容量满”和“Redis 协调超时”。

### 相关配置的取舍

| 配置 | 复现值/建议 | 作用 |
| --- | --- | --- |
| `credentialRpm` | 线上 200；复现一轮 200、一轮 0 | 非零时增加 Redis selection/RPM 原子路径；设 0 可隔离 lease 问题 |
| `credentialMaxConcurrentRequests` | 线上 30 | 必须让账号并发明显高于测试并发，以证明不是账号满 |
| `credentialDispatchMaxWaitSecs` | 线上 runtime 快照 60；长队列复现设置 300 | 控制普通凭据容量等待上限；不要把它当作 Redis 故障的合理等待时间 |
| `dispatchGlobalMaxConcurrentRequests` | 线上 1000 | 设得高于测试并发，避免全局上限成为真实瓶颈 |
| `dispatchMaxQueuedRequests` | 线上 200 | 复现队列增长/满；快速测试可先设 20–50 缩短观察时间 |
| `externalPoolCapacityMode` | 线上 `wait` | 只影响外部池自身容量队列；local-only 复现中不应成为变量 |
| `externalPoolDispatchMaxWaitSecs` | 线上 1 | 外部池容量等待 deadline；除测试 external fallback 外不参与本地 lease deadline |
| `externalPoolLocalRescueMaxWaitSecs` | 线上 300；复现配置设 300 | 外部失败后再救援到本地的等待上限；只有进入 local-rescue 路径时才相关 |
| `localPoolPreflightEnabled` | 线上 true；local-only 复现设 false | 开启时可能在解析/派发前把请求转外部池，影响复现路径 |
| `fallbackOnSchedulerRedisDegraded` | 线上 true；local-only 复现设 false | 开启时有合格外部池的 route 可能快速 fallback，隐藏本地排队表现；另需单独测 enabled 行为 |
| `requestAdmission` | 线上全 0/关闭 | 入口 admission 限制不应先于 scheduler 成为排队来源；复现时保持关闭或显式设足够高 |
| `credentialInFlightLeaseMaxSecs` | 线上 900 | 影响失联 lease 的老化回收；复现持续时间应远小于该值 |
| `weightedCapacity.enabled` | 建议 false | 若开启，大请求会消耗多个容量单位；为了排除 token weight 影响，复现首轮关闭 |
| `credentialSessionMaxConcurrentRequests` | 线上 0 | 0 表示会话并发可达到账号上限；首轮保持 0，避免 sticky/session 分流变量 |

不要为了复现把 `dispatchGlobalMaxConcurrentRequests`、per-credential concurrency 或 queue 全部设为 0（无限制），否则会失去对照边界并可能把压测服务打爆。不要在 59137/59139 生产 Redis、真实账号池或生产 API key 上做故障注入。

## 复现步骤

1. **建立隔离环境**：使用 staging/本地构建和测试账号，独立 PostgreSQL schema/database、独立 Redis 实例或明确隔离的非零 Redis DB、随机 key prefix。调用路径需要 local-only endpoint；外部 pool fallback 在最小复现中关闭。
2. **记录初始值**：导出脱敏 runtime config，记录版本、账号数、model-compatible/available/dispatchable 数、每账号/全局 in-flight 数、queue depth、load average、应用 CPU/RSS、Redis client/command 延迟和 PgSQL pool wait。
3. **无故障基线**：使用 64 个凭据 fixture，先以 10、50、100 并发各发 60 秒请求。确认正常情况下队列回到 0、账号容量指标与实际 in-flight 一致，`/healthz`/`/readyz` 正常。
4. **Redis affinity 延迟**：通过 chaos proxy 给 Redis response 注入 100ms、300ms、500ms 延迟，逐级测试。300/500ms 高于 affinity 75ms 和 capacity 250ms deadline。观察是否产生 breaker、请求等待、队列增加；此阶段先把 RPM 设为 0，定位 affinity/lease。
5. **Redis capacity lease 延迟**：恢复后把 RPM 设为 200，重复相同负载，确认是否在 `try_record_scheduler_selection` 路径触发 capacity breaker。
6. **Redis 断连与恢复**：关闭 proxy 转发 10 秒后恢复，验证 degraded 行为和 manager recovery；分别测外部 fallback eligible/不 eligible 两种 route。仅在 Redis DB 是空的、专供 runner 使用时才允许清理该 DB。
7. **容量对照**：Redis 延迟归零，用单账号/单槽和占满 lease 的 fixture 重放，确认只有真实 capacity-full case 进入容量队列。
8. **联合存储压力**：在隔离 staging 对 PgSQL usage 写入注入 500ms–5s 延迟，或用专用 fixture 模拟 pool acquire wait；同时重复 Redis delay。不要在线上操作大表索引、`VACUUM FULL` 或全表锁。
9. 每轮恢复后检查 proxy latency=0、Redis PING、queue depth 回落、breaker closed、in-flight lease 清理、usage drop 计数及健康状态；保存事件时间线。

## 测试方案与门禁

### 现有 focused Redis chaos 测试

仓库已有 `feature/tests/run-scheduler-redis-chaos-validation.mjs`，会在隔离 Redis 前启动 `redis-chaos-proxy.mjs`，使用真实 Redis 执行 scheduler 的延迟、断连、恢复、lease release 和 usage/scheduler 联合故障测试。默认要求 loopback Redis `DB 1..15`、`KIRO_RS_TEST_REDIS_ISOLATED=1`，默认 3 轮，并由 `feature/tests/run-cargo-scoped.sh` 运行 exact Rust tests。示意调用：

```bash
KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL='redis://127.0.0.1:<port>/<isolated-db>' \
KIRO_RS_TEST_REDIS_ISOLATED=1 \
KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS=3 \
node feature/tests/run-scheduler-redis-chaos-validation.mjs
```

`<isolated-db>` 必须是专供测试的空 DB，不能使用 DB0、生产 Redis 或受保护端口 9022。Runner 会对它执行隔离 DB 清理；只在确认 DB 所有权后使用。运行结束需确认 `childGroupsStopped=true`、`portsReleased=true`、`tempRemoved=true`。该 runner 验证 Redis scheduler 产品路径，但不等同于 64 账号、真实 HTTP/SSE 全链路压测。

相关源码测试候选：

- `redis_affinity_latency_does_not_degrade_capacity_coordination`
- `redis_capacity_latency_boundary_and_recovery_matrix`
- `redis_capacity_consecutive_timeouts_open_breaker_without_all_disabled`
- `redis_lease_release_is_non_blocking_under_latency_and_burst`
- `redis_capacity_disconnect_reconnect_recovers_same_manager`
- `redis_usage_writer_and_scheduler_joint_fault_matrix_recovers_without_spin_or_false_disable`
- `cancelled_provisional_redis_acquire_rolls_back_local_and_tombstones_remote`
- `redis_commit_unknown_provisional_acquire_leaves_no_lease`

### 新增代码后的必需回归测试

建议增加一个明确覆盖本次用户现象的产品级测试，避免只有通用 Redis helper 测试：

1. **单实例 local degraded**：64 个兼容凭据、48+ 有容量、Redis capacity ops 注入 300/500ms latency。断言请求仍从本地账号获得 lease 并进入 fake Kiro upstream；queue wait 接近 0；每账号/global 并发不超限；RSS/任务数有界。
2. **multi-instance fail-closed**：启动两个 manager/process 共用 Redis 权威；Redis unavailable 时断言不会各自本地 fail-open 导致全局并发超发，且在短 deadline 内返回 external fallback 或带 Retry-After 错误。
3. **真实容量满**：所有账号/全局槽已占满时断言进入 `capacity_full` 队列；释放一个 lease 后等待者公平获得槽。
4. **RPM 单独验证**：分别测试 `credentialRpm=0` 和非零；Redis selection timeout 必须分类为 `scheduler_redis_degraded`，不能混成 `credential_capacity_full`。
5. **affinity 不阻塞**：75ms affinity timeout 时本地仍有容量，应降级为非 sticky 选择或按配置 fallback，不应等待完整 capacity deadline。
6. **回收与 commit-unknown**：取消发生在 lease 请求发出前、发出后但结果未知、释放 Redis 超时、进程恢复四种边界，断言没有双重 lease、永久占槽、双重释放和错误本地计数。
7. **PgSQL 联合压力**：usage writer pool 满/事务慢时 `/readyz` 仍响应，scheduler capacity acquire 不被 usage writer 占满；恢复后 queue/drop 指标正确。
8. **healthcheck contract**：模拟 TCP accept 正常但 HTTP handler 卡住，TCP-only 探针必须失败测试；HTTP `/readyz` 在正常和 degraded mode 下返回与运行状态一致的结果。

### staging 压测矩阵

| 组别 | 账号与容量 | Redis | 请求负载 | 期望结果 |
| --- | --- | --- | --- | --- |
| 基线 | 64 个，至少 48 个可调度，每账号 30 | 正常 | 10/50/100 并发，每档 60 秒 | queue 不持续增长，健康检查正常 |
| 故障现状复现 | 同上 | 300–500ms response delay | 50/100/200 突发 | 当前代码可复现 breaker/queue；保留全部指标 |
| 新单实例模式 | 同上 | 300–500ms/断连 10 秒 | 50/100/200 突发 | 本地降级派发；队列不增长；容量不超发 |
| 多实例保正确性 | 2 个 manager，账号池共享 | 300–500ms/断连 | 50/100/200 突发 | 不本地 fail-open；快速 fallback/503；全局并发不超限 |
| 真容量饱和对照 | 单账号 1 槽，或全账号槽占满 | 正常 | 10–50 并发 | 只此组进入容量队列；释放后公平恢复 |
| PG usage 压力 | 与基线相同 | 正常或 250ms | usage batch write 500ms–5s | health/scheduler 响应，usage pressure 只体现在独立队列指标 |

每组至少 3 轮；联合 chaos/长流组至少 30 分钟。保存 p50/p95/p99 dispatch wait、Redis admission wait、Redis command RTT、queue depth、dispatch rate、成功/失败数、fallback 数、breaker open duration、RSS/CPU、PgSQL pool wait。通过标准：账号有容量且 Redis 故障时不会进入长队列；真容量满仍按策略排队；多实例总并发正确；健康检查不被 TCP 假健康掩盖。

### 数据和安全约束

- 只对测试拥有的 loopback Redis DB1..15 做 chaos；每轮开始确认 DB 空且使用随机 scope/key prefix。
- 禁止对生产 DB、共享 DB、DB0 或端口 9022 执行 `FLUSHDB`/`FLUSHALL` 或延迟注入。
- 使用专用测试账号和 fake upstream；不让复现请求消耗生产 Kiro 账号配额。
- 故障注入恢复后确认 proxy 延迟和断连开关已清除，应用切回 Redis 正常状态。
- 日志/报告脱敏，不保存 credential、refresh token、API key、Redis URL 密码或请求正文。

## 代码位置

- `src/kiro/token_manager/manager.rs`：Redis scheduler deadline/breaker 常量；capacity lease、RPM reservation、affinity 和 degraded dispatch wait。
- `src/kiro/token_manager/types.rs`：`AcquireMode` 区分普通等待、fail-fast 和 Redis degraded wait。
- `src/anthropic/handlers.rs`：local preflight、external fallback 和 local rescue deadline。
- `src/anthropic/inference_attempt_budget.rs`：请求级 upstream/dispatch budget。
- `src/kiro/token_manager/storage_task.rs`：专用 storage runtime 和同步桥。当前工作树已使用独立 `kiro-storage-task` runtime；不要把“同步 storage 仍占 HTTP runtime”当作这次新事故的已证实根因。
- `src/anthropic/usage.rs`、`src/storage/postgres.rs`：有界 usage writer 和 PgSQL 批写路径。
- `docker-compose.deploy.yml`：仓库部署 compose 的 HTTP readyz 检查。线上实际容器检查仍是 TCP-only，需修正实际运行的 compose/部署覆盖文件。
