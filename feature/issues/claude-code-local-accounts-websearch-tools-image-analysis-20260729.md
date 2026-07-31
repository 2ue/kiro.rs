# Claude Code local-account WebSearch/tools/image analysis - 2026-07-29

Status: `analysis-recorded / local-account-real-call-evidence-collected / fixes-pending`

Severity: P0/P1。WebSearch mixed-tool 形态会产生无人执行的普通 `tool_use web_search`；Claude Code CLI `WebSearch` 在当前配置下没有真实 tool call；tools 简单路径可用但依赖名称/历史映射修复层；合法图片路径可用但 remote/file/tool_result 边界仍可能导致间歇性失败。

Last observed: 2026-07-29 Asia/Shanghai

## Scope

This note supersedes any external-pool-based conclusions for the current debugging pass.

The user explicitly requested validation through local Kiro accounts only. The local service under test was:

- Service: `kiro-rs` on `127.0.0.1:9022`, listener PID `59668`.
- Claude Code CLI: `2.1.220 (Claude Code)`.
- `ccman cc current`: `local-kiro-rs-9022-current`, URL `http://127.0.0.1:9022/cc`.
- Runtime external pools: disabled for this pass. Backup before the local-only runtime change: `/tmp/kiro-runtime-before-local-only-9022.json`.
- Active non-deleted local credentials: id `7` and id `8`, both `social`, `KIRO FREE`, not disabled.
- Credential smoke test endpoint confirmed both id `7` and id `8` can call `claude-sonnet-4.5` and return `local-ok`.

Do not use older external fallback / external pool records as evidence for the three issues below.

Related historical/specialized records:

- [Claude Code real CLI tools/WebSearch/image debug - 2026-07-29](claude-code-real-cli-tools-websearch-image-debug-20260729.md) is historical external-pool-heavy evidence and has been corrected by this document for local-account diagnosis.
- [WebSearch/MCP protocol, errors, usage, attempts and privacy](websearch-mcp-protocol-usage-and-privacy.md) covers native WebSearch MCP correctness/privacy, but the current mixed native WebSearch behavior remains open.
- [Native WebSearch normalized external fallback preflight](websearch-normalized-external-fallback-preflight.md) covers fallback when pure native WebSearch cannot use local MCP; it does not make Claude Code CLI `WebSearch` a real client tool.
- [Image format unsupported 400](08-image-format-unsupported-400.md) covers bad/invalid image rejection; this document adds local-account evidence that valid direct and CLI `Read` images work.
- [Payload guard semantics, limits and performance](payload-guard-semantics-limits-and-performance.md) remains relevant for large/current-turn image trimming.

## Local-only evidence

All rows below used local account routing unless noted otherwise.

### Model routing

- Direct `model: "claude-sonnet-4.5"` baseline succeeded:
  - request `req_01TKuMnyadGt1qmDASPFTxBs`
  - route `local_credential/local_success`
  - credential `7`
  - upstream `claude-sonnet-4.5`
  - output `direct-local-pong`
- Direct `model: "sonnet"` control succeeded in the current local-only runtime:
  - request `req_01AxDNLhGEHByNx1m7xZBq44`
  - route `local_credential/local_success`
  - credential `8`
  - upstream `claude-sonnet-4.5`
  - output `alias-pong`
- Direct `model: "claude-sonnet-4.6"` also succeeded, but only because it was remapped:
  - request `req_01WKSTtDDqgR9ptyFH4Hdtvq`
  - response body echoed `model: "claude-sonnet-4.6"`
  - usage record shows upstream `claude-sonnet-4.5`
  - route `local_credential/local_success`, credential `7`

Important implication: the response `model` can echo the requested model even when the actual upstream model is `claude-sonnet-4.5`. Debugging must use usage records/logs, not only the downstream response body.

Older records from 2026-07-28 showed `sonnet -> claude-sonnet-5` and local 503. That is a separate model-resolution/catalog-state risk: when the available model catalog contains a newer Sonnet candidate, local free accounts that only support 4.5 can be routed to an unusable model. In the current runtime, the resolver maps `sonnet` and explicit 4.6 requests back to available 4.5.

Relevant source:

- `src/anthropic/handlers.rs:5130` resolves request model using `state.model_capabilities.resolve_model_with_mapping(...)`.
- `src/anthropic/model_capabilities.rs:1327` lists `sonnet` alias candidates in this order: `sonnet`, `claude-sonnet-4.6`, `claude-sonnet-4-6`, `claude-sonnet-4.5`, `claude-sonnet-4-5-20250929`, `claude-sonnet-4`.
- `src/anthropic/model_capabilities.rs:1377` explicitly allows `claude-sonnet-4.6` / `4-6` fallback to `claude-sonnet-4.5`.
- `src/anthropic/converter/model.rs:21` still has static converter alias `sonnet -> claude-sonnet-4.6`, but live request resolution is catalog-aware before conversion.

## WebSearch analysis

### What works

Pure native Anthropic WebSearch works through the local account path:

- Request shape:
  - `tools: [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 1 }]`
  - no other tools
  - model `claude-sonnet-4.5`
- Result:
  - request `req_01yPTQ3uUhHq89z8FGZQycZ9`
  - route `local_credential/local_success`
  - credential `8`
  - upstream `claude-sonnet-4.5`
  - response contained `server_tool_use` and `web_search_tool_result`
  - log contained `handling native WebSearch request`

So WebSearch is not completely absent at the server-side native path.

### What fails / behaves incorrectly

1. Claude Code CLI `--tools WebSearch --allowedTools WebSearch` did not produce a real tool call.

   Evidence:

   - CLI case `websearch_tool`, model `claude-sonnet-4.5`
   - exit `0`, but `toolUseNames=[]`
   - `toolResultCount=0`
   - final usage `server_tool_use.web_search_requests=0`
   - assistant text contained pseudo XML:
     - `<search_web>`
     - `<query>current date time Shanghai China</query>`
   - No actual WebSearch execution happened.

   This means Claude Code CLI did not expose a runnable client-side `WebSearch` tool in this configuration. The model only hallucinated / followed a text convention.

2. Native WebSearch is only detected when it is the only tool.

   Source:

   - `src/anthropic/websearch.rs:260`:
     - `tools.len() == 1`
     - first tool name must be `web_search`
     - first tool type must be `web_search_20250305`
   - `src/anthropic/handlers.rs:5707` only enters the native WebSearch MCP branch when `websearch::has_web_search_tool(&payload)` is true.

   Direct mixed-tool evidence:

   - Request shape:
     - `tools = [native web_search_20250305, echo_value]`
     - model `claude-sonnet-4.5`
   - Result:
     - request `req_01H7Q6sMoZEAN7kyan5zLYjL`
     - route `local_credential/local_success`
     - credential `8`
     - upstream `claude-sonnet-4.5`
     - response `stop_reason: "tool_use"`
     - content contained ordinary `tool_use`:
       - `name: "web_search"`
       - `input: {}`
     - no `server_tool_use`
     - no `web_search_tool_result`

   This is a real compatibility bug for Claude Code style multi-tool turns. If the caller has no local executor for `web_search`, it will look like WebSearch is unsupported or broken.

### Likely causes

- Server-side native WebSearch implementation is a special-case shortcut for pure WebSearch requests, not a general tool execution loop.
- Claude Code CLI's `WebSearch` flag in this run did not register a client-side tool, so no client tool_result can be produced.
- Mixed native WebSearch plus other tools falls through to normal tool conversion, where `web_search` becomes a regular model tool_use instead of being executed by the server.
- If `compatProfile` is strict, `handlers.rs:5708` will reject native WebSearch with `"The web_search tool is not supported for this request."`; current runtime was not strict enough to reject the pure native case.

### Fix direction

Options:

- Support native `web_search_20250305` even when other tools are present by executing the server-side WebSearch tool_use and continuing the turn, or by returning Anthropic-compatible server-tool blocks for mixed tool requests.
- If mixed native WebSearch cannot be supported yet, reject it early with a clear normalized error instead of returning a normal `tool_use name=web_search input={}` that the client cannot execute.
- For Claude Code CLI client-tool WebSearch, verify whether current CLI actually supports a built-in `WebSearch` tool with custom `ANTHROPIC_BASE_URL`. If it does not, do not document CLI WebSearch as supported through `--tools WebSearch`.

## Tools analysis

### What works

Minimal direct tool use works:

- Direct forced tool:
  - request `req_017oazMg5ptjHU64BX1CSYAW`
  - model `claude-sonnet-4.5`
  - route `local_credential/local_success`
  - credential `7`
  - upstream `claude-sonnet-4.5`
  - response `stop_reason: "tool_use"`
  - response tool_use:
    - `name: "echo_value"`
    - `input.value: "local-tool-ok"`

Real Claude Code CLI Bash tool round-trip works:

- CLI case `bash_tool`, model `claude-sonnet-4.5`
- route records:
  - `req_01M1xPMRifBEbdT7BYdMLA8W`
  - `req_01SiVtLGKtBt28qBofFE8wv4`
- both local success, credential `7`, upstream `claude-sonnet-4.5`
- CLI stream summary:
  - `toolUseNames=["Bash"]`
  - `toolResultCount=1`
  - final text `cli-tool-ok`

Real Claude Code CLI `Read` tool with image also works:

- CLI case `read_image`, model `claude-sonnet-4.5`
- route records:
  - `req_01S1yXogpe86mTMb4z951yny`
  - `req_01ygWuidyTnqa3VLK83h1aED`
- both local success, credential `7`, upstream `claude-sonnet-4.5`
- CLI stream summary:
  - `toolUseNames=["Read"]`
  - `toolResultCount=1`
  - final text `Red`

### Risky behavior observed

Tool names are normalized/mapped for Kiro, not passed through literally.

Relevant source:

- `src/anthropic/converter/tools.rs:95` sanitizes tool names into Kiro-safe camelCase names.
- `src/anthropic/converter/tools.rs:130` deterministically maps a name when sanitized name differs or exceeds `TOOL_NAME_MAX_LEN`.
- `src/anthropic/converter/tools.rs:173` records the reverse map.

Observed logs:

- Direct `echo_value` logged `工具名称映射: 1 个超长名称已缩短`.
- CLI `Bash` and `Read` each logged `工具名称映射: 1 个超长名称已缩短`.
- The log message says "overlong", but the mapping also happens for names that are not overlong, such as `Bash`, `Read`, and `echo_value`, because sanitization changes case/underscore form.

The simple tool cases passed because response tool names were mapped back to Claude Code names. But the mapping layer can still explain "tools parsing is wrong" in more complex cases:

- Multiple original names can normalize to the same Kiro-safe name and be rejected as ambiguous.
- Historical assistant tool_use names and current tool definitions must share the same reverse map. Missing or stale mapping can break tool_use/tool_result pairing.
- Tool-choice by name can become ambiguous after normalization.
- Logs currently make mapping look like an overlong-name-only path, hiding the fact that normal built-in names are also being rewritten.
- Old 2026-07-28 logs from the wrong model path showed `prefill-dropped=1` on Claude Code-style requests. Prefill dropping is intentional, but it can make debugging tool history harder if the client expected a terminal assistant prefill to survive.

Current local-only evidence does not show a minimal JSON parser failure for tools. It shows tool compatibility is dependent on the converter's name/schema/pairing repair layer.

## Image analysis

### What works

Valid direct base64 PNG is stable in this small local-only run:

- `req_01jnYaBLB8BP5e6zpx9bDHCQ`: output `Red`
- `req_01odzpeUcmaWFcH4KgMJv4gu`: output `Red`
- `req_01bT5s1FNSz32uaM7PfN1SdY`: output `Red`

All three were:

- model `claude-sonnet-4.5`
- route `local_credential/local_success`
- credential `7`
- upstream `claude-sonnet-4.5`
- logs showed `current_image_count=1`

Declared `image/jpeg` with PNG bytes was corrected and succeeded:

- request `req_01gVYp4s4LCYp7bsjG9C68AW`
- output `Red`
- log warning: `base64 image media_type mismatches were corrected before upstream routing`
- `normalized_image_media_types=1`

Claude Code CLI `Read` image also worked:

- file `/tmp/kiro-cli-red.png`
- CLI final text `Red`
- second request log showed:
  - `current_tool_count=1`
  - `current_tool_result_count=1`
  - `current_image_count=1`
  - warning header `tool-result-content-placeholder=1`

### What is rejected

Invalid/fake image data is rejected before upstream dispatch:

- request `req_014x1Q676MomJY5AD4rp1GAP`
- HTTP `400`
- error: `invalid image data for media_type: image/png`
- usage record status `error`, type `request_rejection`
- log reason `local_body_prepare`, stage `handler_preflight`

This is correct behavior, but it explains one class of "sometimes not recognized": fake or malformed image payloads no longer reach the model.

### Likely causes for intermittent image recognition

The valid base64 path did not reproduce instability. Current likely causes are input-shape dependent:

- Some requests may contain invalid or truncated image bytes. `src/anthropic/converter/content.rs:457` detects format from bytes and rejects structurally invalid PNG/JPEG/GIF/WebP.
- Some requests may use `file`/`file_id`/remote URL images that must be materialized before conversion. If materialization does not happen, converter errors with:
  - `image file source was not materialized before conversion`
  - `remote image URL source was not materialized before conversion`
- Safe mode has bounded remote materialization:
  - `src/anthropic/body_processing.rs:21`: per remote source max 20 MiB
  - `src/anthropic/body_processing.rs:22`: max 20 remote sources
  - `src/anthropic/body_processing.rs:23`: aggregate downloaded max 32 MiB
  - `src/anthropic/body_processing.rs:24`: aggregate materialized max 44 MiB
  - `src/anthropic/body_processing.rs:27`: max 4 concurrent remote workflows
  - `src/anthropic/body_processing.rs:28`: per request timeout 25s
  - `src/anthropic/body_processing.rs:29`: workflow deadline 45s
- Payload guard has `current_images_max_bytes` default 180,000 bytes in `src/model/config.rs:3937`; large current-turn images can be dropped/trimmed depending on payload guard behavior.
- Tool-result images are split into two representations:
  - image bytes are extracted into `images`
  - tool_result textual content receives `[image attached]`
  - source: `src/anthropic/converter/content.rs:606` and `src/anthropic/converter/content.rs:627`
  This worked in the `Read` smoke, but it is a compatibility-sensitive path. If the tool_result content contains several images, text plus images, or edge JSON blocks, the placeholder/image extraction path should be tested directly.
- The response body may echo requested model 4.6 while actual upstream is 4.5. If the caller expects 4.6 image behavior based on response metadata, that expectation is wrong for these local accounts.

## Current conclusion

The three user-visible problems are not caused by local accounts being completely unusable. Both local accounts work with `claude-sonnet-4.5`.

Most likely root causes:

1. Model/capability confusion:
   - local accounts only prove 4.5 support;
   - `sonnet`/`4.6` can be exposed or echoed while upstream is actually 4.5;
   - older runtime/catalog state routed `sonnet` to `claude-sonnet-5`, causing local 503 and masking other issues.
2. WebSearch compatibility gap:
   - pure native WebSearch works;
   - Claude Code CLI `WebSearch` did not produce a real client tool call;
   - mixed native WebSearch plus normal tools falls through as ordinary `tool_use web_search`, which no executor handles.
3. Tool compatibility depends on converter repair:
   - simple direct and CLI Bash/Read tools work;
   - names are rewritten even for normal built-in names, then reversed;
   - ambiguous normalization, stale history mapping, schema-key normalization, or prefill/tool pairing repair can still break complex tool sessions.
4. Image instability is likely payload/source dependent:
   - valid base64 PNG and CLI Read image worked;
   - invalid image bytes are rejected locally;
   - remote/file materialization, media-type mismatch, size limits, tool_result image placeholders, or payload guard trimming are the main paths to inspect for intermittent failures.

## Suggested next tests/fixes

1. Add/adjust tests for mixed native WebSearch plus ordinary tools.
   Expected behavior should be either:
   - server executes the native WebSearch and returns Anthropic-compatible server-tool blocks, or
   - server rejects mixed native WebSearch with a clear normalized error.
   It should not return a normal `tool_use name=web_search input={}` without an executor.

2. Clarify model reporting for remapped requests.
   At minimum, expose/record `requestedModel` and `upstreamModel` consistently. Consider whether downstream `model` should echo requested model or actual upstream model for `/cc` compatibility.

3. Improve tool mapping observability.
   Rename the log from "overlong names shortened" to something like "tool names normalized/mapped" and include separate counts for sanitized-vs-overlong mappings.

4. Add focused regression tests for Claude Code tool histories:
   - `Bash`/`Read` PascalCase names;
   - names that collide after sanitize;
   - tool_choice by original and mapped name;
   - assistant tool_use followed by user tool_result with image content.

5. Add image regression tests for each source path:
   - valid base64 PNG/JPEG/WebP/GIF repeated;
   - declared media type mismatch;
   - invalid/truncated image bytes;
   - tool_result image plus text;
   - multiple tool_result images;
   - file source materialization;
   - remote source timeout/limit/capacity errors.
