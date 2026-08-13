# Redis Usage Writer Atomicity Evidence - 2026-07-18

Status: `focused PASS / isolated-Redis and normal scheduler-burst PASS / faulted-combined-chaos and release NO-GO`

Repository: `/Users/yuanfeijie/Desktop/procode/kiro.rs`

HEAD: `401473ca1649997bdeccf4468e3add1bdb187248` (`v0.0.109`, dirty tree)

Toolchain: Rust `1.92.0`

## Scope

This evidence covers only the Redis usage writer changes in `src/storage/redis_cache.rs` and the bounded Redis batch behavior in `src/anthropic/usage.rs`. It does not certify PostgreSQL usage persistence, a frozen release binary, production Redis capacity, scheduler chaos, or the final release.

## Invalid First Attempt

`usage-summary-atomic-c0-r1` used a tool-level one-second timeout. The tool killed the wrapper before Cargo could produce evidence. It left:

- one 16 KiB `.validation-build-usage-summary-atomic-c0-r1...` directory;
- one empty `.reservation-tmp-...66312...` directory created before the atomic reservation rename;
- no live Cargo/rustc process and no active reservation.

The wrapper reaper removed the owned target (`removed=true`). It correctly refused the marker-less temp reservation; after PID/time/ownership inspection, that exact empty directory was removed and the reaper returned:

```text
active=0 removed=0 failed=0
reservation_active=0 reservation_removed=0 reservation_failed=0
```

This attempt is excluded from all compile and behavior counts.

## Compile Gate R2

Command:

```bash
feature/tests/run-cargo-scoped.sh usage-summary-atomic-c0-r2 -- \
  env RUSTUP_TOOLCHAIN=1.92.0 bash -lc \
  'cargo fmt --all && git diff --check && cargo check --all-targets'
```

Result:

```text
exit=0
Finished dev profile
size_kib=447380
removed=true
reservation_released=true
```

This proved the initial two-RTT implementation compiled, but subsequent review found the cancellation gap between snapshot and aggregate. R2 is therefore compile history, not evidence for the final one-RTT behavior.

## Focused Gate R3

Command:

```bash
feature/tests/run-cargo-scoped.sh usage-summary-atomic-c0-r3 -- \
  env RUSTUP_TOOLCHAIN=1.92.0 bash -lc '
    cargo fmt --all &&
    git diff --check &&
    cargo check --all-targets &&
    cargo test storage::redis_cache::tests::guarded_usage_script_commits_snapshot_aggregate_and_seen_in_order_for_five_rounds -- --exact --nocapture &&
    cargo test anthropic::usage::tests::bounded_usage_batch_uses_one_shared_deadline_without_waiter_fanout -- --exact --nocapture
  '
```

Result:

```text
guarded_usage_script... running 1 test / 1 passed
bounded_usage_batch...  running 1 test / 1 passed
scope size_kib=2016696
removed=true
reservation_released=true
```

Each test contains five internal rounds. The additional `running 0 tests` lines came from the unrelated `kiro_loadtest` test binary after Cargo applied the exact filter; they are not counted as passes and do not invalidate the main test binary's explicit `running 1 test` results.

## Verified Contracts

1. The cache-read cardinality guard precedes every snapshot and aggregate write.
2. Snapshot, records index, aggregate commands and seen marker are encoded in one EVAL.
3. The seen marker appears exactly once and after the aggregate command loop.
4. Pre-marker command errors route through derived-cache invalidation.
5. Redis batch operations run one at a time under one shared deadline rather than creating 64 concurrent waiters and timers.
6. Accepted-path test instrumentation now counts one Redis RTT.
7. `cargo check --all-targets` compiles the five-round real Redis WRONGTYPE and cardinality programs.

## Isolated Redis Dynamic Gate

2026-07-18 使用当前仓库专属隔离 Redis 容器 `kiro-final-20260718-redis` 执行真实 Redis 动态门禁。该容器只服务本轮验证；未使用生产 Redis，未访问 `127.0.0.1:9022`。

初次组合 storage 批次在 `redis_usage_summary_cache_read_cardinality_is_hard_capped_for_five_rounds` 第 2 轮红。根因不是产品错误：第 1 轮 overflow 正确设置 `USAGE_DERIVED_CACHE_INVALIDATED_KEY`，而 `clear_usage_summary()` 设计上不清这个 fail-closed sentinel；第 2 轮复用同一 Redis namespace 时继续 fail-closed。产品语义应保留这个 sentinel，不能为了测试清理而让 cleanup 后重新使用派生缓存。

修复方式是新增 `#[cfg(test)]` helper `clear_usage_derived_cache_invalidation_for_test()`，仅在测试轮次开始/重试前清除该测试 namespace 的派生缓存 invalidation sentinel。产品 `clear_usage_summary()` 未改。

最终动态门禁：

```text
scope=redis-usage-writer-real-20260718-rerun
outer_rounds=3
tests_per_round=2
each_test_internal_rounds=5
result=6/6 top-level invocations passed
cleanup=size_kib=1698820 removed=true reservation_released=true
```

覆盖：

- `redis_usage_summary_cache_read_cardinality_is_hard_capped_for_five_rounds`；
- `redis_usage_summary_partial_command_error_never_sets_seen_for_five_rounds`。

有效结论：

1. cache-read exact-token bucket 超过 4096 时按设计 fail closed，并阻止派生缓存写入；
2. Redis partial command error 不会写入 seen marker；
3. 测试轮次复用隔离 Redis namespace 时不会被上一轮 fail-closed sentinel 污染；
4. scoped target 和 reservation 均在成功后清理。

该结果关闭“真实 Redis WRONGTYPE/基数程序仅编译”的旧状态。2026-07-20 又补齐正常 usage writer 与 scheduler 热路径联合 burst；故障同时注入 usage/scheduler、跨实例或生产 p95/p99 仍未关闭。

## 2026-07-20 normal usage/scheduler burst

标准 runner 现已把
`redis_usage_writer_burst_keeps_scheduler_latency_bounded_for_three_rounds`
纳入真实 Redis gate。独立 batch `usage-scheduler-quarantine-r1` 外层执行该测试
3 次，而每次测试内部再测 3 轮，共 9 个负载轮。结果范围：

- 100-record writer throughput `449.03..617.72 records/s`；
- writer p99 最大 `31.482 ms`；
- scheduler loaded p99 最大 `55.785 ms`；
- scheduler recovery p99 最大 `69.204 ms`；
- RSS end-start 最大约 `8.9 MiB`，FD start/peak/end 恒为 `15`。

九轮均通过 250ms current capacity bound，并在负载后恢复。scope
`1698452 KiB removed=true reservation_released=true`。这关闭正常联合 burst，
但不代替同一时间给 usage 和 scheduler 注入 Redis latency/WRONGTYPE/disconnect、
两实例或生产高基数数据分布。

## Environment Boundary

Read-only discovery found none of:

```text
redis-server
redis-cli
valkey-server
keydb-server
lua
luac
luajit
```

早期 discovery 没有发现本机非 Docker Redis 工具，因此当时保持 pending。随后按用户对当前项目隔离 Docker PG/Redis 的要求，使用 caller-owned `kiro-final-20260718-redis` 执行了上述动态门禁；没有猜测或复用生产 Redis。

仍然 pending 的运行时矩阵：

- combined usage-writer/scheduler latency, disconnect and recovery matrix

The storage test harness calls `integration_test_url("KIRO_RS_TEST_REDIS_URL")`. With `KIRO_RS_REQUIRE_STORAGE_TESTS=1`, a missing URL panics instead of silently skipping. This is the required fail-closed development-program behavior, but it is not a dynamic Redis PASS.

## Performance Interpretation

The source-level operation count improved as follows:

| Version | Per-record accepted path | 64-record waiter shape | Cancellation gap |
| --- | ---: | --- | --- |
| old | 3 Redis RTT | up to 64 concurrent futures before late gate | seen could precede aggregate |
| intermediate R2 | 2 Redis RTT | 64 futures wait on one permit | snapshot could precede cancelled aggregate |
| current R3 | 1 Redis EVAL RTT | sequential shared deadline | no client-side split inside the commit unit |

This is a structural improvement, not a measured production latency claim. A larger Lua command can still occupy Redis long enough to delay scheduler commands. Release remains blocked until isolated real-Redis p50/p95/p99, throughput, drop, RSS/FD and recovery evidence exists.

## Cleanup And Safety

- No usage/protocol/load validation request was sent to `127.0.0.1:9022`. During the separately requested root-target cleanup, one read-only `/health` liveness probe returned HTTP 404 after the protected process's backing path disappeared; the process was not stopped, restarted or reconfigured.
- 本轮使用并保留当前仓库专属隔离 Docker Redis；未清理其他项目容器或 volume。
- No credential file or `kiro_idc_users*.txt` file was read or staged.
- Every valid Cargo batch used the scoped wrapper and removed its owned target.
- The root `target/` is independently recreated by the user's rust-analyzer and is not counted as scoped build output.
