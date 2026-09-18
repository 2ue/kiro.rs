# Main Branch Validation Summary

Date: 2026-09-15

## Build Identity

- Branch: `main`
- Commit: `5313bc1` (`v0.0.163`)
- Frozen `kiro-rs` SHA-256: `2c01592c1ba3525e21ec23c72383f75862a23e6fa325a90dad0c6250482fc825`
- Frozen `kiro_loadtest` SHA-256: `d735a84e35fef8827f9e1e52e6c96004c10a36508cecdf390b5a9ea0ed956c59`
- Build target and reservation cleanup: passed (`removed=true`, `reservation_released=true`)
- Build artifact inventory gate: passed (`targets=0`, `reservations=0`, `target_processes=0`, `blockers=0`)

## Static And Unit Gates

- `cargo fmt --check`: passed
- Rust test suite: `2097 passed`, `0 failed`, `6 ignored`
- `kiro_loadtest` tests: `31 passed`, `0 failed`
- `git diff --check`: passed
- Runtime validation contracts: `20 passed`, `0 failed`

All Cargo commands used the scoped Cargo wrapper. The release binaries were copied outside Cargo's target directory before runtime validation.

## Fake Upstream Loadtest

The frozen `kiro_loadtest` binary was used against isolated loopback fake servers. No real Kiro or Claude upstream was enabled.

| Scenario group | Result |
| --- | --- |
| Normal stream | `5/5` success |
| Normal non-stream | `5/5` success |
| Thinking stream | `5/5` success; first-thinking and first-text timings present |
| Tool-use stream | `5/5` success |
| Malformed SSE | `5/5` expected errors |
| Rate limit 429 | `5/5` expected `429` errors with error IDs |
| Server error 500 | `5/5` expected `500` errors with error IDs |
| Invalid tool format | `5/5` expected `400` errors with error IDs |
| Recovery after burst | `3` controlled failures followed by `5` successes |

## Mock Local And External Accounts

### External-pool scheduler matrix

The repository's product-level scheduler matrix used one temporary frozen `kiro.rs` process, one fake local Kiro upstream, and three fake external-pool upstreams. It covered:

- local-first routing and local transient/capacity fallback;
- normal stream and non-stream;
- route policy blocking and backup-pool selection;
- priority failover after `500`, `429`, and `403`;
- auto-disable and cooldown recovery;
- slow first byte, stream idle, pre-output retry, and post-output no-replay;
- concurrency saturation and backup capacity;
- persistent failures and recovery;
- explicit external-direct requests with no local rescue.

Result: every selected scenario passed. The fake local upstream recorded `23` inference hits in local-expected/fallback cases and `0` inference hits for external-direct cases. External fake pools received `374` requests in total, with successful traffic moving from the primary pool to backups under injected faults. The temporary service's sampled RSS rose from approximately `28 MiB` to `56 MiB` and FD counts stayed in the low double digits; all process and ports were released afterward.

### External-pool priority failover

The focused failover runner used one fake local upstream and three fake external pools:

- two `24`-request failure bursts;
- primary-pool recovery after the cooldown window;
- combined primary `503` and secondary `429`;
- all-pools-failed external-direct request.

Result: all successful phases returned `200`; the final all-pools-failed phase returned a non-`200` response; local inference hits remained `0` throughout business traffic. All fake upstreams included the expected Anthropic protocol version header. Temporary PostgreSQL/Redis state was deleted after the run.

## Scope And Limitations

- The designated long-lived `127.0.0.1:19023` instance was not running, and `tmp/thinking-budget-local/config.json` was absent. The existing designated PostgreSQL database contained historical credentials, so it was not used for mock traffic.
- To avoid touching existing credentials or other project services, the runtime cases used caller-owned temporary PostgreSQL databases, unique Redis prefixes, random loopback service ports, and fake upstreams. No request was sent to port `9022`, and no real credential or real upstream load was performed.
- The test run therefore validates the current `main` binary's local-account routing and external-pool behavior in isolated product-level integration scenarios, but is not a real-account or production-upstream validation.

## Cleanup

- Temporary PostgreSQL databases: deleted.
- Temporary Redis prefixes: zero keys remaining.
- Fake servers and temporary `kiro.rs` processes: stopped.
- Ports `19023`, `19080`, and all L1 fake ports: free.
- Raw reports and frozen runtime copies were removed after the redacted summary was retained.
