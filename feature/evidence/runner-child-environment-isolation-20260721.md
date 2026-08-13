# Runner Child Environment Isolation Contract 2026-07-21

Status: `contract-pass / no-docker / no-cargo / no-runtime-service`

Scope: validation runner safety only. This evidence does not prove product protocol, scheduler,
load, UI, upgrade, or release behavior.

## Issue

Several runtime and Claude CLI validation runners still started child processes with
`...process.env`. That could silently pass caller-owned PostgreSQL/Redis URLs, API keys,
Claude/OpenAI provider flags, or unrelated `KIRO_*` variables into temporary `kiro-rs`, Claude,
proxy, or scoped-Cargo child processes. The main risk is validation contamination: a runner could
appear to validate an isolated fixture while a child actually reads a developer shell override.

## Fix

Added `feature/tests/validation-child-env.mjs` and routed real runner child processes through
`validationChildEnvironment(extra)`.

The helper allowlists only execution basics such as `PATH`, temp locale/user variables, `HOME`,
`VOLTA_HOME`, `CARGO_HOME`, and `RUSTUP_HOME`, then overlays explicit per-child variables. It does
not inherit storage URLs, Anthropic/OpenAI credentials, request keys, or generic `KIRO_*` state
unless the runner passes a specific variable in `extra`.

Updated runner surfaces:

- `feature/tests/bare-invoke-claude-cli.mjs`
- `feature/tests/claude-cli-long-session-continue.mjs`
- `feature/tests/thinking-effort-claude-cli-capture.mjs`
- `feature/tests/external-takeover-scheduler-degraded-nondocker.mjs`
- `feature/tests/e03-real-two-process-scheduler.mjs`
- `feature/tests/scheduler-fairness-sticky-race.mjs`
- `feature/tests/run-redis-fault-domain-product-validation.mjs`

Contract tests now assert:

- the helper does not inherit `DATABASE_URL`, `REDIS_URL`, `ANTHROPIC_API_KEY`,
  `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`, `KIRO_API_KEY`, `KIRO_RS_TEST_REDIS_URL`, or arbitrary
  `KIRO_*`-shaped fixture state;
- every non-test `feature/tests/*.mjs` validation runner source is free of `...process.env`;
- the existing runtime path contract still rejects repository paths, direct `target/debug|release`
  outputs, symlink escapes, wrong types, missing paths, and protected `9022` listener probes.

## Commands Run

No Docker, Cargo, or `kiro-rs` service was started.

```bash
node --test feature/tests/runtime-validation-paths.test.mjs
```

Result: `11/11` pass.

```bash
node --check feature/tests/validation-child-env.mjs \
  && node --check feature/tests/bare-invoke-claude-cli.mjs \
  && node --check feature/tests/claude-cli-long-session-continue.mjs \
  && node --check feature/tests/thinking-effort-claude-cli-capture.mjs \
  && node --check feature/tests/external-takeover-scheduler-degraded-nondocker.mjs \
  && node --check feature/tests/e03-real-two-process-scheduler.mjs \
  && node --check feature/tests/scheduler-fairness-sticky-race.mjs \
  && node --check feature/tests/run-redis-fault-domain-product-validation.mjs
```

Result: pass.

```bash
rg -n "\.\.\.process\.env" feature/tests/*.mjs scripts/loadtest/*.mjs
```

Result: remaining matches are only `.test.mjs` fixture launchers that intentionally pass test
environment to the script under test. Non-test validation runners have no `...process.env` match.

```bash
git diff --check -- \
  feature/tests/validation-child-env.mjs \
  feature/tests/runtime-validation-paths.test.mjs \
  feature/tests/bare-invoke-claude-cli.mjs \
  feature/tests/claude-cli-long-session-continue.mjs \
  feature/tests/thinking-effort-claude-cli-capture.mjs \
  feature/tests/external-takeover-scheduler-degraded-nondocker.mjs \
  feature/tests/e03-real-two-process-scheduler.mjs \
  feature/tests/scheduler-fairness-sticky-race.mjs \
  feature/tests/run-redis-fault-domain-product-validation.mjs
```

Result: pass.

## Evidence Boundary

This closes only the runner child-environment contamination contract. It does not replace dynamic
service runs for E01/E02, E03, E05, F06, request-admission, external takeover, Claude CLI native
upstream, L3/L4/L5 load/chaos, UI browser/build, upgrade, or final release inventory.
