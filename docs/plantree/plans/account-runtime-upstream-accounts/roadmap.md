# Account Runtime Upstream Accounts Roadmap

Role: Durable state for the current Rust Kiro-removal refactor

Status: Planning

Authority: Tracks readiness and execution order for the current-repository target only

As of: 2026-08-13

Related: [Plan root](README.md), [final target plan](topics/final-target-plan.md)

## Done

- User intent clarified: the target system must have no Kiro business concept, not even as a first target provider.
- Mainline workflow decided: commit current work, merge to `master`, then branch for implementation.
- Target plan created under `docs/plantree/plans/account-runtime-upstream-accounts/`.

## In Progress

- Commit current `feature/usage-correction-cost-floor` work and merge it into `master`.

## Next

- Create `feature/account-runtime-upstream-accounts` from updated `master`.
- Start by introducing provider-neutral account runtime types and replacing external-pool terminology with account terminology at the domain boundary.
- Split scheduler primitives from Kiro credential types before deleting Kiro modules.
- Convert body and protocol paths to canonical/upstream-account logic with no Kiro envelope or Kiro event dependency.

## Deferred

- Archive or rewrite old Kiro-focused planning documents after the implementation has landed and the new target has evidence.
