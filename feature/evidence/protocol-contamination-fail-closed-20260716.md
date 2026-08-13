# Protocol Contamination Fail-Closed Evidence

Status: `focused-state-machine-pass / cli-and-fault-injection-pending`

Date: 2026-07-16

Source authority: HEAD `401473c` (`v0.0.109`) plus dirty-tree remediation changes. Current test binary: `target/debug/deps/kiro_rs-d0eac30c038749e6`, SHA-256 `c8de4b66d935a89d0378598e28ce775b5212dfbb94e55ac5cab98b1fc77e4d61`. This is not a release build.

## Reproduced Defect

The response sanitizer could suppress an internal transcript and still let callers synthesize a blank text block, normal `message_delta`/`message_stop`, or a partial 200 response. Marker disappearance therefore did not prove protocol correctness: an answer could become an unexplained blank success or silent truncation.

The historical response fingerprints included `user Continue` plus a known raw or deterministically mapped tool name, legacy `user Tool results provided.` plus `Tool results:`, and roleless result scaffolding. Contamination was reproduced in text, unsigned thinking, signed-thinking content, signature-only content and redacted data. The detector does not trust the `Hashxxxxxxxx` shape by itself.

False-positive fixtures keep standalone/inline markers, arbitrary hash-shaped artifact names, fenced/quoted/indented examples, user content, tool input and tool results value-identical. Clean signed/redacted blocks are atomic and value-identical; they are never decoded or rewritten.

## Selected Contract

- Local stream before downstream commit: classify as protocol contamination and use the existing precommit retry within the shared inference budget.
- Local stream after commit: zero retry, close open content blocks, emit an SSE error, and never emit success terminal events.
- Local non-stream: return a sanitized 502 and record usage `Error`; never return a blank/partial 200.
- External normalized non-stream: classify the pool attempt as retryable protocol contamination and use existing bounded pool failover.
- External SSE: fail closed for text, unsigned thinking, signed thinking, and redacted thinking; suppress later success/tool/usage terminal events.
- Signed/redacted thinking remains atomic. Sanitized text is never recombined with an original signature.

Public responses use a common sanitized processing-failure message and request/error ID. Internal transcript markers are not placed in public errors.

## Executed Matrix

```text
cargo test external_sse_ -- --nocapture
12/12 passed

cargo test transcript -- --nocapture
37/37 passed

cargo test external_non_stream_ -- --nocapture
2/2 passed

cargo test reasoning_leak -- --nocapture
2/2 passed

polluted native signature focused test
1/1 passed

XML thinking character-partition test
1/1 passed

retry-policy/terminal focused test
1/1 passed

cargo check --tests
PASS

git diff --check
PASS at the end of the protocol subtask
```

The test bodies repeat critical classes five times. Coverage includes contamination in the first content event, safe prefix plus contamination in one event, contamination after visible text, tool-boundary and EOF confirmation, LF/CRLF/multi-data-line/one-byte transport partitions, text and all thinking forms, signature arriving after per-character thinking deltas, pollution only in signature/redacted data, atomic buffer overflow, usage terminal state, clean signed-thinking identity, and clean non-stream byte identity.

Request-history coverage additionally includes cross-message/block candidates, current and historical tool names, raw unknown-field preservation, Unicode escaped markers/newlines, a 1 MiB clean escaped body and identity boundaries. This is not the D02 20-tool/120k-history real-CLI gate.

## Remaining Gates And Tradeoffs

Local non-stream currently fails directly with 502 rather than performing response-level credential retry. External SSE does not retry another pool after an HTTP response has been established, even if no downstream content byte was emitted; it guarantees an explicit stream error and error usage state instead. Local-stream contamination retry still needs an HTTP fake-upstream fault harness. Real Claude Code CLI, long session/resume, tool loops, and C06 fault injection remain release blockers.

The incremental candidate buffer is capped at 4 KiB. Signed/redacted/native atomic thinking uses a 1 MiB cap and fails closed on overflow. Marker-free raw requests avoid JSON DOM parsing; bodies containing Unicode escapes use the precise path to prevent escaped-marker bypass. Release p95 scaling and L5 resource stability remain separate gates.
