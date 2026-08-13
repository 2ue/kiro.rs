# 010: Fixed Operational And Acceptance Policies

Role: Architecture and release policy decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding conservative defaults and parameter rules for implementation, supported deployment, performance, recovery, and final acceptance

Scope: Resolves `Q-001` through `Q-013`; applies to all target modules and the final complete release

Affected requirements/findings: `HA-001`, `COR-*`, `SEC-*`, `RES-*`, `PERF-*`, `OPS-*`, `QA-PERF-*`, `QA-RES-*`, `QA-OPS-*`, `QA-SEC-*`, and every work package previously blocked by an open policy question

Decision source: Final-plan convergence on 2026-07-12 under the operator instruction to produce one complete executable plan without personnel or calendar prerequisites. Conservative behavior is selected where current evidence cannot justify permissive behavior.

Related: [Decision 009](009-single-program-modular-build-and-final-cutover.md), [Decision 011](011-explicit-secret-envelope-and-resource-governor-authorities.md), [Decision 014](014-release-generation-recovery-and-rollback-state.md), [Requirements](../topics/requirements-and-quality-attributes.md), [Performance contract](../topics/delivery/performance-contract-and-workloads.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Open-question registry](../open-questions.md)

## Decision Rule

The values below are initial supported-production limits and release minima. Implementation may automatically choose a lower, safer value when the detected process/container budget cannot support the initial ceiling. It may not silently select an unlimited value or loosen a security, replay, durability, recovery, or evidence rule merely to make a test pass.

An implementation measurement may specialize a value only through the documented formula or workload manifest. Any incompatible increase, reduced gate, or accepted data loss requires a later explicit decision with evidence.

## `Q-001`: Supported Replica Mode

Multi-replica operation is supported for one operator and one trust domain. PgSQL is durable authority; Redis coordinates leases, queues, invalidation hints, shared prompt-cache evidence, and rebuildable projections. A production profile must remain correct under concurrent replicas, restart, missed invalidation, and one-replica shutdown.

No tenant, per-user partition, or tenant authorization model is introduced.

## `Q-002`: Terminal, Usage, And Audit Durability

- The minimal terminal envelope and required typed owner obligations commit atomically to PgSQL before a clean downstream terminal completion is acknowledged.
- Scheduler lease completion uses an immediate idempotent Redis path and supervised reconciliation; it is not claimed atomic with PgSQL.
- Usage and credential obligations are delivered at least once from the durable outbox and consumed idempotently by stable ID.
- Redis/dashboard projections are derived and never delay a clean client terminal once the durable authority is accepted.
- Every successful Admin mutation invokes the sealed audit append in the same PgSQL transaction and commits any separately required domain outbox/job record there as an additional write.
- If downstream output is already committed but terminal persistence cannot be accepted, the process records critical residue, becomes unready, retries by stable ID, and cannot report a clean shutdown while residue remains.

## `Q-003`: Files Across Replicas

The supported profile uses a shared `FileObjectStore`. The initial implementation stores bounded file metadata and payload in PgSQL under the `MOD-FILES` authority, with streaming reads/writes, content length and checksum validation, no full-payload process cache, and the decision-011 fixed ceiling: 50 MiB/file, 128 live objects, 256 MiB total, 7-day idle TTL, 30-day absolute age and 5-minute metadata/tombstone cleanup after delete. Quota overflow rejects rather than silently evicting a live object. Redis may hold only bounded derived lookup/affinity hints.

Process-local Files storage is development-only and must report that it is not a supported multi-replica profile. Sticky routing is an optimization, not correctness authority. Restart, failover, and any replica must preserve upload/list/get/delete/materialize semantics for accepted files.

## `Q-004`: Initial Resource Limits

| Resource | Binding initial policy |
| --- | --- |
| JSON nesting depth | 192 |
| Remote sources per request | 8 |
| Remote bytes per source | 20 MiB |
| Aggregate downloaded bytes per request | 32 MiB |
| Aggregate transformed/materialized bytes per request | 64 MiB |
| Complete non-stream upstream success body | 32 MiB; incremental enforcement before allocation exceeds the limit |
| Streamed upstream response | 64 MiB total unless the protocol profile has a lower characterized limit; never fully buffered |
| Retained upstream error detail | 64 KiB sanitized prefix; the remainder is drained or connection-closed without retention |
| Diagnostic capture | Off by default; 256 MiB directory, 1,024 files, 24-hour age, and 4 MiB per record maxima when explicitly enabled |
| Kiro reusable-client cache | 256 entries, 10-minute idle retirement, active-reference protection, bounded concurrent construction |
| Redis stale cleanup | At most 128 members per request-path invocation; remaining backlog is scheduled and observable |
| Production admission | Finite; `0 = unlimited` is rejected in a supported profile |

Decision 011 replaces a constant permit divisor with the accepted weighted `MOD-RESOURCE-GOVERNOR` formula, exact server/pool/queue/cardinality ceilings and a single global byte ledger. A trustworthy memory limit with more than the reserve plus minimum 64-MiB working set is required; otherwise the production profile is invalid and readiness stays closed. Development may explicitly use a bounded 4-admission/8-queue profile but cannot report it as supported production. Overload rejects before body retention/avoidable parsing, media, tokenization, upstream connection or queue retention and returns a stable normalized error with `Retry-After` where applicable.

## `Q-005`: Performance And Regression Gate

`REF-HOST-PRIMARY` is a single-replica release-runner profile with at least 8 logical CPUs, 16 GiB RAM, an explicit process/container memory limit, local isolated PgSQL/Redis, release builds, fixed dependency versions, and a validity manifest. A different host may be supported, but it does not replace this comparison profile without a new manifest revision.

The release minimum on deterministic fake-upstream workloads is:

| Workload outcome | Required minimum |
| --- | --- |
| Ordinary normalized Messages at concurrency 64 and 10 ms fake-upstream delay | At least 100 successful requests/second, at least 99.9% success, 0 unexpected failures |
| Raw passthrough at concurrency 64 and 10 ms fake-upstream delay | At least 200 successful requests/second, at least 99.9% success, and zero heavy-stage executions |
| Proxy-local ordinary request overhead | p95 <= 25 ms, p99 <= 75 ms |
| Pure scheduler with 1,000 candidates | p95 <= 2 ms, p99 <= 5 ms |
| Redis scheduler script duration on the reference profile | p95 <= 5 ms, p99 <= 10 ms, with at most 128 stale members processed |
| Required terminal PgSQL acknowledgement on the reference profile | p95 <= 50 ms, p99 <= 150 ms |
| Burst overload decision after the finite ceiling is reached | p95 <= 100 ms without queue growth beyond the configured bound |
| Ordinary canonical workload peak proxy RSS | <= 1 GiB and within the relative-regression limit |
| Idle recovery after workload/fault cleanup | Within 60 seconds: RSS <= max(baseline + 64 MiB, baseline * 1.10), FD/tasks/connections <= baseline + 5, no queue/lease/file/backlog residue outside its durable policy |

Relative comparison uses at least five alternating legacy/target measured rounds. The target may not regress success throughput by more than 5%, selected successful-response p95 by more than 10%, p99 by more than 15%, or peak RSS by more than 15%. DB/Redis operation counts may not increase unless an accepted correctness decision gives an explicit constant budget and the absolute gate still passes.

`REF-HOST-MULTI` uses at least two equivalent target replicas, one shared isolated PgSQL/Redis pair and the exact decision-014 generation manifest. At twice the single-replica offered load, aggregate ordinary normalized/raw success capacity must be at least 1.7 times the passing single-replica capacity, success remains at least 99.9% with zero unexpected failures, p95/p99 regress no more than 15%/20%, per-launched-request PgSQL/Redis operations regress no more than 5%, and no shared lease/admission/resource ceiling is oversold. Removing/partitioning one replica produces bounded overload/fail-closed behavior and recovery without duplicate work. The actual production replica count also runs a manifest-specific capacity check before cutover.

Ordinary percentile gates require at least 10,000 successful samples; slow/fault/profile-specific tail gates require at least 1,000 classified samples unless the finite corpus is smaller and every case is executed. Missing metrics, wrong process identity, incomplete task accounting, skipped cases, or an invalid percentile population fail closed.

Real Kiro remains low volume: at most 20 requests in one validation run unless a separate hard account quota is lower. Real Claude Code uses three isolated sessions with at least 20 turns each and a predeclared request/token/duration cap. Unknown monetary cost is never treated as zero.

## `Q-006`: Observation And Soak

There is no per-module production soak. Module work packages use deterministic focused and integration evidence only.

Before final cutover, the complete target candidate must pass an isolated soak lasting at least 60 minutes and 100,000 completed fake-upstream requests, whichever condition finishes later, including proxy/client churn, queue saturation, dependency fault/recovery, three process restart cycles, one Redis restart/rebuild, and one PgSQL connection-loss/recovery cycle.

After final cutover, the complete target system has one 24-hour rollback observation window. This is a release correctness gate, not a project schedule or a phased modernization release. Compatibility-only schema/state is contracted only after this window passes and all durable backlogs/residue are reconciled.

## `Q-007`: Prompt-Cache Authority

Prompt-cache evidence shared across replicas is stored under `MOD-PROMPT-CACHE` in bounded Redis state with a versioned key schema, atomic transitions, restart/rebuild semantics and the decision-011 ceiling: 32,768 records, 2 KiB/record, 64 MiB aggregate and 2-hour TTL. Overflow or Redis loss degrades to unknown/no evidence and never fabricates an upstream fact. Durable usage facts record whether cache data is actual upstream fact, locally estimated, or simulated.

Replica-local heuristics may assist request-local decisions but are never presented as authoritative upstream cache creation/read facts and never drive financial-grade accounting.

## `Q-008`: Replay Safety

An upstream attempt is replay-safe only when no request bytes were transmitted, a target-specific response proves rejection before execution, or the target demonstrably deduplicates the same logical operation with a stable idempotency key whose scope and retention cover the retry window.

Ambiguous Kiro or external POST delivery is not retried. HTTP status, timeout, reset, or client-library error names alone do not prove replay safety. Once downstream headers are committed, no alternate upstream is selected for that request.

## `Q-009`: Queue Fairness And Lease Timing

- FIFO applies within the same priority and eligibility class; existing priority, sticky binding, cooldown, and pool-capability semantics are preserved ahead of FIFO.
- Queue cancellation and grant are one atomic terminal transition; a cancelled waiter cannot consume a late grant.
- Queue wait is bounded by the smaller of the downstream remaining deadline and 30 seconds.
- Active leases heartbeat every 15 seconds, expire after 60 seconds without a valid heartbeat, and have a 30-minute default absolute lifetime.
- A request profile may declare a longer absolute lifetime up to 2 hours only for a progressing stream with valid heartbeat and downstream liveness; it is never unlimited.
- Completion/cancel is idempotent and fenced. TTL is crash recovery only, not the normal release path.
- Redis epoch/recovery barriers fail closed after Redis state loss until active-capacity safety is reconciled.

## `Q-010`: Shutdown Deadlines And Residue

Shutdown ordering follows decision 006. The binding outer limit is 120 seconds with separate ceilings: 30 seconds request grace, 10 seconds forced producer cancellation, 30 seconds required durable-writer drain, 15 seconds durable outbox/job checkpoint, 15 seconds scheduler release reconciliation, and the remaining time for dependency close and final report.

Terminal, required usage, Admin audit, committed runtime mutation, credential outcome, and unreconciled lease residue are critical and require non-zero exit. A committed outbox for a derived/rebuildable projection may remain only when its count/bytes/oldest-age checkpoint is recorded and oldest age is at most 5 minutes. Best-effort diagnostic drops are allowed only within bounded counters and never include required durable records.

## `Q-011`: Admin-Key Revocation

PgSQL stores the auth epoch/version. Every immutable auth view carries it; Redis publishes a fast invalidation hint. Replicas poll the durable epoch at least every 5 seconds. Before an Admin mutation, a snapshot older than 2 seconds must revalidate the current epoch at low volume.

A replica that cannot prove a sufficiently current epoch rejects Admin mutations and becomes degraded/unready for the Admin mutation profile. Routine reads return masked values; secret creation/rotation uses reveal-once responses.

Browser login exchanges the reusable Admin key over a same-origin no-store request for a random host-only `HttpOnly`, `Secure`, `SameSite=Strict` session cookie. `MOD-AUTH` stores only a session-token hash plus auth epoch in bounded shared Redis state: 1,024 sessions globally, 256 per accepted origin/auth epoch, 15-minute idle and 8-hour absolute lifetime. Mutations require an in-memory random 256-bit CSRF token whose hash is server-side; at most four 30-minute tokens exist per session. A same-origin `Origin`/Fetch-Metadata-validated mint endpoint supports reload/new tabs, and sensitive auth/reveal actions rotate the calling token. Logout, key revocation and Redis loss invalidate sessions fail-closed. Loopback development may relax `Secure` only in an explicit non-production profile. Zero/unset session or token ceilings are invalid.

Neither maintained frontend stores a reusable Admin key, session token, CSRF token or revealed secret in `localStorage`, `sessionStorage`, IndexedDB, service-worker cache, persisted query cache, logs, URLs, analytics or generated artifacts. Existing durable browser keys are deleted rather than migrated to another JavaScript-readable store.

## `Q-012`: Recovery Objectives And Retention

The supported production profile provides:

| State | RPO | RTO |
| --- | ---: | ---: |
| Runtime config/auth, credentials, proxy resources, external pools, model catalog, Files, terminal/outbox, usage/audit/jobs | <= 5 minutes | <= 60 minutes |
| Redis coordination and derived state | No independent RPO; authority is reconstructed or leases expire safely | <= 30 minutes to safe admission and required rebuild |
| Process-local caches and diagnostics | No durability promise | Cleared/recreated at restart; never block authoritative recovery |

Use encrypted daily full backups plus continuous WAL archiving, at least 14 days retention, an off-host copy, integrity verification, and a monthly isolated restore drill. Recovery evidence includes exact backup/WAL identity, schema and migration ledger, restore ordering, auth/catalog/runtime generations, outbox replay, Redis rebuild/epoch barrier, Files checksums, forward reconciliation, cleanup, and achieved RPO/RTO.

A successful backup command without a restore and application-level verification is not evidence.

## `Q-013`: Production Hardening

Production hardening is part of this modernization's final release, not deferred. The published profile requires non-root execution, dropped Linux capabilities, no-new-privileges, reviewed writable paths, read-only root filesystem where compatible, bounded tmp/runtime volumes, Admin network exposure guidance, and secret-safe image/build provenance.

TLS may terminate at a documented trusted reverse proxy. Direct public Admin exposure without TLS is unsupported. Database-stored reusable credential, proxy, pool and runtime secrets use decision 011's application-level envelope and independently recoverable operator key provider; verify-only request/Admin keys use its keyed verifier and are not recoverably stored. Key rotation, rewrap, off-host key recovery, backup/restore and post-window plaintext contraction are required workflows.

## Consequences

These policies favor correctness, explicit failure, and bounded resource use over permissive compatibility in ambiguous situations. Some existing workloads may receive a clear limit or overload error where the old implementation attempted unbounded work. That is an intentional safety correction and must be represented in compatibility fixtures and release notes.

The policies eliminate `Q-001` through `Q-013` as planning blockers. Implementation still has to produce the specified source audit, manifests, measurements, migrations, tests, and evidence; a fixed policy is not proof that the code satisfies it.
