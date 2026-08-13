# 高并发低 RPM 与 runtime quarantine 聚焦证据

Date: 2026-07-17

Source: dirty tree based on `401473ca1649997bdeccf4468e3add1bdb187248` / v0.0.109

Status: focused + isolated storage + frozen r8 + non-Docker Redis chaos pass; two-instance/native-upstream gates remain open

## 代码合同

- `runtime_persistence_degraded` 只表示 PgSQL mutation 需要按 FIFO、operation ID 和 generation 重放。
- `runtime_persistence_quarantined` 单独表示待重放队列含显式 Disable，或包含 credential disabled、禁用原因、generation 推进的调度状态 Patch，未确认前不能继续调度。
- Success、ApiFailure、RefreshFailure 的首次 PgSQL 写入失败不再仅因 backlog 将账号设为 disabled；本地 failure count 达 3 次真实阈值时仍按原合同禁用。
- FIFO、generation fencing、显式禁用和 Admin patch 的 fail-closed 语义保留。

## C0

Scope `runtime-quarantine-c0-focused-r1`：

```text
cargo fmt --all                       PASS
git diff --check                      PASS
cargo check --all-targets             PASS
test binary link                      PASS
size_kib=2020100
removed=true
reservation_released=true
```

该 scope 的两个 test filter 使用了短名称并附带 `--exact`，实际均为 `running 0 tests`，不得记为行为通过。

Scope `runtime-quarantine-focused-r2` 使用了错误模块前缀 `kiro::token_manager::manager_tests`，两个 filter 仍为 `running 0 tests`。该 scope 仅证明 test binary 可链接，`size_kib=1675412`，退出 `removed=true / reservation_released=true`。

真实模块由 `src/kiro/token_manager/manager.rs` 的 `#[path = "manager_tests.rs"] mod tests;` 定义，所以精确前缀是 `kiro::token_manager::manager::tests`。

## 有效聚焦测试

Scope `runtime-quarantine-focused-r3`，Rust 1.92.0：

| Exact filter | Cargo count | 内部轮次 | 结果 |
| --- | ---: | ---: | --- |
| `kiro::token_manager::manager::tests::non_terminal_runtime_persistence_backlog_does_not_false_disable_pool_for_five_rounds` | 1 | 5 | PASS |
| `kiro::token_manager::manager::tests::forty_by_fifteen_with_global_five_hundred_queues_without_disabling_for_five_rounds` | 1 | 5 | PASS |
| `kiro::token_manager::manager::tests::test_global_capacity_limits_dispatch_and_bounds_wait_queue` | 1 | 1 | PASS |
| `kiro::token_manager::manager::tests::test_fail_fast_global_capacity_full_returns_without_queueing` | 1 | 1 | PASS |
| `kiro::token_manager::manager::tests::test_local_pool_route_state_auto_heals_too_many_failures` | 1 | 1 | PASS |

每个命令均明确输出：

```text
running 1 test
1 passed; 0 failed; 0 ignored
```

资源收尾：

```text
size_kib=1676672
removed=true
reservation_released=true
```

## 已证明与未证明

已证明：非终态 persistence backlog 不再把 40 个健康账号变成 `AllDisabled`；显式 Disable/调度状态 Patch 仍 quarantine；global 500 是独立于逐账号 600 理论总容量的真实瓶颈；释放容量能唤醒 bounded waiter；global unlimited 时第 501 个 weight=1 请求不排队。

未证明：真实 PgSQL pool acquire timeout、Redis+PgSQL 联合抖动、external preflight/fallback、100 秒慢流 TTFB、RSS/FD、跨实例最终一致性和冻结 release 性能。按用户要求不执行 Docker；storage-backed 程序已保留为显式环境 gate，不能把未运行写成 PASS。

## 2026-07-18 Patch 语义补充

后续逐调用点审计发现 `persist_runtime_patch_best_effort_until` 的两个生产调用均用于刷新成功后清零 `refresh_failure_count`。旧 `requires_dispatch_quarantine()` 把所有 `Patch` 都返回 true，因此即使前一轮已经修复 Success/ApiFailure/RefreshFailure，PgSQL 恢复窗口内仍可由自动健康 Patch 逐账号制造假禁用。

当前字段级合同：failure/refresh/warmup/last-used/expected-generation-only Patch 不 quarantine；credential disabled、disabled reason Set/Clear 或 `advance_generation` Patch 必须 quarantine。

Scope `runtime-quarantine-patch-semantics-r4`：

| Exact filter | Cargo count | 内部轮次 | 结果 |
| --- | ---: | ---: | --- |
| `runtime_patch_quarantine_is_field_semantic_for_five_rounds` | 1 | 5 | PASS |
| `non_terminal_runtime_persistence_backlog_does_not_false_disable_pool_for_five_rounds` | 1 | 5 | PASS；40 账号含自动健康 Patch |
| `forty_by_fifteen_with_global_five_hundred_queues_without_disabling_for_five_rounds` | 1 | 5 | PASS |

资源收尾：`size_kib=2017328`、`removed=true`、`reservation_released=true`。每个主 test binary 都明确输出 `running 1 test / 1 passed`；`kiro_loadtest` 的 `running 0 tests` 不计证据。

## 2026-07-20 联合压力与 Redis chaos 补证

`usage-scheduler-quarantine-r1` 在当前项目隔离 Redis 上完成 3 outer。每个
outer 依次执行 usage writer/scheduler burst（三个内部测量轮）、40 账号非终态
backlog（五轮）、40x15/60 RPM/global-500（五轮）、500 finite waiter 0 renewal。
全部通过，因此 40 账号/容量/等待场景累计各 15 个内部轮次，没有
`disabled`/`AllDisabled`、RPM-block 或 queue/renewal 泄漏。

九个 usage/scheduler 测量轮的聚合范围：writer throughput
`449.03..617.72 records/s`；writer p99 最大 `31.482 ms`；scheduler loaded
p99 最大 `55.785 ms`；recovery p99 最大 `69.204 ms`；均低于本测试当前
`250 ms` capacity hot-path bound，且 loaded/recovery 最大值也低于历史 75ms
边界。RSS end-start 最大约 `8.9 MiB`，低于 32 MiB；FD 始终为 `15`。
scope `1698452 KiB removed=true reservation_released=true`。

随后非 Docker chaos runner `scheduler-redis-chaos-r2` 使用当前项目独占空 DB15，
执行 affinity/capacity latency、连续 timeout breaker、300 lease release、
disconnect/reconnect、cancel 和 commit-unknown 共 7 tests x 3 outer，21/21
通过。50ms 请求成功，500ms 在约 250ms deadline fail closed 并恢复；所有
故障形态明确禁止 `AllDisabled`。DB15、代理、端口、temp 和 scoped target
全部清理；scope `1699724 KiB removed=true reservation_released=true`。
详见 [Redis chaos evidence](scheduler-redis-chaos-nondocker-20260720.md)。

该轮把“正常 usage/scheduler 联合 burst”和“单实例 latency/disconnect chaos”
从 open 推进为 pass；两实例、Redis+usage 同时故障、external 接管、真实上游/CLI
长流与工具/search/image/MCP、UI/browser、upgrade 和 final inventory 仍 open。
