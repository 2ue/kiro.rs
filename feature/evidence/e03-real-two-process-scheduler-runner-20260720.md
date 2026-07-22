# E03 真实双进程 scheduler 门禁程序

Date: 2026-07-20

Status: `runtime pass / 52-of-52 safety contracts pass / frozen-candidate 3 outer rounds pass`

## 结论边界

本文件记录的是 E03 真实服务进程验证程序及其安全合同，不把已有的多
`ConnectionManager` / 多 `MultiTokenManager` 单元集成测试冒充两个真实 HTTP
服务进程。

当前已完成：

- 新增两个真实 kiro.rs 进程的独立 runner；
- 新增纯 Node 早拒绝、无副作用和信号清理合同；
- runner 不调用 Cargo，不读取仓库 `target/debug` 或 `target/release`；
- runner 不启动、不检查 Docker，也不创建或删除数据库；调用方必须在当前项目
  已运行的 PostgreSQL 实例中预先创建每个 outer round 的空 `kiro_e03_*` 数据库，
  runner 只连接这些 caller-owned 数据库并在退出时保留它们，数据库清理由调用方
  的外层 `trap/finally` 负责；
- Redis 使用调用方指定的非零 loopback DB 和随机 `keyPrefix`，只扫描、删除该
  prefix，绝不执行 `FLUSHDB`；
- 不探测、不连接、不停止 `9022`。

2026-07-21 已用仓库外冻结候选二进制完成真实 runtime gate。该结论只覆盖 E03
真实双进程 scheduler 协调、lease renewal、SIGKILL stale lease、shared RPM 和
外部池不误接管，不代表其它 Claude CLI、thinking、usage writer fault、升级、
UI 或最终 release inventory 门禁已经关闭。

## 文件

- `feature/tests/e03-real-two-process-scheduler.mjs`
- `feature/tests/e03-real-two-process-scheduler.contract.test.mjs`

## 真实运行矩阵

每个 outer round 使用一个调用方预先创建的、独立的 `kiro_e03_*` PostgreSQL
database，并启动两个不同临时端口的真实 kiro.rs 进程。两进程共享同一
PostgreSQL authority、同一 Redis URL 和同一随机 Redis prefix；三个数据库仍由
同一套当前项目 PostgreSQL 服务承载，不启动第二套基础设施。

| 阶段 | 真实动作 | 必须满足的断言 |
| --- | --- | --- |
| startup | 先后启动 service A/B，共享数据库、凭据和 prefix | 两进程健康；第二进程看到两条凭据和同一个外部池 |
| local baseline | A/B 各发一条真实 `/v1/messages` | 都命中 fake Kiro local upstream；external hit 不增加 |
| acquire + renew | A 持有 credential 1 的流式请求超过 `credentialInFlightLeaseMaxSecs=3` | Redis `last_seen` lease 仍为一条且 score 前移超过 1 秒 |
| shared capacity | renew holder 存活时 B 再请求同模型 | B 不得进入 local fake upstream；不得错误 fallback external |
| release | 释放 holder 后 B 再请求 | Redis lease 清零；B 立即恢复 local 200 |
| SIGKILL | A 持有真实 lease 时对 A 进程组发送 SIGKILL | A 退出；lease 不被另一实例误删；立即请求仍受共享容量约束 |
| TTL recovery | 等待 lease max age 后由 B 发新请求 | B 清理 crash lease、local 200；凭据 `disabled=0`；external hit 不增加 |
| restart | 用同一冻结二进制和共享 authority 重启 A | 重启实例 local 请求成功；无 false-disable |
| shared RPM | credential 2 配置 `rpm=2`，A/B 各成功一次 | Redis `scheduler:rate_limit:2` 有正 TTL；第三次请求被共享 RPM 拒绝 |
| RPM restart fence | 停止并重启 B 后再请求 credential 2 | 第四次仍被 Redis authority 拒绝，不能因进程重启重置 RPM |
| cleanup | 停止所有子进程和 fake servers | 自有端口全释放、Redis prefix 为空、caller-owned PG database 保留不改、temp root 删除 |

配置了一个真实可调度的 fake external pool，但关闭普通 local-capacity 和 RPM/transient
fallback，只保留 Redis degraded / no-available fallback。这样 genuine capacity/RPM
拒绝不会被外部池掩盖；如果两实例错误地把健康凭据判成 all-disabled 或 scheduler
degraded，external hit 会增加并直接使门禁失败。

## 安全合同结果

最终命令：

```text
node --test feature/tests/e03-real-two-process-scheduler.contract.test.mjs
```

最终结果（2026-07-21 复跑）：

```text
tests=52
pass=52
fail=0
skipped=0
duration_ms=3965.746833
```

覆盖如下：

- 有效 validate-only：3/3；
- 缺 PostgreSQL template、Redis URL、Redis prefix：每类 3/3；
- PostgreSQL 缺 `{database}`、远程 PG、远程 Redis、Redis DB0：每类 3/3；
- PostgreSQL/Redis 指向 `9022`：每类 3/3，且不做 listener probe；
- 生产 `kiro_rs:local` prefix、数据库数量/所有权不满足、非法 outer rounds：每类 3/3；
- `SIGHUP`、`SIGINT`、`SIGTERM`：各 3/3，退出码分别为 129/130/143；
- 每轮信号测试确认 ready file、temp root 删除，且 fake Docker/Cargo marker 未创建；
- 静态合同确认没有 Cargo invocation、默认 target 查找、任何 Docker invocation 或
  `9022` listener inspection。

早期合同运行真实暴露并修复了 runner 自身的两个问题：错误路径局部变量遮蔽
`cleanup()` 导致 TDZ `ReferenceError`，以及静态正则把普通源码文本误判成命令。
数据库边界改造后完整矩阵复跑 52/52。

## 真实 runtime 结果

候选：

```text
binary=/tmp/kiro-e03-candidate.T2iG7N/kiro-rs
sha256=98e0f79328b49925dc940faaa3b1e8b0c8ae8ef7b9975725eb219635c8957ee7
```

正式命令使用当前项目隔离 PostgreSQL/Redis，不启动 Docker，不触碰 `9022`：

```text
KIRO_RS_BINARY=/tmp/kiro-e03-candidate.T2iG7N/kiro-rs
KIRO_VALIDATION_ARTIFACT_DIR=/tmp/kiro-e03-artifacts-20260721-rpm-r2-formal
KIRO_E03_POSTGRES_URL_TEMPLATE='postgres://kiro_rs:<redacted>@127.0.0.1:25432/{database}'
KIRO_E03_POSTGRES_DATABASES='kiro_e03_20260721_rpm_r2_1,kiro_e03_20260721_rpm_r2_2,kiro_e03_20260721_rpm_r2_3'
KIRO_E03_REDIS_URL=redis://127.0.0.1:26379/12
KIRO_E03_REDIS_PREFIX=kiro_rs:e03:runtime:20260721:rpm_r2_formal
KIRO_E03_OUTER_ROUNDS=3
node feature/tests/e03-real-two-process-scheduler.mjs
```

结果：

```text
result=pass
runId=e03-20260721013242272-88844-36667d
outerRounds=3
reportPath=/private/tmp/kiro-e03-artifacts-20260721-rpm-r2-formal/reports/e03-real-two-process-scheduler/e03-20260721013242272-88844-36667d.json
cleanup.childGroupsStopped=true
cleanup.serversStopped=true
cleanup.redisPrefixKeysRemaining=[]
cleanup.databasePreserved=true
cleanup.occupiedPorts=[]
cleanup.tempRemoved=true
```

三轮摘要：

| Round | DB | renewal pending | release recovery | SIGKILL immediate | TTL recovery | RPM first | RPM post-restart | external hits | disabled |
| --- | --- | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: |
| 1 | `kiro_e03_20260721_rpm_r2_1` | 1250 ms | 200 | 1250 ms pending | 200 | `[200,200]` | `[429,429]` | 0 | 0 |
| 2 | `kiro_e03_20260721_rpm_r2_2` | 1250 ms | 200 | 1250 ms pending | 200 | `[200,200]` | `[429,429]` | 0 | 0 |
| 3 | `kiro_e03_20260721_rpm_r2_3` | 1250 ms | 200 | 1250 ms pending | 200 | `[200,200]` | `[429,429]` | 0 | 0 |

资源回收：

| Round | Process | RSS start/end | FD start/end |
| --- | --- | --- | --- |
| 1 | A | 27808 / 39072 KiB | 30 / 29 |
| 1 | B | 27584 / 40384 KiB | 30 / 29 |
| 2 | A | 26944 / 30752 KiB | 30 / 29 |
| 2 | B | 27392 / 32960 KiB | 30 / 29 |
| 3 | A | 27280 / 27536 KiB | 30 / 29 |
| 3 | B | 27680 / 30656 KiB | 30 / 29 |

此门禁在修复共享 RPM 同步预约后通过。修复前旧候选曾真实复现第三次跨实例
RPM 请求仍 local `200` 的 fail-open，failure report 为：

```text
/private/tmp/kiro-e03-artifacts-20260721-probe1/reports/e03-real-two-process-scheduler/e03-20260720200151931-93095-24fa20.failure.json
```

失败根因是旧 `record_scheduler_selection()` 先本地记录、再异步 best-effort 写 Redis；
连续跨实例请求可能在下一次选择前读不到 `scheduler:rate_limit:<id>`。当前修复
改为 Redis Lua 原子 `try_record_scheduler_selection()` 同步预约，达到 RPM 时同时设置
shared deadline；manager 侧不再双计数 Redis，并把 RPM-only 候选等待归因为
`凭据 RPM 限制`，避免误写成 account concurrency 或 all-disabled。

## 复现命令模板

先由调用方预创建 caller-owned `kiro_e03_*` 数据库，再执行：

```text
mkdir -p /tmp/kiro-e03-artifacts

KIRO_RS_BINARY=/absolute/outside/repo/frozen/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/tmp/kiro-e03-artifacts \
KIRO_E03_POSTGRES_URL_TEMPLATE='postgres://kiro_rs:<redacted>@127.0.0.1:25432/{database}' \
KIRO_E03_POSTGRES_DATABASES='kiro_e03_r1,kiro_e03_r2,kiro_e03_r3' \
KIRO_E03_REDIS_URL=redis://127.0.0.1:26379/12 \
KIRO_E03_REDIS_PREFIX=kiro_rs:e03:real_two_process_r1 \
KIRO_E03_OUTER_ROUNDS=3 \
node feature/tests/e03-real-two-process-scheduler.mjs
```

候选路径必须是仓库外复制并冻结的绝对路径。runner 会记录候选 SHA-256，但不会
发现或构建候选。报告必须出现：

```text
result=pass
outerRounds=3
every round externalHits=0
every round disabled=0
every round renew.afterLastSeenMs > renew.beforeLastSeenMs + 1000
every round crash.immediateStatus != 200
every round crash.ttlRecoveryStatus = 200
every round rpm.firstStatuses = [200,200]
every round rpm.postRestartStatuses both != 200
cleanup.childGroupsStopped=true
cleanup.serversStopped=true
cleanup.redisPrefixKeysRemaining=[]
cleanup.databasePreserved=true
cleanup.occupiedPorts=[]
cleanup.tempRemoved=true
```

任何字段不满足时 runner 直接非零退出，不允许用日志解释覆盖失败结果。
