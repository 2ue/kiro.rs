# Claude Code CLI `--continue` 长会话回归证据

Date: 2026-07-20

Status: `pass / frozen-binary + real Claude CLI + isolated kiro.rs + fake Kiro upstream`

This evidence closes the real Claude Code CLI session-resume portion of D02 for the
tested fake-upstream contract. It does not claim official Kiro upstream, native
WebSearch/MCP/image/agent, or production-load equivalence.

## Scope

- Claude Code CLI: `2.1.197 (Claude Code)`
- Product binary: frozen external candidate
  `131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631`
- Runtime: isolated `kiro.rs` on a random loopback port; existing `9022` was
  never inspected or touched.
- Upstream: local fake Kiro EventStream/model-discovery server. Inference bodies
  were captured as parsed metadata plus SHA-256, never retained raw.
- Storage: caller-created `kiro_long_session_*` PostgreSQL database and isolated
  Redis key prefix. The runner never creates, drops, flushes, or reuses the caller
  database.
- Session isolation: five independent HOME/config/project roots; each session's
  first turn used a UUID `--session-id`, and every later turn used only
  `--continue` in the same project directory.

## Commands

The release-qualified runs used an external frozen binary and caller-owned storage:

```text
KIRO_RS_BINARY=/absolute/external/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/absolute/external/artifacts \
KIRO_CLAUDE_BINARY=/absolute/path/to/claude \
KIRO_LONG_SESSION_POSTGRES_URL=postgres://<loopback>/kiro_long_session_<owner> \
KIRO_LONG_SESSION_REDIS_URL=redis://<loopback>/<db> \
KIRO_LONG_SESSION_ROUNDS=5 \
KIRO_LONG_SESSION_TOOL_CYCLES=20 \
node feature/tests/claude-cli-long-session-continue.mjs
```

The same command was run with `KIRO_LONG_SESSION_TOOL_CYCLES=100`. A one-round,
two-cycle smoke was run first to validate the harness and cleanup path.

## Results

| Run | CLI turns | Kiro inference hits | Tool turns | Bash / Read | Tool pairs | Leak matches | Unknown upstream requests | Result | Report SHA-256 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |
| 1 x 2 smoke | 4 | 6 | 2 | 1 / 1 | 2 / 2 | 0 | 0 | PASS | raw report deleted |
| 5 x 20 | 110 | 210 | 100 | 50 / 50 | 100 / 100 | 0 | 0 | PASS | `f8a5faa3b254388062663b00208577e6b01845c04794bb462b0a53de4deb06f3` |
| 5 x 100 | 510 | 1010 | 500 | 250 / 250 | 500 / 500 | 0 | 0 | PASS | `cd79f11bd79735d39671c9ef12c78aa9ce4f8b30545dd0da92662922be8829f9` |

For the 5 x 100 run, per-CLI-turn duration was p50 `403.52 ms`, p95
`505.70 ms`, p99 `649.76 ms`, maximum `837.06 ms`. Each session grew from two
history entries to 404; captured Kiro body size grew from `6293` to `76289`
bytes. There was no abrupt failure or unbounded retry as history grew.

## Assertions performed

- The first invocation in every session emitted exactly one session ID; every
  subsequent invocation emitted the same ID while using `--continue` rather than
  reusing `--session-id`.
- Every turn's Kiro body contained the current user marker and, after turn one,
  the previous assistant marker. Tool follow-up bodies contained the executed
  tool result and matching tool-use ID.
- The wire tool catalog was checked for both public tools. When the converter
  emitted request-local Kiro names such as `bashHash<8-hex>`/`readHash<8-hex>`,
  the fake upstream returned that mapped name; the Claude CLI output had exactly
  the public `Bash`/`Read` name and matching tool-use/tool-result IDs.
- Final usage was non-zero for every CLI turn.
- Assistant text, final result text, public tool names, and stderr were scanned
  for `user Continue`, `Tool results provided`, `Tool results:`,
  `<function_results>`, `<function_calls>`, `<invoke name=...>`, known and
  generic `*Hash<8-hex>` tool-name forms. All counts were zero.
- No fake upstream request was unknown; each text turn made one inference and
  each tool turn made exactly two.

## Cleanup and resource checks

Every run reported child groups, service, fake server, Redis namespace, temporary
roots, and owned ports as cleaned. Post-run checks confirmed:

- no `kiro_long_session_*` database remained;
- `SCAN MATCH kiro_rs:validation:long-session-*` returned zero keys;
- both external artifact roots and session temp roots were removed;
- repository root `target/` was absent;
- no listener probe or mutation was made against `9022`.

## Limitations and follow-up

This closes only the real CLI resume/history/tool-pairing gate against the frozen
fake-upstream contract. It does not close native Kiro upstream behavior, real
thinking deltas/usage, native WebSearch, MCP, image/document, agents/subagents,
fault-injected 429/500/partial recovery, two-instance Redis coordination, UI
browser checks, upgrade smoke, or final release inventory. Those remain explicit
open gates in the verification matrix.
