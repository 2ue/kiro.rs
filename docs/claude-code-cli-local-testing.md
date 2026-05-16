# Claude Code CLI Local Testing

This project is commonly tested through Claude Code CLI against the local
Anthropic-compatible endpoint on `http://127.0.0.1:8080`.

## Safe Max-Permission Sandbox

Use max permission mode only inside a disposable working directory. Do not run
it from the repository root when the goal is tool behavior testing.

```powershell
$env:ANTHROPIC_BASE_URL = 'http://127.0.0.1:8080'
$env:ANTHROPIC_AUTH_TOKEN = 'sk-kiro-rs-local-dev'
$env:CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'

Get-Content .\prompt.txt -Raw | claude -p `
  --verbose `
  --output-format stream-json `
  --include-partial-messages `
  --dangerously-skip-permissions `
  --permission-mode bypassPermissions `
  --model claude-sonnet-4-5-20250929 `
  --session-id aaaaaaaa-bbbb-4ccc-dddd-eeeeeeeeeeee `
  *> .\outputs\run.stream.jsonl
```

On Windows, prefer stdin (`Get-Content -Raw | claude -p ...`) for multi-line
prompts. Passing a long multi-line prompt as a command argument can result in
Claude Code receiving only part of the intended task.

## Streaming Checks

For Claude Code CLI process output, use:

- `--output-format stream-json`
- `--include-partial-messages`
- optionally `--include-hook-events`

Expected event evidence:

- `thinking_delta`: thinking/reasoning stream chunks from Claude Code CLI.
- `text_delta`: visible assistant text stream chunks.
- `input_json_delta`: incremental tool input JSON.
- `tool_use` followed by `tool_result`: local CLI tool execution loop.

The interactive TUI may choose to display mostly tool activity and final text.
Use `stream-json` logs to verify whether the proxy is actually forwarding
incremental thinking/text events.

## Windows Tools

Claude Code's Bash tool may emit Unix-like paths on Windows in some cases. For
Windows-specific command testing, enable the PowerShell tool:

```powershell
$env:CLAUDE_CODE_USE_POWERSHELL_TOOL = '1'
```

Then allow it explicitly:

```powershell
Get-Content .\prompt.txt -Raw | claude -p `
  --tools PowerShell,Read `
  --allowedTools PowerShell,Read `
  --dangerously-skip-permissions `
  --permission-mode bypassPermissions
```

## Task Tools

New task-management tools are gated separately:

```powershell
$env:CLAUDE_CODE_ENABLE_TASKS = '1'
```

Do not use `--bare` when validating `TaskCreate` / `TaskUpdate`; in local
testing, `--bare` reduced the available tool set and did not expose these tools.

## Cache Interpretation

For usage records:

- `Read % = cacheReadInputTokens / totalInputTokens`
- `Cached % = (cacheReadInputTokens + cacheCreationInputTokens) / totalInputTokens`

The first request in a stable session normally creates cache but cannot read it.
Later requests read the old prefix and create the newly extended tail. Seeing
both read and creation in the same request is expected for growing Claude Code
sessions.

When `promptCacheSimulationMode` is `local-prompt-cache`,
`promptCacheTargetReadRatio` is treated as a center target rather than an exact
percentage. The effective simulated cache ratio is deterministic for the
cacheable request prefix and floats within about +/- 3 percentage points. For
example, a target of `0.95` should usually produce roughly `92%` to `98%`
cached tokens instead of every record landing exactly on `95%`.
