# 2026-07-19 隔离 Redis/PostgreSQL storage validation 与构建残留门禁

Status: `isolated-storage-pass / artifact-gate-pass / frozen-cli-load-pending / release-NO-GO`

Date: 2026-07-19

HEAD: `401473c` (`v0.0.109`), dirty tree `118 files changed, 66763 insertions(+), 9515 deletions(-)` before this evidence document update.

## 覆盖范围

本轮验证覆盖四类已经实现但需要真实 Redis/PostgreSQL 动态证据的修复：

1. token refresh Redis leader、health claim、identity fence、stale leader、cancel-before-send 和 bucket TTL/version 语义。
2. Redis usage writer 的 cache-read 高基数硬上限和 partial Redis command error fail-closed/seen 语义。
3. external authoritative dispatch 的 static eligibility、body-mode parity、SWR、cross-instance TTL、PG lock/timeout、leader cancellation、prepare 后 revision fence、坏行隔离、runtime snapshot coalescing。
4. high-concurrency/low-RPM runtime quarantine 与 finite Redis dispatch queue 的真实 PostgreSQL pool pressure、pending mutation replay、generation fence、Redis queue deadline/cancel/degraded 语义。

本轮不覆盖 frozen release binary、真实上游、真实 Claude Code CLI、D01-D07、L1-L5、两实例、UI browser、升级 smoke 或生产 recurrence；发布状态仍为 `NO-GO`。

## 隔离依赖与安全边界

- PostgreSQL：当前仓库专属本地测试实例，loopback 端口 `50891`，URL 记录时隐藏密码。
- Redis：当前仓库专属本地测试实例，loopback 端口 `50892`。
- 所有 Cargo 命令均通过 `feature/tests/run-cargo-scoped.sh <scope> -- ...`。
- 未触碰、探测、重启或压测 `127.0.0.1:9022`。
- 未读取、改写或删除 `kiro_idc_users*.txt`。
- 原始 `/tmp/kiro-validation-20260719-*` 日志仅用于提取本摘要和 SHA-256；摘要落盘后删除原始目录。

## 命令摘要

以下命令均设置：

```bash
RUSTUP_TOOLCHAIN=1.92.0
KIRO_RS_TEST_POSTGRES_URL=postgres://kirotest:<redacted>@127.0.0.1:50891/postgres
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:50892
KIRO_RS_TEST_POSTGRES_ISOLATED=1
KIRO_RS_TEST_REDIS_ISOLATED=1
```

运行项：

```bash
./feature/tests/run-token-refresh-redis-validation.sh
./feature/tests/run-redis-usage-writer-validation.sh
feature/tests/run-cargo-scoped.sh storage-suite-real-20260719 -- \
  env RUSTUP_TOOLCHAIN=1.92.0 ... bash -lc '<external dispatch + runtime quarantine exact filters>'
```

storage suite 先执行 `cargo fmt --all -- --check` 与 `git diff --check`，再执行 exact filter 矩阵。

## 结果

| Suite | Scope | Exact invocations | Unique tests | Outer rounds | Result | Cleanup |
| --- | --- | ---: | ---: | ---: | --- | --- |
| Token refresh Redis | `token-refresh-redis-real-20260719` | 15 | 5 | 3 | PASS | `size_kib=1690676 removed=true reservation_released=true` |
| Redis usage writer | `redis-usage-writer-real-20260719` | 6 | 2 | 3 | PASS | `size_kib=1691768 removed=true reservation_released=true` |
| External dispatch + runtime quarantine storage suite | `storage-suite-real-20260719` | 69 | 23 | 6 markers, 3 external + 3 runtime | PASS | `size_kib=1690164 removed=true reservation_released=true` |

未发现 `FAILED`、`panicked`、`error: test failed` 或 `test result: FAILED`。

## exact filters

### Token refresh Redis, 3 outer rounds

- `storage::redis_cache::tests::token_refresh_redis_concurrent_begin_elects_one_leader_for_five_rounds`
- `storage::redis_cache::tests::token_refresh_redis_failure_replay_health_claim_and_identity_are_fenced_for_five_rounds`
- `storage::redis_cache::tests::token_refresh_redis_stale_leader_cannot_overwrite_success_for_five_rounds`
- `storage::redis_cache::tests::token_refresh_redis_cancel_before_send_allows_immediate_new_leader_for_five_rounds`
- `storage::redis_cache::tests::token_refresh_redis_bucket_ttl_refill_and_version_switch_hold_for_five_rounds`

### Redis usage writer, 3 outer rounds

- `storage::redis_cache::tests::redis_usage_summary_cache_read_cardinality_is_hard_capped_for_five_rounds`
- `storage::redis_cache::tests::redis_usage_summary_partial_command_error_never_sets_seen_for_five_rounds`

### External dispatch storage, 3 outer rounds

- `external_pool::tests::external_pool_static_eligibility_snapshot_singleflights_models_and_body_modes`
- `external_pool::tests::external_pool_fallback_body_mode_eligibility_is_raw_normalized_symmetric`
- `external_pool::tests::external_pool_static_snapshot_swr_avoids_pg_lock_hol_c32_c128_for_three_rounds`
- `external_pool::tests::external_pool_static_eligibility_ttl_recovers_cross_instance_changes_for_three_rounds`
- `external_pool::tests::external_pool_static_snapshot_invalidation_never_serves_old_generation_under_pg_lock`
- `external_pool::tests::external_pool_static_eligibility_pg_failure_is_negative_cached_without_rpm_fanout`
- `external_pool::tests::external_pool_authoritative_selection_pg_lock_is_typed_bounded_and_recovers`
- `external_pool::tests::external_pool_authoritative_snapshot_singleflights_c32_c128_for_five_rounds`
- `external_pool::tests::external_pool_authoritative_refresh_survives_leader_cancellation_for_five_rounds`
- `external_pool::tests::external_pool_authoritative_pg_timeout_c128_is_one_query_and_recovers_for_three_rounds`
- `external_pool::tests::external_pool_request_scoped_snapshot_is_reused_across_reselection`
- `external_pool::tests::external_pool_post_lease_revision_fence_rejects_disable_and_update_toctou`
- `external_pool::tests::external_pool_dispatch_fence_coalesces_only_in_flight_c32_c128_for_five_rounds`
- `external_pool::tests::external_pool_dispatch_fence_pg_timeout_c128_is_one_query_and_recovers_for_three_rounds`
- `external_pool::tests::external_pool_dispatch_prepares_then_fences_before_attempt_and_http_send_for_five_rounds`
- `external_pool::tests::malformed_external_pool_rows_are_isolated_and_fail_closed_for_five_rounds`
- `external_pool::tests::external_pool_selection_runtime_snapshot_coalesces_128_waiters_for_five_rounds`

### Runtime quarantine storage, 3 outer rounds

- `kiro::token_manager::manager::tests::postgres_pool_pressure_backlogs_non_terminal_success_without_quarantine_for_five_rounds`
- `kiro::token_manager::manager::tests::postgres_pending_runtime_mutations_replay_in_order_and_unquarantine`
- `kiro::token_manager::manager::tests::postgres_reset_generation_fences_pending_failure_and_disable_replay`
- `kiro::token_manager::manager::tests::finite_redis_dispatch_queue_lease_deadline_does_not_move_after_renew_interval`
- `kiro::token_manager::manager::tests::redis_dispatch_queue_waiter_fails_closed_after_coordination_degrades`
- `kiro::token_manager::manager::tests::redis_dispatch_queue_cancelled_waiter_releases_local_and_remote_lease`

## 构建残留与磁盘复核

storage suite 完成后，第一次只读 artifact inventory 报告根目录 `target/` 为 unmanaged target，大小约 `727872 KiB`，并发现一个 19 天前的 `kiro_cli_repro` Claude CLI/MCP tmux 复现会话仍以 `target/claude-cli-tests/...` 为工作目录；另有一个用户本地 `kiro-rs` 运行进程引用 `target/release/kiro-rs` 与 `target/local-verify/kiro-rs-9022.log`。

处理动作：

1. 关闭旧的 `kiro_cli_repro` tmux 验证会话及其 Claude/MCP 子进程。
2. 未杀正在运行的 `kiro-rs` 服务。
3. 确认没有进程引用 `target/debug` 或 `target/flycheck0` 后，仅删除可再生的 `target/debug`、`target/flycheck0` 和 `.rustc_info.json`。

复核结果：

```text
du -sh target -> 0B
df -h . -> Avail 84GiB
node feature/tests/inventory-build-artifacts.mjs --gate
build-artifact-inventory version=2 mode=read-only targets=0 reservations=0 target_processes=0 blockers=0
release-gate result=pass
```

删除 `/tmp/kiro-validation-20260719-*` 原始日志目录后，编辑器/flycheck 再次重建可见的 `target/debug`/`target/flycheck0`，约 `710 MiB`。复核显示仍无进程引用这些可见子目录；唯一 target 相关运行进程是已有的 `kiro-rs` 服务，引用不可见的 `target/release`/`target/local-verify` 路径。随后再次只删除 `debug`/`flycheck0`/`.rustc_info.json`，最终 `du -sh target -> 0B`，`inventory-build-artifacts --gate` 再次为 `targets=0 reservations=0 target_processes=0 blockers=0`。

本轮 artifact gate pass 只证明当前验证残留已清；它不是最终 release gate，因为 frozen binary、CLI/load raw captures、UI/upgrade 等后续批次还会产生新的临时资产，必须逐批复核。

## 原始日志 SHA-256

- `token-refresh-redis/run.log`: `93390d96ef9eece254530e61b64b901f9341dfd0e2d82fbb6769676cca8981c4`
- `redis-usage-writer/run.log`: `eca713237ea2e434717d7767255259dd6ff5f5f05427b55662b0fe477f148e43`
- `storage-suite/run.log`: `0eab3194e9fb91c39181407ade06a038efca91f6966e5abb31bccf5160630a1c`

## 结论

本轮把先前的 “compile-only / isolated storage pending” 缺口向前推进为：

- token refresh Redis：真实隔离 Redis 三轮动态 PASS。
- Redis usage writer：真实隔离 Redis 三轮动态 PASS。
- external authoritative dispatch：真实隔离 PostgreSQL/Redis 三轮动态 PASS。
- runtime quarantine / finite Redis queue：真实隔离 PostgreSQL/Redis 三轮动态 PASS。
- 构建残留：本轮 storage 验证后的 scoped target、旧 Claude CLI tmux 残留和根 debug/flycheck 已清理；只读 artifact gate PASS。

剩余发布阻断保持不变：frozen release C0、真实 Claude CLI C1-C4、thinking/tool/search/image/MCP/长会话组合、L1-L5 负载与错误风暴、两实例、UI browser、upgrade smoke、最终敏感信息/零残留/release gate 尚未完成。
