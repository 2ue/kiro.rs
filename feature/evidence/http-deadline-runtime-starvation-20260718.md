# HTTP Deadline Runtime Starvation Evidence 2026-07-18

Status: `focused-pass / frozen-load-pending`

Source: HEAD `401473ca1649997bdeccf4468e3add1bdb187248` (`v0.0.109`) plus the current unreleased dirty tree.

Related issue: [Upstream HTTP Deadline Under Runtime Starvation](../issues/upstream-http-deadline-runtime-starvation.md)

Toolchain: Rust `1.92.0`. Every Cargo command used `feature/tests/run-cargo-scoped.sh`. No Docker validation ran, no request targeted `127.0.0.1:9022`, and no `kiro_idc_users*.txt` file was read or staged.

## Red Evidence

`full-unit-current-r9` ran all 1712 then-current tests and exited 101 after 497 seconds. Default harness output exceeded the tool budget and hid the failure summary, so this run is retained only as an undiagnosed red run. Its target was `1676240 KiB` and ended `removed=true / reservation_released=true`.

`full-unit-current-r10` reran the same tree in quiet mode and produced the accepted failure identity:

```text
provider_transport_and_body_fault_matrix_is_private_typed_and_bounded
scenario=provider_header_timeout stream=false pool=20 round=4
attempt durations/statuses:
  1002ms / None / upstream_timeout
  1003ms / None / upstream_timeout
  1001ms / None / upstream_timeout
  1651ms / 500 / server_error
test result: 1705 passed / 1 failed / 6 ignored
```

The fake provider deliberately returns HTTP 500 after 1.5 seconds; the configured response-header timeout is 1 second. Accepting the fourth response proves the deadline was not strict under the complete tree's executor pressure. The run ended after 495.4 seconds; its `1677828 KiB` target and reservation were removed.

Source inspection of Tokio 1.48 confirmed that `Timeout::poll` polls the wrapped value before the delay. Merely increasing the fixture delay would remove the observed race without correcting the production helper and was not accepted.

## Repair

`src/http_client.rs` now uses a shared monotonic deadline helper with biased timer-first selection for response headers and response bodies. If the timer and HTTP future are simultaneously ready, timeout wins. If the HTTP future is ready before the deadline, it still wins normally. Zero disables the stage timeout exactly as before.

Two deterministic tests cover elapsed/future-ready and future-deadline/future-ready order for five rounds each. Existing loopback tests retain real header and body stalls. The production provider retry budget, classification and cooldown logic were not changed.

The first combined focused scope, `http-deadline-provider-r1`, failed at test compile because the test module's `std::time::Instant` shadowed `tokio::time::Instant`. This was a test-type error, not behavioral evidence. The `1115544 KiB` partial target was removed and its reservation released. Both test deadlines were then made explicitly Tokio instants.

## Focused Green

`http-deadline-provider-r2` reused one scoped build for four commands:

```text
deadline_first_timeout_*: 2/2 outer tests, 10/10 internal rounds
send_with_response_header_timeout_expires_before_response_headers: 1/1, 1.00s
response_text_with_body_timeout_expires_after_response_headers: 1/1, 1.01s
provider_transport_and_body_fault_matrix_is_private_typed_and_bounded: 1/1, 245.74s
```

The provider matrix retained all `6 fault classes x stream/non-stream x pool 1/20/60 x 5 rounds`, all 180 outcomes, all 540 expected budgeted sends, privacy marker scans, typed status/class and cooldown assertions. Its earlier isolated baseline was 243.67 seconds; the 2.07-second difference is about 0.85% and does not show a material focused-test regression.

The scope used `1673372 KiB` and ended with `removed=true / reservation_released=true`.

## Complete Tree And Warning Gate

Before the deadline repair, warning cleanup exposed 24 production-bin warnings. Proven test-only reference paths were excluded from the production build, redundant external normalized-body bytes/serialization were removed, and the final Rust 1.92.0 `cargo check --all-targets` completed with zero warning:

```text
scope=warning-cleanup-check-r3
size_kib=446940
removed=true
reservation_released=true
```

After the focused repair, `full-unit-current-r11` completed:

```text
running 1714 tests
1708 passed
0 failed
6 ignored
test time=367.77s
wall time=644.5s
size_kib=1676260
removed=true
reservation_released=true
```

After queue/storage/provider fixtures were added, the current `full-unit-current-r12` also passed: `1715 passed / 0 failed / 6 ignored`, test `351.96s`, wall `581.7s`, `size_kib=1682460`, `removed=true`, `reservation_released=true`. The real-storage fixture bodies remain separately gated when URLs are absent.

The six ignores remain explicit isolated release/performance probes. No scoped validation target from these runs remains.

## Evidence Boundary

This evidence proves deadline-first behavior in deterministic unit tests, real loopback header/body stalls, the complete provider fault matrix and the current complete default-bin unit tree. It does not prove final release behavior under CPU starvation, real upstream networks, long Claude CLI sessions, 500-concurrency/low-RPM queue pressure, or C1-C4/L1-L5. Those remain release blockers and must bind to the frozen binary SHA.
