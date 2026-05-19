---
name: claude-code-cli-service-test
description: Use when testing a local or configured Claude Code CLI-compatible service with the real claude CLI, including long/resumed sessions, tool calls, streaming JSON output, account scheduling/failover, sticky-session migration, usage records, prompt-cache/high-cache continuity, and admin observability. Not for mock HTTP-only tests and not specific to 429.
---

# Claude Code CLI Service Test

Use this skill when the task is to validate that a Claude Code CLI-compatible service works under real CLI traffic. The goal is service availability and correct scheduling behavior, not only reproducing one status code.

## Core Rules

1. Use the real `claude` CLI. Do not replace it with mock HTTP unless the user explicitly asks for protocol-only tests.
2. Capture evidence: CLI `stream-json` files, backend logs, Admin usage records, and credential status.
3. Treat sticky as a healthy-account preference. If the bound account is cooling down, disabled, model-incompatible, auth-broken, quota-exhausted, or request-excluded, it must be skipped or unbound.
4. If any account is healthy and schedulable, the service should continue by failing over to that account.
5. Global error waves are observations and manual-recovery suppressors. They must not block healthy account scheduling.
6. Preserve high-cache continuity across `--resume` and account failover. A credential switch must not erase the Claude Code session or local prompt-cache evidence.

## Baseline Checks

Run these first:

```bash
ccman cc current
claude --help | sed -n '1,120p'
curl -sS http://127.0.0.1:9022/api/admin/config/load-balancing -H 'x-api-key: sk-admin-local-debug'
curl -sS http://127.0.0.1:9022/api/admin/credentials -H 'x-api-key: sk-admin-local-debug'
```

Adjust port and admin key from `config.json` when needed.

## Clean Test State

Only clear state when the user requests clean samples. For incident analysis, preserve Redis scheduler keys unless isolation is required.

```bash
mkdir -p .local-run/claude-tests
: > .local-run/backend-9022.log
rm -f .local-run/claude-tests/*.jsonl
curl -sS -X POST http://127.0.0.1:9022/api/admin/usage-records/clear -H 'x-api-key: sk-admin-local-debug'
```

Do not clear `kiro:sched:v1:*` by default. Those keys may contain the cooldown and sticky evidence needed to diagnose failover.

## CLI Patterns

New session:

```bash
SID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
claude --print --verbose --output-format stream-json \
  --session-id "$SID" \
  --model sonnet \
  'Short prompt here' \
  > ".local-run/claude-tests/new-$SID.jsonl" 2>&1
```

Resume the same session:

```bash
claude --print --verbose --output-format stream-json \
  --resume "$SID" \
  --model sonnet \
  'Continue the previous task' \
  > ".local-run/claude-tests/resume-$SID.jsonl" 2>&1
```

Do not reuse `--session-id` for an existing session. Use `--resume`; otherwise Claude Code can report that the session ID is already in use.

Tool-use test in an isolated workspace:

```bash
claude --print --verbose --output-format stream-json \
  --session-id "$SID" \
  --model sonnet \
  --permission-mode bypassPermissions \
  --tools 'Read,Bash,Write,Edit' \
  'Read README.md, run pwd, and write one summary line to .local-run/claude-tests/tool-smoke.txt.' \
  > ".local-run/claude-tests/tools-$SID.jsonl" 2>&1
```

Use narrower `--allowedTools` when the workspace is not isolated.

## Required Scenarios

Run a representative mix rather than a single prompt:

1. Fresh session: one successful non-tool request.
2. Resume: at least two follow-up turns using `--resume`.
3. Tool use: `Read`, `Bash`, and `Write` or `Edit` through real Claude Code tool calls.
4. Long context: ask the CLI to read project docs and continue the same session to observe cache reads.
5. Multi-session concurrency shape: run several independent session IDs and compare credential distribution.
6. Account failover: let unhealthy accounts cool down and confirm later requests move to healthy accounts.
7. Sticky migration: resume a session whose original credential is now unschedulable and confirm fallback.
8. Error diversity: inspect 429, 401/403, 402 quota, 408/5xx, network/proxy errors when present. Do not hard-code the test as 429-only.

## Evidence To Inspect

Fetch records:

```bash
curl -sS 'http://127.0.0.1:9022/api/admin/usage-records?limit=1000' \
  -H 'x-api-key: sk-admin-local-debug' \
  > .local-run/claude-tests/usage-records.json
curl -sS http://127.0.0.1:9022/api/admin/usage-summary \
  -H 'x-api-key: sk-admin-local-debug' \
  > .local-run/claude-tests/usage-summary.json
```

Check these fields:

```text
status
conversationId
credentialId
attemptedCredentialIds
rateLimitedCredentialIds
lastAttemptedCredentialId
schedulerBlocked
stickyBound
fallbackFromSticky
usageSource
cacheReadInputTokens
cacheCreationInputTokens
errorType
errorMessage
```

Interpretation:

1. `attemptedCredentialIds` should show the actual failover path.
2. `rateLimitedCredentialIds` should identify accounts cooled down by 429 or rate-limit-like responses.
3. `schedulerBlocked=true` is acceptable only when no credential is schedulable or every usable candidate was excluded by this request.
4. `fallbackFromSticky=true` indicates the service escaped a previous session binding.
5. `usageSource=local_prompt_cache` plus cache read fields indicates high-cache continuity.

## Pass Criteria

The service passes the practical availability bar when:

1. A healthy account can still serve requests after earlier accounts fail.
2. The same failed account is not hit repeatedly across later requests while it is cooling down.
3. Sticky sessions move away from unschedulable credentials.
4. Tool calls complete through the real CLI path.
5. Resume keeps context and cache evidence.
6. Usage records explain the credential path without relying only on backend logs.
7. Global wave signals do not produce service-unavailable errors while healthy accounts remain.

## Failure Analysis

Use this triage order:

1. CLI misuse: repeated `--session-id`, missing `--verbose` with `stream-json`, or missing tool permissions.
2. Provider routing: `ccman cc current` points to the wrong service URL.
3. Scheduler blocking: `schedulerBlocked=true` while credentials show healthy candidates.
4. Sticky bug: resumed sessions keep using a cooled-down credential.
5. Global wave bug: a pool-level key/event blocks all scheduling despite healthy accounts.
6. Cache regression: `--resume` works but `cacheReadInputTokens` and local prompt-cache summary stop increasing.
7. Trace gap: failure records lack `lastAttemptedCredentialId` or attempted credential arrays.

When reporting results, include the exact CLI commands, session IDs, output file paths, backend log window, and the last 20 relevant usage records.
