---
name: kiro-claude-cli-validation
description: Validate kiro.rs changes against real Claude Code CLI behavior. Use when changes may affect /cc/v1 protocol compatibility, streaming event order, usage token reporting, thinking output, model aliasing, tools, agents, MCP, prompt cache reporting, error normalization, or interactive Claude Code workflows.
---

# Kiro Claude CLI Validation

Use this skill to validate kiro.rs with the real Claude Code CLI, not only unit tests or direct curl calls.

## Safety Contract

- Do not touch a live `9022` process unless the user explicitly asks for that exact port.
- Prefer a temporary release service with `KIRO_RS_HOST=127.0.0.1` and `KIRO_RS_PORT=<temp-port>`.
- Verify the temp port before and after the run with `lsof -nP -iTCP:<port> -sTCP:LISTEN`.
- Use isolated Claude config directories for tests:
  - `HOME=/tmp/kiro-claude-home-<port>`
  - `CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-<port>`
- Never print API keys, refresh tokens, client secrets, or full credential JSON.
- Keep real upstream calls modest unless the user explicitly asks for high-volume real traffic.
- Stop every temp service started by the validation.

## Tier Selection

Run the lowest tier that can prove the changed behavior. For release-critical protocol or scheduler changes, run all applicable tiers.

- **C0 static gate**: formatting, diff hygiene, unit tests, release build. No real network required.
- **C1 direct protocol gate**: direct `/cc/v1/messages` HTTP/SSE checks for event shape, usage fields, thinking blocks, errors, and aliases.
- **C2 Claude CLI non-interactive gate**: real `claude --print` or `--output-format=stream-json` checks for normal, thinking, alias, tool, and error paths.
- **C3 Claude CLI interactive gate**: real interactive session checks for multi-turn behavior, long context, visible thinking, tools, agents, and MCP.
- **C4 compatibility regression gate**: verify tool-use pairing, signed/redacted thinking preservation where possible, payload shaping, and long-history round trips.

Read `references/cli-test-matrix.md` for case details and `references/evidence-and-observability.md` for evidence requirements.

## Required Baseline Commands

Before any live CLI validation:

```bash
cargo fmt --check
git diff --check
cargo test
cargo build --release
```

Record the CLI version:

```bash
claude --version
```

Start a temp service only after the release build succeeds:

```bash
KIRO_RS_HOST=127.0.0.1 KIRO_RS_PORT=19022 \
  ./target/release/kiro-rs -c config.json --credentials credentials.json
```

If runtime config is loaded from PgSQL, confirm the process actually listens on the requested temp port.

## How To Prove Thinking Is Real

Do not count a prompt containing the word `think` as proof.

A thinking test only passes when at least one of these is captured:

- Claude CLI `stream-json` contains a thinking content block or `thinking_delta`.
- Direct SSE contains `content_block_start` with `type: "thinking"` and subsequent `thinking_delta`.
- Final usage contains `output_tokens_details.thinking_tokens > 0`, and the stream also showed thinking output.

Visible text in the interactive terminal is supporting evidence, not the only proof.

## How To Prove Token Reporting Works

For Claude Code live views, `message_start.usage` may be an estimate. The final `message_delta.usage` or CLI result usage remains authoritative.

A token reporting test passes only when:

- Assistant messages are not stuck at all-zero usage during long tool/agent runs.
- Final usage has reasonable non-zero input tokens.
- Output tokens are non-zero when text or tools were generated.
- Thinking tests include `output_tokens_details.thinking_tokens` when thinking was emitted.
- Cache fields are present and do not contradict the route's reporting policy.

## How To Prove Model Alias Behavior

Capture the requested model, downstream route, request id, and resolved upstream model from logs or usage detail.

For thinking models, verify the thinking-capable model is not silently reduced to a normal non-thinking alias. A `sonnet-thinking` style request must either produce real thinking output or fail with a clear normalized error; it must not silently behave as ordinary `sonnet`.

## Pass/Fail Rule

Fail the validation if any of these occur:

- CLI receives internal terms such as credential, fallback pool, upstream pool, or private scheduler details.
- Thinking is claimed but no thinking block/delta is captured.
- Tool-use or MCP scenarios return malformed tool errors.
- Multi-turn tool history loses required tool_use/tool_result pairing.
- Long sessions degrade into unexplained 400/500 responses.
- CLI live usage remains `0 tokens` for active assistant/tool/agent work.
- A temp process remains running after validation.

