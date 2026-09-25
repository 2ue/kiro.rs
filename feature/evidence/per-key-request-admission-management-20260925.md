# Per-Key Request Admission Management Verification

Date: 2026-09-25

Status: Local implementation and verification passed; `v0.0.172` source/tag pushed; Docker image publication workflow pending.

Related plan: [Per-key request admission management](../../docs/plantree/plans/rust-runtime-scheduler-stabilization/topics/per-key-request-admission-management.md)

## Contract Delivered

- Added optional `requestApiKeyPolicies`; existing `apiKey`, `apiKeys`, key order,
  and Admin response field `requestApiKey` remain compatible.
- Unmanaged legacy keys keep inheriting global `requestAdmission`. Creating a
  managed key snapshots current global admission values unless explicit values
  are supplied.
- Each key has digest-keyed RPM, active concurrency, queue, timeout, and rejection
  state. A key's queue-full response is bounded `429`; it does not trigger an
  external-pool fallback.
- Default and per-key settings publish as one admission-policy snapshot. A live
  update wakes waiters to re-read policy. Existing response-body permits are
  not revoked when a limit is lowered or a key is disabled.
- Database config with existing request keys does not re-import stale file
  policies. Removing a managed key therefore cannot be undone by a stale
  `requestApiKeyPolicies` entry in the config file after restart.
- Main UI uses the existing standalone `/security` page route for the request
  Key list. `admin-ui` keeps its prior access-key section. New and older
  `requestApiKey`-only response shapes are both handled.
- Key material is not added to generic runtime-config responses. Local key
  generation requires Web Crypto; there is no `Math.random()` fallback.

## Concurrency Boundary

Limits are per process and per request API Key, not a distributed cluster quota
and not a limit on a local upstream account. With a per-key limit of five, each
serving instance admits at most five active requests for that key; `N` instances
can therefore admit up to `N * 5` in aggregate. Local-account and external-pool
dispatch capacity/queues remain shared after HTTP admission. Per-key route binding
and per-route local/external scheduler domains are not implemented here.

## Verification

All Cargo commands ran through `feature/tests/run-cargo-scoped.sh` with Rust
1.92.0. The first default full-suite run had one unrelated external-pool usage
projection assertion fail; that test passed alone, and the subsequent complete
default suite passed.

- `cargo fmt --all -- --check`: passed.
- Default `cargo test --locked`: main binary `2179 passed / 0 failed / 6 ignored`;
  `kiro_loadtest` `31 passed / 0 failed`.
- `cargo test --locked --no-default-features -- --test-threads=1`: passed. The
  checked-in Clippy/check/release steps ran afterward in the same `set -e` batch.
- Focused per-key tests after adding explicit multi-instance scope coverage:
  `5 passed / 0 failed`, including Key A queue saturation while Key B completes,
  live RPM/concurrency update, hard cap of five rejecting the sixth, independent
  two-instance accounting, and atomic default/override snapshot behavior.
- Admin/config tests in the full suite cover legacy response field preservation,
  legacy config merge, removed-key non-resurrection, policy validation, and
  disabled-key authentication behavior.
- CI Clippy baseline: `764` warnings; checked-in ceiling `849`; pass.
- `cargo check --all-targets --locked`: passed with no new dead-code warnings.
- Release build: `kiro-rs` and `kiro_loadtest` both built successfully.
- `ui`: `pnpm run build` passed. Existing large-chunk advisory remains.
- `admin-ui`: `pnpm run build` passed.
- Frozen fake-upstream smoke: `12/12` streaming requests succeeded, status `200`,
  concurrency `3`; TTFB p95 `2 ms`, total latency p95 `2 ms`. This exercises the
  loadtest runner and fake server, not the live account scheduler.
- `git diff --check`: passed.
- Build artifact inventory: `targets=0 reservations=0 target_processes=0 blockers=0`.

Frozen binary hashes:

```text
kiro-rs       dbf9a2295dd07d723cb8ffffd1d1a0bf2fd249d960fcd85498804727918ec792
kiro_loadtest 0f08e458f75ec91b48d064c102b400b3e3b7c415cc5a305367041dcb50d4c7cf
```

The final release binaries were rebuilt from the version-bumped source using
`cargo build --release --locked --bins`; the service binary reported
`kiro-rs 0.0.172`. The test-only additions are `#[cfg(test)]` and do not alter
the production source behavior.

## Additional Local Mock Retest

After the request for local mock coverage, the focused key-admission tests were
rerun on 2026-09-25 using the scoped Cargo runner. These tests did not connect to
`127.0.0.1:19023`, use real account credentials, or contact an upstream.

- `cargo test --locked anthropic::request_admission::tests -- --test-threads=1`:
  `27 passed / 0 failed`. The tests exercise the actual Axum admission middleware
  through an in-process `Router::oneshot` HTTP mock, with synthetic request-key
  identities and a counted mock handler.
- The mock matrix covers cross-key isolation while one key is saturated, 429
  envelope/headers and zero rejected-handler hits, five-round FIFO/cancel/timeout
  and recovery, live policy changes, per-instance limits, and permit release at
  response-body EOF, body error, and client drop.
- `common::auth::tests`: `4 passed / 0 failed`; the Admin legacy response field
  test: `1 passed / 0 failed`; the `request_api_key` filter: `8 passed / 0 failed`;
  stale-file policy non-resurrection: `1 passed / 0 failed`. These focused filters
  overlap and must not be added together as a unique-test count.
- `node --test feature/tests/request-api-key-admission-multi-instance.contract.test.mjs`:
  `5 passed / 0 failed`. This validates runner input guards only; it is not a
  runtime multi-instance or database load result.
- The latest local rerun completed in 2m 09s with all 27 admission tests passing.
  The scoped runner removed its owned target and released its reservation.
- Final post-test artifact inventory:
  `targets=0 reservations=0 target_processes=0 blockers=0`; process inspection
  completed and the release gate passed.
- `git diff --check` and relative Markdown link checks for the three changed
  evidence/status documents passed. The repository-wide
  `node feature/tests/check-feature-docs.mjs` check still reports 20 unrelated
  findings under unchanged `feature/issues/` documents (missing archived
  `tmp/prod-evidence` links and issue-contract fields); none are in the changed
  documents.

`Router::oneshot` verifies routing and middleware behavior without opening a TCP
listener. It does not claim real socket-level disconnect behavior, real upstream
behavior, cross-instance shared counters, or local/external scheduler isolation.

## Runtime Safety

No request was sent to the configured `127.0.0.1:19023` service because it was
running with operator-provided account configuration. Its PID and repository
ownership were checked and it was left untouched. The fake loadtest server used
`127.0.0.1:19080` and exited with the command. No real upstream account or
production endpoint was used.

## Release

Release model: `rust-crate`; version authority: root `Cargo.toml` package.

- Remote stable tag used as the base: `v0.0.171`. The `0.0.171` GHCR manifest
  was present with `amd64` and `arm64`; Docker Hub could not be checked from this
  environment because registry authentication was unavailable.
- Work commit: `f12dc919ea737c5a922dd741267cde5f64b02dc4`
  (`feat: add independent request API key admission`).
- Release bump commit: `9021f7c8ce6c68da1ca62dfe395978b65fa14767`
  (`chore(release): 0.0.172`).
- New package version and tag: `0.0.172` / `v0.0.172`.
- Annotated tag object: `e9fb0d00b5650e4447dbbc85c739413ae9815938`.
- Peeled tag commit: `9021f7c8ce6c68da1ca62dfe395978b65fa14767`.
- Branch push and tag push both succeeded. A subsequent remote query confirmed
  `origin/main` and the peeled tag both point to the release bump commit.
- Local release binary build passed, and the service binary reports
  `kiro-rs 0.0.172`; post-build inventory reported
  `targets=0 reservations=0 target_processes=0 blockers=0`.
- GitHub Actions `Publish Docker Images #228`
  (run `36110326384`) was still `In progress` at the time of this record. The
  new GHCR tag `ghcr.io/2ue/kiro-rs:0.0.172` returned `manifest unknown`, so
  container publication is not claimed as complete yet. Production deployment
  is also unverified. The closeout recheck could not refresh the run state:
  `gh` is not installed and the unauthenticated Actions API returned HTTP 403.
  Keep the workflow and image publication status pending until an authenticated
  status check confirms completion.

Remote refs observed:

```text
9021f7c8ce6c68da1ca62dfe395978b65fa14767 refs/heads/main
e9fb0d00b5650e4447dbbc85c739413ae9815938 refs/tags/v0.0.172
9021f7c8ce6c68da1ca62dfe395978b65fa14767 refs/tags/v0.0.172^{}
```
