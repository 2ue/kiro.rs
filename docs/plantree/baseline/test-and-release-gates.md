# Test And Release Gates

## Static Gates

- `cargo fmt --check`
- `git diff --check`
- `cargo test`
- `cargo build --release`

## Frontend Gates When UI Is Touched

- `pnpm --dir admin-ui build`
- `pnpm --dir ui build`

## Protocol Gates When `/cc/v1` Or Usage Changes

- Direct `/cc/v1/messages` stream and non-stream smoke tests.
- Claude Code CLI isolated HOME/CLAUDE_CONFIG_DIR tests when behavior may affect CLI semantics.
- Verify final usage is non-zero and cache fields do not contradict the route policy.

## Load/Chaos Gates

- Use temp ports, not an active production/dev `9022` service unless explicitly requested.
- Prefer fake upstream first.
- Save reports under `target/loadtest/`.
- Capture status counts, p50/p95/p99 TTFB, p50/p95/p99 total latency, CPU/RSS/FD start/peak/end, request ids, and error ids.
