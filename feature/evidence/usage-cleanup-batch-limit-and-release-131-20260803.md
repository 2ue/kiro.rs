# Usage cleanup batch limit and v0.0.131 release recovery - 2026-08-03

Status: `focused-pass / release-131-published / no-production-service-change`

## Scope

This evidence covers two related follow-ups:

- the user-reported Usage cleanup backend safety limit of 500 being too small;
- the remote `v0.0.131` publish failure.

No production host was modified. The local `127.0.0.1:9022` service was not restarted. PostgreSQL tests used per-test schemas under the local configured PostgreSQL database and dropped their test schemas after completion. Redis tests used the local configured Redis test URL.

## Implementation Summary

- Backend `每批数量` default remains `250`.
- Backend `每批数量` maximum changed from `500` to `5,000`.
- New PostgreSQL databases create `usage_cleanup_jobs.batch_size` with `CHECK (batch_size > 0 AND batch_size <= 5000)`.
- Existing databases receive startup migration `usage-cleanup-batch-size-limit-v1`, which drops the old default check constraint name and re-adds the `<=5000` constraint.
- Startup schema compatibility now also checks the cleanup batch-size CHECK constraint. If `postgres.migrateOnStart=false` is used against an old `<=500` database, startup reports `usage_cleanup_jobs.batch_size_check<=5000` instead of letting Admin requests fail later during job insert.
- New and old UIs now clamp/display `每批数量` max as `5,000` and keep the default at `250`.
- The UI text explicitly says large single batches may pause on lock contention and should be lowered before resume if needed.

The safety contract is unchanged: cleanup still runs as short PostgreSQL batches with `FOR UPDATE SKIP LOCKED`, `lock_timeout=250ms`, `statement_timeout=10s`, persisted progress, explicit resume, cancellation, and per-run `maxBatches=10,000`.

## Remote v0.0.131 Failure And Recovery

Remote Actions evidence:

- Workflow: `Publish Docker Images`
- Run ID: `30757990049`
- URL: `https://github.com/2ue/kiro.rs/actions/runs/30757990049`
- Head: `v0.0.131` / `59b4c26f081438e59adc1507278f2591f6ce10b6`
- Result: failed in `quality / Frontend and Rust quality gate`; Docker `build` and `manifest` jobs were skipped.

Failing step:

- Step: `Check Clippy warning baseline`
- Output: total warnings decreased from the checked-in 849 baseline to 812, but one bucket exceeded its own limit:
  `clippy::field_reassign_with_default | src/model/config.rs: 21 warning(s), baseline 20`.

Root cause:

- A route-policy test initialized `Config::default()` and then reassigned `runtime_config_migration_version`.
- The repository Clippy gate enforces each lint/file bucket, not just total warning count.

Fix:

- The test now uses a struct initializer with `..Config::default()` instead of field reassignment.
- The baseline file was not loosened.

Recovery and successful republish:

- The fixed source was committed as `511cebb60e26d970b77b33a3638ec8d9806505de`
  (`fix: raise usage cleanup safety limit`) and pushed to `main`.
- The failed remote `v0.0.131` tag was explicitly deleted and recreated because
  the user requested reusing version 131 rather than advancing to 132.
- The recreated annotated tag `v0.0.131` peels to `511cebb60e26d970b77b33a3638ec8d9806505de`.
- GitHub Actions workflow `Publish Docker Images`, run `30800052601` (`#162`),
  was triggered by the recreated tag:
  `https://github.com/2ue/kiro.rs/actions/runs/30800052601`.
- Run `#162` completed successfully in `25m 36s`. The quality gate, both
  architecture Docker builds, and the multi-architecture manifest job all
  completed successfully.

## Validation

Commands run locally:

```bash
rustup run 1.92.0 cargo fmt --all -- --check
pnpm --dir ui check
pnpm --dir ui build
pnpm --dir admin-ui build
rustup run 1.92.0 node scripts/ci/check-clippy-baseline.mjs
rustup run 1.92.0 cargo check --locked --all-targets

KIRO_RS_TEST_POSTGRES_URL='<local config postgres url>' \
  rustup run 1.92.0 cargo test --locked usage_cleanup_request -- --nocapture

KIRO_RS_TEST_POSTGRES_URL='<local config postgres url>' \
  rustup run 1.92.0 cargo test --locked \
  postgres_startup_migration_expands_usage_cleanup_batch_size_constraint -- --nocapture

KIRO_RS_TEST_POSTGRES_URL='<local config postgres url>' \
  rustup run 1.92.0 cargo test --locked \
  required_postgres_schema_columns_cover_known_upgrade_breakers -- --nocapture

KIRO_RS_TEST_POSTGRES_URL='<local config postgres url>' \
KIRO_RS_TEST_REDIS_URL='<local config redis url>' \
  rustup run 1.92.0 cargo test --locked cleanup -- --nocapture --test-threads=1
```

Results:

| Gate | Result |
| --- | --- |
| Rust format | PASS |
| `ui` typecheck | PASS |
| `ui` production build | PASS; existing Vite chunk-size warning only |
| `admin-ui` production build | PASS |
| Clippy baseline | PASS; `811 warnings <= 849 baseline`, no bucket regression |
| Rust all-target check | PASS |
| `usage_cleanup_request` | PASS; `4/4` |
| cleanup batch-size constraint migration | PASS; `1/1`, old `<=500` check rejected 501, schema compatibility reported `usage_cleanup_jobs.batch_size_check<=5000`, and migration accepted 5000 |
| required schema guard coverage | PASS; `1/1`, `usage_cleanup_jobs.batch_size` is covered as a known upgrade breaker |
| cleanup filtered group | PASS; `42/42`, `0 failed`, `0 ignored` |

The first attempt at the migration test used a guessed PostgreSQL password and failed with authentication error before running product logic. It was rerun by reading the local configured PostgreSQL URL without printing credentials and passed.

## Release Meaning

This closes the focused implementation and local validation for raising the cleanup `每批数量` backend limit from 500 to 5,000, fixes the known `v0.0.131` Clippy publish blocker, and records the successful Docker republish of `v0.0.131` from the repaired commit.

It does not close:

- production-scale cleanup throughput;
- dynamic multi-instance Admin cache and cleanup races;
- Redis slow/reset chaos;
- a final release build or tag;
- production deployment rollout and post-release observation of the successfully republished `v0.0.131`.
