# External Takeover For SchedulerRedisDegraded

Date: 2026-07-21 / updated 2026-07-22

Status: `focused-code-path-pass / non-Docker-runner-contract-pass / dynamic-service-enabled-disabled-pass / broader-two-instance-and-final-release-pending`

Scope: 证明 `SchedulerRedisDegraded` 分类在配置允许时会进入外部池接管路径，并为后续真实服务动态验证提供不依赖 Docker、不触碰 `9022`、不自动编译的 runner。

## 结论

当前源码层面的外部池接管路径已经具备三层保护：

1. `SchedulerRedisDegraded` 不再被普通 local-memory `dispatchable > 0` 估计压制 fallback；只有真实 local ready/dispatchable 状态才保持 strict local-first。
2. 是否 fallback 由独立开关 `fallbackOnSchedulerRedisDegraded` 决定；关闭时应返回脱敏的本地不可用错误，开启且存在 eligible external pool 时才进入 external。
3. external 接管验证程序已就绪，且静态/合同测试证明它不会调用 Docker/Cargo、不会探测既有 `9022`、不会使用 Redis DB0 或共享 `kiro_rs:local` prefix。

2026-07-22 追加了真实临时服务动态验证：使用仓库外冻结 `kiro-rs` binary、loopback PostgreSQL 临时库、loopback Redis DB13、fake local Kiro upstream、fake external upstream 与 Redis chaos proxy。开启 `fallbackOnSchedulerRedisDegraded` 时，Redis 500ms 延迟下注入的 degraded 请求由 external pool 接管；关闭该开关时，请求按预期 fail closed，且不打本地或外部 upstream；移除延迟后恢复到本地账号路由。

这仍不是最终发布通过证据。它关闭的是“单实例、fake upstream、scheduler Redis degraded 外部接管正/负向动态路径”。两实例联合故障、真实上游/CLI 全能力、生产高基数和最终 release inventory 仍需独立通过。

## 源码路径复核

复核对象：

- `src/model/config.rs`
  - `fallback_on_scheduler_redis_degraded` 当前默认值为 true。
  - v5 migration 对旧 broad external fallback 配置做一次兼容恢复。
  - external capacity wait `0` 会迁移/归一为有界等待，不再无限等待。
- `src/anthropic/handlers.rs`
  - `local_pool_route_fallback_reason` 只有在 `fallback_on_scheduler_redis_degraded=true` 时返回 `local_scheduler_redis_degraded`。
  - `local_pool_fallback_reason_for_fresh_state` 对 `SchedulerRedisDegraded` 做独立处理，不让本地内存估计的 `dispatchable > 0` 抑制 external fallback。
  - `fallback_after_local_error_outcome_with_diagnostics` 在 local error 后重新读取 fresh local state，并重新检查 external pool eligibility 后再路由。
- `src/anthropic/handlers/tests.rs`
  - `external_fallback_classifier_respects_scheduler_fallback_toggles`
  - `local_pool_preflight_reason_respects_scheduler_fallback_toggles`
  - `fresh_local_pool_state_blocks_external_while_any_local_account_is_dispatchable`
  - `all_parsed_external_fallback_entrypoints_share_model_and_body_mode_eligibility`

## Focused Rust 证据

Case ID: `external-takeover-focused-20260721-r2`

命令批次：

```bash
feature/tests/run-cargo-scoped.sh external-takeover-focused-20260721-r2 -- \
  cargo test anthropic::handlers::tests::external_fallback_classifier_respects_scheduler_fallback_toggles -- --exact --nocapture --test-threads=1

feature/tests/run-cargo-scoped.sh external-takeover-focused-20260721-r2 -- \
  cargo test anthropic::handlers::tests::local_pool_preflight_reason_respects_scheduler_fallback_toggles -- --exact --nocapture --test-threads=1

feature/tests/run-cargo-scoped.sh external-takeover-focused-20260721-r2 -- \
  cargo test anthropic::handlers::tests::fresh_local_pool_state_blocks_external_while_any_local_account_is_dispatchable -- --exact --nocapture --test-threads=1

feature/tests/run-cargo-scoped.sh external-takeover-focused-20260721-r2 -- \
  cargo test anthropic::handlers::tests::all_parsed_external_fallback_entrypoints_share_model_and_body_mode_eligibility -- --exact --nocapture --test-threads=1

git diff --check
```

结果：

- 4 个 exact handler/fallback tests 均通过。
- `git diff --check` 通过。
- scoped cleanup：`validation-build-cleanup scope=external-takeover-focused-20260721-r2 size_kib=1708372 available_kib=77591172 removed=true reservation_released=true`。
- 复核：没有 `external-takeover-focused-20260721-r2` 的 scoped target、reservation 或进程残留。

说明：`r1` 曾被外层工具 timeout 杀掉，留下 orphan Cargo/rustc process group、scoped target 和 reservation；只清理了该 scope 的 PGID、target 与 reservation，不动根 `target/` 或用户进程。`r2` 是有效证据。

## 新增非 Docker 动态 runner

新增文件：

- `feature/tests/external-takeover-scheduler-degraded-nondocker.mjs`
- `feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs`

设计约束：

- 调用者必须传入仓库外冻结 binary：`KIRO_RS_BINARY`。
- 调用者必须传入仓库外 artifact root：`KIRO_VALIDATION_ARTIFACT_DIR`。
- 调用者必须传入预创建、空、独占的 loopback PostgreSQL database：`KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL`，数据库名必须匹配 `kiro_external_takeover_*`。
- 调用者必须传入 loopback Redis URL，DB 必须为 `1..15` 的非零 DB：`KIRO_EXTERNAL_TAKEOVER_REDIS_URL`。
- 调用者必须传入临时 Redis prefix，且不能包含 `kiro_rs:local`：`KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX`。
- runner 只启动 fake local Kiro upstream、fake external upstream、loopback Redis chaos proxy 和一个临时 `kiro-rs` 进程。
- runner 不调用 Docker，不调用 Cargo，不使用 `target/debug` 或 `target/release` fallback，不读取/探测既有 `9022` listener。

动态 runner 预期验证：

| 模式 | 注入 | 期望 |
| --- | --- | --- |
| `KIRO_EXTERNAL_TAKEOVER_FALLBACK_ENABLED=true` | Redis proxy latency 默认 500ms，超过 capacity hot deadline | 请求 HTTP 200，文本 `external-ok`，local inference hit 为 0，external hit 为 1 |
| `KIRO_EXTERNAL_TAKEOVER_FALLBACK_ENABLED=false` | 同上 | 请求失败但公开错误脱敏，local/external hit 均为 0，返回含 request/error id |
| 恢复阶段 | 移除 Redis latency | 后续请求恢复 local 路由，稳定恢复请求均 `local-ok` |

## Runner contract 证据

命令：

```bash
node --test feature/tests/external-takeover-scheduler-degraded-nondocker.contract.test.mjs
```

结果：

```text
1..8
# tests 8
# pass 8
# fail 0
# duration_ms 447.443625
```

覆盖：

- JavaScript 语法有效。
- validate-only 接受 caller-owned loopback PG/Redis 并报告 `dockerUsed=false`、`cargoUsed=false`、`protected9022ProbeSkipped=true`。
- PostgreSQL port `9022` 预拒绝。
- Redis port `9022` 预拒绝。
- Redis DB0 预拒绝。
- 非 loopback PG/Redis 预拒绝。
- 非 `kiro_external_takeover_*` database 和共享 `kiro_rs:local` prefix 预拒绝。
- 源码扫描确认 runner 不调用 Docker/Cargo。

## 2026-07-22 动态 service 证据

候选 binary：

```text
/var/folders/.../kiro-ext-takeover.J4ohDc/candidate-release-r12/kiro-rs
SHA-256: eca8ce4eb1ebb4c1657d1894dc69d0624313b6ff28e0cba095bf845c0914d13e
```

共同边界：

- PostgreSQL：loopback `127.0.0.1:25433`，临时库名 `kiro_external_takeover_codex_20260722_r1`，每轮前由调用方 drop/create 保证空库。
- Redis：loopback `127.0.0.1:26379` DB13，runner 只清理 owned prefix，不 `FLUSHDB`。
- Docker：未使用。
- Cargo：runner 未使用。
- 受保护端口 `9022`：未探测、未触碰。
- Raw artifact：仅保留在 `/var/folders/.../kiro-ext-takeover.J4ohDc/artifacts-*`，文档记录脱敏摘要。

### Enabled 正向接管

命令形态：

```bash
KIRO_RS_BINARY=<frozen-r12-kiro-rs> \
KIRO_VALIDATION_ARTIFACT_DIR=<owned-temp-artifact-root> \
KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL=<owned-empty-loopback-db> \
KIRO_EXTERNAL_TAKEOVER_REDIS_URL=redis://127.0.0.1:26379/13 \
KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX=kiro_rs:external_takeover:codex_20260722_r16_<round> \
KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS=1 \
KIRO_EXTERNAL_TAKEOVER_REQUESTS=5 \
KIRO_EXTERNAL_TAKEOVER_RECOVERY_REQUESTS=5 \
node feature/tests/external-takeover-scheduler-degraded-nondocker.mjs
```

结果：3 个独立 clean-DB 轮次均通过。

| 轮次 | fallback | Redis latency | degraded 请求 | 期望命中 | 恢复请求 | 清理 |
| --- | --- | --- | --- | --- | --- | --- |
| artifacts-r16-enabled-clean-round-1 | enabled | 500ms | 5/5 HTTP 200 | external upstream | 5/5 local HTTP 200 | Redis remaining 0, temp removed |
| artifacts-r16-enabled-clean-round-2 | enabled | 500ms | 5/5 HTTP 200 | external upstream | 5/5 local HTTP 200 | Redis remaining 0, temp removed |
| artifacts-r16-enabled-clean-round-3 | enabled | 500ms | 5/5 HTTP 200 | external upstream | 5/5 local HTTP 200 | Redis remaining 0, temp removed |

该组证明：`fallbackOnSchedulerRedisDegraded=true` 且 external pool eligible 时，local scheduler Redis degraded 不再被 stale local-memory `dispatchable` 压制，也不会因为 external coordinator 同一 Redis 故障而直接 fail closed；请求通过 bounded emergency local lease 进入 external，恢复后回到 local。

### Disabled 负向 fail-closed

命令形态：

```bash
KIRO_EXTERNAL_TAKEOVER_FALLBACK_ENABLED=false \
KIRO_RS_BINARY=<frozen-r12-kiro-rs> \
KIRO_VALIDATION_ARTIFACT_DIR=<owned-temp-artifact-root> \
KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL=<owned-empty-loopback-db> \
KIRO_EXTERNAL_TAKEOVER_REDIS_URL=redis://127.0.0.1:26379/13 \
KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX=kiro_rs:external_takeover:codex_20260722_r17_disabled \
KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS=1 \
KIRO_EXTERNAL_TAKEOVER_REQUESTS=5 \
KIRO_EXTERNAL_TAKEOVER_RECOVERY_REQUESTS=5 \
node feature/tests/external-takeover-scheduler-degraded-nondocker.mjs
```

结果：`pass`。

关键观测：

- degraded 5/5：HTTP 429，local inference hits `0`、local auxiliary hits `0`、external hits `0`。
- 公开错误示例仅包含 Anthropic 兼容错误、retry-after 秒数、request/error id；没有 Redis、credential、fallback pool、scheduler 内部细节。
- 移除 Redis latency 后，recovery 5/5：HTTP 200，文本 `local-ok`，每次 local inference hit `1`、external hit `0`。
- recovery probe：17 次；约 8.2 秒后稳定 local recovery。
- 资源：RSS 约 27.5MiB -> 31.2MiB，FD 30 -> 31。
- 清理：round 内 Redis owned prefix 删除 34 个 key，最终 remaining 0；runner cleanup `errors=[]`、`tempRemoved=true`。

该组证明：关闭 `fallbackOnSchedulerRedisDegraded` 后不会绕过管理员策略打 external；错误是有界、脱敏、可恢复的。

## 动态 runner 多轮注意事项

早期用 `KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS=3` 在同一个 PostgreSQL database 上连续跑时出现过假红：round 2 健康检查命中新生成端口，但服务从持久 runtime config 读取了 round 1 的旧端口并监听旧端口，导致 health check timeout。该问题是 runner 隔离合同问题，不是产品请求路径红灯。

当前有效证据使用“每轮前调用方 drop/create 独占临时库”的方式执行 3 个 enabled clean round；disabled 负向也在 clean DB 上执行。后续若继续使用多 outer runner，应先扩展 runner 支持每 outer round 独立 database 或显式重写 runtime persisted port。

可重复模板：

```bash
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL='postgres://...@127.0.0.1:<pg-port>/kiro_external_takeover_<owned_empty_db>' \
KIRO_EXTERNAL_TAKEOVER_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-empty-db>' \
KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX='kiro_rs:external_takeover:<unique>' \
KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS=3 \
node feature/tests/external-takeover-scheduler-degraded-nondocker.mjs

KIRO_EXTERNAL_TAKEOVER_FALLBACK_ENABLED=false \
KIRO_RS_BINARY=/abs/outside/repo/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/abs/outside/repo/artifacts \
KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL='postgres://...@127.0.0.1:<pg-port>/kiro_external_takeover_<owned_empty_db_2>' \
KIRO_EXTERNAL_TAKEOVER_REDIS_URL='redis://127.0.0.1:<redis-port>/<nonzero-empty-db>' \
KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX='kiro_rs:external_takeover:<unique-disabled>' \
node feature/tests/external-takeover-scheduler-degraded-nondocker.mjs
```

动态结果必须同时记录：

- binary SHA-256；
- PG database 名称摘要，不记录密码；
- Redis DB/prefix；
- local/external/fake upstream hit 差值；
- degraded 时的 HTTP 状态、公开错误和 request/error id；
- 恢复阶段 local route 命中；
- service/proxy/fake upstream PID、端口释放；
- Redis prefix cleanup；
- temp root cleanup；
- RSS/FD 起峰终值。

## 发布状态

本证据关闭以下子项：

- “外部池接管验证程序是否可安全执行”的合同子项。
- handler fallback/fresh-state 分类 focused 代码路径子项。
- 单实例 fake-upstream 产品动态 enabled 接管路径。
- 单实例 fake-upstream 产品动态 disabled fail-closed 负向路径。

仍然阻断发布的项：

- 两实例 fault/fallback 与 external takeover 联合矩阵。
- 真实 Claude Code CLI/native upstream、search/MCP/image/agent 与 fault recovery。
- UI、upgrade、final inventory 和 release gates。
