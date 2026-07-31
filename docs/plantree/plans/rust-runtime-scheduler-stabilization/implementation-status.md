# Implementation Status

Last reviewed: 2026-07-28 Asia/Shanghai

Current phase:

- Current Rust runtime/scheduler stabilization plan created.
- External pool hot-path fix is implemented and locally validated.
- Documentation migration/archival is in progress.

Last landed evidence:

- `external_pool_cached_immediate_availability` real local PgSQL/Redis: `2 passed / 0 failed`.
- `external_pool_immediate_availability_requires_current_capacity_and_recovers`: `1 passed / 0 failed`.
- `external_fallback`: `9 passed / 0 failed`.
- `raw_external`: `2 passed / 0 failed`.
- `preflight_external_error`: `1 passed / 0 failed`.
- `cargo fmt --check`: pass.
- `cargo check --all-targets --locked`: pass.
- clippy baseline: `811 <= 849`.
- build artifact inventory: `targets=0 reservations=0 target_processes=0 blockers=0`.

Active TODO:

1. Complete first archive batch for old slow-first-token/stream-fluidity analysis.
2. Register this plan in `docs/plantree/README.md`.
3. Decide and implement explicit local capacity overflow policy.
4. Design cooldown policy controls and manual recovery.
5. Run scheduler/external-pool load + chaos validation.

Blocked by:

- Thinking signature Branch A vs B still requires Kiro wire capture.
- Full load/chaos requires scoped fake/local test plan and frozen binary.

Next target:

- Finish document migration/archival batch without breaking links, then proceed to scheduler strategy implementation.

