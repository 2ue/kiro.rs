# Regression Plan

## Static And Unit Gates

- `cargo fmt --check`
- `cargo test --locked --no-default-features`
- Targeted tests for:
  - Usage query request-id exact filtering.
  - Redis/PgSQL/memory model filter parity.
  - Credential supported-model matching.
  - External pool supported-model matching.
  - Prompt-logic retry disabled by default.
  - Prompt-logic retry enabled with untried credential only.
  - External raw and normalized body modes still respect configured body/model behavior.

## Frontend Gates

- `pnpm --dir ui build`
- `pnpm --dir admin-ui build`

## Fake Upstream Gates

- Normal stream and non-stream.
- External raw passthrough and normalized pool.
- Model-restricted credential and pool selection.
- Unsupported model returns clear local routing error and does not call restricted accounts.
- 400 tool-use/prompt error with retry disabled and enabled.
- 429/500/timeout behavior remains unchanged outside the new opt-in branch.
- Slow first byte samples at a few seconds and over 10 seconds to ensure attempt timing fields remain understandable.

## Resource Checks

- Capture proxy RSS/FD before and after fake upstream regression.
- Confirm no generated observability work blocks request completion.
- Confirm no query or sync path runs in the request hot path except dispatch eligibility checks.

## Real Upstream Smoke

Run only after local regression passes and only with low volume:

- Sync supported models from one local credential.
- One successful local credential request using an allowed model.
- One blocked local request using a model not in a configured temporary test allowlist, then restore config.
- One external pool request if a safe test pool is available.

## Executed 2026-07-07

Static and unit:

- `git diff --check`: pass.
- `cargo test --locked --no-default-features`: pass with local Xcode toolchain env; 920 main tests and 19 `kiro_loadtest` tests passed.

Frontend:

- `pnpm --dir ui build`: pass.
- `pnpm --dir admin-ui build`: pass.

Fake upstream direct:

- `target/loadtest/admin-routing-normal-stream.json`: `normal_stream`, 6 requests, all HTTP 200, p95 TTFB 2 ms.
- `target/loadtest/admin-routing-normal-non-stream.json`: `normal_non_stream`, 6 requests, all HTTP 200, p95 TTFB 3 ms.
- `target/loadtest/admin-routing-tiered-slow-first-byte.json`: `tiered_slow_first_byte`, 6 requests, all HTTP 200, p95 TTFB 22005 ms.
- `target/loadtest/admin-routing-long-stream.json`: `long_stream`, 4 requests, all HTTP 200, p95 TTFB 504 ms.
- `target/loadtest/admin-routing-mixed-chaos.json`: `mixed_chaos`, 12 requests, status distribution 10 HTTP 200, 1 HTTP 429, 1 HTTP 500, p95 TTFB 22005 ms.

Not executed:

- Real upstream smoke was not run in this pass because the latest request did not explicitly ask to consume real credentials.
