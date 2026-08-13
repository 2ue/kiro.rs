# Provider Transient Retry Target Guard

Date: 2026-07-18

Status: `focused-fix-pass / full-all-target-dev-pass / release-gate-pending`

## Scope

This evidence records a provider retry guard added after the full `cargo test --all-targets`
tree exposed a controlled fault-matrix timeout.

The issue is related to internal RPM/latency amplification: after a local credential receives a
typed transient upstream failure and enters cooldown, a request must only retry when another
credential is immediately usable for the same request. If no alternate credential exists, the
provider should return the typed upstream failure instead of entering another scheduler acquire
that can wait for cooldown/capacity.

## Red Observation

Command:

```bash
env KIRO_VALIDATION_RESERVE_KIB=10485760 \
  feature/tests/run-cargo-scoped.sh all-target-tests-20260718-r1-devreserve -- \
  env RUSTUP_TOOLCHAIN=1.92.0 bash -lc 'cargo test --all-targets'
```

Result:

```text
FAILED: kiro::provider::tests::provider_transport_and_body_fault_matrix_is_private_typed_and_bounded
panic: provider failure call timed out: provider_header_timeout
1723 passed; 1 failed; 6 ignored
scoped target size_kib=1688040 removed=true reservation_released=true
```

The run used a reduced development reservation because the machine did not have enough free disk
for the default 12 GiB reservation plus 20 GiB floor. This is not a release gate result.

## Fix

Changed `KiroProvider::maybe_exclude_after_transient_failure` to return whether a retry target is
actually available after the transient failure is recorded.

Applied that return value to API and MCP transient branches:

- transport/header/body timeout;
- body read failure;
- non-eventstream success response;
- 429 temporary risk cooldown;
- 402 non-quota transient payment state;
- 408/429/5xx transient status;
- MCP send/body/rate-limit/payment/transient/protocol fallback.

When no retry target is available, the last attempt action is set to `fail` and the typed provider
or MCP error is returned immediately. This keeps multi-credential failover intact while avoiding
cooldown/capacity waits when the current request has no useful next local send.

The diagnostic panic in the fault fixture was also made more specific:

```text
scenario=<scenario> stream=<bool> pool=<n> round=<n>
```

## Green Evidence

Focused provider fault matrix:

```bash
env KIRO_VALIDATION_RESERVE_KIB=7340032 \
  feature/tests/run-cargo-scoped.sh provider-fault-focused-20260718-r2-dev7g -- \
  env RUSTUP_TOOLCHAIN=1.92.0 \
  bash -lc 'cargo test provider_transport_and_body_fault_matrix_is_private_typed_and_bounded -- --nocapture --test-threads=1'
```

Result:

```text
1 passed; 0 failed; 1729 filtered out
test time: 242.62s
scoped target size_kib=1696156 removed=true reservation_released=true
```

Static all-target check:

```bash
env KIRO_VALIDATION_RESERVE_KIB=7340032 \
  feature/tests/run-cargo-scoped.sh provider-retry-target-check-20260718-r6-dev7g -- \
  env RUSTUP_TOOLCHAIN=1.92.0 bash -lc 'cargo fmt --all -- --check && cargo check --all-targets'
```

Result:

```text
PASS
scoped target size_kib=447308 removed=true reservation_released=true
```

Full all-target development pass:

```bash
env KIRO_VALIDATION_RESERVE_KIB=7340032 \
  feature/tests/run-cargo-scoped.sh all-target-tests-20260718-r2-dev7g -- \
  env RUSTUP_TOOLCHAIN=1.92.0 bash -lc 'cargo test --all-targets'
```

Result:

```text
src/main.rs unit tree: 1724 passed; 0 failed; 6 ignored; finished in 291.45s
src/bin/kiro_loadtest.rs unit tree: 27 passed; 0 failed; finished in 2.01s
scoped target size_kib=1696704 removed=true reservation_released=true
```

Node and hygiene checks in the same work session:

```text
run-cargo-scoped lifecycle: 21/21 pass
runtime/thinking/bare/load runner path and signal contracts: 61/61 pass
feature docs section contract: 46 issue documents, 70 links pass
cost format contract: PASS
MCP attempt channel contract: PASS
request API key ID contract: PASS
prompt control independence: PASS
prompt default parity: PASS
git diff --check: PASS
validation scoped target residual scan: zero after each completed batch
```

## Limits

The machine did not have enough free disk to satisfy the default 12 GiB reservation plus 20 GiB
floor after root target cleanup. These Rust runs therefore used a reduced development reservation
of 7-10 GiB and must not be recorded as final release gates.

No Docker-backed dynamic storage tests were run. No production or `127.0.0.1:9022` service was
touched. `kiro_idc_users*.txt` files were not read or staged.

Final release still requires a frozen binary, default-reservation C0, real isolated PG/Redis
storage runners, C1-C4 Claude CLI gates, L1-L5 load/chaos, browser gates, upgrade smoke, inventory
gate and publish workflow.
