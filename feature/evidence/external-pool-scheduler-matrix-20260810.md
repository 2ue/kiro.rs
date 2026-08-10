# Scheduler Validation 2026-08-10

## Conclusion

This was service-level validation with frozen binaries and loopback mock upstreams. It was not a unit-test-only oracle.

- Result: `pass`
- Frozen binary: `/tmp/kiro-current-bin.k9z1NT/kiro-rs`
- Frozen binary SHA256: `e58e116fc940593f42f81bb4cc07695bd08c39a89eaf2fee18090819d36f4e16`
- Build artifact gate: `pass`
- Temp e0102/external_matrix PostgreSQL databases after cleanup: `0`
- Temp e0102/external_matrix Redis keys after cleanup: `0`
- Protected local preview port `9022`: not used

## Local Credential Scheduler Matrix

- Run ID: `e0102-20260810060611723-28385-278ad9`
- Result: `pass`
- Modes: `priority`, `balanced`, `health_balanced`, `weighted_least_inflight`
- Rounds per mode: `3`
- Cases: `12`
- Extra local matrix: `enabled`
- Local inference hits: `4334`
- External hits: `4`

The only external hits were the intentional extra-matrix `fallback-after-local-403` cases in each mode's first round. No external pool was used while local capacity existed outside those allowed fallback fixtures.

### Local Coverage

- Normal distribution across all configured local scheduling modes.
- Sticky session binding across two service instances.
- Long/short mixed requests with per-account concurrency caps.
- Cross-instance Redis slot race under held upstream requests.
- Controlled local `500`, `429`, and `403` responses.
- Recovery after transient local failures.
- Repeated local `403` auto-disable.
- Local failure followed by configured external fallback.
- Weighted capacity in `weighted_least_inflight`.
- Priority behavior in `priority` without leaking into lower-priority credentials during release propagation.

### Local Case Summary

| Case | External Hits | Race Reselect Logs | Primary Queue Peak | Secondary Queue Peak |
| --- | ---: | ---: | ---: | ---: |
| `E0102-priority-R1` | 1 | 108 | 2 | 0 |
| `E0102-priority-R2` | 0 | 192 | 0 | 0 |
| `E0102-priority-R3` | 0 | 268 | 1 | 0 |
| `E0102-balanced-R1` | 1 | 141 | 0 | 0 |
| `E0102-balanced-R2` | 0 | 37 | 0 | 0 |
| `E0102-balanced-R3` | 0 | 192 | 0 | 0 |
| `E0102-health_balanced-R1` | 1 | 116 | 0 | 0 |
| `E0102-health_balanced-R2` | 0 | 129 | 0 | 0 |
| `E0102-health_balanced-R3` | 0 | 109 | 0 | 0 |
| `E0102-weighted_least_inflight-R1` | 1 | 204 | 0 | 0 |
| `E0102-weighted_least_inflight-R2` | 0 | 100 | 0 | 0 |
| `E0102-weighted_least_inflight-R3` | 0 | 110 | 0 | 0 |

## External Pool Scheduler Matrix

- Run ID: `external-matrix-1786342616834-83771-8542b5`
- Result: `pass`
- Scenarios: `20`
- Requests per normal scenario: `16`
- Max concurrency: `8`

### External Coverage

- Direct external policy disabled falls back to local first.
- Local transient failure can fallback to external when configured.
- Local capacity fallback with external failure can rescue back to local only when configured.
- Direct external route never rescues back to local after external failure.
- Normal stream and non-stream external forwarding.
- Per-pool route allow/deny.
- 500, 429, 403, sustained failure, intermittent failure, and recovery.
- Same-pool retry and cross-pool failover.
- Stream pre-output retry, post-output no replay, and idle-before-commit behavior.
- External pool concurrency saturation and backup takeover.
- All-pools persistent 500 returns bounded 502 errors.

### External Case Summary

| Scenario | Statuses | Primary | Backup A | Backup B | Local Hits |
| --- | --- | ---: | ---: | ---: | ---: |
| `direct_policy_disabled_uses_local_first` | `200:12` | 0 | 0 | 0 | 12 |
| `local_transient_then_external_fallback` | `200:12` | 12 | 0 | 0 | 1 |
| `local_capacity_external_fail_local_rescue` | `200:8` | 8 | 8 | 8 | 8 |
| `local_capacity_external_fail_no_local_rescue` | `502:8` | 8 | 8 | 8 | 0 |
| `direct_external_fail_never_local_rescue` | `502:8` | 16 | 16 | 16 | 0 |
| `normal_stream_primary` | `200:16` | 16 | 0 | 0 | 0 |
| `normal_non_stream_primary` | `200:16` | 16 | 0 | 0 | 0 |
| `single_pool_route_block_falls_to_backup` | `200:8` | 0 | 8 | 0 | 0 |
| `priority_500_stream_failover` | `200:24` | 16 | 24 | 0 | 0 |
| `recovery_after_500_backoff` | `200:8` | 8 | 0 | 0 | 0 |
| `rate_limit_429_non_stream_failover` | `200:18` | 12 | 18 | 0 | 0 |
| `auth_403_auto_disable_failover` | `200:8` | 2 | 8 | 0 | 0 |
| `intermittent_500_mixed_stream` | `200:30` | 8 | 25 | 0 | 0 |
| `slow_first_byte_stream` | `200:8` | 8 | 0 | 0 | 0 |
| `stream_idle_before_commit_no_takeover` | `200:8` | 8 | 0 | 0 | 0 |
| `stream_pre_output_error_retry` | `200:8` | 2 | 8 | 0 | 0 |
| `stream_post_output_error_no_replay` | `200:6` | 6 | 0 | 0 | 0 |
| `concurrency_saturation_uses_backup` | `200:16` | 1 | 15 | 0 | 0 |
| `sustained_500_with_backup_rpm` | `200:20` | 2 | 20 | 0 | 0 |
| `all_pools_persistent_500` | `502:8` | 16 | 16 | 16 | 0 |

## Focused Rust Regression

- `cargo fmt --check`: `pass`
- `cargo check --locked`: `pass`
- `cargo test local_rescue --locked`: `7 passed`
- `cargo test external_pool --locked`: `292 passed`
- `cargo test priority_mode --locked`: `2 passed`
- `cargo test test_scheduler_handles_500_daily_credentials_1000_rpm_simulation --locked`: `1 passed`
- `cargo test forty_by_fifteen_with_global_five_hundred_queues_without_disabling_for_five_rounds --locked`: `1 passed`

All Cargo commands were run through `feature/tests/run-cargo-scoped.sh`, and every scoped target was removed by the wrapper.
