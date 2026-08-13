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
- Current `feature/usage-correction-cost-floor` work was committed and merged into `master`.
- Implementation branch `feature/account-runtime-upstream-accounts` was created from updated `master`.
- Initial provider-neutral `account_runtime` domain module was added and verified with scoped `cargo test account_runtime` and `cargo check`.
- Old `ExternalPool` records can now project into the new `UpstreamAccount` boundary for migration staging, with a focused bridge test.
- Account-named Admin/API aliases were added for the existing upstream account management surface while the old UI and DTO names are migrated.
- Admin UI navigation now points to `/accounts` as "上游账号"; legacy `/external-pools` redirects to the account page.
- The upstream account page now uses account-named API calls, while component/type filenames remain to be migrated.

## In Progress

- Convert external-pool UI, DTO and configuration names toward account terminology.

## Next

- Split scheduler primitives from Kiro credential types before deleting Kiro modules.
- Convert body and protocol paths to canonical/upstream-account logic with no Kiro envelope or Kiro event dependency.

## Deferred

- Archive or rewrite old Kiro-focused planning documents after the implementation has landed and the new target has evidence.
