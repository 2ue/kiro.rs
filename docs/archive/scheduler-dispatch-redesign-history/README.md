# Scheduler Dispatch Redesign History Archive

Role: Historical archive for the earlier credential scheduler dispatch redesign

Status: Archived on 2026-07-28

Authority: Preserves the previous implemented dispatch design and July production follow-up links; does not override current scheduler/runtime stabilization work

Read when: Comparing the older health-balanced scheduler design with current RoutePlanner / CapacityLedger / external-pool follow-up work

Current authority: [Rust Runtime Scheduler Stabilization](../../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, and dated production evidence

## Source Paths

| Original path | Archived path | Disposition |
| --- | --- | --- |
| `docs/scheduler-dispatch/**` | [`scheduler-dispatch/`](scheduler-dispatch/README.md) | historical implemented scheduler redesign |

## Current Interpretation

The archived strategy describes an implemented local credential scheduler redesign and remains useful for understanding older `priority` / `balanced` / `health_balanced` behavior, cooldown state, Redis leases, queue limits, and Admin observability fields.

It is no longer the current planning authority for the July 2026 incidents because the active work now includes broader runtime and routing concerns:

- external-pool interference with local credential scheduling;
- fallback and direct-external routing semantics;
- avoiding synchronous PgSQL/Redis waits on request hot paths;
- durable capacity accounting and route planning;
- chaos validation across local credentials, external pools, and degraded dependencies.

Use the current scheduler stabilization plan for active implementation decisions. Use this archive as historical context and provenance.

## Recovery

This collection was moved with `git mv`, so Git history preserves the original files. To restore the previous top-level path for investigation, use a targeted `git mv` from this archive back to `docs/scheduler-dispatch/` in a new change, then re-run inbound-link checks before treating restored content as active.
