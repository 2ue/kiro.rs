# Local credential quota guard focused validation - 2026-08-01

Status: `focused-pass / final-candidate-pending / production-recurrence-pending`

Related issue:

- [Local credential exhausted overage-disabled 400](../issues/local-credential-exhausted-overage-disabled-400-20260731.md)

## Scope

This evidence covers the focused fix for exhausted local Kiro API-key credentials that still had `disabled=false` while `credential_account_info` showed:

- `remaining <= 0`
- `credit_remaining <= 0`
- `overage_status = DISABLED`

The validation was local and did not restart or modify the existing `127.0.0.1:9022` service, production PostgreSQL, production Redis, or production credentials.

## Code path validated

- Startup now can pass `credential_account_info` into `MultiTokenManager`.
- Reload now calls `load_credentials_with_runtime_state_and_account_info()`.
- `CredentialEntry.account_quota_blocked` is a derived in-memory guard, not a persisted runtime disable.
- The guard applies only to fresh API-key snapshots with exhausted remaining and credit plus disabled overage.
- Dispatchability and fallback decisions exclude quota-guarded credentials.
- Generic 400 remains fail-fast unless the selected credential already has the quota guard.

## Commands

```bash
feature/tests/run-cargo-scoped.sh quota-guard -- cargo test --bin kiro-rs quota_guard_
KIRO_RS_REQUIRE_STORAGE_TESTS=1 KIRO_RS_TEST_POSTGRES_URL="<local config PgSQL URL>" feature/tests/run-cargo-scoped.sh reload-quota-guard-pg -- cargo test --bin kiro-rs reload_account_info_quota_guard_reselects_healthy_credential -- --nocapture
feature/tests/run-cargo-scoped.sh provider-bad-request -- cargo test --bin kiro-rs bad_request_retry_matrix_bounds_real_provider_http_hits
feature/tests/run-cargo-scoped.sh postgres-account-info -- cargo test --bin kiro-rs postgres_persists_runtime_config_credentials_stats_usage_and_pricing
feature/tests/run-cargo-scoped.sh token-manager -- cargo test --bin kiro-rs kiro::token_manager
feature/tests/run-cargo-scoped.sh all-targets-test -- cargo test --all-targets --locked
node --test feature/tests/*.test.mjs
cd ui && npm run check
cd admin-ui && npm run build
feature/tests/run-cargo-scoped.sh release-build -- cargo build --release --bins --locked
feature/tests/run-cargo-scoped.sh fmt-check -- cargo fmt --check
git diff --check
```

## Results

- `quota_guard_`: `2 passed / 0 failed`.
- `reload_account_info_quota_guard_reselects_healthy_credential`: `1 passed / 0 failed` with `KIRO_RS_REQUIRE_STORAGE_TESTS=1` against the local PgSQL endpoint through `PostgresStore::connect_test`, which creates and drops an isolated random `kiro_rs_test_<uuid>` schema.
- `bad_request_retry_matrix_bounds_real_provider_http_hits`: `1 passed / 0 failed`; the existing bad-request matrix still issues exactly bounded hits and does not retry generic, tool, image, malformed, or invalid-model 400s.
- `postgres_persists_runtime_config_credentials_stats_usage_and_pricing`: `1 passed / 0 failed`; the joint credentials/runtime/account-info load contract is covered.
- `kiro::token_manager`: `308 passed / 0 failed / 1541 filtered out`.
- `cargo test --all-targets --locked`: main `1843 passed / 0 failed / 6 ignored`; `kiro_loadtest` `31 passed / 0 failed`.
- Node feature tests: `283 tests / 261 pass / 22 skipped / 0 fail`.
- `ui` typecheck: passed.
- `admin-ui` production build: passed.
- `cargo build --release --bins --locked`: passed after removing two new release-only dead-code warnings by making legacy compatibility loaders test-only.
- `cargo fmt --check`: passed after formatting.
- `git diff --check`: passed.

## Release meaning

This closes the focused implementation gap for the local exhausted-account scheduler guard. It does not close the release gate by itself.

Remaining gates:

- frozen release binary binding;
- isolated service startup/reload smoke with fake upstream and account-info snapshots that proves the guarded credential receives zero upstream hits;
- broader scheduler/load validation;
- production recurrence observation on the affected hosts after rollout.
