# Strict Local First, Distribution, And Multi-Instance Scheduling

Status: `release-blocked / intermittent-scheduler-degraded-fallback-reproduced / coordinator-and-real-process-e03-pass / external-takeover-runner-contract-pass / e01-e02-runner-contract-pass / e05-nondocker-runner-contract-pass / external-takeover-dynamic-and-distribution-gates-open`

Severity: P0/P1

## 用户可见现象与影响

local error 分类后，external fallback 路径没有重新读取当前 local route state。前若干账号 500、后续本地账号仍 dispatchable 时，请求仍可能进入 external。lease 竞争失败在普通 wait 模式下可能继续等待同一账号，而不是重选其他有槽账号；sticky 又优先于所有 load-balancing mode，可能造成热点。

现有 24 账号/600 请求 balanced 测试只看最终选择数，容忍度宽，且没有 Redis、两实例、同 session、长流或峰值 in-flight，不能关闭生产偏斜问题。

用户看到的未必是明确 scheduler 文案，也可能只是本地仍有容量却产生 external 费用、特定账号持续高负载、可用账号闲置、请求无理由排队或偶发 429。它与工具 hash/transcript 无关，没有固定内容指纹，必须用 route/lease/选择计数证明。

## 根因

- fallback 决策曾使用故障前或 250 ms cache 中的 route state，local attempt 后没有 model-aware fresh recheck。
- fresh-state guard 又曾把 `SchedulerRedisDegraded` 的本地内存 `dispatchable` 估计当成可取得分布式 lease 的证据，错误压制 external fallback。
- account lease 竞争失败、sticky binding 与 load-balancing policy 的所有权分散；等待固定候选会阻止重选其他空闲账号。
- 单实例累计选择数不能证明跨实例 lease、峰值并发、TTL 恢复或分布公平。

## 复现

- 60 本地账号，前 N 个 500，后续账号 200，external 计数器必须为 0。
- 四种策略 x 同/异 session x 短/长流 x burst，各 3 轮。
- 两实例共享 Redis，交叉 acquire/renew/release、实例 kill/restart、TTL 恢复，各 3 轮。
- lease race 时一个候选满、另一个空，验证重选而非粘住等待。

E05 strict local-first runner 已改成非 Docker 入口，保留 10 类全矩阵模式，但要求调用方提供 caller-owned PostgreSQL/Redis：

```bash
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_E05_POSTGRES_URL_TEMPLATE='postgres://...@127.0.0.1:<pg-port>/{database}' \
KIRO_E05_POSTGRES_DATABASES='kiro_e05_run_01,kiro_e05_run_02,...' \
KIRO_E05_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db>' \
KIRO_E05_REDIS_PREFIX='kiro_rs:e05:<unique>' \
node feature/tests/strict-local-first-routing.mjs
```

默认 10 modes × 3 rounds 需要 30 个预创建 `kiro_e05_*` database。脚本不启动 Docker、不创建 database、不 `FLUSHDB`、不调用 Cargo、不探测 `9022`；Redis fault 使用 `feature/tests/redis-chaos-proxy.mjs`，结束只清理 `KIRO_E05_REDIS_PREFIX:*`。

当前可执行的非 Docker 子项是 SchedulerRedisDegraded external takeover runner：

```bash
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL='postgres://...@127.0.0.1:<pg-port>/kiro_external_takeover_<owned_empty_db>' \
KIRO_EXTERNAL_TAKEOVER_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db>' \
KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX='kiro_rs:external_takeover:<unique>' \
node feature/tests/external-takeover-scheduler-degraded-nondocker.mjs
```

E01/E02 分布公平 runner 也已改成非 Docker 入口，但动态仍需调用者预创建空 PostgreSQL databases：

```bash
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_E01_E02_POSTGRES_URL_TEMPLATE='postgres://...@127.0.0.1:<pg-port>/{database}' \
KIRO_E01_E02_POSTGRES_DATABASES='kiro_e0102_run_01,...' \
KIRO_E01_E02_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db>' \
KIRO_E01_E02_REDIS_PREFIX='kiro_rs:e0102:<unique>' \
node feature/tests/scheduler-fairness-sticky-race.mjs
```

E05 runner contract 已通过，但动态 service run 尚未执行；不能由 E01/E02 或 SchedulerRedisDegraded 子项替代。动态通过前，E05 仍保持 release blocker。

## 修复方向

- fallback 前原子/近实时 recheck local route state；Ready/dispatchable > 0 时继续本地策略，不能 external。
- lease 竞争失败将账号加入本请求短期 excluded set 后重选；排队对象应是池容量，不是固定账号。
- sticky 只在 bound credential 有实时容量且健康时优先；否则落入配置策略。
- 观测峰值 in-flight、queue wait、lease race/reselect、sticky hit/fallback，而非只看累计选择数。

方案比较：完全依赖缓存最快但会误路由；fallback 前对所有账号做重型全扫描会增加正常请求 Redis/CPU 成本；选定方案是 model-aware fresh summary、严格状态枚举和原子 lease/reselect，normal Ready 保持 local-first，degraded 只由其独立策略决定。跨实例必须继续 fail closed，不能为了低延迟退回 local-memory 超卖。

## 当前实现与动态结果

- fallback/preflight 使用绕过旧 250 ms route-state cache 的 fresh state，并按当前请求 model 检查 local 与 external eligibility。
- local 状态 Ready 或普通状态仍有真实 dispatchable 账号时禁止 external；NoCredentials、AllDisabled、NoModelCompatible、AllCoolingDown、CapacityFull 和 SchedulerRedisDegraded 仅在各自开关允许时 fallback。
- 七状态矩阵 3 轮 x 5 请求通过：本地 60 账号均可调度但连续 transient 500 时，每请求 3 个有界 local attempt、15/15 external 0；external 500 时每请求 external 1、local 0，不形成循环。
- CapacityFull 使用真实 holder 占唯一 local slot，15/15 probe 均 local 0/external 1；probe TTFB p50 `19.87 ms`、p95 `72.79 ms`。
- external eligibility 现在按 model 判断并在真正 fallback 时绕过短缓存；错误模型不能借通用 pool 状态进入 external。

详细报告与构建身份见 [strict local-first 与 scheduler chaos](../evidence/strict-local-first-and-scheduler-chaos-20260716.md)。这些是 focused dirty-build 结果；E01 分布、E02 race、E03 双实例和最终统一 binary 仍 pending。

### 2026-07-16 最新组合复核：P0 红灯

完整 E05 首轮先暴露了 runner 自身的隔离缺陷：不同 PostgreSQL authority 按 round 复用了同一 Redis DB/default prefix，导致前一个 `local_all_cooling` 的 credential cooldown 污染后一个 `local_capacity_full`。runner 已改为每个 mode/round 使用独立 `redis.keyPrefix`，并增加 binary 起止 SHA 一致性断言和失败报告；单独 CapacityFull 3 轮在 binary `f654f2bb...` 上通过。此前从未把 AllCoolingDown 与 CapacityFull 放进同一轮的历史报告不能证明这种组合隔离。

随后用固定 binary `dd15a7bf79e5017e4218e8fda6e99656fb826180b0327c8beaa249619a07dbc1` 对 `local_ready_transient` 做 3 次外层复核，每次要求 3 内轮 x 5 请求：

- 外层 1 完整通过；报告 `target/e05-reports/e05-20260716123127641-79445-306886.json`。
- 外层 2 在首内轮 request 3 错误成功走 external：该请求 local inference 1、external 1；Redis 原子写 sticky binding 和 acquire credential slot 均超过 75 ms。失败报告 `target/e05-reports/e05-20260716123215840-88348-da60dd.json`。
- 外层 3 在首内轮 request 4 错误成功走 external：该请求 local inference 2、external 1；Redis session soft-failure 操作实测 118 ms，随后 acquire slot 超过 75 ms；同窗口 PgSQL stats delta 写入 179 ms。失败报告 `target/e05-reports/e05-20260716123250625-93136-3b59e2.json`。

两份失败报告均为 `binaryStableDuringRun=true`，cleanup 三项全 true，fixture key/token 扫描 0 命中。结论不是“external fallback 开关错误”，而是本地仍有约 49-54 个 dispatchable credential 时，客户端/runtime/同步持久化压力仍能让 75 ms Redis 热路径进入 degraded；该分类又按配置允许 external，于是 strict local-first 在突发错误流量下间歇失效。固定候选 3 次外层有 2 次失败，旧的 focused pass 只能作为历史证据，不能关闭 E01/E05。

## 验收、回滚与残余风险

E01-E05。不得超卖；本地仍有可用容量时 external calls=0；错误/重启后 lease 在 TTL 内恢复；各模式分布满足预先定义的负载/权重合同。

每个结果必须同时记录 selected credential、session/sticky 状态、lease ID、实例、峰值 in-flight、queue wait、reselect 次数、local/external hit 和恢复时间。两实例至少 3 轮 kill/restart/TTL recovery；60 账号每策略至少 3 轮且预先给出公平/权重容差，不能测试后再放宽。

当前协调层 E03 已在当前项目隔离 Redis 完成 3 outer × 5 internal，即 15/15：独立连接/manager 的 lease 唯一性、交叉 release、touch、崩溃 TTL recovery、共享 queue `4/16` 与 RPM reservations 均通过，最终 queue/lease=0。2026-07-21 scheduler Redis failure classification 修复后又用空 DB7 重跑 `multi-instance-redis-coordination-20260721-r3`，再次 3 outer × 5 internal（15/15）通过，scope `1708432 KiB removed=true reservation_released=true`。详见 [两实例协调证据](../evidence/multi-instance-redis-coordination-20260720.md)。

2026-07-21 真实服务进程 E03 也已通过：用冻结候选 `/tmp/kiro-e03-candidate.T2iG7N/kiro-rs`（sha256 `98e0f79328b49925dc940faaa3b1e8b0c8ae8ef7b9975725eb219635c8957ee7`）跑 3 outer rounds。每轮两个真实 kiro.rs 进程共享同一 caller-owned PostgreSQL database、Redis DB12 和独立 prefix，覆盖 holder renewal、B 进程 shared capacity pending、holder release 后 local 200、A 进程组 SIGKILL、stale lease TTL recovery、A restart、shared RPM 和 B restart 后 RPM fence。三轮均为：

```text
renew.blockedPendingMs=1250
renew.blockedStatusAfterRelease=200
crash.immediatePendingMs=1250
crash.staleLeaseRecoveryStatus=200
crash.ttlRecoveryStatus=200
rpm.firstStatuses=[200,200]
rpm.postRestartStatuses=[429,429]
externalHits=0
disabled=0
cleanup.redisPrefixKeysRemaining=[]
cleanup.occupiedPorts=[]
```

报告见 [E03 真实双进程证据](../evidence/e03-real-two-process-scheduler-runner-20260720.md)。这关闭“真实 kiro.rs 进程 SIGKILL/restart 和共享 RPM 未执行”的缺口，但不能关闭 E01/E02 分布、公平性、E05 degraded external takeover、生产高基数和最终发布候选全矩阵。scheduler degraded 转 external 的异常路径在 fault + usage writer 联合压力下仍需复核首字延迟。

2026-07-21 external takeover 的 focused 代码路径与非 Docker runner 合同已补齐：四个 handler/fallback exact tests 通过，runner contract 8/8 通过，并明确验证 enabled 模式应由 external 接管、disabled 模式应规范失败、恢复后应回 local。由于本轮没有可确认独占空 PostgreSQL database URL，动态 service runner 仍未执行；该项不能替代 E05 产品门禁。详见 [SchedulerRedisDegraded 外部池接管验证程序](../evidence/external-takeover-scheduler-degraded-20260721.md)。

同日 E01/E02 runner 自身也做了安全合同修正：`scheduler-fairness-sticky-race.mjs` 不再启动 Docker、不再 `FLUSHDB`、不再创建 PostgreSQL database，改为要求 caller-owned PG URL template、`modes × rounds` 个预创建 `kiro_e0102_*` database、loopback nonzero Redis DB 和 caller-owned Redis prefix；每 case 使用独立 `redis.keyPrefix`。`scheduler-fairness-sticky-race.contract.test.mjs` 7/7 通过，`runtime-validation-paths.test.mjs` 9/9 通过，证明不会误用 Docker/Cargo、不会探测 `9022`、不会使用仓库内 binary/artifact。详见 [E01/E02 scheduler fairness runner contract](../evidence/scheduler-fairness-nondocker-runner-contract-20260721.md)。这只关闭 runner 安全合同，不关闭 E01/E02 动态分布公平。回滚不得重新允许 local-memory fail-open 或绕过 strict local-first。

同日又把 `strict-local-first-routing.mjs` 改成非 Docker 全矩阵入口：调用方必须提供仓库外冻结 binary、owned artifact root、`modes × rounds` 个预创建 `kiro_e05_*` database、loopback Redis DB1..15 和 caller-owned Redis prefix；脚本不启动 Docker、不创建 database、不 `FLUSHDB`、不调用 Cargo、不探测 `9022`，Redis fault 使用 `redis-chaos-proxy.mjs`。`strict-local-first-routing.contract.test.mjs` 6/6 通过，且同批 `runtime-validation-paths` 9/9、external takeover contract 8/8、E01/E02 contract 7/7，合计 30/30 通过；`git diff --check` 通过。inventory 仍因用户服务 PID `84264` 引用根 `target/` 预期失败，不是本轮产物。详见 [strict local-first E05 non-Docker runner contract](../evidence/strict-local-first-nondocker-runner-contract-20260721.md)。这仍不是 E05 动态 PASS；后续需要冻结 binary 与预创建空 PG databases 执行全矩阵。
