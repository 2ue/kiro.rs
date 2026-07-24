---
name: kiro-claude-cli-validation
description: Validate kiro.rs changes against real Claude Code CLI behavior. Use when changes may affect /cc/v1 protocol compatibility, streaming event order, usage token reporting, thinking output, model aliasing, tools, agents, MCP, prompt cache reporting, error normalization, or interactive Claude Code workflows.
---

# Kiro Claude CLI Validation

Use this skill to validate kiro.rs with the real Claude Code CLI, not only unit tests or direct curl calls.

## Safety Contract

- For normal local validation, default to the existing local environment and local service port (commonly `9022`); restart that service directly when a restart is needed.
- Use an isolated temporary service only when isolation is required: initialization/deployment validation, no usable local service/config, or a concrete risk of corrupting unrelated state.
- Before touching an existing local service, identify the exact listener PID/command and confirm it is this project’s `kiro-rs`; stop only that process.
- Verify the selected port before and after the run with `lsof -nP -iTCP:<port> -sTCP:LISTEN`.
- Use isolated Claude config directories for tests:
  - `HOME=/tmp/kiro-claude-home-<port>`
  - `CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-<port>`
- Never print API keys, refresh tokens, client secrets, or full credential JSON.
- Keep real upstream calls modest unless the user explicitly asks for high-volume real traffic.
- Stop every temp service started by the validation.

## Build Artifact Contract

- Every local Cargo command, including `fmt`, `check`, `test`, and `build`, must run inside `feature/tests/run-cargo-scoped.sh <scope> -- <command...>`. The only exemption is CI that explicitly documents that its entire filesystem is ephemeral and discarded after the job.
- A branch owns one scoped target for one logical build batch. That target must be deleted by the wrapper after success, failure, timeout, or signal; cleanup happens after each branch batch, not after all branches finish.
- If later runtime tests need a binary, copy only the finished binary out of `$CARGO_TARGET_DIR` before the wrapper exits, record its SHA-256, and pass its absolute immutable path as `KIRO_RS_BINARY`. A runner must not discover `target/debug`, `target/release`, or silently invoke Cargo.
- Raw CLI captures belong in an owned temporary directory that is deleted after a redacted summary and hashes are recorded. Do not retain `deps`, `build`, or `incremental` as evidence.
- Before a release gate, run `node feature/tests/inventory-build-artifacts.mjs --gate`. Any unknown/unmanaged Cargo target, stale or active reservation, incomplete inventory, or process referencing a target blocks release. The inventory is report-only and never authorizes broad deletion.

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
git diff --check
candidate_root="$(mktemp -d "${TMPDIR:-/tmp}/kiro-cli-candidate.XXXXXX")"
KIRO_FROZEN_BINARY="$candidate_root/kiro-rs" \
  feature/tests/run-cargo-scoped.sh claude-cli-c0 -- \
  bash -lc 'cargo fmt --check && cargo test && cargo build --release && install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"'
shasum -a 256 "$candidate_root/kiro-rs"
export KIRO_RS_BINARY="$candidate_root/kiro-rs"
```

The caller owns `candidate_root` and removes it after the CLI matrix. The scoped Cargo target has already been removed before the first service starts.

Record the CLI version:

```bash
claude --version
```

Start a temp service only after the release build succeeds:

```bash
KIRO_RS_HOST=127.0.0.1 KIRO_RS_PORT=19022 \
  "$KIRO_RS_BINARY" -c config.json --credentials credentials.json
```

If the user asked to restart the existing local service, start the release binary with the real local `config.json` / `credentials.json` and the existing configured port instead of the temp command above. If runtime config is loaded from PgSQL, confirm the restarted process actually listens on the intended local port.

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
