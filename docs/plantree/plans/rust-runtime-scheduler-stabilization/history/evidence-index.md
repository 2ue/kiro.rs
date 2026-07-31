# Evidence Index

Last reviewed: 2026-07-28 Asia/Shanghai

## Current evidence

| Date | Evidence | Scope | Notes |
| --- | --- | --- | --- |
| 2026-07-28 | `external_pool_cached_immediate_availability` | External pool cached/no-wait hot path | Real local PgSQL/Redis, 2 passed |
| 2026-07-28 | `external_pool_immediate_availability_requires_current_capacity_and_recovers` | Authoritative external capacity behavior | Real local PgSQL/Redis, 1 passed |
| 2026-07-28 | `external_fallback` | Handler fallback classifier/gates | 9 passed |
| 2026-07-28 | `raw_external` | Raw external route request regression | 2 passed |
| 2026-07-28 | `preflight_external_error` | External preflight local rescue loop prevention | 1 passed |
| 2026-07-28 | `cargo check --all-targets --locked` | Static compile gate | pass |
| 2026-07-28 | clippy baseline | Static lint gate | `811 <= 849` |
| 2026-07-28 | build artifact inventory | Disk/build hygiene | `targets=0 reservations=0 target_processes=0 blockers=0` |

## Related evidence packages

- `docs/kiro-rs-root-cause-package-20260726T170519+0800/`
- `feature/evidence/runtime-completion-storage-coupling-validation-20260727.md`
- `feature/evidence/final-runtime-storage-cli-load-validation-20260727.md`
- `feature/evidence/protocol-thinking-cli-live-20260725.md`
- `feature/evidence/external-pool-success-zero-billing-20260723.md`

Rules:

- Evidence is dated and build/config scoped.
- Do not treat an old evidence file as current pass unless the current code and command were rerun.
- Raw production archives must stay redacted and must not be copied into active roadmap files.

