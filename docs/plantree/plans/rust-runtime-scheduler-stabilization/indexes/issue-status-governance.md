# Issue Status Governance

Role: Bridges plan-tree roadmap authority with `feature/issues` issue-status detail

Status: `active-governance / current-as-of-2026-07-31`

Authority: Defines when implementation changes must update issue docs, the current issue status index, and plan-tree state

Read when: Changing code, config, tests, validation gates, release status, or issue documentation for the current Rust runtime/scheduler stabilization work

Related:

- [Plan root](../README.md)
- [Implementation status](../implementation-status.md)
- [Document disposition](document-disposition.md)
- [Current issue status index](../../../../../feature/issues/current-issue-status-index-20260731.md)
- [Issue analysis priority queue](../../../../../feature/issues/issue-analysis-priority-queue-20260731.md)
- [Feature workspace](../../../../../feature/README.md)

## Current Answer

Plan-tree already manages durable planning, roadmap state, decisions, open blockers, and release-gate direction. It should remain the single planning entrypoint.

Plan-tree does not replace `feature/issues`. The split is:

| Layer | Owns |
| --- | --- |
| `docs/plantree/` | roadmap, active phase, decisions, release gates, cross-issue priorities |
| `feature/issues/` | per-issue root cause, repro, fix plan, status, validation matrix, residual risk |
| `feature/issues/current-issue-status-index-20260731.md` | cross-issue rollup and current blocker index |
| `feature/issues/issue-analysis-priority-queue-20260731.md` | ordered one-by-one analysis queue |
| `feature/evidence/` | dated validation proof and build/request identities |

The operating rule is: change implementation and status together. A tracked issue is not durably handled if the code changed but the owning issue, rollup index, or plan-tree status still describes the old reality.

## Required Update Rules

Update the owning `feature/issues/*.md` in the same change set when a code/config/test/documentation change:

- fixes, partially fixes, or intentionally defers a documented issue;
- changes a reproduction result or root-cause explanation;
- changes status from `NO-GO`, `release-blocked`, `pending`, `open`, or `partial`;
- adds or removes a validation gate;
- changes a user-visible behavior mentioned in an issue;
- changes scheduler/runtime/fallback/WebSearch/tools/image/usage/dashboard/release behavior covered by an issue.

Update [the current issue status index](../../../../../feature/issues/current-issue-status-index-20260731.md) when:

- a new issue is added;
- an issue moves into or out of a blocker category;
- a `NO-GO` / `release-blocked` item closes;
- a `fixes-pending` or `implementation-in-progress` item becomes implemented;
- a final candidate, browser, real CLI, real upstream, load, or production recurrence gate closes;
- an issue is superseded or archived.

Update plan-tree when:

- roadmap phase, next target, or active TODO changes;
- a release blocker opens or closes;
- a cross-cutting decision is made;
- a validation/release gate changes;
- a new long-lived plan/topic/index is created;
- user priority changes the owning plan's execution order.

Do not update plan-tree for every small code edit. Update it when the edit changes durable planning truth.

## Definition Of Done

For tracked issues, done requires all applicable layers:

1. Implementation or config change exists.
2. Owning issue document records the new status, evidence, and residual risk.
3. Current issue status index no longer lists the issue under an obsolete category.
4. Evidence is linked or summarized with date, command/gate, binary/build identity, request id, or rollout observation.
5. Plan-tree roadmap/status is updated if the issue was a roadmap item or release gate.

## Validation

After issue-document edits, run:

```bash
node feature/tests/check-feature-docs.mjs
```

When editing plan-tree links or indexes, also run a focused link check if available, or at minimum inspect the touched relative links.

## Residual Risk

This is a governance rule, not an automated enforcement mechanism. It reduces drift only if maintainers follow it. Future work can add a CI check that fails when a code change touches known domains while issue statuses remain stale, but that requires a maintained mapping from source paths to issue owners.
