# Account Runtime Upstream Accounts

Role: Current Rust refactor plan for removing Kiro concepts and making upstream accounts the scheduling unit

Status: In Progress; implementation branch created

Authority: Defines the target boundary for the current-repository refactor requested after `feature/usage-correction-cost-floor`

As of: 2026-08-14

Related: [Plan Tree](../../README.md), [current module map](../../baseline/module-map.md), [protocol contracts](../../baseline/protocol-and-api-contracts.md), [runtime flows](../../baseline/runtime-flows.md), [target architecture](topics/final-target-plan.md)

## Purpose

Turn the current Rust service into a Kiro-independent account scheduling and usage shaping runtime.

The target system has no Kiro account, Kiro provider, Kiro upstream protocol, Kiro endpoint, Kiro credential refresh, Kiro model/quota/subscription, Kiro EventStream, or Kiro body-envelope concept. Current Kiro-named code may be mined only for generic scheduler, lease, usage, body, stream, and observability behavior. It must not remain as product terminology or runtime dependency in the target implementation.

## Final Target

The final codebase should operate around upstream accounts:

```text
client Anthropic/Claude-compatible request
  -> protocol entry
  -> body, model and capability processing
  -> account runtime and scheduler
  -> selected upstream account HTTP execution
  -> stream/non-stream response shaping
  -> raw, effective and reported usage
  -> statistics, Admin and observability
```

The former external pool concept becomes the account concept. There is no local-versus-external pool split in the target architecture. If accounts later need grouping, groups are configuration filters and UI organization only; they are not a scheduler boundary.

## Required Git Start Sequence

Implementation must not start on the current feature branch.

1. Commit the current `feature/usage-correction-cost-floor` work, including this plan.
2. Fast-forward the default mainline branch. `origin/HEAD` currently points to `origin/master`, so `master` is the mainline unless the remote default changes before execution.
3. Merge `feature/usage-correction-cost-floor` into `master`.
4. Create a new implementation branch from the updated `master`, preferably `feature/account-runtime-upstream-accounts`.
5. Implement the target refactor only on that new branch.

## Plan Documents

- [Final target plan](topics/final-target-plan.md): scope, deletion boundary, retained generic behavior, module shape, implementation order, and acceptance checks.

## Current State

The implementation branch `feature/account-runtime-upstream-accounts` has been created from the updated `master`.

Initial neutral `account_runtime` domain types have landed as the first code boundary:

- upstream account identity, auth policy, proxy and limits;
- account attempt trace, delivery evidence and normalized upstream error classes;
- raw usage facts with explicit confidence;
- a minimal dispatch candidate/selection primitive for later scheduler extraction.
- a migration bridge from the old `ExternalPool` record into the new `UpstreamAccount` boundary, proving the former external pool can be treated as an upstream account without exposing Kiro concepts.
- `/api/admin/accounts` aliases for the existing external-pool Admin handlers and matching frontend account API aliases, giving new callers an account-named boundary while the old UI is migrated.
- the Admin UI resource navigation now exposes the upstream account page at `/accounts`, with the old `/external-pools` route redirecting to it.
- the upstream account page now calls the account-named Admin API aliases for list, status, create, update, enable, cooldown, supported-model discovery, delete and test actions.
- `/cc/v1/messages` can now run through configured upstream accounts when no `KiroProvider` is installed, for both stream and non-stream normalized requests.
- account selection now honors the configured request body mode during eligibility and immediate-availability checks, so raw-preparse routing does not steal normalized-only accounts and normalized routing does not select raw-only accounts.
- startup can now run without installing a legacy `KiroProvider` when the upstream account runtime is enabled, so missing legacy credential files do not block account-only deployments.
- Admin service dependencies now treat the legacy provider as optional; old credential/model-test endpoints return explicit compatibility errors when the provider is absent while account management remains available.
- `/api/admin/accounts` now has account-named handlers, service methods and response DTOs. List and status responses expose `accounts`, while legacy `/api/admin/external-pools` remains as a compatibility surface with `pools`.
- the upstream account page now consumes the account-shaped `accounts` responses, with frontend compatibility normalization retained for older server responses during migration.
- runtime config now exposes and accepts `accountRuntime` as the account policy surface while mirroring the old `externalPools` field for compatibility; the frontend normalizes both names and the upstream account page writes the account-named field.
- usage query/admin dashboard surfaces now accept account-named filters and paths: `accountId`, `routeKind=account`, `/usage-dashboard/account-billing`, and `/usage-dashboard/account-risk`, with frontend usage and risk pages moved to account terminology while old external-pool paths remain compatibility aliases.
- App state, Anthropic router dependencies, Admin service dependencies and process wiring now inject `AccountRuntimeManager` from the `account_runtime` boundary. The current manager facade still delegates to the legacy external-pool implementation, but new integration points no longer depend on `ExternalPoolManager` as their primary type.
- `/api/admin/accounts` now uses account-named wire DTOs for list/status/mutation/test responses. The JSON field shape remains compatible with the current upstream account page, while legacy `/api/admin/external-pools` keeps its old pool DTOs.
- `AccountRuntimeConfig` now fronts the legacy external-pool config structure at new integration boundaries. App state, Anthropic router config, request-runtime config, Admin runtime-config DTOs and runtime reload invalidation use account runtime terminology while persisted `external_pools` storage and compatibility JSON remain mirrored.
- The Admin frontend account page has moved from `features/external-pools` to `features/accounts`. Its page, modal, test modal, helper components and utilities now use account component names, while `/external-pools` remains only a redirect compatibility route.
- The frontend account runtime boundary now has `AccountRuntimeConfig`, `defaultAccountRuntimeConfig` and `DiscoverAccountSupportedModelsRequest` aliases. New account UI/API code uses those account-named types while legacy `ExternalPoolsConfig` and discovery request names remain only for compatibility functions and mirrored wire fields.
- `account_runtime::migration` now owns the temporary projection from legacy `ExternalPool` records into `UpstreamAccount`. The old `ExternalPool::to_upstream_account` method was removed so the account runtime boundary, not the legacy record, owns the migration bridge.
- Admin `/accounts` request surfaces now have account-owned Rust DTOs for create, update, enabled toggles, supported-model discovery and test calls. They preserve the same camelCase JSON shape and explicitly convert into the legacy external-pool storage DTOs only at the compatibility boundary.
- Account runtime manager callers now use account-named cache/data invalidation wrappers for runtime config reloads, local account mutations and cross-instance data events. Legacy external-pool method names remain only inside the delegated implementation while Redis channel and storage compatibility keys are still unchanged.
- Top-level runtime configuration now has account-runtime accessors. New startup, router, Admin runtime-config and reload-invalidation call sites use `account_runtime_config()` / `set_account_runtime_config()` while the persisted `external_pools` field remains the compatibility storage mirror.
- Request-state Anthropic wiring now names the injected account policy as `account_runtime` in `AppState` and `RequestRuntimeConfig`; raw direct, preflight and normalized fallback entrypoints read the account-named field while still delegating to the legacy implementation internals.
- Admin account service methods now route list/status/discovery/test/cache-invalidation through account-named bridge methods. The bridge still uses the current external-pool storage/cache keys as compatibility details, but `/accounts` service methods no longer call the legacy public external-pool service methods directly.
- `AccountRuntimeConfigExt` now provides account-named semantic accessors for enabled route checks, dispatch wait, legacy local-rescue wait, account request timeout and usage cost-floor settings. New Admin/request-entry/route-gate call sites use those methods instead of reading legacy field names directly.
- Runtime config validation now enters through `validate_account_runtime_config`, and the old private `validate_external_pools_config` helper was removed. Tests use account-runtime names while still asserting the current compatibility JSON field names.
- `account_runtime` now exports `UpstreamAccountStorageRecord` and `UpstreamAccountStatusRecord` aliases. Admin account bridge methods use those account-named record types while legacy storage/cache keys remain an internal compatibility detail.
- `account_runtime::store` now provides account-named storage bridge methods for listing, loading, creating, updating, deleting, enabling, supported-model updates and auto-disable clearing. `/api/admin/accounts` service methods use those bridge methods while legacy `/external-pools` methods keep their old compatibility calls.
- `account_runtime` now exposes account-named status, cooldown and upstream URL helpers. Account status, account cooldown clearing, account supported-model discovery and account tests enter through those helpers and return account-facing Admin messages instead of external-pool wording.
- Anthropic request entry and handler code now uses account-runtime aliases for account route requests, route preparation cache, request body mode, forward outcome, final route error and latency trace. Raw direct/preflight helper names now use account-route terminology while the underlying legacy implementation remains behind the facade.
- `account_runtime` now exposes account-named manager wrappers for eligibility, immediate availability, direct route reason and failover execution. Anthropic handler code enters through those wrappers instead of directly calling legacy pool-named manager methods.
- `account_runtime` now exports account-named Admin/runtime enum aliases for account auth, usage projection, stream response, raw model, auto-disable, retry, model mapping and route mode. New Account DTOs and account discovery/test service paths use these aliases, while legacy `/external-pools` DTOs keep their compatibility type names.
- `/api/admin/usage-dashboard/account-billing` now returns an account-shaped billing response with `accountBillingByAccount` rows containing `accountId` and `accountName`. The old `/external-pool-billing` response remains unchanged, and the Admin UI consumes the account-shaped field with a temporary fallback for older responses.
- `/api/admin/usage-dashboard/account-risk` now returns an account-shaped risk response with `byAccount`, `accountId`, `accountName`, `accountBillingPresent` and `missingAccountBillingRecords`. The old `/external-pool-risk` response remains unchanged, and the Admin UI consumes the account-shaped response while retaining fallback normalization for older responses.

Last verified on 2026-08-14:

- `feature/tests/run-cargo-scoped.sh account-runtime-initial-test2 -- cargo test account_runtime`
- `feature/tests/run-cargo-scoped.sh account-runtime-initial-check -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-bridge-test2 -- cargo test external_pool_projects_to_upstream_account_boundary`
- `feature/tests/run-cargo-scoped.sh account-bridge-check2 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-only-normalized-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-body-mode-test2 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-only-slice-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-only-slice-check2 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-startup-decision-test2 -- cargo test legacy_provider_is_not_required_when_upstream_accounts_are_enabled -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-startup-account-only-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-startup-fmt3 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-startup-check2 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-admin-boundary-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-admin-boundary-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-usage-api-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-usage-api-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-usage-api-test2 -- cargo test usage_records_query_accepts_account_aliases -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-facade-fmt3 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-facade-check3 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-runtime-facade-test1 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-admin-dto-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-admin-dto-check3 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-admin-dto-test1 -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-facade-fmt3 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-facade-check3 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-facade-test2 -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-facade-test3 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `pnpm --dir ui check`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-migration-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-migration-test2 -- cargo test account_runtime -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-migration-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-admin-dto-split-fmt5 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-admin-dto-split-check3 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-admin-dto-split-test1 -- cargo test account_request_dtos_deserialize_and_convert_to_storage_compat_requests -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-admin-dto-split-test2 -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-wrapper-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-wrapper-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-runtime-wrapper-test1 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-accessor-fmt3 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-accessor-check2 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-accessor-test1 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-accessor-test2 -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh request-runtime-account-field-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh request-runtime-account-field-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh request-runtime-account-field-test4 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh request-runtime-account-field-test3a -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `feature/tests/run-cargo-scoped.sh request-runtime-account-field-test5 -- cargo test account_runtime_endpoint_gate_applies_global_enable_and_route_policy -- --nocapture`
- `feature/tests/run-cargo-scoped.sh admin-account-bridge-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh admin-account-bridge-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh admin-account-bridge-test1 -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `feature/tests/run-cargo-scoped.sh admin-account-bridge-test2 -- cargo test account_request_dtos_deserialize_and_convert_to_storage_compat_requests -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-ext-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-ext-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-ext-test1 -- cargo test account_runtime_config_ext_applies_enablement_and_route_policy -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-ext-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-config-ext-test3 -- cargo test account_request_dtos_deserialize_and_convert_to_storage_compat_requests -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-validation-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-runtime-validation-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-runtime-validation-test1 -- cargo test account_runtime_ -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-record-alias-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-record-alias-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-record-alias-test1 -- cargo test account_runtime_ -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-record-alias-test2 -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-store-bridge-fmt4 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-store-bridge-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-store-bridge-test1b -- cargo test account_request_dtos_deserialize_and_convert_to_storage_compat_requests -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-store-bridge-test2b -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-store-bridge-test3 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-route-alias-fmt5 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-route-alias-check3 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-route-alias-test1b -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-alias-test2 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-manager-wrapper-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-manager-wrapper-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-manager-wrapper-test1 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-manager-wrapper-test2 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-dto-alias-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-dto-alias-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-dto-alias-test1 -- cargo test account_request_dtos_deserialize_and_convert_to_storage_compat_requests -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-dto-alias-test2 -- cargo test account_status_response_serializes_account_boundary_names -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-billing-response-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-billing-response-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-billing-response-test1 -- cargo test account_billing_response_serializes_account_boundary_fields -- --nocapture`
- `pnpm --dir ui check`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-risk-response-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-risk-response-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-risk-response-test1 -- cargo test account_risk_response_serializes_account_boundary_fields -- --nocapture`
- `pnpm --dir ui check`
- `git diff --check`
