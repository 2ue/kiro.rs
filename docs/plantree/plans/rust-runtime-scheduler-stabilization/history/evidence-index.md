# Evidence Index

Last reviewed: 2026-08-05 Asia/Shanghai

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
| 2026-08-04 | `external-pool-billing-verification-20260804.md` | External-pool usage raw-cost, PgSQL rollup, Redis Dashboard and Admin UI | `external_pool::tests 214/214`; PgSQL 2×`1/1`; Redis `1/1`; docs/diff/fmt/admin-ui build pass; production observation pending |
| 2026-08-04 | `external-pool-body-mode-model-routing-fix-20260804.md` | External-pool body-mode/model routing P0 | `raw_route_ 2/2`; `eligibility 7/7`; `external_pool::tests 218/218`; handler source contract `1/1`; fmt pass; production observation pending |
| 2026-08-04 | `external-pool-retry-cooldown-fix-20260804.md` | External-pool same/cross-pool retry configuration, Retry-After cooldown, consecutive transient cooldown escalation, cooldown clear and runtime snapshot invalidation | local PostgreSQL/Redis + fake upstream: `external_pool_ 146/146`; same-pool retry `3/3`; cooldown clear/atomic acquire `1/1`; default config `1/1`; production observation and multi-instance race pending |
| 2026-08-04 | `scheduler-target-contract-focused-validation-20260804.md` | Proposed local/external scheduler target contract and current compliance confirmation | Rust external focused `10/10`; handler focused `9/9`; Node contracts `104 total / 92 passed / 12 skipped`; docs/diff pass; complete L1–L5 sustained matrix not run |
| 2026-08-05 | `scheduler-shared-deadline-and-load-chaos-20260805.md` | Redis scheduler/usage joint fault boundary, external-pool priority failover and half-open recovery | Redis chaos `24/24` exact across 3 outer rounds; 75ms boundary reproduced once then passed in 3 complete rounds; external priority failover pass; no production deadline relaxation |
| 2026-08-05 | `scheduler-shared-deadline-and-load-chaos-20260805.md` (final candidate dynamic supplement) | Fresh-database L3/L4/L5 and external priority failover | L3 `9/9`, L4 `12/12`, external priority failover pass, L5 `3/3` with `1380/1380` long-stream success and settled RSS/FD after warm baseline |

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
