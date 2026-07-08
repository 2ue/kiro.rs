# Implementation Status

Current Phase: Locally verified implementation

Last Landed: 2026-07-07 request-id/model usage query fixes, supported-model routing for credentials and external pools, opt-in prompt logic retry, and UI updates.

Next Target: Optional low-volume real upstream smoke for supported-model sync and one allowed/blocked dispatch case, if explicitly requested.

Active TODO:

- Keep default generic usage search lightweight; add a separate deep JSON search only if needed later.
- Consider adding a dedicated request-id input in usage UI if auto-detection is not obvious enough for operators.
- Run a real upstream smoke only with explicit approval and low volume.

Blocked By: None.

Last Verified:

- `git diff --check`: pass.
- `cargo test --locked --no-default-features`: pass with temporary Xcode toolchain env; 920 main tests and 19 `kiro_loadtest` tests passed.
- `pnpm --dir ui build`: pass.
- Fake upstream direct reports:
  - `target/loadtest/admin-routing-normal-stream.json`: 6/6 HTTP 200.
  - `target/loadtest/admin-routing-normal-non-stream.json`: 6/6 HTTP 200.
  - `target/loadtest/admin-routing-tiered-slow-first-byte.json`: 6/6 HTTP 200, p95 TTFB about 22005 ms.
  - `target/loadtest/admin-routing-long-stream.json`: 4/4 HTTP 200.
  - `target/loadtest/admin-routing-mixed-chaos.json`: expected mixed status distribution, 10 HTTP 200, 1 HTTP 429, 1 HTTP 500.

Environment Note:

- Local PATH resolves `cc` to `/Users/yuanfeijie/.volta/bin/cc`, which breaks Rust linking. Verification used command-level `SDKROOT`, `CC`, `HOST_CC`, `CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER`, and `RUSTFLAGS=-Clinker=...` pointing to Xcode tools. No project config was changed for this.
