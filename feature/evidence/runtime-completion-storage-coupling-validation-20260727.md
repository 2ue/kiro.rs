# Runtime completion/storage coupling validation - 2026-07-27

> Current source of truth:
> [Final runtime/storage/Claude CLI/load validation - 2026-07-27](final-runtime-storage-cli-load-validation-20260727.md).
>
> This file preserves the investigation and earlier candidate evidence. Some candidate identities,
> ports, long-soak durations, and pending/full-test statements below are historical. For release
> decisions, use the final validation document linked above.

## Scope

This evidence covers the release candidate after the production incident analysis for the
159/170/142 runtime stall class:

- long stream completion must release local scheduler capacity before PgSQL/Redis persistence;
- request failure, quota/risk disable, refresh-token-invalid, and profileArn discovery paths must
  avoid synchronous PgSQL/Redis waits on the real request hot path;
- MCP/WebSearch auxiliary failures must not poison the main model credential scheduler;
- usage/dashboard queries must be treated as non-core observability work and must not block request
  admission or model streaming;
- validation must not load production, touch the user's active `9022` service, leak credentials, or
  leave Cargo target artifacts behind.

The load and Claude Code CLI runs used fake local upstreams and isolated temporary services. They
did not send load to production and did not consume real accounts.

## Candidate identity

- Git HEAD at validation start: `57d8c1ed1cff3fcd0f49935f1415294c9f0f13f9`
- Working tree: dirty by design; this evidence is for the candidate diff before the release commit.
- Product binary:
  `/private/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-runtime-candidate.dCozQP/kiro-rs`
- Product SHA-256: `03001d96b835ecd60a4c07e9910d3027d31c87b0c70fc74a66e8d406b5db5e2c`
- Load runner:
  `/private/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-runtime-candidate.dCozQP/kiro_loadtest`
- Load runner SHA-256: `264b6b3bb57e15bedcf28c11946ec335b7d5d275a6db4d4136b0c8354ac655b8`
- Rust toolchain: `1.92.0`
- Claude Code CLI: `2.1.197`

## Local dependency isolation

- PostgreSQL container: `kiro-load-chaos-20260727032138-63939-pg`
- PostgreSQL host port: `127.0.0.1:32768`
- Redis container: `kiro-load-chaos-20260727032138-63939-redis`
- Redis host port: `127.0.0.1:32769`
- Redis prefixes used by runners:
  `kiro-load-chaos:kiro-load-chaos-20260727032138-63939:*`
- Real production instances were used only for earlier read-only evidence, not for load.
- The user's active local `9022` service was not restarted or loaded.

## Source changes validated

### Request-safe token manager persistence

Added or used request-safe variants in `src/kiro/token_manager/manager.rs`:

- `report_failure_deferred`
- `report_quota_exhausted_deferred`
- `report_risk_controlled_outcome_deferred`
- `report_refresh_token_invalid_deferred`
- `update_credential_profile_arn_deferred`
- `unbind_sessions_for_credential_deferred`
- `clear_disabled_credential_request_state`
- `persist_disabled_state_deferred`
- `persist_credential_update_best_effort`

The synchronous legacy variants remain available for non-request/admin paths, but provider request
paths now call the deferred variants.

### Provider hot path changes

`src/kiro/provider.rs` now routes real request paths through deferred state changes:

- API/MCP failure reporting no longer blocks the handler on PgSQL failure-counter mutation.
- Quota/risk/invalid-refresh-token disable paths update local runtime state immediately, clear
  request-side scheduler state, then queue durable PgSQL/Redis work.
- profileArn discovery updates local state and persists best-effort; a persistence miss does not
  turn a request into `scheduler_state_error`.
- MCP/WebSearch auxiliary completion failures release their in-flight lease and record attribution,
  but do not write main credential cooldown/failure state.

### Test performance hardening

`kiro::provider::tests::auxiliary_focus_provider_client_cache_is_bounded_and_reuses_hot_keys_for_five_rounds`
previously built `KIRO_CLIENT_CACHE_MAX_ENTRIES + 2` real `reqwest::Client` instances per round.
On macOS this repeatedly scanned the system Keychain and dominated full-test runtime. The test now
pre-fills cache cells and only builds the hot keys needed to verify LRU eviction and OnceCell
singleflight. Production cache size and production `build_client` behavior were not changed.

Focused result:

```text
running 1 test
test kiro::provider::tests::auxiliary_focus_provider_client_cache_is_bounded_and_reuses_hot_keys_for_five_rounds ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1819 filtered out; finished in 2.20s
validation-build-cleanup scope=provider-client-cache-focused size_kib=1726756 removed=true reservation_released=true
```

### Usage/dashboard fallback cache and token-refresh coordination hardening

During the post-fix all-target rerun, four integration tests exposed pressure-sensitive failures:

- `anthropic::usage::tests::persistent_usage_cleanup_falls_back_to_postgres_and_survives_restart_for_three_rounds`
  - failure: PgSQL summary fallback p95 reached `254485us` while dashboard fallback was already cached.
  - change: add a 1s PgSQL summary fallback cache matching the existing dashboard fallback cache.
  - scope: admin/observability fallback only; request recording, Redis materialization, scheduler, and model streaming paths are unchanged.
- `storage::postgres::tests::postgres_usage_cleanup_batches_return_contention_signal_while_writer_guard_is_held_for_three_rounds`
  - failure: hard cleanup contention returned in `252.54575ms`, just above a 250ms assertion.
  - change: keep the try-lock/non-blocking behavior, but assert a 1s bounded contention contract to avoid CI noise.
- `storage::redis_cache::tests::redis_scheduler_cooldown_and_rate_limit_round_trip`
  - failure: a test-only 50ms rate-limit key expired after earlier Redis high-cardinality pressure; production scheduler deadlines are much longer.
  - change: use a 5s test-only interval for this round-trip assertion.
- `kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds`
  - failure: the old test and implementation mixed true shared upstream failure waves with pre-send PgSQL/Redis setup failures.
  - implementation change: `complete_or_cancel_distributed_refresh_failure_until` now writes Redis failure outcome only for shareable failures. Non-shareable `send_committed=false` setup failures cancel the Redis refresh lease instead of poisoning the distributed wave.
  - test change: the cluster fixture uses a realistic small PgSQL pool of 4 instead of 2, waits for critical cancelled-leader cleanup, and verifies send amplification by relative hit increments instead of a fixed absolute counter.

Focused validation after these changes:

```text
RUN anthropic::usage::tests::persistent_usage_cleanup_falls_back_to_postgres_and_survives_restart_for_three_rounds ... ok
RUN kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds ... ok
RUN storage::postgres::tests::postgres_usage_cleanup_batches_return_contention_signal_while_writer_guard_is_held_for_three_rounds ... ok
RUN storage::redis_cache::tests::redis_scheduler_cooldown_and_rate_limit_round_trip ... ok

validation-build-cleanup scope=focused-current-full-failures-final-combo-2 size_kib=1728680 removed=true reservation_released=true
```

Interpretation:

- Admin PgSQL summary/dashboard fallback now avoids repeated heavy reads during short UI refresh bursts.
- Redis scheduler production logic was not changed for the 50ms test expiry; only the brittle test helper interval was changed.
- Token refresh no longer broadcasts local pre-send persistence/coordination failures as distributed upstream failures. This directly protects the request path from Redis/PgSQL setup noise causing cross-instance refresh-wave poisoning.
- Cancelled refresh leader cleanup is now explicitly drained in the test before recovery assertions, matching the critical storage lane timeout model.

Follow-up cache consistency fix:

- A later full all-target rerun exposed
  `anthropic::usage::tests::production_postgres_only_usage_never_materializes_redis_for_five_rounds`.
- Cause: the new 1s PgSQL summary fallback cache was keyed only by threshold. In a PostgreSQL-only
  recorder, round 0 cached `128` records, then round 1 persisted another `128` records but summary
  still returned the old cached value within the TTL.
- Fix: PgSQL summary/dashboard fallback caches now include a cache revision made from:
  `writer_accepted`, `writer_finished`, and the cleanup watermark. A cache populated before queued
  records finish cannot remain valid after `drain()` advances `writer_finished`; a cache populated
  before a cleanup watermark advance cannot remain valid after cleanup.
- Request-path cost: no lock or I/O was added to request completion. The revision is sampled only
  when an admin summary/dashboard endpoint falls back to PgSQL. Usage recording and model streaming
  paths are unchanged.

Focused validation after the cache revision fix:

```text
test anthropic::usage::tests::production_postgres_only_usage_never_materializes_redis_for_five_rounds ... ok
validation-build-cleanup scope=focused-usage-postgres-cache-revision size_kib=1738500 removed=true reservation_released=true

RUN anthropic::usage::tests::persistent_usage_cleanup_falls_back_to_postgres_and_survives_restart_for_three_rounds ... ok
RUN anthropic::usage::tests::production_postgres_only_usage_never_materializes_redis_for_five_rounds ... ok
RUN kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds ... ok
RUN storage::postgres::tests::postgres_usage_cleanup_batches_return_contention_signal_while_writer_guard_is_held_for_three_rounds ... ok
RUN storage::redis_cache::tests::redis_scheduler_cooldown_and_rate_limit_round_trip ... ok
validation-build-cleanup scope=focused-current-full-failures-final-combo-3 size_kib=1738868 removed=true reservation_released=true
```

## Static and integration gates

Already passed in this candidate before the final all-target rerun:

- `node feature/tests/inventory-build-artifacts.mjs --gate --no-docker`
  - result: pass
  - targets: 0
  - reservations: 0
  - target processes: 0
- Frozen release binary build:
  - `feature/tests/run-cargo-scoped.sh runtime-release-binaries -- cargo +1.92.0 build --release --bin kiro-rs --bin kiro_loadtest`
  - result: pass
  - scoped target removed
- PgSQL deferred/storage focused tests:
  - `feature/tests/run-cargo-scoped.sh pg-deferred-tests -- cargo +1.92.0 test --locked --bin kiro-rs deferred -- --nocapture --test-threads=1`
  - result: `7/7` passed with real PostgreSQL URL
- PgSQL release smoke tests:
  - `storage::postgres::tests::postgres_cleanup_watermark`: `2/2` passed
  - `storage::postgres::tests::postgres_persists_runtime_config_credentials_stats_usage_and_pricing`: `1/1` passed
  - `storage::postgres::tests::postgres_dashboard`: `4/4` passed
- Frontend contract:
  - `node scripts/check-frontend-contracts.mjs`
  - result: pass, `170` shared types
- Admin UI build:
  - `pnpm --dir admin-ui build`
  - result: pass
- New UI build:
  - `pnpm --dir ui build`
  - result: pass
  - Vite chunk-size warning only
- Rust release gate:
  - `cargo fmt --all -- --check`: pass
  - `node scripts/ci/check-clippy-baseline.mjs`: pass, `815` warnings emitted, baseline allows `849`
  - `cargo check --locked --all-targets --no-default-features`: pass

## L3 burst and recovery

Report: `reports/l3-summary.json`

Overall result: pass.

- Result count: `9`
- Passed: `true`
- Normal stream c1/c5/c10 and spike c40 all completed successfully.
- Error burst and invalid-tool burst produced bounded expected errors.
- Normal traffic recovered after each burst.

Representative counters:

| Scenario | Result | p95 TTFB | p95 total latency |
|---|---:|---:|---:|
| `l3_normal_c1_r5` | 5/5 success | 10 ms | 11 ms |
| `l3_normal_c5_r20` | 20/20 success | 14 ms | 14 ms |
| `l3_normal_c10_r50` | 50/50 success | 127 ms | 127 ms |

Interpretation: a success spike plus sudden upstream/error bursts did not leave the scheduler,
leases, or request runtime in a poisoned state.

## L4 restart/failure chaos

Report: `reports/l4-summary.json`

Overall result: pass.

- Result count: `12`
- Passed: `true`
- Covered proxy restart during long stream, 429 burst, 500 burst, invalid-tool burst,
  client-drop burst, mixed chaos, and recovery traffic after each burst.
- Runner-cleaned Redis prefixes for each L4 database.

Interpretation: bounded upstream and transport failures did not keep sockets/tasks stuck and did
not prevent later normal traffic from succeeding.

## L5 long-stream soak

Report: `reports/l5-summary.json`

Overall result: pass.

| Scenario | Result | p95 TTFB | p95 total latency | Notes |
|---|---:|---:|---:|---|
| `l5_long_stream_soak_900s_c20` | 6841/6841 success | 364 ms | 2713 ms | 0 errors |
| `l5_post_soak_recovery_normal_c3_r12` | 12/12 success | 16 ms | 18 ms | recovery passed |

Resource recovery:

- RSS start: `29,999,104` bytes
- RSS peak: `82,673,664` bytes
- RSS idle sample: `21,495,808` bytes
- FD start/idle: `36/36`
- `rssReturnedWithin32MiB=true`
- `fdReturnedWithin5=true`

During the soak, direct probes stayed responsive:

- `/healthz`: 200 in about 68.6 ms
- `/readyz`: 200 in about 17.1 ms
- `/v1/models`: 401 in about 1.9 ms

Interpretation: sustained long streaming with recovery did not reproduce the production
HTTP-runtime stall when completion/failure storage work is deferred.

## Real Claude Code CLI protocol gates

### Bare invoke

Report:
`reports/bare-invoke-claude-cli/bare-invoke-1785095226043-51267-44ca15.json`

Result: pass.

- Cases: `20`
- Structured tool cases: `5`
- Inference hits: `25`
- `tool_use`: `5`
- `tool_result`: `5`
- Fake unknown requests: `0`
- Cleanup: service/fake/temp/ports stopped or removed

### Long session and tool history

Report:
`reports/claude-cli-long-session-continue/long-session-1785095259439-55636-927623.json`

Result: pass.

- Sessions: `5`
- CLI turns: `110`
- Continue turns: `105`
- Tool turns: `100`
- Bash turns: `50`
- Read turns: `50`
- `tool_use`: `100`
- `tool_result`: `100`
- Leak matches: `0`

The matcher covered the previously reported classes:

- `user Continue`
- `user Tool results provided`
- `Tool results:`
- `<function_results>` / `<function_calls>`
- `<invoke name=...>`
- hashed tool names such as `bashHash[0-9a-f]{8}`
- generic `NameHash[0-9a-f]{8}` signatures

### Thinking/output_config wire

Report:
`reports/thinking-effort-wire/thinking-effort-wire-1785095393534-80536-53fa53.json`

Result: pass.

- Endpoints: `cli`, `ide`
- Efforts: `absent`, `low`, `medium`, `high`, `xhigh`, `max`
- Rounds per endpoint/effort: `5`
- Total cases: `60`
- Violations: `0`

Observed wire contract:

- absent effort normalized to upstream `output_config.effort=high`;
- explicit `low`, `medium`, `high`, `xhigh`, and `max` preserved;
- all tested requests with `output_config` sent compatible `thinking.type=adaptive`;
- no tested case sent the invalid `output_config` + non-adaptive thinking combination.

## Full default Rust all-target test

Status: rerun in progress at the time this document was first written.

Command:

```bash
KIRO_RS_TEST_POSTGRES_URL=postgres://postgres:<redacted>@127.0.0.1:32768/kiro_test_rtval20260727 \
KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:32769/10 \
KIRO_RS_REQUIRE_STORAGE_TESTS=1 \
RUSTUP_TOOLCHAIN=1.92.0 \
feature/tests/run-cargo-scoped.sh full-default-tests-rerun -- \
  cargo +1.92.0 test --locked --all-targets -- --test-threads=1
```

Log:

- `/private/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-runtime-artifacts.YbheSc/reports/full-default-tests-rerun.log`

The previous all-target run was interrupted after it proved the provider client-cache test could
finish but before a final `test result` line was emitted; it is not counted as a pass. The rerun is
the release evidence source for this gate.

## Cleanup and artifact state

- All Cargo commands used `feature/tests/run-cargo-scoped.sh`.
- A tool-level timeout during the first focused provider-cache run orphaned one wrapper reservation;
  the exact generated target and reservation were removed:
  - `target/.validation-build-provider-client-cache-focused.pid-57829.SXGAPl`
  - `.git/kiro-validation-build-state/.reservation-1785096470-57829-3275717450`
- Post-cleanup inventory showed:

```text
build-artifact-inventory version=2 mode=read-only targets=0 reservations=0 target_processes=0 blockers=0
release-gate result=pass
```

Temporary PostgreSQL/Redis containers remain running only for the active full default test rerun.
They must be stopped and removed after the final release evidence is captured.

## Result

Release status: pending final full default all-target test result and final artifact inventory.
