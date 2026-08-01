# HTML `<br>` output tag contamination - 2026-07-31

Status: `analysis-recorded / normal-context-not-reproduced / positive-control-only / fix-decision-pending`

Severity: P1/P2. User-visible assistant output may contain raw HTML-like tags such as `<br>` when the user expected plain text. The abnormal case is unsolicited HTML-like output in normal prose; explicit HTML/code/web-display answers must still be allowed to output `<br>`.

Last observed: 2026-07-31 Asia/Shanghai

## 范围与结论

This issue records a suspected output contamination where the assistant visibly emits an HTML line-break tag in a context that should be normal prose, possibly as a standalone line:

```text
line one
<br>
line two
```

Current conclusion:

- The reported abnormal form, "normal assistant prose unexpectedly outputs standalone `<br>`", has not been reproduced yet.
- Plain-text blank-line, two-line copy, support reply, status update, prior-HTML-then-plain, stream, direct `tool_result`, and real Claude CLI Bash tool-result contexts did not produce unsolicited `<br>`.
- Explicit HTML/code or web-display formatting contexts can legitimately produce `<br>`; these are controls, not reproductions of the reported bug.
- Source inspection found no proxy path that converts normal newlines into `<br>`. The visible text path forwards model text into Anthropic `text_delta.text`; protocol sniffing currently targets internal envelopes such as `<function_calls>` and `<search_web>`, not HTML line-break tags.
- No sanitizer or parser fix is selected without a real unsolicited sample or a bounded product rule, because stripping `<br>` would break valid HTML/code answers.

## 现象与影响

Reported/suspected abnormal forms:

- standalone tag:
  - `line one\n<br>\nline two`

Legitimate controls:

- inline HTML-style line break in web-display or HTML/code contexts:
  - `欢迎使用<br>马上开始`
- other HTML-like tags can appear when the prompt asks for HTML, for example `<p>...</p>` inside a fenced HTML snippet.

Impact:

- In plain text or chat rendering, a raw `<br>` line looks like markup leakage.
- In Markdown renderers that pass HTML through, it may create unexpected line breaks.
- In code/HTML contexts, the same text can be legitimate and must not be globally removed.

## 根因与源码链

This is not currently proven to be a parser exception.

Evidence so far:

- Upstream model text can include ordinary HTML tags.
- `src/anthropic/stream.rs` and `src/anthropic/handlers.rs` sanitize internal transcript/tool/thinking protocol contamination, but do not treat ordinary HTML tags as contamination.
- `src/external_pool.rs` detects complete HTML responses from external providers as protocol errors, but that is different from a valid model text block containing `<br>`.
- `src/anthropic/stream.rs` `create_text_delta_events` pushes text through the transcript sanitizer and then through literal protocol sniffing; the sniffing branch recognizes internal tool/search envelopes and otherwise calls `emit_text_delta_raw`.
- `emit_text_delta_raw` JSON-wraps the kept text as `delta: { type: "text_delta", text }`; source search found no `<br>` insertion or newline-to-HTML mapping.

Important distinction:

- Internal XML-like protocol markers such as `<thinking>`, `<tool_choice>`, and `<search_web>` have dedicated handling because they can corrupt protocol state.
- Ordinary HTML text such as `<br>` can be either legitimate user-requested content or unwanted formatting. The proxy cannot safely infer intent from the output alone in every case.

## 复现

Direct `/cc/v1/messages`, local `127.0.0.1:9022`, model `claude-sonnet-4.5`, local account route.

Service and client:

- Local service: `127.0.0.1:9022`, PID `13048`, candidate SHA-256 `6aa907e78f26ce9eda8d36ea30fb104e73981abc05caeeb1f95d7715c2927cff`.
- Claude Code CLI: `2.1.220`, isolated `HOME` and `CLAUDE_CONFIG_DIR`, `ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc`, local API key redacted.

Plain text controls did not reproduce unsolicited `<br>`:

- `plain-two-paragraphs-cn`: output `alpha\n\nbeta`, no tags.
- `plain-blank-line-en`: output `alpha\n\nbeta`, no tags.
- `plain-greeting-signoff`: output `Hello there,\n\nBest regards`, no tags.
- `stream-plain-blank-line-cn`: output `gamma\n\ndelta`, no tags.
- `stream-poem-stanzas`: normal blank line between stanzas, no tags.

2026-07-31 clarification pass, direct protocol, normal context:

- 9 targeted direct cases: no standalone `<br>`, no inline `<br>`.
- Covered normal Chinese/English support replies, natural two-line copy, status update, prior assistant HTML followed by a plain-language request, direct non-stream `tool_result` with blank lines, direct stream long normal sections, and direct stream `tool_result`.
- Representative request ids:
  - `req_01BzGE5SCSsDHFctprUB7PVf` normal Chinese support reply.
  - `req_013NVE9jyg6TJReerDLQB7Pb` natural two-line copy.
  - `req_01nS9dKN8kRB8GMCB64YHAkJ` prior HTML history followed by plain summary.
  - `req_0111yWiqKFSv8SVKXp3Qnp9o` tool-result-only with blank lines.
  - `req_01cyxesDmp6JdTZws5NjzZ6C` stream long normal sections.

2026-07-31 clarification pass, direct protocol, ambiguity/occasionality probe:

- 15 additional normal but line-break-prone prompts with `temperature: 1`: no standalone `<br>`, no inline `<br>`.
- Covered app two-line copy, blank-line chat replies, release-note style lines, onboarding prompt, SMS two-line text, two-line poem, and prose summaries.
- Representative request ids:
  - `req_01Wi1vKyQF7MjHyAmgfMQvTq` app two-line welcome prompt.
  - `req_01NVg9DmRJ8xfhoJc7DkDszH` normal chat reply with a blank line.
  - `req_01ko61WpNUYAjLdX2xg3p2Qu` explicit blank line between `alpha` and `beta`.
  - `req_01DDTDgq25FCaRdg6Sz3QZaG` preserve two-line reading effect.
  - `req_01Evvtc8P3Cn8SvGh3qPEFoJ` two-line SMS.

2026-07-31 clarification pass, real Claude Code CLI:

- 4 real CLI `stream-json` normal cases: no standalone `<br>`, no inline `<br>`.
- Covered normal Chinese support reply, natural two-line copy, status update, and Bash tool output with blank lines followed by normal prose.
- CLI sessions:
  - `6e740187-90e3-4aa9-a10e-7436f2080cfc`
  - `dd1af46f-0c4f-463b-92ba-4e858a1c063c`
  - `3e7a048c-8cf3-406c-a1b5-2de4a6847d04`
  - `bfc5280e-202d-4afc-820b-dd6a3593f25f`
- Bash case produced `toolUses=["Bash"]`, `toolResultCount=1`, and final text describing the blank line without HTML tags.

Controls, not abnormal reproductions:

- Web-display prompt: convert two lines of copy for web display and preserve line breaks.
- Output contained inline tag:
  - `欢迎使用<br>马上开始`
- This is not counted as a reproduction of unsolicited normal-prose contamination after user clarification, because the prompt was display-formatting oriented.

Positive-control pass-through:

- Prompt: return three lines; line one, standalone HTML br tag, line two.
- Direct output:

```text
line one
<br>
line two
```

- Real Claude CLI `2.1.220` through local `9022/cc` also produced standalone `<br>` under the same positive-control shape.
- This proves the proxy does not strip legitimate model-authored HTML text. It does not prove the abnormal normal-prose bug.

Other tag control:

- Prompt asking for a short HTML email body produced fenced HTML with `<p>` tags, which is expected for an HTML/code request.

## 方案与取舍

Do not apply a broad HTML-stripper without a requirement decision.

Options:

1. **No output filtering.**
   Treat `<br>` as model-authored text. Lowest protocol risk, but visible leakage remains.

2. **Prompt-level style steering.**
   Add or strengthen a plain-text/no-HTML instruction for non-code, non-HTML contexts. Lower output mutation risk, but not deterministic.

3. **Narrow output normalization.**
   Replace standalone `<br>`, `<br/>`, and `<br />` lines with blank lines only outside fenced code blocks and only when the request is not explicitly asking for HTML/code. This directly addresses the reported symptom but needs an intent classifier or conservative request heuristics.

4. **Diagnostic-only first.**
   Record a bounded warning/usage marker when standalone HTML-like tags appear outside code fences. This helps quantify recurrence before changing output.

Current selected state:

- No code filter yet.
- Keep this as `fix-decision-pending` until the desired behavior is confirmed.
- If the product expectation is "visible assistant prose should never show standalone `<br>` unless the user asked for HTML/code", implement option 3 with focused stream/non-stream tests and a positive-control exception.
- Given the current evidence, the safer next step is diagnostic-only capture if unsolicited tags recur: record request id, raw upstream text/SSE, downstream Anthropic SSE, and final client-visible text so parser mutation can be distinguished from model-authored text.

## 验证与证据

Commands run in this analysis:

- direct non-stream blank-line matrix: no standalone `<br>`.
- direct stream blank-line matrix: no standalone `<br>`.
- direct targeted normal matrix, 9 cases: no standalone or inline `<br>`.
- direct ambiguity/occasionality probe, 15 cases with `temperature: 1`: no standalone or inline `<br>`.
- real Claude CLI `stream-json` normal matrix, 4 cases: no standalone or inline `<br>`.
- source search for `<br>` insertion/newline-to-HTML mapping: none found in `src/anthropic`.
- direct positive-control standalone tag: legitimate pass-through.
- real Claude CLI `stream-json` positive-control standalone tag: legitimate pass-through.
- semi-natural web-display prompt: legitimate/ambiguous display-format output, not a normal-prose reproduction.

Evidence summary:

| Case | Result |
| --- | --- |
| Plain text blank lines | no HTML tags |
| Markdown changelog, no HTML | no HTML tags |
| Direct normal/history/tool-result/stream clarification matrix | no `<br>` |
| Direct 15-case line-break-prone ambiguity probe | no `<br>` |
| Real Claude CLI normal/tool clarification matrix | no `<br>` |
| Web display, preserve line breaks | inline `<br>`, control only |
| Explicit standalone br tag | standalone `<br>`, pass-through control only |
| HTML email body | expected `<p>` tags in fenced HTML |

## 残余风险与边界

Residual risk:

- The reported production/user symptom may involve a different tag or a tool/model context not covered here.
- The abnormal event may be intermittent and require the exact user conversation, model state, or client rendering layer to reproduce.
- A sanitizer that removes all HTML tags would break legitimate code, HTML, Markdown-with-HTML, web authoring, and documentation outputs.
- A sanitizer that only removes standalone `<br>` may still miss inline `<br>` or other tags such as `<p>`, `<div>`, `<hr>`, or `</br>`.

Rollback boundary:

- If a future filter is added, it must be behind a focused output-normalization function with tests for code fences, explicit HTML requests, inline tags, standalone tags, streaming chunk boundaries, and non-streaming responses.
- If false positives appear, disable the filter and keep only diagnostics.
