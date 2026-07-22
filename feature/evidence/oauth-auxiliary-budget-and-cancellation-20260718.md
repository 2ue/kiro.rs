# OAuth Auxiliary Budget And Cancellation Evidence

Status: `process-local-focused-pass / cluster-provider-load-pending / NO-GO`

Date: 2026-07-18

Source identity: HEAD `401473ca1649997bdeccf4468e3add1bdb187248` plus the dirty working-tree changes described below; no frozen candidate binary was produced.

Issue authorities: [Retry Budget, Admission, And RPM Amplification](../issues/retry-budget-admission-and-rpm-amplification.md) and [Token Refresh Failure Wave And Cluster RPM](../issues/token-refresh-failure-wave-and-cluster-rpm.md)

## Evidence Boundary

This record covers process-local OAuth auxiliary attempt limits, refresh concurrency, same-credential failure singleflight, cancellation cleanup, default refresh admission, and fake-HTTP recovery. It does not cover live Redis coordination, two replicas, PostgreSQL refresh CAS, real Claude Code traffic, frozen release performance, or L3-L5 resource gates.

No production service, credential file, OAuth secret, bearer/refresh token, request body, response body, or protected port was accessed. `127.0.0.1:9022` was not contacted. All HTTP traffic used ephemeral loopback fake servers.

## Red Findings

| Batch | Red result | Root cause | Disposition |
| --- | --- | --- | --- |
| `auxiliary-rpm-focus-r1` | 21/22 passed; `auxiliary_focus_cancelled_refresh_releases_process_permit_for_five_rounds` failed in round 1 with `in_flight=1` after the request task had been aborted and joined | `refresh_token_until` placed the permit-owning HTTP future in an unjoined Tokio child. Parent cancellation called `abort()` but did not wait for the child future to be destroyed, so the shared permit could remain occupied after the caller had ended | Production path fixed; post-fix focused batches passed |
| `oauth-shared-burst-r1` | First recovery wave failed with process-local refresh rate limiting | The concurrency test used the production default `60 RPM / burst 8` and then incorrectly required 16 immediate recovery refreshes after two failure sends | Fixture split: concurrency matrix gets an explicit high test-only bucket; the production 60/8 contract remains a separate exact test |
| `oauth-shared-burst-r2` | Header-timeout recovery exhausted a caller's two-attempt budget | The timeout fake endpoint retained a test-only 25ms response-header deadline after its server state switched to Success | Added explicit failure-wave connection cleanup evidence and corrected the test-only header timeout to 250ms; production timeouts are unchanged |
| `oauth-shared-burst-r3` | Timeout c8 recovery produced 23 sends for 16 callers | Same persistent 25ms test marker; the new diagnostics proved manager permits were already zero and old fake-server connections were already idle | Fixture correction confirmed by the full r4 matrix |

Every red batch exited through the scoped wrapper with `removed=true` and `reservation_released=true`; none of its roughly 1.64 GiB target survived the batch.

## Production Repair

`run_refresh_step_until` now polls the permit-owning refresh future in the request task under `tokio::time::timeout_at`. Request cancellation or timeout therefore drops the HTTP future and `AuxiliaryConcurrencyPermit` before the parent future completes. Panic normalization is retained with `AssertUnwindSafe(...).catch_unwind()`.

The removed child-task boundary was no longer needed to isolate synchronous client construction: `AuxiliaryRuntime::refresh_client` already performs `build_client` through `spawn_blocking`. The ready-token/API-key path, request body, Redis calls, and ordinary inference path are unchanged. The repair adds no Redis RTT, queue, timer task, request-body copy, or new operator setting.

The regression `auxiliary_focus_refresh_step_deadline_drops_future_owned_resources_before_returning` runs five rounds and asserts that timeout returns a typed failure only after a future-owned drop probe has fired. The original cancellation test independently aborts a live refresh and requires the process permit to be zero immediately after join.

## Fixture Corrections

- `fake_refresh_manager_with_all_limits` lets the process-concurrency matrix use `6000 RPM / burst 256`, which prevents the refresh token bucket from masking the concurrency controller. This value exists only in the test manager.
- `token_refresh_process_local_burst_is_hard_bounded_for_five_rounds` still constructs the production default `60/8` controller and strictly requires `8 admitted / 120 rejected` in each of five rounds.
- The fake response-header timeout marker is test-only and now uses 250ms. It still deterministically times out a server that sends no headers, but no longer classifies a concurrent loopback Success response as an artificial timeout. Production refresh response deadlines remain 60/90-second workflow limits.
- Before each shared-manager recovery phase, the matrix requires process auxiliary `in_flight=0` and all fake failure-wave connections to reach idle within two seconds. This is a test phase boundary, not a production sleep.

## Accepted Executed Results

| Scope | Dynamic coverage | Result | Cleanup |
| --- | --- | --- | --- |
| `auxiliary-rpm-focus-r3` | 23 top-level tests; handler/public errors, request auxiliary ledger, websearch typed mapping, profile/model discovery, client cache, concurrency, cancel, external credential refresh, 3/20/60-account refresh fixtures; every named scenario has five internal rounds | 23/23 passed, 0 ignored; 406.89s dynamic | `size_kib=1679436`, `removed=true`, `reservation_released=true` |
| `oauth-shared-burst-r4` | 128 expired credentials; 12 OAuth failures; c1/c8/c32; five rounds per cell; 180 failure cells plus 180 16-caller recovery cells | Passed; per-request sends <=2, process peak <=16, disabled=0, refresh failure count=0, recovery 16/16 | `size_kib=1678708`, `removed=true`, `reservation_released=true` |
| `oauth-independent-burst-r1` | 20 expired credentials in independent managers; 12 failures; c1/c8/c32; five rounds per cell; 180 failure cells and 16-way recovery | Passed; hits exactly bounded by c x 2 (2/16/64), independent of pool size; disabled=0; recovery succeeded | `size_kib=1678716`, `removed=true`, `reservation_released=true` |
| `oauth-singleflight-wave-r1` | One credential; 32 callers; 500/header-timeout/disconnect/malformed; five rounds per class | Passed; one HTTP leader, 31 typed followers, aggregate auxiliary consumption 1, connection/permit cleanup | `size_kib=1679076`, `removed=true`, `reservation_released=true` |
| `token-refresh-default-admission-r1` | Process-local 60 RPM / burst 8; 128 immediate reservations; five rounds | Passed; every round exactly 8 admitted / 120 rejected with process-local authority | `size_kib=1678732`, `removed=true`, `reservation_released=true` |
| `auxiliary-rpm-check-r1` | Rust 1.92.0 `cargo check --locked --all-targets` | Passed; production and test conditional compilation checked | `size_kib=447728`, `removed=true`, `reservation_released=true` |

`cargo fmt --all -- --check` and `git diff --check` also passed. The loadtest binary reported zero matching tests under name filters; those lines count only as target compilation, never as dynamic PASS.

## What This Proves

- One downstream request cannot scan a 20- or 60-account expired pool and emit refresh HTTP proportional to pool size; the request auxiliary budget is authoritative before each real refresh send.
- Same-credential transient waiters share one typed failure wave instead of serializing 32 OAuth sends.
- A single process has a hard auxiliary concurrency limit, and cancellation/timeout no longer leaves its permit occupied after the request future has ended.
- Budget/concurrency/admission rejection remains health-neutral; the tested transient/auth/rate-limit classes do not persistently disable credentials.
- Production default refresh admission remains 60 RPM with burst 8 and does not use the dispatch queue or pretend Redis authority exists when Redis is absent.

## Remaining Release Blockers

- The requested 1/20/60 provider-level Messages matrix is only partially covered here: 20-account and 128-account manager HTTP matrices passed, while a frozen binary through real handler/provider routing, usage persistence, downstream API-key attribution, and 1/60 exact provider cells remain open.
- Live Redis leader/wait/replay, aggregate 60/8 across two replicas, health-claim cancellation, Redis slow/disconnect/restart, and process-local versus Redis-global recovery remain open.
- PostgreSQL old-access-token CAS, non-rotating refresh-token fencing, revision-only same-token handling, and leader pre-send authority checks remain open.
- Real API/MCP invalid-bearer c1/c8/c32, client disconnect during committed OAuth send, external fallback behavior, and public error/usage attribution remain open.
- Frozen release L3-L5 must still record TTFB/latency percentiles, OAuth/inference/downstream hits, RSS, FD, tasks, sockets, queues, recovery, and idle return. Debug unit-test elapsed time is not a production performance result.

The release decision remains `NO-GO`.
