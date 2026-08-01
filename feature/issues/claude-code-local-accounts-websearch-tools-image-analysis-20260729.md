# Claude Code local-account WebSearch/tools/image analysis - 2026-07-29

Status: `analysis-recorded / local-account-real-call-evidence-collected / wave1-websearch-direct-and-cli-focused-verified / fixes-pending`

Severity: P0/P1。WebSearch direct native 与 Claude Code CLI focused path 已在本地账号路径验证，但完整 mixed tool 状态机、工具长历史/名称映射边界、模型报告清晰度和图片 remote/file/tool_result/source 矩阵仍可能导致用户可见失败。

Last observed: 2026-07-31 Asia/Shanghai

Last updated: 2026-07-31 Asia/Shanghai

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
- [WebSearch/MCP protocol, errors, usage, attempts and privacy](websearch-mcp-protocol-usage-and-privacy.md) covers native WebSearch MCP correctness/privacy; this document records the 2026-07-31 focused local-account verification for versioned direct native and Claude Code CLI WebSearch.
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

### Current behavior after 2026-07-31 focused fix

`web_search_20250305` is not a random special-case name. It is Anthropic's versioned server-tool shape, and current official documentation also lists newer `web_search_20260209` and `web_search_20260318` forms. The implemented compatibility rule is therefore:

- treat `type: "web_search_YYYYMMDD"` plus `name: "web_search"` as native WebSearch;
- keep the known official versions only for observability;
- allow future version-looking types such as `web_search_20270101` through the same basic WebSearch path instead of failing closed;
- reject only the malformed native shape where `type` is versioned WebSearch but `name` is not `web_search`;
- do not hijack same-name custom client tools unless the native `type` is present.

Focused direct live validation against the current local `9022` candidate:

| Case | Tool shape | Stream | Request id | Result |
| --- | --- | --- | --- | --- |
| official current | `web_search_20250305` | no | `req_01GDJcJ8QCyBm5q4j6MZzzhW` | HTTP 200, `server_tool_use=1`, `web_search_tool_result=1`, ordinary `tool_use=[]` |
| official current | `web_search_20260318` | no | `req_01FuBYdXdnQBk8f3rmGgMnE1` | HTTP 200, `server_tool_use=1`, `web_search_tool_result=1`, ordinary `tool_use=[]` |
| future-looking format | `web_search_20270101` | no | `req_01Z1k1iXfg8qLFX38PerhnXd` | HTTP 200, `server_tool_use=1`, `web_search_tool_result=1`, ordinary `tool_use=[]`; log marks `native_websearch_unlisted_version=true` |
| mixed native + client tool | `web_search_20260318` plus `echo_value` | yes | `req_01DoTcTMR8zc37cqd584eopP` | HTTP 200, streamed `server_tool_use=1`, `web_search_tool_result=1`, ordinary `tool_use=[]`, `message_stop=true` |

All four usage records show:

- `status=success`
- `model=claude-sonnet-4.5`
- `upstreamModel=claude-sonnet-4.5`
- `routeKind=local_credential`
- `routeSubtype=local_success`

The admin usage endpoint did not expose a credential id for these rows, but the service startup loaded the two local credentials and the route is local credential, not external pool.

Real Claude Code CLI focused validation against the same local service:

- Command shape: real `claude` binary `2.1.220`, isolated `HOME`/`CLAUDE_CONFIG_DIR`, `ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc`, `--model claude-sonnet-4.5 --tools=WebSearch --allowedTools=WebSearch`.
- Session: `10c87c84-4b3f-4b74-81e1-de663161621f`.
- Result: exit `0`, `toolUseNames=["WebSearch"]`, `toolResultCount=1`, final answer included search-derived sources, `hasInternalLeak=false`.
- Latest usage rows for the CLI turn show local success on `claude-sonnet-4.5`, including `req_01WjEJPnvPcjyYUWnzEcx6bA`, `req_01L5yowV6HXGk9mKALN6n2cC`, `req_01fLHtwh1oXS2Yhc9SDYiGTa`, and `req_01yG1AWc1TUFTqAgvmKxbWSy`.

Important boundary: this proves the direct native server-tool path and the Claude CLI client-side `WebSearch` path. It does not yet implement a complete Anthropic mixed-tool state machine where the model can freely alternate native server tools and ordinary client tools in the same turn. The current server behavior is: when a native WebSearch declaration is present, execute the server-side WebSearch branch and return WebSearch blocks instead of falling through to normal tool conversion.

### What worked before this focused fix

Pure native Anthropic WebSearch already worked through the local account path:

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

### What failed before this focused fix

1. Claude Code CLI `--tools WebSearch --allowedTools WebSearch` did not produce a real tool call in one earlier local run.

   Evidence:

   - CLI case `websearch_tool`, model `claude-sonnet-4.5`
   - exit `0`, but `toolUseNames=[]`
   - `toolResultCount=0`
   - final usage `server_tool_use.web_search_requests=0`
   - assistant text contained pseudo XML:
     - `<search_web>`
     - `<query>current date time Shanghai China</query>`
   - No actual WebSearch execution happened.

   This meant the compatibility layer needed to tolerate the pseudo XML shape when a known `WebSearch` tool was declared. The current stream/non-stream parser now has focused tests for complete `<search_web><query>...</query></search_web>` recovery, while the latest real CLI run produced a proper `tool_use name="WebSearch"` and completed normally.

2. Native WebSearch used to be detected only when it was the only tool.

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

   This was a real compatibility bug for Claude Code style multi-tool turns. If the caller had no local executor for `web_search`, it looked like WebSearch was unsupported or broken.

### 2026-07-31 focused fix

The selected behavior for native WebSearch is server-side execution for any versioned native WebSearch declaration:

- `src/anthropic/websearch.rs` now distinguishes:
  - native WebSearch: tool named `web_search` with `type: "web_search_YYYYMMDD"`;
  - pure native WebSearch: exactly one native WebSearch tool;
  - mixed native WebSearch: a native WebSearch tool plus at least one other tool;
  - unlisted/future version-looking native WebSearch types;
  - misnamed native WebSearch types;
  - same-name custom client tools without native type.
- `src/anthropic/handlers.rs` now routes any native WebSearch declaration to the server-side MCP/WebSearch branch before normal tool conversion.
- Mixed native WebSearch no longer returns an ordinary `tool_use name="web_search"` with no executor.
- Future-looking `web_search_YYYYMMDD` types are accepted and logged as unlisted, not rejected.
- Misnamed native WebSearch still fails with HTTP `400 invalid_request_error` and public message `The native web_search tool must be named web_search.`
- Same-name custom client tools are not hijacked by the native detector.
- `src/anthropic/stream.rs` now recovers complete Claude Code-style pseudo XML `<search_web><query>...</query></search_web>` into a synthetic `WebSearch` tool_use only when a known WebSearch tool is available and the text is at a protocol-visible position.

Focused evidence:

```bash
feature/tests/run-cargo-scoped.sh wave1-websearch-version-format-tests2 -- bash -lc 'cargo test -q native_websearch_detection -- --nocapture && cargo test -q native_websearch_current_official_and_future_version_formats_route_to_mcp -- --nocapture && cargo test -q literal_search_web_protocol -- --nocapture && cargo test -q websearch_canonical_detection_and_current_long_history_query_are_exact_for_five_rounds -- --nocapture'
feature/tests/run-cargo-scoped.sh wave1-websearch-version-format-check -- cargo check --all-targets --locked
```

Result:

- native WebSearch detection: pass, including official current versions and future-looking `web_search_20270101`.
- handler WebSearch routing matrix: pass; pure native hits MCP, same-name custom tool still hits normal upstream, mixed native hits MCP and returns `web_search_tool_result`.
- literal `<search_web>` protocol recovery: pass for stream and non-stream, including negative controls for unknown tool/code-fence/plain-text positions.
- `cargo check --all-targets --locked`: pass.
- Full candidate build evidence: scoped `cargo fmt --check && cargo test && cargo build --release` passed with main tests `1831 passed / 0 failed / 6 ignored`, `kiro_loadtest` `31 passed / 0 failed`; candidate SHA-256 `46ce4540fc23f121c4cc5f349e4da722db3da04f790fb3ff438b54e6a7129711`.

### Likely causes

- Server-side native WebSearch implementation was originally a special-case shortcut for one pure tool type, not a version-family detector.
- Claude Code CLI can use a client-side `WebSearch` tool, but an earlier local run showed the model may also emit Claude Code-style pseudo XML instead of a formal `tool_use`.
- Before the 2026-07-31 focused fix, mixed native WebSearch plus other tools fell through to normal tool conversion, where `web_search` became a regular model tool_use instead of being executed by the server.
- If `compatProfile` is strict, the handler rejects native WebSearch with `"The web_search tool is not supported for this request."`; current runtime was not strict enough to reject the pure native case.

### Fix direction

Selected current behavior:

- Native `web_search_YYYYMMDD` is accepted generically and executed through the existing server-side MCP WebSearch branch.
- Mixed native WebSearch is executed server-side instead of falling through to ordinary tool conversion.
- Future-looking version suffixes are allowed and logged as unlisted, so new official WebSearch versions do not require a code release just to stop being rejected.
- Claude Code CLI client-tool WebSearch is focused-verified with the current CLI/service combination.
- Complete mixed native/client tool alternation remains a separate product/design decision if a future compatibility requirement needs it.

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

Observed historical logs:

- Direct `echo_value` logged `工具名称映射: 1 个超长名称已缩短`.
- CLI `Bash` and `Read` each logged `工具名称映射: 1 个超长名称已缩短`.
- The log message says "overlong", but the mapping also happens for names that are not overlong, such as `Bash`, `Read`, and `echo_value`, because sanitization changes case/underscore form.

2026-07-31 focused fix:

- The mapping algorithm is unchanged for compatibility.
- `src/anthropic/converter.rs` no longer logs every mapping as an overlong-name shortening.
- New structured log fields:
  - `mapped_tool_name_count`
  - `sanitized_tool_name_count`
  - `overlong_tool_name_count`
- `src/anthropic/converter/tools.rs` now has a focused summary helper covered by a regression test for `Bash`, `echo_value`, and a true overlong tool name.

2026-07-31 tool parsing focused reproduction and fix:

- Deterministic tests passed for:
  - safe short name passthrough;
  - separator/PascalCase normalization;
  - sanitized and overlong mapping counters;
  - raw-vs-mapped name collision rejection;
  - precomputed real hash collision rejection;
  - tool_choice structured filtering;
  - invalid schema property key sanitization and reverse mapping;
  - stream reverse mapping for tool names and sanitized schema keys.
- Live direct local-account matrix on the current candidate passed:
  - `Bash` returns downstream `tool_use name="Bash"`;
  - `weather-lookup` returns downstream `tool_use name="weather-lookup"`;
  - `bad name` returns downstream `tool_use name="bad name"` after Kiro-safe mapping;
  - overlong `mcp__plugin_very_long_server_name__extremely_long_tool_name_exceeds_63` returns the original long name downstream;
  - `schema_probe` with invalid keys returns downstream input keys `bad key` and `nested/key`, not the generated Kiro-safe hash keys;
  - ambiguous normalized `tool_choice name="fooBar"` for `foo-bar` + `foo_bar` returns local HTTP `400 invalid_request_error`;
  - raw-vs-mapped collision returns local HTTP `400 invalid_request_error`.
- A real bug was reproduced in Claude CLI and direct protocol:
  - When the final user turn contained only a paired `tool_result` and no text, the converter set current content to `"."`.
  - The structured `tool_results` were present, but Kiro often ignored them and answered generic readiness text.
  - CLI repro before the fix: `Bash` produced `tool_result="cli-bash-ok"`, but the final model text was generic Chinese readiness text.
  - Direct repro before the fix: paired current `tool_result` only returned generic readiness text and header `tool-result-content-placeholder=1`; adding any short text after the tool_result made the model consume the result.
- Fix:
  - `src/anthropic/converter.rs` keeps the truly empty user placeholder as `"."`.
  - Tool-result-only turns now use `Tool result received.` as the non-empty Kiro current-message marker.
  - This avoids an internal `user Continue` transcript while giving Kiro enough semantic boundary to consume `context.toolResults`.
- Post-fix evidence on the restarted local candidate SHA-256 `6aa907e78f26ce9eda8d36ea30fb104e73981abc05caeeb1f95d7715c2927cff`:
  - direct paired current `tool_result` only with `Bash` returned `direct-fixed-ok`;
  - real Claude CLI `Bash` returned `toolUses=[Bash]`, `toolResults=["cli-fixed-ok"]`, final text `cli-fixed-ok`;
  - CLI request ids `req_017Ak2o1y98qaae2jLUPueiZ` and `req_01zLtw32ZEke7gsrReKEaStw` both stayed on the local `claude-sonnet-4.5` path and did not leak internal scheduler/credential terms.

The simple tool cases passed because response tool names were mapped back to Claude Code names. But the mapping layer can still explain "tools parsing is wrong" in more complex cases:

- Multiple original names can normalize to the same Kiro-safe name and be rejected as ambiguous.
- Historical assistant tool_use names and current tool definitions must share the same reverse map. Missing or stale mapping can break tool_use/tool_result pairing.
- Tool-choice by name can become ambiguous after normalization.
- Older logs made mapping look like an overlong-name-only path, hiding the fact that normal built-in names are also being rewritten. The 2026-07-31 observability fix separates total mapped, sanitized, and overlong counts.
- Old 2026-07-28 logs from the wrong model path showed `prefill-dropped=1` on Claude Code-style requests. Prefill dropping is intentional, but it can make debugging tool history harder if the client expected a terminal assistant prefill to survive.

Current local-only evidence does not show a minimal JSON parser failure for tools. It shows tool compatibility is dependent on the converter's name/schema/pairing repair layer, and one concrete pairing-adjacent bug was the tool-result-only current turn using an inert `"."` placeholder.

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
   - old native detection only matched one exact type and only when it was the only tool;
   - mixed native WebSearch plus normal tools previously fell through as ordinary `tool_use web_search`;
   - one earlier Claude CLI run emitted pseudo XML rather than a formal WebSearch tool call;
   - current focused fix supports versioned direct native WebSearch, mixed native server-side execution, and pseudo XML recovery, while latest real CLI emitted a proper `WebSearch` client tool call.
3. Tool compatibility depends on converter repair:
   - simple direct and CLI Bash/Read tools work;
   - names are rewritten even for normal built-in names, then reversed;
   - tool-result-only current turns previously used an inert `"."` placeholder and could make Kiro ignore otherwise valid tool results; this is fixed with `Tool result received.`;
   - ambiguous normalization, stale history mapping, schema-key normalization, or prefill/tool pairing repair can still break complex tool sessions.
   - mapping observability now reports sanitized-vs-overlong counts, so normal built-in name rewrites are visible.
4. Image instability is likely payload/source dependent:
   - valid base64 PNG and CLI Read image worked;
   - invalid image bytes are rejected locally;
   - remote/file materialization, media-type mismatch, size limits, tool_result image placeholders, or payload guard trimming are the main paths to inspect for intermittent failures.

## Root cause / 根因

The current evidence points to multiple compatibility gaps rather than a single unavailable-account root cause:

- WebSearch root cause: server-side native WebSearch was previously implemented as an exact single-tool branch for `web_search_20250305`. Before the focused fix, mixed native WebSearch requests bypassed that branch and became ordinary `tool_use web_search`; one earlier Claude CLI run also emitted pseudo XML rather than a formal tool call. Current behavior accepts `web_search_YYYYMMDD`, executes mixed native requests server-side, recovers complete pseudo XML in the parser, and has a latest real CLI pass for `WebSearch`.
- Tools root cause: current simple tool paths work, but Kiro-safe tool-name and schema-key mapping is part of the compatibility layer. The concrete 2026-07-31 bug was not name reverse mapping; it was current tool-result-only turns using `"."` as the Kiro content placeholder, which made real CLI follow-up answers ignore valid tool_result content. Ambiguous normalization, stale reverse maps, tool_choice filtering, or long-history pairing can still break complex sessions.
- Image root cause: valid inline and CLI Read image paths work. Intermittent failures are likely source/materialization/limit dependent rather than a blanket local-account or model failure.
- Model root cause: local accounts only prove `claude-sonnet-4.5`; aliases or echoed requested model names can obscure the actual upstream model used for the request.

## Reproduction / 复现

Use the local-only setup recorded in the Scope section: `127.0.0.1:9022`, `ccman` current profile `local-kiro-rs-9022-current`, external pools disabled, and credential `7` / `8`.

Minimal repro classes:

- Pure native WebSearch: direct `/cc/v1/messages` body with only `web_search_20250305`, `web_search_20260318`, or future-looking `web_search_YYYYMMDD` should return `server_tool_use` and `web_search_tool_result`.
- Mixed WebSearch focused fix: direct body with `[web_search_YYYYMMDD, echo_value]` now returns HTTP 200 with server-side WebSearch blocks instead of ordinary `tool_use name="web_search"` without a server tool result.
- CLI WebSearch: `claude --print --model claude-sonnet-4.5 --tools=WebSearch --allowedTools=WebSearch ...` should produce `tool_use name="WebSearch"`, one `tool_result`, and final search-derived text.
- Pseudo XML fallback: if upstream text contains complete `<search_web><query>...</query></search_web>` at a protocol-visible position while a known WebSearch tool is declared, the stream parser should recover it into an executable tool_use; unit tests cover this path.
- Tool controls: direct forced `echo_value`, CLI `Bash`, and CLI `Read` image are working controls.
- Tool-result-only follow-up: a user turn containing only a paired `tool_result` should use the `Tool result received.` Kiro content marker and answer from the tool output, not generic readiness text.
- Image controls: valid base64 PNG succeeds; invalid/fake image bytes return local 400.

## Suggested next tests/fixes

1. WebSearch direct/CLI focused fix landed on 2026-07-31.
   - Selected behavior: accept `web_search_YYYYMMDD`, execute native WebSearch server-side even when other client tools are declared, and recover complete `<search_web>` pseudo XML when a known WebSearch tool is available.
   - Remaining optional product work: design a complete mixed native/client tool state machine if Anthropic/Claude Code compatibility requires the model to alternate server tools and ordinary client tools in one turn.

2. Clarify model reporting for remapped requests.
   At minimum, expose/record `requestedModel` and `upstreamModel` consistently. Consider whether downstream `model` should echo requested model or actual upstream model for `/cc` compatibility.

3. Tool mapping observability: focused fix landed on 2026-07-31.
   - The log now says names were normalized/mapped and includes separate sanitized-vs-overlong counts.
   - Tool-result-only current turn placeholder fix also landed on 2026-07-31 and is live-verified through direct and Claude CLI `Bash`.
   - Remaining work is broader long-history/tool_choice coverage, not the misleading log or current tool-result-only placeholder itself.

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

## Solution / 修复方案

Focused implementation recorded on 2026-07-31:

- Native WebSearch detection now supports `web_search_YYYYMMDD` generically. Current official versions are tracked only for observability, and future-looking versions execute through the same basic WebSearch path.
- Mixed native WebSearch now executes server-side before normal tool conversion. This prevents returning an ordinary `tool_use name="web_search"` that no client-side executor handles.
- Claude Code-style `<search_web><query>...</query></search_web>` pseudo XML can be upgraded to an executable WebSearch tool_use when a known WebSearch tool is declared.
- Tool-name mapping observability now reports normalized/mapped names accurately, with separate total/sanitized/overlong counters.
- Tool-result-only current turns now use `Tool result received.` instead of `"."`, so valid CLI/direct tool results are consumed by Kiro on the follow-up turn.

Still open:

- WebSearch: complete mixed native/client tool alternation remains a design gap; focused direct native and real CLI `WebSearch` are verified.
- Model reporting: expose/record requested vs resolved/upstream model more clearly for local accounts limited to Sonnet 4.5.
- Tools: add broader regression tests for multi-tool long histories, original-name tool_choice, repeated tool_result content with images, and stale reverse-map edge cases.
- Images: expand regression coverage across inline, file, remote URL, tool_result, multiple images, size limits, and timeout/capacity failures.

## Residual risk / rollback

Residual risks:

- This document records local-account evidence from a small controlled run. It does not prove all local account/model/catalog states are safe.
- Direct native WebSearch and Claude Code CLI WebSearch are different protocol shapes; both have focused passes now, but a complete mixed native/client state machine is still not implemented.
- Tool mapping algorithm behavior is unchanged; observability is improved, but ambiguous normalization and stale reverse-map/history cases still need broader regression coverage.
- Image success for simple PNG and CLI Read does not close remote/file/materialization and payload guard boundaries.

Rollback boundary:

- Do not re-enable external-pool evidence as the authority for this local-account diagnosis.
- Do not claim WebSearch is unsupported globally; direct native WebSearch and current Claude CLI WebSearch focused paths work.
- Do not claim images are fixed globally until broader image-source gates pass.
