# Final Target Plan

Role: Implementation-driving architecture plan

Status: Planning; not yet implemented

Authority: Defines the final target for removing Kiro concepts from the current Rust runtime

As of: 2026-08-13

Related: [Plan root](../README.md), [roadmap](../roadmap.md), [module map](../../../baseline/module-map.md)

## Core Objective

Refactor the current Rust service into a Kiro-independent account scheduling and usage shaping runtime.

The target is not a Kiro gateway, not a Kiro provider extraction, and not a Kiro-compatible abstraction. Kiro is absent from the target product vocabulary and runtime dependency graph. Existing Kiro-named code is only a source of generic implementation ideas where it contains reusable scheduling, lease, usage, stream, body, model, proxy, or observability behavior.

## Target Runtime Flow

```text
Anthropic/Claude-compatible client request
  -> protocol adapter
  -> request admission and immutable request context
  -> body/model/capability processing
  -> account route planning
  -> account scheduler acquire
  -> one observable upstream account attempt
  -> canonical events or canonical response
  -> protocol encoder
  -> terminal outcome, usage facts, statistics and Admin visibility
```

## Target Vocabulary

Use these terms in target code and UI:

- `account`
- `account_runtime`
- `account_scheduler`
- `account_lease`
- `account_attempt`
- `upstream_account`
- `route_policy`
- `protocol_profile`
- `canonical_event`
- `raw_usage`
- `effective_usage`
- `reported_usage`
- `usage_projection`

Do not introduce or retain these as runtime/product terms:

- `Kiro`
- `kiro`
- `KiroCredentials`
- `KiroProvider`
- `KiroEndpoint`
- `local_pool`
- `external_pool`
- `profile_arn`
- `kiro_api_key`
- `machine_id`
- `codewhisperer`
- `IdC`
- `external_idp`
- `KiroRsTool`

Historical docs, archived evidence, and migration notes may mention old terms, but runtime source, Admin source, config fields, API DTOs, database schema, Redis keys, and tests for the target should not depend on them.

## Preserve As Generic Capabilities

Preserve and rename these behaviors where current code already implements useful logic:

- Account selection by priority, model support, health, probation, warmup and selection pressure.
- Weighted concurrency limits, per-account RPM, bounded queueing, acquire deadlines, Redis/local leases, release guards and stale cleanup.
- Sticky session affinity and soft-failure handling.
- Cooldown and transient failure penalties.
- Proxy resource selection and fail-closed proxy binding.
- Raw request probing, missing `max_tokens` policy, request history sanitization, thinking validation and tool schema cleanup.
- Raw and normalized upstream body modes for compatible upstreams.
- Payload guard, body shaping, image/file materialization and model mapping.
- Stream/non-stream protocol encoding, ordered content/thinking/tool/usage events and normalized public errors.
- Raw usage preservation, effective usage calculation, reported usage projection, prompt-cache simulation, pricing, rollups and dashboard queries.
- Request API keys, Admin operations, audit, health and diagnostics that remain meaningful for upstream accounts.

## Remove Kiro-Specific Behavior

Remove these target behaviors rather than moving them behind a new abstraction:

- Kiro OAuth, Social, IdC, external IdP, AWS SSO or Kiro API-key credential import and refresh.
- Kiro credential files, `KIRO_API_KEY`, Kiro credential backup/import/export and Kiro account testing.
- Kiro IDE/CLI endpoints, endpoint registry, request envelopes, profile ARN discovery and profile ARN/body injection.
- Kiro EventStream parser, CRC/framing, Kiro event DTOs and direct Kiro-event-to-Anthropic stream handling.
- Kiro model seed, model capability sync from Kiro, quota sync, usage limits, subscription titles and Opus eligibility derived from Kiro subscription.
- Kiro cachePoint insertion and `kiro_cache_point_*` configuration.
- Local Kiro pool, external fallback, local rescue and local pool circuit terminology.
- Kiro metering usage fields as Kiro-named statistics.

## Target Module Shape

The exact file layout may change during implementation, but dependencies should move toward this shape:

```text
src/
  account_runtime/
    account.rs
    attempt.rs
    scheduler.rs
    lease.rs
    queue.rs
    rpm.rs
    sticky.rs
    health.rs
    proxy.rs
    coordination.rs

  upstream/
    account_http.rs
    request.rs
    response.rs
    error.rs
    usage.rs

  protocol/
    anthropic/
    canonical/

  usage/
    raw.rs
    projection.rs
    recorder.rs
    pricing.rs

  body/
    request_probe.rs
    normalized.rs
    payload_guard.rs
    files.rs
```

Dependency rules:

- Protocol adapters cannot import account implementation details or storage adapters.
- Account runtime cannot import Anthropic HTTP DTOs or legacy Kiro types.
- Upstream account execution cannot hide business attempts that may consume capacity or bill usage.
- Usage persistence stores raw attempt facts separately from client-reported request usage.
- Storage code may persist typed account/runtime facts but should not define product semantics.

## Account Domain

The former external pool becomes the account domain. A target account should include at least:

```text
id
name
enabled
base_url
secret reference or redacted API-key metadata
header/auth policy
supported models
priority
rpm
max concurrent requests
proxy config or proxy resource binding
health and cooldown state
recent attempts and usage summaries
```

There should be no local/external/fallback split. Routing selects ordered account candidates according to route policy and capabilities. Retry or failover is an attempt-engine decision based on delivery evidence, downstream commitment, classified error, account health, and remaining candidates.

## Body And Protocol Boundary

Body processing must be upstream-account oriented, not Kiro oriented.

Keep:

- raw passthrough for compatible upstreams when safe and configured;
- normalized Anthropic-compatible body construction;
- file/image/tool/thinking/history compatibility processing;
- payload guard and shaping policies;
- model resolution and capability checks.

Remove:

- Anthropic-to-Kiro request conversion;
- Kiro cachePoint conversion;
- Kiro endpoint/profile/body injection;
- code paths named `local_body_pipeline` if `local` means Kiro pool;
- stream code that imports Kiro event DTOs.

Introduce canonical events before protocol encoding:

```text
upstream account response
  -> canonical event stream
  -> Anthropic/Claude-compatible stream or non-stream encoder
```

## Usage Boundary

Usage must keep explicit fact layers:

```text
upstream raw usage
+ effective request facts
+ cache evidence or labeled simulation
+ route reporting policy
+ pricing revision
-> effective usage
-> client reported usage
-> operational usage/cost record
```

Rename or replace old fields:

- `KiroCredentialAttempt` -> `AccountAttemptTrace`
- `ExternalPoolAttempt` -> `AccountAttemptTrace` or `UpstreamAttemptTrace`
- `external_pool_billing` -> `upstream_account_billing` or generic usage projection details
- `kiro_metering_usage` -> `upstream_metering_units` or provider-neutral raw usage/cost facts
- `UsageRouteKind::LocalCredential` / `ExternalPool` -> account/route attempt terms

Do not coerce missing or unknown upstream usage to zero when an attempt may have executed. Client-reported usage should project only the delivered attempt, while operational usage preserves every potentially executed attempt.

## Admin And UI Boundary

Rename and reshape Admin around accounts:

- `External Pools` -> `Accounts`
- pool enablement -> account runtime enablement
- pool health/cooldown -> account health/cooldown
- external attempts -> account attempts
- external billing -> upstream account usage/projection

Delete Kiro-specific UI/API:

- Kiro credentials page and import/export formats;
- Kiro model sync;
- Kiro quota/subscription views;
- Kiro endpoint/profile/auth forms;
- Kiro cachePoint settings.

Runtime settings should expose account scheduler, body policies, route policies, usage projection, protocol profiles, and observability settings.

## Implementation Order

The target should be implemented directly on a new branch from updated `master`. The following order is for reviewability and dependency control, not phased product delivery:

1. Commit and merge the current feature branch into `master`, then create `feature/account-runtime-upstream-accounts`.
2. Add target-neutral account/runtime/canonical usage types.
3. Convert external pool configuration and UI terminology toward account terminology.
4. Extract scheduler primitives from Kiro credential dependencies into account runtime modules.
5. Replace local/external/fallback route terminology with account route planning and attempt terminology.
6. Introduce canonical upstream response/events and remove direct Kiro event imports from protocol encoders.
7. Convert body pipelines to upstream-account body preparation and remove Kiro envelope/cachePoint handling.
8. Convert usage records, rollups and Admin DTOs to account/upstream terminology.
9. Delete Kiro authentication, endpoint, provider, parser, model, credential and Admin paths once no runtime code imports them.
10. Run source-level scans, unit/integration tests, frontend builds and targeted protocol/usage checks.

## Acceptance Checks

The implementation is not complete until all of these are true:

- Runtime source and UI source have no Kiro business dependency.
- `src/kiro` is removed or contains no compiled runtime module.
- No target config, DTO, DB table, Redis key, Admin page or test uses `kiro`, `local_pool` or `external_pool` terminology except migration/archive text.
- Accounts are the only upstream scheduling unit.
- Account scheduling supports priority, model support, RPM, weighted concurrency, proxy, health, cooldown, sticky, queueing and bounded release behavior.
- Body handling supports compatible raw/normalized upstream account requests without Kiro envelope logic.
- Protocol handling emits correct stream/non-stream Anthropic/Claude-compatible responses from canonical upstream events.
- Usage preserves raw/effective/reported layers and records account attempts without Kiro-named metering fields.
- Admin can configure accounts, routes, scheduler settings, body policies and usage projection without Kiro settings.
- Regression checks include source scans for removed terms and tests covering account acquire/release, retry/failover, usage projection, stream final usage and UI account configuration.
