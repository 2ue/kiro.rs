# Final Release Gate - 2026-08-01

Status: `current-batch-release-gate-passed / version-bump-and-tag-pending`

Scope: Current working-tree fixes for WebSearch/tool parsing, local credential quota guard, downstream standard usage caps, usage error diagnostics, Pro Max subscription classification, and Claude CLI validation harness compatibility.

Release target: `v0.0.126` after a dedicated Cargo package version bump.

## Candidate Identity

- Branch: `main`.
- Upstream: `origin/main`, `0 ahead / 0 behind` before the work commit.
- Base release tag before this batch: `v0.0.125`.
- Candidate binary used for real Claude CLI validation:
  - Path: `/tmp/kiro-cli-candidate.NZWBnG/kiro-rs`.
  - SHA-256: `bca03a67e3744685e19f95e49b7601fd7d744575e421f140a9d895b1a7c8f3a6`.
  - Built through scoped Cargo wrapper, copied outside repository `target`, then all scoped build artifacts were removed.
- Claude Code CLI:
  - Real executable: `/Users/yuanfeijie/.volta/tools/image/packages/@anthropic-ai/claude-code/bin/claude`.
  - Version: `2.1.220`.

The candidate binary was built before the release version bump while `Cargo.toml` was still `0.0.125`. Because `src/admin/handlers.rs` embeds `CARGO_PKG_VERSION`, the release bump must be followed by release-tag version validation and a fresh release build before tagging.

## Code And Test Harness Changes Covered

- Native WebSearch accepts the official `web_search_YYYYMMDD` family, including current and future-looking date suffixes, and routes server-side WebSearch instead of treating it as an ordinary client tool.
- Tool-name/schema-key mapping covers invalid names, overlong names, normalized collisions, and reversible schema input mapping.
- Tool-result-only turns use a semantic Kiro placeholder instead of an inert dot that could cause upstream to ignore the tool result.
- Local API-key credentials with fresh exhausted `remaining<=0`, `credit_remaining<=0`, and `overage_status=DISABLED` snapshots are guarded out of dispatch/fallback selection.
- Downstream standard usage fields apply cache read/write caps in both full `reportedUsage` and no-full-reported local prompt-cache paths; non-success local/external records keep request estimates in diagnostics and zero standard fields.
- Usage detail surfaces retain upstream/processing diagnostics instead of showing only normalized errors.
- Account cards and backend sorting/filtering distinguish `Pro Max` from generic `Pro`; `Power` and `Pro Max` filter options are exposed.
- `feature/tests/thinking-effort-kiro-wire.mjs` no longer hard-codes Claude CLI `2.1.197` as the only accepted default. It now records the actual CLI version, enforces an optional exact `KIRO_EXPECTED_CLAUDE_VERSION`, and otherwise requires a recognizable version at or above the supported minimum `2.1.197`.

## Validation Matrix

### Rust

- `feature/tests/run-cargo-scoped.sh final-full-default-20260801 -- cargo test --locked --all-targets -- --test-threads=1`
  - Main tests: `1850 passed / 0 failed / 6 ignored`.
  - `kiro_loadtest`: `31 passed / 0 failed`.
  - Scoped target cleanup: `removed=true / reservation_released=true`.
- `feature/tests/run-cargo-scoped.sh final-full-no-default-20260801 -- cargo test --locked --all-targets --no-default-features -- --test-threads=1`
  - Main tests: `1850 passed / 0 failed / 6 ignored`.
  - `kiro_loadtest`: `31 passed / 0 failed`.
  - Scoped target cleanup: `removed=true / reservation_released=true`.
- `feature/tests/run-cargo-scoped.sh final-release-build-20260801 -- cargo build --release --bins --locked`
  - Passed.
  - Scoped target cleanup: `removed=true / reservation_released=true`.
- `feature/tests/run-cargo-scoped.sh final-cli-candidate-build-20260801 -- cargo build --release --bin kiro-rs --locked`
  - Passed.
  - Copied frozen binary SHA-256 `bca03a67e3744685e19f95e49b7601fd7d744575e421f140a9d895b1a7c8f3a6`.
  - Scoped target cleanup: `removed=true / reservation_released=true`.
- `feature/tests/run-cargo-scoped.sh final-fmt-after-cli-20260801 -- cargo fmt --all -- --check`
  - Passed.

### Frontend

- `npm run check` in `ui`
  - Passed.
- `npm run build` in `ui`
  - Passed.
  - Vite emitted the existing chunk-size warning; build completed successfully.
- `npm run build` in `admin-ui`
  - Passed.

### Node Contracts And Documentation

- `node --test feature/tests/*.test.mjs`
  - `283 tests`.
  - `261 passed`.
  - `22 skipped`.
  - `0 failed`.
- `node --test feature/tests/thinking-effort-kiro-wire-contract.test.mjs`
  - `3 passed / 0 failed`.
  - Covers the updated Claude CLI version policy.
- `node feature/tests/check-feature-docs.mjs`
  - `68 issue documents`.
  - `272 relative links`.
  - `0 failures`.
- `git diff --check`
  - Passed.
- `node feature/tests/inventory-build-artifacts.mjs --gate`
  - `targets=0`.
  - `reservations=0`.
  - `target_processes=0`.
  - `blockers=0`.
  - `release-gate result=pass`.

### Real Claude Code CLI

Runner:

```bash
KIRO_RS_BINARY=/tmp/kiro-cli-candidate.NZWBnG/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/tmp/kiro-cli-artifacts.1DsCpM \
KIRO_CLAUDE_BINARY=/Users/yuanfeijie/.volta/tools/image/packages/@anthropic-ai/claude-code/bin/claude \
KIRO_VALIDATION_PROGRESS=1 \
feature/tests/run-claude-cli-release-suite-local.sh
```

The first full suite invocation passed `bare-invoke` and `long-session`, then stopped before behavior testing in `thinking-wire` because the runner still defaulted to an exact `2.1.197` CLI version. That was a validation-harness issue, not a candidate service failure.

After the version-policy fix:

```bash
KIRO_RS_BINARY=/tmp/kiro-cli-candidate.NZWBnG/kiro-rs \
KIRO_VALIDATION_ARTIFACT_DIR=/tmp/kiro-cli-artifacts.1DsCpM \
KIRO_CLAUDE_BINARY=/Users/yuanfeijie/.volta/tools/image/packages/@anthropic-ai/claude-code/bin/claude \
KIRO_VALIDATION_PROGRESS=1 \
KIRO_CLI_SUITE_ONLY=thinking \
feature/tests/run-claude-cli-release-suite-local.sh
```

Results:

- `bare-invoke`: `result=pass`, `rounds=5`, cleanup all true, candidate SHA matched `bca03a67e3744685e19f95e49b7601fd7d744575e421f140a9d895b1a7c8f3a6`.
- `long-session`: `result=pass`, `rounds=5`, `5 sessions / 110 turns / 100 tool pairs`, `leakMatches=[]`, cleanup all true, candidate SHA matched.
- `thinking-wire`: `result=pass`, `totalCases=60`, `cli/ide x absent/low/medium/high/xhigh/max x 5`, `violations=0`, `claudeCliVersion=2.1.220`, `claudeCliVersionPolicy=minimum`, cleanup all true, candidate SHA matched.

Report files:

- `/tmp/kiro-cli-artifacts.1DsCpM/reports/bare-invoke-claude-cli/bare-invoke-1785575901588-75231-251c3d.json`
- `/tmp/kiro-cli-artifacts.1DsCpM/reports/claude-cli-long-session-continue/long-session-1785575919331-76320-2ca2d6.json`
- `/tmp/kiro-cli-artifacts.1DsCpM/reports/thinking-effort-wire/thinking-effort-wire-1785576266349-660-569e8b.json`

The real CLI suite used caller-owned temporary PostgreSQL databases, isolated Redis DB/prefixes, isolated Claude config/home directories, temporary non-`9022` service ports, and fake upstream. Existing local `127.0.0.1:9022` was not restarted or probed by the runner.

## Release Decision

This evidence supports releasing the current scoped patch batch as `v0.0.126` after the required Cargo version bump and tag-version validation.

Broader scheduler/load/chaos, production recurrence, browser screenshots, image-source matrix, and long-running architecture gates remain open in the issue index. They are not marked closed by this release evidence and remain post-release work.

## Residual Risk

- Real production upstream recurrence for the exhausted-account 400 class still needs observation after rollout.
- Image-source instability analysis remains open; this batch contains existing payload/image safeguards but does not claim the full image matrix is closed.
- The Pro Max label bug is deterministic and fixed by classifier order/key/rank tests; a live browser screenshot was not required for root cause, but browser verification remains useful for UI regression coverage.
- The candidate binary SHA above is pre-version-bump; release bump validation must confirm `v0.0.126` matches Cargo package version before tag push.
