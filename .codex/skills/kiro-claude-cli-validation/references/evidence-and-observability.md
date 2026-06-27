# Evidence And Observability

## Minimum Evidence Per Run

Record:

- Git commit or working-tree diff summary.
- Binary path and build mode.
- Service port and PID.
- Claude CLI version.
- Base URL with secrets redacted.
- Test case id.
- Request id and error id when present.
- HTTP status and route.
- Requested model and resolved upstream model when available.
- First byte time for direct HTTP/SSE tests.
- `message_start.usage` and final `message_delta.usage`.
- Thinking block count and thinking delta count.
- Tool use count and tool result count.
- Logs or usage records proving internal errors were retained without leaking to downstream.

## Secret Handling

Do not copy raw values for:

- `ANTHROPIC_API_KEY`
- Kiro refresh tokens
- OAuth client secrets
- full credential imports
- proxy passwords

Use `<redacted>` in notes and summaries.

## What Counts As A Real Claude CLI Test

The test must execute the installed `claude` binary and send traffic through kiro.rs. Direct curl tests are useful but do not count as real Claude CLI validation.

For non-interactive runs, prefer `stream-json` output so event structure can be inspected.

For interactive runs, capture the terminal output and pair it with server-side request ids.

## What Does Not Count

- Unit tests alone.
- Mocked upstream alone.
- A prompt containing `think` without captured thinking deltas.
- A successful direct `/cc/v1/messages` curl without running `claude`.
- A single happy-path prompt used as proof for tools, MCP, agents, or long sessions.

