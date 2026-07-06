# Request Body Capability Modularization Plan

Date: 2026-07-06

Status: implemented and validated for the current refactor.

This document complements `docs/request-pipeline-modularization-analysis-20260706.md`. The earlier document recorded the first file-level split. This plan focuses on the next step: making body processing capabilities explicit, so local credentials, external normalized pools, and external raw pools can reuse the same mental model without sharing accidental branches.

## Goal

Refactor request processing into explicit capability stages while preserving caller-visible behavior:

```text
raw request
  -> cheap raw facts when needed
  -> route target selection
  -> target capability profile
  -> enabled body/model/payload stages only
  -> upstream call
  -> independent usage projection, pricing, logs, errors
```

The important distinction is that "body processing" is not one thing. It currently contains thinking controls, multimodal materialization, Anthropic-to-Kiro conversion, tool/schema cleanup, payload guard, token counting, and diagnostics. These should be named stages, with existing defaults kept intact.

## Current Inventory

### Parse Entry

- `request_entry::handle_messages_endpoint` first tries raw external direct and raw local-preflight fallback before parsing.
- If those do not match, it parses `MessagesRequest` and enters `post_messages_inner`.

### Parsed Preprocessing

`post_messages_inner` currently runs these before final target selection:

- thinking override from model name
- thinking trigger mode
- thinking trace logging
- multimodal source handling via `body_processing::prepare_multimodal_sources`

This preserves current behavior, but it means parsed external direct/normalized fallback pays multimodal processing cost before route completion.

### Local Body

`local_body_pipeline::prepare` currently owns:

- `convert_request_with_resolved_model`
- Kiro request construction
- too-long retry body capture
- `prepare_kiro_request_body`
- payload guard report logging and byte breakdown
- local token counting
- warning header construction
- cache-point retry body capture

### External Body

`external_pool/body_pipeline.rs` already branches by selected pool:

- `RawPassthrough`: original bytes, optional raw model probe/rewrite.
- `Normalized`: parsed payload, optional external payload guard, outbound model mapping, thinking normalization.

This is the correct direction, but the branch should be represented by a plan/profile so future toggles do not become nested ad hoc checks.

### Independent Usage

`external_pool/usage_projection.rs` is already separate and should stay independent from raw/normalized body mode.

## Target Profiles

### LocalCredential

Default enabled stages:

- parsed thinking preprocessing
- multimodal preprocessing from current `imageProcessing`
- model resolution from local capabilities
- Anthropic-to-Kiro conversion
- tool/schema/thinking/doc/image compatibility conversion
- Kiro payload guard according to current runtime config
- token counting
- diagnostics and retry body capture

### ExternalNormalized

Default enabled stages:

- parsed body availability
- current external payload guard behavior
- outbound model mapping
- external thinking normalization
- usage projection according to external pool and path policy

### ExternalRaw

Default enabled stages:

- raw bytes only
- optional raw top-level model probe/rewrite from current `rawModelMode`
- usage projection according to external pool and path policy

Default disabled stages:

- parsed multimodal materialization
- Anthropic-to-Kiro conversion
- tool/schema cleanup
- payload guard
- token counting unless usage projection explicitly needs it

## Refactor Phases

1. Documentation and plan-tree registration. Done.
2. Add explicit capability plan types for parsed Anthropic, local Kiro, and external body pipelines. Done.
3. Wire existing functions through these plans with defaults matching existing behavior. Done.
4. Add branch tests proving. Done:
   - raw external stays raw
   - raw model probe does not mutate body
   - raw model rewrite mutates only top-level `model`
   - normalized external still runs payload guard when enabled
   - local default still builds Kiro body and retry payloads
5. Run static gates and fake upstream load/chaos. Done.
6. Record validation results in the plan status. Done.

## Validation Matrix

Static:

- `cargo fmt --check`
- `git diff --check`
- `cargo test`
- `cargo build --release`

Fake upstream:

- normal stream
- normal non-stream
- slow first byte
- slow thinking then text
- long stream
- 429
- 500
- recovery after burst
- long context payload
- large tool result payload
- deep tool input payload
- many tools payload
- mixed pathological payload with long stream

Resource evidence:

- CPU start/peak/end
- RSS start/peak/end
- FD start/peak/end
- p50/p95/p99 TTFB
- p50/p95/p99 total latency
- status distribution

## Compatibility Invariants

- Existing successful requests should continue to succeed.
- Raw external pools must not be ignored when explicit direct is off.
- Usage shaping must follow usage settings, not body mode.
- Payload guard disabled must avoid guard work.
- `/cc/v1/messages` behavior must not diverge from the shared messages entry path.
- Kiro local conversion stays compatible with current tool-use, thinking, image, document, and cache semantics.

## 2026-07-06 Landing Summary

Implemented files:

- `src/anthropic/body_capabilities.rs`
- `src/anthropic/handlers/parsed_body_pipeline.rs`
- `src/anthropic/handlers.rs`
- `src/anthropic/handlers/local_body_pipeline.rs`
- `src/external_pool/body_pipeline.rs`
- `src/bin/kiro_loadtest.rs`
- `docs/testing/loadtest.md`

Validation:

- Static and unit gates passed.
- Fake upstream validation covered raw and normalized external pools.
- Slow first byte now includes fixed, random, dense, and tiered 3/10/22 second cases.
- Mixed chaos combines 429, 500, tiered slow first byte, random slow first byte, long stream, normal responses, long context, deep tool input, many tools, and large tool results.

## 2026-07-06 Full Refactor Completion

The second pass completed the deeper converter split and runtime/UI configuration work.

Implemented converter modules:

- `src/anthropic/converter/schema.rs`: JSON schema normalization and unsupported field cleanup.
- `src/anthropic/converter/model.rs`: model mapping and Kiro native reasoning fields.
- `src/anthropic/converter/content.rs`: text, image, document, tool_use, and tool_result content conversion.
- `src/anthropic/converter/tools.rs`: tool schema conversion, tool-name mapping, tool-choice steering, and chunked tool policy.
- `src/anthropic/converter/tool_pairing.rs`: tool_use/tool_result pairing repair.
- `src/anthropic/converter/history.rs`: system/history/current message construction.

Implemented configuration surfaces:

- `BodyConversionConfig` in `src/model/config.rs`, defaulting all compatibility capabilities on to preserve current behavior.
- Runtime admin response/update wiring in `src/admin/types.rs` and `src/admin/service.rs`.
- Request-time wiring through `AppState`, `RequestRuntimeConfig`, `LocalKiroBodyPlan`, and `ConverterOptions`.
- UI toggles in both frontends:
  - `ui/src/features/runtime/runtime-page.tsx`
  - `admin-ui/src/components/runtime-config-panel.tsx`

Current boundaries:

- Scheduler/route selection remains separate from body preparation.
- Local credentials use `LocalKiroBodyPlan` and `BodyConversionConfig`.
- External normalized pools use normalized body processing and external payload guard/model/thinking stages.
- External raw pools use raw bytes, with only optional raw model probe/rewrite.
- External usage projection and billing remain independent from body mode.

Final validation run:

- Run directory: `target/loadtest/modular-20260706160929`.
- Summary file: `target/loadtest/modular-20260706160929/final-validation-summary.json`.
- Static gates:
  - `cargo fmt --check`: pass.
  - `git diff --check`: pass.
  - `pnpm check` in `ui/`: pass.
  - `pnpm exec tsc -b --pretty false` in `admin-ui/`: pass.
  - `CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test`: pass, 904 main tests and 19 `kiro_loadtest` tests.
  - `CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo build --release`: pass, 3m39s.

Representative fake-upstream proxy results:

- `normalized-normal-stream.json`: 20/20 success.
- `normalized-normal-non-stream.json`: 12/12 success.
- `normalized-thinking.json`: 12/12 success, p95 first thinking 39 ms, p95 first text 1596 ms.
- `normalized-tool-use.json`: 12/12 success.
- `normalized-tiered-slow-first-byte.json`: 9/9 success, p95 TTFB 22053 ms.
- `normalized-payload-mixed-long-stream.json`: 24/24 success, p95 total latency 6424 ms, RSS 33.6 MB -> 128.7 MB -> 110.8 MB, CPU peak 26.7%.
- `normalized-burst-high-concurrency.json`: 120/120 success.
- `normalized-recovery-after-429.json`: 12/12 success after a 429 burst and cooldown.
- `normalized-recovery-after-500.json`: 12/12 success after a 500 burst and cooldown.
- `normalized-client-drop-retry.json`: upstream returned 200 for 12/12 while the client intentionally dropped streams, counted as expected client-side errors.
- `normalized-mixed-chaos-multipool.json`: 36/36 final success with 429/500/slow-first-byte/long-stream mixed upstream behavior and failover.
- `normalized-sustained-60s-c40.json`: 25323/25323 success over 61.1s, about 24852 RPM, p95 TTFB 174 ms, p95 total latency 175 ms.
- `raw-explicit-direct-normal-stream.json`: 16/16 success.
- `raw-explicit-direct-non-stream.json`: 8/8 success.
- `raw-explicit-direct-long-stream.json`: 24/24 success, p95 total latency 6192 ms, RSS 35.2 MB -> 81.9 MB -> 78.2 MB, CPU peak 8.7%.
- `raw-model-rewrite.json`: 3/3 success; fake upstream captured top-level `model=mapped-raw-sonnet-45`.
- `raw-model-none.json`: 3/3 success; fake upstream captured original `model=claude-sonnet-4.5`.
- `raw-fallback-no-explicit-direct.json`: 12/12 success with `externalDirectPolicyEnabled=false`, proving raw pools are not ignored in ordinary fallback.

Usage evidence:

- Raw fallback usage record had `externalPoolId=2`, `usageProjectionApplied=true`, and `externalPoolBilling.usageProjectionMode=current_path_policy`.
- Raw body mode did not bypass usage projection or external-pool billing.

Sustained high-concurrency resource evidence:

- `normalized-sustained-60s-c40.json`: RSS 22.3 MB -> 80.5 MB -> 70.2 MB during the sampled run, FD 29 -> 118 -> 118, CPU peak 80.2%, end CPU 8.5%.
- After 8 seconds idle, `sustained-post-idle-resource.txt` showed RSS about 14.5 MB and FD count 37.

Remaining design work:

- Route planner extraction is still deferred. It can reduce parsed-body work for some future non-raw external routes, but it changes target selection timing and should be done separately.
- A full plugin/trait runtime for every processing stage is still deferred. The current implementation gives explicit module boundaries and focused config toggles without changing the caller-visible protocol.
