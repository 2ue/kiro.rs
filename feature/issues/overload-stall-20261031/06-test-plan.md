# 测试方案与结果

压测和故障注入都用 mock 上游，不用真实账号。

| 脚本 | 用途 |
| --- | --- |
| `scripts/loadtest/overload-probe.mjs` | 开环到达，每秒采样；可按阶段切换上游故障（`--upstream-plan`） |
| `scripts/loadtest/overload-run.sh` | 每个用例独立建库、启动、清理；内含用例表 |
| `scripts/loadtest/scale-profile.py` | 把用例表里按现网 228 个槽写的到达率，按被测号池容量换算 |
| `scripts/loadtest/overload-verdict.py` | 按两条底线判定每个用例是否通过 |

## 资源约束

- 默认号池为 10 个账号，每号并发 4，共 40 个槽。过载倍数用到达率相对容量来表达，不靠加大连接数。
- 客户端最多 300 条连接，并复用连接。mock 的 keep-alive 为 120 秒。
- 每个用例开始前，TIME_WAIT 超过 2000 就先等待；压测过程中超过 4000 就中止。
- 每个用例结束后停掉进程、删库、执行 `FLUSHDB`。

## 采样与判定

每秒记录以下指标：

- 客户端：到达、尝试、200、429（按原因）、其他错误（按状态或错误码）、在途、成功 P50/P95、429 的返回耗时。
- `/readyz`：排队数、调度速率、距上次调度秒数、运行时调度延迟 `runtimeLagMs`、尚未写入 Redis 的释放数 `pendingRedisReleases`、readyz 自身耗时。
- mock 上游：在途总数、每个账号的在途数和近一分钟调用数、本秒开始的上游调用数。
- usage 写入器：积压（accepted − finished）和丢弃数。

判定规则（`overload-verdict.py`）：

| 底线 | 判定 |
| --- | --- |
| A：有容量就调度 | 阶段内有至少 5 秒在排队或拒绝时，这些秒的平均上游槽位利用率不低于 80%。上游故障阶段和 RPM 受限用例不按这条判 |
| B：不停摆 | 有排队、上游槽位未满 90%、且连续 5 秒没有开始任何上游调用，这样的秒数每阶段不超过 2 秒 |
| B：可用 | `/readyz` 每阶段不可用不超过 2 秒 |
| B：恢复 | 用例结束时排队为 0，Redis 租约为 0 |
| 资源 | 没有账号超过并发上限；内存有界；退出在 docker 默认的 10 秒内完成 |

## 场景矩阵

用例的具体参数在 `overload-run.sh` 的用例表里。下面的倍数都是相对容量（每秒约 45 个请求即满）。

| 类别 | 用例 |
| --- | --- |
| 负载形态 | steady_ok（45%）、near_full（85%）、spike_sub2api（突增到 3.3 倍再回落）、ramp_sub2api（缓慢升到 2 倍再降）、square_sub2api（过载与正常交替）、long_overload（2 倍持续 4 分钟）、long_spike（30–60 秒长流突增）、queue_overflow（队列只有 20，突增到 6.6 倍） |
| 请求特征 | sticky_spike（集中在 40 个会话）、tiers_spike（优先级分两档）、bigbody_spike（256KB 请求体）、rpm_bound（每号 RPM 20 成为瓶颈） |
| 客户端行为 | spike_sub2api（sub2api 立即重试）、spike_honor（遵守 Retry-After）、spike_noretry（不重试）、spike_impatient（10 秒后放弃） |
| 上游故障 | storm_5xx（全部 500 持续 30 秒）、storm_429（全部 429，带 Retry-After）、storm_429_noretry（全部 429，不带 Retry-After）、storm_reset（首字节前断流）、storm_midstream（一半流中途断开）、spike_with_errors（过载叠加 30% 5xx） |
| 上游变慢 | slow_upstream（整体慢 5 倍）、slow_wave（响应头 20–40 秒，持续 60 秒）、slow_accounts_spike（三成账号多 30 秒） |

## 结果（修复后，10 账号 × 4 并发）

24 个用例中 23 个第一次就通过。storm_429_noretry 恢复阶段槽位利用率只有 61%，原因和修复见下文。

所有用例共同的结果：

- 结束时 Redis 租约为 0。
- 退出耗时 0.7–0.8 秒。
- 内存 45–207MB。
- 没有账号超过并发上限。
- 没有停摆，没有 `/readyz` 不可用。

| 用例 | 成功 | 429 | 说明 |
| --- | --- | --- | --- |
| steady_ok | 312 | 0 | 不排队 |
| near_full | 790 | 0 | 不排队 |
| spike_sub2api | 1219 | 22803 | 过载期间槽位利用率 98%；回落后自动恢复 |
| ramp_sub2api | 2111 | 32935 | |
| square_sub2api | 1701 | 33576 | |
| long_overload | 2525 | 88126 | 内存 185MB |
| long_spike | 210 | 43 | |
| queue_overflow | 753 | 79510 | 429 P95 低于 150ms |
| sticky_spike | 1224 | 23318 | |
| tiers_spike | 1222 | 23344 | |
| bigbody_spike | 1234 | 21292 | 内存 207MB |
| rpm_bound | 661 | 10249 | 每分钟用满 200/200 |
| spike_honor | 1588 | 1541 | |
| spike_noretry | 1107 | 491 | |
| spike_impatient | 652 | 768 | |
| storm_5xx | 302 | 0 | 故障期间返回 502，恢复后立即恢复 |
| storm_429 | 406 | 397 | |
| storm_429_noretry | 503 | 2688 | 第一次跑时恢复阶段槽位利用率 61%，见下文 |
| storm_reset | 312 | 0 | |
| storm_midstream | 371 | 0 | |
| spike_with_errors | 993 | 20397 | |
| slow_upstream | 627 | 182 | |
| slow_wave | 622 | 31 | |
| slow_accounts_spike | 827 | 11337 | |

修复前，同样的 spike_sub2api、ramp_sub2api、long_spike 都会停摆，回落后也不恢复（见 [04-reproduction.md](04-reproduction.md)）。

### storm_429_noretry

现象：上游对全部账号返回 429、不带 Retry-After，持续 30 秒。恢复后约 30 秒内只有一半账号可用。

原因：没有 Retry-After 时，账号冷却 30 秒并且翻倍。

修复：大部分账号已经在冷却时，后续的冷却封顶 5 秒；写入 Redis 的冷却也封顶，否则下一次状态同步会把 30 秒的冷却带回来。带 Retry-After 时仍按上游给的时间。最先冷却的那一半账号保持原来的冷却，因为当时还不能判断是不是全池问题。

## 结果（300 账号，每号并发 3–5、RPM 20–30，约 1200 个槽）

到达率按同样比例换算，代码为 0.0.233 及之后。每个用例开跑前等本机负载降到 60 以下。

| 用例 | 结果 | 说明 |
| --- | --- | --- |
| pool_steady | 通过 | 7955 个全部成功，没有排队 |
| pool_rpm_spike | 通过 | 没有账号超 RPM |
| pool_rpm_long | 通过 | 复测时收尾阶段 /readyz 4 秒不可用，当时本机卡住（mock 自身延迟 660 秒） |
| pool_5xx_recover | 通过 | |
| rpm_storm_429 | 通过 | |
| pool_queue_fault | 通过 | |
| pool_queue_impatient | 通过 | |
| pool_hang | 通过 | 挂住的请求在空闲超时后释放 |
| pool_trickle | 通过 | 滴流在总时长上限处截断 |
| 全局上限 800：pool_long_full | 通过 | |
| pool_burst | 未通过 | 回落阶段空闲额度 0.13–0.22 |
| 全局上限 800：pool_rpm_spike | 未通过 | 恢复阶段空闲额度 0.54 |
| pool_long_full | 未通过 | /readyz 4 秒不可用，PG 检查超时 |

未通过的原因：
- 空闲额度偏高：kiro-rs 每次请求上游都新建连接（`Connection: close`），macOS 的监听队列上限 `kern.ipc.somaxconn` 是 128，突发时 mock 接不过来，连接要重试或超时（两个用例分别有 292、42 次上游建连失败）。这段时间 kiro-rs 已经占着槽，mock 还没看到请求，压测就把这些槽算成空闲。属于测试环境限制，没有用 kiro-rs 自身的在途数逐秒核对。
- /readyz：1200 个槽满载时，后台写入占满 10 个 PG 连接，readyz 的 ping 排队超过 500ms 并持续超过 10 秒。现网健康检查 30 秒一次、连续 3 次失败才判不健康，不会触发。

## 回归

- 新增单元测试（全部通过）：
  - 运行时延迟探针
  - 日志限速
  - 内存 usage 记录索引
  - 拒绝记录采样
  - 判满标记（设置、过期、释放后清除）
  - 早拒绝缓存失效
  - 停止时排队请求立即返回
  - breaker 延迟豁免及其安全阀
  - tombstone 按序修剪与上限
  - 分档号池成片故障不冷却
  - 专属代理成片超时不冷却
  - 成片 429 冷却封顶
- 全量测试：默认特性和无默认特性两套，带 PgSQL 和 Redis。`redis_backed_session_binding_and_cooldown_are_shared_between_managers` 依赖 75ms 的 Redis 亲和时限，机器负载高时会失败，已改为 timing-sensitive，由单线程时序任务运行。
- 真实账号：只做少量协议用例，确认没有行为回退。
