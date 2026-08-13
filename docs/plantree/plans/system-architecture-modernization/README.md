# System Architecture Modernization

Role: Historical complete implementation specification for the superseded Rust rewrite target

Status: Superseded on 2026-07-13; production implementation never started; archive/reference only

Authority: Historical Rust target and verified-analysis source only; the Greenfield AI Gateway plan owns current target architecture and implementation

As of: `v0.0.102` / `e9479df` / superseded 2026-07-13

Read when: Retrieving historical Rust target reasoning, verified findings, current-system evidence, or semantic invariants explicitly inherited by the greenfield plan

Related: [Greenfield AI Gateway](../greenfield-ai-gateway/README.md), [supersession record](history/superseded-by-greenfield-ai-gateway-2026-07-13.md), [Plan Tree](../../README.md), [Business context](../../baseline/business-context.md), [Current system context](../../baseline/system-context.md), [Index registry](indexes/README.md), [Authority/source map](indexes/authority-and-source-map.md)

## Supersession Notice

The user replaced this plan's Rust, two-frontend and fixed 50-module target with a new general AI Gateway implemented in a separate repository using Go plus one React/TypeScript/Tailwind Admin application. Kiro is now one complete provider module behind versioned contracts.

No source implementation started under this plan. All target-specific implementation statements below are preserved as a historical snapshot and must not be executed. The [Greenfield AI Gateway plan](../greenfield-ai-gateway/README.md) wins target architecture, technology, module, frontend, work-order, acceptance and cutover conflicts.

Current-source evidence, verified findings and the replay/terminal/lease/shutdown invariants identified in the [supersession record](history/superseded-by-greenfield-ai-gateway-2026-07-13.md) remain reference inputs only.

Historical canonical implementation: [Final complete Rust implementation plan](topics/delivery/migration-sequence.md)

Historical modular work: [Rust modular implementation work map](indexes/execution-slice-map.md)

Historical target technical authorities: [50-module ledger](indexes/target-module-ledger.md)

Historical complete source coverage: [Rewrite inventory](indexes/rewrite-inventory.md)

Historical implementation loop: [Entry and completion contract](topics/delivery/next-package-brief.md)

Historical validation/cutover: [Verification, final cutover and whole-system rollback](topics/delivery/verification-rollout-and-rollback.md)

Historical readiness: [Final-plan readiness review](history/final-plan-readiness-review-2026-07-12.md)

## Purpose

This plan converts a system-wide audit into one complete rewrite specification. Large files and coupling are symptoms, not the acceptance target: the plan addresses state authority, dependency direction, request-path I/O, consistency, retries, terminal behavior, resource bounds, performance, lifecycle, security, frontend contract drift, test validity, recovery and release provenance.

The finding set remains open-ended. The operator's observations and the current 47 verified findings seed systematic review; every module work unit repeats the relevant audit axes and promotes newly verified problems before they become silent code choices.

## Final Goal

Rewrite the complete first-party Rust core, both maintained Admin frontends, schema/state lifecycle, validation tooling and release tooling into one high-performance domain-oriented modular monolith.

Implementation is organized by module and dependency, but this is not phased product modernization. The legacy production release remains authoritative until one complete target-only candidate has all 50 modules, no legacy runtime code, both frontend rewrites, complete migrations/recovery and all gates. Production then performs one whole-system cutover; rollback selects the previous whole release.

No intermediate module, dependency group or partial candidate is a delivered product version.

## Product Boundary

`kiro-rs` is a self-hosted, single-operator, single-trust-domain Anthropic/Claude Code compatibility gateway. Multiple API keys, Kiro credentials, external pools or replicas do not represent users or tenants.

The rewrite must not introduce tenant identity, tenant repositories, per-tenant authorization, billing, quotas or data partitioning. Multiple replicas are supported for availability/capacity inside the same trust domain. External clients/upstreams/URLs, PgSQL, Redis, logs and filesystem remain protocol, failure and security boundaries.

## Complete Scope

- public request authentication, runtime capture, route/target planning, scheduling, request artifacts/payload, upstream attempts, retry policy, response/SSE, terminal recording and errors;
- PgSQL authority, Redis coordination/derived state, process snapshots, shared Files, migrations, backfills, backup/restore, release-generation fencing and forward recovery;
- usage, prompt-cache evidence, credentials, proxy resources, external pools, model catalog, audit and maintenance jobs;
- versioned secret encryption/key recovery, one weighted process resource governor, remote egress/media/PDF/tokenizer work, queues, bodies, caches, tasks, memory, FDs, connections, files, diagnostics and timeouts;
- Admin command/query services, generated Rust-to-TypeScript contract, all eleven workflows in both maintained frontends and browser state/security/accessibility/responsive behavior;
- bootstrap, producer-aware supervision/shutdown, honest readiness/health, real Claude Code, bounded real Kiro, contract/load/chaos/browser harnesses;
- Rust/frontend/dependency/Docker/CI artifacts, SBOM, signature/provenance, examples/runbooks, repository cleanup and release evidence;
- removal of superseded Rust/UI/script/test/config/schema/key/artifact paths and post-deletion/post-contraction verification.

## Accepted Architecture And Delivery

- [001](decisions/001-single-user-single-trust-domain.md): one operator and one trust domain.
- [003](decisions/003-attempt-replay-and-downstream-commitment.md): conservative replay safety and downstream commitment.
- [004](decisions/004-terminal-authority-and-partial-failure-recovery.md): one terminal decision, stable obligations and partial-failure recovery.
- [005](decisions/005-scheduler-queue-and-lease-lifecycle.md): bounded queue/lease/fencing/heartbeat/cancellation lifecycle.
- [006](decisions/006-producer-aware-shutdown-and-residue.md): producers finish before writer ingress closes; residue affects exit.
- [007](decisions/007-domain-oriented-modular-monolith-and-module-ownership.md): stable domain technical authorities, narrow contracts and no new God layer.
- [008](decisions/008-domain-owned-migrations-and-recoverable-adoption.md): domain-owned manifests, separate common migration runner and recovery authority.
- [009](decisions/009-single-program-modular-build-and-final-cutover.md): one complete implementation, target-only modular integration, one final cutover and whole-system rollback.
- [010](decisions/010-fixed-operational-and-acceptance-policies.md): fixed multi-replica, durability, Files/cache, resource/performance, scheduler, shutdown, revocation, recovery and hardening policies.
- [011](decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md): dedicated versioned secret-envelope and weighted process-resource authorities, expanding the target to 50 modules.
- [012](decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md): profile-specific tool normalization and collision-free reversible schema/property mapping.
- [013](decisions/013-owner-transaction-audit-acceptance.md): sealed same-transaction business mutation and audit append without a generic transaction escape hatch.
- [014](decisions/014-release-generation-recovery-and-rollback-state.md): signed release generation, expected-instance fencing, one-window migration and fail-closed per-state rollback compatibility.

[Decision 002](decisions/002-complete-module-by-module-rewrite.md) is superseded as a delivery model. Its complete-rewrite rationale remains historical, but per-module production canary/switch/soak/rollback is no longer active.

## Technical Authority, Not Personnel

`Owner`, `authority`, `state owner`, `contract owner` and `MOD-*` in this tree identify code/state/invariant boundaries. They do not mean a human responsible person.

This plan contains no project estimate, calendar, staffing, maintainer assignment, accountability assignment or person-dependent start gate. Runtime deadlines, queue/lease TTLs, performance sample counts, stability duration, RPO/RTO and final observation windows remain technical correctness constraints.

## Non-Goals

- Microservices or a workspace-crate-per-module split before evidence justifies it.
- Multi-user/multi-tenant identity, authorization, billing or partitioning.
- A public plugin ABI without an independently developed extension.
- Mechanical file splitting without changing authority/dependencies.
- Global horizontal `application`, `ports`, `adapters`, `workers`, service-locator, generic repository/event or broad immutable-context God layers.
- Keeping a legacy fallback, old/new selector or partial migration in the target release.
- Production traffic mirroring or duplicate real upstream/mutating comparison.
- Retiring either maintained frontend without a separate product decision.
- Treating local ignored reports, screenshots, workbooks or one benchmark as durable proof.

## Acceptance Requirements

- One immutable versioned runtime capture per request and only narrow views downstream.
- Pure decisions free of transport/storage/framework/shared-lock dependencies.
- Explicit state authority, transaction/CAS/outbox/idempotency/rebuild/failure semantics.
- Every allocation, inbound connection/stream/body, retained resource and blocking operation is admitted through accepted bounds with cancellation, overload and recovery behavior.
- Request/Admin API keys are verifier-only; replayable upstream secrets use the versioned envelope and recoverable external key ring.
- No ambiguous upstream replay and no reroute after downstream header commitment.
- One terminal lifecycle; response, scheduler, credentials and usage retain distinct authorities.
- Domain-owned immutable migrations, fenced common runner/ledger, separate recovery subsystem and previous-binary proof.
- Both frontend rewrites consume one generated contract and satisfy security/browser/accessibility/responsive gates.
- Absolute capacity/SLO plus same-host relative regression, operation budgets and RSS/FD/task/queue/connection/file recovery.
- Target source contains no legacy import/fallback/selector/stub, and every superseded path is deleted with post-deletion proof.
- A signed expected-instance release-generation barrier and private smoke pass while public readiness remains closed.
- Full static/Rust/storage/protocol/real-client/UI/load/recovery/Docker/supply-chain/evidence gates before one final cutover.

## Reading Path

1. Read [business context](../../baseline/business-context.md) and relevant current baseline maps.
2. Read accepted [decisions](decisions/README.md), especially 007-014.
3. Read the [final complete implementation plan](topics/delivery/migration-sequence.md).
4. Select an exact row from the [modular work map](indexes/execution-slice-map.md) and its `MOD-*` row.
5. Use the [entry/completion contract](topics/delivery/next-package-brief.md), affected traceability rows and gates.
6. Update plan state/evidence as source implementation lands; do not infer completion from planning.

## Readiness Semantics

- **Historical target implementation ready:** as of 2026-07-12, final Rust scope, technical decisions, exact work graph, conservative defaults, gates, deletion and final-cutover rules were fixed. This was the source plan's readiness state before supersession and is not current target readiness.
- **Production implementation Not Started:** no modernization target source/evidence has landed under this plan.
- **Production cutover Not Ready:** no complete candidate digest, migration rehearsal, load/recovery/client/browser/release evidence or rollback rehearsal exists.
- **Complete:** only after target activation, full-system observation, compatibility-state contraction, legacy cleanup and final post-contract gates.

Exact symbol maps, fixture results, measurements, migrations and `EVID-*` files are implementation outputs. Their absence does not mean the final plan is unfinished; it correctly prevents module/cutover completion claims.

## Documentation State

The earlier [planning-readiness review](history/planning-readiness-review-2026-07-12.md) is a preserved historical snapshot of the superseded phased-delivery model. The current result is the [final-plan readiness review](history/final-plan-readiness-review-2026-07-12.md).

Three reviewed analysis-only documents were deleted with Git recovery records; ten historical request/body and UI documents were archived with provenance. The remaining legacy documents stay protected until a coherent authority-domain disposition includes inbound-reference review and recovery instructions. Documentation cleanup never authorizes deleting runtime/user data or unregistered evidence.

## Current Evidence

No modernization `EVID-*` record exists. No Rust/frontend build, Docker run, PgSQL/Redis drill, load/chaos test, real Kiro request, real Claude Code session, browser E2E, migration rehearsal, final cutover or rollback has been run for this accepted plan. Planning readiness is not implementation or performance evidence.

No modernization `implementation-status.md` exists while source implementation has not started. Create it when implementation actually begins and keep it as a short handoff for the one complete program.
