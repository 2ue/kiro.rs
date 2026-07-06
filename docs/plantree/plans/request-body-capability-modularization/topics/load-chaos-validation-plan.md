# Load And Chaos Validation Plan

## Fake Upstream Scenarios

Use `src/bin/kiro_loadtest.rs` with temp ports and reports under `target/loadtest/`.

Required scenarios:

- normal stream
- normal non-stream
- slow first byte
- slow thinking then text
- long stream
- 429 burst
- 500 burst
- recovery after burst
- client drop when practical

## Payload Hotspots

Run representative payload cases:

- `text-history` with long contexts.
- `large-tool-results` with slow first byte.
- `deep-tool-input` for nested JSON traversal.
- `many-tools` for schema/tool normalization.
- `mixed-pathological` with long stream.

## Resource Evidence

Capture for the proxy process:

- RSS start, peak, end.
- FD start, peak, end.
- CPU percent start, peak, end.
- p50/p95/p99 TTFB.
- p50/p95/p99 total latency.
- status distribution.
- request ids and error ids.

## Pass Criteria

- Normal requests succeed on local and external-compatible paths.
- Raw external passthrough does not parse or mutate body unless configured to rewrite top-level model.
- Normalized external paths still apply configured usage projection and payload guard.
- No capacity error appears while eligible external pools are available.
- RSS and FD counts return near baseline after traffic stops.
- Error responses remain normalized and do not expose internal scheduler details.

## 2026-07-06 Validation Evidence

Static gates:

- `cargo fmt --check`: pass.
- `git diff --check`: pass.
- `pnpm check` in `ui/`: pass.
- `pnpm exec tsc -b --pretty false` in `admin-ui/`: pass.
- `CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test`: pass, 904 main tests and 19 `kiro_loadtest` tests.
- `CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo build --release`: pass, 3m39s.

Fake upstream proxy setup:

- Temporary proxy: `127.0.0.1:19022`.
- Fake upstream: `127.0.0.1:19080`.
- External raw pool and external normalized pool were tested separately.
- Temporary database and Redis namespace were isolated and cleaned after validation.
- Run directory: `target/loadtest/modular-20260706160929`.
- Final summary: `target/loadtest/modular-20260706160929/final-validation-summary.json`.
- Cleanup: temp proxy stopped, `19022/19080` released, database `kiro_rs_loadtest_modular_20260706160929` dropped, Redis prefix `kiro_rs:loadtest:modular-20260706160929:*` deleted.

Representative reports:

- `normalized-normal-stream.json`: 20/20 success, p95 TTFB 64 ms.
- `normalized-normal-non-stream.json`: 12/12 success, p95 TTFB 13 ms.
- `normalized-thinking.json`: 12/12 success, p95 first thinking 39 ms, p95 first text 1596 ms.
- `normalized-tool-use.json`: 12/12 success.
- `normalized-tiered-slow-first-byte.json`: 9/9 success, p95 TTFB 22053 ms.
- `normalized-payload-mixed-long-stream.json`: 24/24 success, p95 total latency 6424 ms.
- `normalized-burst-high-concurrency.json`: 120/120 success.
- `normalized-rate-limit429-short.json`: expected 429/cooldown errors; after 1s test cooldown, `normalized-recovery-after-429.json` was 12/12 success.
- `normalized-server-error500-short.json`: expected 502/cooldown errors; after 1s test cooldown, `normalized-recovery-after-500.json` was 12/12 success.
- `normalized-client-drop-retry.json`: 12 upstream 200 responses with intentional client drops, counted as expected client-side errors.
- `normalized-mixed-chaos-multipool.json`: 36/36 final success under mixed 429/500/slow-first-byte/long-stream behavior and failover.
- `normalized-sustained-60s-c40.json`: 25323/25323 success over 61.1 seconds, about 24852 RPM, p95 TTFB 174 ms, p95 total latency 175 ms.
- `raw-explicit-direct-normal-stream.json`: 16/16 success.
- `raw-explicit-direct-non-stream.json`: 8/8 success.
- `raw-explicit-direct-long-stream.json`: 24/24 success, p95 total latency 6192 ms.
- `raw-model-rewrite.json`: 3/3 success; fake upstream captured `model=mapped-raw-sonnet-45`.
- `raw-model-none.json`: 3/3 success; fake upstream captured original `model=claude-sonnet-4.5`.
- `raw-fallback-no-explicit-direct.json`: 12/12 success with `externalDirectPolicyEnabled=false`.

Observed resource notes:

- Normalized long-context long-stream: RSS 33.6 MB -> 128.7 MB -> 110.8 MB, FD 34 -> 52 -> 52, CPU peak 26.7%.
- Normalized sustained 60s/c40: RSS 22.3 MB -> 80.5 MB -> 70.2 MB during the report; after 8 seconds idle, RSS was about 14.5 MB and FD count was 37.
- Raw explicit-direct long-stream: RSS 35.2 MB -> 81.9 MB -> 78.2 MB, FD 36 -> 52 -> 52, CPU peak 8.7%.
- The normalized path is heavier than raw under the same mixed pathological payload because it intentionally runs parsed body, converter-compatible stages, and payload guard.
- Raw fallback usage record had `externalPoolId=2`, `usageProjectionApplied=true`, and `externalPoolBilling.usageProjectionMode=current_path_policy`, proving usage projection/billing is independent from body mode.
