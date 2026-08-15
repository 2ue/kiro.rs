Status: `analysis-recorded / external-pool-wire-debug-implemented`

# External Pool Claude CLI Interaction Diagnostics

## Problem

External pool requests can look successful in the admin/usage view while Claude Code CLI
interactive UI shows no visible assistant output, or while follow-up instructions submitted
during an active stream appear to be ignored. The current local external-pool diagnostic switch
is useful for usage projection analysis, but it is not a complete wire/interaction diagnostic.

The core risk is treating a derived observation as the root cause. In particular:

- `output_tokens > 0` in usage/admin is not proof that upstream returned visible text.
- A successful usage record is not proof that Claude Code received a renderable `message_stop`
  sequence.
- A server-side stream error record is not proof that Claude Code UI displayed an error.
- A missing follow-up in the model answer is not proof that the model ignored it; the follow-up
  may never have reached the service, may have been omitted from the request body, or may have
  been transformed before upstream dispatch.

## Current Diagnostic Coverage

Current code path:

- `src/external_pool.rs::ExternalUsageCapture`
- `src/external_pool.rs::external_pool_usage_debug_non_stream_record`
- `src/external_pool.rs::external_pool_usage_debug_stream_record`
- `src/external_pool.rs::record_external_usage_debug_sse_event`
- `src/external_pool.rs::process_sse_event_with_plan_and_transcript`
- `src/external_pool.rs::ExternalStreamUsageGuard`

Non-stream coverage is relatively strong:

- Captures upstream response body bytes/hash/preview.
- Collects raw usage candidates from upstream JSON.
- Captures outbound request body bytes/hash/preview.
- Captures processing result, including `usageCapture`, `protocolContamination`,
  `downstreamBodyChanged`, and processed `downstreamBody`.

The original stream usage-debug coverage was incomplete for Claude CLI interaction issues:

- Captures raw upstream SSE preview and raw upstream usage event samples.
- Captures event counts, data-line counts, done counts, JSON parse errors, event type counts,
  and usage paths for raw upstream SSE.
- Captures final `usageCapture`, billing usage, terminal status, and estimated output tokens
  from forwarded chunks.
- Does not capture a semantic summary of raw upstream visible output.
- Does not capture a semantic summary of processed downstream SSE after usage rewrite,
  error masking, thinking buffering, and transcript sanitization.
- Does not prove that downstream SSE contained visible `text_delta`, valid event ordering,
  or `message_stop`.
- The implemented wire debug adds handler-level inbound request summaries, so a follow-up typed in
  Claude CLI can be correlated even if the request never reaches an external pool.
- The implemented wire debug structurally summarizes effective raw, working raw, and outbound
  request bodies, so long-context follow-up markers are not dependent on byte-preview position.

## Usage Interpretation Hazards

`usageCapture.rawUsage` is not always true upstream usage.

When usage is estimated, `apply_estimated_usage_capture` fills `raw`, `shaped`, and `reported`
with estimated values and marks `usageEstimated=true` with a reason such as
`missing_upstream_usage` or `unrecognized_success_body`. Therefore:

- True upstream usage should be read from raw upstream usage candidates where present.
- If `usageEstimated=true`, admin `output_tokens` may be local estimation and must not be used
  as proof that upstream emitted visible content.
- For stream responses, `estimatedOutputTokensFromForwardedStream` estimates processed chunks
  consumed by the response body stream; it is not the same as upstream-provided usage.

To determine whether "body content is really zero", diagnostics must measure content directly:

- non-stream: parsed upstream body content text/thinking/tool counts and processed downstream body
  content text/thinking/tool counts.
- stream: raw upstream SSE semantic counts and processed downstream SSE semantic counts.

Required semantic fields include at least:

- `message_start`, `message_delta`, `message_stop` counts.
- `content_block_start`, `content_block_delta`, `content_block_stop` counts.
- visible text chars from `content_block_start.content_block.text`.
- visible text chars from `content_block_delta.delta.text`.
- thinking chars from thinking blocks and `thinking_delta`.
- `signature_delta` count.
- tool/server-tool block counts.
- input-json delta chars.
- error event counts.
- JSON parse errors.
- stop reason.
- first visible text event index.
- first semantic output event index.

## Invisible Error Possibility

Yes, it is possible for an error to be recorded by the server or returned on the wire without
Claude Code CLI interactive UI showing a clear visible error.

Cases to account for:

- HTTP non-2xx before downstream commit may be retried by Claude Code or by this service; the UI
  may only show a pause or no final visible error if a retry succeeds or stalls.
- A stream may return HTTP 200 and later emit an SSE `error` event. Depending on timing and event
  shape, Claude Code UI may not render it as a normal assistant-visible error.
- This service can mask upstream stream error events before returning a safe downstream error
  event. If the CLI treats that sequence as a transport/protocol failure or logs it only in debug,
  the interactive UI may remain blank.
- A body stream can fail after headers are sent. The server records `StreamError`, but the CLI UI
  may show an incomplete turn, a retry, or no visible message rather than a clean error banner.
- A client disconnect/drop is recorded as `ClientDropped`. That is server evidence of the body
  being dropped before completion, not evidence that UI showed an error.
- A malformed or incomplete SSE sequence, especially missing `message_stop`, can prevent the CLI
  from considering the turn complete. Follow-up input may remain queued and appear ignored.
- If the response only contains thinking/signature/tool scaffolding and no visible text/result,
  usage may be non-zero while the interactive UI displays little or nothing.

Therefore diagnostics must not collapse "error recorded", "error returned downstream",
"CLI received error", and "UI displayed error" into one state.

## Required Evidence Chain

A reproducible investigation must correlate these layers by request id and timestamp:

1. CLI terminal transcript
   - Exact time and text of follow-up inputs.
   - Whether input was sent while the previous stream was still active.

2. Claude CLI machine-readable capture
   - `stream-json`/JSONL where possible.
   - Claude debug logs.
   - Provider selected by `ccman`, without credentials.

3. Handler inbound request ledger
   - One record per `/messages` request, even if it does not use an external pool.
   - Request id, endpoint, stream flag, model, route, conversation/session id.
   - Structured inbound request summary: message count, last N message roles, last user text
     length/hash/short preview, tool counts.

4. External outbound request summary
   - Final body sent to upstream after request body mode, model mapping, prompt steering, and
     normalization.
   - Same structured summary as inbound.
   - Explicit diff flags: message count changed, last user hash changed, system changed,
     tools changed, model changed.

5. Raw upstream response summary
   - HTTP status, headers, upstream request id.
   - Raw body/SSE bytes hash and bounded preview.
   - Parsed semantic response summary.
   - True upstream usage candidates by JSON path and raw value.

6. Processing summary
   - Usage projection mode and whether projection applied.
   - `rawUsage`, `shapedUsage`, `reportedUsage`, `usageEstimated`, `usageEstimateReason`.
   - Protocol contamination result.
   - Error masking result.
   - Transcript sanitizer suppression/rewrite/fatal counters.
   - Thinking buffer complete/incomplete/overflow counters.

7. Processed downstream response summary
   - HTTP status and content-type returned to Claude CLI.
   - Downstream body/SSE bytes hash and bounded preview.
   - Downstream usage candidates by JSON path and raw value.
   - Downstream semantic response summary.
   - Event ordering/protocol validation result.
   - Whether `message_stop` was emitted.

8. Stream lifecycle
   - First upstream header/chunk/output timestamps.
   - First downstream semantic output timestamp.
   - Chunks/events before first output.
   - Last chunk timestamp.
   - Terminal status: success, stream error, client dropped, retry before commit, retry after
     non-commit, or timeout.

## Triage Matrix

Use the evidence chain to classify failures:

- No inbound request after follow-up input: Claude CLI interaction/queue/provider state issue.
- Inbound request exists, but follow-up absent from inbound body: Claude CLI did not include it
  in the request.
- Inbound body contains follow-up, outbound body does not: local request processing bug.
- Outbound body contains follow-up, upstream answer ignores it: upstream/model behavior or prompt
  context issue.
- Upstream visible text chars are zero and usage is estimated: upstream content may truly be empty;
  admin output is not proof of content.
- Upstream visible text chars are non-zero and downstream visible text chars are zero: local
  stream processing/sanitizer/rewrite issue.
- Downstream visible text chars are non-zero but `message_stop` is missing: CLI can hang and
  follow-ups may remain queued.
- Downstream stream is protocol-invalid: Claude CLI may log/debug/fail silently rather than show
  a clean UI error.
- Downstream stream is protocol-valid and CLI `stream-json` has text, but interactive UI is blank:
  UI/rendering state issue.
- Downstream stream is protocol-valid but CLI `stream-json` is blank: client-side transport or
  parser compatibility issue; compare with a raw capture proxy.
- Server terminal status is `ClientDropped`: treat as downstream/client cancellation, not upstream
  empty output.

## Proposed Instrumentation Shape

Do not overload usage debug indefinitely. Keep usage debug for usage projection, and add a separate
external-pool wire/interaction diagnostic mode.

Suggested config:

- `externalPoolUsageDebugEnabled`: current usage-focused behavior.
- `externalPoolWireDebugEnabled`: request/response wire summaries for CLI issues.
- `externalPoolWireDebugMaxBodyBytes`.
- `externalPoolWireDebugMaxFiles`.
- `externalPoolWireDebugDir`.

The implementation uses `summary` plus bounded preview/full-body hash behavior under
`externalPoolWireDebugMaxBodyBytes`.

Default is disabled. When enabled, body previews are bounded by
`externalPoolWireDebugMaxBodyBytes`.

Implementation points:

- Handler ingress: write inbound request summary before external routing.
- External dispatch: write outbound request summary after body processing.
- Upstream response: summarize raw upstream non-stream body or raw upstream SSE.
- Downstream response: summarize processed downstream non-stream body or processed downstream SSE.
- Stream guard: write lifecycle close reason and client-drop/stream-error/success status.

The output should be one correlated JSON document per request where possible, or multiple stage
records with a shared `requestId` and monotonic stage/timestamp.

## Validation Requirement

After instrumentation, validation must use real Claude Code CLI interactive sessions, not only
curl:

- Run multiple rounds per external-pool parameter set.
- Include direct external-pool provider baseline and local-service provider path.
- Submit follow-ups while the previous stream is still producing output.
- Use unique markers in every follow-up.
- Preserve CLI JSONL/debug logs, terminal transcript, local wire debug records, and optional
  CLI-to-local raw capture proxy output.

Only then can we distinguish upstream empty output, local processing loss, downstream protocol
shape errors, invisible CLI errors, client drops, and actual model non-compliance.
