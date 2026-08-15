# Account Runtime Upstream Accounts Roadmap

Role: Durable state for the current Rust Kiro-removal refactor

Status: In Progress

Authority: Tracks readiness and execution order for the current-repository target only

As of: 2026-08-15

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
- Account-only `/cc/v1/messages` routing now works through configured upstream accounts when no local-upstream provider is installed, covering normalized stream and non-stream requests.
- Account eligibility and availability checks now honor request body mode, so raw passthrough and normalized account routes are scheduled against compatible accounts only.
- Startup no longer installs the legacy credential provider when upstream account runtime is enabled, and missing legacy credential files do not block account-only deployments.
- Admin service construction accepts an absent legacy provider; old credential/model-test actions fail explicitly in that mode while account management and usage surfaces can remain mounted.
- `/api/admin/accounts` now has account-named handlers, service wrappers and DTOs; list/status responses expose `accounts` while legacy `/external-pools` remains compatibility-only.
- The upstream account page consumes the account-shaped responses through the account API client, with temporary normalization for older `pools` payloads.
- Runtime config now exposes `accountRuntime` as the account-policy field, accepts it on update, mirrors legacy `externalPools` for compatibility, and has frontend normalization so old and new callers stay consistent.
- Usage admin queries now accept `accountId` and `routeKind=account`, and the dashboard exposes `/usage-dashboard/account-billing` plus `/usage-dashboard/account-risk` while legacy external-pool paths remain compatibility aliases.
- Usage and risk UI entry points now use account terminology and account-named hooks/API functions for upstream account filters and risk views.
- `AccountRuntimeManager` is now the injection boundary for App state, Anthropic router dependencies, Admin service dependencies and process lifecycle wiring. It currently aliases the legacy external-pool manager so the scheduler/transport internals can be migrated behind an account runtime facade in smaller verified steps.
- Account Admin responses now have account-named DTOs for list/status/mutation/test surfaces, with a focused serialization test proving `/accounts` uses `accounts/account` fields rather than legacy `pools/pool` wrappers.
- `AccountRuntimeConfig` is now the config facade for new runtime/Admin/router/handler boundaries, while old persisted `external_pools` fields remain as the compatibility storage and wire mirror.
- The Admin account feature directory has moved from `features/external-pools` to `features/accounts`; account page, form modal, test modal, components and utilities now use account component/file names, with `/external-pools` kept as a redirect.
- Frontend account runtime config and supported-model discovery now have account-named type/default aliases, so the account page and new account API helpers no longer depend directly on `ExternalPoolsConfig` or external-pool discovery request names.
- Legacy `ExternalPool` to `UpstreamAccount` projection now lives in `account_runtime::migration`; the old `ExternalPool::to_upstream_account` method and old-module bridge test were removed.
- Admin `/accounts` request handlers and service methods now use account-owned Rust DTOs for create/update/enabled/supported-model discovery/test, converting to legacy storage DTOs only at the current compatibility boundary.
- Account runtime manager callers now use account-named wrappers for runtime-policy invalidation, local account mutation notifications and cross-instance data event observation; old external-pool method names are confined to the delegated implementation.
- Top-level runtime configuration now exposes account-runtime accessors, and new startup, router, Admin runtime-config and reload-invalidation call sites use those accessors instead of direct `Config.external_pools` access while compatibility storage remains mirrored.
- Anthropic request-state wiring now uses `account_runtime` fields in `AppState` and `RequestRuntimeConfig`, so raw direct, preflight and normalized fallback entrypoints no longer receive the account policy through an external-pool-named request field.
- Admin account service methods now use account-named bridge methods for list, status, supported-model discovery, test and cache/data invalidation instead of directly calling legacy external-pool service methods. Storage and cache keys remain compatibility details for now.
- `AccountRuntimeConfigExt` now gives new Admin/request-entry/route-gate code account-named accessors for enablement, route policy, dispatch wait, request timeout and usage cost-floor settings while the legacy config structure remains the temporary storage mirror.
- Runtime config validation now uses `validate_account_runtime_config`; the old private external-pool validation helper was removed, and validation tests use account-runtime names while compatibility JSON field assertions remain explicit.
- `account_runtime` now exports account-named storage/status record aliases, and Admin account bridge signatures use them so new `/accounts` service code does not expose legacy record names at its boundary.
- `account_runtime::store` now owns account-named storage compatibility methods, and `/api/admin/accounts` service code uses those methods for list, create, update, delete, enable, supported models and auto-disable operations.
- Account status, cooldown clearing, supported-model discovery and account test helpers now enter through account-named runtime/URL helpers and return account-facing Admin messages while legacy `/external-pools` keeps its old wording.
- Anthropic raw request entry and handler route construction now use account-runtime route/body/outcome/error/latency aliases and account-route helper names. Legacy external-pool route types remain behind the facade until the implementation moves out of `external_pool`.
- Anthropic handler scheduling checks and failover execution now call account-named runtime manager wrappers for eligibility, immediate availability, direct route reason and forwarding instead of calling legacy pool-named manager methods directly.
- Account Admin DTOs now use account-named aliases for auth, usage projection, stream response, request body, raw model, auto-disable, retry, model mapping and route-mode types. Account supported-model discovery and account test service paths also match on `AccountAuthType`, while legacy external-pool DTOs and service paths retain old compatibility type names.
- Account usage billing now has an account-shaped Admin response for `/usage-dashboard/account-billing`: rows expose `accountId/accountName` under `accountBillingByAccount`. The frontend overview and usage API client consume that account field, while legacy external-pool billing endpoints and fallback parsing remain for compatibility.
- Account usage risk now has an account-shaped Admin response for `/usage-dashboard/account-risk`: filters, totals, samples and grouping expose account terminology (`accountId`, `accountName`, `accountBillingPresent`, `missingAccountBillingRecords`, `byAccount`). The frontend risk page and usage API client consume the account shape while legacy external-pool risk endpoints remain compatibility-only.
- Admin UI navigation now points to `/account-risk`, and the account risk page module lives under `features/account-risk`; the old `/external-pool-risk` UI route is only a redirect compatibility path.
- Inference attempt budgeting now has an `Account` attempt kind, and real upstream account sends reserve it. The old `ExternalPool` kind remains only as a compatibility alias into the same counter until old snapshot fields and legacy route paths are removed.
- Inference attempt snapshots now expose `accountAttempts` as the primary account send counter. `externalAttempts` remains serialized as a compatibility copy, legacy snapshot JSON maps into the account field on deserialization, and both maintained UIs show local/account/MCP breakdowns.
- Usage records now expose `accountId`, `accountName` and `accountAttempts` as account-facing JSON fields while retaining `externalPoolId`, `externalPoolName` and `externalAttempts` compatibility fields. Usage record/detail/billing UI surfaces prefer account fields and show upstream-account wording with fallback for old records.
- New upstream account usage records now write `routeKind: "account"` as the primary route kind. Account/upstream-account usage filters, Redis summaries, Postgres rollups/dashboard queries and maintained usage UI display all treat `account` plus historical `external_pool` as the upstream-account class during compatibility migration.
- Usage records now expose `accountBilling` as the primary upstream-account billing detail while retaining `externalPoolBilling` as a compatibility copy. Recorder/storage paths fill both fields, read paths normalize historical records, and maintained usage UI reads account billing first.
- Usage summary and dashboard window summary now expose `accountBilling` plus `accountBillingByAccount` aggregate fields while retaining legacy external-pool aggregate fields for compatibility. Redis/Postgres materialization fills both, and the maintained overview UI consumes the account fields first.
- Usage records, usage summaries, dashboard windows/series/top aggregates and credential usage summaries now expose `upstreamMeteringUnits` / `totalUpstreamMeteringUnits` as the account-neutral metering fields. Old `kiroMeteringUsage` / `totalKiroMeteringUsage` JSON fields and current DB/Redis compatibility keys remain mirrored, historical old-only records are normalized on read, and maintained usage UI surfaces show "上游计量" instead of Kiro metering wording.
- Stream and handler internals now carry upstream metering values as `upstream_metering_units`; the old Kiro-named usage field remains only as the serialized/storage compatibility copy, and stream tests assert metering stays internal rather than leaking into downstream SSE.
- New upstream account usage route subtypes now serialize with account terminology: `account_fallback_preflight`, `account_fallback_after_local_attempts`, `account_direct_policy`, `account_error` and `local_rescue_after_account`. Historical `external_*` and `local_rescue_after_external` values remain accepted and displayed as compatibility values in maintained UIs.
- Anthropic parsed/raw fallback routing now uses `AccountFallbackContext` and account-named fallback/preflight helper methods. Local rescue preflight metadata now writes account fields while retaining old `external*` copies, and focused handler/account-only tests passed.
- Local rescue fallback reasons now use account terminology (`account_rate_limit`, `account_timeout`, `account_capacity`, `account_bad_request`, `account_error`), and request-entry/local-rescue handler code reads account-named config accessors instead of external-pool fields.
- Anthropic router, AppState and request runtime now use `payload_guard_account_enabled` for the account-route payload guard switch, while the existing persisted/runtime-config JSON field remains a compatibility boundary. Maintained runtime UI text describes upstream-account payload shaping instead of Kiro/external-pool payload handling.
- Runtime configuration UI wording now presents the legacy external-pool runtime section as upstream-account routing, covering route policy, retry/failover, local rescue, usage diagnostics, prompt steering and body/payload shaping while retaining compatibility state keys internally.
- Local parsed body capability planning now has local-upstream type names and logs. The concrete `KiroRequest` payload type is still isolated to the current legacy local-provider boundary pending provider/body replacement.
- Account-route raw/normalized body capability planning now has account body plan names, with the legacy executor still delegated through `external_pool` until that module is migrated.
- Official upstream error-message extraction now uses account-neutral envelope/helper names while preserving the public-message filtering that blocks sensitive or internal scheduler/account/provider wording from downstream protocol errors.
- Payload guard runtime wrappers and account-forwarding sanitizer now use local-upstream/account names, while the concrete legacy local payload type remains isolated until the provider/body implementation is replaced.
- Account-route body/model/retry pipeline diagnostics now use account wording for payload guard, model rewrite, model mapping and cooldown failures inside the delegated legacy executor.
- Account-route usage debug JSON now writes local processing details under `upstreamProcessing` instead of the old Kiro-named processing key.
- Account-route model processing helpers now use account names for outbound raw model selection and model processing errors inside the delegated legacy executor.
- Account-route retry helpers now use same-account/cross-account names and write `retry_same_account` attempt actions while old config field names remain compatibility storage.
- Core retry/failover tests now use same-account/cross-account terminology for behavior names and assertions while compatibility config fixture fields remain unchanged.
- Runtime config now provides account-named retry status accessors used by the retry pipeline, with old accessor names retained as compatibility delegates.
- Account runtime facade comments and upstream-account integration-test skip messages now avoid presenting the migrated runtime as an external-pool feature.
- Proxy warning responses now use `x-account-runtime-warnings` as the primary header and double-write the old warning header for compatibility.
- Anthropic handler payload guard call sites now enter local-upstream wrappers for guarding and serialization; cache-point retry and thinking-signature retry no longer call the legacy Kiro-named guard helpers directly.
- JSON stream error-envelope usage diagnostics now keep provider-message privacy by storing shape/fingerprint metadata for complete JSON error envelopes instead of raw message bodies, and remaining malformed/incomplete raw snippet sources use neutral official-upstream wording.
- Local-upstream payload diagnostics now use wrapper names for byte breakdown and tool-use format diagnostics at handler/local body pipeline call sites, leaving Kiro-named helpers inside the legacy payload implementation only.
- Anthropic handler runtime log/comment text for local-upstream cache-point retry, payload guard, tool-format, slow interaction and stream/non-stream retry diagnostics now uses local-upstream/account wording instead of Kiro product wording.
- The local body pipeline now exposes `PreparedLocalUpstreamBody.local_upstream_request` and local-upstream helper parameter names at the handler boundary, while the concrete legacy request type remains isolated behind that boundary.
- Stream conversion now offers `process_local_upstream_event` as the handler-facing event processor; the legacy concrete event method remains internal to the stream module and existing stream tests until the event model is replaced.
- Anthropic `AppState`, router dependencies, request-entry flow and handler tests now use `local_upstream_provider` / `with_local_upstream_provider` for the optional legacy local upstream executor, keeping the concrete legacy provider type behind the protocol boundary.
- Anthropic converter module docs, diagnostics, tool-name collision errors and compatibility comments now describe local-upstream/upstream-safe behavior instead of Kiro protocol behavior.
- Model capability seed/status source values now use upstream-account terminology for new writes, normalize old `kiro-*` source strings on read, and expose `sync_from_upstream_catalog` as the main/Admin synchronization entrypoint.
- Stale Kiro-named model capability sync wrappers were removed; runtime and test callers now use upstream-named sync entrypoints directly.
- Native WebSearch MCP routing now uses local-auxiliary-upstream names for provider/error helper wrappers, MCP call helper names and runtime comments while retaining the concrete legacy provider type as a compatibility detail.
- `local_upstream` now provides compatibility aliases for the legacy local provider, call-trace, MCP attribution and response types; Anthropic router, middleware, handler and WebSearch boundaries import those aliases instead of legacy provider names.
- `local_upstream` now provides local request/event aliases used by `payload_guard_runtime`, `tool_format_debug` and `cache`, reducing direct legacy request/event imports in small Anthropic body/diagnostic boundaries.
- Anthropic handler stream/body code now consumes local-upstream request/event/metadata/decoder aliases from the facade for payload guard retries, stream retry state, EventStream decoding, latency classification and metadata usage, leaving concrete legacy request/event/decoder imports inside the compatibility facade.
- Anthropic stream conversion and debug helpers now consume local-upstream event aliases from the facade, and stream tests use `process_local_upstream_event` as the primary event processor entrypoint.
- Local-provider raw upstream error diagnostics now use neutral `official_upstream` source labels and redacted body metadata for provider status/non-eventstream bodies, so private provider messages do not persist in attempt or usage diagnostics.
- Local-upstream timeout, stream-retry and cache-point settings now have `Config` accessors, and Anthropic router/AppState/request runtime/converter/Admin model-test/WebSearch call sites use local-upstream runtime names while persisted compatibility fields remain unchanged.
- The local-upstream agent mode strategy now uses `LocalUpstreamAgentModeStrategy` as the Rust type name while retaining existing compatibility config/wire field names.
- The Claude Code tool prompt-cache strategy now uses `PromptCacheStrategyType::ClaudeCodeTool` as the Rust enum variant while serde keeps legacy `kiro_rs_tool` serialization and accepts `claude_code_tool` on read.
- Claude Code tool prompt-cache policy fields now use `claude_code_tool` inside Rust config and handler code while serde keeps `kiroRsTool` output and accepts `claudeCodeTool` on read.
- Claude Code tool prompt-cache policy and plan structs now use `ClaudeCodeTool*` Rust type names while the existing `kiro_rs_tool` config field and strategy value remain compatibility boundaries.
- Claude Code tool prompt-cache internal methods/helpers now use `claude_code_tool_*` names while the existing `kiro_rs_tool` config field and strategy value remain compatibility boundaries.
- Claude Code tool prompt-cache request/projection state fields now use `claude_code_tool_*` names while the existing `kiro_rs_tool` config field and strategy value remain compatibility boundaries.
- Payload guard report cache-point diagnostics now serialize local-upstream field names and accept old Kiro-named JSON fields only as compatibility read aliases.
- Local body preparation now uses an upstream reasoning capability alias and local-upstream request aliases at its handler/test boundary.
- Converter tool-use/tool-result pairing now uses local-upstream request aliases at its production import boundary.
- Converter body, history, tool and model modules now use local-upstream request aliases and upstream reasoning aliases instead of direct legacy request model imports.
- Usage attempt-chain search and diagnostics now use local-upstream call-trace aliases for credential attempts and summary formatting.
- Handler local dispatch policy, account fallback preflight and request-entry fast-fail code now use local-upstream dispatch aliases instead of direct token-manager route-state/acquire-mode imports.
- Model capability catalog ingestion now uses local-upstream model catalog aliases for available models, cohort keys and token-limit fixtures.
- Payload guard production code and local payload fixtures now use local-upstream request aliases instead of direct legacy request model imports.
- Payload guard guarding, serialization, byte-breakdown and tool-use diagnostic entrypoints now use local-upstream names instead of Kiro-named module APIs.
- Anthropic stream tests now use local-upstream event facade aliases, removing direct legacy event imports from `src/anthropic`.
- Admin service model-test request construction and response parsing now use local-upstream request/event/decoder aliases instead of direct legacy paths.
- Model capability cohort fencing and startup readiness now use upstream reasoning contract-match naming instead of the old Kiro-named type.
- Model capability reasoning field path, capability and state types now use upstream names as their real Rust types, including Postgres persistence and local-upstream provider call sites.
- Main process wiring now uses local-upstream provider naming for the optional local executor and model capability recovery worker.
- Admin service dependencies and internal provider state now use local-upstream provider naming, leaving legacy credential behavior behind the local executor boundary.
- Admin service credential backup, validation, balance and snapshot code now uses local-upstream credential/usage-limit/manager aliases instead of direct legacy credential manager imports.
- The unmounted `src/test.rs` Kiro-specific manual stream caller was removed.
- Account-route local auxiliary attempt traces now use local-upstream credential-attempt aliases instead of direct legacy call-trace paths.
- Account-route Redis lease cleanup now uses an account-runtime critical storage-task alias, and that bridge now resolves through the local-upstream facade instead of direct legacy token-manager imports.
- Main shutdown lifecycle now uses account-runtime storage-task aliases for best-effort storage task stats, drain and shutdown calls.
- Postgres model-capability persistence now uses local-upstream model catalog aliases for reasoning cohort keys.
- Postgres credential persistence and API-key bootstrap now use local-upstream credential aliases instead of direct legacy credential imports.
- Runtime config default endpoint lookup now uses the local-upstream endpoint facade instead of a direct legacy endpoint path.
- Main endpoint registry construction now uses local-upstream endpoint aliases for IDE/CLI endpoint setup instead of direct legacy endpoint imports.
- Main startup and Redis runtime-event wiring now use local-upstream credential/config/manager aliases instead of direct legacy credential manager imports.
- Main startup local API-key bootstrap variables and Admin supported-model normalization variables now use local-upstream/upstream naming while legacy environment/config field names remain compatibility boundaries.

## In Progress

- Migrate account runtime internals away from legacy external-pool names behind the `account_runtime` facade while preserving current scheduler, proxy, body-mode, retry, usage projection and compatibility behavior.
- Convert remaining backend config DTO/type names and storage compatibility bridges from external-pool terminology toward account terminology while preserving temporary compatibility aliases only where existing clients still need them.

## Next

- Split scheduler primitives from Kiro credential types before deleting Kiro modules.
- Convert body and protocol paths to canonical/upstream-account logic with no Kiro envelope or Kiro event dependency.

## Deferred

- Archive or rewrite old Kiro-focused planning documents after the implementation has landed and the new target has evidence.
