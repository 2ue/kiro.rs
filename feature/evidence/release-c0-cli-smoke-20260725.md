# Release C0 and Claude CLI smoke evidence 2026-07-25

Status: `passed / release-candidate-local-smoke`

Scope:

- Current working tree after local-pool fast-fail, JSON content-type body sniffing, and external-pool dotted pricing fixes.
- Static/release C0 gate.
- Local direct `/cc/v1/messages` smoke.
- Real Claude Code CLI `stream-json` smoke.
- Minimal Bash tool-use smoke.
- Direct `thinking.type=adaptive + output_config.effort=max` smoke.
- Build artifact inventory after validation.

## Candidate binary

Frozen candidate copied out of scoped Cargo target before cleanup:

```text
candidate_root=/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T//kiro-release-candidate.JjCroS
kiro-rs sha256=1290930ed48ee6e20d6ed0ea01095aa08d190164c5252921b7fa1688ca5e569e
kiro_loadtest sha256=4f529fd5484b8f5552c0abb2a47bb5c9197b526c825a43c6d7cc44cd37a7ffd2
```

The frozen binary was started on the existing local validation port `127.0.0.1:9022` after confirming the prior listener was this project’s temporary `kiro-rs -c config.json --credentials credentials.json` process.

```text
listener=127.0.0.1:9022
pid=23501
healthz={"service":"kiro-rs","status":"ok"}
validation_root=tmp/validation/claude-cli-current-20260725-050400
```

## C0 static/release gate

Command class:

```text
RUSTUP_TOOLCHAIN=1.92.0 feature/tests/run-cargo-scoped.sh release-c0-final -- ...
```

The scoped batch ran:

- `cargo fmt --all -- --check`
- `cargo test --locked --all-targets`
- `cargo build --release --bins`
- copied `kiro-rs` and `kiro_loadtest` to `candidate_root`

Result:

```text
main tests: 1781 passed / 0 failed / 6 ignored
kiro_loadtest tests: 31 passed / 0 failed
release build: passed
scoped target cleanup: removed=true / reservation_released=true
```

Post-C0 inventory initially found a transient root `target/` generated after the build. No process referenced it, so it was deleted. Final inventory:

```text
node feature/tests/inventory-build-artifacts.mjs --gate
targets=0
reservations=0
target_processes=0
blockers=0
result=pass
```

## Direct protocol smoke

### C1 normal stream

Request:

```text
POST /cc/v1/messages
model=sonnet
stream=true
prompt="Reply exactly: direct-pong"
```

Observed:

```text
message_start: present
text_delta: produced "direct-pong"
terminal: public api_error after content_block_stop
leak markers: none
```

This run proves stream event shape reached the client and did not expose internal scheduler/credential terms, but it is not counted as a full success because the upstream/local route ended with a public error event and did not emit final `message_delta.usage`.

### C1 normal non-stream

Initial request returned a public local-capacity response:

```text
type=rate_limit_error
message="No account is ready for this request right now. Retry after 6 seconds..."
leak markers: none
```

After waiting for cooldown, two low-frequency retries were run:

```text
retry_1: public api_error, leak markers none
retry_2: success
```

Successful retry:

```json
{
  "model": "sonnet",
  "usage": {
    "input_tokens": 23,
    "cache_creation_input_tokens": 7413,
    "cache_read_input_tokens": 0,
    "output_tokens": 4
  },
  "text_ok": true,
  "leak_markers": []
}
```

## Real Claude Code CLI smoke

CLI version:

```text
2.1.197 (Claude Code)
```

Command class:

```text
HOME=/tmp/kiro-claude-home-9022-current
CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-9022-current
ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc
ANTHROPIC_API_KEY=<redacted>
claude --bare --print --verbose \
  --output-format=stream-json \
  --include-partial-messages \
  --no-session-persistence \
  --model sonnet \
  "Reply exactly: cli-pong"
```

Observed:

```text
stdout bytes=4428
stderr bytes=321
text_ok=true
errors=[]
leak markers=[]
```

Final usage was non-zero:

```json
{
  "input_tokens": 42,
  "cache_creation_input_tokens": 10288,
  "cache_read_input_tokens": 0,
  "output_tokens": 2,
  "server_tool_use": {
    "web_search_requests": 0,
    "web_fetch_requests": 0
  },
  "service_tier": "standard"
}
```

The only stderr note was Claude Code workspace trust warning for local settings; it did not block bare/print mode.

## Claude CLI Bash tool smoke

Command class:

```text
HOME=/tmp/kiro-claude-home-9022-tool
CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-9022-tool
ANTHROPIC_BASE_URL=http://127.0.0.1:9022/cc
ANTHROPIC_API_KEY=<redacted>
claude --bare --print --verbose \
  --output-format=stream-json \
  --include-partial-messages \
  --no-session-persistence \
  --model sonnet \
  --dangerously-skip-permissions \
  --allowedTools=Bash -- \
  "Use Bash to run: printf cli-tool-ok. Then reply exactly: tool-done"
```

Observed:

```text
stdout bytes=10777
stderr bytes=318
tool_result mentions=1
final_has_tool_done=true
errors=[]
leak markers=[]
```

Final usage was non-zero:

```json
{
  "input_tokens": 110,
  "cache_creation_input_tokens": 13045,
  "cache_read_input_tokens": 12211,
  "output_tokens": 23,
  "server_tool_use": {
    "web_search_requests": 0,
    "web_fetch_requests": 0
  },
  "service_tier": "standard"
}
```

Marker scan across direct and CLI outputs checked:

```text
bashHash
readHash
editHash
Tool results provided
function_results
credential
fallback pool
upstream pool
private scheduler
local_scheduler
```

All observed hits were empty.

## Thinking / effort smoke

Direct request:

```json
{
  "model": "sonnet",
  "max_tokens": 64,
  "stream": false,
  "thinking": { "type": "adaptive" },
  "output_config": { "effort": "max" },
  "messages": [
    { "role": "user", "content": "Reply exactly: think-pong" }
  ]
}
```

Observed:

```text
status=success
model=sonnet
content_types=["text"]
text_ok=true
leak markers=[]
```

Usage:

```json
{
  "input_tokens": 13,
  "cache_creation_input_tokens": 7417,
  "cache_read_input_tokens": 0,
  "output_tokens": 3
}
```

This live smoke proves the current proxy accepts `thinking.type=adaptive` together with `output_config.effort=max` and does not downgrade it into a local validation error. Exact upstream Kiro wire preservation is covered by the Rust C0 tests and historical thinking wire evidence; this smoke does not include a packet capture of the upstream request body.

## Limitations

- The direct stream smoke ended with a public upstream/local error after emitting text; it is useful for leak/error normalization but not counted as a successful final-usage stream sample.
- The CLI tool case is a minimal Bash tool scenario, not a long interactive session.
- The thinking smoke confirms request acceptance and successful response, not visible thinking deltas.
- No high-volume real upstream load was run in this pass.
