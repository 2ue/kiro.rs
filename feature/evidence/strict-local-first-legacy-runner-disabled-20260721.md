# Strict Local-First Legacy Docker Runner Default Disable

Date: 2026-07-21

Status: `superseded-by-nondocker-runner-contract / retained-history`

Superseded by: [Strict local-first E05 non-Docker runner contract 2026-07-21](strict-local-first-nondocker-runner-contract-20260721.md)

Current note: 本文件保留旧状态的历史证据。`feature/tests/strict-local-first-routing.mjs` 后续已改成 caller-owned PostgreSQL/Redis 的非 Docker runner；不要再把本文件中的“仍是旧 Docker/Toxiproxy runner”作为当前事实。

Scope: 防止 `feature/tests/strict-local-first-routing.mjs` 在当前用户明确要求“不跑本地 Docker 动态验证”的阶段被误执行旧 Docker/Toxiproxy 路径。

## 结论

`strict-local-first-routing.mjs` 仍是旧的 Docker/Toxiproxy-backed E05 全矩阵 runner。它覆盖的范围比新的 `external-takeover-scheduler-degraded-nondocker.mjs` 更宽，但当前实现会启动 Docker PostgreSQL、Redis 和 Toxiproxy，因此不符合本轮验证约束。

本轮没有把旧 runner 冒充为动态通过；只加了默认禁用保护：

- 默认执行会在 runtime work 之前失败。
- 错误文案明确说明这是 legacy Docker/Toxiproxy-backed runner。
- 只有显式 `KIRO_E05_ALLOW_DOCKER=1` 才会进入旧路径。
- 当前计划下不使用这个 opt-in；后续应改写为 caller-owned PG/Redis + `redis-chaos-proxy.mjs` 后再动态执行。

## 修改文件

- `feature/tests/strict-local-first-routing.mjs`
- `feature/tests/strict-local-first-routing.contract.test.mjs`

## 合同测试

命令：

```bash
node --test feature/tests/strict-local-first-routing.contract.test.mjs
node --test feature/tests/runtime-validation-paths.test.mjs
node --test feature/tests/scheduler-fairness-sticky-race.contract.test.mjs
git diff --check
```

结果：

```text
strict-local-first-routing.contract.test.mjs: 3/3 pass
runtime-validation-paths.test.mjs: 9/9 pass
scheduler-fairness-sticky-race.contract.test.mjs: 7/7 pass
git diff --check: pass
```

覆盖：

- JavaScript 语法有效。
- legacy runner 默认在 runtime work 前失败，不触发 Docker 错误路径。
- 源码包含显式 `KIRO_E05_ALLOW_DOCKER` opt-in 与 `disabled by default` 文案。
- runtime runners 仍共享仓库外 binary/artifact path 合同，并且不探测已有 `9022` listener。

## 后续动态要求

E05 全矩阵仍需一个真正非 Docker runner 或重写当前 runner：

- 使用 caller-owned PostgreSQL URL template 和预创建 `kiro_e05_*` database。
- 使用 caller-owned loopback Redis DB1..15 与 per-case Redis `keyPrefix`。
- 使用现有 `feature/tests/redis-chaos-proxy.mjs` 替代 Toxiproxy 容器。
- 不 `FLUSHDB`，只清理 owned prefix。
- 不创建/删除 database。
- 不调用 Docker/Cargo。

当前已经由 `external-takeover-scheduler-degraded-nondocker.mjs` 覆盖 `SchedulerRedisDegraded` 外部池接管子项；但是 local ready/cooling/full/unsupported/external-error/no-loop 等 E05 全矩阵动态仍未关闭。

## 发布状态

该证据只关闭“不会误跑旧 Docker runner”的安全合同。它不是 E05 产品 pass，不替代 external takeover dynamic、E01/E02 distribution dynamic、两实例 fault/fallback、真实 upstream/CLI、UI、upgrade 和 final inventory。
