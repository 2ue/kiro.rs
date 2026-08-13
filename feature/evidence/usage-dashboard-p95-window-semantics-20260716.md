# Usage Dashboard P95 And Window Semantics Evidence

Role: Duration percentile implementation, dashboard consumer, time-window, storage-authority and
query-plan evidence

Status: `historical-red-evidence / current-source-recheck-added / runtime-reverification-pending`

Date: 2026-07-16

Issue: [Usage Dashboard P95 And Window Semantics](../issues/usage-dashboard-p95-and-window-semantics.md)

## Conclusion

Historical red evidence: `p95DurationMs` was not a percentile in the pre-fix implementation.
PostgreSQL assigned the maximum hourly duration to `p95_duration_ms`; Redis also stored and merged
only `duration_ms_max`; both UIs then displayed that value as P95 and used it for the 60-second
warning threshold.

The bug is silent. It emits no error, request ID, log fingerprint or invalid payload. It is hidden
by the current three-record Redis test because nearest-rank P95 for three samples is the maximum.
For durations 1 through 100, the expected discrete weighted P95 is 95 while the current result is
100. For 95 requests at 10 ms and five requests at 1000 ms, expected P95 is 10 ms while the current
result is 1000 ms.

## 2026-07-21 Current Source Recheck

The capture above remains valid as historical pre-fix evidence. Current source recheck shows:

- `UsageRecorder::dashboard()` and `dashboard_windows()` now require `postgres_store` and bail
  rather than serving a Redis-backed dashboard.
- `PostgresUsageStore::dashboard_windows()` computes `p95_duration_ms` via weighted nearest-rank
  P95 over duration histogram + boundary detail, not by taking a maximum.
- The Redis `duration_ms_max -> p95_duration_ms` conversion still exists in helper / series / test
  paths, but it is no longer the user-visible dashboard authority.
- As a result, the original MAX-as-P95 issue is historical for the current `/api/admin/usage-dashboard`
  and `/api/admin/usage-dashboard/windows` code path; runtime re-verification is still pending in
  this environment because the isolated PostgreSQL test URL is unavailable here.

A second correctness defect affects the sample population. PostgreSQL includes an hourly bucket
only when its `bucket_start` is inside `[from, to)`. Redis floors `from` to the start of its hour and
enumerates every touched hour. For rolling windows whose lower bound is not hour-aligned,
PostgreSQL excludes the complete first partial hour while Redis includes it. The same API can
therefore change totals and the alleged P95 when Redis is invalidated, unavailable, or slow and the
request falls back to PostgreSQL.

## Complete Consumer Map

| Layer | Path | Current behavior |
| --- | --- | --- |
| API DTO | `src/anthropic/usage.rs` `UsageDashboardSummary` | serializes numeric `p95DurationMs` |
| PostgreSQL full dashboard | `PostgresUsageStore::dashboard` -> `dashboard_windows` | `MAX(b.duration_ms_max) AS p95_duration_ms` |
| PostgreSQL windows endpoint | `dashboard_windows_only` -> `dashboard_windows` | same maximum |
| PostgreSQL breakdown helper | `dashboard_breakdown_only` -> `dashboard_windows` | computes the same field even though breakdown does not consume it |
| Redis write | `append_usage_dashboard_bucket_aggregate` | custom command stores only `duration_ms_max` per hour |
| Redis window merge | `sum_usage_hash_refs` | takes maximum `duration_ms_max` across hours |
| Redis response | `dashboard_summary_from_values` | maps `duration_ms_max` directly to `p95_duration_ms` |
| Redis-first full route | `UsageRecorder::dashboard` | returns Redis value; falls back to PostgreSQL |
| Redis-first split route | `UsageRecorder::dashboard_windows` | returns Redis value; falls back to PostgreSQL |
| Admin API | `/api/admin/usage-dashboard` | consumed by legacy Admin UI; full result has a two-second Admin cache |
| Admin API | `/api/admin/usage-dashboard/windows` | consumed by current UI; no equivalent Admin response cache or explicit dashboard timeout |
| Current UI | `ui/src/features/overview/overview-page.tsx` | card title `P95 耗时`; warning at 60,000 ms |
| Legacy Admin UI | `admin-ui/src/components/usage-dashboard-panel.tsx` | descriptor `P95 ...ms`; warning at 60,000 ms |
| TypeScript contracts | both `src/types/api.ts` files | require numeric `p95DurationMs` |
| Redis regression test | `redis_usage_summary_dashboard_and_record_query_work` | three samples; expected maximum happens to equal nearest-rank P95 |

The older `/usage-summary` response does not contain a P95 field and is not directly affected.
Hourly/daily chart series do not expose a duration percentile either.

## Source Fingerprints

PostgreSQL:

```sql
COALESCE(MAX(b.duration_ms_max), 0)::bigint AS p95_duration_ms
```

Redis response conversion:

```text
p95_duration_ms = duration_ms_max
```

Redis multi-hour merge:

```text
duration_ms_max = max(each hourly duration_ms_max)
```

UI fingerprints:

```text
P95 耗时
P95 <value>ms
p95DurationMs >= 60_000
```

No-fingerprint variants are more important operationally: normal HTTP 200, plausible numeric
value, no error log, and no request-level evidence that the number is a maximum. The value can
coincidentally be correct when the 95th-percentile sample equals the maximum, so spot checks with
small samples do not disprove the bug.

## Current Time-Window Semantics

`usage_dashboard_windows` defines six half-open metadata windows:

| Key | Metadata interval |
| --- | --- |
| `today` | local midnight to `now` |
| `last24h` | `now - 24h` to `now` |
| `yesterday` | prior local midnight to current local midnight |
| `last7d` | `now - 7d` to `now` |
| `last30d` | `now - 30d` to `now` |
| `thisMonth` | local month start to `now` |

PostgreSQL joins hourly rollups with:

```sql
b.bucket_start >= w.from_at AND b.bucket_start < w.to_at
```

Redis enumerates from `date_trunc('hour', from)` through
`date_trunc('hour', to - 1 second)`. At `12:30`, a `last24h` record from the lower-bound hour at
`12:45` belongs to the advertised interval. PostgreSQL excludes its `12:00` bucket; Redis includes
the whole `12:00` bucket, including records from `12:00-12:29` that are outside the advertised
interval.

For the primary UI timezones (`Asia/Shanghai` and UTC), `today`, `yesterday`, and `thisMonth` have
hour-aligned lower bounds. The API also accepts fixed offsets such as `UTC+05:30`; local midnight
then falls at a UTC half-hour and the same mismatch affects calendar windows too.

The hourly and daily chart specs are aligned at their starts, so their bucket population does not
have this particular lower-bound discrepancy. The rolling summary windows do.

## Weighted Percentile Definition

The selected definition is the discrete nearest-rank weighted percentile over successful and
failed usage records alike, because the existing duration aggregate includes every persisted
request:

```text
N = sum(positive request weights)
rank = ceil(0.95 * N)
P95 = smallest duration whose cumulative ascending weight is >= rank
empty population = 0
```

Do not expand counts with `generate_series(1, requests)` and do not feed only distinct duration
values to `percentile_disc`; both produce a wrong or unbounded plan. Use cumulative weights.

Core SQL shape for already selected histogram rows:

```sql
WITH histogram AS (
    SELECT window_key, duration_ms, SUM(requests)::bigint AS weight
    FROM selected_duration_rows
    WHERE requests > 0
    GROUP BY window_key, duration_ms
), ranked AS (
    SELECT
        window_key,
        duration_ms,
        SUM(weight) OVER (
            PARTITION BY window_key
            ORDER BY duration_ms
            ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
        ) AS cumulative_weight,
        SUM(weight) OVER (PARTITION BY window_key) AS total_weight
    FROM histogram
)
SELECT
    window_key,
    COALESCE(
        MIN(duration_ms) FILTER (
            WHERE cumulative_weight::numeric * 100
               >= total_weight::numeric * 95
        ),
        0
    ) AS p95_duration_ms
FROM ranked
GROUP BY window_key;
```

Casting before multiplication avoids a theoretical `BIGINT` overflow in long-lived aggregates.

## Exact Boundary Algorithm

Hourly histograms cannot identify which samples belong to a partial boundary hour. For an exact
half-open `[from, to)` percentile without scanning the full detail window:

1. Compute `full_start = ceil_to_hour(from)` and `full_end = floor_to_hour(to)`.
2. Read positive histogram weights for complete buckets in `[full_start, full_end)`.
3. Read detail durations for the leading range `[from, min(to, full_start))` when `from` is not
   aligned.
4. Read detail durations for the trailing range `[max(from, full_end), to)` when `to` is not
   aligned and it does not overlap the leading range.
5. Clamp detail duration to the histogram's `i32::MAX` representation, union the weights, aggregate
   by duration and apply the cumulative weighted rank once.

Generate at most two boundary ranges per dashboard window, then range-join those ranges to active
`usage_records`. Do not use a broad 30-day detail scan with an `OR` boundary predicate.

The current total/average Redis and PostgreSQL windows use different boundary populations. A true
P95 must not be merged into a response whose other latency metrics describe a different sample
set without an explicit decision. The safe target is PostgreSQL-authoritative dashboard windows
with one documented population. If exact hybrid boundary aggregation for every metric is deferred,
the response and UI must explicitly use an hourly-bucket window contract instead of claiming an
exact rolling interval.

## Storage Authority Decision

PostgreSQL is mandatory in the service startup path and already has the exact subtractable
duration histogram. It is the selected P95 authority.

Do not add an exact duration histogram to the shared Redis instance. A per-hour Redis duration
histogram would add high-cardinality fields and a write on every usage record, directly opposing
the scheduler/usage Redis isolation work. An approximate Redis sketch is also a poor fit for exact
negative deltas after same-ID replacement and soft cleanup.

For Redis-first dashboard data, either:

- make PostgreSQL authoritative for the complete window response; or
- overlay a PostgreSQL percentile only when the base response is guaranteed to use the identical
  population and generation.

The first option is preferred. Separate Redis and PostgreSQL writer queues converge eventually;
combining a fresh Redis request count with a lagging PostgreSQL percentile can otherwise produce a
mixed-generation response even when their boundary rules match.

If the PostgreSQL percentile query fails or exceeds its bound, the API must return an unavailable
metric or a clear bounded error. It must not silently relabel the Redis maximum as P95.

## Index And Query-Plan Review

Relevant indexes and keys:

- `usage_duration_rollup_time_buckets` primary key: `(bucket_start, duration_ms)`.
- `idx_usage_duration_rollup_time_bucket`: the same key order as the primary key and therefore
  structurally redundant for this query.
- `idx_usage_duration_rollup_positive_max`: `(duration_ms DESC) WHERE requests > 0`; useful for
  the global maximum repair after negative deltas, not for a time-range weighted percentile.
- `idx_usage_records_created_at`: `created_at DESC WHERE deleted_at IS NULL`; appropriate for the
  one- or two-hour active-detail boundary ranges.

Start with the primary-key range scan. Do not add a covering
`(bucket_start, duration_ms) INCLUDE (requests) WHERE requests > 0` index without production-shaped
`EXPLAIN (ANALYZE, BUFFERS)` evidence. It could enable index-only reads, but `requests` changes on
every usage write, so including it increases index write amplification and prevents HOT updates.

Required plan fixtures:

| Dataset | Histogram shape | Purpose |
| --- | --- | --- |
| 30 days x 100 durations/hour | 72,000 rows | normal cardinality |
| 30 days x 1,000 durations/hour | 720,000 rows | high-cardinality bound |
| two boundary hours with production-shaped detail | range-index check | exact lower/upper fragments |
| six overlapping dashboard windows | one query | avoid six independent full scans |

Reject plans that expand weights into rows, sequentially query each window, scan all 30-day detail,
or add Redis commands proportional to distinct durations. On the frozen release candidate, target
the storage contract budget: percentile query p95 no more than 50 ms and p99 no more than 150 ms,
with no regression beyond 10%/15% versus the same data's dashboard baseline. Record heap/index
blocks, rows scanned, temp spill, query memory and writer throughput.

The split `/usage-dashboard/windows` path currently lacks the full endpoint's explicit two-second
Redis and five-second PostgreSQL wrapper timeouts. A heavier percentile query must not be added
without making both route variants share a bounded query path.

## Required Correctness Matrix

Each row runs at least three outer rounds against isolated PostgreSQL. Redis-specific red cases run
against isolated Redis and are not retained as the target percentile authority.

| Case | Fixture | Expected nearest-rank P95 | Additional assertion |
| --- | --- | ---: | --- |
| Ordered 1..100 | one hour, weight 1 each | 95 | current maximum 100 must fail red test |
| Repeated lower weight | 10 ms x95, 1000 ms x5 | 10 | maximum 1000 is not accepted |
| Repeated upper threshold | 10 ms x94, 1000 ms x6 | 1000 | verifies exact rank boundary |
| Cross-hour weighted | H1 10 ms x95, H2 1000 ms x5 | 10 | do not percentile hourly maxima |
| Cross-hour reverse weight | H1 10 ms x5, H2 1000 ms x95 | 1000 | weight, not bucket count |
| Empty | no positive rows | 0 | no null/negative cast |
| Same-ID replacement | 1000 ms replaced by 10 ms | 10 | old histogram contribution removed |
| Cleanup negative delta | durations 1..100, remove 96..100 | 91 | active detail and histogram agree |
| Hard cleanup after soft | same fixture | 91 | no second subtraction |
| Rolling lower boundary | fixed `now=:30`, record before/after lower bound | exact in-range population | Redis/PG cannot differ |
| Half-hour timezone | `UTC+05:30`, local midnight boundary | exact in-range population | calendar window correct |

For the cleanup row, 95 remaining samples give nearest rank `ceil(90.25)=91`.

## Multi-Round And Failure Matrix

- Run `today`, `last24h`, `yesterday`, `last7d`, `last30d`, and `thisMonth` in UTC and
  Asia/Shanghai for five fixed-clock rounds.
- Run fixed offsets `UTC+05:30` and `UTC-03:30` for boundary coverage.
- Force Redis hit, Redis empty, Redis timeout, Redis disconnect, cleanup invalidation and
  PostgreSQL-only paths; the P95 value and population must not change.
- Run concurrent writer plus dashboard reads, then drain writers and require convergence to the
  exact persisted value. Do not claim a cross-store atomic snapshot before drain.
- Run same-ID negative replacement and soft/hard cleanup while repeatedly reading the dashboard;
  no zero/negative histogram rows and no stale maximum may survive the committed operation.
- Run the 30-day high-cardinality query-plan fixtures for at least 100 reads per round and record
  p50/p95/p99, buffers, temp bytes, CPU, PostgreSQL connections, RSS and writer throughput.

## Current Validation Status

Static source tracing plus current-source recheck now distinguish the historical defect from the
current code path. The old red capture remains authoritative for the pre-fix build, but the current
source shows PgSQL-only dashboard authorities and weighted nearest-rank P95.

Runtime re-execution of the 1..100, weighted, cross-hour, cleanup and plan-shape matrices against
the corrected build is still pending in this environment because `KIRO_RS_TEST_POSTGRES_URL` is not
available here.

## Residual Boundaries

- Exact historical percentiles are available only while the duration histogram/detail authority
  remains consistent; this issue does not invent percentiles for installations that never stored
  those rows.
- A discrete nearest-rank P95 is intentionally not a linearly interpolated percentile. The API and
  tests must keep one definition.
- Millisecond histograms clamp durations above `i32::MAX`; boundary detail must apply the same
  clamp or the two sources diverge at that extreme.
- PostgreSQL authority removes shared-Redis cardinality risk but increases dashboard read load;
  release evidence, bounded timeouts and low-cardinality response caching are required.
- Renaming the metric to maximum is a safe rollback/minimum fix, but it is not evidence that a true
  P95 was implemented.
