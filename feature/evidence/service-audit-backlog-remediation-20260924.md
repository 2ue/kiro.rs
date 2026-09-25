# Service Audit And Backlog Remediation Evidence

- Evidence date: 2026-09-25
- Source revision: working tree based on `main`, Cargo version `0.0.170`
- Scope: audit items 1-6 and 8; item 7 intentionally excluded; backlog causality remediation
- Safety: no production load; Redis checks used repository-owned loopback namespaces with
  generated test prefixes; runtime ownership for `127.0.0.1:19023` was verified before any
  dynamic case.
- Frozen candidate binaries:
  - `kiro-rs` SHA-256
    `02eb56dc589980d4af39febae084719e95a35c3f73f87085634982891f227184`
  - `kiro_loadtest` SHA-256
    `daab1e8cdc29c39f556a9edae91bf4283a0a352dc80a08b0831fed7d400301cf`

## Implemented changes

- Shared `AdminApiKeyStore` is used by `AdminState` and Redis runtime-config reloads.
- Admin authentication uses `parking_lot::RwLock` and rejects empty presented/configured keys.
- Redis dispatch queue lease acquisition awaits the existing scheduler Redis breaker path;
  queue counts, commit-unknown reconciliation, TTL policy and client-visible errors are
  unchanged.
- Usage latency traces add optional `credentialDispatchElapsedMs` and
  `upstreamHeaderWaitMs`; historical `upstreamHeaderMs` remains unchanged.
- `.DS_Store` is ignored prospectively; existing files and `.kilo` were preserved.

The implementation deliberately does not add a total stream wall-clock limit, move RPM
reservation, change queue capacity/FIFO/timeout, change retry boundaries, or cancel a
stream that continues to produce valid data. Those are explicit non-goals because the
backlog optimization must not solve capacity pressure by terminating otherwise-valid
requests.

## Verification completed

| Command/case | Result |
| --- | --- |
| `feature/tests/run-cargo-scoped.sh service-audit-check -- cargo check --all-targets --locked` | pass |
| scoped `cargo fmt --all` and subsequent format check | pass |
| scoped `cargo test --locked latency -- --nocapture --test-threads=1` | 12 passed, 0 failed; 5 Redis/Toxiproxy cases skipped by missing optional env |
| scoped `cargo test --locked admin_auth -- --nocapture --test-threads=1` | 1 passed, 0 failed |
| scoped `cargo test --locked cloned_admin_key_store -- --nocapture --test-threads=1` | 1 passed, 0 failed |
| Redis `redis_dispatch_queue_cancelled_waiter_releases_local_and_remote_lease` | 1 passed, 0 failed |
| Redis `finite_redis_dispatch_queue_lease_deadline_does_not_move_after_renew_interval` | 1 passed, 0 failed |
| default-feature all-target Rust tests | 2,170 passed, 0 failed, 6 ignored |
| no-default-feature all-target Rust tests | 2,170 passed, 0 failed, 6 ignored |
| `kiro_loadtest` all-target tests (default and no-default batches) | 31 passed, 0 failed per batch |
| release build (`kiro-rs` and `kiro_loadtest`) | pass |
| Clippy baseline | 764 warnings, within the allowed 849 |
| `cargo fmt --all -- --check` | pass |
| `git diff --check` | pass |
| `node feature/tests/inventory-build-artifacts.mjs --gate` | pass: `targets=0`, `reservations=0`, `target_processes=0`, `blockers=0` |
| frozen L3 fake-upstream matrix | 9/9 scenarios passed; `cleanupError=null` |
| repository-wide `node feature/tests/check-feature-docs.mjs` | not clean: 20 pre-existing violations across 79 legacy issue documents; no listed document was modified by this work |

All Cargo commands above were run through `feature/tests/run-cargo-scoped.sh`; each
scoped target was removed and its reservation released.

The repository-wide feature-document contract result is recorded as a separate
pre-existing hygiene gap, not as a failure of the service-audit implementation. The
reported violations are missing historical temporary evidence links and missing contract
headings in older issue files; the current diff does not touch those files.

## Runtime ownership preflight

- PID `91239` listening on `127.0.0.1:19023` was a `kiro-rs` binary under this repository's
  `tmp/thinking-budget-local/runtime-cli-real-20260924-224113/` directory.
- Its cwd was `/Users/yuanfeijie/Desktop/procode/2ue_kiro.rs` and command line used
  `-c tmp/thinking-budget-local/config.json`.
- The first probe used `/health` and returned 404; source inspection showed the readiness
  endpoint is `/readyz`. The corrected probe returned
  `{"status":"ready","checks":{"postgres":true,"redis":true,"redisRuntimeEvents":true}}`.
- Redis listener PID `40664` was a Colima SSH forward on `26379`; no foreign service was
  targeted by the focused Redis tests.

## Backlog validation

The dedicated runner is
[`feature/tests/service-audit-backlog-http.mjs`](../tests/service-audit-backlog-http.mjs).
It uses one caller-owned PostgreSQL database, one isolated Redis database/prefix, one fake
HTTP upstream, one credential slot and one global dispatch slot. The fake upstream holds a
stream open while continuing to emit valid chunks, then allows capacity to release. The
runner records offered/queued/sent/completed/cancelled behavior and redacts request bodies
and credentials from its report.

Three independent fresh-database runs were completed:

| Run | Long-stream result | Queued normal requests | Cancelled queued request | Recovery request | Cleanup |
| --- | --- | --- | --- | --- | --- |
| `run8` | 47 upstream chunks; no early cutoff | both returned HTTP 200 after capacity release | `AbortError`; no fake-upstream hit | HTTP 200 | Redis `scanned=12`, `removed=12`, `remaining=0`; 4 successful usage rows |
| `run9` | 47 upstream chunks; no early cutoff | both returned HTTP 200 after capacity release | `AbortError`; no fake-upstream hit | HTTP 200 | Redis `scanned=12`, `removed=12`, `remaining=0`; 4 successful usage rows |
| `run10` | 47 upstream chunks; no early cutoff | both returned HTTP 200 after capacity release | `AbortError`; no fake-upstream hit | HTTP 200 | Redis `scanned=12`, `removed=12`, `remaining=0`; 4 successful usage rows |

The low-capacity configuration for each run was:

```text
credentialMaxConcurrentRequests = 1
dispatchGlobalMaxConcurrentRequests = 1
dispatchMaxQueuedRequests = 8
credentialDispatchMaxWaitSecs = 15
streamIdleTimeoutSecs = 8
```

The three runs produced the expected latency separation:

```text
run8:  credentialDispatchElapsedMs 3..8772, upstreamHeaderWaitMs 1..4
run9:  credentialDispatchElapsedMs 3..8764, upstreamHeaderWaitMs 1..2
run10: credentialDispatchElapsedMs 3..8759, upstreamHeaderWaitMs 1..2
```

The historical `upstreamHeaderMs` field was still present and retained its previous
semantics. The evidence supports the following bounded conclusion:

1. A valid long stream occupies the credential and global dispatch slots while it emits
   data.
2. Normal requests wait when those slots are full, then complete when capacity naturally
   releases.
3. Cancelling one queued request releases only that request; it does not cancel or starve
   the other queued requests.
4. The optimization does not terminate the valid long stream to manufacture capacity.
5. Credential-dispatch wait and upstream-header wait are observably different phases.

This reproduces the source-level causal chain with a fake HTTP upstream. It does **not**
prove that a particular production incident was caused by long upstream streams, because
the production configuration and request timeline were not supplied.

## Harness lessons and corrected reruns

The first low-capacity attempts were not product failures:

- One attempt locked a `ReadableStream` in the test assertion before the request body
  consumer could read it. The runner was corrected to consume the body exactly once.
- Later attempts reused a prior PostgreSQL database whose persisted runtime configuration
  still pointed at an earlier fake-upstream URL. Those databases were abandoned rather
  than mutated; all final runs used new caller-owned databases (`run8`, `run9`, `run10`).

The runner contract now requires a fresh database for every run so persisted runtime
configuration cannot contaminate the fake-upstream target.

## Remaining evidence boundary

- Production `credential_dispatch_max_wait_secs`, queue capacities, weighted-capacity
  setting, active account/lease snapshot and request timestamps were not supplied.
- No production timeline currently separates receive, credential-selected, upstream-send,
  response-headers, first-chunk, terminal and cancellation events.
- Therefore the local result proves the mechanism and protects normal requests, but does
  not attribute the observed production hundreds-of-seconds incident to one specific cause.
  A read-only, redacted production evidence package is required before changing dispatch
  wait policy, capacity sizing or stream duration policy.

No service, fake upstream, load process or Redis key namespace from this evidence remains
running after the final cleanup checks.
