# Release 114 Hardening History Archive

Role: Historical archive for the v0.0.114 post-upgrade production hardening package

Status: Archived on 2026-07-28

Authority: Preserves v0.0.114-specific production issue analysis, fixes, and validation notes; does not define current release status or current runtime/scheduler direction

Read when: Retrieving the 114 upgrade dashboard/schema/external-billing/request-rejection/scheduler-risk/thinking-output-config/window-timeout hardening notes

Current authority: [Rust Runtime Scheduler Stabilization](../../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, active `feature/issues/**`, and current `feature/evidence/**`

## Source Paths

| Original path | Archived path | Disposition |
| --- | --- | --- |
| `feature/release-114-hardening/**` | [`release-114-hardening/`](release-114-hardening/README.md) | historical v0.0.114 hardening package |

## Current Interpretation

The package records a real production-hardening pass after v0.0.114. It is no longer the current work queue because later versions and incidents introduced new scheduler/runtime, dashboard, external-pool, and protocol issues. Current active work must be tracked through the plan-tree and issue/evidence indexes.

## Recovery

Moved with `git mv`. Restore by moving the directory back to `feature/release-114-hardening/` in a separate change and re-running inbound-link checks.
