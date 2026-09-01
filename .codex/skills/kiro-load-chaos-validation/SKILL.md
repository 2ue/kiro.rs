---
name: kiro-load-chaos-validation
description: Validate kiro.rs scheduler, cache, rate-limit, streaming latency, and resource stability under real load and chaos. Use when changes affect account selection, RPM limits, concurrency, dfcache/high-cache routing, external accounts, payload guard, usage logging, retries, restarts, memory, file descriptors, or network queueing.
---

# Kiro Load Chaos Validation

Use this skill to validate that kiro.rs remains fast and stable under normal traffic, bursts, upstream errors, recovery, and restart conditions.

## Safety Contract

- Do not run load against production or another project's active service.
- Apply `docs/testing/project-test-instance.md`. In this repository the
  designated instance is `127.0.0.1:19023` with
  `tmp/thinking-budget-local/config.json`; all load and chaos cases reuse that
  one project-owned `kiro.rs` process. Isolation is a project ownership
  boundary, not a new service per case.
- Start a temporary `kiro.rs` service only when the designated instance cannot
  safely execute a destructive case. Record the reason, exact resources,
  lifetime, and cleanup result in the evidence report.
- Use fake upstream scenarios before real upstream pressure.
- Real upstream pressure requires an explicit purpose, low starting concurrency, and a hard cap.
- Do not print secrets or full credentials.
- Stop every fake server, temp proxy, and background load process. Do not stop
  the designated project instance unless the test explicitly covers
  restart/recovery and the process is confirmed to belong to this repository.
- Save raw reports under an owned temporary directory. Retain only redacted summaries and hashes under `feature/evidence/`, then remove the raw directory.

## Build Artifact Contract

- Every local Cargo command must run through `feature/tests/run-cargo-scoped.sh <scope> -- <command...>`. CI is exempt only when the job explicitly uses an ephemeral filesystem that is discarded in full.
- Concurrent branches are allowed when the wrapper admits their atomic disk reservations. Every branch still deletes its own scoped target immediately after its logical build batch on success, failure, timeout, or signal.
- Copy only a completed candidate binary out of `$CARGO_TARGET_DIR` before wrapper cleanup. All load, chaos, runtime, and restart runners must receive the resulting absolute immutable `KIRO_RS_BINARY`; they must not inspect `target/debug`, `target/release`, or rebuild automatically.
- Run `node feature/tests/inventory-build-artifacts.mjs --gate` before release validation. Unknown/unmanaged targets, stale/active reservations, incomplete process inspection, and target-referencing live processes are release blockers. Docker disk information is read-only evidence; Docker cleanup always requires separate manual review.

## Tier Selection

- **L0 static/resource gate**: source diff review, `git diff --check`, and one scoped batch containing `cargo fmt --check`, `cargo test`, and `cargo build --release`; copy and hash the frozen binary before scoped cleanup.
- **L1 fake upstream smoke**: validate loadtest tooling, streaming parsing, thinking, tool-use, malformed SSE, and error normalization without consuming real accounts.
- **L2 real low-concurrency gate**: validate `/cc`, `/v1`, `/dfcache/*`, RPM limits, model aliases, high-cache reporting, and usage records with small real traffic.
- **L3 burst and recovery gate**: sudden concurrency spike, sudden invalid traffic spike, mixed success/error traffic, and sudden drop to zero.
- **L4 chaos gate**: restart temp proxy during traffic, upstream 429/500/invalid-tool bursts, recovery-after-burst, client disconnects, idle streams.
- **L5 soak gate**: sustained 15-30 minute run for memory, FD, queueing, TTFB, inter-chunk delay, and cleanup behavior.

Use `docs/testing/loadtest.md` as the command reference for the existing `kiro_loadtest` binary.

## Resource Measurements

Capture before, during, and after:

```bash
ps -o pid,rss,vsz,%cpu,%mem,etime,command -p <pid>
lsof -nP -p <pid> | wc -l
lsof -nP -iTCP -sTCP:ESTABLISHED | wc -l
```

On macOS, also capture system memory pressure if a long test is run:

```bash
vm_stat
netstat -anv | head
```

Reports must include:

- p50/p95/p99 TTFB.
- p50/p95/p99 total latency.
- first thinking latency for thinking scenarios.
- first text latency for stream scenarios.
- status-code distribution.
- sampled request ids and error ids.
- process RSS start, peak, and end.
- FD start, peak, and end.
- success/error counts by scenario.

## Required Scenarios

Read `references/load-chaos-matrix.md` for the full matrix.

At minimum for scheduler or protocol releases:

- Normal stream and non-stream.
- Thinking stream.
- Tool-use stream.
- Configured `/dfcache/*` route.
- Missing `/dfcache/*` route.
- Per-account RPM saturation and recovery.
- Concurrency saturation and recovery.
- 429 burst, 500 burst, invalid tool-use burst.
- Client disconnect.
- Slow first byte and slow thinking before text.
- Recovery after burst.

## Performance Interpretation

Do not treat any single latency number as enough. Compare across load steps:

- If TTFB grows roughly linearly with concurrency, inspect dispatch queueing and account RPM/concurrency caps.
- If TTFB is low but text arrives in pauses, inspect upstream streaming chunk cadence and proxy flush behavior.
- If failures are mostly 429, reduce per-account concurrency/RPM or add scheduler backoff.
- If failures are capacity 503 while accounts are available, inspect scheduler snapshots and real-time capacity reads.
- If RSS or FD count does not return near baseline after traffic stops, inspect stream cleanup and connection pool behavior.
- If CPU spikes with large histories, inspect payload guard, tool schema normalization, and usage detail generation.

## Pass/Fail Rule

Fail the validation if any of these occur:

- RSS or FD count keeps rising after traffic stops.
- Temp proxy does not shut down cleanly.
- Scheduler returns capacity errors while eligible accounts or configured external accounts are available.
- Real upstream low-concurrency requests time out without clear request ids or internal logs.
- Error responses expose internal pool, credential, fallback, or private scheduler wording.
- A restart leaves sockets, tasks, or temp processes behind.
- A sudden error burst prevents later normal traffic from recovering.
