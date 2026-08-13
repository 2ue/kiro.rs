# Handler EventStream And Runtime Stack Matrix Evidence

Date: 2026-07-16

Status: `focused-matrix-and-release-2mib-pass / final-cli-http-load-pending`

## 身份与隔离

- Source baseline: `401473ca1649997bdeccf4468e3add1bdb187248` plus the recorded dirty remediation tree.
- Claude Code CLI: `2.1.197`.
- Test shape: real Axum Anthropic Router + reqwest `KiroProvider` + loopback fake upstream + synthetic credentials.
- No production port, account, API credential, Redis namespace or upstream was used.
- The protected `127.0.0.1:9022` service was not contacted or restarted.

该页记录聚焦测试事实，不冒充最终 release/CLI/load 证据。最终候选 binary SHA 必须在冻结后另行追加。

## Provider 与 handler 所有权

标准 `HTTP 200 + application/json` exception 在 provider 层完成 typed 分类和 credential retry，不进入 handler stream sniffer。`application/vnd.amazon.eventstream + JSON bytes` 才进入 decoder，并按 `protocol_error` 处理。handler precommit 主矩阵因此为 read error、idle、bad CRC、truncated、incomplete status 和 protocol contamination 六类。

## 已完成计数

| Matrix | Pass | Attempts/hits and telemetry |
| --- | ---: | --- |
| handler precommit 6 faults x 5 | 30/30 | 2 hits; `streamRetryAttempts=1`; no dispatch failure |
| provider JSON retry + single failure | 10/10 | dual 2 hits and alternate credential; single 1 hit/429; all handler retry fields null |
| EventStream CT + JSON bytes | 5/5 | 2 hits; `protocol_error:sends=1` |
| single credential bad CRC | 5/5 | 1 hit; no replay; `streamRetryDispatchFailures=1` |
| postcommit text/thinking/tool | 15/15 | 1 hit; SSE error; no fake `message_stop` |
| unknown-only stream | 5/5 | 2 hits and recovery; no private marker |
| missing completion after text | 5/5 | 1 hit; SSE error; no `message_stop` |
| non-stream unknown/missing | 10/10 | 502; 1 hit; usage Error |
| legacy metadata and complete tool controls | 20/20 | stream/non-stream success; 1 hit |
| non-stream response limit/recovery | 20/20 | declared/chunked over rejected then recovered; exact/small pass |
| JSON exception privacy | 5/5 | downstream/usage/DEBUG marker matches: 0 |
| WebSearch handler complete/drop/privacy/error/recovery | 90/90 | attributed attempts bounded and sequential; raw query/result marker matches: 0 |

WebSearch 的 90 轮由 complete、never-polled drop、partial drop、non-stream success、privacy，以及 12 类错误与 recovery 组成。错误覆盖 HTTP 400/429/500、header/body timeout、disconnect、malformed JSON、JSON-RPC error、`isError`、mismatched id、Content-Length over 和 chunked over。

## 正常能力单点

- transcript sanitizer: 7 fixtures x 1,000 unique partitions = `7,000/7,000`。
- converted history: 20/100 tool-cycle atomic trim, 5 rounds each = `10/10`。
- GIF/WebP: base64 and data URL round-trip, 5 rounds per form = `20/20`。
- 16 MiB response controls: exact limit and small normal remain valid after over-limit failures。
- legacy terminal controls prevent the fail-closed change from rejecting text+metadata and complete `stop=true` tool use。

## Runtime stack evidence

旧 debug binary 的阈值为 1/2 MiB abort，4/8/16 MiB pass。最小 JSON exception case 在 2 MiB debug 仍 abort。future object sizes were 576 B (case), 472 B (handler call) and 144 B (upstream start), so the evidence points to unoptimized debug poll/call depth rather than a multi-megabyte future object.

Historical interim release test binary content:

```text
target/release/deps/kiro_rs-8e21067b2ccc5c02
sha256 4cf63c759a39d1f1987dbdf7ecc0b1da3bca3c7c622d7902cc2a7d9d51e96d15
```

该 binary 的显式 2 MiB Tokio worker 最小 case 为 `1/1`，当时完整 35-case matrix 为 `35/35`。它早于最终 provider ownership/fixture 调整，只是 release 反证，不是最终发版绑定。

The current checkpoint rebuilt the same Cargo artifact path with different content:

```text
build completed 2026-07-16 17:23:59 +0800 (9m01s)
target/release/deps/kiro_rs-8e21067b2ccc5c02
size 28293904 bytes
sha256 3b7825c33ff1c4fde3d3856a239852af7f36882f14bcf22a7d4ff7b168243a2e
explicit 2 MiB Tokio worker, bad-CRC handler retry: 1/1 PASS
```

Because Cargo reused the artifact filename, SHA and timestamp are the authority; the path alone is not evidence identity.

## 当前 checkpoint 最终 focused 重跑

- handler precommit 6 faults x 5: `30/30`。
- provider standard JSON dual/single: `10/10`。
- EventStream Content-Type + JSON bytes: `5/5`。
- single-credential bad CRC dispatch failure: `5/5`。
- unknown-only after common 4 MiB debug fixture wrapper: `5/5`。
- future object sizes: 576 B / 472 B / 144 B。
- transcript sanitizer module: `29/29`，其中 unique partition fixture 为 `7,000/7,000`。
- payload guard module: `64/64`。
- converter module: `114/114`。
- stream state-machine module: `100/100`。

运行中另发现 unknown-only 在默认约 2 MiB debug libtest 栈会 abort，证明仅包装两个原始重型 fixture 不足。测试基础设施已将同一 Router fault family 统一放入独立 4 MiB OS thread + current-thread Tokio runtime；默认测试命令随后通过。生产源码/runtime 未因此改变。

## 最终候选重跑命令

```bash
cargo fmt --all -- --check
git diff --check
cargo check --all-targets

cargo test --bin kiro-rs handler_eventstream_precommit_faults_retry_once_and_recover_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs provider_json_exception_retry_and_single_credential_failure_are_private_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs eventstream_content_type_with_json_bytes_uses_protocol_retry_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs handler_single_credential_precommit_retry_is_bounded_and_fails_closed_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs handler_eventstream_postcommit_faults_never_retry_or_fake_success_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs handler_unknown_event_only_retries_before_empty_success_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs handler_missing_completion_after_text_fails_closed_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs handler_non_stream_untrusted_eof_fails_closed_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs handler_legacy_metadata_and_complete_tool_are_trusted_terminals_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs handler_non_stream_response_body_limit_and_recovery_hold_for_five_rounds -- --nocapture --test-threads=1
cargo test --bin kiro-rs websearch -- --test-threads=1
cargo test --bin kiro-rs kiro::provider::tests::mcp_ -- --test-threads=1
```

## Pending release evidence

- Bind the already-passing release-only explicit 2 MiB worker gate to the final tag binary SHA.
- Start a temporary release service and independent fake upstream; run at least 1,000 real HTTP normal/fault requests plus three bursts while sampling RSS, FD and threads.
- Run Claude Code CLI C1-C4 on the same frozen SHA: normal, alias, thinking deltas/tokens, Bash/Read, MCP, 20/100 tool cycles, long history and resume.
- Preserve redacted JSONL/SSE, request/error IDs, upstream hit deltas, usage summaries and post-run port cleanup evidence.

Until these are complete, the supported statement is: focused Router/provider/state-machine tests are green and bounded for the enumerated fixtures; production-scale and real CLI behavior on the frozen release candidate are not yet proven.
