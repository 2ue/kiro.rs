# External Pool Usage Billing Production Audit - 2026-07-23

## Purpose

This document consolidates the production evidence, problem explanation, downstream usage semantics, and complete repair design for external-pool success requests that are recorded with zero billable output or missing `externalPoolBilling`.

The target behavior is explicit:

- A normal upstream HTTP 200 model response should continue to return HTTP 200 to the downstream client.
- A normal HTTP 200 response must not produce abnormal downstream billing.
- A normal HTTP 200 response must not produce abnormal kiro.rs internal billing.
- If upstream usage is missing or unusable, kiro.rs must synthesize and mark a conservative usage record instead of returning/recording a success with unusable billing metadata.
- If a pool is configured with `usage_projection_mode=current_path_policy`, the response usage returned downstream must use the same reported-usage policy for stream and non-stream responses.

This document intentionally does not claim historical raw response bodies were recovered. Historical success response bodies were not stored in PostgreSQL, Redis, or existing logs.

## Evidence Package

Local evidence root:

```text
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/
```

Default redacted archive:

```text
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/20260723-003327-kiro-prod-usage-zero-redacted.tar.gz
```

Main problem folders:

```text
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/problems/P001-external-pool-nonstream-zero-billing/
tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero/problems/P002-external-pool-upstream-instability/
```

Raw evidence exists only locally under `raw/` and is excluded from the default archive. The redacted archive includes summaries, problem files, fingerprints, and redacted command outputs.

## Production Deployment Context

Deployment inspected:

```text
/root/docker-compose/<prod-deployment-redacted>
```

Runtime containers:

```text
<prod-app-container-redacted>
<prod-postgres-container-redacted>
<prod-redis-container-redacted>
caddy
```

App image:

```text
ghcr.io/2ue/kiro-rs:latest
org.opencontainers.image.revision=c1748265b904aacdbd6fa33f4bd2e86985ad1f53
org.opencontainers.image.version=0.0.112
```

Public entry:

```text
<prod-public-entry-redacted> -> Caddy -> <prod-app-container-redacted>:8990
```

Health checks:

```text
/healthz: 200
/readyz: 200, postgres=true, redis=true, redisRuntimeEvents=true
```

`usage_records` size at audit time:

```text
1355 MB
```

Relevant indexes exist on `usage_records.created_at`, `usage_records.status`, and JSON external pool fields.

## Relevant Runtime Configuration

External pool runtime config:

```text
externalPoolsEnabled=true
externalPoolRetryMaxAttempts=6
externalPoolStreamResponseMode=event_passthrough
externalPoolUsageProjectionUpliftPercent=35
externalPoolUsageProjectionOutputUpliftMinTokens=2000
externalPoolUsageProjectionOutputUpliftPercent=25
externalPoolAutoDisableEnabled=false
```

Reported usage config:

```text
reportedUsage.default.enabled=true
reportedUsage.default.skipNonStreamUsageProjection=false
```

Enabled pools relevant to this audit:

| pool id | name | enabled | priority | usage projection | request body mode | raw model mode | stream response mode |
| ---: | --- | --- | ---: | --- | --- | --- | --- |
| 15 | `apiv3.52codeflow` | true | 2 | `current_path_policy` | `normalized` | `none` | unset |
| 4 | `kkkkyue` | true | 50 | `current_path_policy` | `normalized` | `rewrite_top_level` | unset |

Both relevant pools are configured for `current_path_policy`. Therefore a normal response should not simply expose upstream raw usage when projection is active. Stream and non-stream should produce the same reported-usage semantics.

## Problem Statements

### Problem A: Success 200 can be recorded as zero billing

External-pool success records can be written as:

```text
status=success
usageSource=request_estimate
outputTokens=0
estimatedCostUsd=0
pricingAvailable=false
rawUsage missing/null
externalPoolBilling missing/null
```

This affects internal accounting, dashboards, rollups, and any system that relies on kiro.rs usage records rather than parsing downstream response bodies.

### Problem B: Non-stream downstream usage is not aligned with current_path_policy

For pool 15, non-stream response bodies return syntactically standard Anthropic `usage`, but the usage shape looks like upstream raw/pass-through usage. Stream responses for the same pool and prompt are shaped according to `current_path_policy`.

This means the downstream client may see parseable usage, but not the usage shape kiro.rs is configured to report.

### Problem C: Stream and non-stream behavior diverge

Stream responses:

- capture raw usage;
- project usage under `current_path_policy`;
- return projected usage downstream;
- record `externalPoolBilling`.

Non-stream responses in reproduced cases:

- return a normal JSON body with top-level `usage`;
- do not record raw usage;
- do not record external billing;
- appear to return raw/pass-through usage despite `current_path_policy`.

### Problem D: External upstream instability is present but separate

Pool 4 has 403/auth/quota failures. Pool 15 has Cloudflare 502/524, blocked/security, and model unavailable failures.

These failures affect routing, latency, retry chains, and pool health. They do not explain the reproduced success-200 non-stream billing loss, so they should be treated as a separate problem class.

## Aggregate Evidence

Recent 24-hour aggregate from `usage_records`:

| pool | stream | status | usageSource | total | outputZero | rawUsagePresent | billingPresent | successZeroMissingBilling | costSum |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| #15 `apiv3.52codeflow` | true | success | `request_estimate` | 28240 | 28240 | 0 | 0 | 28240 | 0 |
| #15 `apiv3.52codeflow` | false | success | `request_estimate` | 12119 | 12119 | 0 | 0 | 12119 | 0 |
| #4 `kkkkyue` | false | success | `request_estimate` | 7159 | 7159 | 0 | 0 | 7159 | 0 |
| #4 `kkkkyue` | true | success | `request_estimate` | 1 | 1 | 0 | 0 | 1 | 0 |
| #15 `apiv3.52codeflow` | true | success | `local_prompt_cache` | 25449 | 0 | 25449 | 25449 | 0 | 5175.787544 |
| #4 `kkkkyue` | true | success | `local_prompt_cache` | 19023 | 0 | 19023 | 19023 | 0 | 4965.352941 |
| #4 `kkkkyue` | false | success | `upstream_metadata` | 4156 | 0 | 4156 | 4156 | 0 | 63.110695 |
| #4 `kkkkyue` | false | success | `local_prompt_cache` | 361 | 0 | 361 | 361 | 0 | 8.293168 |

Pool 4 non-stream shape:

| class | count | first seen | last seen | p50 max_tokens | p90 max_tokens | max max_tokens | p50 output | max output | costSum |
| --- | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `zero_missing_billing` | 7159 | 2026-07-21 17:28:42Z | 2026-07-22 16:23:10Z | 8192 | 64000 | 128000 | 0 | 0 | 0 |
| `nonzero_or_billed` | 4517 | 2026-07-21 17:27:28Z | 2026-07-22 16:18:49Z | 50 | 50 | 81920 | 5 | 209 | 71.403863 |

The aggregate evidence proves this is not a one-off display bug. It is a repeated production accounting shape.

## Reproduction Evidence

Two rounds of low-frequency production calls were run through the public Caddy domain with the real service API key. The prompt was controlled and short:

```text
Reply with exactly OK.
```

No production config, DB rows, Redis keys, or services were modified. The calls naturally created normal usage records.

### First Reproduction Set

Valid requests:

| request id | endpoint | stream | model | max_tokens | HTTP body usage | DB billing |
| --- | --- | --- | --- | ---: | --- | --- |
| `req_01MWSzCj1NfoaixzzPkgVqzn` | `/v1/messages` | false | haiku | 1 | present | missing |
| `req_01PxztmgfkWZQWComggKTtSr` | `/v1/messages` | false | haiku | 32 | present | missing |
| `req_01HkFDzEaqT348ueGoAjy7zX` | `/ha/v1/messages` | false | opus | 8192 | present | missing |
| `req_016RAGjvJrkjYo9JiyJyktxd` | `/v1/messages` | true | haiku | 16 | present | present |

Representative non-stream response body:

```json
{
  "content": [{"text": "OK", "type": "text"}],
  "type": "message",
  "usage": {
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 734,
    "input_tokens": 81,
    "output_tokens": 1
  }
}
```

Corresponding DB/Redis record:

```text
status=success
usageSource=request_estimate
outputTokens=0
rawUsage=missing
externalPoolBilling=missing
estimatedCostUsd=0
```

### Expanded Curl Matrix

The first matrix attempt used Python `urllib` and returned HTTP 403 for every case. Those 403s were excluded from usage conclusions because a plain `curl` client was known to succeed and the `urllib` client shape was not representative.

The matrix was rerun with `curl`.

| class | count | HTTP result | body usage | DB billing |
| --- | ---: | --- | --- | --- |
| `/v1/messages` haiku non-stream max 1 | 2 | 2/2 HTTP 200 JSON | 2/2 top-level usage | 0/2 present |
| `/v1/messages` haiku non-stream max 32 | 2 | 2/2 HTTP 200 JSON | 2/2 top-level usage | 0/2 present |
| `/ha/v1/messages` opus non-stream max 8192 | 2 | 2/2 HTTP 200 JSON | 2/2 top-level usage | 0/2 present |
| `/v1/messages` haiku stream max 16 | 2 | 2/2 HTTP 200 SSE | 2/2 SSE usage events | 2/2 present |
| `/ha/v1/messages` opus stream max 64 | 1 | 1/1 HTTP 200 SSE | 1/1 SSE usage events | 1/1 present |

Expanded non-stream request IDs:

```text
req_01EUPuBjS7mHL2VC386QBoCT
req_016JdMVRsjW167oR7E8vx3N1
req_01oM8Ca7iyUSomcs9TGiWzJr
req_01Ut1HPMrgPNs9qwifeYZwNi
req_01QU4fGNVXRWizgnkpAhkGsf
req_01NKn9T5YaAX3UvVDmxDecRk
```

Expanded stream control request IDs:

```text
req_01koirP7qiCAm2FaGY31PumB
req_017fPy8SMRqse17BDNuBE6ru
req_01ErCUa7DSyPxkPb7wAfWD11
```

### Non-stream vs Stream Usage Shape

Representative non-stream haiku response usage:

```json
{
  "cache_creation_input_tokens": 0,
  "cache_read_input_tokens": 734,
  "input_tokens": 81,
  "output_tokens": 1
}
```

Representative stream haiku response usage for the same pool and prompt:

```json
{
  "cache_creation_input_tokens": 0,
  "cache_read_input_tokens": 0,
  "input_tokens": 817,
  "output_tokens": 1
}
```

Representative stream DB billing for the same model/prompt shape:

```text
rawUsage:
  inputTokens=815
  cacheReadInputTokens=734
  outputTokens=1

reportedUsage:
  inputTokens=817
  cacheReadInputTokens=0
  outputTokens=1

externalPoolBilling=present
usageProjectionApplied=true
```

This is the clearest body-shape evidence:

- stream response is projected under `current_path_policy`;
- non-stream response is not projected and looks like raw/pass-through upstream usage;
- non-stream DB/Redis billing is missing.

## Current Source Expectation

The source path in `src/external_pool.rs` says non-stream should read the response body, call `maybe_project_non_stream_usage`, then generate billing:

```text
forward_once non-stream:
  response.bytes()
  maybe_project_non_stream_usage(bytes, projection_context)
  external_pool_billing_from_capture(route, pool, usage_capture)
  record_external_success(..., billing)
```

The parser currently:

- parses the body as JSON;
- only looks for top-level `usage`;
- only recognizes Anthropic-style fields:
  - `input_tokens`
  - `output_tokens`
  - `cache_creation_input_tokens`
  - `cache_read_input_tokens`
  - nested `cache_creation.ephemeral_5m_input_tokens`
  - nested `cache_creation.ephemeral_1h_input_tokens`
- requires `capture.raw` to be present before `externalPoolBilling` can be generated.

This source expectation is contradicted by production reproduction:

- final downstream non-stream JSON body contains top-level Anthropic-style `usage`;
- DB/Redis record still lacks `rawUsage` and `externalPoolBilling`;
- final downstream non-stream usage also does not reflect current-path projection.

This means the repair must not only "add another parser path". The repair must enforce an invariant around the final body returned downstream and the billing record generated internally.

## What Is Proven

The following are proven by production evidence:

1. Pool 15 non-stream HTTP 200 can return standard top-level JSON `usage` to the downstream client while DB/Redis billing is missing.
2. Pool 15 stream HTTP 200 can return SSE usage and correctly record billing for the same service and pool.
3. Pool 15 non-stream response usage is not shaped like stream response usage under `current_path_policy`.
4. Pool 4 non-stream historical zero-billing success records are numerous and strongly correlated with larger `requestedMaxTokens`.
5. `usage_repair_loop` did not overwrite the reproduced rows; Redis and Postgres both contain the same missing-billing state.
6. External pool instability exists separately, including pool 4 auth/quota failures and pool 15 Cloudflare/model/channel failures.

## What Is Inferred

The following are strong inferences, not direct captures of historical raw bodies:

1. Pool 15 non-stream response usage appears raw/pass-through or at least unprojected.
2. The non-stream external-pool accounting path loses or fails to derive usage before creating `UsageRecord`.
3. For historical pool 4 zero-billing rows, some responses may have had downstream usage and some may not have. Existing records cannot distinguish those cases because raw response bodies were not stored.

## What Is Unknown

The following still needs instrumentation or code-level reproduction:

1. Why `maybe_project_non_stream_usage` does not produce billing in production when the final body has top-level Anthropic usage.
2. Whether the running binary differs from the checked source despite matching labels and similar source snippets.
3. Whether body bytes at parse time differ from body bytes sent to the downstream client due to runtime transformation.
4. The exact raw body shape for historical pool 4 zero-billing success records.

## Downstream Billing Semantics

There are three different "usage" concepts and the fix must keep them separate:

| concept | meaning |
| --- | --- |
| raw usage | usage as returned by upstream external pool before kiro.rs policy projection |
| reported usage | usage returned by kiro.rs to downstream client |
| billable usage | usage used by kiro.rs internal accounting; currently intended to equal reported usage for external pool billing |

For `usage_projection_mode=pass_through`:

```text
reported usage = raw usage
billable usage = raw usage
```

For `usage_projection_mode=current_path_policy`:

```text
raw usage      = upstream observed usage
reported usage = usage after current path cache/output policy
billable usage = reported usage
```

For a normal 200 response with no usable upstream usage:

```text
raw usage      = absent or estimated marker
reported usage = synthesized conservative usage
billable usage = synthesized conservative usage
metadata       = usageEstimated=true / response_estimate
```

Returning a syntactically valid usage object is not sufficient. It must be the correct reported-usage object for the pool configuration.

## Repair Invariant

Every successful external-pool model response must satisfy this invariant:

```text
HTTP status is 200
AND response is a normal model response
=>
downstream response contains parseable Anthropic-compatible usage
AND UsageRecord contains raw/reported usage evidence
AND UsageRecord contains externalPoolBilling
AND UsageRecord token/cost fields are derived from reported usage
AND response usage shape respects pool.usage_projection_mode
```

Allowed exception:

```text
pricing catalog missing for the model
```

In that case, `externalPoolBilling` should still exist with usage snapshots and `pricingAvailable=false`; token fields should not collapse to `outputTokens=0` when response usage is known.

## Complete Repair Design

### 1. Introduce a final response usage processor

Add a shared processor for external-pool response usage:

```rust
struct ExternalResponseUsageResult {
    body: Bytes,
    raw_usage: Option<CacheUsage>,
    shaped_usage: Option<CacheUsage>,
    reported_usage: Option<CacheUsage>,
    usage_projection_applied: bool,
    usage_estimated: bool,
    diagnostics: ExternalResponseUsageDiagnostics,
}
```

It should run for non-stream responses after the full response body is available and before building the downstream response.

It should also expose stream helpers so stream and non-stream use the same usage policy rules.

### 2. Classify successful response bodies

Before deciding usage:

```text
body class:
  html_success_protocol_error
  error_envelope_success_protocol_error
  anthropic_message_json
  anthropic_wrapper_json
  sse_text_on_non_stream
  unknown_text_or_binary
```

Existing HTML/error-envelope checks should remain. The repair must not convert clear HTML/error bodies into normal 200 usage-bearing responses.

Normal JSON message responses should be allowed to remain HTTP 200.

### 3. Extract usage from multiple candidate paths

Non-stream usage extraction should support:

```text
$.usage
$.message.usage
$.delta.usage
$.data.usage
$.response.usage
```

It should recognize Anthropic fields:

```text
input_tokens
output_tokens
cache_creation_input_tokens
cache_read_input_tokens
cache_creation.ephemeral_5m_input_tokens
cache_creation.ephemeral_1h_input_tokens
```

It should also normalize OpenAI-style fields when present:

```text
prompt_tokens -> input_tokens
completion_tokens -> output_tokens
total_tokens -> diagnostic only unless input/output missing
```

If multiple usage candidates exist, choose the best final candidate:

1. top-level `usage` on a final message;
2. `message.usage`;
3. final delta usage;
4. wrapper usage.

Record candidate paths in diagnostics.

### 4. Apply pool usage projection consistently

Given `raw_usage` or synthesized usage basis:

```text
if pool.usage_projection_mode == pass_through:
    reported_usage = raw_or_estimated_usage
    body_usage = reported_usage
    usage_projection_applied = false

if pool.usage_projection_mode == current_path_policy:
    shaped_usage = apply current path policy to raw_or_estimated_usage
    reported_usage = apply external uplift/output uplift/final guards
    body_usage = reported_usage
    usage_projection_applied = true
```

This must be the same policy used by the stream path.

The non-stream response body must be rewritten when `current_path_policy` changes usage fields.

### 5. Generate billing from the same usage returned downstream

`ExternalPoolBilling` should be generated whenever usage is known or synthesized:

```text
rawUsage       = raw upstream usage when observed, otherwise estimated raw snapshot
shapedUsage    = shaped usage under policy, otherwise raw
reportedUsage  = final usage returned downstream
billableCost   = price(reportedUsage)
reportedCost   = price(reportedUsage)
rawCost         = price(rawUsage) when raw was observed; otherwise estimated raw cost
pricingAvailable = price lookup availability
usageEstimated = true when any usage field was synthesized
```

If pricing is unavailable, preserve usage snapshots and set costs to 0 with `pricingAvailable=false`. Do not drop `externalPoolBilling`.

### 6. Synthesize usage when normal response lacks usage

When HTTP 200 body is a normal model response but no usage candidate is usable:

Input:

```text
request_input_tokens from route.request_input_tokens
```

Output:

```text
estimate from response content text
```

Rules:

```text
if content text is non-empty and estimator returns 0:
    output_tokens = 1
if response has stop_reason but no text:
    output_tokens = 0
```

Use the best available tokenizer/count estimator already present in the codebase. If model-specific tokenization is unavailable, use a deterministic conservative estimate and mark it.

Then inject standard Anthropic usage:

```json
{
  "usage": {
    "input_tokens": <reported input>,
    "cache_creation_input_tokens": <reported cache creation>,
    "cache_read_input_tokens": <reported cache read>,
    "output_tokens": <reported output>
  }
}
```

The response stays HTTP 200.

### 7. Stream fallback behavior

For stream responses:

Track:

```text
raw usage events
reported usage events
text delta byte/token estimate
message_stop seen
```

If a usage event is observed:

```text
process as today, but share projection code with non-stream
```

If no usage event is observed but text was emitted:

```text
before message_stop:
    inject a final message_delta usage event
    use synthesized reported usage
    record externalPoolBilling
```

If no text and no usage are observed:

```text
record success only if the upstream response is otherwise a valid empty model response;
inject/record zero output usage with estimated input;
mark usageEstimated=true
```

Stream `message_start.usage` and final `message_delta.usage` should not conflict in a way that causes downstream parsers to choose different billing categories. The final `message_delta.usage` should be treated as authoritative, but the implementation should avoid sending contradictory cache classes when possible.

### 8. Add a success usage guard

Before recording success:

```rust
if status == success && route_kind == external_pool {
    assert_or_repair_success_usage_invariant(...)
}
```

The guard should not panic in production. It should:

1. repair when safe;
2. emit a bounded diagnostic when repaired or unrepaired;
3. never allow known-normal response usage to be ignored by billing.

Diagnostic fields:

```text
request_id
endpoint
stream
pool_id
pool_name
usage_projection_mode
response_content_type
body_len
body_sha256
json_parse_ok
body_class
top_level_keys
usage_candidate_paths
selected_usage_path
raw_usage_present
reported_usage_present
billing_present_before_guard
billing_present_after_guard
usage_estimated
body_prefix_redacted_512
```

Do not store prompt, API key, full output, or complete body.

### 9. Persist explicit estimation metadata

Add fields to `ExternalPoolBilling` or a sibling diagnostic object:

```text
usageEstimated: bool
usageEstimateReason: "missing_upstream_usage" | "unparseable_usage" | "stream_missing_final_usage"
usageCandidatePath: string | null
bodyUsageProjectionApplied: bool
```

Consider adding a new usage source:

```text
response_estimate
```

If adding a new enum is too broad for the first patch, keep `request_estimate` for compatibility but preserve `externalPoolBilling` and explicit `usageEstimated=true`. The long-term model should distinguish request-only estimates from response-content estimates.

## Data and Backfill Design

Historical exact backfill is not possible because historical successful response bodies were not stored.

Allowed historical handling:

```text
mark affected records as external_pool_success_missing_billing
compute exposure ranges by pool/model/stream/max_tokens/input_tokens
do not claim exact output usage
do not silently convert historical zero records into authoritative billed records
```

Optional conservative backfill:

```text
for records with known downstream response usage from future diagnostics:
    backfill exact usage
for historical records without body:
    only add anomaly marker and estimated exposure fields
```

Historical markers should be separate from live billing fields unless the business decision explicitly accepts estimated backfill.

## Test Plan

### Unit tests

Add tests around the response usage processor:

1. Non-stream top-level Anthropic usage:
   - input body has top-level `usage`;
   - `externalPoolBilling` is present;
   - `rawUsage` is present.

2. Non-stream `current_path_policy` projection:
   - raw usage has cache read;
   - returned body usage is projected reported usage;
   - DB reported usage equals returned usage.

3. Non-stream `pass_through`:
   - returned body usage equals raw usage;
   - billing is present.

4. Non-stream missing usage but normal content:
   - body has `content[].text`;
   - response gets injected usage;
   - billing is present;
   - `usageEstimated=true`.

5. Non-stream wrapper usage:
   - usage under `message.usage` or `data.usage`;
   - parser finds it;
   - response emits standard top-level usage if endpoint requires Anthropic shape.

6. OpenAI-style usage:
   - `prompt_tokens` and `completion_tokens` normalize correctly.

7. HTML success:
   - remains protocol error, not synthesized normal usage.

8. Error envelope with HTTP 200:
   - remains protocol error, not synthesized normal usage.

### Stream tests

1. SSE with normal usage:
   - billing is present;
   - final downstream usage equals reported usage.

2. SSE missing usage but emits text:
   - final usage event is injected;
   - billing is present;
   - `usageEstimated=true`.

3. SSE message_start and message_delta usage:
   - final message_delta usage is authoritative;
   - DB reported usage matches final downstream usage.

4. Stream pass-through vs current-path-policy:
   - both modes are covered.

### Integration tests

Use a fake external pool server:

1. Non-stream JSON with usage.
2. Non-stream JSON without usage.
3. Stream SSE with usage.
4. Stream SSE without usage.
5. Upstream HTML 200.
6. Upstream error envelope 200.

For each successful case assert:

```text
HTTP 200 returned downstream
downstream usage present
UsageRecord status=success
externalPoolBilling present
outputTokens matches reported usage
estimatedCostUsd > 0 when pricing available and output/input nonzero
```

### Production regression test

After patch deployment, rerun the same curl matrix:

```text
/v1/messages haiku non-stream max 1
/v1/messages haiku non-stream max 32
/ha/v1/messages opus non-stream max 8192
/v1/messages haiku stream max 16
/ha/v1/messages opus stream max 64
```

Expected results:

```text
non-stream:
  HTTP body usage present
  body usage shaped when current_path_policy
  DB rawUsage present
  DB externalPoolBilling present
  DB reportedUsage equals downstream body usage

stream:
  same as above
```

## Operational Evidence Notes

External upstream instability remains documented separately:

```text
pool 4:
  403 auth_error / insufficient balance

pool 15:
  Cloudflare 502
  Cloudflare 524
  403 blocked/security_lock
  model_not_found / no available channel
```

These issues should be fixed operationally, but they do not replace the success-200 usage invariant. Even a healthy upstream can return a normal 200 response without usage or with a response shape the parser fails to understand. The service must still return and record valid billing usage for normal successful responses.

## Implementation Touch Points

Likely code areas:

```text
src/external_pool.rs
  forward_once non-stream branch
  maybe_project_non_stream_usage
  process_usage_slots_in_sse_value
  external_pool_billing_from_capture
  record_external

src/external_pool/usage_projection.rs
  shared projection context and cache commit behavior

src/anthropic/usage.rs
  optional usage source or billing metadata additions

src/storage/postgres.rs
src/storage/redis_cache.rs
  serialization compatibility for new billing metadata
```

The implementation should avoid route-specific hacks for pool 15 or pool 4. This is an external-pool success invariant and should apply to all pools.

## Final Required Behavior

For every successful external-pool response:

```text
HTTP 200 normal response stays HTTP 200.
Downstream body contains Anthropic-compatible usage.
When pool mode is current_path_policy, downstream usage is reported/projected usage.
When pool mode is pass_through, downstream usage is raw usage.
Internal UsageRecord has raw/reported usage evidence.
Internal UsageRecord has externalPoolBilling.
Internal token/cost fields are not zero unless the actual reported usage is zero or pricing is unavailable.
Missing upstream usage is estimated, injected, billed, and explicitly marked.
```

## Fix Status And Validation

The implementation has been landed locally with the following behavior:

```text
1. Non-stream routes are no longer switched to the stream branch only because the upstream response header looks like SSE.
2. Non-stream JSON bodies are processed as JSON. If the upstream header is wrongly declared as text/event-stream but the body is JSON, the downstream content-type is corrected to application/json.
3. Non-stream success responses support top-level usage, wrapper usage, and OpenAI-style usage. Missing usage on a normal model response is conservatively estimated and injected instead of silently becoming zero billing.
4. Stream responses without terminal usage now receive a synthetic final usage event so billing is not left empty.
5. pass_through continues to preserve the body unchanged.
6. current_path_policy continues to shape both the response body and billing.
```

Validation completed:

```text
cargo fmt --check
git diff --check
cargo test external_pool:: -- --nocapture
cargo test
cargo build --release
```

Test counts:

```text
main tests: 1286 passed
kiro_loadtest tests: 26 passed
external_pool subset: 140 passed
```

The fake-upstream backup-pool integration test also passed:

```text
external_pool_fake_upstream_non_stream_json_with_sse_header_records_billing
```

That test returns:

```text
HTTP 200
content-type: text/event-stream
body: Anthropic JSON message with usage
route.stream=false
pool.usage_projection_mode=current_path_policy
```

It asserts:

```text
downstream HTTP 200
downstream content-type=application/json
downstream usage is projected
UsageRecord.status=success
rawUsage present
externalPoolBilling present
```

During release-build validation, one additional build-only issue was found and fixed: the input-token estimation helper used by the non-stream fallback path was test-only. It is now available in the release build, and `cargo build --release` passes.

The "do not affect other business logic" requirement is covered by existing and new tests for:

```text
pass_through preserves body
current_path_policy shapes as configured
normal model calls still return 200
stream paths still produce usage
```
