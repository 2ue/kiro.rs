# Bare invoke protocol evidence, 2026-07-17

Scope: response-text-to-tool boundary in `src/anthropic/stream.rs`, including stream/non-stream identity, strict wrapper recovery, chunk invariance, structured-tool deduplication and build cleanup.

Current status: component gates pass; real Claude Code CLI C2 awaits a repository-external frozen release binary.

## Source behavior under test

- A bare known or unknown `<invoke>` remains text.
- Unknown, malformed, mismatched and incomplete function-call wrappers remain atomic text.
- Exact protocol-root wrappers with fully known, strict calls recover tools.
- Code fences, lists, blockquotes, inline code, ordinary explanations and prompt-injection-style explanatory prefixes do not recover tools.
- A later equivalent structured `ToolUseEvent` wins without duplicate execution.
- Existing tool-name/schema-key reversal, `AskUserQuestion` repair and multiline parameter handling remain available after strict validation.

## Commands and results

All Cargo commands used Rust 1.92.0 through the scoped build runner.

```text
feature/tests/run-cargo-scoped.sh bare-invoke-compile-2 -- cargo +1.92.0 check --bin kiro-rs
result: pass
cleanup: size_kib=447264 removed=true reservation_released=true

feature/tests/run-cargo-scoped.sh bare-invoke-tests-2 -- \
  cargo +1.92.0 test --bin kiro-rs literal_tool_protocol_ -- --nocapture
result: 8 passed, 0 failed, 1530 filtered out
test runtime: 0.02s (compilation excluded)
cleanup: size_kib=1625528 removed=true reservation_released=true

feature/tests/run-cargo-scoped.sh bare-invoke-stream-regression-1 -- \
  cargo +1.92.0 test --bin kiro-rs anthropic::stream::tests -- --nocapture
result: 93 passed, 0 failed, 1445 filtered out
test runtime: 0.03s (compilation excluded)
cleanup: size_kib=1627988 removed=true reservation_released=true
```

An initial `bare-invoke-tests-1` command hit the outer 120-second execution timeout during test-binary linking. The compiler process group was allowed to finish, then the scoped stale reaper removed the owned 1,625,284 KiB target and released its 12 GiB reservation. It was an orchestration timeout, not a test failure; the identical matrix then passed as `bare-invoke-tests-2` with a sufficient outer timeout.

## Matrix detail

| Class | Internal repetition | Result |
| --- | ---: | --- |
| Bare known/unknown/invalid/incomplete invoke, stream and non-stream | 5 rounds per fixture | exact visible text, zero tool use |
| Strict plain and `antml` wrapper, typed inputs, multiple calls | 5 rounds | expected tool blocks only |
| Fence/list/quote/inline/explanation/prompt-injection context | 5 rounds per fixture | exact visible text, zero tool use |
| Unknown/mixed/missing-name/unclosed/duplicate-key/mismatched wrapper | 5 rounds per fixture | atomic exact text, zero partial execution |
| Valid wrapper every-byte partition | all bytes | one tool use |
| Valid wrapper deterministic partitions | 25 seeds | one tool use per seed, no XML text leak |
| Wrapper followed by equivalent structured event | 5 rounds | one tool use total |
| Name/schema reversal, AskUserQuestion and multiline literal closes | focused positive | preserved |
| Bare immediate output and incomplete-wrapper EOF flush | focused latency/identity | no bare hold; incomplete wrapper restored at EOF |

## Real CLI gate prepared, not yet claimed

[bare-invoke-claude-cli.mjs](../tests/bare-invoke-claude-cli.mjs) passes `node --check` and requires:

- `KIRO_RS_BINARY`: absolute repository-external frozen binary;
- `KIRO_VALIDATION_ARTIFACT_DIR`: absolute repository-external owned artifact root;
- `KIRO_BARE_INVOKE_POSTGRES_URL`: caller-owned empty isolated database;
- `KIRO_BARE_INVOKE_REDIS_URL`: isolated Redis endpoint; the runner uses and deletes only a unique key prefix;
- Claude Code CLI available as `claude` or `KIRO_CLAUDE_BINARY`.

It hard-requires five rounds and records binary SHA-256, Claude CLI version, JSONL tool/tool-result counts, final usage, fake-upstream inference hits, output hashes, sentinel state and cleanup. Raw JSONL, service logs, fake keys and connection URLs remain only in an owned temporary directory and are deleted.

No C2 pass is asserted in this evidence until that runner completes against the final frozen candidate and the caller drops its owned PostgreSQL database.

The runner's signal-only fixture was rerun after the final cleanup changes:

```text
node --test feature/tests/bare-invoke-claude-cli-signal.test.mjs
result: 3 passed, 0 failed
SIGHUP/SIGINT/SIGTERM exit: 129/130/143
cleanup: owned child group stopped; fake and RESP ports released; bounded Redis-prefix cleanup acknowledged; TEMP_ROOT removed
residual bare-invoke temp roots/processes/scoped targets/reservations: 0
protected 127.0.0.1:9022 listener: early evidence used an unchanged-PID probe; this is now deprecated for release validation. Current runner must not inspect the existing listener and instead reports `protected9022ProbeSkipped:true` while excluding port 9022 by value.
```

The checked runner SHA-256 is `22a1642790f4059fe7387ccebd8c95c1f5422f3776fcf53ede036b9e046bec7e`; the signal fixture SHA-256 is `4dd259d647c049b7a48a543837c630ad4bc55004657a0d31e2d38837ad5c50ec`. This fixture starts no Rust build, gateway service, PostgreSQL database or real Redis process; it proves cleanup behavior only, not C2 protocol behavior.

## Scope limits

This evidence does not replace the final C1-C4 protocol matrix, 20/100 tool-loop history, MCP/agent/image cases, load/chaos, UI or release gates. It proves the listed response parser contracts on the current working tree only. It does not support a claim that no future upstream syntax or untested prompt shape can ever expose a similar problem.
