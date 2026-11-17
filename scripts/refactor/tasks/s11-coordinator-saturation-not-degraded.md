# 任务 s11：协调器信号量瞬时饱和不得降级为进程内 lease（会越过账号并发上限）

## 背景

第二轮 quick 验收矩阵 19 个用例 18 个 PASS，唯一 FAIL 是开环用例 `queue_overflow`（到达率约 53/s 冲 40 槽、队列上限 8、sub2api 式重试风暴）：mock 上游观测到 `mock-access-7002` 与 `mock-access-7003` 峰值在飞 7，账号 `maxConcurrentRequests=4`。报告 `/tmp/claude-pool-acceptance/20261117-104956-quick/queue_overflow.json`，`accountsOverConcurrency=2`。第一轮同一用例 PASS，说明是竞态。

服务日志 `/tmp/claude-pool-acceptance/20261117-104956-quick/server.log` 在 03:13:14 有 6 条：

```
WARN 账号 Redis lease 准入不可用，尝试进程内有界 lease request_id=... pool_id=2/3 detail="coordinator_breaker_open"
```

但全程**没有**一条 `账号 Redis coordinator 不可用，暂停新的账号准入`（`AccountCoordinatorBreaker::complete_failure` 的日志），即熔断器从未真正打开。

## 根因

`src/account/mod.rs`：

- `AccountCoordinatorBreaker::try_begin`（约 3362 行）有两种 `Err(AccountCoordinatorOpen)`：(a) 熔断真的打开（`open_until > now` 或恢复探测进行中）；(b) `operation_semaphore`（`ACCOUNT_COORDINATOR_MAX_IN_FLIGHT = 256`）瞬时打满。两者返回同一个类型，调用方无法区分。
- `acquire_pool_for_route`（约 9028 行）拿到 `Err` 一律返回 `PoolAcquireUnavailable { reason: CoordinatorUnavailable, detail: "coordinator_breaker_open", wait_for: Some(retry_after) }`。
- 转发循环（约 6676 行）看到 `CoordinatorUnavailable` 就调 `acquire_degraded_fallback_local_pool`，用**进程内**信号量发 lease。此时 Redis 里该账号已有 4 个真实 lease 在飞，进程内信号量是另一本账，于是账号实际在飞 = Redis lease 数 + 进程内 lease 数，越过上限。

设计契约（`docs/claude-pool-refactor/04-调度.md` 第 7 节、`11` 章 3.11）说的是"coordinator **熔断打开**时用进程内 lease"，前提是 Redis 不可用、Redis 账本不可信。信号量瞬时饱和时 Redis 完全健康，不该切账本。

## 修复

1. `AccountCoordinatorOpen` 增加字段 `kind: AccountCoordinatorOpenKind`，枚举 `{ BreakerOpen, AdmissionSaturated }`；`try_begin` 的三个 `Err` 分支分别填 `AdmissionSaturated`（信号量满）、`BreakerOpen`（`open_until > now`）、`BreakerOpen`（恢复探测进行中）。
2. `acquire_pool_for_route` 的 `Err(open)` 分支：
   - `AdmissionSaturated` → 返回 `PoolAcquireUnavailable { reason: PoolCapacityWaitReason::Full, wait_for: Some(open.retry_after /* 100ms */), exclude_pool_for_reselect: false, detail: "coordinator_admission_saturated" }`。`Full` 走现有 `handle_capacity_unavailable` 的等待/排队路径（受 `dispatchMaxWaitSecs` 约束），短睡后重试，**不**降级。
   - `BreakerOpen` → 保持现状（`CoordinatorUnavailable` + `coordinator_breaker_open`）。
3. 转发循环 6676 行附近那条 WARN 的文案改为 `账号 Redis coordinator 熔断已打开，改用进程内有界 lease`（它现在只会在真熔断时出现）。
4. `/readyz` 的 `dispatch` 增加 `coordinatorAdmissionSaturated: <累计次数>`（`coordinator_breaker.stats.saturated`），方便验收时看到饱和是否发生。`DispatchStatus` 结构与 `readyz_dispatch_value` 同步加字段，现有 readyz 单测补断言该字段存在且为 0。
5. 另外 `load_pool_runtime_snapshots`（约 9446 行）那条 `try_begin().map_err(...)` 同样区分：`AdmissionSaturated` 时错误文案改为 `account coordinator admission saturated; retry after ...ms`，并且调用方（约 8277 行把它当 `RedisError` 置 `coordinator_unavailable=true`）在 `AdmissionSaturated` 时改为设置 `wait_reason=Some(Full)`、`wait_for=Some(100ms)`、`coordinator_unavailable=false`、`degraded_fallback_local_lease=false`。若该处现有结构不便传递 kind，可在 anyhow 错误上用 `downcast_ref::<AccountCoordinatorOpen>`（给它派生 `Debug + Display + std::error::Error`）。

## 回归单测（`src/account/tests.rs`）

- `coordinator_admission_saturation_waits_instead_of_degrading_to_local_lease`：用 `AccountCoordinatorBreaker::new_with_min_backoff(1, ...)`（信号量 1）替换 manager 的 `coordinator_breaker`（已有测试 4551 行附近演示了构造方式；若 manager 字段不可直接替换，加一个 `#[cfg(test)] fn with_coordinator_breaker(self, ...)`），先 `try_begin()` 持有一个 permit 不放，再对一个 `maxConcurrentRequests=1` 的账号并发发 3 个请求（fake 上游首包延迟 500ms）：断言期间 mock 上游最大在飞 ≤ 1（用现有 `ExternalStreamFakeServer` 的计数或 `/accounts/status` 的 `inFlight`）、没有 `degraded-fallback-local` epoch 的 lease（`AccountLease.coordination_epoch` 不等于该字符串；可通过 `manager.degraded_fallback_local_leases` 为空断言）、释放 permit 后 3 个请求全部成功。
- `coordinator_breaker_truly_open_still_degrades_to_bounded_local_lease`：对照组，用 `failure(Timeout)` 真把熔断打开，断言走进程内 lease 且 `degraded_fallback_local_leases` 非空、并发仍 ≤ 账号上限。
- readyz 单测补 `coordinatorAdmissionSaturated == 0`。

## 允许改动的文件

`src/account/mod.rs`（只改上面列出的点）、`src/account/tests.rs`、`src/main.rs`（readyz 字段）。**不得改** `src/anthropic/**`、`src/account/usage_projection.rs`、`src/account/golden_tests.rs`、`tests/golden/**`、调度选择算法（`select_account_candidate` 及评分）。

## 自检

```
cargo fmt --all
cargo build -q --release --bins
cargo test --bin claude-pool -q -- coordinator
cargo test --bin claude-pool -q -- readyz
cargo test --bin claude-pool -q -- golden_usage
cargo test --bin claude-pool -q 2>&1 | tail -3
KEEP=0 scripts/loadtest/run-acceptance-matrix.sh overload queue_overflow spike_sub2api
```
最后一条必须 `总结果：PASS` 且 `overload-verdict.log` 两个用例 `accountsOverConcurrency=0`；把 SUMMARY.md 路径与该用例报告里的 `accountMaxInFlight` 写进汇报。release 二进制在 `/tmp/kiro-rs-rust-analyzer-target/release/claude-pool`，矩阵用的是它，所以 `cargo build --release` 必须先完成。
