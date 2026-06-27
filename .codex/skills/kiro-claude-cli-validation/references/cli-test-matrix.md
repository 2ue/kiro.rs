# Claude CLI Test Matrix

## C1 Direct Protocol

Use direct HTTP/SSE to prove the proxy protocol before involving Claude CLI.

| Case | Request | Required evidence |
| --- | --- | --- |
| C1.1 normal stream | `/cc/v1/messages`, `stream:true`, short prompt | 200, `message_start`, text delta, final `message_delta.usage`, request id |
| C1.2 normal non-stream | `/cc/v1/messages`, `stream:false` | 200, content text, final usage |
| C1.3 thinking stream | thinking-capable model or request | `thinking` block, `thinking_delta`, final `thinking_tokens` |
| C1.4 thinking non-stream | `stream:false` thinking request | content has thinking/text blocks, usage details |
| C1.5 alias model | requested alias such as `sonnet`, `sonnet-thinking` | requested and resolved model captured; thinking alias keeps thinking behavior |
| C1.6 invalid request | malformed messages or impossible model | normalized public error, internal details only in local logs |
| C1.7 tool payload | minimal Anthropic tool schema | tool_use event shape is valid; no invalid tool format |

## C2 Claude CLI Non-Interactive

Use isolated config and `--output-format=stream-json` when possible.

Common flags:

```bash
HOME=/tmp/kiro-claude-home-19022 \
CLAUDE_CONFIG_DIR=/tmp/kiro-claude-config-19022 \
ANTHROPIC_BASE_URL=http://127.0.0.1:19022/cc \
ANTHROPIC_API_KEY=<redacted> \
claude --bare --print --verbose \
  --output-format=stream-json \
  --include-partial-messages \
  --no-session-persistence \
  --model sonnet \
  'Reply with exactly: pong'
```

| Case | Prompt pattern | Required evidence |
| --- | --- | --- |
| C2.1 normal | exact short response | result text, assistant usage non-zero, final usage non-zero |
| C2.2 alias | run `sonnet`, full model id, and thinking suffix | model mapping logs; behavior matches requested capability |
| C2.3 thinking | ask for brief thinking and final exact answer | real thinking delta and `thinking_tokens` |
| C2.4 tool | ask Bash to run `printf tool-ok`, then reply | tool use count, tool result count, final text |
| C2.5 error | invalid input or intentionally unavailable model | public normalized error with request/error id |
| C2.6 long output | ask for moderate structured output | no stalled stream, final usage reasonable |

## C3 Interactive And Long Session

Use a throwaway project directory and capture terminal output with `script` or equivalent.

Scenarios:

- Multi-turn: ask a question, follow up with a correction, then ask it to summarize prior turns.
- Long context: add a generated local file, ask it to inspect and edit or summarize it across several turns.
- Tool loop: ask it to run shell commands, inspect output, and make a second decision.
- Agent behavior: ask Claude Code to run multiple subagents or parallel analysis if the installed CLI supports that feature; capture agent usage/tokens live.
- MCP behavior: configure a trivial local MCP server in the isolated Claude config, then force a call to that MCP tool.
- Thinking: in interactive mode, request a thinking-capable model and verify actual stream evidence, not just visible prose.

The test passes only if the session remains coherent, tool results round-trip, usage updates are not stuck at zero, and no internal system terms leak to the user.

## C4 Compatibility Regression

Run these after changes to converter, payload guard, stream state, or thinking logic:

- assistant thinking followed by tool_use and then tool_result.
- prior signed thinking or redacted thinking blocks if available from upstream.
- historical tool_result with large output.
- MCP tool schema containing nullable or nested `required` / `properties` edge cases.
- payload near `payloadGuardMaxBytes`.
- route-specific usage policy and high-cache/dfcache usage fields.

