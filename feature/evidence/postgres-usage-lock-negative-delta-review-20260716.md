# PostgreSQL Usage Lock And Negative-Delta Review

Status: `static-review-complete / focused-runtime-partial-pass / six-authority-runtime-pending`

Date: 2026-07-16

Reviewed source SHA-256:

```text
src/storage/postgres.rs
4ffb9017c538a359ab62a415f96b3eca6486371a7226be97c1f0148951e3bd0f
```

Related issue: [Usage cleanup safety and Redis isolation](../issues/usage-cleanup-safety-and-redis-isolation.md)

## Scope

This review covers the PostgreSQL usage-detail writer and its six derived authorities:

1. `usage_rollup_totals`
2. `usage_rollup_time_buckets`
3. `usage_cache_read_totals`
4. `usage_cache_read_rollup_time_buckets`
5. `usage_duration_rollup_time_buckets`
6. `usage_credential_cost_summary`

It focuses on concurrent first insert, same-existing-ID replacement and negative deltas, cleanup
coordination, explicit compression/backfill maintenance, deadlock freedom, starvation, and online
data-loss boundaries. It does not treat a retry after PostgreSQL `40P01` as success.

## Current Conclusion

The current lock order is coherent when every writer runs this version:

```text
writer
  shared usage commit guard
  -> sorted per-request-ID advisory locks
  -> usage detail rows ordered by ID FOR UPDATE
  -> six rollup tables in a fixed order
  -> complete primary-key order within each table
  -> negative-delta reconciliation
  -> commit

cleanup or usage maintenance
  exclusive usage commit guard
  -> detail/rollup work
  -> commit
```

The per-ID lock closes the concurrent first-insert hole for new-to-new traffic. The fixed table and
key order closes the cross-process `HashMap` row-lock inversion. The exclusive guard prevents a
current-version cleanup or maintenance transaction from snapshotting rollups while a
current-version writer can commit.

This is not yet a complete release proof. The existing concurrent-writer test checks the global
request count, but it does not independently reconcile every metric and key in all six derived
authorities to the final active detail rows. Explicit compression and legacy-cost backfill are
consistent only by blocking all usage writers; a long online maintenance run can make the usage
writer exhaust its three five-second attempts and drop whole batches. Mixed old/new binaries also
do not share the new per-ID/exclusive-lock protocol.

## Static Findings

### S1: Same-ID serialization is correct for a homogeneous new-version cluster

`record_batch` deduplicates and sorts request IDs, obtains the shared cleanup/maintenance guard,
then obtains deterministic per-ID advisory locks before reading the old active rows. Two current
writers for the same missing or existing ID therefore cannot both read an absent/stale old value.

The following cases remain correctly no-op:

- A record older than the soft-delete watermark does not reach `ON CONFLICT` and adds no rollup.
- A soft-deleted/tombstoned ID cannot be updated because the conflict `WHERE` requires an active,
  rollup-active row.
- An input batch containing duplicate IDs persists only the last in-memory value for that ID.

Hash collisions between unrelated request IDs can serialize unrelated writes but do not corrupt
data. The lock is a 64-bit SHA-256 prefix in a domain; collision probability is negligible but not
mathematically impossible.

### S2: Rollup deadlock order is deterministic

The writer uses this table order:

```text
usage_rollup_totals
usage_rollup_time_buckets
usage_cache_read_totals
usage_cache_read_rollup_time_buckets
usage_duration_rollup_time_buckets
usage_credential_cost_summary
negative-delta reconciliation in the same authority order
```

Every `HashMap` is converted to a vector and sorted by the complete database key before SQL is
issued. Request-ID advisory locks are also sorted independently of incoming batch order. This
removes the known writer/writer lock inversion for homogeneous current-version instances.

### S3: Negative deltas need a six-authority oracle

For an existing ID, the transaction subtracts the previously locked active record and adds the new
record. Rows whose request count reaches zero are deleted only for keys affected by that batch.
Global duration maxima are recomputed from the exact positive duration histogram after a negative
delta.

The implementation deliberately does not recompute `duration_ms_max` for non-global dimensions.
The current Dashboard reads the global time-bucket rows, so those non-global maxima are not a
current external authority. A whole-row equality test must either exclude that field for
non-global dimensions or first change the product contract and implementation.

Floating-point cost columns are additive. Test equality must use an absolute tolerance of
`1e-12`; integer counts and tokens remain exact.

### S4: Maintenance is consistent but not online-lossless

Compression and legacy-cost backfill now take the exclusive usage guard in their transaction.
This prevents a current writer from committing between compression's snapshot and `TRUNCATE`, and
prevents backfill from racing current rollup updates.

The lock has no acquisition timeout, and the maintenance transaction can hold it for the full
historical scan/update. Online usage persistence has a five-second timeout and three attempts.
Consequently, maintenance lasting long enough can cause blocked batches to be retried and then
dropped. The exclusive lock proves consistency of what the maintenance transaction sees; it does
not prove zero usage loss while the service remains writable.

The release-safe contract is therefore one of:

- stop/drain every usage writer in the cluster before maintenance, then run maintenance; or
- implement a cluster-wide maintenance fence, reject/drain new usage work, prove queues empty,
  perform maintenance, then reopen admission.

"Run at low traffic" is not a sufficient correctness contract.

### S5: Soft-cleanup watermark acquisition is not covered by the 250 ms lock timeout

Soft cleanup first calls `advance_soft_delete_cleanup_watermark` in a separate transaction. That
transaction obtains the exclusive usage guard before `configure_usage_cleanup_transaction` is
used. The documented 250 ms lock timeout and two-second statement timeout therefore apply to the
later batch transaction, not to watermark advancement. A long or stuck writer can leave soft
cleanup waiting without that bound. Hard cleanup configures the timeout before acquiring its
exclusive guard and does not have this specific ordering gap.

### S6: Mixed-version rolling deployment is unsafe for the new lock protocol

An old writer does not acquire the per-ID advisory lock. During a rolling deployment, an old and a
new instance can both pass the missing-row read for the same ID; the database conflict serializes
the detail row, but the old writer can still calculate its rollup delta from an empty old snapshot.
Likewise, an old writer does not honor the exclusive maintenance guard and can write between a
compression snapshot and `TRUNCATE`.

Until the database operation itself returns an atomic old/new delta or a version fence exists, the
deployment contract must stop/drain all old instances before enabling new writers or running
maintenance. A mixed old/new rolling update is not covered by the current proof.

## Runtime Evidence Already Collected

The shared test binary used for this focused evidence had SHA-256:

```text
cbe135ab377fb92a036265f5354fd51c724192c1ee3840d94d72c54ad00d442f
```

Against an isolated real PostgreSQL 16 instance, the enhanced concurrent-writer test passed three
separate outer runs. Each run contained:

- three missing-ID advisory-lock blocking checks;
- 64 barrier-synchronized pairs writing distinct IDs;
- 32 barrier-synchronized pairs updating the same two IDs in reverse input order;
- a final global request count of 130.

Observed outer-run durations were 21.75 s, 15.00 s, and 9.58 s. All passed with zero observed
`40P01`. The following focused cleanup coordination tests also passed once against the same
isolated PostgreSQL:

```text
postgres_cleanup_is_consistent_with_concurrent_old_writes_for_three_rounds
postgres_cleanup_watermark_waits_for_inflight_usage_commit_for_three_rounds
postgres_usage_cleanup_locked_rows_remain_visible_until_release_for_three_rounds
postgres_usage_cleanup_batches_are_bounded_idempotent_and_skip_locked
```

These runs prove the focused blocking and lock-order paths. They do not provide the independent
six-authority oracle below, maintenance starvation evidence, final service-binary evidence, or
release-mode latency.

## Required Six-Authority Oracle

Use two records with the same ID but intentionally different values for every routing and rollup
axis. Example fixture differences:

| Field | Record A | Record B |
| --- | --- | --- |
| `created_at` | hour H1 | hour H2 |
| endpoint/model/status/source | A keys | B keys |
| conversation | `conversation-a` | `conversation-b` |
| credential | 7 | 8 |
| cache read | 111 | 222 |
| duration | 31 | 62 |
| stream/pricing/tokens/cost | A metrics | B metrics |

After concurrent writers join, read the active `usage_records.data` row to identify the actual
winner. Do not assume task scheduling order. The independent assertions are:

### A1: Detail authority

```sql
SELECT id, created_at, deleted_at, rollup_active, data
FROM usage_records
WHERE id = $1;
```

Expected: exactly one active, rollup-active detail row, and its JSON is exactly A or B.

### A2: Totals

```sql
SELECT dimension, dimension_key, dimension_label,
       requests, success_requests, error_requests,
       stream_requests, non_stream_requests,
       priced_requests, unpriced_requests,
       total_input_tokens, billable_input_tokens, total_output_tokens,
       total_cache_read_input_tokens, total_cache_creation_input_tokens,
       total_estimated_cost_usd, total_original_cost_usd,
       duration_ms_sum, duration_ms_count, duration_ms_max
FROM usage_rollup_totals
ORDER BY dimension, dimension_key;
```

Expected: `global/all` has one request and winner metrics. Winner status/source/model/endpoint/
credential/conversation keys have one request. Losing keys are absent. No row has
`requests <= 0`. Non-global `duration_ms_max` is excluded unless its authority contract is changed.

### A3: Time buckets

```sql
SELECT bucket_start, dimension, dimension_key, dimension_label,
       requests, success_requests, error_requests,
       stream_requests, non_stream_requests,
       priced_requests, unpriced_requests,
       total_input_tokens, billable_input_tokens, total_output_tokens,
       total_cache_read_input_tokens, total_cache_creation_input_tokens,
       total_estimated_cost_usd, total_original_cost_usd,
       duration_ms_sum, duration_ms_count, duration_ms_max
FROM usage_rollup_time_buckets
ORDER BY bucket_start, dimension, dimension_key;
```

Expected: only the winner hour and winner time-enabled dimension keys remain. Global metrics equal
the winner. Losing hour/key rows are absent and no row has `requests <= 0`.

### A4: Cache-read total histogram

```sql
SELECT cache_read_input_tokens, requests
FROM usage_cache_read_totals
ORDER BY cache_read_input_tokens;
```

Expected: exactly `(winner.cache_read_input_tokens, 1)`.

### A5: Cache-read time histogram

```sql
SELECT bucket_start, cache_read_input_tokens, requests
FROM usage_cache_read_rollup_time_buckets
ORDER BY bucket_start, cache_read_input_tokens;
```

Expected: exactly `(winner.hour, winner.cache_read_input_tokens, 1)`.

### A6: Duration time histogram

```sql
SELECT bucket_start, duration_ms, requests
FROM usage_duration_rollup_time_buckets
ORDER BY bucket_start, duration_ms;
```

Expected: exactly `(winner.hour, winner.duration_ms, 1)`. The global total and global winner-hour
`duration_ms_max` must equal the maximum positive histogram duration.

### A7: Credential cost summary

```sql
SELECT credential_id, requests, estimated_cost_usd, original_cost_usd,
       kiro_metering_usage, priced_requests, unpriced_requests
FROM usage_credential_cost_summary
ORDER BY credential_id;
```

Expected: only the winner credential remains, with one request and exact winner pricing counts;
cost/metering values match within `1e-12`.

## Required Concurrency Matrix

Run every row at least three outer times against real isolated PostgreSQL. Count `40P01` directly;
do not infer it from eventual success.

| Case | Writers | Batch | Rounds | Required result |
| --- | ---: | ---: | ---: | --- |
| Missing same ID, A versus B | 2 | 1 | 64 | one detail winner; A1-A7 exact |
| Existing same ID, base then A/B | 2 | 1 | 64 | two negative-delta transitions; A1-A7 exact |
| Two IDs in reverse input order | 2 | 2 | 64 | no deadlock; detail-derived aggregate exact |
| High contention same 8 IDs | 4 | 23 | 32 | no deadlock; no zero/negative rows |
| High contention same 32 IDs | 8 | 32 | 32 | no deadlock; no pool starvation |
| Distinct high-cardinality IDs | 8 | 64 | 32 | p50/p95/p99 and throughput baseline |

For multi-record cases, build the expected rows independently from the final active detail rows,
sort by the complete primary key, and compare each table. Avoid calling
`UsageRollupBatchDelta::add_record` as the sole oracle because it would duplicate implementation
bugs in the expected result.

## Required Starvation Matrix

### Short holder and queued exclusive

1. Hold one writer shared guard for less than 100 ms.
2. Start cleanup or maintenance and observe it waiting for the advisory lock.
3. Start another writer after the exclusive waiter is queued.
4. Release the first writer.

Expected on a small fixture: exclusive operation acquires within 500 ms of release, the late
writer does not bypass it, and all tasks finish within two seconds without `40P01` or lost detail.

### Long holder

- Hard cleanup must return a clear lock-timeout result in the configured 250-500 ms observation
  band and succeed on a retry after release.
- Soft watermark advancement currently has no such bound; a test must demonstrate the current
  behavior before the contract is changed.
- Compression/backfill currently wait. The test must confirm that they do not deadlock, while the
  release gate must still reject online runs that could exceed usage writer retry time.

### Continuous readers

Use 4 and 8 short writer loops for at least 100 acquisitions while an exclusive waiter is queued.
The exclusive waiter must acquire before the writer loops complete; later shared acquisitions must
not starve it. Record acquisition p50/p95/p99, maximum wait, writer failures, and queue depth.

### Maintenance correctness

For compression and legacy-cost backfill, run three rounds with an active writer, a queued
maintenance task, and a later writer. Expected:

- maintenance waits for the active writer;
- the later writer does not commit into the protected snapshot window;
- the schema migration marker appears exactly once;
- detail and all six authorities remain equivalent after both maintenance and the later writer;
- no writer exceeds its persistence timeout in the accepted offline/drained test topology.

## Release Gates

This area is not release-green until all of the following hold on the frozen candidate:

- the six-authority oracle passes the full concurrency matrix for three outer runs;
- dynamic service logs show zero usage deadlock retries across the final two-instance admission
  matrix;
- writer baseline versus guarded writer p50/p95/p99 has an explicit accepted budget;
- cleanup acquisition timeout behavior matches its documented contract;
- maintenance is fenced behind a stop/drain workflow or is proven not to drop online usage;
- deployment instructions prohibit mixed-version writers unless a compatible database-level
  atomic delta/fence is implemented;
- all temporary PostgreSQL containers, schemas, ports, and test processes are removed.

