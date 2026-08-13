# Historical Implementation Snapshot

Role: Historical landed handoff retained for dated evidence

Status: Closed; not an active implementation status

As of: 2026-07-07

Authority: Records local verification at landing; current maintenance and future backend/frontend rewrite state live in the roadmap and modernization plan

## Landed Scope

- Exact request-ID usage query and lightweight generic search.
- Supported-model eligibility for credentials and external pools.
- Opt-in bounded prompt/protocol retry with untried credentials.
- Admin configuration grouping and related UI updates.

## Current Ownership

There is no active TODO under this completed plan. Its landed query, eligibility, retry, and configuration semantics remain authoritative. The [system architecture modernization plan](../../system-architecture-modernization/README.md) owns the complete control-plane/backend/frontend rewrite, generated contracts, cross-replica invalidation, state ports, validation, and legacy deletion.

Optional low-volume real-upstream smoke remains evidence that may be collected only when explicitly requested; it is not active implementation work.

## Historical Verification

- `git diff --check`: passed.
- `cargo test --locked --no-default-features`: passed with the recorded temporary Xcode toolchain environment; 920 main tests and 19 `kiro_loadtest` tests passed.
- `pnpm --dir ui build`: passed.
- Historical fake-upstream summaries included normal stream/non-stream, approximately 22-second slow-first-byte, long stream, and mixed chaos cases.
- Raw reports under `target/loadtest/admin-routing-*` are ignored supporting artifacts, not durable current authority.

## Historical Environment Note

The local PATH resolved `cc` to `/Users/yuanfeijie/.volta/bin/cc`, which broke Rust linking. Verification used command-level Xcode `SDKROOT`, `CC`, `HOST_CC`, target linker, and linker flags. No project configuration was changed for that workaround.
