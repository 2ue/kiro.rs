# Per-Key Request Admission Management Verification

Date: 2026-09-25

Status: Local implementation and verification passed; release publication pending.

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
kiro-rs       ce2fddb09ede2215d2aa9c13334bb908a73880affbea15407e3bb9b1ff5f690c
kiro_loadtest 89a0a94057d23ede60e1253c05e553dc3b1d1a72ad6966b972e29d5632b77511
```

The final focused test added after the frozen release build is `#[cfg(test)]`
only; production source and release-binary behavior did not change afterward.

## Runtime Safety

No request was sent to the configured `127.0.0.1:19023` service because it was
running with operator-provided account configuration. Its PID and repository
ownership were checked and it was left untouched. The fake loadtest server used
`127.0.0.1:19080` and exited with the command. No real upstream account or
production endpoint was used.

## Release

Publication is pending. The release version and tag will be recorded after the
remote branch/tag state is fetched and the safe push sequence completes.
