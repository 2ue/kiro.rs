# Account Runtime Upstream Accounts

Role: Current Rust refactor plan for removing Kiro concepts and making upstream accounts the scheduling unit

Status: In Progress; implementation branch created

Authority: Defines the target boundary for the current-repository refactor requested after `feature/usage-correction-cost-floor`

As of: 2026-08-15

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
- `/cc/v1/messages` can now run through configured upstream accounts when no local-upstream provider is installed, for both stream and non-stream normalized requests.
- account selection now honors the configured request body mode during eligibility and immediate-availability checks, so raw-preparse routing does not steal normalized-only accounts and normalized routing does not select raw-only accounts.
- startup can now run without installing the legacy credential provider when the upstream account runtime is enabled, so missing legacy credential files do not block account-only deployments.
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
- The Admin UI risk page now lives under `features/account-risk`, navigation points to `/account-risk`, and the old `/external-pool-risk` UI path redirects to the account risk page as a compatibility route.
- Inference attempt budgeting now has an `Account` attempt kind. Real upstream account sends reserve that kind, while the old `ExternalPool` kind is retained only as a compatibility alias into the same counter until snapshot fields and legacy paths are migrated.
- Inference attempt snapshots now expose `accountAttempts` as the primary account send counter while still serializing `externalAttempts` as a compatibility copy. Rust deserialization maps old snapshots that only contain `externalAttempts` into the account counter, and both maintained UIs display local/account/MCP breakdowns.
- Usage records now double-write account-facing upstream account fields: `accountId`, `accountName` and `accountAttempts` are primary JSON fields, while `externalPoolId`, `externalPoolName` and `externalAttempts` remain compatibility copies. The maintained usage UIs prefer account fields, fall back to old records, and no longer display external-pool wording in usage record/detail/billing views.
- New upstream account usage records now write `routeKind: "account"` as the primary route kind. Query, Redis summary, Postgres rollup/dashboard and maintained usage UIs treat `account` and historical `external_pool` route kinds as the same upstream-account class while preserving the old value only for stored-record/filter compatibility.
- Usage records now expose `accountBilling` as the primary upstream-account billing detail while retaining `externalPoolBilling` as a compatibility copy. Recorder/storage write paths fill both fields, historical records are normalized on read, and maintained usage detail/cost UI reads account billing first with fallback to old records.
- Usage summary and dashboard window summary now expose `accountBilling` and `accountBillingByAccount` as account-facing aggregate fields while retaining `externalPoolBilling` and `externalPoolBillingByPool` for compatibility. Redis/Postgres dashboard materialization fills both shapes, and the maintained overview UI reads the account fields first.
- Usage record, summary, dashboard and credential usage APIs now expose account-neutral `upstreamMeteringUnits` / `totalUpstreamMeteringUnits` fields while retaining old `kiroMeteringUsage` / `totalKiroMeteringUsage` compatibility copies. Recorder, Redis and Postgres read/write boundaries normalize old-only and new-only metering records into both fields, and maintained usage UI surfaces now show "上游计量" instead of Kiro metering wording.
- Stream and handler code now pass upstream metering values through `upstream_metering_units` naming. The old Kiro-named field remains only where a usage record writes the legacy compatibility copy, and downstream SSE still excludes both upstream and compatibility metering fields.
- New upstream account usage records now write account-named route subtypes (`account_fallback_preflight`, `account_fallback_after_local_attempts`, `account_direct_policy`, `account_error`, `local_rescue_after_account`). Legacy `external_*` subtype values remain deserializable and accepted by compatibility route checks, and maintained UIs label both old and new values as upstream-account routes.
- Anthropic handler fallback routing now uses `AccountFallbackContext` and account-named local-preflight/fallback helper methods. The old external-pool terminology remains only in compatibility subtype values, legacy usage fields and delegated storage/runtime internals, and local rescue preflight metadata now double-writes account fields with old `external*` copies.
- Account local-rescue decisions now enter account-named runtime config accessors and write account-named fallback reasons (`account_rate_limit`, `account_timeout`, `account_capacity`, `account_bad_request`, `account_error`). Request-entry dispatch deadlines and pre-body rejection logs also use account runtime terminology while persisted config fields remain compatibility storage details.
- Anthropic router/AppState/request runtime now names the account-route payload guard switch as `payload_guard_account_enabled`; it still reads and writes the persisted `payloadGuardExternalEnabled` compatibility field when crossing existing config and legacy route boundaries. Maintained runtime UIs now describe the control as upstream-account payload shaping rather than Kiro/external-pool payload handling.
- Maintained runtime configuration UIs now label the old external-pool runtime section as upstream-account routing, including routing mode/rules, retry/failover/cooldown, local rescue, usage diagnostics, prompt steering and payload/body shaping text. Compatibility state keys such as `externalPools` and `payloadGuardExternalEnabled` remain unchanged internally.
- Local parsed body planning now uses local-upstream type names (`LocalUpstreamConverterPlan`, `LocalUpstreamBodyPlan`, `PreparedLocalUpstreamBody`) and local-upstream conversion logs. The concrete legacy `KiroRequest` payload type remains at the current local-provider boundary until the provider/body implementation is replaced.
- Account-route body capability planning now uses account body plan names (`AccountBodyPlan`, `AccountBodyBytesPlan`, `AccountRaw`, `AccountNormalized`). The delegated legacy route executor still lives in `external_pool`, but body processing decisions no longer expose external-pool names at the capability-plan boundary.
- Anthropic upstream error-envelope helpers now use account-neutral official-upstream naming (`official_upstream_public_message` / `official_upstream_public_error`) while retaining the existing sensitive/internal-term filtering behavior.
- Payload guard runtime wrappers now use local-upstream/account names (`PreparedLocalUpstreamRequestBody`, `prepare_local_upstream_request_body`, `PreparedAccountMessagesPayload`, `prepare_account_messages_payload`, `sanitize_anthropic_messages_for_account_forwarding`). The underlying legacy local payload still uses `KiroRequest` until the provider/body implementation is replaced.
- Anthropic handler call sites now enter local-upstream payload guard wrappers (`guard_local_upstream_request` / `serialize_local_upstream_request`) instead of calling legacy Kiro-named guard functions directly. The old guard functions remain inside the concrete legacy local payload implementation while request handling, cache-point retry and thinking-signature retry use local-upstream naming.
- Local-upstream payload diagnostics now enter through `breakdown_local_upstream_request` and `diagnose_local_upstream_tool_use_format`, so handler and local body pipeline diagnostics no longer call Kiro-named payload breakdown/tool-format helpers directly.
- Account-route body/model/retry pipeline diagnostics now use account wording for normalized/raw payload guard, model rewrite, model mapping and model cooldown errors while the delegated executor types remain under the legacy `external_pool` module.
- `PreparedLocalUpstreamBody` now exposes its typed local body as `local_upstream_request` instead of `kiro_request`, and the local-upstream conversion/diagnostic helper parameters use local-upstream naming at that boundary. The concrete payload type remains the legacy `KiroRequest` until the provider/body implementation is replaced.
- Account-route model processing helpers now use account names (`account_outbound_model_for_raw`, `process_account_model`, `account_model_processing_error`) inside the delegated legacy executor.
- Account-route retry helpers now use same-account/cross-account names and write `retry_same_account` attempt actions while compatibility config fields such as `external_pool_same_pool_*` remain unchanged.
- Retry behavior tests now use same-account/cross-account names for the core retry/failover cases while keeping compatibility config field names in fixtures.
- Runtime config now exposes account-named retry status accessors (`same_account_retry_status_codes`, `cross_account_retry_status_codes`) and the retry pipeline uses them. Old accessor names remain as compatibility delegates.
- Account runtime facade comments and upstream-account integration-test skip messages no longer present the migrated runtime as an external-pool feature.
- Proxy warning responses now write `x-account-runtime-warnings` as the primary header while also writing the old `x-kiro-rs-warnings` compatibility copy. Code comments and maintained Admin UI text describe the account-runtime header.
- JSON stream error-envelope diagnostics now avoid retaining complete provider JSON error messages in usage raw-body fields. Usage keeps shape/fingerprint metadata for these envelopes, and remaining malformed/incomplete raw upstream snippets use the neutral `official_upstream` source label instead of a Kiro-specific source.
- Anthropic handler runtime log/comment text for local-upstream cache-point retries, payload guard diagnostics, tool-use rejection diagnostics, slow interaction diagnostics and stream/non-stream retry paths now uses local-upstream/account wording rather than Kiro product wording. The remaining handler Kiro names are type/module compatibility boundaries.
- Stream conversion now exposes `process_local_upstream_event` as the handler-facing event processor. The old concrete `process_kiro_event` method remains inside the stream module for the current legacy event type and stream tests, while runtime handler code enters through the local-upstream wrapper.
- Anthropic `AppState`, router dependencies, request-entry flow and handler tests now use `local_upstream_provider` / `with_local_upstream_provider` for the optional legacy local upstream executor. The underlying concrete type is still the legacy provider until the provider implementation is replaced, but the Claude/Anthropic protocol boundary no longer exposes a Kiro-named provider field.
- Anthropic converter module docs, diagnostics, collision errors and compatibility comments now use local-upstream/upstream-safe wording instead of presenting the request conversion as a Kiro protocol surface. The concrete legacy request type remains isolated behind the current local-upstream body boundary until the provider/body implementation is replaced.
- The model capability seed/source boundary now uses upstream-account terminology. New seed/status writes use `upstream-model-seed` and `upstream-account-model-catalog`, old `kiro-*` source values are normalized on read, and main/Admin model capability sync call the account-neutral `sync_from_upstream_catalog` entrypoint while the legacy provider method remains a compatibility delegate.
- Native WebSearch MCP routing now enters through local-auxiliary-upstream names in `websearch.rs`: provider/error helper wrappers, MCP call helper names and runtime comments no longer present WebSearch as a Kiro MCP surface. The concrete legacy provider type and timeout config field remain compatibility details behind the wrapper.
- `local_upstream` now provides a compatibility facade for the current legacy local provider/call-trace response types. Anthropic router, middleware, handler and WebSearch boundaries import local-upstream aliases instead of directly depending on legacy provider type names, while the underlying implementation remains unchanged.
- The `local_upstream` facade now also exposes request/event aliases for local-upstream request bodies, tool-use diagnostics and metadata usage. `payload_guard_runtime`, `tool_format_debug`, `cache` and native reasoning comments use those aliases/neutral wording instead of importing legacy request/event types directly at those small boundaries.
- Anthropic handler stream/body code now imports local-upstream request, event, metadata usage and EventStream decoder aliases from the `local_upstream` facade. Handler-facing retry plans, payload guard retries, non-stream event decoding, stream latency classification and stream retry state use local-upstream names, while concrete legacy request/event/decoder types remain isolated behind the facade.
- Anthropic stream conversion and debug helpers now import local-upstream event aliases instead of legacy event types, and stream tests call the local-upstream event processor as the primary entrypoint. Concrete event fixture structs in tests still come from the legacy implementation until the event model itself is replaced.
- Local-provider raw upstream error diagnostics now use neutral `official_upstream` source labels and redacted body metadata for provider status/non-eventstream bodies, preserving body size, content type, status and a short fingerprint without copying provider error messages into attempt/usage diagnostics.
- `Config` now exposes local-upstream accessors for response timeout, stream retry and cache-point compatibility settings. Anthropic router/AppState/request runtime/converter/Admin model-test/WebSearch call sites use local-upstream field names or accessors while persisted config and Admin runtime DTO compatibility fields remain unchanged.
- Payload guard reports now serialize local-upstream cache-point diagnostic fields (`localUpstreamCachePointsPlanned` / `localUpstreamCachePointsInserted`) while still accepting legacy Kiro-named JSON fields as read aliases.
- Local body preparation now receives upstream reasoning capability state through an upstream-named alias, and its test request fixtures use local-upstream request aliases instead of importing legacy request config types directly.
- Converter tool-use/tool-result pairing now imports local-upstream request aliases for local conversation messages and tool results instead of importing legacy request model paths directly.
- Converter body, history, tool and model modules now import local-upstream request aliases for conversation state, images, tools, tool results and native reasoning request fields. Converter model logic uses upstream reasoning aliases, leaving the concrete legacy request types behind the `local_upstream` facade.
- Usage attempt-chain recording now imports local-upstream call-trace aliases for local credential attempts and attempt-chain summaries instead of depending on the legacy call-trace path directly.
- Handler local dispatch policy and request-entry fast-fail code now use local-upstream dispatch/route-state aliases for acquire modes and local route-state kinds instead of importing token-manager types directly.
- Model capability catalog ingestion now uses local-upstream model catalog aliases for available models, cohort keys and test token-limit fixtures instead of importing legacy available-model types directly.
- Payload guard production code and local payload fixtures now use local-upstream request aliases for request bodies, images, tools, tool results and native reasoning config instead of importing legacy request model paths directly.
- Anthropic stream tests now construct local-upstream event fixtures through the `local_upstream` event facade, including metering/code/invalid-state aliases, so `src/anthropic` no longer imports legacy event paths directly.
- Admin service local model-test request construction and EventStream response parsing now import local-upstream request/event/decoder aliases instead of direct legacy request, event and decoder paths. The old request-module re-exports remain only as legacy compatibility exports.
- Model capability cohort fencing now exposes `UpstreamReasoningCohortContractMatch`; startup recovery decisions and model capability tests no longer use the old Kiro-named contract-match type for upstream reasoning readiness.
- Main process wiring now constructs and passes the optional local executor as `local_upstream_provider` using the local-upstream provider alias. The remaining Admin `kiro_provider` field is an unmigrated compatibility service boundary, not the process-level provider name.
- Admin service dependencies and internal provider state now use `local_upstream_provider` and `LocalUpstreamProvider`, removing the old provider field/type name from main/Admin wiring while legacy credential operations remain behind that local-upstream executor.
- Removed the unmounted `src/test.rs` manual stream caller, which was a Kiro-specific scratch entrypoint and not part of the compiled scheduler/proxy/usage runtime.
- Account-route request state now stores local auxiliary attempt traces as `LocalUpstreamCredentialAttempt` through the local-upstream call-trace facade instead of directly naming the legacy credential attempt type.

Last verified on 2026-08-15:

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
- `pnpm --dir ui check`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-attempt-kind-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-attempt-kind-check2 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-attempt-kind-test1 -- cargo test legacy_external_pool_kind_counts_as_account_attempt -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-attempt-kind-test2 -- cargo test counts_channels_without_exceeding_limit_for_five_rounds -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-attempt-kind-test3 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-attempt-snapshot-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-attempt-snapshot-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-attempt-snapshot-test1 -- cargo test account_snapshot_serializes_account_attempts_with_external_compatibility -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-attempt-snapshot-test2 -- cargo test counts_channels_without_exceeding_limit_for_five_rounds -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-attempt-snapshot-test3 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-attempt-snapshot-test4 -- cargo test external_usage_trace_preserves_local_auxiliary_attempts_for_five_rounds -- --nocapture`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `feature/tests/run-cargo-scoped.sh local-upstream-body-plan-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-body-plan-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh local-upstream-body-plan-test1 -- cargo test body_capabilities -- --nocapture`
- `feature/tests/run-cargo-scoped.sh local-upstream-body-plan-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-body-plan-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-body-plan-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-body-plan-test1 -- cargo test body_capabilities -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-body-plan-test2 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `feature/tests/run-cargo-scoped.sh upstream-public-envelope-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh upstream-public-envelope-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh upstream-public-envelope-test1 -- cargo test official_upstream_public_message -- --nocapture`
- `feature/tests/run-cargo-scoped.sh upstream-handler-tests-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh upstream-handler-tests-test1 -- cargo test official_upstream -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-payload-wrapper-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-payload-wrapper-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-payload-wrapper-test1 -- cargo test account_guard -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-payload-wrapper-test2 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-payload-wrapper-test3 -- cargo test disabled_local_guard_serializes_without_report -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-payload-wrapper-test4 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-pipeline-errors-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-pipeline-errors-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-pipeline-errors-test1 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-pipeline-errors-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-model-helper-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-model-helper-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-model-helper-test1 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-retry-helper-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-retry-helper-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-retry-helper-test1 -- cargo test same_account_retry -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-retry-helper-test2 -- cargo test external_pool_same_pool_retry -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-retry-tests-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-retry-tests-test1 -- cargo test account_same_account_retry -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-retry-tests-test2 -- cargo test account_cross_account -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-retry-tests-test3 -- cargo test account_terminal_error -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-retry-config-accessor-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-retry-config-accessor-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-retry-config-accessor-test1 -- cargo test account_same_account_retry_limit_caps_to_one_and_rejects_terminal_errors -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-runtime-text-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-warning-header-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-warning-header-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-warning-header-test1 -- cargo test warnings_header_writes_account_header_with_legacy_copy -- --nocapture`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `feature/tests/run-cargo-scoped.sh local-upstream-guard-wrapper-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-guard-wrapper-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh local-upstream-guard-wrapper-test1b -- cargo test disabled_local_guard_serializes_without_report -- --nocapture`
- `feature/tests/run-cargo-scoped.sh local-upstream-guard-wrapper-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh local-upstream-guard-wrapper-body-tests -- bash -lc 'cargo test thinking_signature_retry_body_removes_only_native_reasoning_five_rounds -- --nocapture && cargo test cache_point_then_signature_retry_never_reintroduces_cache_point_five_rounds -- --nocapture && cargo test payload_guard_then_signature_retry_preserves_actual_trimmed_history_five_rounds -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh json-stream-usage-privacy-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh json-stream-usage-privacy-test1 -- cargo test handler_thinking_signature_retry_rejects_json_error_envelope_for_five_rounds -- --nocapture`
- `feature/tests/run-cargo-scoped.sh json-stream-usage-privacy-test2 -- cargo test signature_retry -- --nocapture`
- `feature/tests/run-cargo-scoped.sh json-stream-usage-privacy-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh local-upstream-diagnostics-wrapper-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-diagnostics-wrapper-test1 -- bash -lc 'cargo check && cargo test payload_guard -- --nocapture && cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-upstream-log-text-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-log-text-check2 -- cargo check`
- `feature/tests/run-cargo-scoped.sh local-upstream-log-text-test1 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh local-upstream-body-field-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-body-field-test1 -- bash -lc 'cargo check && cargo test body_capabilities -- --nocapture && cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-upstream-stream-event-wrapper-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-stream-event-wrapper-test1 -- bash -lc 'cargo check && cargo test stream_success_records_requested_max_tokens_and_downstream_stop_reason -- --nocapture && cargo test test_requested_max_tokens_infers_max_tokens_stop_reason -- --nocapture && cargo test test_context_usage_percentage_uses_catalog_window_for_final_usage -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-upstream-provider-field-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-provider-field-test1 -- bash -lc 'cargo check && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh converter-neutral-text-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh converter-neutral-text-test1 -- bash -lc 'cargo check && cargo test convert_tools_rejects -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh upstream-model-source-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh upstream-model-source-test3 -- bash -lc 'cargo check && cargo test model_capabilities -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-aux-mcp-websearch-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-aux-mcp-websearch-test1 -- bash -lc 'cargo check && cargo test websearch -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-upstream-facade-fmt4 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-facade-test4 -- bash -lc 'cargo check && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture && cargo test websearch -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-upstream-request-alias-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-request-alias-test2 -- bash -lc 'cargo check && cargo test disabled_local_guard_serializes_without_report -- --nocapture && cargo test tool_format_debug -- --nocapture && cargo test metadata_cache -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-upstream-event-alias-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-event-alias-test2 -- bash -lc 'cargo check && cargo test stream_success_records_requested_max_tokens_and_downstream_stop_reason -- --nocapture && cargo test stream_zero_context_and_metadata_record_request_estimate_consistently -- --nocapture && cargo test complete_eventstream_decoder_accepts_only_complete_valid_frames -- --nocapture && cargo test disabled_thinking_suppresses_downstream_thinking_even_with_native_effort_for_five_rounds -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh stream-local-upstream-entry-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh stream-local-upstream-entry-test2 -- bash -lc 'cargo check && cargo test anthropic::stream -- --nocapture && cargo test anthropic::handlers::tests::stream_success_records_requested_max_tokens_and_downstream_stop_reason -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh provider-raw-error-redaction-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh provider-raw-error-redaction-test1 -- bash -lc 'cargo check && cargo test raw_upstream_error -- --nocapture && cargo test provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh provider-raw-error-redaction-stream-regression -- cargo test stream -- --nocapture`
- `feature/tests/run-cargo-scoped.sh local-upstream-runtime-config-accessors-fmt3 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-upstream-runtime-config-accessors-test3 -- bash -lc 'cargo check && cargo test body_capabilities -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture && cargo test anthropic::converter::tests:: -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh payload-report-local-upstream-fields-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh payload-report-local-upstream-fields-test2 -- bash -lc 'cargo check && cargo test payload_guard_report_uses_local_upstream_cache_point_fields_with_legacy_aliases -- --nocapture && cargo test cache_point_plan_inserts_markers_in_serialized_kiro_body -- --nocapture && cargo test payload_guard -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh local-body-upstream-alias-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh local-body-upstream-alias-test2 -- bash -lc 'cargo check && cargo test disabled_thinking_suppresses_downstream_thinking_even_with_native_effort_for_five_rounds -- --nocapture && cargo test adaptive_or_omitted_thinking_with_output_effort_exposes_downstream_thinking_for_five_rounds -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh converter-tool-pairing-alias-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh converter-tool-pairing-alias-test1 -- bash -lc 'cargo check && cargo test test_validate_tool_pairing -- --nocapture && cargo test converted_request_never_copies_duplicate_or_orphan_tool_result_content_into_text -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh converter-request-alias-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh converter-request-alias-test2 -- bash -lc 'cargo check && cargo test native_reasoning_uses_discovered_reasoning_path_and_preserves_max -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh converter-request-alias-test3 -- cargo test anthropic::converter::tests:: -- --nocapture`
- `feature/tests/run-cargo-scoped.sh usage-call-trace-alias-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh usage-call-trace-alias-test1 -- bash -lc 'cargo check && cargo test recorder_search_matches_model_account_session_and_error_text -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh handler-local-dispatch-alias-fmt3 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh handler-local-dispatch-alias-test3 -- bash -lc 'cargo check && cargo test account_fallback -- --nocapture && cargo test preflight_account_error_can_rescue_once_then_attempt_budget_blocks_cycle_five_rounds -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh model-catalog-alias-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh model-catalog-alias-test1 -- bash -lc 'cargo check && cargo test model_capabilities -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh payload-guard-request-alias-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh payload-guard-request-alias-test1 -- bash -lc 'cargo check && cargo test payload_guard -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh stream-event-test-alias-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh stream-event-test-alias-test1 -- bash -lc 'cargo check && cargo test anthropic::stream -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh admin-local-upstream-alias-fmt2 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh admin-local-upstream-alias-test2 -- bash -lc 'cargo check && cargo test admin::service::tests -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh upstream-reasoning-cohort-name-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh upstream-reasoning-cohort-name-test1 -- bash -lc 'cargo check && cargo test model_capabilities -- --nocapture && cargo test native_reasoning_startup_decision_never_blocks_service_for_five_rounds -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh main-local-upstream-provider-name-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh main-local-upstream-provider-name-test1 -- bash -lc 'cargo check && cargo test native_reasoning_startup_decision_never_blocks_service_for_five_rounds -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh admin-provider-field-alias-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh admin-provider-field-alias-test1 -- bash -lc 'cargo check && cargo test admin::service::tests -- --nocapture && cargo test native_reasoning_startup_decision_never_blocks_service_for_five_rounds -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `feature/tests/run-cargo-scoped.sh remove-unused-kiro-scratch-test-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh external-route-local-attempt-alias-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh external-route-local-attempt-alias-test1 -- bash -lc 'cargo check && cargo test external_usage_trace_preserves_local_auxiliary_attempts_for_five_rounds -- --nocapture && cargo test account_fallback -- --nocapture && cargo test account_only_routes_normalized_requests_without_local_upstream_provider -- --nocapture'`
- `node feature/tests/mcp-attempt-channel-contract.mjs`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-usage-record-fields-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-usage-record-fields-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-usage-record-fields-test1b -- cargo test usage_record_serializes_account_attempts_with_external_compatibility -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-usage-record-fields-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-usage-record-fields-test3 -- cargo test external_usage_trace_preserves_local_auxiliary_attempts_for_five_rounds -- --nocapture`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `node feature/tests/mcp-attempt-channel-contract.mjs`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-route-kind-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-route-kind-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-route-kind-test1 -- cargo test route_kind -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-kind-test2 -- cargo test redis_usage_record_query_treats_account_and_external_pool_as_upstream_account -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-kind-test3 -- cargo test postgres_rollup_treats_account_and_legacy_external_pool_as_upstream_account -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-kind-test4 -- cargo test usage_record_serializes_account_attempts_with_external_compatibility -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-kind-test5 -- cargo test usage_records_query_accepts_account_aliases -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-kind-test6 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-kind-test7 -- cargo test external_usage_trace_preserves_local_auxiliary_attempts_for_five_rounds -- --nocapture`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `node feature/tests/mcp-attempt-channel-contract.mjs`
- `feature/tests/run-cargo-scoped.sh account-route-kind-fmt3 -- cargo fmt --check`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh account-billing-field-fmt1 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-billing-field-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-billing-field-test1 -- cargo test usage_record_serializes_account_billing_with_external_compatibility -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-billing-field-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-billing-field-test3 -- cargo test postgres_rollup_treats_account_and_legacy_external_pool_as_upstream_account -- --nocapture`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `feature/tests/run-cargo-scoped.sh account-billing-field-fmt2 -- cargo fmt --check`
- `node feature/tests/mcp-attempt-channel-contract.mjs`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh --reap-stale`
- `feature/tests/run-cargo-scoped.sh account-summary-field-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-summary-field-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-summary-field-test1 -- cargo test usage_summaries_serialize_account_billing_with_external_compatibility -- --nocapture`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `feature/tests/run-cargo-scoped.sh account-summary-field-fmt3 -- cargo fmt --check`
- `node feature/tests/mcp-attempt-channel-contract.mjs`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh --reap-stale`
- `feature/tests/run-cargo-scoped.sh upstream-metering-field-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh upstream-metering-field-check2 -- cargo check`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `feature/tests/run-cargo-scoped.sh upstream-metering-field-test2 -- cargo test usage_metering_fields_serialize_upstream_with_kiro_compatibility -- --nocapture`
- `feature/tests/run-cargo-scoped.sh upstream-metering-field-test3 -- cargo test usage_summaries_serialize_account_billing_with_external_compatibility -- --nocapture`
- `feature/tests/run-cargo-scoped.sh upstream-metering-field-fmt3 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh upstream-metering-field-check3 -- cargo check`
- `feature/tests/run-cargo-scoped.sh upstream-metering-field-test4 -- cargo test dashboard_window_basic_fallback_preserves_core_series_metrics -- --nocapture`
- `git diff --check`
- `feature/tests/run-cargo-scoped.sh --reap-stale`
- `feature/tests/run-cargo-scoped.sh upstream-metering-internal-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh upstream-metering-internal-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh upstream-metering-internal-test1 -- cargo test test_metering_event_is_recorded_but_not_emitted_downstream -- --nocapture`
- `feature/tests/run-cargo-scoped.sh upstream-metering-internal-test3 -- cargo test handler_legacy_metadata_metering_and_complete_tool_are_trusted_terminals_for_five_rounds -- --nocapture`
- `feature/tests/run-cargo-scoped.sh upstream-metering-internal-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-test1 -- cargo test usage_route_subtype_serializes_account_values_with_external_compatibility -- --nocapture`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-test3 -- cargo test normalized_external_direct_policy_skips_raw_preparse_without_raw_pool -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-test4 -- cargo test fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-test5 -- cargo test preflight_external_error_can_rescue_once_then_attempt_budget_blocks_cycle_five_rounds -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-route-subtype-fmt2 -- cargo fmt --check`
- `feature/tests/run-cargo-scoped.sh account-handler-fallback-rename-fmt3 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-handler-fallback-rename-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-handler-fallback-rename-test1 -- cargo test account_fallback -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-handler-fallback-rename-test2 -- cargo test native_websearch_runs_local_pool_preflight_before_mcp_intercept -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-handler-fallback-rename-test3 -- cargo test direct_account_policy_resolves_model_before_route_request -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-handler-fallback-rename-test4 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-local-rescue-reason-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-local-rescue-reason-check2 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-local-rescue-reason-test1 -- cargo test account_fallback -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-local-rescue-reason-test2 -- cargo test preflight_account_error_can_rescue_once_then_attempt_budget_blocks_cycle_five_rounds -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-local-rescue-reason-test3 -- cargo test account_runtime_config_ext_applies_enablement_and_route_policy -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-local-rescue-reason-test4 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-payload-guard-runtime-fmt1 -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh account-payload-guard-runtime-check1 -- cargo check`
- `feature/tests/run-cargo-scoped.sh account-payload-guard-runtime-test1 -- cargo test payload_guard -- --nocapture`
- `feature/tests/run-cargo-scoped.sh account-payload-guard-runtime-test2 -- cargo test account_only_routes_normalized_requests_without_kiro_provider -- --nocapture`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
- `pnpm --dir ui check`
- `pnpm --dir admin-ui exec tsc -b --pretty false`
