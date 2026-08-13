# Architecture, Performance, And State

Role: Detailed architecture and hot-path finding analysis
Status: Open findings; performance magnitude requires workload- and scenario-specific measurement
Authority: Evidence, hypotheses, and acceptance conditions for the listed IDs
As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11
Read when: Designing module boundaries, changing request/scheduler/storage paths, or defining benchmarks
Related: [Problem index](README.md), [Current module map](../../../../baseline/module-map.md), [Target architecture](../architecture/target-system-architecture.md), [Module contracts](../architecture/module-boundaries-and-contracts.md), [Performance contract and canonical workloads](../delivery/performance-contract-and-workloads.md), [Decision 005](../../decisions/005-scheduler-queue-and-lease-lifecycle.md), [Decision 010](../../decisions/010-fixed-operational-and-acceptance-policies.md), [Decision 011](../../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md)

## Performance Interpretation

Large source files do not directly cause runtime latency. The relevant mechanisms are lock contention, algorithmic scans, network/storage operations, allocation/copying, serialization, blocking work, queue behavior, connection reuse, and recovery after faults.

Existing [registered runtime evidence](../../../runtime-correctness-and-release-gates/history/evidence-index.md) shows that protocol, concurrency, load/chaos, restart, SIGTERM, and resource-recovery scenarios passed at the recorded revision. Historical production evidence also shows that real requests are substantially heavier than short fake calls: dozens of tools, large contexts, 30-60-second first-byte latency, long streams, and high token volume. These facts show that the current service cannot be dismissed as universally slow, but they do not establish a performance ceiling or protect the rewrite from regression.

Therefore:

- no performance item in this document is P0;
- verified hot-path operations are still valid findings even when current p95 is acceptable;
- severity depends on fault-injection and scaling results, not static line count;
- all changes require same-machine before/after metrics and resource recovery checks.

## `ARCH-001`: Central Objects Own Too Many Independent Responsibilities

Severity: P1/P2
Technical authority area: application architecture

### Evidence

- `MultiTokenManager` has 28 fields covering configuration, credential entries, refresh, PgSQL, Redis, proxies, statistics, runtime mutations, sticky sessions, queues, leases, and notifications: `src/kiro/token_manager/manager.rs:368`.
- `entries: Mutex<Vec<CredentialEntry>>` is acquired throughout the 8,178-line manager implementation; acquisition orchestration begins around `src/kiro/token_manager/manager.rs:3124` and spans selection, Redis, queueing, refresh, and persistence.
- `handlers.rs` is approximately 7,025 lines and coordinates parse, policy, route, local/external attempts, response translation, cache, usage, errors, and diagnostics.
- `AdminService` is approximately 6,602 lines and exposes credentials, proxies, pools, configuration, usage, catalogs, security, audit, and cleanup operations.
- `PostgresStore` and `RedisStore` each expose dozens of domain-specific methods across unrelated state classes.

### Impact

- lock and state ownership cannot be reasoned about locally;
- a change in one domain invalidates broad white-box tests and compile units;
- synchronous I/O can hide behind methods that appear to be state accessors;
- retry, persistence, and client-visible response behavior are intertwined;
- extracted helper files still depend on parent internals rather than stable contracts.

### Required Target

Treat the current God Objects only as characterization references while independently rebuilding complete modules behind explicit target services. Target runtime code never imports a legacy orchestration surface or God Object:

- `MessagesService` and request-scoped `ProcessingPlan`;
- pure `SchedulerCore` plus async `SchedulerCoordinator`;
- `CredentialCatalog`, `CapacityCoordinator`, `SessionAffinity`, and `RefreshCoordinator`;
- `ExternalPoolSelector`, transport adapter, error policy, and usage projector;
- domain-specific Admin command/query services;
- supervised usage/runtime/audit workers;
- repository and coordination ports.

### Acceptance

- lock ownership is documented per state object;
- pure scheduler/policy code has no I/O dependencies;
- handlers call narrow target application contracts rather than broad facades or storage/scheduler internals;
- module dependency checks prevent forbidden edges;
- each responsibility integrates only into the target-only candidate after deterministic offline comparison from immutable facts and focused gates; its superseded legacy implementation is then deleted during the same module work;
- post-deletion checks prove the complete candidate contains no legacy bridge, runtime selector, fallback, duplicate authority, live-traffic comparison path, or per-responsibility activation mechanism.

## `ARCH-002`: Domain And Infrastructure Have Bidirectional Compile-Time Dependencies

Severity: P2
Technical authority area: domain types, repositories, adapters

### Evidence

- `src/storage/postgres.rs:13` imports Anthropic usage/pricing/model, external-pool, and credential types.
- `src/storage/redis_cache.rs:10` imports usage Dashboard/query DTOs and scheduler types.
- `UsageRecorder` directly stores concrete `PostgresUsageStore` and `RedisStore`: `src/anthropic/usage.rs:1131-1146`.
- `ExternalPoolManager` directly owns concrete PgSQL and Redis stores: `src/external_pool.rs:879-885`.

### Impact

Storage schema/query changes and HTTP/dashboard DTO changes propagate across each other. Mocking requires constructing broad concrete objects. Physical file splits would retain the same coupling unless domain persistence records and narrow ports are introduced.

### Required Target

- neutral domain identifiers/events/policies;
- ports owned by the application/domain need, not by a database library;
- PgSQL/Redis adapters implement ports and map to persistence records internally;
- HTTP/Admin/dashboard DTO mapping stays in transport/query adapters;
- domain has no Axum, reqwest, sqlx, Redis, or frontend dependencies.

### Acceptance

Static dependency checks prove the allowed direction. A pure domain/scheduler test target compiles without database or HTTP adapters.

## `PERF-001`: Synchronous Storage Work Enters Request Completion

Severity: P1/P2; magnitude requires storage-latency injection
Technical authority area: credential runtime state, Redis lease completion

### Evidence

- Credential success persistence enters `persist_success_state`: `src/kiro/token_manager/manager.rs:5425`.
- The PgSQL mutation starts a transaction and performs generation/revision/deduplication operations: `src/storage/postgres.rs:2317` onward.
- `InFlightLeaseGuard::release` releases local state and then calls `block_on_storage` for Redis release/wakeup: `src/kiro/token_manager/concurrency.rs:231-268`.
- The Redis critical operation timeout is two seconds: `src/kiro/token_manager/concurrency.rs:23`.

### Impact Hypothesis

PgSQL or Redis latency can extend request tail completion, occupy blocking threads, and amplify pool pressure. The local lease is released first, so this is not equivalent to a guaranteed two-second local capacity leak; the concern is tail cost and executor/blocking-pool pressure.

### Required Target

- successful high-frequency runtime outcomes become idempotent, mergeable events handled by a supervised writer;
- strong generation/reset/disable transitions retain transactional CAS;
- local lease release completes immediately;
- Redis release uses a supervised high-priority path with bounded fallback and active-lease registry;
- request traces expose persistence-tail time separately from client response time.

### Acceptance

Inject PgSQL/Redis delays of 20, 100, and 500 ms and compare p95/p99, blocking tasks, pool wait, queue depth, and lease recovery before/after. No required event may be silently lost.

## `PERF-002`: Repeated Storage Round Trips Occur During Routing And Sticky Handling

Severity: P1/P2
Technical authority area: scheduler coordination, external availability

### Evidence

- Sticky acquisition and binding paths can read binding state more than once during a successful request: `src/kiro/token_manager/manager.rs:2870-2915`, `3163`, `3562`, and `6275`.
- External raw direct and preflight checks can each query availability before a local request is parsed: `src/anthropic/handlers/request_entry.rs:37` onward.
- External pool availability can load pool definitions and then query capacity/cooldown per pool: `src/external_pool.rs:2116-2160`.

### Required Target

- one `SessionBindingSnapshot` per acquire attempt;
- one request-scoped external availability snapshot;
- event-driven immutable pool definitions;
- batch Redis Lua/pipeline for capacity/cooldown state;
- metrics for PgSQL/Redis commands per request and per candidate set.

### Acceptance

The accepted scheduler workload assigns numeric PgSQL/Redis round-trip and operation budgets for one scheduling decision; the budget remains constant as credential/pool count grows, and the evidence reports actual counts rather than only wall-clock latency.

## `PERF-003`: Scheduler Uses A Broad Lock And Repeated O(N) Scans

Severity: P1/P2
Technical authority area: scheduler state and candidate indexing

### Evidence

- Mutable credentials are stored as `Mutex<Vec<CredentialEntry>>` in `MultiTokenManager`.
- Candidate selection scans and allocates candidate collections: `src/kiro/token_manager/manager.rs:2760`.
- Acquire and lease paths perform additional cleanup/scans around `src/kiro/token_manager/manager.rs:3124-3603` and `2172`.

For N credentials and repeated attempts, cost is at least multiple O(N) passes plus shared-lock contention. This can be acceptable for small N and still become a bottleneck at 100-1,000 synthetic credentials.

### Required Target

- immutable static credential metadata snapshot;
- ID-to-slot index and model/endpoint eligibility indexes;
- separately owned dynamic scheduling facts;
- one batch runtime snapshot and pure ranking pass;
- no network I/O while a shared candidate lock is held;
- deterministic offline parity comparison from the same immutable fact bundle, with no lease, refresh, mutation, live-traffic duplication, runtime selector, or duplicate upstream execution.

### Acceptance

Benchmark 10, 100, and 1,000 credentials at concurrency 32, 128, and 512. Record scheduler p50/p95/p99, lock wait, allocations, candidates scanned, and selection parity.

## `PERF-004`: Runtime Configuration And Request State Are Repeatedly Cloned

Severity: P2; mixed-version correctness is tracked separately as `COR-005`
Technical authority area: configuration publication and request context

### Evidence

- Top-level `Config` begins at `src/model/config.rs:2500` and has about 101 fields.
- `AppState` begins at `src/anthropic/middleware.rs:38` and expands many of the same policies/services.
- `RequestRuntimeConfig` begins at `src/anthropic/handlers.rs:607` and copies another large subset.
- `runtime_config()` clones complete config state: `src/kiro/token_manager/manager.rs:1091`.
- Request entry and main handling each materialize overlapping runtime state at `src/anthropic/handlers/request_entry.rs:12` and `src/anthropic/handlers.rs:4305`.

### Required Target

Separate `BootConfig` from immutable, versioned `RuntimeSnapshot`, publish an `Arc` atomically, and acquire it once in `RequestEnvelope`. Group policy substructures behind `Arc` so unchanged sections share allocation across versions. `COR-005` owns the requirement that one request cannot mix versions; this performance item owns clone/allocation reduction.

### Acceptance

Allocation profiles show no complete-config clone in the Messages hot path and quantify request-context allocation reduction. Mixed-version tests close `COR-005`, not this item.

## `PERF-005`: Request Artifacts Are Repeatedly Parsed, Cloned, Canonicalized, And Serialized

Severity: P2
Technical authority area: payload artifacts, prompt cache, diagnostics, endpoint transport

### Evidence

- Provider/endpoint/compression code parses or serializes request JSON in multiple stages: `src/kiro/provider.rs:3685`, `src/kiro/endpoint/ide.rs:166`, `src/kiro/endpoint/cli.rs:206`, `src/http_client.rs:36`.
- Prompt-cache flatten/profile/append paths clone values and canonicalize blocks more than once: `src/anthropic/prompt_cache.rs:283`, `717`, `1034`, `1302`.
- External payload-guard retry clones payload and route state: `src/external_pool/retry_pipeline.rs:20`.

### Required Target

Use a request-scoped artifact store keyed by `PayloadRevision`:

- raw bytes;
- lazily parsed Anthropic value;
- typed effective body;
- canonical cache bytes/hash/token metadata;
- final serialized target body;
- optional diagnostic breakdown only when enabled.

One revision should be serialized once per concrete target attempt, and mutation must invalidate dependent artifacts.

### Acceptance

Counters assert raw passthrough performs no full parse and each unchanged target revision performs at most one final serialization/canonicalization.

## `PERF-006`: Usage Batching Still Amplifies Storage Work

Severity: P1/P2
Technical authority area: usage writer and rollups

### Evidence

- Writer queues are bounded at 4,096 with maximum batch 64: `src/anthropic/usage.rs:24-32`.
- PgSQL batch handling still iterates records: `src/storage/postgres.rs:3833`.
- Totals, time buckets, cache, duration, and credential summaries involve repeated operations: `src/storage/postgres.rs:5139` onward.
- Queue saturation can wait and synchronously fall back from the request path: `src/anthropic/usage.rs:1064`, `1486`.

### Required Target

- multi-row insert/upsert in one transaction;
- append-only authoritative event with unique ID;
- set-based rollup or asynchronous derived worker;
- Redis multi-event pipeline/Lua;
- explicit degradation of optional dimensions rather than hidden request-path fallback;
- backlog age and storage operations/event metrics.

### Acceptance

The accepted usage workload assigns numeric statement, transaction, row, and derived-projection operation budgets per 64-event batch. The measured batch satisfies those absolute budgets and the accepted old-versus-replacement regression threshold; fault/restart replay remains idempotent.

## `PERF-007`: PDF And Optional Remote Tokenizer Can Block Request Execution

Severity: P1/P2 when the relevant feature/path is exercised
Technical authority area: content conversion and token counting

### Evidence

- PDF extraction executes in conversion code and uses a global standard mutex: `src/anthropic/converter/content.rs:263`, `326`.
- Configured remote token counting uses `block_in_place`, creates a client, clones payload, and permits a long timeout: `src/token.rs:110-170`.

### Required Target

- separate bounded `spawn_blocking` pools/weighted semaphores for PDF work;
- reusable remote tokenizer client and end-to-end async interface;
- request/global work-unit and byte budgets;
- cancellation and stage-specific deadlines;
- no blocking bridge in the ordinary async handler path.

### Acceptance

Concurrent PDF/tokenizer tests measure Tokio heartbeat lag, queue wait, RSS, cancellation, and unrelated request p99. Work cannot exceed configured global permits.

## `PERF-008`: Kiro Requests Disable Connection Reuse

Severity: P2 conditional
Technical authority area: Kiro HTTP transport

### Evidence

Kiro upstream requests explicitly set `Connection: close`: `src/kiro/provider.rs:2749`. Provider clients are otherwise cached by proxy configuration: `src/kiro/provider.rs:76-89`, `947`.

### Risk

HTTP/1.1 TCP/TLS connections may be recreated per request, increasing latency and socket churn. The header may also exist for upstream compatibility; removing it without real Kiro validation is unsafe.

### Required Target And Acceptance

Preserve explicit `Connection: close` conservatively in the isolated target construction until a bounded low-volume `G-KIRO` comparison establishes the final target transport profile. The comparison records success/protocol behavior, new TCP/TLS connections, handshake time, TTFB, FD recovery, and proxy/TLS-backend combinations. The accepted result freezes one profile before the complete candidate is released; it never duplicates production traffic or creates per-module activation or independently selectable behavior.

## `PERF-009`: Lease-Acquire Lua Performs Unbounded Stale Cleanup

Severity: P1/P2 conditional on stale-lease cardinality

Technical authority area: local/external scheduler Redis coordination

### Evidence

- Local acquire calls `ZRANGEBYSCORE` without `LIMIT` for credential and global expired sets and loops over every returned member: `src/storage/redis_cache.rs:2538-2566`.
- External-pool acquire repeats the same unbounded pattern for pool and global sets: `src/storage/redis_cache.rs:2681-2691`.
- Redis executes each Lua script atomically on its command thread; one request can therefore make cleanup work proportional to all historical stale members before ordinary clients proceed.

Accepted decisions 005, 010 and 011 require constant-bounded scheduler operations, finite admission and resource-governor enforcement, but those target contracts do not themselves close this verified current mechanism. `RES-005` can increase the stale population while current admission remains unlimited.

### Required Target

- each acquire/cleanup invocation processes a fixed maximum member count or time budget independent of total stale cardinality;
- stale cleanup advances through an explicit cursor/batch/reconciliation owner and remains idempotent under retry, crash and duplicate workers;
- capacity counters, weights, fencing and per/global membership remain correct while cleanup is incomplete;
- script duration, members scanned/removed, remaining backlog, Redis command latency and blocked-client indicators are observable without per-lease labels;
- local and external key classes use versioned scripts and rollback-compatible key semantics.

### Acceptance

- Deterministic tests use 1, 1,000 and 100,000 stale leases plus concurrent acquire/complete/cancel operations.
- One script invocation never exceeds the accepted member/time budget, and backlog drains without capacity drift or stale grant.
- Redis p95/p99/maximum command latency, scheduler latency and unrelated client progress remain within absolute and relative gates during cleanup and restart.

## Non-Solutions

- Splitting one large file into several files that all use `super::*` and the same shared state.
- Replacing every mutex with `DashMap` without defining ownership and consistency.
- Introducing traits around pure one-line helpers that have no alternate implementation or test need.
- Moving synchronous I/O behind another facade without making the async cost observable.
- Adding caches before proving invalidation and source-of-truth semantics.
- Rewriting the system as microservices before the in-process domain boundaries are stable.
- Optimizing for only a zero-delay fake upstream while ignoring long-lived production streams.

## Required Measurement Matrix

These dimensions feed the versioned [canonical workloads](../delivery/performance-contract-and-workloads.md#canonical-workloads). They are not permission to select ad hoc subsets after observing a result, and they do not require one unsafe Cartesian-product run.

| Dimension | Values |
| --- | --- |
| Credentials | 10, 100, 1,000 synthetic |
| Concurrency | 1, 16, 32, 64, 128, 512 where host-safe |
| Payload | 4 KiB, 1 MiB, representative 20 MiB edge |
| Response | stream/non-stream, fast, 30/60s first byte, >180s active stream |
| Dependencies | normal, 20/100/500ms PgSQL/Redis delay, partial failure, disconnect/restart |
| Cache | miss, creation, read, eviction, shaped payload, no-cache, external projection |
| Pools | local only, 1/5/20 external pools, raw, normalized, fallback, rescue |
| Client | normal reader, slow reader, stopped reader, disconnect |

Every run records offered/launched/completed outcome distribution, success throughput, scheduler latency/lock wait, DB/Redis ops and pool wait, TTFB/local-stage/total/tail p50/p95/p99 with sample counts, CPU, RSS, FD, tasks, connections, queue backlog, permits, cost where applicable, and recovery through the accepted idle window. Host-safe concurrency and artifact budgets are mandatory; metric and target-process validity follow `TEST-004` and the performance contract.
