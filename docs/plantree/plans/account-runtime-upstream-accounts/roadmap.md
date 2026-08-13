# Account Runtime Upstream Accounts Roadmap

Role: Durable state for the current Rust Kiro-removal refactor

Status: Planning

Authority: Tracks readiness and execution order for the current-repository target only

As of: 2026-08-14

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
- Account-only `/cc/v1/messages` routing now works through configured upstream accounts when no `KiroProvider` is installed, covering normalized stream and non-stream requests.
- Account eligibility and availability checks now honor request body mode, so raw passthrough and normalized account routes are scheduled against compatible accounts only.
- Startup no longer installs the legacy `KiroProvider` when upstream account runtime is enabled, and missing legacy credential files do not block account-only deployments.
- Admin service construction accepts an absent legacy provider; old credential/model-test actions fail explicitly in that mode while account management and usage surfaces can remain mounted.
- `/api/admin/accounts` now has account-named handlers, service wrappers and DTOs; list/status responses expose `accounts` while legacy `/external-pools` remains compatibility-only.
- The upstream account page consumes the account-shaped responses through the account API client, with temporary normalization for older `pools` payloads.
- Runtime config now exposes `accountRuntime` as the account-policy field, accepts it on update, mirrors legacy `externalPools` for compatibility, and has frontend normalization so old and new callers stay consistent.
- Usage admin queries now accept `accountId` and `routeKind=account`, and the dashboard exposes `/usage-dashboard/account-billing` plus `/usage-dashboard/account-risk` while legacy external-pool paths remain compatibility aliases.
- Usage and risk UI entry points now use account terminology and account-named hooks/API functions for upstream account filters and risk views.
- `AccountRuntimeManager` is now the injection boundary for App state, Anthropic router dependencies, Admin service dependencies and process lifecycle wiring. It currently aliases the legacy external-pool manager so the scheduler/transport internals can be migrated behind an account runtime facade in smaller verified steps.

## In Progress

- Migrate account runtime internals away from legacy external-pool names behind the `account_runtime` facade while preserving current scheduler, proxy, body-mode, retry, usage projection and compatibility behavior.
- Convert remaining backend Admin/API/DTO names from external-pool/local-credential terminology toward account terminology while preserving temporary compatibility aliases only where existing clients still need them.

## Next

- Split scheduler primitives from Kiro credential types before deleting Kiro modules.
- Convert body and protocol paths to canonical/upstream-account logic with no Kiro envelope or Kiro event dependency.

## Deferred

- Archive or rewrite old Kiro-focused planning documents after the implementation has landed and the new target has evidence.
