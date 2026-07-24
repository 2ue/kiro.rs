# Protocol / Social EventStream / Thinking Effort Live Evidence - 2026-07-25

Status: `final candidate validated / release pending`

Scope:

- Local service: existing `127.0.0.1:9022`, restarted with frozen release binaries built from the current working tree.
- Claude Code CLI: `2.1.197`.
- Real credential: one imported `social` / IDE credential in local validation state, only local credential id `6` enabled; local ids `1..5` disabled to avoid burning known bad accounts.
- Raw credential JSON, API keys, refresh tokens, access tokens, profile ARN, and Authorization headers were not printed.

## Root cause class A: JSON-labeled binary EventStream misclassified as protocol error

Production symptom:

- Production `.142` records showed `upstream_status=200`, `content_type=json`, `reason=api_protocol_error`.
- A real upstream probe later showed the IDE endpoint can return `HTTP 200` with `content-type: application/json` while the body is binary AWS EventStream containing normal `assistantResponseEvent`, `contextUsageEvent`, and `meteringEvent` frames.

Fix:

- `src/kiro/provider.rs`
  - `2xx + application/json` is no longer treated as a provider-terminal non-eventstream failure.
  - Provider now preserves response headers/body for handler body sniffing.
- `src/anthropic/handlers/tests.rs`
  - Added JSON-labeled binary EventStream fixture covering stream and non-stream.

Validation:

```text
json_content_type_response_headers_remain_for_handler_sniffing_for_five_rounds: passed
eventstream_content_type_json_body_remains_for_handler_sniffing_for_five_rounds: passed
handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds: passed
json_stream_sniffer_passes_binary_eventstream_mislabeled_as_json: passed
```

## Root cause class B: EventStream EOF without messageStatus but with metering/context was treated as failure

Real local reproduction before the EOF fix:

```text
status=200
content delta: OK
then event:error
service log: upstream eventstream ended without a trusted completion signal
usage: stream_error
credential id 6 temporarily cooled down
```

Fix:

- `src/anthropic/stream.rs`
  - `contextUsageEvent` and `meteringEvent` are tracked as trusted upstream terminal side signals.
  - Empty/unknown/silent EOF still fails closed.
- `src/anthropic/handlers.rs`
  - Non-stream complete EventStream path applies the same trusted-signal logic.
- `src/anthropic/handlers/tests.rs`
  - Added `TextWithMeteringNoStatus` fixture:
    - `assistantResponseEvent`
    - `contextUsageEvent`
    - `meteringEvent`
    - no `messageStatus`
  - Verified both stream and non-stream return success and preserve `kiro_metering_usage`.

Targeted validation:

```text
trusted_terminal_contract_rejects_silent_eof_and_keeps_legacy_terminals_for_five_rounds: passed
handler_legacy_metadata_metering_and_complete_tool_are_trusted_terminals_for_five_rounds: passed
handler_missing_completion_after_text_fails_closed_for_five_rounds: passed
handler_non_stream_untrusted_eof_fails_closed_for_five_rounds: passed
handler_eventstream_postcommit_faults_never_retry_or_fake_success_for_five_rounds: passed
handler_non_stream_eventstream_faults_fail_closed_for_five_rounds: passed
```

Real local validation after fix:

### Direct `/cc/v1/messages` stream

Request shape:

```json
{
  "model": "claude-haiku-4.5",
  "max_tokens": 32,
  "stream": true,
  "messages": [{"role": "user", "content": "Reply with exactly: OK"}]
}
```

Result:

```text
request_id=req_01rcdfQa9LLrPHEGFfeBF7CA
HTTP 200
content-type=text/event-stream
visible text=OK
message_stop count=1
event:error=false
```

Usage record:

```text
status=success
routeKind=local_credential
routeSubtype=local_success
credentialId=6
usageSource=context_estimate
totalInputTokens=4957
compatInputTokens=13
billableInputTokens=4957
outputTokens=1
cacheCreationInputTokens=4944
estimatedCostUsd=0.006198
originalCostUsd=0.004962
kiroMeteringUsage=0.006290560331674959
pricingModel=claude-haiku-4-5
downstreamStopReason=end_turn
```

### Direct `/cc/v1/messages` non-stream

Request shape:

```json
{
  "model": "claude-haiku-4.5",
  "max_tokens": 32,
  "stream": false,
  "messages": [{"role": "user", "content": "Reply with exactly: OK"}]
}
```

Result:

```text
request_id=req_01K443eSSVjKQrShiXTkZLgt
HTTP 200
type=message
visible text=OK
stop_reason=end_turn
usage non-zero
```

Usage record:

```text
status=success
routeKind=local_credential
routeSubtype=local_success
credentialId=6
usageSource=context_estimate
totalInputTokens=4957
compatInputTokens=17
billableInputTokens=4957
outputTokens=1
cacheCreationInputTokens=4940
estimatedCostUsd=0.006197000000000001
originalCostUsd=0.004961999999999999
kiroMeteringUsage=0.0033496648092868992
pricingModel=claude-haiku-4-5
downstreamStopReason=end_turn
```

Debug log confirmed the upstream non-stream response included:

```text
contextUsageEvent
meteringEvent
```

and did not log `api_protocol_error` or `upstream eventstream ended without a trusted completion signal`.

## Claude Code CLI protocol compatibility

### Simple stream-json

Command shape:

```text
ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc
claude --bare --verbose --no-session-persistence --model claude-haiku-4.5 \
  --output-format=stream-json --include-partial-messages --print 'Reply exactly: cli-ok'
```

Result:

```text
Claude CLI exit=0
contains cli-ok=true
stream event includes message_stop
stderr empty
errorsCount=0
```

CLI final usage:

```text
input_tokens=9
cache_creation_input_tokens=6970
output_tokens=2
```

Server usage for `req_018At1wXGwgK3zHkH127UurM`:

```text
status=success
routeKind=local_credential
routeSubtype=local_success
credentialId=6
totalInputTokens=6979
compatInputTokens=9
outputTokens=2
cacheCreationInputTokens=6970
kiroMeteringUsage=0.00905493744610282
estimatedCostUsd=0.008731500000000001
pricingModel=claude-haiku-4-5
```

### Bash tool-use stream-json

Command shape:

```text
ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc
claude --bare --verbose --no-session-persistence --model claude-haiku-4.5 \
  --permission-mode bypassPermissions --tools Bash \
  --output-format=stream-json --include-partial-messages \
  --print 'Use Bash to run exactly: printf tool-ok. Then answer with exactly the command output.'
```

Result:

```text
Claude CLI exit=0
contains tool-ok=true
tool_use/tool_result present
stderr empty
errorsCount=0
```

Leak marker scan:

```text
Tool results provided: absent
<function_results>: absent
bashHash/readHash/editHash/writeHash: absent
user Continue: absent
```

Server usage:

```text
req_01fwu3PBdcumBkmgGJhHkJBV:
  status=success
  routeSubtype=local_success
  stop_reason=tool_use
  outputTokens=18
  kiroMeteringUsage=0.011784541094527364

req_01uQopXW4SASnsfu3zRJS1i8:
  status=success
  routeSubtype=local_success
  stop_reason=end_turn
  outputTokens=184
  kiroMeteringUsage=0.012613988988391374
```

## Thinking / effort mapping

External references consulted:

- Kiro CLI effort docs: `https://kiro.dev/docs/cli/chat/effort/`
  - The Kiro docs describe Claude models as using `output_config.effort` together with `thinking.type` and `thinking.display`.
  - The same page lists `low`, `medium`, `high`, `xhigh`, and `max` as supported effort values where supported by the model.
- Claude Platform thinking steering docs: `https://platform.claude.com/docs/en/build-with-claude/thinking-steering-and-cost`
  - Claude adaptive thinking can choose whether and how much to think per request; effort is guidance, not a guarantee that every turn emits thinking.

Fix:

- `src/anthropic/converter/model.rs`
  - Native Kiro `output_config` path now sends:

    ```json
    {
      "additionalModelRequestFields": {
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": "<selected>"}
      }
    }
    ```

  - If visible thinking is explicitly triggered, it sends:

    ```json
    {
      "thinking": {"type": "adaptive", "display": "summarized"},
      "output_config": {"effort": "<selected>"}
    }
    ```

  - Explicit `output_config.effort=max` is preserved as `max`; it is not downgraded to `high`.
- `src/anthropic/handlers.rs`
  - Debug conversion summary now includes safe structural fields:
    - `reasoning_path`
    - `reasoning_effort`
    - `native_thinking_type`
    - `native_thinking_display`
  - It still does not log request body, credentials, tokens, or prompt text.

Targeted tests:

```text
explicit_max_output_config_effort_survives_authoritative_wire_conversion_five_rounds: passed
native_output_config_visible_thinking_sets_summarized_display_for_five_rounds: passed
omitted_output_config_effort_uses_authoritative_max_wire_default_for_five_rounds: passed
explicit_high_output_config_effort_survives_authoritative_wire_conversion_five_rounds: passed
enabled_thinking_budget_remains_authoritative_over_omitted_output_effort_five_rounds: passed
test_ide_does_not_invent_thinking_for_output_config_effort: passed
test_ide_preserves_existing_schema_owned_thinking_field: passed
```

Real direct request:

```json
{
  "model": "claude-sonnet-4.6",
  "max_tokens": 256,
  "stream": true,
  "thinking": {"type": "adaptive"},
  "output_config": {"effort": "max"},
  "messages": [{"role": "user", "content": "ultrathink briefly, then reply exactly: think-ok"}]
}
```

Result:

```text
request_id=req_01QLqyRf2H7xKtSZeCzXGdiU
HTTP 200
message_stop count=1
event:error=false
visible text=think-ok
thinking_delta present=true
output_tokens_details.thinking_tokens=1
```

Server usage:

```text
status=success
model=claude-sonnet-4.6
upstreamModel=claude-sonnet-4.6
routeSubtype=local_success
totalInputTokens=5001
compatInputTokens=10
outputTokens=3
kiroMeteringUsage=0.02579119930348259
pricingModel=claude-sonnet-4-6
firstThinkingDeltaMs=2481
```

Debug conversion summary:

```text
reasoning_path=Some("output_config")
reasoning_effort=Some("max")
reasoning_source=Some("kiro_model_schema")
```

Real Claude Code CLI `--effort max`:

```text
claude --bare --verbose --no-session-persistence --model claude-sonnet-4.6 \
  --effort max --output-format=stream-json --include-partial-messages \
  --print 'ultrathink briefly, then reply exactly: cli-think-ok'
```

Result:

```text
Claude CLI exit=0
contains cli-think-ok=true
thinking_delta present=true
message_delta.usage.output_tokens_details.thinking_tokens=4
stderr empty
errorsCount=0
request_id=req_01GujCBenEN4NwCqCWpCqXvH
```

Server usage:

```text
status=success
model=claude-sonnet-4.6
upstreamModel=claude-sonnet-4.6
routeSubtype=local_success
totalInputTokens=6775
compatInputTokens=5
outputTokens=7
kiroMeteringUsage=0.03333279336650083
pricingModel=claude-sonnet-4-6
firstThinkingDeltaMs=1846
```

### Important Claude Code CLI ingress finding

A temporary local logging proxy captured the actual JSON Claude Code CLI 2.1.197 sends to `/cc/v1/messages` for `--effort max`. The proxy did not log Authorization headers or full body text.

Captured protocol fields:

```json
{
  "model": "claude-sonnet-4.6",
  "stream": true,
  "max_tokens": 32000,
  "thinking": {"type": "disabled"},
  "output_config": null,
  "top_level_effort": null,
  "toolCount": 0
}
```

followed by the real model request:

```json
{
  "model": "claude-sonnet-4.6",
  "stream": true,
  "max_tokens": 32000,
  "thinking": {"type": "enabled", "budget_tokens": 31999},
  "output_config": null,
  "top_level_effort": null,
  "toolCount": 3
}
```

Additional captures for `--effort medium`, `--effort high`, and `--effort xhigh` showed the same visible protocol shape: no `output_config`, no top-level effort, and `thinking.enabled + budget_tokens=31999`.

Conclusion:

- The proxy is no longer downgrading explicit `output_config.effort=max`; that path is fixed and directly tested.
- Claude Code CLI 2.1.197 does not expose the selected `--effort` level in the JSON fields visible to the proxy. It sends `thinking.enabled + budget_tokens=31999`.
- Because medium/high/xhigh/max look identical at the inbound JSON layer for this CLI version, the proxy cannot safely infer `max` from `budget_tokens=31999`. Mapping that value to `max` would incorrectly upgrade all captured CLI effort levels to `max`.

## Broad targeted regression batch

Command batch:

```text
feature/tests/run-cargo-scoped.sh broad-targeted-regression -- bash -lc '...'
```

Passed tests:

```text
explicit_max_output_config_effort_survives_authoritative_wire_conversion_five_rounds
handler_legacy_metadata_metering_and_complete_tool_are_trusted_terminals_for_five_rounds
handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds
handler_json_stream_secret_markers_never_reach_logs_or_usage_for_five_rounds
handler_missing_completion_after_text_fails_closed_for_five_rounds
synthetic_thinking_activation_never_forges_client_effort_five_rounds
test_process_message_content_rejects_fake_declared_image
test_process_message_content_accepts_base64_image_source
canonical_pricing_model_maps_aliases_and_thinking_suffixes
pricing_sync_candidates_include_capability_models_and_family_version_fallbacks
non_stream_unknown_json_without_usage_injects_estimated_usage_and_billing
non_stream_unknown_text_without_usage_records_estimated_billing_without_rewriting_body
openai_usage_is_normalized_for_non_stream_external_pool_body
request_rejection_reason_count_covers_every_index
```

Batch hygiene:

```text
git diff --check: passed
cargo fmt --check: passed
scoped target cleanup: removed=true reservation_released=true
```

## Remaining validation before release

- None for this evidence slice. The remaining release work is version/tag/push discipline and any separate production deployment monitoring explicitly requested by the operator.

## Final candidate rerun - 2026-07-25

Frozen `kiro-rs` candidate:

```text
path=/var/folders/.../kiro-cli-candidate.q4sULL/kiro-rs
sha256=25ea01fb741bdffb103fa95397f0fb29b60c8bffee9267741f563f388ae237a4
service=existing local 127.0.0.1:9022
pid=49735
Claude Code CLI=2.1.197
```

The local service was restarted directly on the existing `9022` process. No separate real-upstream service was started for normal CLI validation. Isolated ports were used only for fake-upstream load/chaos validation.

### Static and source gates

```text
git diff --check: passed
cargo fmt --check: passed
cargo test --bin kiro-rs -- --test-threads=2: passed
  1784 passed, 0 failed, 6 ignored
node feature/tests/check-feature-docs.mjs: passed
  50 issue documents, 123 relative links
node --test feature/tests/*.test.mjs: passed
  261 passed, 22 skipped, 0 failed
node feature/tests/inventory-build-artifacts.mjs --gate: passed
  targets=0 reservations=0 target_processes=0 blockers=0
```

One full-tree red item was found and fixed during this rerun:

```text
kiro::provider::tests::provider_sends_converter_max_effort_without_inventing_thinking_for_five_rounds
```

The old test expected `additionalModelRequestFields` to contain only `output_config.max`. That contradicted the corrected native Kiro contract for output-config reasoning, where the final wire must contain both:

```json
{
  "thinking": {"type": "adaptive"},
  "output_config": {"effort": "max"}
}
```

The test was renamed to `provider_sends_converter_max_effort_with_native_adaptive_thinking_for_five_rounds` and now asserts:

- `effort=max` reaches final wire unchanged;
- `thinking.type=adaptive` is present;
- Anthropic-only `budget_tokens` is not sent to Kiro native adaptive thinking.

The targeted test and full `cargo test --bin kiro-rs` both passed after the correction.

### Final direct protocol smoke

Direct stream:

```text
model=claude-haiku-4.5
HTTP 200
content-type=text/event-stream
text=final-stream-ok
message_stop present=true
event:error=false
final usage: input_tokens=6, cache_creation_input_tokens=4961, output_tokens=4
usage record=req_01zTw4N2BT1zjB9Dj6TZBTKp
status=success
routeSubtype=local_success
pricingModel=claude-haiku-4-5
kiroMeteringUsage=0.006603686633499171
```

Direct non-stream:

```text
model=claude-haiku-4.5
HTTP 200
type=message
text=final-nonstream-ok
stop_reason=end_turn
usage: input_tokens=62, cache_creation_input_tokens=4906, output_tokens=6
usage record=req_01PyMPfCFKwMz3hLEbQmqMof
status=success
routeSubtype=local_success
pricingModel=claude-haiku-4-5
kiroMeteringUsage=0.003777497379767828
```

Direct `thinking.adaptive + output_config.effort=max`:

```text
model=claude-sonnet-4.6
HTTP 200
content-type=text/event-stream
text=final-think-ok
thinking block starts=1
thinking deltas=1
final usage: input_tokens=14, cache_creation_input_tokens=4992, output_tokens=8
final usage output_tokens_details.thinking_tokens=4
event:error=false
usage record=req_012sgLrDSyGquBRht8opDBef
status=success
routeSubtype=local_success
pricingModel=claude-sonnet-4-6
kiroMeteringUsage=0.026230265356550583
```

### Final Claude Code CLI smoke

Simple CLI stream-json:

```text
command: claude --bare --verbose --no-session-persistence --model claude-haiku-4.5 --output-format=stream-json --include-partial-messages
text=final-cli-ok / final-cli-ok-2
exit=0
final usage non-zero=true
leak patterns found=false
stderr only contained Claude workspace trust warning
```

Bash tool:

```text
command: claude --bare --verbose --no-session-persistence --model claude-haiku-4.5 --permission-mode bypassPermissions --tools Bash --output-format=stream-json --include-partial-messages
tool_use=1
tool_result=1
text=final-tool-ok
final usage non-zero=true
leak patterns found=false
```

Thinking CLI:

```text
command: claude --bare --verbose --no-session-persistence --model claude-sonnet-4.6 --effort max --output-format=stream-json --include-partial-messages
text=final-cli-think-ok
thinking blocks=1
thinking deltas=1
final usage non-zero=true
leak patterns found=false
```

Claude CLI `result.usage` in this version does not expose `output_tokens_details.thinking_tokens`; direct SSE above proves real thinking tokens for the proxy/upstream path.

Multi-turn CLI session:

```text
session id fixed across 3 print invocations
turn 1 text=mt-turn-1-ok
turn 2 text=mt-alpha-729
turn 3 Bash tool_use=1, tool_result=1, text=mt-tool-3-ok
all turns final usage non-zero=true
leak patterns found=false
```

MCP CLI:

```text
server=.local-run/cc-real-tests/mcp-ping-server.js
config=.local-run/cc-real-tests/mcp-config.json
tool_use=mcp__kiro-local-test__ping
tool_result contains=mcp-pong-kiro-local
final text=final-mcp-ok
final usage non-zero=true
leak patterns found=false
```

The first MCP prompt successfully called the MCP tool but produced final text `Ready.`. It was classified as model instruction-following noise, not protocol failure, because `tool_use`, `tool_result`, usage, and leak checks were correct. A stronger final-answer prompt then produced `final-mcp-ok`.

### Image and WebSearch direct smoke

Image:

```text
valid RGB 16x16 PNG: success
text=final-image-ok
usage non-zero=true
kiroMeteringUsage=0.006530426666666667
```

Bad image:

```text
declared image/png with invalid bytes: local reject
public error=invalid_request_error
message=invalid image data for media_type: image/png
usage record=req_01FbRDgE41yTHYCzuBrtRT1Y
status=error
model=unknown
errorMessage=request rejected before upstream dispatch
kiroMeteringUsage=0
```

An earlier 1x1 gray+alpha PNG was rejected by upstream as `image_invalid_bad_request` while payload guard showed no local body mutation. A standard RGB PNG passed, so the proxy image conversion path is not corrupting valid image bodies.

WebSearch:

```text
request tools=[{"type":"web_search_20250305","name":"web_search","max_uses":1}]
HTTP 200
server_tool_use blocks=1
web_search_tool_result blocks=1
message_stop present=true
errors=0
final usage: input_tokens=946, output_tokens=721
```

### Fake-upstream load/chaos validation

`kiro_loadtest` binary:

```text
path=/var/folders/.../kiro-loadtest-bin.nAer2f/kiro_loadtest
sha256=da338c62b21a22f061e5eb5dbd2f26f60ab59e34255703fcf93aa5ece819d13f
```

Load/chaos used fake upstream and temporary proxy ports. It did not send load to real `9022` or real upstream accounts.

L1 fake server smoke:

```text
normal-stream requests=20 concurrency=5
success=20 errors=0
statusCounts={"200":20}
ttfbMs.p95=2
totalLatencyMs.p95=3
```

L3 burst/recovery:

```text
passed=true
cases=9/9
normal c1/c5/c10/c40 spike all success
post-spike recovery success=10/10
error burst recovered=true
invalid-tool burst error-classified and recovered=true
max spike ttfbMs.p99=46
max spike totalLatencyMs.p99=52
```

L4 chaos:

```text
passed=true
cases=12/12
covered proxy restart, 429 burst, 500 burst, invalid-tool burst, client-drop, mixed-chaos, and recovery
mixed-chaos requests=96 success=29 errors=67
mixed-chaos recovery success=12/12
post-recovery ttfbMs.p95=8
post-recovery totalLatencyMs.p95=20
```

L5 short soak:

```text
60s load + 5s idle: business requests passed, FD returned, RSS did not return within the 32MiB threshold
classification: not accepted as pass; rerun with default-style longer idle required
```

L5 formal short soak with 60s idle:

```text
passed=true
durationSecs=60
idleCooldownSecs=60
long-stream requests=441 success=441 errors=0
post-soak recovery success=12/12
rssReturnedWithin32MiB=true
fdReturnedWithin5=true
idle rssBytes=47431680
idle fdCount=31
```

Temporary load-chaos PostgreSQL databases were dropped after the run. Redis prefixes were cleaned by the runner. Raw runtime roots were not retained.
