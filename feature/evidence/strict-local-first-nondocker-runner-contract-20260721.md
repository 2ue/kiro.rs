# Strict Local-First E05 Non-Docker Runner Contract 2026-07-21

Role: 记录 E05 strict local-first 全矩阵 runner 从旧 Docker/Toxiproxy 入口改为 caller-owned PostgreSQL/Redis 入口后的合同验证。

Status: `runner-contract-pass / dynamic-service-run-pending / NO-GO`

## 结论

`feature/tests/strict-local-first-routing.mjs` 已不再是默认禁用的 legacy Docker runner。当前脚本保留旧 E05 的 10 类路由断言，但运行依赖改为调用方提供的隔离资源：

- 仓库外冻结 `kiro-rs` binary：`KIRO_RS_BINARY`。
- 仓库外 owned artifact root：`KIRO_VALIDATION_ARTIFACT_DIR`。
- caller-owned PostgreSQL URL template：`KIRO_E05_POSTGRES_URL_TEMPLATE`，必须且只能包含一个 `{database}` 占位。
- caller-owned、预创建、建议为空的 PostgreSQL database 列表：`KIRO_E05_POSTGRES_DATABASES`，数量必须等于 `modes × rounds`，每个名称必须是 `kiro_e05_*`。
- loopback Redis DB1..15：`KIRO_E05_REDIS_URL`。
- caller-owned Redis prefix：`KIRO_E05_REDIS_PREFIX`。

runner 不启动 Docker，不创建 PostgreSQL database，不 `FLUSHDB`，不调用 Cargo，不探测或触碰受保护端口 `9022`。Redis 故障注入改用仓库内 `feature/tests/redis-chaos-proxy.mjs`；结束后只扫描并删除 `KIRO_E05_REDIS_PREFIX:*` 下的 owned keys。runner child process 使用最小环境，不继承整份 `process.env`，避免 caller-owned PG/Redis URL 进入 validation child environment。

这关闭的是 E05 runner 安全合同，不是 E05 产品动态 pass。动态执行仍需要冻结 binary 和调用方预创建的空 PostgreSQL databases。

## 覆盖的 E05 模式

默认仍覆盖 10 类模式，每类默认 3 轮、每轮 5 请求：

- `no_credentials`
- `all_disabled`
- `unsupported_model`
- `local_all_cooling`
- `local_capacity_full`
- `scheduler_redis_degraded`
- `scheduler_redis_chaos`
- `fallback_disabled_no_credentials`
- `external_error_no_loop`
- `local_ready_transient`

动态运行时仍会启动 fake local Kiro upstream、fake external upstream、redis-chaos-proxy 和一个临时 `kiro.rs` 服务。`local_capacity_full` 保留真实 holder 占槽；`scheduler_redis_degraded` 和 `scheduler_redis_chaos` 通过 redis-chaos-proxy 注入 latency/disconnect；`external_error_no_loop` 验证 external 失败不会形成 local/external retry loop。

## 新输入合同

默认 10 modes × 3 rounds 需要 30 个预创建数据库。可以用 mode subset 缩小运行规模，但 database 数量仍必须与 `modes × rounds` 精确一致。

示例：

```bash
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_E05_POSTGRES_URL_TEMPLATE='postgres://...@127.0.0.1:<pg-port>/{database}' \
KIRO_E05_POSTGRES_DATABASES='kiro_e05_run_01,kiro_e05_run_02,...' \
KIRO_E05_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db>' \
KIRO_E05_REDIS_PREFIX='kiro_rs:e05:<unique>' \
node feature/tests/strict-local-first-routing.mjs
```

只验证输入与安全合同，不启动服务：

```bash
KIRO_E05_VALIDATE_ONLY=1 \
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_E05_POSTGRES_URL_TEMPLATE='postgres://...@127.0.0.1:<pg-port>/{database}' \
KIRO_E05_POSTGRES_DATABASES='kiro_e05_run_01,kiro_e05_run_02,kiro_e05_run_03' \
KIRO_E05_MODES=no_credentials \
KIRO_E05_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-db>' \
KIRO_E05_REDIS_PREFIX='kiro_rs:e05:<unique>' \
node feature/tests/strict-local-first-routing.mjs
```

## 已执行验证

### 1. E05 runner 合同

命令：

```bash
node --test feature/tests/strict-local-first-routing.contract.test.mjs
```

结果：

- 6 tests。
- 6 passed。
- 0 failed。
- duration about `299 ms`。

覆盖内容：

- JavaScript 语法有效。
- `KIRO_E05_VALIDATE_ONLY=1` 可接受 caller-owned PG/Redis 输入。
- database list 数量/命名必须满足 `modes × rounds` 与 `kiro_e05_*`。
- Redis DB0 与 protected port `9022` 在 runtime work 前拒绝。
- `kiro_rs:local` 共享 prefix 在 runtime work 前拒绝。
- 源码不包含 `spawnSync('docker')`、`CREATE DATABASE`、`FLUSHDB`、`KIRO_E05_ALLOW_DOCKER` 旧 opt-in 或 validation child `...process.env` 继承；源码必须包含 `minimalChildEnv`、`redis-chaos-proxy.mjs`、`cleanupOwnedRedisKeys` 和 `KIRO_E05_VALIDATE_ONLY`。

红绿补充：

- 最小环境断言加入后，E05 合同先红于 `startService` 仍将整份 `process.env` 传给 child。
- 修复为 `env: { RUST_LOG: 'info', KIRO_API_KEY: '' }` 后，`strict-local-first-routing.contract.test.mjs` 复跑 6/6 passed。

### 2. 共享 runtime path 合同

命令：

```bash
node --test feature/tests/runtime-validation-paths.test.mjs
```

结果：

- 9/9 passed。

确认所有 runtime runner 仍只接受仓库外冻结 binary 和仓库外 artifact root，不使用 `target/debug` 或 `target/release` 直接输出，不探测既有 `9022` listener。

### 3. 相邻 runner 回归

命令：

```bash
node --test feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs
node --test feature/tests/scheduler-fairness-sticky-race.contract.test.mjs
```

结果：

- external takeover contract：8/8 passed。
- E01/E02 scheduler fairness contract：7/7 passed。

说明本次 E05 runner 改造没有破坏 external takeover 和 E01/E02 的非 Docker 入口合同。

### 4. 合同组合与 diff hygiene

命令：

```bash
node --test \
  feature/tests/strict-local-first-routing.contract.test.mjs \
  feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs \
  feature/tests/scheduler-fairness-sticky-race.contract.test.mjs

git diff --check
```

结果：

- 合同组合：30/30 passed，0 failed。
- `git diff --check`：passed。

### 5. 构建产物 inventory

命令：

```bash
node feature/tests/inventory-build-artifacts.mjs --gate
```

结果仍为预期 release-gate fail：

- `targets=1`。
- `reservations=0`。
- `target_processes=1`。
- `blockers=2`。
- blocker 为 `<repo>/target`，约 `725148 KiB`，由 PID `84264` 的 `kiro-runtime` 引用。

这不是本次 E05 runner 合同测试产生的产物；本轮没有运行 Cargo，也没有生成 scoped target。按当前约束，不能停止该用户服务或删除其占用的根 `target/`。

## 本轮未做的事

- 未运行 Cargo。
- 未启动 Docker。
- 未启动 PostgreSQL/Redis 容器。
- 未创建或删除 PostgreSQL database。
- 未执行 E05 动态 service run。
- 未触碰 `127.0.0.1:9022`。
- 未读取或暂存 `kiro_idc_users*.txt`。

因此不能声明 E05 产品门禁通过。后续动态 pass 必须使用当前候选冻结 binary、预创建空 PG databases、loopback Redis DB/prefix，并记录每 mode/round 的 local/external hits、request IDs、TTFB/total latency、RSS/FD、Redis prefix cleanup 和 binary SHA 稳定性。

## 动态验收条件

动态执行通过后，E05 才能从 runner-contract pass 升级为 product pass。验收必须至少包含：

- 本地仍 ready 且有可调度账号时，external hits 必须为 0。
- `NoCredentials`、`AllDisabled`、`UnsupportedModel`、`AllCoolingDown`、`CapacityFull`、`SchedulerRedisDegraded` 只在对应 fallback 开关允许时进入 external。
- `fallback_disabled_no_credentials` 必须规范失败，不调用 external。
- `external_error_no_loop` 必须每请求只调用 external 一次，不回环本地/外部重试。
- `scheduler_redis_chaos` latency/disconnect 后必须恢复到 local，并记录恢复时间。
- 公开错误不得暴露 Redis、credential、scheduler、external pool 等内部路由状态。
- 所有临时服务、端口、temp root 和 owned Redis prefix 清理完成。

## 残余风险

该 runner 的合同改造只证明验证程序符合当前“不跑本地 Docker 动态验证”的约束。它不证明：

- E05 全矩阵已经在当前候选 binary 上通过。
- external takeover enabled/disabled 动态已经通过。
- E01/E02 分布公平和 sticky/lease race 已通过。
- 两实例真实服务 fault/fallback、真实 upstream/Claude CLI、多能力 native tool/search/image/MCP/agent、UI、upgrade 和 final inventory 已通过。

发布状态保持 `NO-GO`。
