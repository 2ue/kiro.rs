# Requirements And Quality Attributes

Role: Durable system requirements
Status: Accepted requirements baseline
Authority: Product requirements and invariants for the modernization plan; accepted decisions override implementation suggestions
As of: `v0.0.102`, commit `e9479df71ee0`, updated 2026-07-12
Read when: Designing, reviewing, implementing, testing, integrating, or accepting the complete target system
Related: [Business context](../../../baseline/business-context.md), [Problems](problems/README.md), [Continuous audit](problems/continuous-audit-and-finding-lifecycle.md), [Traceability](../indexes/traceability-matrix.md), [Target architecture](architecture/target-system-architecture.md), [Verification](delivery/verification-rollout-and-rollback.md)

## Normative Language

- **MUST** means a target module cannot integrate or the complete release cannot be accepted without satisfying the applicable requirement.
- **SHOULD** means the requirement is expected unless an accepted decision records a concrete exception and consequence.
- **MAY** means an implementation choice that remains inside the required boundaries.

These clauses are binding through accepted decisions 001 and 003-014. Decision 002 is superseded for delivery mechanics. A later decision may explicitly supersede a clause, but implementation cannot silently weaken it.

## Fixed Product Constraints

1. The system MUST remain a single-user, single-trust-domain product unless a future product decision explicitly changes that model.
2. The design MUST NOT introduce tenant IDs, tenant repositories, per-tenant data partitioning, tenant billing, or tenant schedulers.
3. Multiple request API keys MUST be treated as equivalent access credentials for the same operator, with rotation and revocation support but no user identity semantics.
4. Multiple Kiro credentials and external pools MUST be treated as operator-owned capacity resources.
5. Single-process development and supported multi-replica production profiles MUST preserve the accepted semantics. Cross-replica behavior is an HA concern, not user isolation.
6. The complete first-party implementation MUST be rewritten using the 50 target authority modules. Modules integrate only into the target-only candidate; production activates and rolls back the complete system once, and the target release contains no legacy selector/fallback.
7. The product SHOULD remain a modular monolith and single deployable binary during this rewrite. A microservice split requires separate evidence and decision.
8. Current successful Anthropic, Claude Code, Kiro, external-pool, Admin, PgSQL, and Redis contracts MUST be preserved unless an accepted decision explicitly changes a documented defect.

## Functional Requirements

### Protocol And Routing

- `FUN-001`: The gateway MUST support streaming and non-streaming Messages calls for `/v1`, `/cc/v1`, `/ha/v1`, `/na/v1`, and configured `/dfcache/{route}/v1` paths.
- `FUN-002`: `/cc/v1` MUST preserve Claude Code-compatible SSE event order, thinking/signature behavior, tool use/result pairing, final `message_delta.usage`, model aliases, Files, MCP/tool/search workflows, and normalized errors.
- `FUN-003`: Route policy MUST be represented explicitly. Path behavior MUST NOT emerge from unrelated booleans scattered across handlers.
- `FUN-004`: External raw passthrough MUST preserve original body bytes except for transformations explicitly enabled by the selected pool and recorded in diagnostics.
- `FUN-005`: External normalized mode MUST apply only the selected pool's model, body, payload, response, and usage policies.
- `FUN-006`: `preservePath=true` MUST forward the validated inbound route path, including `/cc`, `/ha`, `/na`, and `/dfcache`; `false` MUST use the configured canonical Messages endpoint.
- `FUN-007`: Once response headers or SSE bytes have been sent to the downstream client, the request MUST NOT fail over to another credential or external pool.

### Scheduling And Upstreams

- `FUN-010`: Scheduler eligibility MUST account for enabled state, model support, endpoint/proxy availability, cooldown, RPM, concurrency, health, priority, excluded attempts, and session affinity.
- `FUN-011`: A scheduling decision MUST use one coherent candidate/runtime snapshot; it MUST NOT perform unbounded per-candidate network I/O.
- `FUN-012`: Every acquired local or Redis lease MUST be released on success, error, cancellation, timeout, client disconnect, and shutdown, with TTL only as the final recovery mechanism.
- `FUN-013`: Redis unavailability MUST fail closed for distributed concurrency/RPM guarantees unless an explicit single-instance degraded mode is enabled and observable.
- `FUN-014`: Retry classification MUST distinguish requests definitely not sent from requests possibly accepted by an upstream. Ambiguous external POST outcomes MUST NOT be retried by default without an effective idempotency mechanism.
- `FUN-015`: Model upstream timing MUST distinguish connect, response header/first byte, stream idle, and total execution. A first byte after 30 or 60 seconds and a total execution longer than 180 seconds MUST be representable as valid configured behavior.
- `FUN-016`: Request terminalization MUST accept one terminal cause, produce one immutable terminal plan with stable child event IDs, and make every durable or coordination effect independently idempotent and replayable. Cross-PgSQL/Redis completion MUST NOT be described as one atomic exactly-once transaction.
- `FUN-017`: Scheduler queue and lease lifecycles MUST define admission, wait, late-grant prevention, cancellation, heartbeat/fencing, completion, expiry, duplicate completion, Redis restart recovery, and bounded fallback behavior.
- `FUN-018`: Every upstream response path MUST enforce a configured response-byte budget while reading incrementally. `Content-Length` is only an early-rejection hint; compressed, chunked, error, non-stream, and streaming/event-frame accumulation MUST stop at the budget without first materializing the complete response.
- `FUN-019`: Redis scheduler acquire and stale-cleanup work MUST be constant or explicitly batch-bounded per request-path invocation. Remaining cleanup backlog MUST stay visible and continue through bounded supervised work rather than one unbounded Lua execution.

### Configuration And Admin

- `FUN-020`: Each request MUST read one immutable, versioned runtime snapshot and use it for the complete request lifecycle.
- `FUN-021`: Runtime configuration updates MUST use expected-version compare-and-swap or an equivalent conflict-detecting patch contract. A conflict MUST be visible to the Admin caller and MUST NOT silently overwrite data.
- `FUN-022`: Runtime and catalog updates MUST converge across replicas through durable state plus notification and periodic version reconciliation.
- `FUN-023`: Admin key rotation MUST stop acceptance of the old key and start acceptance of the new key on every healthy replica within a documented bounded interval.
- `FUN-024`: Admin DTOs and both maintained frontend clients MUST share a generated or schema-verified contract derived from the Rust API authority.
- `FUN-025`: Routine Admin read, list, snapshot, and export responses MUST mask reusable secrets. Plaintext may be returned only by an explicit one-time creation, rotation, or reveal contract with authorization, audit, expiry or single-consumption semantics, no-store responses, and no later recovery through an ordinary read API.

### Cache And Usage

- `FUN-030`: Raw upstream usage, cache evidence/simulation, downstream reported usage, and internal billable usage MUST remain distinct facts.
- `FUN-031`: Cache simulation MUST NOT be described as a model response cache or as proof of real upstream cache residency.
- `FUN-032`: Payload shaping MUST invalidate stale token/cache calculations and produce a new payload revision.
- `FUN-033`: When a cache policy reduces the reported uncached input basis, a single pure projection function MUST account for the difference in cache-read/cache-creation fields or record an explicit no-cache/pass-through reason. It MUST NOT reduce input while leaving both cache fields zero without such a reason.
- `FUN-034`: Stream and non-stream results for the same usage facts and route policy MUST converge on the same final usage values.
- `FUN-035`: Accepted usage events MUST have stable event IDs, idempotent PgSQL persistence, and replayable derived rollups.
- `FUN-036`: Redis usage event deduplication and all derived increments MUST be atomic, or Redis summaries MUST be rebuilt from the durable event source.

#### Cache Accounting Relation

For one final reported usage record, define:

- `T`: reported total input basis;
- `U`: reported uncached `input_tokens`;
- `R`: `cache_read_input_tokens`;
- `C`: `cache_creation_input_tokens`;
- `C_5m`, `C_1h`: five-minute and one-hour creation breakdown;
- `S`: explicitly suppressed/capped input movement, with a typed reason;
- `A`: explicitly added uplift/simulation amount, with a typed reason.

The projection MUST satisfy:

```text
T = U + R + C

when creation TTL breakdown is complete:
C = C_5m + C_1h

for a policy that reduces uncached input:
U_before - U_after
  = max(0, R_after - R_before)
  + max(0, C_after - C_before)
  + S
```

Additional rules:

- every value is non-negative and arithmetic is checked/saturating at the wire boundary;
- `S` is zero unless a named cap/suppression policy intentionally changes the total basis;
- uplift `A` is recorded separately and produces the explicitly selected new `T`, rather than disappearing into cache fields;
- a local high-cache policy with no creation/read evidence MUST NOT reduce `U` merely to manufacture a target ratio; it keeps the raw/effective basis and records no local cache movement;
- a no-cache policy reports `R = 0`, `C = 0`, and `U = T` after applying its explicit raw/effective basis;
- pass-through preserves normalized upstream fields and marks the projection mode;
- incomplete TTL breakdown records `breakdown_complete=false` and a reason instead of falsely claiming `C = C5 + C1`;
- billable input is a separate policy output and MUST NOT be inferred only from `T` when cache pricing differs.

### Files, Media, And Diagnostics

- `FUN-040`: Files-compatible staging MUST document and enforce single-file, total-byte, live-count, ordering-metadata/tombstone, TTL/eviction, restart, and multi-replica behavior. Explicit delete MUST release payload and indexing metadata within a bounded policy.
- `FUN-041`: Remote source fetching MUST bind the validated DNS/IP result to the actual connection and repeat validation for every redirect.
- `FUN-042`: Request body, remote sources, transformed payloads, decoded media, PDFs, tokenization, and blocking work MUST consume explicit request-level and global resource budgets.
- `FUN-043`: Request-body diagnostic capture MUST be disabled by default.
- `FUN-044`: Explicit diagnostic capture MUST apply field allowlists/redaction, file permission restrictions, root/symlink validation, single-record limits, total bytes, file count, retention age, automatic expiry, and dropped-record metrics.
- `FUN-045`: Kiro/local and explicitly normalized external tool definitions MUST treat missing/blank description and absent/explicit-null input schema according to the accepted profile policy, while raw external passthrough remains byte-identical and performs no tool repair.
- `FUN-046`: A target-specific tool property-name repair MUST be deterministic, collision-free and reversible through streaming/non-streaming `tool_use.input`, update every applicable property-reference keyword, and reject locally when semantic round-trip proof is unavailable. Silent lossy renaming is prohibited.
- `FUN-047`: Public/Admin connection, header, body and structured traversal work MUST acquire the accepted global resource budget before retention/traversal, enforce count/depth/edge/string and slow-read limits with cancellation, and hand downstream only a budget-bound body/artifact. A byte limit alone is not a CPU/object-cardinality limit.

## Correctness Invariants

- `INV-001`: A single request records exactly one runtime configuration version.
- `INV-002`: A request has at most one terminal outcome and one terminal usage event ID.
- `INV-003`: A lease cannot remain counted after its terminal request has been fully released, except within a documented bounded retry/TTL interval.
- `INV-004`: The scheduler never chooses a resource excluded by any mandatory eligibility predicate.
- `INV-005`: Once downstream response headers are committed and `DownstreamCommitment` leaves `Uncommitted`, the request is never retried or rerouted, including when zero body bytes have been sent.
- `INV-006`: Runtime config concurrent writers either serialize successfully or receive an explicit conflict; no field is silently lost.
- `INV-007`: Redis/PgSQL retries cannot apply the same usage or runtime mutation twice.
- `INV-008`: Reported usage fields are non-negative and satisfy the centrally defined cache/input accounting relation.
- `INV-009`: Raw body mode does not execute full parse, Kiro conversion, media materialization, payload guard, or unconditional token counting.
- `INV-010`: No default log or diagnostic path records prompts, tool results, images, tokens, secrets, API keys, cookies, or credentials.
- `INV-011`: A terminal cause is selected once; retries of terminal persistence, lease completion, credential outcome, usage, or projection effects converge by stable terminal/owner idempotency keys, and any unacknowledged residue remains visible and recoverable. Separately accepted audit events converge through `MOD-AUDIT` owner-defined stable event IDs; a request terminal does not implicitly create an audit obligation.
- `INV-012`: For one prepared tool-definition revision, every mapped upstream property name resolves to exactly one original property/path and every returned mapped tool argument is reversed before downstream delivery; an ambiguous or incomplete map never reaches an upstream attempt.

## Quality Attributes

### Compatibility

- `QA-COMP-001`: Existing successful request/response fixtures MUST remain byte- or semantic-equivalent according to each protocol profile.
- `QA-COMP-002`: Real Claude Code CLI validation MUST cover at least three independent sessions of 20 or more turns, including tools, files, search, thinking, MCP, and multi-agent-triggered protocol calls.
- `QA-COMP-003`: Raw and normalized external pools MUST have separate conformance suites.

### Reliability

- `QA-REL-001`: Required background writers MUST expose accepted, committed, retrying, dropped, abandoned, backlog, and oldest-age metrics.
- `QA-REL-002`: Shutdown MUST stop new acceptance, allow in-flight work within policy, join every producer before closing writer ingress, drain required writers, and exit non-zero when required accepted data remains abandoned.
- `QA-REL-003`: All business-spawned tasks SHOULD be owned by a lifecycle supervisor with explicit restart and shutdown policy.
- `QA-REL-004`: Derived state MUST be rebuildable or explicitly classified as disposable.
- `QA-REL-005`: PgSQL backup/restore, Redis key-class rebuild/recovery, previous-binary compatibility with expanded schema, and forward reconciliation MUST have executable isolated drills before the rewrite is complete.

### Performance

- `QA-PERF-001`: Performance is judged by request-path work, contention, I/O, allocation, and recovery, not source-file line count.
- `QA-PERF-002`: Raw passthrough MUST avoid heavy normalized/local stages.
- `QA-PERF-003`: Redis and PgSQL operation counts per request MUST be measured and bounded by scenario rather than growing one network call per credential or pool.
- `QA-PERF-004`: Slow upstreams MUST not create unbounded memory, task, queue, connection, lease, or file growth.
- `QA-PERF-005`: The complete target SHOULD keep throughput within 5%, p95 within 10%, p99 within 15%, and peak RSS within 15% of the same-host legacy baseline unless a superseding correctness decision gives a constant operation budget and replacement capacity proof.
- `QA-PERF-006`: Repeated identical load rounds MUST show no monotonic FD, task, queue, retained-byte, or diagnostic-file growth.
- `QA-PERF-007`: The complete target candidate MUST satisfy both the accepted absolute capacity/outcome SLO and relative legacy-versus-target regression threshold for each canonical workload; a slow baseline alone cannot define “high performance.”
- `QA-PERF-008`: Throughput, success/error populations, latency stages, scheduler/queue work, DB/Redis operations, RSS, FD, tasks, connections, recovery, and cost MUST use explicit metric semantics and accepted workload-specific thresholds. Missing or invalid measurements MUST NOT be encoded as zero or treated as a pass.
- `QA-PERF-009`: Absolute capacity MUST identify the single-replica or multi-replica reference profile, exact replica count/membership, release generation and artifact/config/schema digests, load-balancer/shared-dependency topology, offered load, concurrency, success/error policy, workload/corpus revision, and the highest step that satisfies latency, operation, resource, and recovery gates.
- `QA-PERF-010`: Request-path Redis scripts MUST have constant or accepted batch-bounded time and result cardinality independent of total stale-set size. Load evidence MUST measure per-invocation work, backlog convergence, Redis latency, and unrelated-request progress under large stale populations.

### Resource Safety

- `QA-RES-001`: All queues, caches, files, diagnostic directories, request transformations, remote downloads, and blocking worker pools MUST have hard bounds.
- `QA-RES-002`: Byte-heavy work SHOULD acquire weighted permits before allocation rather than after a large object is already resident.
- `QA-RES-003`: Resource-limit rejection MUST use a stable normalized error and record which budget was exceeded without recording body content.
- `QA-RES-004`: Reusable HTTP clients and client caches MUST bound key cardinality, entries, retained bytes, and age; define eviction and invalidation on endpoint, TLS, credential, or proxy change; and release retained secret-bearing configuration within a documented interval.
- `QA-RES-005`: Every supported production profile MUST configure finite non-zero global admission, in-flight, and queued-request ceilings for local and external scheduling. Zero or unset MUST NOT silently mean unbounded outside an explicitly unsupported development profile.

### Security

- `QA-SEC-001`: Secret-bearing headers and fields MUST use allowlists at external provider boundaries.
- `QA-SEC-002`: Remote URLs MUST be protected against private/link-local/loopback/metadata access and DNS rebinding.
- `QA-SEC-003`: Logs, traces, metrics labels, diagnostic records, exported evidence, and test artifacts MUST be scanned for credential-shaped values before retention or publication.
- `QA-SEC-004`: The single-user assumption MUST NOT be used to waive upstream boundary, SSRF, secret, filesystem, or resource protections.
- `QA-SEC-005`: Ordinary info/debug logs MUST NOT record raw WebSearch queries, MCP request bodies, MCP response bodies, prompts, tool content, or file content. Any explicitly approved sensitive capture MUST use the bounded, expiring, redacted diagnostic path rather than ordinary tracing.
- `QA-SEC-006`: Configurable outbound endpoints MUST apply an accepted scheme, host, port, DNS/IP, redirect, proxy-resolution, and cross-origin credential-forwarding policy before every connection and redirect hop. Validation MUST bind to the connected address and prevent secrets from following an unapproved origin change.
- `QA-SEC-007`: A reusable Admin credential MUST NOT be retained in `localStorage` or another long-lived JavaScript-readable store by either maintained UI. Both UIs MUST use the decision-010/011 bounded HttpOnly session, server-hashed CSRF and fail-closed revocation contract.
- `QA-SEC-008`: Every database-stored reusable credential, proxy, pool and runtime secret MUST use the decision-011 versioned application-level envelope, external recoverable key provider, bounded rotation/rewrap workflow and post-rollback-window plaintext-residue-zero gate.

### Observability

- `QA-OBS-001`: Each request MUST have a request ID and, for failures, a stable error ID.
- `QA-OBS-002`: Stage timing MUST separate queue, config/plan, materialization, payload guard, scheduler, upstream connect/header/first chunk, response translation, and persistence tail.
- `QA-OBS-003`: Metrics MUST include scheduler candidates/lock wait, DB/Redis operations and pool wait, resource permits, writer backlog, config-version lag, RSS, FD, tasks, and diagnostic directory size.
- `QA-OBS-004`: Metrics labels MUST avoid unbounded request IDs, arbitrary model strings, credential labels, URLs, or error bodies.

### Maintainability

- `QA-MAINT-001`: Domain policy MUST NOT depend on Axum, reqwest, sqlx, Redis clients, Admin DTOs, or frontend types.
- `QA-MAINT-002`: Handlers MUST NOT execute SQL, Redis scripts, or mutate scheduler-internal collections directly.
- `QA-MAINT-003`: Storage adapters MUST NOT import HTTP response/dashboard DTOs.
- `QA-MAINT-004`: A module split is accepted only when it establishes ownership, a stable contract, a test boundary, or measurable work reduction.
- `QA-MAINT-005`: Legacy facades MAY exist only in isolated test-only characterization tooling outside release features. Target runtime modules MUST NOT import them, and every old runtime implementation/facade is deleted before the complete candidate freezes and before the one final cutover.

### Testing And Evidence

- `QA-TEST-001`: Both maintained frontend applications MUST have component and browser end-to-end coverage for critical operator workflows, conflict/error states, and accessibility basics.
- `QA-TEST-002`: Before the one final whole-system cutover, the complete post-deletion candidate MUST pass repeatable same-host single-replica and supported multi-replica performance/resource gates with pinned workloads, replica/generation identity, sample policy, thresholds and recovery assertions. Exactly two replicas MUST deliver at least `1.7x` aggregate throughput with p95 no more than `+15%`, p99 no more than `+20%`, and per-launched-request PgSQL/Redis operations no more than `+5%` versus the passing one-replica control; the actual release-generation replica count MUST also pass its manifest-specific absolute gate.
- `QA-TEST-003`: A performance result MUST prove the target process identity, account exactly once for every offered/launched terminal driver outcome, distinguish unavailable measurements from zero, use per-operation timing, and sample through an accepted idle-recovery window. A harness that cannot prove those facts cannot pass a release gate.
- `QA-EVID-001`: Every completion claim MUST identify the exact source revision, commands, configuration identity, result, thresholds, artifact manifest or hashes, sensitive-data scan, and cleanup status.
- `QA-EVID-002`: Active Markdown entrypoints and runbooks MUST resolve, declare their authority/date, and must not present historical output as current proof.

### Deployment And Supply Chain

- `QA-OPS-001`: Liveness, readiness, degraded mode, and request overload MUST have distinct observable meanings, and deployment health checks MUST use application readiness.
- `QA-OPS-002`: Supported restore, Redis rebuild, previous-binary rollback, and forward-reconciliation procedures MUST be executable from versioned runbooks.
- `QA-OPS-003`: Schema migrations MUST be immutable, versioned, checksum-verified, concurrency-safe, and transactional where supported, with restart-safe or explicitly resumable failure semantics and an additive previous-binary compatibility window. Large data backfills MUST run as separate bounded, observable, resumable jobs rather than inline startup migration work.
- `QA-SUP-001`: Release artifacts MUST be traceable to the source revision, lockfiles, toolchain, build command, and immutable artifact digest.
- `QA-SUP-002`: The accepted release profile MUST produce and verify an SBOM, signature, and provenance attestation before publication.

## Workload Envelope

Validation MUST include more than short synthetic success calls:

- concurrency `1`, `16`, `64`, and `128`, plus abrupt bursts;
- partial and widespread 408/429/5xx/network/protocol failures;
- first-byte delays around 3, 10, 30, and 60 seconds;
- total streaming duration beyond 180 seconds with continuing progress;
- clients that read slowly, stop reading, or disconnect;
- slowloris/slow headers, chunked and slow uploads, idle keepalive churn, HTTP/2 concurrent-stream saturation, dishonest/missing body lengths, and proof that health/control capacity remains available;
- long histories, dozens of tools, large tool results, nested schemas, thinking, images, documents, PDFs, and Files references;
- below/at/above-bound message, content, tool and schema node/edge/property/string cardinality with traversal cancellation and weighted resource-cost accounting;
- remote resource servers that are slow, oversized, redirecting, unavailable, or resolve unsafely;
- upstream success and error responses that are oversized, compressed, chunked, slowly streaming, or carry missing or misleading `Content-Length` values;
- configurable external endpoints that resolve to private/metadata addresses, rebind DNS, cross origins through redirects, or attempt to carry credentials across an unapproved hop;
- PgSQL/Redis latency, partial failure, disconnect, restart, and recovery;
- concurrent startup, partial migration failure, restart, previous-binary compatibility, and large backfills separated from schema activation;
- 10, 100, and 1,000 credential candidate sets where feasible in synthetic scheduler tests;
- large stale Redis queue/lease populations that prove per-acquire cleanup is batch-bounded and backlog converges without blocking unrelated work;
- repeated endpoint/proxy/credential rotation that proves reusable client-cache bounds, invalidation, and timely release of retained secret-bearing entries;
- local and external admission/queue saturation under finite production ceilings, including rejection of zero/unset unbounded production defaults;
- local Kiro, external raw, external normalized, direct, preflight, fallback, and local-rescue routes;
- prompt-cache creation, read, eviction, shaping, no-cache, and usage projection transitions;
- Admin secret create/rotate/read/export behavior plus reload and multi-tab browser checks proving neither maintained UI persists a reusable credential in long-lived JavaScript-readable storage;
- process restart and SIGTERM while requests and background writes are active.

The [performance contract and canonical workloads](delivery/performance-contract-and-workloads.md) and decisions 010/011/014 bind workload identity, reference-host and release-generation evidence, metric semantics, absolute outcomes, relative and multi-replica scaling, operation budgets, resource admission, recovery and real-upstream cost controls. Implementation must produce evidence; the values are no longer open choices.

## Non-Requirements

- No tenant isolation, per-user file ownership, or per-key billing is required.
- No microservice extraction is required.
- No generic plugin ABI is required before multiple independent implementations need it.
- No unlimited retry, queue, stream duration, or artifact retention is acceptable as an availability strategy.
- The modernization is not complete merely because a large file was split or line count decreased.
