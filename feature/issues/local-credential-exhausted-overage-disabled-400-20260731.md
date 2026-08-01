# Local credential exhausted overage-disabled 400 - 2026-07-31

Status: `analysis-recorded / production-evidence-collected / usage-detail-diagnostics-improved / scheduler-quota-guard-implemented / focused-tests-passed / scoped-release-gate-passed / production-recurrence-pending`

Severity: `P0 production-impact / local-credential-routing`

## Summary

Three production hosts showed repeated `/ha/v1/messages` local credential failures with public `invalid_request_error` and message `The request body is invalid`.

The root cause is not a general request-body, image, or tools serialization failure. Old local KIRO POWER API-key credentials are exhausted and have overage disabled, but remain dispatchable because scheduler admission does not include credential account balance/overage state. The upstream returns an opaque 400 Bad Request for those credentials. Current provider error handling treats generic 400 as caller/request invalid, so it does not disable the credential, increment failure count, or retry another healthy local credential.

2026-08-01 scoped release gate: this fix is included in [Final release gate - 2026-08-01](../evidence/final-release-gate-20260801.md). Full Rust default/no-default all-target tests, release build, UI/admin-ui build, Node contracts, real Claude CLI fake-upstream suite, feature docs, diff hygiene, fmt, and artifact inventory passed for the current batch. Production recurrence remains open after rollout.

## Symptom / impact

Users see a client-facing 400 `invalid_request_error` that says the request body is invalid. Operationally, every request scheduled to the exhausted old local credentials fails immediately and does not automatically retry a healthy newly added local credential or external pool route.

The symptom persists across service restart because the persisted scheduler startup state still says the old credentials are enabled and have no runtime failure count. Only manual disable changes the scheduler-visible state today.

## Production evidence

Local evidence package:

- `tmp/prod-evidence/20260731-221200-ha-invalid-request-400/`
- Default archive excludes raw evidence: `tmp/prod-evidence/20260731-221200-ha-invalid-request-400/20260731-221200-ha-invalid-request-400-redacted.tar.gz`

Affected hosts and primary old credentials:

| Host | Old failing credentials | Evidence |
| --- | --- | --- |
| 152.53.243.159 | 1141, 1142 | hundreds of matching local 400s; both had `remaining=0`, `credit_remaining=0`, `overage_status=DISABLED` |
| 152.53.194.142 | 947, 948 | hundreds of matching local 400s; both remained `disabled=false`, `failure_count=0` |
| 152.53.194.170 | 623, 624 | hundreds of matching local 400s; both remained `disabled=false`, `failure_count=0` |

New local credentials added around the same time succeeded on the same route/model families, and external pool paths also succeeded. This isolates the failure to old exhausted local credentials, not the request payload itself.

Exact copied request:

- request id: `req_01mn8ETfbu1ziEEwhqSWMrfv`
- route: `/ha/v1/messages`
- route subtype: `local_error_no_fallback`
- credential: 1141
- upstream status: 400
- diagnostic: `upstream_failure class=invalid_request upstream_status=400 public_status=400 body_bytes=146 retry_after_secs=unknown content_type=other reason=bad_request`
- payload guard: `finalBytes=64391`, no dropped images, no material tool/history cleanup

## Root cause

The state shown in the account usage page and the state used by the scheduler are separate:

- `credential_account_info` stores display/probe account balance fields such as `remaining`, `credit_remaining`, `usage_percentage`, and `overage_status`.
- scheduler startup/reload loads `credentials` and `credential_runtime_state`, not `credential_account_info`.
- `CredentialEntry` does not carry account balance/overage state.
- `credential_is_dispatchable` checks disabled state, model support, proxy availability, cooldown, RPM, and concurrency only.

Therefore a credential can be visibly exhausted in the UI while still enabled and dispatchable after a process restart.

Generic Kiro API 400 handling then makes the incident persistent:

- quota disable/failover is only triggered for `status == 402` plus quota markers such as `MONTHLY_REQUEST_COUNT` or `OVERAGE_REQUEST_LIMIT_EXCEEDED`;
- generic 400 is classified as `bad_request`;
- the generic 400 path returns the error without `report_quota_exhausted_deferred`, without `report_failure_deferred`, and without retrying another local credential.

## Reproduction

Minimal local reproduction shape:

1. Create two local API-key credentials.
2. Persist account info for the first credential with `remaining=0`, `credit_remaining=0`, `usage_percentage=100`, and `overage_status=DISABLED`.
3. Keep the first credential enabled in `credentials` and with `failure_count=0` in `credential_runtime_state`.
4. Keep the second credential healthy.
5. Start or reload the service.
6. Mock the first credential's upstream request to return an opaque 400 that classifies as `bad_request`.

Expected current behavior:

- the first credential remains dispatchable after restart;
- the request fails with public `invalid_request_error`;
- no automatic disable or failure-count increment happens;
- the same request does not retry the healthy second local credential for the generic 400 path.

## Selected fix / solution

The durable fix needs both scheduler and provider changes:

- scheduler admission must consult fresh enough account-info state, or a derived runtime quarantine state, before dispatching local API-key credentials;
- an account-info row with no remaining credit and overage disabled must become non-dispatchable until refresh/reset/manual enable proves it is usable again;
- provider 400 handling must recognize known Kiro account-state/quota fingerprints when account-info already proves exhaustion;
- generic request-body 400s must still fail fast without broad retry to avoid amplifying real malformed requests.

## Implemented in this change

Usage detail modals now surface real diagnostic fields more prominently:

- new UI: `ui/src/features/usage/usage-detail-modal.tsx`
- old admin UI: `admin-ui/src/components/usage-records-panel.tsx`

The modal now shows a dedicated "upstream / processing error" section composed from:

- `errorDetail` or `errorMessage`;
- local credential attempt `errorMessage` / `errorType`;
- external pool attempt `errorMessage` / `errorType`;
- `errorMetadata`;
- public normalized error as a separate, lower-priority item.

Attempt-table error cells were also changed from narrow truncated text to wrapping text so upstream diagnostics are visible without relying only on hover title text.

Scheduler admission now has a freshness-bounded account-info quota guard:

- `PostgresStore::load_credentials_with_runtime_state_and_account_info()` loads `credentials`, `credential_runtime_state`, and `credential_account_info` from one repeatable-read read-only transaction.
- `main.rs` startup and `MultiTokenManager::reload_credentials_from_postgres()` now use that joint snapshot.
- `CredentialEntry` carries a derived `account_quota_blocked` flag and diagnostic `account_quota_block_reason`. This flag is not persisted and does not overwrite Admin/runtime `disabled`, `disabledReason`, failure counts, or generation.
- The guard applies only to API-key credentials when account info is fresh, `remaining <= 0`, `credit_remaining <= 0`, and `overage_status` is case-insensitive `DISABLED`.
- Missing, stale, malformed, non-disabled-overage, or OAuth account-info snapshots do not block dispatch.
- `credential_is_usable_for_model()`, all dispatchability helpers built on it, startup current credential selection, reload selection, `switch_to_next()`, and automatic terminal-failure fallback checks now exclude quota-guarded credentials.
- Kiro API and MCP 400 handling still fail fast for generic request/tool/image/malformed errors, but if the selected credential is already quota-guarded and the 400 is opaque `bad_request` or `request_body_invalid`, that credential is treated as quota-exhausted and local retry may continue to a genuinely available alternate.

The selected policy is intentionally a derived scheduler guard rather than a persisted runtime disable. A stale usage snapshot must not permanently strand a credential; a fresh account-info refresh that shows remaining credit or overage enabled clears the guard on reload/startup.

## Remaining gates

- Bind this code to a frozen release binary and run the broader scheduler/runtime release matrix.
- Run an isolated service/fake-upstream startup/reload smoke that proves an exhausted account-info row is skipped before any upstream hit. The narrower manager reload path has already passed against a real PgSQL test schema.
- Confirm production recurrence after rollout on the three affected hosts.
- Preserve bounded, redacted upstream error diagnostics for future opaque 400 samples where safe.

## Validation

Focused evidence:

- [Local credential quota guard focused validation - 2026-08-01](../evidence/local-credential-quota-guard-20260801.md)

Commands run locally:

```bash
cd ui && npm run check
cd admin-ui && npm run build
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

All listed commands passed in the local workspace. The PgSQL reload regression used `PostgresStore::connect_test`, which creates a random isolated test schema and drops it after the run. The `token_manager` suite covered `308` tests before the reload regression was added; focused reload coverage adds `1 passed / 0 failed`. The broad Rust run covered main `1843 passed / 0 failed / 6 ignored` plus `kiro_loadtest` `31 passed / 0 failed`; Node feature tests covered `283 tests / 261 pass / 22 skipped / 0 fail`.

## Residual risk

The exact 146-byte upstream body for the production copied request was not preserved in `usage_records`. The body content is inferred from persisted status/body-size/content-kind/reason and from account state. Future fixes should preserve bounded, redacted upstream error diagnostics where safe.
