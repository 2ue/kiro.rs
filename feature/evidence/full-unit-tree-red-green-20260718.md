# Full Unit Tree Red/Green Record 2026-07-18

Status: `current dirty-tree default-bin unit pass / broader release gates pending`

Source: HEAD `401473ca1649997bdeccf4468e3add1bdb187248` (`v0.0.109`) plus the unreleased working-tree changes described below.

Toolchain: Rust `1.92.0`. Every Cargo command ran through `feature/tests/run-cargo-scoped.sh`; no Docker validation ran, no request targeted `127.0.0.1:9022`, and no `kiro_idc_users*.txt` file was read or staged.

## Scope

This record covers the current default binary unit-test tree, `cargo check --all-targets`, and the focused repairs needed to make those gates deterministic and warning-free. It does not cover `cargo test --all-targets`, `--no-default-features`, real PostgreSQL/Redis, a frozen release service, real Claude Code CLI, L1-L5, either UI browser/build gate, or release identity.

## Red Sequence

| Scope | Result | Finding | Cleanup |
| --- | --- | --- | --- |
| `full-unit-current-r1` | process abort | default debug Tokio worker overflowed in `all_multimodal_handlers_reject_21_remote_sources_before_upstream_for_five_rounds` | `removed=true`, `reservation_released=true` |
| exact multimodal rerun | `1/1`, 150 internal handler calls | moving only the heavy test fixture to the existing 4 MiB test thread removed the debug-only abort | cleaned |
| `full-unit-current-r2` | process abort | another real Router fixture, `local_non_stream_success_commits_shared_attempt_budget_before_usage_for_five_rounds`, still used the default worker stack | `removed=true`, `reservation_released=true` |
| `full-unit-current-r4` | `1702 passed / 4 failed / 6 ignored` | exposed one real external Kiro usage-policy conflict and three stale refresh/lease fixtures | `size_kib=1677744`, cleaned |
| `full-unit-failures-r1` | format-only red | rustfmt rejected one closure layout before compilation | `size_kib=28`, cleaned |
| `full-unit-current-r5` | `1705 passed / 1 failed / 6 ignored` | invalid refresh configuration still built an HTTP client before local validation; full-tree pressure broke the 500 ms wall-clock fixture | `size_kib=1678768`, cleaned |
| `invalid-refresh-config-r1` | format-only red | rustfmt rejected one assertion layout before compilation | `size_kib=28`, cleaned |
| `full-unit-current-r6` | red, failure identity not accepted | direct output truncation hid the failure name; this run is retained as a red diagnostic only | `size_kib=1676456`, cleaned |
| `full-unit-current-r7` | `1704 passed / 2 failed / 6 ignored` | two provider fault matrices each launched 15 independent providers at once; together with the rest of the full tree, immediate malformed-UTF8 and HTTP-200 JSON-error cells exceeded their 30-second fixture bound | `size_kib=1676440`, cleaned |
| `provider-fault-matrix-r1` | format-only red | rustfmt rejected two stream layouts before compilation | `size_kib=28`, cleaned |
| `warning-cleanup-fmt-r2` | format-only red | rustfmt required one deterministic import reorder | `size_kib=28`, cleaned |
| `warning-cleanup-check-r1` | check success but warning gate red | `cargo check --all-targets` exposed 24 production-bin warnings, including test-only reference paths and redundant normalized-body bytes | `size_kib=446980`, cleaned |
| `warning-cleanup-check-r2` | check success but warning gate red | the first classification pass reduced the set to three warnings | `size_kib=447312`, cleaned |
| `full-unit-current-r9` | red, failure identity not accepted | default harness output hid the failure summary after all 1712 tests; retained only as an undiagnosed red run | `size_kib=1676240`, cleaned |
| `full-unit-current-r10` | `1705 passed / 1 failed / 6 ignored` | configured 1-second provider header deadline accepted a delayed HTTP 500 at 1.651 seconds under full-tree executor pressure | `size_kib=1677828`, cleaned |
| `http-deadline-fmt-r1` | format-only red | rustfmt required one standard-library import reorder | `size_kib=28`, cleaned |
| `http-deadline-provider-r1` | test compile red | test-local `std::time::Instant` shadowed the production helper's Tokio instant | `size_kib=1115544`, cleaned |

The earlier `full-unit-current-r3`/quiet diagnostic sequence, `full-unit-current-r9`, and any `running 0 tests` filters are not behavioral evidence. Accepted behavioral counts came from non-zero exact filters or complete 1712/1714-test trees. Format/check/compile reds remain evidence of gate and fixture defects, not runtime behavior.

## Repairs

### Heavy Handler Fixtures

Heavy real Router/loopback tests now run their async bodies through the existing test-only 4 MiB OS-thread/current-thread Tokio helper. Production runtime stack configuration and production handler futures were not changed. The wrapped set includes multimodal admission, local non-stream shared-budget, contamination, all WebSearch handler matrices, and provider JSON fault handler paths.

### External Kiro-RS Tool Usage

`KiroRsToolCachePolicy.reportedInputMinTokens/MaxTokens` was previously overwritten by the generic external `input=raw` policy in a manually constructed fixture. The old test passed only because it mutated the request body without refreshing `request_input_tokens` and the preparation cache.

The repaired contract is:

- a resolved Kiro-RS Tool route still performs its own usage/cache projection when generic reported-usage shaping is disabled;
- generic shaping remains authoritative only when explicitly enabled;
- a failed attempt does not commit prompt-cache state;
- a successful attempt commits state and the next turn can report cache read;
- the Kiro-RS Tool input range remains `32..=4096` in the default strategy fixture.

### Refresh Fixtures And Local Invalid Configuration

- A one-second Redis lease touch interval expects the exact `Duration::checked_div(3)` value (`333.333333ms`), not a production truncation to `333ms`.
- A malformed refresh endpoint fails before auxiliary admission, so expected peak auxiliary concurrency is `0`.
- The concurrency-only OAuth success matrix uses test-only `6000 RPM / burst 256`; the production default `60/8` remains independently tested.
- Final refresh-source validation now runs after any Redis/PostgreSQL authority reload but before HTTP-client construction or admission. Five invalid-configuration rounds observed `2890/117/89/87/87 us`, zero client cache entries/builds/hits/misses, zero auxiliary peak, zero sends, and no credential health mutation.

### Provider Fault Matrix Resource Bound

The status/JSON and transport/body fault matrices retain every `pool 1/20/60 x 5 rounds x stream/non-stream` cell and every attempt/privacy/cooldown assertion. Their test-only internal provider concurrency is now bounded at four instead of `join_all` over all 15 specifications. High-concurrency behavior remains owned by the L3-L5 load runners.

Focused results:

- status/JSON matrix: `1/1`, internal matrix complete, `141.73s`;
- transport/body matrix: `1/1`, internal matrix complete, `243.67s`;
- scoped target: `size_kib=1673816`, `removed=true`, `reservation_released=true`.

### Production Warning Boundary And Normalized Body Cost

The all-target production check exposed more warnings than the test build because several reference helpers were used only under `cfg(test)`. Static call tracing separated test introspection/reference implementations from live production paths. Breaker state probes, the legacy Redis full-dashboard reader, legacy usage-script probes, refresh-count fixture APIs and test conveniences are now compiled only for tests. The production Redis series/top readers and current idempotent usage writer remain live.

`PreparedExternalMessagesPayload.raw_body` was also proven redundant. Guard mutations are carried by the typed payload and overlaid onto `effective_raw_body`, preserving unknown fields; the returned bytes were never read, and the guard-fallback path performed an unused JSON serialization. Removing that field/serialization changes neither normalized wire semantics nor failover caching and reduces request-local cloning/work. The complete body/overlay tests and final tree passed.

`warning-cleanup-check-r3` completed `cargo check --all-targets` on Rust 1.92.0 with zero warning. Its `446940 KiB` target was removed and its reservation released.

### Strict HTTP Header And Body Deadlines

`full-unit-current-r10` proved a production correctness issue rather than another assertion-only fixture problem. Tokio 1.48's timeout future can accept a wrapped result when the executor resumes after the deadline and both result and timer are ready. Three attempts returned typed timeout at 1001-1003 ms, while the fourth accepted the fake provider's 500 at 1651 ms.

The shared HTTP helper now uses monotonic deadline-first biased selection for headers and bodies. Ten deterministic ordering rounds, real loopback header/body stalls and the complete provider transport/body matrix passed. The repaired matrix took `245.74s` versus its prior isolated `243.67s` baseline, retained all 180 outcomes/540 sends, and added no attempts. See [dedicated evidence](http-deadline-runtime-starvation-20260718.md).

## Accepted Green Runs

`full-unit-failures-r2` ran the original four failures exactly:

- external Kiro-RS Tool projection: `1/1`;
- Redis lease touch interval: `1/1`;
- refresh client-builder failure: `1/1`;
- OAuth shared success: `1/1`, with `c1/c8/c16/c32 x 5` internal rounds and HTTP hits exactly equal to caller count.

The scope used `1682164 KiB` and ended with `removed=true` and `reservation_released=true`.

`invalid-refresh-config-r2` then passed the invalid-configuration five-round contract and the builder-failure contract (`2/2` exact test functions). The scope used `1676464 KiB` and was fully removed.

The then-current first accepted tree was `full-unit-current-r8` (`1706 passed / 0 failed / 6 ignored`). Warning cleanup and strict-deadline tests increased the tree by two entries. The first accepted post-deadline tree was:

```text
scope=full-unit-current-r11
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

Queue-lease lifetime, request-deadline freezing, real-storage fixtures and the API/MCP final-attempt request fixture then added seven test functions. The current complete default-bin tree is:

```text
scope=full-unit-current-r12
running 1721 tests
1715 passed
0 failed
6 ignored
test time=351.96s
wall time=581.7s
size_kib=1682460
removed=true
reservation_released=true
```

The real PostgreSQL pool-pressure and real Redis 22-second queue-deadline functions are part of the compiled tree, but without their explicit storage URLs their bodies return early. They remain compile-only here and are not counted as dynamic storage evidence; `run-runtime-quarantine-storage-validation.sh` requires dependencies and fails before Cargo when they are absent.

The six ignored cases are explicit isolated release/performance probes: three JSON whitespace-compression probes, payload-guard release size matrix, CLI endpoint transform performance, and IDE endpoint transform performance. They are not silently skipped storage tests and remain separate release gates.

The final full-tree run did not suppress warnings. The separate `cargo check --all-targets` warning gate is zero-warning. This closes the known warning set, not `cargo test --all-targets`, no-default, release, storage, frozen CLI or load gates.

## Artifact And Release Boundary

After the accepted run:

- no `.validation-build-*` directory remained;
- no `kiro-full-unit-r*.??????` temporary report directory remained;
- free disk was about `32 GiB`;
- root `target/` remained about `708 MiB` and was not deleted because it is editor-owned;
- `git diff --check` passed for the changed source set.

This is a current dirty-tree default-bin unit pass, not a frozen-candidate or release pass. The project remains `NO-GO` until the remaining WebSearch/MCP, stream, storage/scheduler, UI, upgrade, C0-C4 and L1-L5 gates are implemented and current.
