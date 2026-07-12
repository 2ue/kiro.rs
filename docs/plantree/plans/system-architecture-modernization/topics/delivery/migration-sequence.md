# Final Complete Implementation Plan

Role: Canonical end-to-end implementation specification for the complete system rewrite

Status: Accepted and implementation-ready; production implementation Not Started

Authority: Defines the one-program dependency order, target-only integration model, complete deliverables, legacy removal, final candidate, one system cutover and whole-system rollback

As of: `v0.0.102` / `e9479df` / plan updated 2026-07-12

Read when: Starting or resuming modernization implementation, integrating any target module, checking final-scope coverage, preparing the complete release candidate, or executing final cutover/rollback

Related: [Decision 009](../../decisions/009-single-program-modular-build-and-final-cutover.md), [Decision 010](../../decisions/010-fixed-operational-and-acceptance-policies.md), [Decision 011](../../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md), [Decision 012](../../decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md), [Decision 013](../../decisions/013-owner-transaction-audit-acceptance.md), [Decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md), [Module work map](../../indexes/execution-slice-map.md), [Target modules](../../indexes/target-module-ledger.md), [Rewrite inventory](../../indexes/rewrite-inventory.md), [Entry/completion contract](next-package-brief.md), [Migration subsystem](migration-foundation-brief.md), [Performance](performance-contract-and-workloads.md), [Verification/final cutover](verification-rollout-and-rollback.md), [Roadmap](../../roadmap.md)

## Final Objective

Rewrite the complete first-party Rust core, both maintained Admin frontends, schema/state lifecycle, validation tooling and release tooling into one domain-oriented modular monolith with 50 explicit technical authority modules.

The result must preserve characterized Anthropic, Claude Code, Kiro, external-pool, Files, Models, count-token and Admin behavior except for explicit accepted safety corrections. It must eliminate broad mutable state, God Objects, hidden I/O, unbounded work, duplicate policy, request-lifecycle ambiguity, startup mutation drift, handwritten frontend contract drift, unreliable evidence and obsolete legacy code.

Success is the complete final system, not smaller files, moved functions, a partial module set, one benchmark, or a planning-document count.

The current planning registry routes 47 verified findings and 16 candidate records through 100 binding requirement/invariant/quality clauses to all 50 modules and 16 complete-system gates. Decisions 001 and 003-014 are Accepted; decision 002 is Superseded. This coverage makes the plan implementation-ready but is not source, test, performance, migration, or release evidence.

## Delivery Model

This is one modernization, not a sequence of separately released refactors.

```text
freeze and characterize the legacy baseline
-> implement accepted constraints and final harnesses
-> build target modules by dependency
-> continuously integrate modules into one target-only candidate
-> delete superseded source as each responsibility is replaced
-> prove global zero legacy/stub/duplicate authority
-> run the complete candidate gate set and dress rehearsal
-> activate the whole target system once
-> roll back the whole system if required
-> after the full-system observation and whole-release rollback window close, contract compatibility-only state
-> rerun full post-contract gates and close the one modernization
```

The legacy production version remains the only production authority until final cutover. No target module is independently canaried, made default, soaked, or rolled back in production. The release binary contains no per-module old/new selector or fallback.

`R0`-`R10` are dependency groups and stable traceability identifiers only. They do not create phase approvals, staffing gates, calendar milestones, or partial product completion.

## Fixed Product And Architecture Constraints

- One operator and one trust domain; no users, tenants, tenant repositories, tenant authorization, billing or partitioning.
- One deployable modular-monolith backend and two maintained frontend artifacts; no microservice split.
- 50 `MOD-*` technical authorities; no horizontal application/ports/adapters/workers God layer.
- One composition root; only bootstrap sees concrete construction, and bootstrap contains no business or migration logic.
- One raw runtime configuration authority and one capture per authenticated request; downstream receives only narrow immutable views of the same version.
- Pure domain decisions contain no Axum, reqwest, sqlx, Redis client, filesystem, frontend DTO or broad lock dependency.
- PgSQL owns durable truth; Redis owns bounded coordination/derived state; process memory/filesystem state has explicit loss, TTL, quota and rebuild semantics.
- Every queue, cache, body, file, remote fetch, task, permit, retry, diagnostic path, Redis script and blocking workload is bounded.
- Ambiguous upstream POST delivery is not retried; downstream header commitment prohibits another upstream.
- One terminal decision, stable obligation IDs, durable PgSQL acceptance, module-specific idempotency and no false cross-system exactly-once claim.
- Producer-aware shutdown joins producers before writer ingress closes, drains/checkpoints required work before dependencies close, and reports critical residue with non-zero exit.
- Domain modules own immutable migration definitions; `MOD-MIGRATIONS` owns only common runner/ledger mechanics; `MOD-RECOVERY` owns only supported-profile backup/restore/rebuild/forward recovery.
- Rust is the Admin schema authority; both frontend clients/types are generated and both applications are fully rewritten.
- Absolute capacity/SLO and same-host relative performance both pass, with resource/operation/recovery proof; exactly two equivalent replicas scale aggregate throughput to at least `1.7x` with p95/p99 no worse than `+15%`/`+20%` and per-launched-request PgSQL/Redis operations no worse than `+5%`, while the actual release replica topology passes its own manifest gate.
- Target runtime source has no legacy implementation import, selector, hidden fallback, release fake/stub or duplicate authority.

## Implementation Mechanics

Each exact work unit follows the [entry/completion contract](next-package-brief.md). The first action maps current symbols at the pinned revision; it is not a reason to reopen the plan or request a human assignee. The final public contract and technical authority are already fixed.

AI implementation may proceed sequentially or in parallel where public contracts and state do not overlap. A change is integrated only when:

1. its accepted target contract and resource/error/lifecycle behavior are implemented;
2. focused tests and architecture checks pass;
3. it connects only to target public contracts in the target-only candidate;
4. affected aggregate gates pass;
5. the superseded legacy responsibility is deleted;
6. post-deletion import/residue and focused/aggregate gates pass;
7. versioned evidence records the exact revision and result.

If a newly discovered fact contradicts an accepted contract, record a candidate and update the decision before silently widening an interface, adding a fallback or changing a safety default. The problem catalog remains open-ended throughout implementation.

## Isolated Target Candidate

The candidate uses dedicated PgSQL, Redis, Files/diagnostic roots, ports, browser state, CLI HOME, artifacts and process limits. External network is deny-by-default. Deterministic fake Kiro, external-pool, media, DNS/redirect and fault endpoints drive normal tests.

Legacy and target comparisons use two independent state clones from one sanitized baseline and execute sequentially. Pure decisions may consume the same immutable fact bundle. No comparison sends the same logical upstream request, acquires the same lease, mutates the same File/Admin/resource or writes the same durable state twice.

Real Kiro and Claude Code validation is bounded, explicit and independent. It proves protocol/client behavior, not deterministic model output or general capacity.

## Dependency Group R0: Constraints, Fixtures, Final Harnesses

Implement the final architecture, contract and load/chaos harnesses once. Create reusable diagnostics, media, Files, Kiro response/client, external egress/response and local/external scheduler fixture corpora.

R0 does not patch the legacy production path and does not create temporary containment wrappers. Later module work consumes these final fixtures and accepted decision-010/011 limits. R0 must implement correct target PID identity, exact result accounting, typed invalid/missing metrics, per-operation timing, warmup/measurement/cooldown, watchdog, artifact-budget and cleanup semantics in the final load harness. No modernization harness implementation or passing evidence currently exists; R1/R9 add the accepted evaluator, manifests, orchestration and evidence to that one implementation.

Completion output:

- `MOD-ARCH-FITNESS`, `MOD-CONTRACT-HARNESS` and `MOD-LOAD-CHAOS-HARNESS` final implementations;
- sanitized invariant/corpus manifests with hashes and no business secrets;
- deterministic fake dependencies and fault controls;
- safety fixtures for every formerly proposed R0 containment concern;
- no claim that product behavior or performance is fixed merely because harnesses exist.

## Dependency Group R1: Kernel And Cross-Cutting Runtime Foundations

Implement `MOD-KERNEL`, `MOD-RUNTIME-CONFIG` capture/views, `MOD-PROTO-ANTHROPIC`, `MOD-OBSERVABILITY`, `MOD-DIAGNOSTICS`, `MOD-SECRET-ENVELOPE` and `MOD-RESOURCE-GOVERNOR`.

The raw complete snapshot remains private to runtime-config. Authenticated public entry captures once; `CapturedRuntime` contains typed routing, scheduler, processing, usage and resource views with one version. No downstream provider reload is permitted.

Diagnostics is implemented once as the final bounded owner: default off, explicit opt-in, redaction/allowlist, safe root/symlink handling, restrictive permissions, bounded queue/record/directory/file/age quotas, restart cleanup and observable drop/disable behavior. Ordinary tracing contains no raw WebSearch/MCP bodies, keys, credentials, prompts or tools.

`MOD-SECRET-ENVELOPE` exclusively owns versioned AEAD/key-provider/envelope/rewrap mechanics; domains own secret lifecycle and never receive master-key authority. `MOD-RESOURCE-GOVERNOR` exclusively composes weighted process-wide admission for request bytes, media/materialization bytes, blocking work, tasks, connections and queues while domain schedulers retain fairness and lease policy. Accepted ceilings fail closed and cannot be silently raised or disabled.

Add final reference-host workload manifests and the accepted threshold evaluator to the R0 load harness. Missing measurements fail closed.

## Dependency Group R2: State, Migrations, Config, Auth, Catalog, Journal

Implement `MOD-MIGRATIONS` and every exact domain manifest/repository/Redis class listed in the work map. Do not create a generic store or migration God module.

Required outputs include:

- immutable manifests, deterministic dependency plan, fencing, active/applied/adopted/checkpoint ledger and legacy adoption map;
- runtime-config typed CAS and generation convergence;
- auth epoch, verifier-only request/Admin keys, secret-envelope-backed replayable secrets, rotation/revocation and fail-closed stale mutation policy;
- model catalog alias/capability/pricing authority and validated publication;
- encrypted credential/proxy resources, external-pool durable state, usage/audit/job repositories;
- shared PgSQL Files object store with bounded streaming/checksum/TTL semantics;
- terminal journal/outbox with stable IDs and replay;
- versioned bounded Redis classes for invalidation, schedulers, usage projection and prompt-cache evidence.

Each secret-owning domain adopts legacy plaintext through immutable manifests and bounded resumable jobs, records envelope/key versions without exposing plaintext, supports decision-011 rewrap and independent key recovery, and carries only the explicitly frozen previous-release compatibility projection through the whole-system rollback window. R10 contraction deletes that projection and proves reusable plaintext residue is zero.

Fresh, legacy, partial, corrupt, concurrent and previous-binary migration paths all pass before downstream stateful modules integrate.

## Dependency Group R3: Usage And Prompt Cache

Implement `MOD-USAGE` and `MOD-PROMPT-CACHE` once against the accepted R2 authorities.

Keep actual upstream usage, estimated usage, downstream reported usage, accounting usage and cache evidence distinct. Stream/non-stream equivalent facts produce equivalent projection. Durable usage events are idempotent and batched; dashboard Redis state rebuilds from PgSQL without marker gaps.

Prompt-cache shared Redis evidence has TTL/capacity/versioned atomic transitions. Actual upstream facts, local estimates and simulation are labeled separately. Replica-local approximation never becomes authoritative accounting.

## Dependency Group R4: Schedulers, Credentials, Proxy Resources, Pools

Implement final local/external scheduler lifecycles, credential refresh/outcome authority, proxy-resource lifecycle/binding publication and external-pool lifecycle.

Pure selection is separate from Redis/PgSQL/refresh/HTTP. FIFO applies within priority/eligibility classes; admission and queues are finite; cancel/grant is atomic; leases are fenced, heartbeat only while active, complete/cancel idempotently and use TTL only for crash recovery. Redis loss uses an epoch/recovery barrier and never fails over to unsafe process-local scheduling.

One complete scheduler path is integrated into the target candidate. Offline pure parity performs no lease or refresh. There is no dual production scheduler namespace or cohort.

## Dependency Group R5: Kiro/External Upstreams And Attempt Policy

Implement `MOD-PROTO-KIRO`, `MOD-PROTO-EXTERNAL`, `MOD-KIRO-UPSTREAM`, `MOD-EXTERNAL-UPSTREAM` and `MOD-ATTEMPT-POLICY`.

Each adapter owns one complete real attempt and returns bounded transport facts. Kiro owns endpoint/TLS/proxy/auth/client-cache/response lifecycle; external owns safe destination/DNS/redirect/path/header/credential/response behavior. Success/error/stream bytes are enforced incrementally; error retention is bounded; cache labels contain no secrets.

Retry policy receives explicit request-delivery, upstream execution possibility, response and downstream commitment facts. Unknown delivery is not replay-safe. No HTTP status or library error alone authorizes duplicate execution. Tests use deterministic fakes; real Kiro operations are separate bounded requests.

## Dependency Group R6: Request Planning, Payload, Files, Media, Endpoints

Implement `MOD-MESSAGES` planning/orchestration, `MOD-REQUEST-ARTIFACTS`, `MOD-PAYLOAD`, `MOD-FILES`, `MOD-MEDIA`, `MOD-TOKEN-COUNT`, Models use case and thin non-Messages public routes.

Route/target intent is selected before expensive work. Request facts are lazy, revisioned and bounded; each payload revision is parsed/count/serialized only as required. External raw executes zero forbidden heavy stages. Local Kiro and external normalized behavior retain established body-capability contracts.

Files is shared and replica-safe. Remote media validates and binds every connection/redirect to the accepted egress policy before bytes are consumed. PDF/tokenizer/blocking work uses owned executors/permits with cancellation and resource recovery.

Tool-definition handling follows decision 012: raw external profiles remain byte-preserving; normalized profiles apply deterministic description/schema behavior and only collision-free reversible property-key maps. Every schema reference and returned stream/non-stream tool input round-trips, or the request fails locally before upstream execution.

## Dependency Group R7: SSE, Response, Terminal, Messages Transport

Implement `MOD-PROTO-SSE`, `MOD-RESPONSE`, `MOD-TERMINAL-LIFECYCLE` and the final thin Messages transport.

Response owns response-session state, headers, commitment, SSE ordering, backpressure and wire emission. Terminal lifecycle owns one neutral terminal decision and stable child IDs. Scheduler, credential and usage modules retain private state and project typed obligations; response and terminal never persist another authority's effect.

Headers handed to transport are irreversible commitment even before body bytes. Slow/downstream-disconnect paths remain bounded. Required terminal acceptance, lease release/reconciliation, usage/credential idempotency and partial failure converge under decisions 003-006 and 010.

All exact response profiles and routes integrate together into the target candidate; they are not separately activated.

## Dependency Group R8: Admin Backend, Contract, Browser, Both Frontends

Move nine Admin domains to their technical authorities: runtime config, auth, model catalog, credentials, proxy resources, external pools, usage, audit and maintenance jobs. System/version is a thin state-free query; readiness remains `MOD-READINESS`.

Generate the TypeScript client/types from the Rust schema. Routine reads mask secrets; create/rotate uses reveal-once; keep/replace/clear semantics are explicit. Neither frontend persists reusable Admin credentials or revealed values in browser-accessible durable storage.

Rewrite all nine domain workflows plus validation and overview/system in both `admin-ui` and `operator-ui`. Each has loading, empty, stale/conflict, retry, partial failure, destructive confirmation, accessibility, responsive and browser tests. The browser harness is final and isolated. Both complete frontend artifacts enter the one target release.

Every accepted Admin domain mutation appends its audit fact inside the same owning PgSQL transaction through the sealed narrow capability defined by decision 013. The audit module owns schema/query/export policy but cannot create a later best-effort substitute for a missing transaction-local append.

## Dependency Group R9: Lifecycle, Recovery, Real Clients, Release

Implement `MOD-SUPERVISOR`, `MOD-READINESS`, `MOD-TRANSPORT-HEALTH`, `MOD-BOOTSTRAP`, `MOD-RECOVERY`, `MOD-REAL-CLIENT-HARNESS`, and `MOD-RELEASE-HARNESS`; integrate final manifests into the existing contract/load/browser harnesses.

Shutdown order is runtime behavior, not a modernization phase:

```text
close admissions
-> drain or cancel producers
-> join producer barriers
-> close writer ingress
-> drain/checkpoint durable consumers
-> reconcile derived and lease work
-> close PgSQL/Redis/HTTP dependencies
-> report residue and exit
```

Readiness reflects dependencies, migration, auth epoch, queues, writer backlogs, scheduler epoch, workers and critical residue rather than TCP availability.

`MOD-RECOVERY` validates decisions 010 and 014 RPO/RTO through backup/restore, Files checksums, Redis rebuild, previous-binary and forward-reconciliation drills. It calls `MOD-MIGRATIONS` and domain recovery contracts but owns neither their state nor migration runner.

`MOD-RELEASE-HARNESS` produces the signed release-generation manifest with the expected replica membership, backend/frontend/image digests, configuration/schema/auth/catalog versions and evidence identities. `MOD-READINESS` keeps public traffic closed until every expected member satisfies that generation, including after Redis loss or member replacement. The release artifact also fixes the single production migration window and the per-authority previous-release rollback compatibility matrix required by decision 014.

Run real Claude Code 3x20-turn sessions, bounded independent real Kiro checks, both-app browser coverage, Docker/image consumer checks, SBOM/signature/provenance and clean artifact/secret scans.

## Dependency Group R10: Complete Candidate And Final Activation

R10 does not postpone module replacement. It proves every module already deleted its superseded legacy responsibility and no global residue remains.

Required closure:

- all 50 modules `Verified In Candidate`;
- no legacy runtime imports, selectors, fallbacks, duplicate DTO/policy/store/worker, obsolete UI or old harness path;
- no release fake, stub, TODO, `unimplemented!()`, panic placeholder or permissive/unlimited default;
- all rewrite-inventory rows mapped and deleted/replaced/reused only under an explicit accepted treatment;
- one frozen target backend/frontend/image/release digest;
- one signed release-generation manifest with exact expected replica membership and satisfied private-smoke/public-readiness barrier;
- one rehearsed production migration window plus a complete per-authority previous-release rollback compatibility matrix;
- full post-deletion static/Rust/storage/protocol/frontend/load/chaos/recovery/real-client/Docker/supply-chain gates;
- isolated 60-minute and 100,000-request stability run, restart/dependency-fault cycles and idle recovery;
- exact final cutover and whole-system rollback dress rehearsal.

Then execute the one final cutover under the verification runbook. If any rollback trigger fires, roll back the whole release, not individual modules. After the 24-hour full-system observation passes, contract compatibility-only schema/Redis/projection state and rerun all post-contract gates.

## Data Rules

- Expand/adopt/backfill is complete and rehearsed before traffic; destructive contract waits until whole-system rollback closes.
- Production executes the complete migration/adoption plan in the single decision-014 window; no dependency group receives its own production schema switch.
- One target authority writes each state. No independent old/new dual write is allowed.
- Previous binary compatibility is proven against additive schema and target ledger facts; its old runner is disabled/isolated.
- PgSQL/Redis/filesystem authority and rebuild/loss policy are explicit in every module.
- Durable outbox/job/terminal/usage/audit identities are stable and replayable; derived state rebuilds.
- Backup/restore proof includes application invariants, not only a database command success.

## Code And Change Discipline

- Keep commits/work units bounded by technical authority even though the final product is one release.
- Do not mix unrelated user work or normalize the dirty worktree destructively.
- Add no new behavior to legacy God Objects except a separately authorized incident hotfix; such a hotfix does not become target architecture.
- Every target change updates affected plan mappings before a discovery silently changes contracts.
- Do not create `implementation-status.md` until source implementation actually starts. When it starts, track the complete modernization with at most five active implementation items, not pseudo-production phases.
- Do not mark a module or finding complete from planning text. Only exact source and versioned evidence can advance implementation state.

## Completion Definition

The modernization is complete only after the complete target system is production-authoritative, the full-system observation and whole-release rollback window pass, compatibility-only state is contracted, all legacy implementation/artifacts are deleted or intentionally archived with provenance, and the final post-contract evidence set passes.

No personnel assignment, project estimate, intermediate dependency group, module test, local benchmark, file-count reduction or documentation checkpoint substitutes for that result.
