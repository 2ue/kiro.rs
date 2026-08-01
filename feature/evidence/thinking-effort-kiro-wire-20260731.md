# Thinking/Effort Kiro Wire - 2026-07-31

Status: `current-frozen-candidate-pass / broader-protocol-release-gates-open / NO-GO`

## Scope

This record covers the real Claude Code CLI thinking/effort wire gate for the
current frozen `kiro-rs` candidate. It does not certify real Kiro upstream
success, native WebSearch/image/agents, signed-thinking response semantics,
cross-instance scheduler behavior, or the final release.

## Environment and safety

- Candidate binary: external frozen copy
  `/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T/kiro-release-candidate-final.bvSRfz/kiro-rs`
- Candidate SHA-256:
  `00b318aa66fa139e876acd88f7472388c7da4358aa2fef21e925c5f240cb27d7`
- Claude Code CLI: `2.1.220`
- Claude executable used:
  `/Users/yuanfeijie/.volta/tools/image/packages/@anthropic-ai/claude-code/bin/claude`
- Service and fake Kiro upstream used caller-owned loopback PostgreSQL/Redis
  resources and temporary ports.
- Existing `127.0.0.1:9022` was not restarted or modified. It remained PID
  `13048` throughout this validation.
- Raw artifact root was temporary and removed after the redacted result and
  SHA-256 were recorded.

## Matrix and result

Command:

```bash
KIRO_CLAUDE_BINARY="$(volta which claude)" \
KIRO_CLI_SUITE_ONLY=thinking \
KIRO_EXPECTED_CLAUDE_VERSION=2.1.220 \
KIRO_RS_BINARY=<frozen-candidate> \
KIRO_VALIDATION_PROGRESS=1 \
feature/tests/run-claude-cli-release-suite-local.sh
```

Matrix:

- endpoints: `cli`, `ide`
- effort: absent, `low`, `medium`, `high`, `xhigh`, `max`
- rounds: `5` per endpoint/effort cell
- total cases: `60`

Result: `60/60` passed, exit code `0`, with no violations, no unknown fake
requests, and complete child/process/temporary-resource cleanup.

The final wire preserved the model-advertised effort path. Explicit `max` was
not silently reduced to `high`; the runner also verified the advertised schema
and request/response accounting for both endpoints.

## Initial failure and root cause

The first attempt used `/Users/yuanfeijie/.volta/bin/claude`. The runner
canonicalized that symlink to the `volta-shim` executable, and Volta rejects
direct invocation of `volta-shim`. The resulting `volta-shim failed or timed
out` was a validation-runner environment failure, not a Kiro protocol
assertion. Re-running with `volta which claude` returned the real Claude
binary and passed the complete matrix.

## Release boundary

This closes the current frozen-candidate thinking/effort wire sub-gate only.
The protocol-capability matrix remains release-blocking because native
WebSearch/image/agents, real upstream, signed/redacted response paths, long
mixed histories, browser evidence, and the scheduler/storage P0 gates are not
all closed. The release decision therefore remains `NO-GO`.
