# 002: Complete Module-By-Module Rewrite

Role: Architecture delivery decision record

Status: Superseded by [decision 009](009-single-program-modular-build-and-final-cutover.md)

Date: 2026-07-11

Authority: Historical delivery decision; the complete-rewrite and modular-build intent is retained by decision 009, while its per-module production rollout model is no longer binding

Scope: All first-party core implementation, including the Rust runtime/control/infrastructure modules, both maintained Admin frontends, validation/release harnesses, migration sequence, compatibility facades, shadow comparison, rollback, and legacy deletion

Affected requirements/findings: Fixed constraints 6-8 and every current problem-catalog ID for delivery sequencing. This decision accepts complete module replacement, not the detailed remediation proposed for each finding.

Decision source: Operator instruction on 2026-07-11: `直接全部重写，但是是分模块进行`.

Related: [Plan root](../README.md), [Target architecture](../topics/architecture/target-system-architecture.md), [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Rewrite inventory](../indexes/rewrite-inventory.md), [Migration sequence](../topics/delivery/migration-sequence.md), [Roadmap](../roadmap.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md)

Supersession note: The operator's 2026-07-12 instruction replaced per-module production switch/canary/soak/rollback with modular target-only implementation and one final system cutover. The original text below is retained as the historical rationale and rejected delivery model; use decision 009 for current execution.

## Context

The current implementation has meaningful helper modules but concentrates ownership in `MultiTokenManager`, Messages handlers, `AdminService`, `PostgresStore`, `RedisStore`, `ExternalPoolManager`, and large configuration structures. Continuing to extract individual functions into adjacent files would leave the same shared state, I/O, and dependency direction.

The operator explicitly selected a complete rewrite, performed by module rather than as one all-system replacement.

## Decision

The modernization will rewrite the complete first-party core implementation module by module. This includes the Rust data plane, control plane, protocol/application/domain/infrastructure code, both maintained Admin frontends, process lifecycle, and the validation/release harnesses needed to prove the replacement. Third-party dependencies, historical documentation, and generated artifacts are not rewritten merely to satisfy the word “complete”; they are replaced only when the target design or evidence requires it.

For each module:

1. freeze and characterize the external and cross-module contract;
2. define the new module's inputs, outputs, state ownership, failure semantics, metrics, and resource budgets;
3. implement the replacement in the target module tree without adding its new domain logic to the old God Object;
4. run pure shadow comparison or dual-read comparison where safe, never a duplicate real upstream side effect;
5. cut over behind a narrow compatibility adapter or deterministic canary;
6. retain the old implementation only for the documented rollback window;
7. delete the superseded implementation, adapter branch, obsolete tests/config fields, and dead dependencies after exit gates pass.

The complete system is not switched at once. Only one bounded module or dependency slice is switched per accepted phase. External protocols and durable data remain compatible unless a separate accepted decision changes them.

## Module Rewrite Order

The default dependency-first order is:

1. shared identifiers, error taxonomy, runtime snapshot, resource budget, and observability primitives;
2. configuration repository/CAS and state ports;
3. usage/cache accounting event model and durable writers;
4. scheduler domain and coordinator;
5. upstream Kiro and external adapters;
6. request planning/body artifact pipeline;
7. response/SSE translation and terminal outcome handling;
8. Admin command/query services, generated frontend contracts, and both maintained frontend applications;
9. bootstrap, supervisor, readiness, shutdown, recovery, validation harnesses, and release integration;
10. removal of remaining legacy backend/frontend/storage/handler/manager/facade/harness paths.

The roadmap may split these into smaller packages but may not invert dependencies merely to rewrite a visible module first.

## Alternatives Considered

### Continue incremental in-place refactoring

Rejected as the final strategy. It risks making the existing God Objects permanent compatibility containers and can preserve hidden coupling while only moving code.

### Rewrite and switch the whole system at once

Rejected. It removes reliable characterization, rollback, fault isolation, and causal performance comparison.

### Split into microservices first

Rejected for this plan. Process boundaries would add network consistency and operations before domain boundaries are proven. The new modules remain independently bounded inside a modular monolith.

## Compatibility And Data Consequences

- Public API, SSE, upstream, cache/usage, and Admin behavior stays characterized throughout the rewrite.
- Database changes use expand-contract; a new binary can run against the expanded schema while the previous binary remains rollback-capable.
- Event IDs, versions, and new columns are additive before legacy fields are removed.
- Old and new code must not both execute a real Kiro/external call for shadow comparison.
- Dual writes are allowed only with explicit idempotency and reconciliation; shadow calculation is preferred.

## Rollout And Rollback

- Every rewritten module has an independent execution switch.
- Canary selection uses a stable request ID hash or explicit operator flag, never a user/tenant dimension.
- Rollback switches execution to the old module without requiring schema downgrade.
- Rollback conditions include compatibility mismatch, data divergence, resource growth, latency regression, queue/backlog growth, readiness failure, or unrecoverable writer residue.
- The old module cannot be deleted until the rollback window, soak, and evidence gates pass.

## Completion Criteria

This decision is complete only when all core modules have been replaced and the old implementations, compatibility branches, obsolete state, and redundant dependencies have been removed. Smaller files alone are not completion evidence; the target dependency rules, invariants, performance/resource gates, and external compatibility must all pass.

The [rewrite inventory](../indexes/rewrite-inventory.md) is the coverage ledger. Every inventory row must identify its replacement package, switch evidence, legacy deletion evidence, and any explicitly accepted reuse/replacement exception.

## Observability And Verification Obligations

- Each module exposes old/new execution selection, shadow mismatch, cutover percentage, rollback, and legacy-reference metrics where applicable.
- Pure shadow work is deterministic and bounded; it never repeats a real upstream or other side effect.
- Each cutover records protocol/behavior parity, state reconciliation, performance/resource comparison, recovery, and cleanup evidence.
- Deletion requires import/dependency search, accepted soak threshold, previous-binary data compatibility, and post-deletion full gates.
- A module cannot be marked rewritten when its old implementation remains the default or is silently called as fallback outside the registered rollback switch.
