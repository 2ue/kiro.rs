# Request API Key Admission Multi-Instance Evidence

Status: `provisional-debug-pass / PostgreSQL-lock-order-focused-pass / final-service-rerun-pending`

Date: 2026-07-16

Issue: [Request API Key Admission](../issues/request-api-key-admission.md)

Runner: [`feature/tests/request-api-key-admission-multi-instance.mjs`](../tests/request-api-key-admission-multi-instance.mjs)

## Conclusion

The current implementation is a process-local, per-instance admission controller. It is not a
distributed global quota. With `rpm=4`, one request key received four accepted requests on each of
two service instances, for an aggregate of eight; the fifth request on each instance returned 429
before the fake upstream. This behavior is both the source contract and the observed runtime
behavior.

The request admission hot path remained independent of Redis in the focused matrix. Across the
three-round plateau report, each Redis cell sent 192 admission-rejected requests, and all 768/768
returned 429 with zero inference-upstream hits. Injecting 75 ms, 150 ms, or `reset_peer` did not add
the injected Redis delay to admission latency. A further five same-process waves per round sent
960/960 admission-rejected requests with zero upstream hits, constant real file-descriptor counts,
and no linear RSS growth.

The phase is not a release pass. The service binary was a debug binary with SHA-256
`dd15a7bf79e5017e4218e8fda6e99656fb826180b0327c8beaa249619a07dbc1`, built before the
PostgreSQL usage rollup lock-order fix. Every plateau round reproduced PostgreSQL usage writer
deadlock retries. The corrected source subsequently passed three outer focused PostgreSQL runs,
each containing 64 synchronized writer pairs, with zero deadlocks. Final acceptance still requires
a newly frozen service binary, five rounds, zero dynamic deadlock retries, and a release-mode
absolute-latency run.

## Safety And Isolation

- Two real `kiro-rs` processes were started on temporary loopback ports.
- Both processes shared one isolated PostgreSQL database and one isolated Redis instance through
  Toxiproxy.
- The Kiro endpoint was a local fake Amazon EventStream server; all credentials and request keys
  were generated fixtures.
- No production host, real credential, or real API key was read.
- Early runs snapshotted `127.0.0.1:9022` before and after every run. That probe is now deprecated for release validation; the current runner must not inspect the existing listener and instead reports `protectedPortProbeSkipped:true` while excluding port 9022 by value.
- Runtime secret directories, Docker containers, and temporary listeners were removed after every
  successful and failed run.
- Reports contain only full SHA-256 request-key IDs, not raw fixture keys.

Both passing reports record:

```text
servicePort9022Touched=false
protectedPortProbeSkipped=true
cleanup.containersRemoved=true
cleanup.tempSecretsRemoved=true
cleanup.portsReleased=true
```

## Source Contract

The implementation itself documents the scope:

- `src/anthropic/request_admission.rs:3-5`: the hot path is process-local and the aggregate limit is
  the sum of all instance limits.
- `src/model/config.rs:2928`: `RequestAdmissionConfig` has RPM, concurrency, queue, and timeout
  fields, but no distributed/global scope field.
- `src/anthropic/request_admission.rs:451`: every controller owns its own state and calls local
  `reserve_rpm` and `acquire_concurrency`.
- `src/anthropic/request_admission.rs:477`: state is a local sharded map keyed by the 32-byte digest.
- `src/anthropic/request_admission.rs:642`: RPM uses a local token bucket.
- `src/anthropic/request_admission.rs:676`: active/queued ticket state is local.
- `src/anthropic/request_admission.rs:945`: the permit wraps the downstream response body and is not
  released merely because response headers exist.
- `src/common/auth.rs:18`: authenticated identity stores only a SHA-256 digest.
- `src/common/auth.rs:27`: the stable observable ID is the complete 64-hex SHA-256 digest.
- `src/main.rs:952`: Redis is used for runtime configuration notification and PostgreSQL reload,
  not request admission decisions.
- `src/anthropic/usage.rs:500`: sampled pre-handler rejection records carry the request-key digest,
  reason, stage, status, and observed sampled count.

The earlier issue text that described a 16-hex short ID and said usage attribution did not exist was
stale. Runtime evidence below confirms the current full-digest contract.

## Test Topology

Each outer round created a new PostgreSQL database and restarted both service processes so RPM
token buckets could not leak between rounds. The two instances shared PostgreSQL and Redis, while
each retained a separate in-process `RequestAdmissionController`.

Initial admission config:

```json
{
  "rpm": 4,
  "maxConcurrentRequests": 0,
  "maxQueuedRequests": 0,
  "queueTimeoutMs": 0
}
```

Queue config:

```json
{
  "rpm": 0,
  "maxConcurrentRequests": 1,
  "maxQueuedRequests": 1,
  "queueTimeoutMs": 350
}
```

Chaos config disabled queuing while one long response body per instance held the same request-key
permit. Every chaos probe therefore had to return 429 before scheduler/provider dispatch.

## Passing Reports

### Functional Three-Round Report

Path:

```text
target/request-admission-multi-instance-reports/
request-admission-20260716123444603-9558-a97d6d.json
```

Identity:

```text
binarySha256=dd15a7bf79e5017e4218e8fda6e99656fb826180b0327c8beaa249619a07dbc1
runnerSha256=3a53a9bc611d30b5b266a20aea3c280c9334a36745903f27632c53786b2086e1
rounds=3
probesPerInstancePerChaosCell=32
result=pass
```

This report established the complete functional path and four Redis cells before the repeated
plateau waves were added.

### Plateau Three-Round Report

Path:

```text
target/request-admission-multi-instance-reports/
request-admission-20260716124043314-55530-37e11b.json
```

Identity:

```text
binarySha256=dd15a7bf79e5017e4218e8fda6e99656fb826180b0327c8beaa249619a07dbc1
runnerSha256=04b28e60dbfdc3c5286af380a42d777380d97ab7c9032924c1c8ac844e4570f1
rounds=3
probesPerInstancePerChaosCell=32
stabilityWaves=5
result=pass
```

## Runtime Results

### Per-Instance Scope

Every round produced the same boundary:

| Scenario | Instance result | Two-instance aggregate | Upstream result |
| --- | ---: | ---: | ---: |
| Same key, RPM 4 | 4 accepted then fifth 429 per instance | 8 accepted | 8 hits |
| Key B, RPM 4 | 4 accepted then fifth 429 per instance | 8 accepted | 8 hits |
| Key C, RPM 4 | 4 accepted then fifth 429 per instance | 8 accepted | 8 hits |
| Same key, long-body concurrency 1 | 1 active per instance | 2 active | 2 held streams |

This disproves any interpretation that the current `rpm=4` or concurrency `1` is a global limit.

### Response-Body Permit And Queue

- Long streams emitted an initial downstream chunk while the fake upstream remained open.
- Header plus first-chunk time was 9.44-16.87 ms in the plateau report.
- A different request key entered the same instance while the first key was held, proving per-key
  isolation.
- One queued request per instance remained upstream-free.
- The next request returned queue-full 429 in 3.14-15.89 ms.
- Aborting the waiter released its queue registration; the next waiter could register and reached
  queue timeout normally.
- Queue-timeout responses arrived in 353.48-359.44 ms for a configured 350 ms timeout.
- Raising concurrency from one to two woke waiters on both instances; observed wake completion was
  60.76-203.28 ms.
- Completing both holders allowed one recovery request on each instance.

### Redis Independence

Each row aggregates three rounds and 64 requests per round, split evenly across the two instances.

| Redis cell | Requests | 429 | Upstream hits | p95 range | p99 range |
| --- | ---: | ---: | ---: | ---: | ---: |
| 0 ms | 192 | 192 | 0 | 25.81-53.02 ms | 27.38-54.01 ms |
| 75 ms downstream latency | 192 | 192 | 0 | 26.06-59.75 ms | 27.69-60.35 ms |
| 150 ms downstream latency | 192 | 192 | 0 | 32.35-54.01 ms | 33.78-55.80 ms |
| downstream `reset_peer` | 192 | 192 | 0 | 26.08-41.80 ms | 27.61-48.28 ms |

The 150 ms cell was not 150 ms slower than baseline. This is direct dynamic evidence that the
admission-rejection path did not synchronously call Redis. It does not claim that accepted requests
or the local credential scheduler are Redis-independent.

### Config Propagation

- Normal primary update to secondary convergence: 66.40-72.49 ms for the first queue config.
- While Toxiproxy disabled Redis, primary `PUT /api/admin/config/runtime` returned 200 and updated
  its local controller; secondary remained on the old config after 500 ms in all three rounds.
- After Redis recovery, secondary reload convergence was 275.01-294.41 ms.
- A subsequent normal runtime event converged in 583.14-621.16 ms.

The config contract is therefore eventual across instances, not atomic. During a Redis event
outage, different instances may enforce different limits until reconnect reload or the 60-second
periodic reload.

### Usage Attribution And Sampling

Every round queried shared PostgreSQL by each 64-hex request-key ID:

| Key | Accepted records | Sampled rejected records | Sampled reasons |
| --- | ---: | ---: | --- |
| A | 14 | 32 | RPM, queue full, queue timeout, concurrency full |
| B | 9 | 2 | RPM |
| C | 9 | 2 | RPM |

Every returned record had the queried digest in `requestApiKeyId`. Sampled rejection records had
`errorType=request_rejection`, `errorStatusCode=429`, `errorMetadata.sampled=true`,
`errorMetadata.stage=admission`, and the expected reason. The number 32 is not the total rejected
request count: detail records intentionally keep the first eight observations and powers of two,
subject to a global log budget.

No raw fixture key or local token occurred in the reports or service logs.

### Resource Plateau

After the four Redis cells, each round ran five additional no-toxic rejection waves. Every wave
sent 64 requests and produced 64 local 429 responses with zero upstream hits.

| Metric | Observed across six service processes |
| --- | ---: |
| Real FD count in all five waves | 23 |
| FD spread within a process | 0 |
| Wave 1 to wave 5 RSS change | -19,840 KB to +5,728 KB |
| Idle behavior | RSS fell after the two-second idle sample in every round |
| Plateau-wave p95 | 22.72-193.51 ms |

The runner initially counted every `lsof` output row, including cwd, text images, and mapped
libraries, as an FD. It was corrected to parse `lsof -Ff` and count only numeric descriptors. The
older repository runners that count raw `lsof` rows cannot be used as strict FD evidence without
the same correction.

The 178-193 ms debug spikes occurred without Redis injection and on a shared development machine.
They are retained as evidence, not presented as release latency. Final release-mode testing uses a
separate 100 ms absolute p95 threshold; debug correctness uses the relative Redis-injection bound.

## PostgreSQL Deadlock Release Blocker

The plateau report observed 4, 2, and 4 PostgreSQL usage writer deadlock retries across its three
rounds. Successful retry does not make this green: the test used only two service instances and a
small sampled usage volume, yet reproduced the issue every round.

The minimum source cause is deterministic:

1. `PostgresUsageStore::record_batch` previously consumed `records_by_id.into_values()` without
   sorting request IDs.
2. `UsageRollupBatchDelta::apply` iterated six `HashMap` collections directly.
3. Rust `HashMap` seeds differ across processes, so two writers locked common global/status/model,
   time-bucket, cache, duration, and credential rollup rows in different orders.
4. The usage writer's shared advisory guard coordinates writers with cleanup, but does not
   serialize writer against writer, so it cannot prevent this row-lock inversion.

The current source sorts request records and every rollup collection by its complete database key,
and orders `SELECT ... FOR UPDATE` by request ID. The focused two-store PostgreSQL test runs 64
barrier-synchronized writer rounds and expects 128 final global requests.

The focused test was executed three separate outer times against real isolated PostgreSQL. All
three passed: 192 synchronized transaction pairs, 384 records, exactly 128 global requests per
outer run, and zero observed deadlocks. This proves the source-level lock-order contract. Final
multi-instance admission evidence must still be generated from the rebuilt service binary and must
report zero deadlock retry logs.

## Global-Limit Design Review

### Option A: Synchronous Exact Redis Quota

Not recommended as the default. It would add at least one Redis round trip to every accepted or
rejected request and force a fail-open, fail-closed, or local-fallback decision during Redis
failure. Fail-closed reproduces the production 429 storm pattern; fail-open breaks the promised
hard limit; local fallback silently changes a global limit into a per-instance one during the
worst failure window.

The separate scheduler report
`target/e01-e02-reports/e0102-20260716114923354-45865-40169e.json` is a concrete warning: 75 ms
scheduler operations entered a 2,000 ms degraded backoff while local credentials still existed.
Putting request admission on that same synchronous path would remove the independent protection
demonstrated by this matrix.

### Option B: Asynchronous Quota Leases

Potentially viable as a future dedicated limiter, but not a small extension. Instances would lease
RPM tokens and concurrency permits in batches, with bounded oversell equal to all live unconsumed
leases. Correctness also requires TTL, epoch/fencing, crash recovery, scale-up/down rules, clock
handling, partition semantics, separate long-stream concurrency leases, config-version changes,
and tests across restart and dynamic replica counts.

This avoids a Redis RTT per request but does not provide exact global limits without sacrificing
availability. It should be a separately designed component rather than an implicit mode added to
the current controller.

### Option C: Explicit Per-Instance Scope

Recommended for this release. It matches the implementation, preserves the Redis-independent hot
path, and makes the actual capacity contract visible:

```text
theoretical aggregate RPM/concurrency = configured per-instance limit x active instance count
```

Global hard limits should remain at a load balancer, API gateway, or purpose-built distributed
limiter until Option B has its own failure and scale semantics. Runtime/API/UI wording must say
`per-instance`; it must not imply a cluster-global quota. Config propagation remains eventual.

## Failed Calibration Reports

Failed reports were retained rather than deleted:

| Report suffix | Failure | Classification |
| --- | --- | --- |
| `122744777-46361-4a3fd0` | runner read the stream twice and locked `ReadableStream` | runner defect |
| `123012179-70716-aad9ee` | startup RSS/raw `lsof` rows used as leak baseline | invalid baseline |
| `123159647-86171-d88d29` | raw `lsof` rows still counted as FD | invalid FD metric |
| `123319442-97602-e59154` | numeric FD fixed, but client keep-alive sockets retained | fixture interference |
| `123907057-41136-99a51d` | 0 ms debug p95 was 132.67 ms vs 100 ms absolute bound | shared-debug absolute gate |

All five reports show successful cleanup and no change to port 9022. None is represented as a
product admission failure. They explain each runner correction and prevent later cherry-picking of
only green runs.

## Remaining Gates

- Freeze and rebuild after all scheduler command-reduction and auxiliary-attempt changes finish
  landing; the focused PostgreSQL test binary is not the final service binary.
- Run this full two-instance matrix for five rounds and require zero deadlock retry logs.
- Repeat release-mode load with an explicit 100 ms p95 threshold and record binary SHA.
- Add explicit `per-instance` scope wording to config/API and both UIs only after owner approval.
- Keep cluster-global admission out of this release unless its partition, crash, scale, and fallback
  semantics receive a separate design and chaos matrix.
