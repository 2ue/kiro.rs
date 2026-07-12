# Performance Contract And Canonical Workloads

Role: Binding measurable definition of high performance, workload identity, metric semantics, harness validity, and complete-candidate performance evidence

Status: Accepted contract and minimum thresholds; no modernization performance run exists

Authority: Defines the binding `G-PERF` contract under decisions 010, 011 and 014; workload manifests may specialize safely but cannot weaken these minima

As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-12

Read when: Building or reviewing the performance harness, adding target instrumentation, changing request/scheduler/storage/resource behavior, comparing the legacy artifact and complete target candidate, running load/chaos, or preparing final cutover

Related: [Requirements](../requirements-and-quality-attributes.md), [performance findings](../problems/architecture-performance-and-state.md), [verification](verification-rollout-and-rollback.md), [rewrite sequence](migration-sequence.md), [question registry](../../open-questions.md), [traceability](../../indexes/traceability-matrix.md), [current resource baseline](../../../../baseline/resource-and-concurrency-model.md), [current test gates](../../../../baseline/test-and-release-gates.md)

## Performance Acceptance Model

“High performance” is not established by smaller files, one successful load run, or relative comparison alone. The complete target candidate passes `G-PERF` only when all applicable parts pass:

```text
correctness and compatibility gates
AND absolute outcome gate
AND relative regression gate
AND resource/recovery gate
AND harness/evidence validity gate
```

- The **absolute outcome gate** proves that the accepted deployment profile sustains its target workload, success/error policy, latency, resource, operation, and recovery limits.
- The **relative regression gate** compares the immutable legacy artifact and complete target candidate on the same reference host and canonical workload so a rewrite cannot consume materially more time or resources while still staying under a loose absolute ceiling.
- The **resource/recovery gate** applies even when latency is fast. A run fails on unbounded or unrecovered RSS, FD, task, connection, queue, lease, permit, backlog, file, or diagnostic growth.
- The **harness/evidence validity gate** fails closed when the target process, workload, sample count, metric source, cleanup, or artifact identity cannot be proven. Missing measurements are not zero and are not a pass.

An accepted correctness tradeoff may justify additional I/O or latency only through a recorded decision with new absolute capacity evidence. It cannot be hidden by weakening a workload after observing the result.

## Current Non-Claims

- The dated production observations are workload-shape evidence, not accepted SLOs or maximum capacity.
- The relative limits of throughput `-5%`, p95 `+10%`, p99 `+15%`, and peak RSS `+15%` are binding minima under decision 010.
- Concurrency values in the requirements are test dimensions, not a promise that every host, route, payload, or upstream sustains the largest value.
- Real Kiro and real Claude Code runs prove compatibility and low-volume behavior. They do not establish general capacity because upstream latency, quota, model behavior, and cost are not deterministic.
- No module work unit or complete candidate currently has a passing modernization performance evidence record.

## Reference Host Identity

Every baseline and comparison uses a stable `host_profile_id`. The evidence manifest records at least:

| Class | Required identity |
| --- | --- |
| Source | full commit, version, dirty-tree patch identity, legacy-artifact or target-candidate identity |
| Binary | absolute path, SHA-256, debug/release profile, features, allocator, Rust/toolchain version |
| Host | hardware model, architecture, physical/logical CPU count, memory, OS/kernel, power/thermal mode where observable |
| Runtime | process PID/start time/command, CPU affinity or limits, memory/container limits, environment/config hashes |
| Dependencies | PgSQL/Redis versions and isolated database/schema/prefix, pool sizes, injected latency/fault mode |
| Network | loopback/host/container topology, TLS/proxy mode, fake-upstream version and configured delay/chunk pattern |
| Deployment topology | replica case/count, expected instance identities, release-generation/transition IDs, instance and frontend digests, load-balancer algorithm/health policy, per-replica resource/pool allocations and shared dependency capacity |
| Frontend/CLI when used | Node/pnpm/browser/Claude CLI versions and isolated HOME/config identity |
| Host safety | pre-run available memory/disk, protected services/ports, outer timeout, process/file/byte limits |

`REF-HOST-PRIMARY` is explicitly a **single-replica** decision-010 release-runner profile: exactly one target process, at least 8 logical CPUs and 16 GiB RAM available to that replica, an explicit process/container limit, release build, fixed dependencies and local isolated PgSQL/Redis. Each run still records the concrete hardware/OS identity. A secondary constrained-host profile supplements but cannot replace this primary comparison.

`REF-HOST-MULTI` has two mandatory, separately reported cases against the same canonical corpus and dependency profile:

1. `replica_case: exactly-two` runs exactly two equivalent target replicas. Each replica has the same binary/configuration/generation and resource-governor profile and an allocation equivalent to `REF-HOST-PRIMARY`; a colocated runner must prove that CPU and memory are not oversubscribed. One shared isolated PgSQL/Redis pair and the load balancer are part of the measured topology.
2. `replica_case: release-generation` runs the exact supported production replica count and expected instance set/count from the decision-014 `ReleaseGenerationManifest`, including the manifest's per-replica pool/capacity allocation. It is not replaced by a two-replica extrapolation.

Both cases fail when expected and observed membership, generation ID, binary/frontend/schema/config digests, load-balancer route set, or shared dependency allocation differ. The exactly-two case must achieve the scaling gate below; the release-generation case must pass its declared absolute offered-load/capacity gate, all correctness/resource gates and safe replica-loss/rejoin behavior.

Old and replacement comparisons run from the same checkout/worktree state wherever possible, use the same dependency data and fake-upstream process, and alternate execution order across measured rounds. Reboot, thermal throttling, unrelated host pressure, dependency drift, or measurement-tool failure invalidates or explicitly qualifies the comparison.

## Harness Validity Contract

Before any result can count as `G-PERF` evidence, the harness must prove:

1. the measured PID/process identity belongs to the target proxy, not the load generator, fake upstream, shell, or a stale process;
2. every offered, launched, completed, cancelled, timed-out, panicked, and transport-failed operation is accounted for exactly once;
3. per-request latency uses that request's own start/end timestamps;
4. unavailable RSS/FD/CPU/task/metric samples are represented as unavailable and fail a gate that requires them; they are never converted to zero;
5. every percentile includes metric name, unit, population, sample count, success/error class, and percentile algorithm;
6. warmup, measured interval, drain, idle cooldown, and final recovery sampling are distinct phases;
7. resource time series samples the target throughout the run and cooldown, not only immediately before and after traffic;
8. target metrics can be correlated to the same run through run ID and bounded labels;
9. an outer watchdog terminates validation-owned process trees and records timeout/cleanup as a result;
10. every replica and load-balancer target is attributed separately while aggregate metrics reconcile to the same request population;
11. observed membership, generation/digests and shared dependency topology match the immutable workload and release-generation manifests;
12. the report schema is versioned and validated before threshold evaluation.

Until the verified `TEST-004` defects are fixed, `kiro_loadtest` output may support investigation but must not independently pass `G-PERF`.

## Canonical Workload Manifest

Every executable workload is a versioned manifest rather than an ad hoc command. The manifest contains:

```yaml
workload_id: PW-<domain>-<sequence>
revision: schema-and-corpus-version
purpose: absolute-capacity | relative-regression | multi-replica-scaling | fault-recovery | sustained-stability | compatibility
dependency_groups: [R<n>]
gates: [G-PERF]
host_profile_id: accepted-reference-host
replica_case: single | exactly-two | release-generation
release_generation: generation-id-manifest-revision-transition-id
replica_topology: expected-count-instance-ids-hosts-zones-and-observed-membership
artifact_digests: backend-frontends-schema-plan-and-config-schema
load_balancer: algorithm-health-policy-target-set-connection-reuse-and-digest
per_replica_allocation: cpu-memory-governor-pg-redis-and-socket-profile
shared_dependency_capacity: postgres-redis-limits-reserves-and-pool-sum-proof
driver: exact-binary-or-script-hash
target: per-instance-pid-command-port-binary-digest
corpus: fixture-path-and-sha256
routes: weighted-route-and-mode-mix
models: requested-model-profile
credentials_pools: synthetic-counts-limits-and-policy-hash
dependencies: postgres-redis-fake-upstream-profile
arrival: closed-loop-concurrency | offered-rate
offered_rate: accepted-value-or-not-applicable
concurrency: fixed-values
warmup: duration-and-unscored-request-count
measurement: duration-and-or-request-count
cooldown: idle-duration-and-recovery-sample-cadence
repetitions: count-and-old-new-order
timeouts: client-stage-and-outer-watchdog
budgets: rss-fd-task-connection-disk-files-and-cost-stop-limits
thresholds: accepted-absolute-relative-and-scaling-profile
artifacts: manifest-report-metrics-secret-scan-cleanup
```

Changing payload shape, route mix, target/replica counts, membership, generation/digests, load-balancer topology, pool allocation, fake delay, arrival model, duration, or threshold creates a new manifest revision. A failed run is not rerun with an easier workload under the same ID.

## Canonical Workloads

The following IDs define required workload families. Decisions 010/011/014 fix release minima; each versioned manifest fills exact corpora, route weights and safe host-specific values without weakening them.

| Workload | Purpose and fixed dimensions | Binding minimum or specialization rule |
| --- | --- | --- |
| `PW-RAW-001` | External raw stream/non-stream; original bytes; 10 ms deterministic fake upstream; asserts zero full parse/media/Kiro conversion/payload guard/unconditional token count | Concurrency 64, >=200 success/s, >=99.9% success, 0 unexpected failure, local overhead p95/p99 <=25/75 ms |
| `PW-MSG-001` | Local Kiro and external normalized stream/non-stream; 4 KiB ordinary payload; concurrency `1/16/64/128` | Concurrency 64, >=100 success/s, >=99.9% success, 0 unexpected failure, local overhead p95/p99 <=25/75 ms |
| `PW-SCHED-001` | Pure scheduler and coordinated acquire lifecycle; `10/100/1,000` candidates; finite admission saturation; `1/1,000/100,000` stale leases; concurrency `32/128` | Pure 1,000-candidate p95/p99 <=2/5 ms; Redis script p95/p99 <=5/10 ms; <=128 stale members/call |
| `PW-PAYLOAD-001` | 4 KiB, 1 MiB and representative 20 MiB edge; long histories, tools/results, nested schemas and thinking | Corpus hash is versioned; decision-010 byte/depth limits and relative CPU/RSS/latency gates apply |
| `PW-INGRESS-001` | Slowloris/slow headers, honest/missing/dishonest `Content-Length`, chunked upload, slow/stopped body, keepalive churn, HTTP/2 concurrent-stream saturation and unauthorized body-before-auth attempts | Decision-011 listener/header/body/idle/age/HTTP2 limits; reserve before retained bytes, no unauthorized body retention, stable overload, reserved health/control progress and recovery within 60 seconds |
| `PW-STRUCTURE-001` | Message/content/tool/schema node, edge, property, required/dependency, string and depth boundaries; below/at/above each limit; cancellation during traversal; raw-direct zero-traversal cases | Decision-011 cardinality limits and weighted cost-class accounting; traversal work is linear in accepted nodes/edges, stops at the first hard bound and releases all scoped permits |
| `PW-RESOURCE-001` | Raw/streaming, normalized, and remote/media/PDF/tokenizer/non-stream-heavy classes at floor and maximum declared live bytes; unknown lengths, atomic stage upgrades, cancellation and overload | Charge `max(class_floor, ceil(1.5 * live_bytes))` with 8/16/64-MiB floors, reserve unknown length at its accepted maximum, never exceed the one process ledger and recover permits/resources within 60 seconds |
| `PW-SLOW-001` | First-byte delays `3/10/30/60s`; progressing stream beyond `180s`; slow/stopped/disconnected clients; oversized response variants | Decision-010 response/cache/lease limits, no leaks and recovery within 60 seconds |
| `PW-STORE-001` | PgSQL/Redis baseline plus injected `20/100/500ms`; partial failure, disconnect, restart/replay; migration/backfill cases | Fixed operation budgets below, terminal p95/p99 <=50/150 ms and exact recovery/replay |
| `PW-BURST-001` | Steady then abrupt overload, partial/widespread faults, fallback/rescue and recovery | Finite configured bounds, overload p95 <=100 ms, no bound bypass and recovery within 60 seconds |
| `PW-MEDIA-001` | Remote `0/1/8/>limit` sources, PDF/tokenizer concurrency, bytes/permit exhaustion and cancellation | Decision-010 permit formula and byte limits; no unrelated starvation; recovery within 60 seconds |
| `PW-STABILITY-001` | Sustained stream/non-stream mix with proxy/client rotation, cache/queue/stale cleanup and dependency fault/recovery | At least 60 minutes and 100,000 completed requests, three process restarts, Redis restart/rebuild and PgSQL loss/recovery |
| `PW-MULTI-001` | One-replica control, exactly-two equivalent replicas and the actual release-generation replica count; identical corpus/config/generation, shared PgSQL/Redis and measured load-balancer distribution; replica loss/partition/rejoin | Exactly two at twice the single offered load: aggregate capacity >=1.7x, p95 <=single +15%, p99 <=single +20%, per-launched-request PgSQL/Redis operations <=single +5%; actual release count passes its manifest-specific absolute gate without oversold shared limits |
| `PW-CLI-001` | Real Claude Code compatibility | Three independent sessions, at least 20 turns each, predeclared request/token/duration cap; not capacity evidence |
| `PW-KIRO-001` | Low-volume real Kiro compatibility using independent logical operations | At most 20 requests or lower account cap, with request/token/duration/cost/error stops; not capacity evidence |

Development validation may run a documented reduced member, but it cannot replace the complete-candidate release manifest. The reduction and omitted dimensions remain visible.

## Metric Semantics

### Traffic And Outcome

| Metric | Definition |
| --- | --- |
| `offered` | operations scheduled by the workload's arrival model, including operations not launched because the client-side concurrency ceiling or deadline was reached |
| `launched` | operations for which the driver began the HTTP/application call |
| `completed` | launched operations reaching one classified terminal driver result; must equal success + expected failure + unexpected failure + cancelled/timeout classes |
| `success` | completed operations satisfying the workload's expected HTTP/protocol/usage assertions |
| `expected_failure` | deliberately injected failure returning the specified normalized class; never counted as success throughput |
| `unexpected_failure` | every other error, task panic, missing result, malformed response, or assertion failure |
| `success_throughput` | successful operations divided by measured wall-clock seconds, excluding warmup and cooldown |
| `offered_rate_achievement` | launched/offered plus schedule-lag distribution for offered-rate workloads; silently delaying or dropping arrivals is not full achievement |
| `success_rate` | success divided by launched operations for success-expected traffic; injected expected failures are reported separately |

Absolute capacity is the highest accepted offered-rate/concurrency step that simultaneously satisfies success/error, latency, operation, resource, and recovery gates. Merely launching the requested concurrency is not capacity proof.

### Latency

- Report queue wait, scheduler decision/acquire, request planning/materialization, payload guard, upstream connect, response header, first chunk, first thinking, first text, response translation, terminal durable acknowledgement, and total latency separately when applicable.
- End-to-end TTFB and total latency are reported but do not replace proxy-local stage metrics.
- For deterministic fake upstream, `local_overhead` is derived from synchronized proxy/fixture stage timestamps or an explicitly defined known-delay subtraction. Negative or missing values invalidate that derivation rather than becoming zero.
- Success, expected-failure, and unexpected-failure latency populations are separate.
- p50/p95/p99 results without decision-010 minimum samples are marked insufficient: 10,000 ordinary successes and 1,000 applicable tail/fault cases unless a finite corpus is smaller and fully executed.
- Timeout results retain their configured timeout and class; they do not use whole-run elapsed time.

### Scheduler, Queue, And Storage

Each applicable request or event reports:

- candidates considered and rejected by reason;
- pure scheduler latency and shared-lock wait/hold time;
- queue admission result, wait, wakeups, cancellations, late-grant count, depth and oldest age;
- lease acquire, heartbeat, complete/cancel and reconciliation lag;
- stale members scanned/removed/remaining per invocation, script result cardinality and script/blocked-client latency;
- PgSQL statements/transactions/rows, pool wait and query latency;
- Redis commands/scripts/round trips, pool wait and operation latency;
- writer batch size, statements/event, backlog count/bytes/oldest age and persistence tail.

Ingress/resource metrics additionally report accepted TCP connections and public/Admin/health streams, header/read deadlines, retained body bytes, reservation upgrades/rejections, weighted charge by cost class, structured nodes/edges/strings visited, HTTP/2 streams per connection and control-plane progress. The scaling gate divides PgSQL/Redis aggregate operations by all launched requests and includes every zero-operation launch; completed/success-only denominators are reported only as diagnostics and cannot pass the regression gate.

An accepted workload threshold gives numeric operation budgets. “Constant-bounded” or “substantially lower” is a design direction, not a pass result.

### Process And Resource

Resource time series uses explicit units and source validity:

- proxy RSS/current/peak and RSS after accepted idle cooldown;
- CPU time and normalized CPU utilization where supported;
- open FD and established/total connection counts;
- runtime task count, blocking tasks, queue/backlog, permits and leases;
- admitted/in-flight/queued requests, upstream response bytes and reusable-client cache entries/active/idle/evictions;
- request/artifact/cache/File/diagnostic retained entries and bytes;
- PgSQL/Redis client-pool occupancy and waiters;
- validation-owned disk bytes/files and cleanup residue.

`idle_recovery` passes only within 60 seconds when RSS is <= `max(baseline + 64 MiB, baseline * 1.10)`, FD/tasks/connections are <= baseline + 5, and no queue/lease/file/backlog residue violates its durable policy. Repeated rounds show no positive growth trend.

### Cost And Real-Upstream Safety

Every real Kiro or real Claude Code workload declares before execution:

- maximum requests and sessions;
- maximum input/output/cache tokens when observable;
- maximum duration;
- maximum monetary/account quota exposure when price/quota data is available;
- maximum expected and unexpected errors;
- immediate abort conditions and the isolated authorized test account/profile.

The driver stops before exceeding a hard budget. Unknown cost is not interpreted as zero; it requires a conservative request/token cap. Cost evidence contains counts/totals and sanitized profile identity, never credentials or prompt bodies.

## Absolute Outcome Gate

Decisions 010/011/014 fix the release minima:

| Outcome | Binding value |
| --- | --- |
| target offered load and concurrency | `PW-RAW-001` >=200 success/s and `PW-MSG-001` >=100 success/s at concurrency 64 with 10 ms fake upstream |
| success/expected/unexpected outcomes | >=99.9% success for success workloads, exactly expected injected failures, 0 unexpected failure |
| local/scheduler/Redis/terminal/overload latency | local p95/p99 <=25/75 ms; scheduler <=2/5 ms; Redis script <=5/10 ms; terminal <=50/150 ms; overload p95 <=100 ms |
| ordinary peak RSS | <=1 GiB and within relative regression limit |
| Redis script work | <=128 stale members per request-path invocation; remaining backlog observable and convergent |
| admission/queue/cache/body/diagnostic/lease state | decision-010 count/byte/age/timing limits; no supported `0 = unlimited` |
| inbound protocol, structure and cost class | `PW-INGRESS-001`, `PW-STRUCTURE-001` and `PW-RESOURCE-001` pass every decision-011 connection/header/body/read/keepalive/HTTP2/cardinality/weighted-charge ceiling, preserve the reserved control channel and reject before over-limit retention/traversal/allocation |
| topology and generation identity | observed replica set/count, generation/transition IDs, artifact/schema/config digests, load-balancer targets and dependency/pool allocation exactly match the workload plus decision-014 manifests; any mismatch is a failed/invalid run, never a qualified pass |
| multi-replica deployment | both `REF-HOST-MULTI` cases pass `PW-MULTI-001`; exactly-two meets the numeric scaling gate and the actual production count meets its manifest-specific absolute offered-load/capacity gate |
| recovery | <=60 seconds and exact residual bands defined above |
| stability | >=60 minutes and >=100,000 completions plus required restart/fault cycles |
| real upstream | Kiro <=20 requests or lower account limit; Claude Code 3 sessions x >=20 turns; all runs predeclare request/token/duration/cost/error stops |

Binding request-path operation budgets are:

| Operation | Maximum normal-path budget |
| --- | --- |
| Runtime config/auth/catalog data access after request capture | 0 PgSQL and 0 Redis calls; bounded auth-epoch revalidation is separately metered |
| Scheduler acquire | 1 Redis script round trip per attempt |
| Scheduler heartbeat | 1 Redis script round trip per 15-second heartbeat interval while active |
| Scheduler complete/cancel | 1 Redis script round trip plus bounded supervised retry only on failure |
| Terminal durable acceptance | 1 PgSQL transaction and at most 4 statements |
| Usage append batch of up to 64 events | 1 PgSQL transaction and at most 4 statements |
| Usage Redis projection batch of up to 64 events | at most 2 Redis script round trips |
| Admin mutation including sealed audit append and any required domain outbox/job write | 1 PgSQL transaction and at most 6 statements |

An accepted correctness change may use a different constant budget only through a superseding decision and absolute capacity proof.

Structural zero/identity invariants already defined elsewhere, such as raw heavy-stage count `0`, one payload serialization per unchanged revision, exact terminal IDs, and no duplicate scheduler/storage effect, remain mandatory independent of calibration.

## Relative Regression Gate

Binding relative limits are:

- success throughput decrease no more than 5%;
- selected successful-response p95 increase no more than 10%;
- selected successful-response p99 increase no more than 15%;
- peak proxy RSS increase no more than 15%;
- no increase in DB/Redis operation count unless an accepted correctness decision states and budgets the added work;
- no regression from valid measurement to missing/invalid measurement;
- no new monotonic growth or longer recovery beyond the accepted absolute gate.

They apply to comparable deterministic canonical workload metrics. End-to-end real-upstream latency is compatibility evidence and does not independently fail a local performance comparison unless the upstream profile is deterministic.

Each comparison uses at least five alternating legacy/target measured rounds. Reports retain every failed/noisy/invalid round and the adjudication; rerunning until one pass does not erase earlier evidence.

## Multi-Replica Scaling Gate

`PW-MULTI-001` first establishes a passing single-target control on `REF-HOST-PRIMARY`, then uses the unchanged corpus, route mix, fake-upstream delay, thresholds and per-replica resource profile on `REF-HOST-MULTI`.

The exactly-two case offers twice the passing single-replica load and passes only when all of the following hold:

- aggregate successful throughput is at least `1.7x` the passing single-replica throughput;
- success remains at least 99.9% with zero unexpected failures;
- selected successful-response p95 is no more than `+15%` and p99 no more than `+20%` versus the single-replica control;
- PgSQL statements/transactions/round trips and Redis commands/scripts/round trips per launched request are each no more than `+5%`;
- load-balancer distribution, shared pool wait, connection use, lease/admission totals and permit accounting prove that neither a replica nor a shared authority is bypassed or oversold;
- loss/partition of one replica yields bounded overload or fail-closed behavior, no duplicate logical effect, and accepted recovery/rejoin under the same generation rules.

The release-generation case uses the exact production replica count from the immutable manifest and runs even when that count is two. It passes its predeclared absolute offered-rate/capacity, latency, operation, resource and recovery thresholds; it cannot claim support from the two-replica ratio alone. Changing replica count, instance set, pool allocation, resource profile, generation/digests or load-balancer topology creates a new manifest revision and invalidates comparison with unmatched rounds.

## Harness And Evidence Sequence

### R0 Dependency Group: Final Harness

The R0 dependency group implements the final measurement harness once:

- versioned workload/report schema and manifest validator;
- correct target-process identity;
- exact launched/completed/error/task accounting;
- invalid-versus-zero metric representation;
- warmup/measurement/cooldown phases and cleanup watchdog;
- one deterministic smoke workload proving the harness rather than product capacity;
- deterministic safety corpora for response/cache/admission/script bounds without modifying the legacy production path.

This closes only `TEST-004` measurement validity. It does not establish target performance.

### R1 Dependency Group: Manifests And Instrumentation

The R1 dependency group adds:

- concrete single-replica and multi-replica reference-host identities satisfying decisions 010/014, including the exactly-two and actual release-generation cases;
- request-stage, scheduler, admission/queue, response-byte, client-cache, Redis-script/backlog, storage/migration-operation and resource metric sources;
- canonical workload manifests needed by target modules and the final candidate;
- immutable legacy-artifact baseline runs;
- the already accepted decision-010 absolute and relative thresholds.

No target performance claim is valid when required metrics or host identity are absent.

### Module And Aggregate Gates

Each performance-affecting module selects workload IDs through traceability, adds domain-specific fixtures/faults and records focused target results. After integration, aggregate target-only workloads rerun. These results guide correction but do not authorize a module production switch.

### R9 Dependency Group And Complete Candidate

The R9 dependency group integrates the unchanged valid harness, workload manifests, fault controllers, threshold evaluation and evidence validation into release orchestration. Only the complete post-deletion candidate runs the binding legacy-versus-target comparison and sustained stability gate.

## Evidence And Pass/Fail

A passing performance record includes:

- canonical workload and schema versions plus fixture hashes;
- reference host, replica case/count, release-generation/digest, load-balancer and shared-dependency identity plus validity checks;
- every target instance and load-generator process identity;
- warmup/measurement/cooldown timing and sample counts;
- offered/launched/completed/outcome accounting;
- actual absolute and relative thresholds with metric populations;
- stage, operation, resource, recovery and cost summaries;
- raw artifact manifest/hash/retention and threshold-evaluator version;
- prior failed/invalid runs and their disposition;
- secret scan, process/dependency/artifact cleanup and remaining residue.

The result is `Blocked`, not `Passed`, when the canonical manifest is incomplete, a required metric is missing, the target identity is not proven, or the fault/cleanup controller cannot establish its technical authority and cleanup scope.

## Manifest Completion Checklist

Each release manifest supplies:

1. concrete reference-host and dependency identity;
2. replica case/count, expected instance membership, release-generation/transition identity, artifact/schema/config digests, load-balancer topology, per-replica allocations and shared-pool capacity proof;
3. canonical workload/corpus revision and hashes;
4. fixed decision-010/011 absolute/relative/scaling thresholds plus any stricter profile limits;
5. warmup, sample, five-round alternating order and percentile-validity details;
6. sustained-stability and idle-recovery settings;
7. real-upstream request/token/duration/cost/error stops;
8. any superseding correctness decision for a changed constant operation budget;
9. R0 dependency-group harness-validity and final-candidate evidence identities.
