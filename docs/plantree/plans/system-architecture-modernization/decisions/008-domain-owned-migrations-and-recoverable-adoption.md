# 008: Domain-Owned Migrations And Recoverable Adoption

Role: Architecture decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding migration definition authority, common migration protocol and ledger, legacy-database adoption, bounded backfills, previous-binary compatibility, and legacy-runner deletion contract

Scope: PgSQL schema and data evolution for every state-owning target module, `R2.0.migration-foundation`, all `R2.4.<domain>` slices, bootstrap migration invocation, and R9 dependency-group recovery integration

Affected requirements/findings: `OPS-005`, `INV-007`, `QA-REL-005`, `QA-OPS-003`, `ARCH-002`, and the migration/recovery portions of `G-C`, `G-OPS`, `G-S`, and `G-EVID`

Decision source: Migration-authority and legacy-adoption review plus final-plan convergence on 2026-07-12; final execution follows decision 009, recovery objectives follow decision 010, and the one production migration/rollback state follows decision 014

Related: [Migration foundation brief](../topics/delivery/migration-foundation-brief.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Execution slice map](../indexes/execution-slice-map.md), [Rewrite sequence](../topics/delivery/migration-sequence.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md), [Decision index](README.md)

## Context

The current PgSQL startup path combines connection setup, a large mutable inline schema, semicolon-delimited statement execution, versioned migration rows whose checksums can be overwritten for the inline schema, and table-wide repair/backfill work. The relevant implementation is concentrated in `src/storage/postgres.rs:200-220`, `280-353`, `3437-3494`, `6793-6810`, and `7032-7283`, with startup behavior controlled from `src/model/config.rs:2455-2474`.

Splitting `PostgresStore` without changing migration authority would move the same risk into a new shared migration God module. Conversely, making every domain invent its own runner, lock, ledger, and partial-failure behavior would produce incompatible recovery semantics. The target needs domain-owned schema definitions and one common execution protocol without transferring domain DDL ownership to bootstrap, the migration runner, or disaster-recovery orchestration.

The target must also adopt databases created by the current runner. A new ledger cannot safely infer that an old database is correct from the mutable `inline-schema` checksum alone, rerun every statement blindly, or mark an unknown partial state as applied. Fresh databases, clean legacy databases, partial legacy executions, checksum drift, and concurrent replica startup require different, explicit paths.

## Decision

Migration responsibility is divided among technical authority modules:

| Responsibility | Technical authority | Boundary |
| --- | --- | --- |
| Immutable migration instances | Each state-owning `MOD-*` module | Own ordered manifest entries, SQL/DDL, preconditions, postconditions, compatibility notes, and any owner backfill handoff for its own state only. |
| Common migration protocol and mechanics | `MOD-MIGRATIONS` | Own manifest schema/validation, dependency planning, advisory locking/fencing, transaction-or-resume execution, active-run/applied/checkpoint ledger semantics and ports, inspection, resume, abort, and migration forward reconciliation. It owns no domain SQL/DDL, backup policy, Redis rebuild, or disaster-recovery orchestration. |
| Normal startup orchestration | `MOD-BOOTSTRAP` | Validate infrastructure prerequisites, supply the registered owner manifests in accepted dependency order, invoke the `MOD-MIGRATIONS` public contract, hold readiness closed, and report the outcome. It owns neither manifest contents nor runner internals. |
| Supported-profile recovery | `MOD-RECOVERY` | Own backup/restore verification, Redis rebuild/epoch coordination, previous-binary recovery procedure, and cross-authority forward-recovery orchestration through public contracts. It owns no migration definitions, common runner, or migration ledger. |
| Bounded data backfill | The state-owning domain, normally through `MOD-MAINTENANCE-JOBS` execution contracts | Own row selection, transformation invariant, checkpoint meaning, rate/resource budget, cancellation, and completion criteria. Normal startup never hides a large backfill, and production execution occurs only inside decision 014's one fenced final maintenance window. |

A physical PgSQL database and a common ledger do not create shared domain schema authority. `MOD-MIGRATIONS` validates and executes immutable owner definitions; it does not author, mutate, reorder, or silently repair those definitions.

## Identity And Ledger Contract

Every structural migration has a stable identity containing at least `module_id`, an owner-local monotonic `version`, and a checksum over the immutable canonical definition. Its manifest also records dependencies, transaction mode, compatibility window, precondition and postcondition probes, and whether a separate owner backfill must complete before the complete target candidate can enter final activation.

The common durable ledger distinguishes:

- immutable applied records, keyed by module and version, with checksum and application identity;
- an active run with owner set, plan identity, lock/fence identity, start/update time, and terminal state;
- step/checkpoint state for declared non-transactional work;
- explicit adoption records that map verified legacy state to target migration identities without pretending the target runner executed the old statements;
- failure and operator-action state that can be inspected without reading secret-bearing SQL or row values.

Exact table and column names remain an implementation detail of the accepted `R2.0.migration-foundation` brief. Semantics are not optional: an already applied identity with a different checksum is a pre-mutation hard failure. The runner never overwrites historical checksums to make startup succeed.

## Fresh And Legacy Adoption

The first target runner supports five separately tested entry classes:

1. **Fresh database:** create the minimum foundation ledger under one fenced transaction, then apply owner manifests in dependency order.
2. **Verified legacy database:** inspect current catalog/schema facts, legacy `schema_migrations` rows, constraints, indexes, and owner postconditions; compare them with a reviewed adoption map; record target adoption identities only when every required probe agrees.
3. **Partial or interrupted legacy execution:** keep readiness closed, report the exact unknown/partial owner step, and require a declared idempotent resume or forward-reconcile action. Absence of an old marker is not proof that no statement ran.
4. **Corrupt or drifting state:** fail before mutation when checksum, catalog, constraint, or owner invariant evidence conflicts. Automatic checksum replacement, broad best-effort repair, and silent downgrade are prohibited.
5. **Concurrent startup:** one fenced runner owns mutation. Other replicas inspect/wait within policy or fail readiness; they never execute the same plan concurrently or report a partial step as applied.

The adoption map is a versioned source artifact produced by the `R2.0.migration-foundation` entry audit. It enumerates every current versioned migration, the mutable inline schema baseline, unversioned repair, conditional rollup compression, and startup backfill that must become an owner migration, an owner job, an explicit compatibility probe, or a rejected unknown. No catch-all "current schema" marker is sufficient.

## Backfills And Contract Windows

Structural expand steps may run during normal migration when their work is bounded and transactional. Large scans, recomputation, compression, repair, and row transformation use a separate owner job with:

- stable job and checkpoint identity;
- bounded batch, lock, row, WAL, disk, memory, and duration budgets;
- observable progress and oldest-checkpoint age;
- idempotent retry and cancellation semantics;
- a completion condition referenced by the one final whole-system target activation;
- no secret-bearing row values in logs or evidence.

Schema evolution uses expand, bounded backfill, one final whole-system target activation, whole-system observation, then contract. These are integration/deletion boundaries inside one candidate and one production maintenance window, not per-module production switches. A previous binary must tolerate the expanded schema during the documented whole-system rollback window. Destructive downgrade is not the normal rollback mechanism.

## Previous-Binary Compatibility

Before final cutover, the accepted rollback profile must prove the immutable previous binary starts with `KIRO_RS_POSTGRES_MIGRATE_ON_START=false`, rollup compression disabled, and every legacy conditional repair/backfill either disabled or isolated from target history. If any old startup writer cannot be disabled and cannot prove compatibility, cutover is blocked. Decision 014 fixes the complete previous-state matrix; an assumption that both runners will coexist is not evidence.

Rollback selects the previous compatible application reader/writer against additive schema. It does not delete target ledger rows, rewrite target checksums, or run destructive reverse DDL.

## Legacy Runner Deletion

The old startup runner is deleted from target source before final candidate freeze, after:

1. the target runner is the sole executor in fresh, verified legacy, partial/interrupted, corrupt/drift, and concurrent-start target fixtures;
2. every current schema statement, versioned row, repair, and startup backfill has a named domain owner and target disposition;
3. the accepted previous-binary profile passes against the expanded schema and ledger;
4. source/reference search finds no live startup path through the delimiter executor, mutable inline-schema checksum, hidden repair, or hidden startup backfill;
5. `migrateOnStart`, rollup-compression startup options, deployment docs, and runbooks are removed or have an explicit time-bounded compatibility disposition;
6. durable evidence records ledger inspection, target candidate/release identity, rollback-profile result, and post-deletion checks.

The old `schema_migrations` table or selected rows may remain read-only during the previous-binary window when required for compatibility. Retention does not make them the new authority, and their removal is an explicit later contract step.

## Alternatives And Tradeoffs

### Let bootstrap own all migrations

Rejected. It makes process composition the schema authority and recreates the current central storage/startup coupling.

### Let each domain implement a complete runner and ledger

Rejected. Locking, checksum, adoption, checkpoint, and recovery semantics would drift, and concurrent startup would be harder to reason about.

### Treat current catalog state as automatically applied

Rejected. Catalog similarity cannot prove that constraints, data repairs, partial statements, or compatibility invariants are complete.

### Keep large backfills in startup for operational simplicity

Rejected. It makes startup duration and resource use unbounded, hides cancellation/progress, and couples readiness to table cardinality.

The accepted split adds manifest mapping, probes and ledger machinery. That cost is justified because it makes schema authority, partial failure and deletion readiness testable per module while keeping the one whole-system rollback and disaster recovery explicit and separate.

## Target Integration And System Rollback

1. Complete the migration source/schema audit and produce the legacy-to-module-authority adoption map.
2. Implement the common target protocol/ledger and every immutable domain manifest in the target candidate; legacy production remains authoritative during development.
3. Validate fresh and independently cloned legacy fixtures, including partial, interrupted, drifting, corrupt, and concurrent states.
4. Rehearse the complete target migration plan in isolation. Never run legacy and target real DDL concurrently against the same database.
5. Delete the legacy runner and hidden startup backfills from target source before the final candidate freezes.
6. During final cutover, run one fenced target migration plan and retain additive previous-binary compatibility through the whole-system rollback window.

Rollback before target mutation returns to the previous binary on an untouched compatible database. Rollback after target mutation uses the accepted previous-binary profile and expanded schema; it never erases applied target history or runs the old migration executor blindly.

## Verification

- Fresh, legacy-adopted, partial, corrupt, checksum-drift, concurrent-start, lock-loss, transaction-failure, and non-transactional-resume fixtures are required.
- Owner manifests are unique, deterministic, dependency-acyclic, and produce stable checksums on a clean checkout.
- A fake or isolated authority proves Migrations can execute and resume without importing that authority's private SQL types.
- A previous binary starts or is deliberately blocked exactly as its accepted rollback profile states.
- Large backfill fixtures prove bounded batches, cancellation, restart, progress, and no normal-startup full-table scan.
- Static checks prove bootstrap, Migrations, and Recovery contain no domain DDL; Recovery cannot own the common migration runner/ledger; and domain modules do not implement private competing ledgers.
- Evidence records source revision, database fixture identity, starting ledger/catalog state, target plan identity, result, cleanup, and secret scan.

## Implementation Parameters

- The migration audit must enumerate exact legacy version/checksum/adoption mappings and fail closed on state that probes cannot prove.
- The target defines a versioned ledger schema, stable advisory-lock namespace, bounded lock wait, supported PostgreSQL matrix, and transaction-or-resume behavior; these are executable artifacts, not personnel assignments.
- The previous-binary profile disables or isolates the legacy migration executor and proves conditional startup work cannot reinterpret target history.
- Decision 010 resolves backup/recovery objectives (`Q-012`).
- This record accepts the migration contracts; implementation and evidence remain Not Started.
