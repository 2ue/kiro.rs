# Migration Subsystem Contract

Role: Exact implementation contract for `R2.0.migration-foundation` and its final-cutover use

Status: Accepted and Ready; implementation/evidence Not Started

Authority: Binds `MOD-MIGRATIONS`, domain migration authorities, bootstrap invocation, recovery separation, legacy adoption, release-generation fencing and previous-binary compatibility under decisions 008-014

As of: 2026-07-12

Read when: Implementing schema/ledger/adoption behavior, adding a domain manifest, changing startup migration, rehearsing final cutover, or deleting the legacy runner

Related: [Decision 008](../../decisions/008-domain-owned-migrations-and-recoverable-adoption.md), [Decision 009](../../decisions/009-single-program-modular-build-and-final-cutover.md), [Decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md), [Module ledger](../../indexes/target-module-ledger.md), [Work map](../../indexes/execution-slice-map.md), [State ownership](../architecture/state-ownership-and-consistency.md), [Verification](verification-rollout-and-rollback.md)

## Fixed Boundary

| Concern | Technical authority |
| --- | --- |
| Manifest instances, SQL/DDL, pre/postconditions, compatibility notes, owner backfill invariant | Each state-owning `MOD-*` module |
| Common manifest schema/validation, dependency plan, fencing, transaction/resume execution, active/applied/adopted/checkpoint ledger and migration reconciliation | `MOD-MIGRATIONS` |
| Prerequisite checks, manifest collection/order, invocation and readiness gating | `MOD-BOOTSTRAP` |
| Large backfill claim/progress/cancel/checkpoint execution | Domain authority through `MOD-MAINTENANCE-JOBS` |
| Backup/restore verification, Redis rebuild, previous-binary and cross-authority forward recovery | `MOD-RECOVERY` |

`MOD-MIGRATIONS` owns no domain SQL, business repair rule, large backfill, backup policy, Redis rebuild, Files restore, or general lifecycle supervision. `MOD-RECOVERY` owns no migration manifest, runner, migration lock, or applied ledger. This split prevents a new lifecycle/storage God module.

## Public Contract

The common migration API is narrow and typed:

```text
inspect(database_identity, manifests) -> Inspection
plan(inspection, manifests) -> DeterministicPlan
apply(plan, fence, deadline) -> RunOutcome
resume(run_id, fence, deadline) -> RunOutcome
abort(run_id, reason) -> AbortOutcome
reconcile(run_id, forward_action) -> ReconcileOutcome
status() -> MigrationStatus
```

The API accepts immutable manifest metadata and typed probe/execution ports. It does not accept arbitrary mutable SQL strings, repositories, generic service maps, bootstrap state, untyped JSON commands, or owner-private records.

## Identity And Ledger

Every structural migration identity includes:

- exact `module_id` and owner-local monotonic version;
- checksum of the immutable canonical definition;
- dependencies and transaction mode;
- precondition and postcondition probe identities;
- additive/previous-binary compatibility contract;
- separate backfill prerequisite when required.

The PgSQL ledger records immutable applied/adopted identities, one fenced active run, deterministic plan identity, step/checkpoint state for declared resumable work, result/failure state and bounded non-secret diagnostic facts. An applied identity with a different checksum fails before mutation. No command overwrites history to make startup pass.

The advisory-lock namespace and ledger schema are versioned constants. Lock acquisition is bounded by startup/recovery deadline and lock loss invalidates the fence immediately. Concurrent replicas inspect/wait or fail readiness; only the current fence may mutate.

## Legacy Adoption Map

Implementation begins by generating a pinned-revision adoption artifact that maps every current:

- versioned migration row and checksum;
- mutable `inline-schema` statement/section;
- index, constraint and table definition;
- startup repair, delimiter-split statement group and conditional compression action;
- startup/full-table backfill and hidden post-migration mutation;
- affected reader/writer and previous-binary behavior;

to one immutable domain manifest, one bounded domain job, one explicit verified adoption probe, or one rejected unknown state.

A broad `current schema` marker is insufficient. Unknown, conflicting or partial facts hold readiness closed. Adoption records are created only when exact catalog and domain postcondition probes agree.

## Required Entry Classes

1. **Fresh:** create the minimum common ledger under a fenced transaction, then execute all registered domain manifests in deterministic dependency order.
2. **Verified legacy:** compare catalog, old ledger, constraints/indexes and domain postconditions with the reviewed adoption map; record adoption identities without claiming the target ran old statements.
3. **Partial/interrupted:** identify the exact module/version/step, retain readiness closed and use only a declared idempotent resume or forward reconcile.
4. **Corrupt/drifting:** fail before mutation on checksum, catalog, constraint, data invariant or dependency disagreement.
5. **Concurrent start:** one fenced runner mutates; other replicas wait within the bounded policy or remain unready.

Each class has fresh fixtures and multiple production-shaped legacy snapshots. Tests inject failure before/after lock, statement, transaction, checkpoint, ledger commit and postcondition.

## Backfill Rules

Bounded transactional structural work may run in the migration plan. Large scans, recomputation, compression, repair or row transformation are domain jobs with:

- stable job/checkpoint identity and idempotent batches;
- explicit row/WAL/disk/memory/lock/time budgets;
- cancellation and restart semantics;
- observable progress/backlog/oldest checkpoint;
- a final postcondition required before target readers/writers activate;
- no secret or business row bodies in logs/evidence.

Normal startup does not hide a large data job, and no target backfill mutates production while legacy serves traffic. After legacy admission/job claim/producers/writers stop, the one final maintenance window executes every required structural/adoption/backfill step. A recent production-shaped clone must prove the exact plan within the decision-014 45-minute migration budget; missing identity/capacity proof aborts before mutation, and an overrun after additive work checkpoints and returns to the previous release without reverse DDL.

## Previous-Binary Profile

The previous complete binary must be tested against every additive target schema/event/key change for the whole-system rollback window. Its profile must disable or isolate the legacy migration executor and any conditional startup repair/compression that could overwrite, rerun or misinterpret target history.

Rollback after target mutation:

- does not reverse DDL blindly;
- does not delete target ledger/outbox/job rows;
- does not replace target checksums;
- starts only when compatibility probes pass;
- preserves target-written additive facts for later forward reconciliation.

If the previous binary cannot be made compatible without corrupting state, final cutover is blocked; this is not converted into a no-rollback release.

## Target-Only Integration

During development, target migrations run only on dedicated fresh or cloned databases. Legacy and target runners never execute mutating work against the same database. Read-only legacy observation may generate normalized facts, but no release target imports the old runner.

Domain manifest packages integrate into one deterministic full plan. Every new manifest reruns dependency-cycle, checksum, fresh, adoption, previous-binary and affected restore tests. The complete plan is rehearsed rather than activating one domain schema in production at a time.

## Final-Cutover Use

1. Stop legacy admission, Admin/config mutation and job claim.
2. Join all legacy producers before writer ingress closes under decision 006.
3. Verify backup/WAL, Redis/lease reconciliation and previous-binary checkpoint.
4. Acquire the target fence and inspect the exact production database identity.
5. Execute the full deterministic expand/adopt/backfill plan within the rehearsed maintenance-window budget.
6. Require every postcondition, compatibility and no-active-backfill probe to pass.
7. Start the complete target candidate with readiness/load-balancer admission closed; attest every expected instance to one signed release generation and prove every old generation fenced.
8. Run deployment-private migration/state/API/Admin/Files/isolated-client smoke without duplicate real side effects while public admission remains closed.
9. Open target traffic only as the one final system activation.

Destructive contract is prohibited during the whole-system rollback window. After the final observation passes, compatibility-only columns/keys/projections are contracted through immutable domain manifests and all full gates rerun.

## Verification

- stable manifest ordering/checksums on clean checkout;
- no cycles, duplicate identities, mutable history or unregistered SQL;
- fresh/legacy/partial/corrupt/concurrent/lock-loss cases;
- transactional rollback and non-transactional idempotent resume;
- large-backfill bounds, cancellation, restart and no startup full scan;
- previous-binary start/blocked behavior exactly matching its profile;
- bootstrap contains no SQL/runner internals and calls only the public migration contract;
- Recovery contains no migration runner/ledger or domain DDL;
- Migrations contains no backup/Redis-rebuild/domain DDL;
- source/database/plan/ledger/backup identity, cleanup and secret-safe evidence.

## Legacy Deletion Gate

Delete the old delimiter executor, mutable inline schema/checksum behavior, legacy common runner and hidden startup repairs/backfills from the target source before the final release candidate freezes. Deletion requires:

1. every old statement/repair/backfill appears in the adoption map;
2. every accepted target responsibility has an immutable manifest or bounded job;
3. all entry classes and previous-binary tests pass;
4. target import/reference searches find no old runner path;
5. full fresh/adoption/recovery/startup gates pass after deletion;
6. the immutable previous release artifact, not embedded target source, remains available for whole-system rollback.

## Current State

The contract is accepted and implementation-ready. The adoption artifact, ledger schema/code, manifests, fixtures, rehearsals, deletion evidence and `EVID-*` records do not yet exist; they are implementation outputs, not missing personnel assignments or open architecture questions.
