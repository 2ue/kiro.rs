# Scheduler, State, External Pool, And Runtime History Archive

Role: Historical archive for local credential scheduler, external fallback pools, Redis/PgSQL state, runtime usage scheduler, and credential data-plane analysis

Status: Archived on 2026-07-28

Authority: Preserves older scheduler/state/external-pool/runtime analysis; does not define current local/external routing, Redis/PgSQL hot-path, fallback, or capacity-ledger behavior

Read when: Retrieving historical scheduler/state rationale or comparing old designs with current RoutePlanner / CapacityLedger work

Current authority: current source, [Rust Runtime Scheduler Stabilization](../../plantree/plans/rust-runtime-scheduler-stabilization/README.md), active `feature/issues/*scheduler*` documents, and dated production evidence packages

## Source Paths

| Original path | Archived file |
| --- | --- |
| `docs/credential-list-data-plane-optimization-design.md` | [credential-list-data-plane-optimization-design.md](credential-list-data-plane-optimization-design.md) |
| `docs/credential-rate-limit-and-scheduler-optimization.md` | [credential-rate-limit-and-scheduler-optimization.md](credential-rate-limit-and-scheduler-optimization.md) |
| `docs/credential-scheduler-hotpath-performance-analysis.md` | [credential-scheduler-hotpath-performance-analysis.md](credential-scheduler-hotpath-performance-analysis.md) |
| `docs/external-fallback-pools-design.md` | [external-fallback-pools-design.md](external-fallback-pools-design.md) |
| `docs/redis-pgsql-migration-optimization-analysis.md` | [redis-pgsql-migration-optimization-analysis.md](redis-pgsql-migration-optimization-analysis.md) |
| `docs/redis-pgsql-state-model-full-analysis.md` | [redis-pgsql-state-model-full-analysis.md](redis-pgsql-state-model-full-analysis.md) |
| `docs/runtime-usage-scheduler-performance-fix-20260620.md` | [runtime-usage-scheduler-performance-fix-20260620.md](runtime-usage-scheduler-performance-fix-20260620.md) |

## Current Interpretation

This archive preserves earlier scheduler and state-model reasoning. Current runtime safety work must use the active stabilization plan because later incidents exposed broader external-pool, storage-bridge, fallback-loop, and degraded-dependency failure modes.

## Recovery

Moved with `git mv`. Restore a file by moving it back from this archive in a separate change and re-running inbound-link checks.
