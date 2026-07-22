# Usage Cleanup Storage Integration Evidence

Date: 2026-07-16

Status: `cleanup-36x3-and-writer-lock-order-focused-pass / dynamic-multi-instance-pending`

Build identity: working tree based on `401473c` / `v0.0.109`; unreleased dirty-tree candidate

Scope: F03 PostgreSQL persistent job/batch semantics, detail/rollup consistency, cleanup watermark, Redis invalidation and bounded pattern deletion. This is dirty-tree storage evidence, not final release-binary, browser, real CLI or production-scale chaos evidence.

Product contract: soft cleanup removes matching detail and subtracts its summary, Dashboard, cost, credential, cache-read and duration-rollup contribution exactly once. Hard cleanup physically deletes tombstones and must not subtract a normal soft-cleaned contribution again.

## Isolated infrastructure

- PostgreSQL container: `kiro-rs-validation-pg-20260716`, bound only to `127.0.0.1:47432`.
- Redis container: `kiro-rs-validation-redis-20260716`, bound only to `127.0.0.1:47379`.
- Existing `127.0.0.1:9022` and existing Redis/PostgreSQL listeners were not used.
- Credentials were local disposable fixture values and are not production secrets.

## First run and defect found

The initial PostgreSQL command ran two tests. Batch/idempotent/`SKIP LOCKED` passed, but persistent job recovery failed while decoding `batch_size`:

```text
mismatched types; Rust type i64 (INT8) is not compatible with SQL type INT4
```

`usage_cleanup_jobs.batch_size`, `max_batches`, `batches`, `redis_max_command_keys` and `redis_scan_passes` are PostgreSQL `INTEGER`. `usage_cleanup_job_from_row` incorrectly used the BIGINT reader for them. The fix reads those columns as `i32`, converts nonnegative values to `usize`, and leaves actual BIGINT columns on the existing `i64` path.

The failed run is not counted as pass evidence.

## PostgreSQL verification

Command shape, executed three separate times after the fix:

```bash
KIRO_RS_TEST_POSTGRES_URL='postgres://<isolated>@127.0.0.1:47432/kiro_rs_validation' \
  cargo test postgres_usage_cleanup -- --nocapture --test-threads=1
```

Result per run: `2 passed, 0 failed`. Aggregate after fix: `6 passed, 0 failed` across three outer runs.

Covered behavior: one active job; claim and unexpired lease exclusion; persistent cancel; requeue checkpoint preservation; pass-limit reset; expired lease takeover; completed final state; bounded soft/hard batches; idempotent terminal zero; and `FOR UPDATE SKIP LOCKED` under a held row lock.

## Redis verification

Command shape, executed three separate times:

```bash
KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:47379' \
  cargo test redis_pattern_delete_is_bounded_and_cancellable -- --nocapture --test-threads=1
```

Result per run: `1 passed, 0 failed`. Each run internally created and removed 321, 338 and 355 matching keys, asserted `max_command_keys <= 64`, required a final empty scan pass and asserted zero residue. Aggregate: nine key-set rounds plus three cancellation/index rounds.

The cancellation branch proved that the snapshot index is removed immediately while the item may remain for resumable cleanup.

## Expanded consistency verification

The following command was executed once at an earlier consistency stage; it is preserved as discovery evidence and superseded by the later 36-test three-outer-run result. Tests containing “for three rounds” perform three internal rounds:

```bash
KIRO_RS_TEST_POSTGRES_URL='postgres://<isolated>@127.0.0.1:47432/kiro_rs_validation' \
KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:47379' \
CC=/usr/bin/cc \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc \
cargo test cleanup -- --nocapture --test-threads=1
```

Result: `35 passed, 0 failed`. This group covered persistent job lease/cancel/resume/reclaim, bounded batches and lock contention, monotonic PostgreSQL/Redis/process watermarks, concurrent old writes, soft/hard rollup subtraction, legacy cost compatibility, global duration-max recomputation, high-cardinality zero-row pruning, Redis guarded final commit, restart fallback and scheduler cleanup helpers.

Two new PostgreSQL cases add the following exact coverage:

- `postgres_cleanup_rollup_update_subtracts_legacy_cost_for_three_rounds`: historical JSON with `originalCostUsd=0`, both estimated-cost fallback and external raw-cost fallback, same-ID lower-cost/lower-duration update, and exact remaining global/credential/external/duration values.
- `postgres_cleanup_prunes_high_cardinality_zero_rollups_for_three_rounds`: three rounds of 48 unique old conversation/credential/cache/duration records plus one cutoff-newer record, batch size 7, no `requests <= 0` residue in six rollup/histogram tables, and the newer detail/rollups exactly once.

PostgreSQL-authoritative fallback latency during the same suite was:

| Round | Summary p95 | Dashboard p95 |
| ---: | ---: | ---: |
| 1 | 8.596 ms | 18.071 ms |
| 2 | 5.746 ms | 21.703 ms |
| 3 | 5.762 ms | 18.528 ms |

The isolated Redis usage-writer/scheduler burst case also passed three internal rounds. Loaded scheduler p95 was `3.027-4.406 ms`; loaded p99 was `4.319-15.936 ms`, below the current 75 ms hot-path budget. These local values prove no obvious regression in this fixture only; they do not prove production extreme-cardinality contention is solved.

Static gates after these patches passed: `cargo check`, `cargo check --tests`, `cargo fmt --all -- --check` and targeted `git diff --check`.

## Same-ID contract drift and current resolution

The cleanup filter is not a full PostgreSQL regression gate. Running the pre-existing round-trip case explicitly against the same isolated database failed:

```bash
KIRO_RS_TEST_POSTGRES_URL='postgres://<isolated>@127.0.0.1:47432/kiro_rs_validation' \
cargo test postgres_persists_runtime_config_credentials_stats_usage_and_pricing \
  -- --nocapture --test-threads=1
```

That run failed at the then-current `src/storage/postgres.rs:10691`: after soft clear and a same-ID write, the test expected one active record but observed zero. The result exposed a test/product-contract drift and was not selected by `cargo test cleanup`, so the earlier `35/35` result is not a full regression pass.

The product decision is now explicit: while the soft tombstone exists, the same request ID must not be revived even when a later write carries `created_at > cutoff`; a different ID with `created_at > cutoff` is accepted and contributes exactly once. After hard cleanup physically removes the tombstone, the watermark still rejects a normal replay carrying its original old `created_at`, but cannot identify the same ID if a caller forges a newer timestamp. Both UI sources describe the rollup-deletion contract. After compilation recovered, `postgres_persists_runtime_config_credentials_stats_usage_and_pricing` passed 1/1 with the updated tombstone/new-ID assertions. The historical failure remains discovery evidence, not a current-code failure.

## In-flight commit race guard

Static review found another race not covered by the earlier concurrent-writer test: a usage transaction could read the old watermark and stage an old detail/rollup, cleanup could advance its watermark while that row remained uncommitted and invisible, then the writer could commit after cleanup. The current source adds a transaction-scoped advisory lock on `USAGE_CLEANUP_COMMIT_LOCK_ID`:

- every PostgreSQL usage `record_batch` holds `pg_advisory_xact_lock_shared` from before watermark/upsert work through commit;
- watermark advance holds `pg_advisory_xact_lock` exclusively through its commit;
- cleanup therefore waits for already-started writers; writers that start later wait for cleanup and observe the advanced watermark.

`postgres_cleanup_watermark_waits_for_inflight_usage_commit_for_three_rounds` opens a writer transaction, holds the shared guard, stages old detail/rollup, proves watermark advance remains blocked for 50 ms, commits the writer, then requires watermark advance and cleanup to remove the detail and all authorities. It passed all three internal rounds after compilation recovered.

This guard adds one PostgreSQL advisory-lock statement to every persisted usage batch and intentionally queues writer commits while cleanup holds the exclusive lock. The three-round correctness test cannot establish acceptable production latency or starvation behavior; writer baseline/loaded p50/p95/p99, throughput and recovery remain required.

## Cross-instance writer deadlock and stable lock order

The isolated two-service request-admission runs exposed PostgreSQL `40P01 deadlock detected` retries on every outer round. The three-round report contained one to three retries per round; the later same-process plateau report observed two to four. Several failures had `record_count=1`, and slow usage writes reached about one second while PostgreSQL's deadlock detector selected a victim. Retrying preserved the sampled requests, but a recovered retry is not a performance or correctness pass for the release gate.

Static review found that one usage transaction updates multiple shared rollup rows while iterating process-random `HashMap` order. Separate service processes therefore lock the same global/status/model/endpoint rows in different orders. The shared cleanup advisory lock permits concurrent writers by design, so it does not serialize away this writer/writer inversion.

The current source now:

- sorts deduplicated usage records by request ID and locks existing detail rows with `ORDER BY id FOR UPDATE`;
- sorts total and time-bucket rollups by their complete composite keys;
- sorts cache-read, cache-time, duration-time and credential-summary entries by complete key;
- keeps the table update sequence unchanged and deterministic across every writer.

Focused command shape, executed three separate outer times against `127.0.0.1:47432`:

```bash
KIRO_RS_TEST_POSTGRES_URL='postgres://<isolated>@127.0.0.1:47432/kiro_rs_validation' \
  cargo +1.92.0 test postgres_usage_concurrent_writers_keep_rollup_lock_order_stable \
  -- --nocapture --test-threads=1
```

Each test performs 64 synchronized pairs of concurrent transactions and verifies the global rollup reaches exactly 128. All three outer runs passed, for 192 transaction pairs / 384 records and zero observed deadlocks. Test execution times were 5.21 s, 2.53 s and 3.85 s; compilation/file-lock wait is excluded. This proves the isolated PostgreSQL lock-order contract but not the rebuilt service path. Final dynamic acceptance requires a new fixed binary and at least five two-instance admission rounds with `usageDeadlockRetries=0`, bounded slow-write counts and unchanged usage totals.

## Post-contract focused rerun

After the intermediary compile failure was repaired, the focused rerun covered four tests:

| Test | Result | Scope |
| --- | --- | --- |
| `postgres_persists_runtime_config_credentials_stats_usage_and_pricing` | 1/1 pass | updated soft-tombstone/new-ID round-trip plus its broader storage fixture |
| `postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup` | 1/1 pass | 1000 external billing records removed from summary/Dashboard rollup by soft cleanup |
| `postgres_cleanup_rejects_late_replay_but_accepts_newer_records_for_three_rounds` | internal 3/3 pass | old replay rejected; cutoff-newer new ID accepted |
| `postgres_cleanup_watermark_waits_for_inflight_usage_commit_for_three_rounds` | internal 3/3 pass | exclusive watermark advance waits for shared-guard old writer, then cleanup removes all authorities |

The aggregate shell wall time was 77.3 seconds and included compilation/build-directory lock wait. It is recorded only as command context and is not a cleanup, query or writer latency measurement. Each case had one outer focused execution; the two `for_three_rounds` cases looped three times internally. This was not three outer repetitions and not the full PostgreSQL/all-target suite.

## Three outer cleanup-suite runs

After the focused contract rerun, the complete cleanup filter was executed three separate outer times against the isolated PostgreSQL and Redis services:

```bash
KIRO_RS_TEST_POSTGRES_URL='postgres://<isolated>@127.0.0.1:47432/kiro_rs_validation' \
KIRO_RS_TEST_REDIS_URL='redis://127.0.0.1:47379' \
cargo test cleanup -- --nocapture --test-threads=1
```

Each outer run passed `36/36` with `0 ignored`; aggregate result `108/108`. The newly added in-flight commit guard and all other `cleanup`-named cases were therefore included in every outer run. Cases with `for_three_rounds` also loop internally, but the aggregate `108` counts Rust test cases rather than their internal scenario iterations.

Across the internal fallback measurements printed by those runs, PostgreSQL summary p95 ranged from `4.952 ms` to `9.945 ms`; Dashboard p95 ranged from `16.645 ms` to `49.070 ms`. These are isolated read-fallback fixture results. They do not measure PostgreSQL usage-writer latency while the exclusive cleanup advisory lock is held and do not substitute for Redis chaos or production-scale load.

## Remaining gates

- Run the complete PostgreSQL and all-target suites; the cleanup filter's three outer passes do not select every storage regression (including the broader round-trip by name).
- Repeat the in-flight commit guard under PostgreSQL writer and cleanup/scheduler concurrent load, capturing writer wait/latency/throughput and recovery.
- Measure PostgreSQL usage-writer latency/throughput before, during and after cleanup advisory-lock contention; require bounded waiting, no starvation/deadlock and full recovery.
- Rebuild the service and rerun the two-instance admission matrix for at least five rounds; require `usageDeadlockRetries=0` instead of accepting retry recovery.
- Run both maintained UIs' browser save/refresh and confirmation-text gates against the updated source wording.
- Redis 50/74/75/90/150/500 ms, disconnect, restart and recovery with live scheduler traffic.
- Worker process termination/restart and heartbeat lease takeover.
- Deliberate Redis pass-limit failure followed by resume.
- Two-instance cleanup ownership under injected heartbeat failure.
- Production-scale scheduler degraded/429 counters during cleanup and a bounded strategy for the persistent Redis derived-cache invalidation state.
- Container/port cleanup is recorded only after all dependent chaos tests finish.
