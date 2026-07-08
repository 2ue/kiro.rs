# Test And Release Gates

## Static Gates

- `cargo fmt --check`
- `git diff --check`
- `cargo test`
- `cargo build --release`

## Frontend Gates When UI Is Touched

- `pnpm --dir ui build`

## Docker Gate Before Tag Release

- `docker build -t kiro-rs:local-release-check .`
- The Dockerfile must build only the maintained `ui/` frontend. Legacy `admin-ui/` or `admin-ui-daisy/` paths must not be referenced by the active Docker build.

## Protocol Gates When `/cc/v1` Or Usage Changes

- Direct `/cc/v1/messages` stream and non-stream smoke tests.
- Claude Code CLI isolated HOME/CLAUDE_CONFIG_DIR tests when behavior may affect CLI semantics.
- Verify final usage is non-zero and cache fields do not contradict the route policy.

## Load/Chaos Gates

- Use temp ports, not an active production/dev `9022` service unless explicitly requested.
- Prefer fake upstream first.
- Save reports under `target/loadtest/`.
- Capture status counts, p50/p95/p99 TTFB, p50/p95/p99 total latency, CPU/RSS/FD start/peak/end, request ids, and error ids.
