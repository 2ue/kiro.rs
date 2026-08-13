# E01/E02 Scheduler Fairness Non-Docker Runner Contract

Date: 2026-07-21

Status: `runner-contract-pass / dynamic-service-run-pending`

Scope: 将 E01/E02 分布公平、sticky 和 lease-race runner 从旧的 Docker-managed PostgreSQL/Redis 入口收敛为 caller-owned dependency 入口，避免本地验证误启动 Docker、误 `FLUSHDB` 或创建数据库。

## 结论

`feature/tests/scheduler-fairness-sticky-race.mjs` 已改为安全入口：

- 要求 `KIRO_RS_BINARY` 为仓库外复制的冻结候选，不接受 `target/debug` 或 `target/release` 直接产物。
- 要求 `KIRO_VALIDATION_ARTIFACT_DIR` 为仓库外 owned artifact root。
- 要求调用者提供 `KIRO_E01_E02_POSTGRES_URL_TEMPLATE`，模板必须包含一次 `{database}`，且只能指向 loopback PostgreSQL。
- 要求调用者提供 `KIRO_E01_E02_POSTGRES_DATABASES`，数量必须等于 `modes × rounds`，名称必须是 caller-owned `kiro_e0102_*`。
- 要求调用者提供 `KIRO_E01_E02_REDIS_URL`，只能是 loopback `redis://`，DB 必须是 `1..15`，不允许 auth/query/fragment。
- 要求调用者提供 `KIRO_E01_E02_REDIS_PREFIX`，不能包含生产/共享 `kiro_rs:local`。
- dynamic runner 不再执行 Docker、不再 `FLUSHDB`、不再 `CREATE DATABASE`；每个 case 使用独立 Redis `keyPrefix`，只清理该 prefix。
- 支持 `KIRO_E01_E02_VALIDATE_ONLY=1`，可在无 PG/Redis 网络访问前验证输入、binary/artifact 路径、database 数量和安全边界。

这不是 E01/E02 产品动态通过证据；动态服务跑仍需要调用者提供独占空 PostgreSQL databases 和 Redis DB/prefix。

## 修改点

文件：

- `feature/tests/scheduler-fairness-sticky-race.mjs`
- `feature/tests/scheduler-fairness-sticky-race.contract.test.mjs`

行为变化：

| 旧行为 | 新行为 |
| --- | --- |
| 默认通过 Docker 启动 PostgreSQL/Redis | 不启动 Docker，必须使用 caller-owned loopback PG/Redis |
| 通过 Docker `psql CREATE DATABASE` 创建 case DB | 不创建数据库，要求调用者预创建 `kiro_e0102_*` DB |
| 每 case 用 Docker `redis-cli FLUSHDB` | 不 `FLUSHDB`，每 case 使用独立 `redis.keyPrefix` 并只删除 prefix keys |
| 报告记录 Docker image/container | 报告记录 sanitized host/port、DB 数量、Redis DB 和 prefix hash |
| runner 源码仍可误调用 Docker/Cargo | contract 扫描禁止 Docker/Cargo 调用 |

## 合同测试

命令：

```bash
node --test feature/tests/scheduler-fairness-sticky-race.contract.test.mjs
node --test feature/tests/runtime-validation-paths.test.mjs
node --test feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs
git diff --check
```

结果：

```text
scheduler-fairness-sticky-race.contract.test.mjs: 7/7 pass
runtime-validation-paths.test.mjs: 9/9 pass
external-takeover-scheduler-degraded-nondocker.contract.test.mjs: 8/8 pass
git diff --check: pass
```

覆盖：

- JavaScript 语法有效。
- validate-only 接受 caller-owned loopback PG/Redis，并报告 `dockerUsed=false`、`cargoUsed=false`、`protected9022ProbeSkipped=true`。
- 默认 4 modes × 3 rounds 需要 12 个 database；mode subset 会改变所需 database 数。
- 无 `{database}` placeholder、非 loopback PG、PG port `9022`、不安全 DB 名称全部在 runtime 前拒绝。
- Redis DB0、非 loopback Redis、Redis port `9022`、共享 `kiro_rs:local` prefix 全部在 runtime 前拒绝。
- runner source 不调用 Docker/Cargo。
- 所有 runtime runners 继续共享仓库外 binary/artifact path 合同，不探测既有 `9022` listener。

## 动态运行模板

需要预创建 `modes × rounds` 个空 PostgreSQL database；runner 不创建/删除 DB。

```bash
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_E01_E02_POSTGRES_URL_TEMPLATE='postgres://...@127.0.0.1:<pg-port>/{database}' \
KIRO_E01_E02_POSTGRES_DATABASES='kiro_e0102_run_01,kiro_e0102_run_02,...' \
KIRO_E01_E02_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db>' \
KIRO_E01_E02_REDIS_PREFIX='kiro_rs:e0102:<unique>' \
KIRO_E01_E02_ROUNDS=3 \
node feature/tests/scheduler-fairness-sticky-race.mjs
```

可缩小到某类策略：

```bash
KIRO_E01_E02_MODES='balanced,weighted_least_inflight' \
KIRO_E01_E02_POSTGRES_DATABASES='kiro_e0102_bal_01,kiro_e0102_bal_02,kiro_e0102_bal_03,kiro_e0102_weight_01,kiro_e0102_weight_02,kiro_e0102_weight_03' \
...
```

动态验收仍按 [reverification matrix](../tests/reverification-matrix.md)：

- E01：priority/balanced/health/weighted，每策略 3 轮，记录峰值 in-flight、选择数、分布、外部池 hit。
- E02：sticky、lease race、其他账号有空槽，每策略 3 轮；sticky 不突破容量，竞争失败必须重选。
- 所有 case 结束后 service ports released、temp removed、Redis owned prefixes remaining=0、external hits=0。

## 发布状态

该证据只关闭“runner 不会误用 Docker/Cargo/根 target/共享 Redis”的安全合同子项。E01/E02 动态分布公平、两实例 fault/fallback、external takeover dynamic、真实 upstream/CLI、UI、upgrade 和 final inventory 仍然阻断发布。
