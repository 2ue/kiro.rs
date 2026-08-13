# Frozen Claude CLI thinking and bare-invoke gate

Date: 2026-07-19

Status: `r8 fake-upstream frozen runtime pass / real upstream and long-session still pending`

## Scope

This evidence covers two real Claude Code CLI gates against a repository-external frozen `kiro-rs` binary:

1. `bare-invoke-claude-cli.mjs`: literal protocol text must not become executable Claude Code tools, while real structured `toolUseEvent` still round-trips.
2. `thinking-effort-kiro-wire.mjs`: Claude CLI/IDE ingress `thinking.type=adaptive` and `output_config.effort` must reach the Kiro wire according to the advertised upstream schema, without `max -> high` clamping and without invented unsupported fields.

It does not close real Kiro upstream, native thinking delta/usage, long interactive sessions, MCP/search/image combinations, L1-L5 load, two-instance scheduler chaos, UI browser gates, upgrade smoke, or final release inventory.

## Frozen binaries

The first frozen C0 candidate existed at:

```text
/tmp/kiro-frozen-20260719/kiro-rs
sha256=70c9741b897ea9fe0343d4a279f6555e35f0bc23bccac58cec493a74bad76a80
```

It reproduced a real Claude CLI compatibility failure described below.

After the fix, a new repository-external runtime candidate was built with Rust 1.92.0:

```text
/tmp/kiro-frozen-20260719-r2/kiro-rs
sha256=e16df13a0ded4d53ac255f26ddc24056c4d385dde418a63944a2e00d122c642a
```

Build command:

```bash
env RUSTUP_TOOLCHAIN=1.92.0 \
  KIRO_FROZEN_BINARY=/tmp/kiro-frozen-20260719-r2/kiro-rs \
  feature/tests/run-cargo-scoped.sh frozen-runtime-20260719-r2 -- \
  bash -lc 'cargo build --release --bin kiro-rs && install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"'
```

Cleanup evidence:

```text
validation-build-cleanup scope=frozen-runtime-20260719-r2 size_kib=787044 available_kib=84600096 removed=true reservation_released=true
```

The later r8 scheduler/runtime frozen candidate used for the current rerun is:

```text
/tmp/kiro-frozen-20260719-r8/kiro-rs
sha256=131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631
```

## Red: Claude CLI default adaptive effort rejected on non-native model catalog

The real CLI runner first failed against the pre-fix frozen binary.

Environment facts:

- Claude Code CLI: `2.1.197`
- Runner: `feature/tests/bare-invoke-claude-cli.mjs`
- Model requested by runner: `sonnet`
- Fake Kiro `ListAvailableModels`: advertised only `claude-sonnet-4` with no `additionalModelRequestFieldsSchema`
- Protected port handling: runner rejects `9022` by numeric value; one manual read-only `lsof` probe made during this session is excluded from evidence and not repeated

Observed CLI JSONL failure:

```text
API Error: 400 model claude-sonnet-4 does not advertise a native reasoning effort field
```

Root cause:

- Claude Code CLI 2.1.197 sends `thinking.type=adaptive` and a default `output_config.effort=high` even for ordinary `--model sonnet` requests.
- The runtime model catalog can legitimately resolve `sonnet` to an older advertised model such as `claude-sonnet-4` when that is the only model returned by Kiro model discovery.
- The converter treated explicit `output_config.effort` as requiring a verified native Kiro reasoning schema and returned 400 before the existing compatibility prompt fallback could preserve the effort.
- This made ordinary Claude CLI traffic fail whenever the active Kiro catalog did not advertise native reasoning fields for the selected model.

This red result is materially different from the original `max -> high` suspicion: it was not a clamp; it was an early hard failure caused by coupling CLI default adaptive effort to native schema availability.

## Fix

Product change:

- `src/anthropic/converter/model.rs`
  - `build_additional_model_request_fields()` now returns `Ok(None)` when native reasoning capability is `LegacyFallback` without a model match, `Unknown`, `AuthoritativeAbsent`, or `AuthoritativeInvalid`.
  - It no longer rejects solely because `output_config.effort` is explicit and no safe native schema exists.
  - Supported native schemas still preserve/validate the explicit effort. For example, `claude-sonnet-4.6` still rejects explicit `xhigh` instead of silently remapping.

The fallback remains bounded:

- If compatible thinking prompt controls are enabled, the converter injects a synthetic history control preserving the effort, e.g. `<thinking_effort>high</thinking_effort>`.
- If both native reasoning fields and compatible thinking prompt controls are disabled, conversion still fails with an explicit error. The structured capability should be split from the broad “prompt steering” master in a later config/UI cleanup; this evidence does not claim that coupling is fully resolved.

Test observability change:

- `feature/tests/bare-invoke-claude-cli.mjs`
  - `commandOutput()` now includes `spawnSync.error` in failure messages, which exposed the earlier `spawnSync claude ENOENT` PATH issue.
  - CLI failure assertions now print redacted stdout/stderr head+tail diagnostics, which exposed the 400 reasoning-effort failure.

## Focused Rust validation

Command:

```bash
env RUSTUP_TOOLCHAIN=1.92.0 \
  feature/tests/run-cargo-scoped.sh reasoning-fallback-20260719-r3 -- \
  bash -lc '
    set -euo pipefail
    cargo test explicit_reasoning_effort_falls_back_to_compat_prompt_without_native_schema_five_rounds -- --nocapture
    cargo test test_sonnet_4_6_explicit_unsupported_xhigh_is_rejected -- --nocapture
  '
```

Result:

```text
explicit_reasoning_effort_falls_back_to_compat_prompt_without_native_schema_five_rounds ... ok
test_sonnet_4_6_explicit_unsupported_xhigh_is_rejected ... ok
validation-build-cleanup scope=reasoning-fallback-20260719-r3 size_kib=1693704 available_kib=84462364 removed=true reservation_released=true
```

One previous run without `RUSTUP_TOOLCHAIN=1.92.0` failed at compile time on existing let-chain syntax. That was an environment/toolchain issue, not a product assertion failure, and its scoped target was cleaned:

```text
validation-build-cleanup scope=reasoning-fallback-20260719 size_kib=1123124 available_kib=84425288 removed=true reservation_released=true
```

## Real Claude CLI bare-invoke gate

Runner command shape:

```bash
KIRO_RS_BINARY=/tmp/kiro-frozen-20260719-r2/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/private/tmp/kiro-validation-artifacts-bare_<run> \
KIRO_BARE_INVOKE_POSTGRES_URL=<caller-created-empty-db> \
KIRO_BARE_INVOKE_REDIS_URL=redis://127.0.0.1:50892 \
KIRO_CLAUDE_BINARY=/Users/yuanfeijie/.volta/bin/claude \
node feature/tests/bare-invoke-claude-cli.mjs
```

Current project isolated PostgreSQL/Redis were reused:

- PostgreSQL: current-project container `kiro-final-20260718-pg`, loopback `127.0.0.1:50891`
- Redis: current-project container `kiro-final-20260718-redis`, loopback `127.0.0.1:50892`
- One caller-owned empty database was created and dropped with `DROP DATABASE ... WITH (FORCE)`.
- Raw artifact directory was removed after hashing the report.

Report:

```text
report_sha256=67c9d7c9ee45d6b6c66c705a11f02185b9c2304f3d820e499fb297467dd0dec4
binary_sha256=e16df13a0ded4d53ac255f26ddc24056c4d385dde418a63944a2e00d122c642a
```

Result summary:

```json
{
  "result": "pass",
  "totals": {
    "cases": 20,
    "negativeCases": 15,
    "structuredCases": 5,
    "inferenceHits": 25,
    "toolUseCount": 5,
    "toolResultCount": 5,
    "fakeModelDiscoveryRequests": 1,
    "fakeUnknownRequests": 0
  },
  "cleanup": {
    "childGroupsStopped": true,
    "serviceStopped": true,
    "fakeStopped": true,
    "tempRemoved": true,
    "portsReleased": true,
    "protected9022ProbeSkipped": true,
    "redisKeysRemoved": true
  }
}
```

### r8 rerun

The same gate was rerun against r8 with a caller-owned PostgreSQL database and isolated Redis prefix. The first attempt failed before Kiro startup because `claude` was not resolvable by `spawnSync` in this shell environment; it is excluded as environment setup evidence. The valid rerun used the canonical Claude package executable.

Result summary:

```json
{
  "result": "pass",
  "binarySha256": "131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631",
  "claudeCliVersion": "2.1.197 (Claude Code)",
  "rounds": 5,
  "totals": {
    "cases": 20,
    "negativeCases": 15,
    "structuredCases": 5,
    "inferenceHits": 25,
    "toolUseCount": 5,
    "toolResultCount": 5,
    "fakeModelDiscoveryRequests": 1,
    "fakeUnknownRequests": 0
  },
  "cleanup": {
    "childGroupsStopped": true,
    "serviceStopped": true,
    "fakeStopped": true,
    "tempRemoved": true,
    "portsReleased": true,
    "protected9022ProbeSkipped": true,
    "redisKeysRemoved": true
  }
}
```

The caller-owned database and artifact root were deleted by the harness after extracting this redacted summary.

Interpretation:

- 15 literal XML/function-call examples stayed text and did not create the owned Bash sentinel.
- 5 structured `toolUseEvent` cases round-tripped as exactly one Claude Code Bash tool use and one tool result.
- The pre-fix 400 reasoning-effort failure is gone for the same fake model catalog.

## Real Claude CLI thinking/effort frozen wire gate

The runner needs a canonical `psql` executable. The host PATH has no native `psql`, so this run used a temporary canonical wrapper under `/private/tmp` that invoked psql inside the current-project PostgreSQL container. The wrapper, two caller-owned databases, and artifact directory were deleted by the harness.

Important CLI path note:

- `/Users/yuanfeijie/.volta/bin/claude` is a Volta shim symlink.
- `thinking-effort-kiro-wire.mjs` canonicalizes executable paths before running them.
- Passing the shim made the runner execute the real shim path as `volta-shim`, which failed.
- The successful run therefore used the canonical package executable:

```text
/Users/yuanfeijie/.volta/tools/image/packages/@anthropic-ai/claude-code/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe
```

Runner matrix:

```text
2 endpoints (cli, ide) × 6 effort states (absent, low, medium, high, xhigh, max) × 5 rounds = 60 cases
```

Report:

```text
report_sha256=439e1e69ec8407db9334a132dbd75aca0f4aa7c714441263529d615dcdf7336f
binary_sha256=e16df13a0ded4d53ac255f26ddc24056c4d385dde418a63944a2e00d122c642a
```

Result summary:

```json
{
  "result": "pass",
  "totalCases": 60,
  "totals": {
    "inferenceHits": 60,
    "modelDiscoveryHits": 2,
    "modelDiscoverySchemaHits": 2,
    "discoveryCounts": {
      "cli": 1,
      "ide": 1
    },
    "balanceHits": 0,
    "unknownRequests": 0,
    "invalidWireJson": 0,
    "protocolViolations": 0,
    "violations": 0
  },
  "cleanup": {
    "childGroupsStopped": true,
    "serversStopped": true,
    "redisKeysRemoved": true,
    "tempRemoved": true,
    "portsReleased": true,
    "forbiddenPortsNeverAllocated": true
  }
}
```

### r8 rerun

The same 60-case Kiro wire gate was rerun against r8. Because the host still has no native `psql`, the harness used a temporary canonical wrapper under `/tmp` to invoke psql inside the current-project PostgreSQL container. The wrapper, two caller-owned databases and artifact root were deleted after summary extraction. The successful Claude executable path was the canonical package binary:

```text
/Users/yuanfeijie/.volta/tools/image/packages/@anthropic-ai/claude-code/bin/claude
```

Result summary:

```json
{
  "result": "pass",
  "binarySha256": "131696bd81e1cdaeceaac6a45f9c76bf698eb559785b379a82fd77e2f742e631",
  "claudeCliVersion": "2.1.197",
  "totalCases": 60,
  "rounds": 5,
  "endpoints": ["cli", "ide"],
  "efforts": ["absent", "low", "medium", "high", "xhigh", "max"],
  "cleanup": {
    "childGroupsStopped": true,
    "serversStopped": true,
    "redisKeysRemoved": true,
    "tempRemoved": true,
    "portsReleased": true,
    "forbiddenPortsNeverAllocated": true
  }
}
```

This rerun preserves the same protocol conclusion on r8: `max` is not clamped to `high`, and the proxy does not invent an upstream `thinking` field when the Kiro discovery schema only advertises `output_config.effort`.

Interpretation:

- The frozen runtime preserved the requested effort values through the final fake Kiro wire.
- `max` was not clamped to `high`.
- The runner observed the advertised model-discovery schema on both CLI and IDE endpoints.
- Unknown requests and invalid wire JSON were zero.
- This is fake-upstream wire compatibility evidence. It does not prove real Kiro upstream currently accepts every value in production.

## Runner signal validation

Command:

```bash
node --test feature/tests/bare-invoke-claude-cli-signal.test.mjs
```

Result:

```text
2026-07-19 current rerun after cleanup hardening:
node --test feature/tests/bare-invoke-claude-cli-signal.test.mjs \
  feature/tests/thinking-effort-claude-cli-capture-signal.test.mjs \
  feature/tests/thinking-effort-kiro-wire-signal.test.mjs \
  ...

Full Node runner/path/signal/load-target contract: 85/85 pass
```

Two test-runner abnormal-path reds were found and fixed before the 85/85 rerun:

- `thinking-effort-kiro-wire.mjs` command-timeout cleanup initially waited the normal 1s TERM period after a child had already exceeded its command deadline. Under load the natural cleanup fixture crossed its strict 2.5s test bound once. Timed-out commands now use a shorter TERM grace before KILL; the focused signal suite then passed 42/42 and the full Node suite passed 85/85.
- `thinking-effort-claude-cli-capture.mjs` could abort SIGINT cleanup if a transient `ps` inspection failed while draining the owned Claude process group. The cleanup now treats process-group inspection failure as retryable during shutdown and still sends bounded TERM/KILL. The capture signal gate passed 9/9, and the full Node suite passed 85/85.

```text
tests=3 pass=3 fail=0
SIGHUP/SIGINT/SIGTERM cleanup all passed
duration_ms=340.9715
```

## Cleanup and disk state

Removed during this run:

- failed bare-invoke artifact roots:
  - `/private/tmp/kiro-validation-artifacts-bare_20260719095518_71330`
  - `/private/tmp/kiro-validation-artifacts-bare_20260719095658_90075`
  - `/private/tmp/kiro-validation-artifacts-bare_20260719095836_23854`
- historical validation databases matching `kiro_thinking_wire_twire%_{cli,ide}`
- successful run artifact roots and temporary psql wrapper

Post-run DB checks found no `kiro_bare_%` or `kiro_thinking_wire_twire%` database.

Post-run build inventory:

```text
build-artifact-inventory version=2 mode=read-only targets=1 reservations=0 target_processes=1 blockers=2
target classification=unmanaged-repo-cargo-target size_kib=725912
target-process pid=84264 classification=kiro-runtime
release-gate result=fail
```

This blocker is the user's existing `9022` service running from `./target/release/kiro-rs`. It is not a leftover from the scoped validation builds. Under the safety contract, it was not stopped or deleted. Final release inventory cannot pass until that service is stopped or moved to a repository-external binary by an explicitly authorized step.

## Remaining release blockers

- Run full C0 again on the final tree: `cargo fmt --check`, `git diff --check`, full tests, release build, and frozen binary copy in one scoped batch.
- Run C1 direct protocol stream/non-stream/thinking/tool/error/alias cases against the final frozen binary.
- Run C2/C3/C4 real Claude CLI long conversation, tool loop, MCP/search/image, resume, and contamination-retry fault cases.
- L1-L5 fake-upstream load/chaos has passed for the current r8 frozen candidate; rerun only if later code changes alter the frozen binary. Remaining load work is two-instance Redis/PostgreSQL scheduler chaos and targeted usage+scheduler pressure.
- Complete two-instance Redis/PostgreSQL scheduler chaos, usage+scheduler combined pressure, UI browser gates, upgrade smoke, and final zero-residue inventory.
- Resolve the configuration design issue where broad prompt steering can still disable compatibility thinking controls; this run only fixed the premature native-schema 400.
