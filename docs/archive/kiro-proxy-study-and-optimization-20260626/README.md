# Kiro Proxy Study And Optimization Plans Archive

Role: Historical archive for the 2026-06-26 Kiro proxy comparison study and derived optimization plans

Status: Archived on 2026-07-28

Authority: Preserves historical research and implementation notes; does not define current scheduler/runtime direction

Read when: Retrieving the 2026-06 external-project comparison, old optimization backlog, or June 27 implementation record

Current authority: [Rust Runtime Scheduler Stabilization](../../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, and dated evidence

## Source Paths

| Original path | Archived path | Disposition |
| --- | --- | --- |
| `docs/kiro-proxy-study-20260626/**` | [`kiro-proxy-study-20260626/`](kiro-proxy-study-20260626/README.md) | historical external-project study |
| `docs/kiro-optimization-plans-20260626/**` | [`kiro-optimization-plans-20260626/`](kiro-optimization-plans-20260626/README.md) | historical optimization plan and implementation record |

## Current Interpretation

These documents remain useful as historical context for:

- external project comparison;
- early scheduler/health/cachePoint/testing ideas;
- the June 27 implementation record and its then-current validation claims.

They are no longer current execution authority because later production incidents and fixes changed the active questions:

- external-pool/local-first scheduling and fallback loops;
- RoutePlanner / CapacityLedger design;
- HTTP runtime vs PgSQL/Redis synchronization boundaries;
- thinking-signature and real Claude Code CLI protocol safety.

Use the current plan-tree topics for active work and this archive only for retrieval or provenance.

## Recovery

This collection was moved with `git mv`, so Git history preserves the original files. To restore the previous top-level paths for investigation, use a targeted `git mv` from this archive back to the original path in a new change, then re-run inbound-link checks before treating restored content as active.
