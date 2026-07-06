# Implementation Status

Current Phase: Body capability modularization implemented and validated

Next Target: Keep behavior stable, then consider route planner extraction only if future profiling shows parsed preprocessing is still too expensive for non-raw external routes.

Last Landed: 2026-07-06 capability plan extraction, converter module split, runtime/UI capability toggles, and fake upstream regression.

Active TODO:

1. Keep compatibility defaults enabled unless a future change intentionally disables a specific capability.
2. Consider route planner extraction as a separate performance project.
3. Consider a plugin/trait layer only after another upstream family needs different stage composition.

Blocked By: Nothing known.

Last Verified:

- `cargo fmt --check`
- `git diff --check`
- `pnpm check` in `ui/`
- `pnpm exec tsc -b --pretty false` in `admin-ui/`
- `CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test`
- `CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo build --release`
- Temp proxy on `127.0.0.1:19022` against fake upstream on `127.0.0.1:19080`.
- Reports under `target/loadtest/modular-20260706160929/`.
- Final summary: `target/loadtest/modular-20260706160929/final-validation-summary.json`.
- Static result: 904 main tests and 19 `kiro_loadtest` tests passed; release build passed in 3m39s.
- Fake upstream result: normalized and raw external pools both passed normal, non-stream, thinking/tool, slow-first-byte, long-context long-stream, burst, error/cooldown/recovery, and mixed-chaos coverage.
- Cleanup status: temp proxy stopped, `19022/19080` released, database `kiro_rs_loadtest_modular_20260706160929` dropped, Redis prefix `kiro_rs:loadtest:modular-20260706160929:*` deleted.
