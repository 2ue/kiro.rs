# Kiro upstream protocol malformed/context analysis - 2026-06-17

## Scope

This note records the current local analysis and code changes for Kiro upstream protocol compatibility. It focuses on:

- `400 Bad Request {"message":"Improperly formed request.","reason":null}`
- Claude/Kiro tool-use transcript shape
- Kiro `contextUsageEvent` percentage based token reconstruction
- Sonnet/free-account context-window behavior
- Local real-service validation through `http://127.0.0.1:9022/cc`

## Real Local Evidence

Local service health:

```text
GET http://127.0.0.1:9022/healthz -> {"service":"kiro-rs","status":"ok"}
```

Service restarted with rebuilt local binary:

```text
./target/debug/kiro-rs -c config.json --credentials credentials.json
PID: 3007
Log: .local-run/kiro-protocol-20260617/server-after2.log
```

Real local Sonnet request after the final fix:

```text
POST /cc/v1/messages model=sonnet
HTTP/1.1 200 OK
Result: assistant text OK, stop_reason=end_turn
```

Real local tool request:

```text
POST /cc/v1/messages model=sonnet with readFile tool
HTTP/1.1 200 OK
Result: assistant returned structured tool_use, stop_reason=tool_use
tool_name=readFile
text_has_xml=false
```

Real local tool-result follow-up:

```text
history: user -> assistant tool_use -> user tool_result
HTTP/1.1 200 OK
Result: assistant final text, stop_reason=end_turn
```

Real local duplicate-id protocol test:

```text
history:
  user
  assistant tool_use id=toolu_dup_real
  user tool_result toolu_dup_real
  assistant tool_use id=toolu_dup_real
current:
  user tool_result toolu_dup_real

HTTP/1.1 200 OK
Result: assistant returned structured tool_use, stop_reason=tool_use
Payload guard log: renamed_duplicate_tool_uses=1, still_oversized=false
No "Improperly formed request" in the test log for this request.
```

This duplicate-id test is intentionally synthetic, but it matches a real malformed-risk class: a set-based checker can treat reused ids as globally paired, while Kiro expects adjacent tool-use/tool-result structure. The final body repair renamed the later duplicate tool-use id and rewrote its adjacent current tool result before sending to Kiro.

These tests prove the local free Sonnet path can complete normal text, tool-use, tool-result, and a duplicate-id repair scenario against real Kiro upstream. They do not prove every large/malformed production transcript is fixed; they validate that the fixed paths preserve normal behavior and eliminate one concrete malformed request class.

## Context Window Findings

Kiro has two separate limits that must not be mixed:

1. Model context window, reported by Kiro model capabilities and surfaced indirectly through `contextUsageEvent.contextUsagePercentage`.
2. Serialized request body shape/size, which can cause `Improperly formed request` even when the model context window is not full.

The code already uses Kiro model catalog data first:

```text
prepare_usage_context:
  model_capabilities.max_input_tokens_for(upstream_model)
  else get_context_window_size(...)
```

The correct rule is:

- If Kiro `ListAvailableModels` says the resolved upstream model is 200K, use 200K.
- If Kiro `ListAvailableModels` says the resolved upstream model is 1M, use 1M.
- Only use `get_context_window_size()` as a fallback when the catalog is missing.

Current local/free catalog behavior observed earlier:

```text
requested_model=claude-sonnet-4-6
upstream_model=claude-sonnet-4.5
context_window=200000
resolution=family_normalized
```

This explains why local free Sonnet cannot be assumed to support 1M. Other accounts may have `auto` or dot-minor `claude-sonnet-4.6` with 1M, but that must come from the real Kiro catalog.

Code change:

- `get_context_window_size()` is now conservative for fallback aliases like `sonnet` and dash variants.
- Plain `sonnet`, `opus`, `claude-sonnet-4-6`, and `claude-opus-4-7` fallback to 200K unless the catalog says otherwise or `[1m]` is explicit.
- Dot-minor `claude-sonnet-4.6`, `claude-opus-4.6`, `claude-opus-4.7`, and `auto` remain 1M fallback candidates.

Regression coverage:

- `anthropic::converter::tests::test_context_window_size_for_kiro_auto_and_dash_variants`
- `anthropic::model_capabilities::tests::catalog_context_window_for_sonnet_follows_real_upstream_model`

## `Improperly formed request` Findings

The upstream error:

```text
400 Bad Request {"message":"Improperly formed request.","reason":null}
```

is a Kiro request-body validation failure. It is not necessarily caused by local token counting. It can be caused by:

- current user has `tool_result` that does not belong to the immediately previous assistant tool call
- assistant history has an unpaired `tool_use`
- user history has orphan `tool_result`
- duplicate `tool_use_id` across assistant turns
- duplicate `tool_result` for the same id in one user turn
- tool schema/name issues
- oversized serialized request body
- empty Kiro `content` when only tool results or tool uses exist

Before this change, `payload_guard` already repaired:

- leading assistant history after trimming
- empty `tool_uses` arrays
- orphan historical/current `tool_result`
- unpaired `tool_use`
- current/historical tool-result truncation for payload shaping
- empty content placeholders for tool-only/tool-result-only cases through converter

The missing strictness was occurrence-aware duplicate handling and consistent current-turn adjacency. A set-based pairing check can incorrectly treat duplicate ids as paired. Example:

```text
assistant tool_use id=X
user tool_result X
assistant tool_use id=X
user tool_result X
```

Using only a `HashSet`, the second assistant tool use can appear valid because `X` existed earlier. Kiro can reject such a transcript as improperly formed.

There is a second edge: after conversion, the final Anthropic user message is removed from history and becomes Kiro `currentMessage`. Therefore the immediately previous assistant tool-use can be the last item in Kiro history. Current `tool_result` must be allowed to pair with that last assistant item, while a result for an older non-adjacent assistant must be textified or dropped from structured `toolResults`.

## Implemented Fix

`src/anthropic/payload_guard.rs` now performs final Kiro-body repair for:

- duplicate `tool_use_id` inside a single assistant message: remove duplicate/empty ids
- repeated `tool_use_id` across history turns: rename the later tool_use to a deterministic unique id and rewrite the immediately following `tool_result` id to match
- repeated `tool_use_id` in the final assistant before current message: rename the assistant id and rewrite current `tool_result`
- duplicate current `tool_result`: keep the first structured result and append duplicate content to user text
- duplicate historical `tool_result`: keep the first structured result

This is deliberately applied after Anthropic-to-Kiro conversion, because it validates the actual body sent to Kiro, not just the original Anthropic messages.

`src/anthropic/converter.rs` now performs current-turn pairing with the same adjacency rule:

- current `tool_result` may pair with the last assistant `tool_use` in history
- current `tool_result` for an older assistant is removed from structured `toolResults` and kept as plain text in compatibility mode
- current-paired ids are excluded from historical orphan cleanup, so the converter does not delete the assistant `tool_use` that the current `tool_result` is about to satisfy
- reused ids across turns are preserved long enough for the final Kiro payload guard to rename the later occurrence and synchronize the adjacent result

New report fields were added with `serde(default)` for backward compatibility:

```text
removedDuplicateToolUses
renamedDuplicateToolUses
removedDuplicateToolResults
textifiedDuplicateToolResults
```

The end-user malformed error no longer says "conversion":

```text
Request body is improperly formed. Check message ordering, tool_use/tool_result pairing, tool schema, multimodal sources, and payload size.
```

Provider-side Chinese label also no longer says "Anthropic 到 Kiro 的 payload 转换".

The payload guard log now includes duplicate-id repair counters:

```text
removed_duplicate_tool_uses
renamed_duplicate_tool_uses
removed_duplicate_tool_results
textified_duplicate_tool_results
```

This matters operationally because a real production log can now distinguish "payload guard trimmed history" from "payload guard repaired repeated tool ids".

## Why This Should Not Reduce Model Intelligence

The change avoids semantic degradation by preferring structure-preserving repair:

- It renames duplicated tool-use ids and synchronizes adjacent tool results instead of deleting valid tool-use/result pairs.
- It only textifies duplicate current tool results when a second structured result for the same id would violate protocol shape.
- It does not truncate current content unless existing payload-shaping config already requires it.
- It does not alter normal text-only or correctly paired tool transcripts.
- It uses real Kiro model catalog context windows first, avoiding incorrect 1M/200K usage reconstruction.

Remaining risk:

- If a client sends logically contradictory duplicate tool results, the second result becomes plain text. That is preferable to sending invalid structured JSON that Kiro rejects.
- This does not solve upstream overload, 429, model temporarily unavailable, or account-level free model limits.

## Tests Run

Targeted:

```text
cargo test --locked --no-default-features payload_guard
30 passed

cargo test --locked --no-default-features converter
64 passed

cargo test --locked --no-default-features model_capabilities
24 passed

cargo test --locked --no-default-features classifies_bad_request_protocol_reasons
1 passed
```

Full:

```text
PATH=/Users/yuanfeijie/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin \
  cargo test --locked --no-default-features --quiet
598 passed
```

Local toolchain note:

```text
The first local cargo test attempts failed before running tests because PATH resolved `cc`
to `/Users/yuanfeijie/.volta/bin/cc` instead of Apple clang. The failing linker/build-script
errors were `unknown command .../symbols.o` and `unknown option '-O0'` from the Volta shim.
Pinning PATH so `cc` resolves to `/usr/bin/cc` made the same test suite pass.
```

Real local service after final restart:

```text
healthz -> ok
text request -> HTTP 200, OK
tool request -> HTTP 200, structured tool_use, no XML leakage
tool_result follow-up -> HTTP 200, final assistant text
duplicate reused tool_use_id -> HTTP 200, payload guard log renamed_duplicate_tool_uses=1
```

Latest real service check against the current local service on `http://127.0.0.1:9022/cc`:

```text
Service process: ./target/debug/kiro-rs -c config.json --credentials credentials.json
ccman current: local-kiro-rs-9022 -> http://127.0.0.1:9022/cc

assistant-prefill risk request:
  input: final assistant message after user
  behavior: converter logged final non-user/prefill dropped
  upstream result: HTTP 200, text OK

duplicate tool_use_id risk request:
  input: two assistant turns reused tool_use_id=toolu_dup_final_check
  behavior: payload guard logged renamed_duplicate_tool_uses=1
  upstream result: HTTP 200

No `Improperly formed request` appeared in the server log delta for these two requests.
Artifacts:
  .local-run/kiro-real-20260617-current/final-check/assistant-prefill.*
  .local-run/kiro-real-20260617-current/final-check/duplicate-tool-id.*
  .local-run/kiro-real-20260617-current/final-check/server-delta.log
```

Real `/cc/v1/messages` matrix already saved under `.local-run/kiro-real-20260617-current`:

```text
01-minimal-nonstream              HTTP 200
02-minimal-stream                 HTTP 200
03-tool-use                       HTTP 200
04-tool-result                    HTTP 200
05-duplicate-tool-use-id          HTTP 200
06-duplicate-current-tool-result  HTTP 200
07-assistant-prefill              HTTP 200
08-empty-current                  HTTP 200
09-large-420k-stream              HTTP 400 request body length threshold
```

The `09-large-420k-stream` result is important: it returned the local preflight error
`Request input content length exceeded the request threshold`. This is a serialized body-size
threshold, not a Kiro model context-window result and not the generic upstream
`Improperly formed request` body-shape error.

Real Claude Code CLI checks already saved under `.local-run/kiro-real-20260617-current/cli`:

```text
smoke.stream.jsonl   real CLI smoke through /cc completed
tools.stream.jsonl   real CLI used Grep and Read tools through /cc
long-1/long-2        multi-turn/longer CLI session progressed through many tool loops
mcp.stream.jsonl     custom MCP server was connected, but the --bare run did not expose the
                     custom MCP tool to the model, so this is an MCP connection check rather
                     than a full MCP tool-call pass
mcp-final.stream.jsonl
                     non-bare real CLI run used ToolSearch, then called
                     mcp__kiro-local-test__ping, received mcp-pong-final as tool_result, and
                     returned mcp-pong-final as the final answer
```

The later CLI failures in this run were caused by credential/account pressure, not by a fresh
malformed request:

```text
Kiro upstream 429 suspicious activity temporary limits disabled several free credentials.
Claude Code default max output tokens was 32000, making free-account tests expensive.
One CLI run ended with a downstream quota/pre-deduction failure after the Kiro requests had
already demonstrated the tool-loop path.
```

The final MCP run generated three real `/cc/v1/messages` calls in one session:

```text
request 1: model selected ToolSearch
request 2: model selected mcp__kiro-local-test__ping
request 3: model answered with the MCP tool_result text

All three upstream attempts returned credential_chain #7(200). The server log delta contains no
`Improperly formed request`, `assistant-prefill`, 400, 429, or 500 for this run.
Artifacts:
  .local-run/kiro-real-20260617-current/cli/mcp-final.stream.jsonl
  .local-run/kiro-real-20260617-current/cli/mcp-final.debug.log
  .local-run/kiro-real-20260617-current/cli/mcp-final-server-delta.log
```

Note: setting `CLAUDE_CODE_MAX_OUTPUT_TOKENS=512` in the command environment did not override the
user settings for this Claude Code CLI invocation; the service log still showed `max_tokens=32000`.
The MCP run was still low-output in practice, but future long-session cost tests should use an
explicit temporary Claude Code settings file if the output token cap must be enforced.

## Local Artifacts

```text
.local-run/kiro-protocol-20260617/real-minimal.json
.local-run/kiro-protocol-20260617/real-tool-use.json
.local-run/kiro-protocol-20260617/real-tool-result.json
.local-run/kiro-protocol-20260617/real-duplicate-tool-id-after2.json
.local-run/kiro-protocol-20260617/server-after2.log
```

## Cross-Project Protocol Comparison

This section compares the current `kiro.rs` implementation with:

- `/Users/yuanfeijie/Desktop/procode/Kiro-account-manager/Kiro-account-manager`
- `/Users/yuanfeijie/Desktop/procode/freedom-kirors`

The comparison is about Kiro upstream protocol compatibility, not general code style.

### Official Claude / Claude Code Constraints Used As Reference

Kiro upstream is not the public Anthropic Messages API, but the observed failures map closely to Claude-style message constraints:

- Tool results must immediately follow their matching assistant tool-use turn, and in a user message containing tool results, `tool_result` blocks must come before text. Anthropic documents this as a 400-producing formatting requirement in the tool-use guide: https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls
- Claude 4.6+ does not support final assistant prefill. Anthropic documents that prefill on Claude Sonnet 4.6, Opus 4.6, Opus 4.7, Opus 4.8, Fable 5, etc. returns 400 and should be replaced by system prompt instructions: https://platform.claude.com/docs/en/build-with-claude/working-with-messages
- Claude context-window behavior is model/surface dependent. Anthropic documents Sonnet 4.6 as 1M on supported surfaces, while Sonnet 4.5 is 200K; Claude Code additionally says Sonnet 1M availability varies by model and plan: https://platform.claude.com/docs/en/build-with-claude/context-windows and https://code.claude.com/docs/en/model-config
- `model_context_window_exceeded` is a normal stop reason on newer Claude models when generation reaches the context window limit; it is not the same class of error as a malformed request body. Anthropic documents this stop reason in the context window and stop reason docs.

These constraints explain both real errors reported by the user:

```text
assistant-prefill final message is not supported; last message must be user
```

This is a direct final-assistant-prefill violation. The local converter/payload guard must ensure the final upstream Kiro request always has `conversationState.currentMessage.userInputMessage`.

```text
400 Bad Request {"message":"Improperly formed request.","reason":null}
```

This is consistent with a tool transcript/order/body-shape violation, not just an input-token overflow.

### Kiro-account-manager Findings

Relevant files:

- `src/main/proxy/kiroApi.ts`
- `src/main/proxy/translator.ts`
- `src/main/proxy/tokenCounter.ts`
- `src/main/proxy/proxyServer.ts`
- `README.md` `v1.6.x`
- `test/e2e-fullsuite/cases/*`

Release-level problems solved by `Kiro-account-manager`:

- `v1.6.x` adds account rotation with circuit breaker/sticky behavior/exponential backoff/probabilistic retry, plus a `FATAL` vs `RECOVERABLE` error classifier. This matches our production need to stop bad requests quickly instead of burning all credentials.
- `v1.6.x` adds model capability badges derived from `ListAvailableModels`, including thinking/caching/effort metadata. This validates the decision that model capability and context window must come from the Kiro catalog first.
- `v1.6.x` says request headers/UA/version were aligned to official Kiro IDE captures, and request body includes `agentContinuationId` / `agentTaskType`. Current `kiro.rs` already has these body fields in the Kiro conversation state path and Kiro IDE-style headers in `src/kiro/endpoint/ide.rs`.
- `v1.6.x` fixes static alias downgrades and says short model aliases should resolve to official `ListAvailableModels` IDs. Current `kiro.rs` has the same direction in `ModelCapabilitiesCatalog::resolve_model`, and the new conservative fallback prevents free Sonnet from being guessed as 1M.
- `v1.6.x` simplifies thinking mode by passing through native Kiro reasoning events and supporting `additionalModelRequestFields`. Current `kiro.rs` already parses `reasoningContentEvent` / `messageMetadataEvent` / `ContextUsageEvent`, and maps thinking through existing converter paths.

Protocol details in `kiroApi.ts` / `translator.ts`:

- `resolveProfileArn(account)` uses real ARN first, then Enterprise fallback, then Social ARN, then BuilderID placeholder. Current `kiro.rs` splits this into `resolve_profile_arn()` for header/query endpoints and `resolve_streaming_profile_arn()` for streaming request bodies. This split is more precise because `ListAvailableModels` / MCP / usage endpoints must not receive BuilderID or Enterprise fallback ARNs, while streaming bodies may need a body `profileArn`.
- `fetchEnterpriseProfileArn()` calls `/ListAvailableProfiles` and persists the resolved ARN through a callback. Current `kiro.rs` mirrors this in `fetch_enterprise_profile_arn_for_context()` and `ensure_profile_arn_for_context()`.
- `fetchKiroModels()` paginates `ListAvailableModels` with `origin=AI_EDITOR`, `maxResults=50`, optional `profileArn`, and `nextToken`. Current `kiro.rs` mirrors that in `IdeEndpoint::models_url()` and `KiroProvider::list_available_models_for_context()`.
- `parseEventStream()` handles `contextUsageEvent` by reverse-calculating input tokens from `contextUsagePercentage * contextLen / 100`. Current `kiro.rs` mirrors this in both stream and non-stream handling, but improves the source of `contextLen` by using `ListAvailableModels.maxInputTokens` first.
- `translator.ts` deliberately strips request-history `reasoningContent` because sending it back to Kiro history can trigger `400 Improperly formed request`. Current `kiro.rs` has payload-guard support for removing history thinking blocks and native reasoning output handling.
- `translator.ts` ensures a Kiro current message exists and is a user message when the client ends with assistant. Current `kiro.rs` does this at the Anthropic converter and final Kiro payload-guard level; this is necessary for the `assistant-prefill final message is not supported` error.
- `translator.ts` preserves tool uses and tool results but relies more heavily on general `sanitizeConversation()` / `normalizeToolHistory()`. Current `kiro.rs` is now stricter: it repairs against the final Kiro body, renames repeated tool-use IDs by occurrence, and synchronizes adjacent tool results.

What current `kiro.rs` should keep from KAM:

- Catalog-first model capability, not alias guessing.
- Exact Kiro-ish endpoint/body/header shape.
- Context percentage based usage reconstruction.
- Fatal bad-request classification.
- Thinking schema driven by the upstream model catalog.

What current `kiro.rs` should not copy blindly:

- Adding extra execution-discipline prompts into every request can change model behavior and may reduce official-protocol fidelity. The current project should preserve client prompts and fix protocol shape at the transport layer.
- Falling back to Enterprise placeholder/profile ARNs on header/query endpoints is risky. Current `kiro.rs`'s split between streaming body ARN and header/query ARN is safer.

### freedom-kirors Findings

Relevant files:

- `src/kiro/model/credentials.rs`
- `src/kiro/endpoint/ide.rs`
- `src/kiro/endpoint/cli.rs`
- `src/kiro/provider.rs`
- `src/anthropic/handlers.rs`
- `src/anthropic/stream.rs`
- `CHANGELOG.md`

Release-level problems solved by `freedom-kirors`:

- `0.6.x` series focuses heavily on `profileArn`: Enterprise / IdC must resolve real profiles through `ListAvailableProfiles`, pure BuilderID may need placeholder body profile on streaming, while usage/model-list/MCP/header/query endpoints should avoid placeholder profile leakage.
- `0.6.x` adds credential-level `ListAvailableModels`, exposing the real model set and `maxInputTokens` by credential. This directly addresses the free Sonnet vs 1M Sonnet ambiguity.
- `0.5.9` blocks 503 retry storms from client validation errors. This is important because Bedrock/Kiro may report malformed tool transcripts as 5xx/503, but retrying against more credentials cannot fix it.
- `0.5.5` adds request trace persistence with per-attempt status, credential id, endpoint, status code, error classification, upstream error snippet, and duration. Current `kiro.rs` has in-memory/log attempt chains and usage records, but persistent per-attempt trace UI is still an area to borrow if production debugging remains hard.
- `0.5.2` splits 429 suspicious-activity account throttle from generic high-load 429. Current `kiro.rs` already has richer scheduler/cooldown logic; the key principle is to keep upstream overload (`MODEL_TEMPORARILY_UNAVAILABLE`, high load) separate from credential/account punishment.
- `0.5.8` caps the HTTP client cache, avoiding unbounded per-proxy `reqwest::Client` retention. Current `kiro.rs` should keep checking this if large proxy pools are used.

Protocol details:

- `KiroCredentials` supports BuilderID/Social/Enterprise/IdC/API Key, per-credential regions, per-credential endpoint selection, proxy, group, and rate limit fields. Current `kiro.rs` has the same broad credential model and already migrated API keys to the correct endpoint behavior.
- `effective_profile_arn()` vs `streaming_profile_arn()` is the conceptual split that current `kiro.rs` now implements as `resolve_profile_arn()` vs `resolve_streaming_profile_arn()`.
- `provider.rs` resolves Enterprise profile before calls, persists real ARN, and avoids retrying known client validation errors. Current `kiro.rs` mirrors this pattern and now labels bad requests without exposing Anthropic-to-Kiro conversion wording to users.
- `anthropic/stream.rs` in `freedom-kirors` also reconstructs input tokens from `contextUsageEvent` and has fallback logic for literal `<invoke>` text leakage. Current `kiro.rs` already has similar leaked invoke recovery, code-fence protection, and duplicate structured-tool de-duplication.

What current `kiro.rs` should keep from freedom-kirors:

- The account/profile split is protocol-critical, not a cosmetic implementation detail.
- Bad request / validation errors must fail fast and should not rotate through the whole credential pool.
- Context usage percentage is the best available signal for real upstream input-token usage when metadata usage is absent.
- Real model availability must be observed per credential/channel where possible, because free accounts and paid accounts can expose different model IDs and context limits.
- Persistent request trace UI is the biggest remaining operational improvement if remote logs are frequently cleared.

### Current kiro.rs State After This Round

Implemented or already present:

- Final upstream request always has a user current message in normal conversion flow; assistant prefill is classified separately as `assistant_prefill_bad_request`.
- Kiro request-body guard runs after Anthropic-to-Kiro conversion and validates/repairs the actual body sent upstream.
- `tool_use_id` repair is occurrence-aware. Reused IDs are renamed only on later occurrences and matching adjacent tool results are rewritten.
- Current `tool_result` is allowed only for the immediately previous assistant tool-use turn. Older results are textified or dropped from structured tool results.
- Duplicate current `tool_result` is kept once structurally and additional result text is appended as plain text.
- `ListAvailableModels.maxInputTokens` is the primary context-window source; `get_context_window_size()` is only a conservative fallback.
- Kiro `contextUsageEvent` is used to reconstruct input tokens when metadata usage is absent.
- 100% context usage maps to `model_context_window_exceeded`.
- Bad request labels no longer expose "Anthropic to Kiro conversion" language to downstream users.

Remaining risks:

- A truly oversized serialized Kiro body can still fail even if context percentage is below 100%; request byte limits and context-token limits are independent.
- If all usable Sonnet credentials enter refresh failure/disabled/cooldown, real protocol tests cannot proceed. The latest failed supplementary tests were blocked by credential/channel exhaustion, not by a fresh malformed body.
- `THINKING_SIGNATURE_INVALID` repair exists in KAM; current `kiro.rs` removes/truncates history thinking in payload guard, but should continue to be watched under real Claude Code extended-thinking tool loops.
- Persistent trace storage like `freedom-kirors` would make future remote-log-loss incidents easier to analyze. This is operational, not required to fix the current malformed class.

## Regression Test Design

The purpose of testing is to compare official-Kiro-compatible behavior before and after the fix, not to keep branch-only test code in production.

### Static / Unit Regression

Run after every protocol change:

```text
cargo test --locked --no-default-features payload_guard
cargo test --locked --no-default-features converter
cargo test --locked --no-default-features model_capabilities
cargo test --locked --no-default-features stream
cargo test --locked --no-default-features
```

Coverage target:

- Assistant prefill / final assistant message becomes a valid Kiro current user message or fails as bad request without retry storm.
- Current user `tool_result` pairs only with the immediately previous assistant tool-use turn.
- Reused `tool_use_id` across turns is renamed and adjacent result ID rewritten.
- Duplicate current tool results are not sent as duplicate structured results.
- Free Sonnet catalog maps to 200K when `ListAvailableModels` says 200K.
- Dot/minor 1M-capable models only use 1M when catalog or explicit `[1m]` says so.
- `contextUsageEvent` percentage uses the catalog window and 100% produces `model_context_window_exceeded`.

### Local Real Protocol Matrix

Prerequisites:

- Local service running on a clean debug binary.
- `ccman cc current` points to the local service.
- Only Sonnet is selected because local credentials are free.
- At least one credential/channel can complete a real Sonnet request.

Commands/artifacts should be saved under a timestamped `.local-run/kiro-protocol-YYYYMMDD-*` directory:

```text
healthz
/cc/v1/models
/cc/v1/messages minimal non-stream Sonnet
/cc/v1/messages minimal stream Sonnet
/cc/v1/messages with tool definition, expect structured tool_use
/cc/v1/messages tool_result follow-up, expect final answer
/cc/v1/messages synthetic duplicate tool_use_id transcript, expect 200 or repaired payload log
/cc/v1/messages large prompt near observed catalog window, record contextUsageEvent percentage
```

Pass criteria:

- No `Improperly formed request` in server log for repaired transcripts.
- Payload guard logs show specific repair counters when synthetic bad transcripts are used.
- Normal prompts do not show repair counters beyond zero or expected large-payload observation.
- Tool responses are structured Anthropic-compatible SSE/JSON, not leaked Kiro XML.
- Stream output contains a single coherent final answer, no repeated final text loop.
- Context usage percentage and reconstructed input tokens are consistent with the model catalog window.

### Claude Code CLI Real Interaction Matrix

Use the real CLI against the local service through `ccman`, not only curl mocks:

```text
ccman cc use local-kiro-rs-9022
claude --model sonnet --print "..."
claude --model sonnet --output-format stream-json "..."
```

Scenarios:

- Smoke: short answer, no tools.
- Multi-turn: resume the same session for at least 3 turns.
- Tools: ask Claude Code to inspect a small local workspace using read/list/search tools.
- Search-like behavior: use grep/ripgrep style tool calls through Claude Code and verify tool-use/tool-result loop.
- MCP: start a local MCP filesystem/test server, ask the CLI to call it, verify tool results are paired in one user turn.
- Agent/subtask: run a CLI task likely to trigger internal task/explore behavior, but force Sonnet where possible; if Claude Code selects Haiku internally, record it as a test-environment limitation, not a Kiro protocol failure.
- Long session: accumulate enough history/tool results to trigger payload guard observation/trimming without semantic collapse.
- No-repeat output: capture stream-json and assert the final assistant text is not duplicated by replaying `content_block_delta` handling.

Pass criteria:

- CLI can complete multiple real rounds without `assistant-prefill` or `Improperly formed request`.
- Tools and MCP produce valid paired tool loops.
- Long session remains fluent and task-aware after guard trimming/repair.
- No duplicate final output in stream-json or cleaned log.
- Failures caused by `MODEL_TEMPORARILY_UNAVAILABLE`, upstream high load, disabled credentials, or missing Sonnet channels are classified separately from payload malformed failures.

### Before / After Comparison

Expected behavior before the current fix:

- Reused `tool_use_id` could be treated as already paired by set-level checks.
- A current `tool_result` for a reused ID could be dropped as duplicate or sent against the wrong occurrence.
- Kiro could reject the final body as `Improperly formed request`.
- Bad request wording could expose internal conversion wording to users.
- Sonnet aliases could be overestimated as 1M without catalog confirmation.

Expected behavior after the current fix:

- Reused IDs remain semantically paired by occurrence through rename-and-sync.
- Current tool results are validated against the final Kiro body shape.
- Real local tool and duplicate-ID synthetic tests pass against Kiro upstream.
- User-visible errors describe request shape/tool pairing/schema/payload size, not converter internals.
- Context-window math follows the real Kiro catalog first.

## Remaining Validation

Current status after the latest local run:

1. Real Claude Code CLI multi-turn through `ccman` with Sonnet: completed far enough to validate
   repeated `/cc` calls, tool-use/tool-result pairing, and long-session progression. Later turns
   were blocked by free credential 429/quota pressure, not by malformed body errors.
2. Real Claude Code CLI tool/search: completed with `Grep` and `Read` tool loops. No
   `assistant-prefill` or `Improperly formed request` was observed in the saved service log for
   those requests.
3. MCP: completed in the non-`--bare` run. Claude Code used `ToolSearch` to discover
   `mcp__kiro-local-test__ping`, emitted a real MCP tool_use, received the MCP tool_result, and
   completed the final answer. No malformed-body errors appeared in the service log delta.
4. No-repeat output: the saved real CLI streams did not show a repeated final-output loop in the
   successful tool/smoke portions. The duplicate-output class was not reproduced.
5. Large context/body: a 420K serialized request hit the local request body threshold. The free
   local Sonnet catalog observed here is 200K-class, so this run does not prove 1M Sonnet behavior;
   it does prove body-size and context-window limits must be treated separately.

Still useful after credentials cool down or a paid/less restricted credential is available:

1. A long session with `CLAUDE_CODE_MAX_OUTPUT_TOKENS` reduced from the local default 32000, to
   avoid unnecessary free-account burn while still exercising repeated tool loops.
2. A real account/model catalog that exposes a 1M-capable Sonnet/Auto model, then a controlled
   contextUsageEvent test to verify percentage-based token reconstruction against that catalog.

Only Sonnet should be used for local free credentials.
