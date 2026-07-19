# Roadmap

## Done

- Reproduced the audit findings against real PgSQL/Redis dependencies and removed raw credential exposure from logs.
- Made PgSQL failure counting, statistics, revisions, and compare-and-swap updates atomic across instances.
- Made Redis sticky state, in-flight accounting, and local/external dispatch queues lease-based, renewable, stale-safe, and fail-closed.
- Bounded usage/storage writers and runtime cleanup work, added explicit graceful shutdown drain and generation fences, and made final stats/runtime draining cross batch limits within one deadline.
- Moved external-pool concurrency lease touch/release work into the bounded, drainable storage executor with critical release retry and synchronous fallback.
- Made storage-backed tests fail when dependencies are missing and aligned main CI, tag publication, release features, and both frontend build gates.
- Preserved Claude stream/non-stream, thinking, tool-use, usage, model-alias, cache-reporting, and normalized error behavior.
- Corrected the affected frontend types and shared contracts without removing either maintained frontend.
- Passed the full default/no-default Rust suites, focused real-storage tests, lint baseline, formatting, release build, protocol/load/chaos, resource-recovery, and in-flight SIGTERM checks.
- Verified both Docker frontend production builds and precisely cleaned the isolated builder after the later Cargo fetch timeout.
- Landed the current schema-key compatibility fix: default reversible sanitize, explicit reject/disabled modes, both UI config controls, real local-service stream/non-stream validation, and local release build with both embedded UI bundles.
- Scanned the final runtime evidence for credential-shaped data and removed generated frontend, debug, gate, Docker, Redis, database, port, and temporary-file residue owned by this validation.
- Indexed the landed validation and cleanup evidence in [history/evidence-index.md](history/evidence-index.md).
- Recorded the 2026-07-13 `docs/feature` / `tmp/analysis-usage-llm-errors` follow-up status, including `/ha` high-input usage explanation and the remaining real-service validation checklist.
- Corrected the 2026-07-17 scheduler incident diagnosis: credentials were not persistently disabled, but PgSQL success-persistence timeouts did temporarily quarantine each process-local entry before slow Redis sticky cleanup amplified the incident into admission degradation and external fallback.
- Implemented the uncommitted `fix/scheduler-redis-degradation-isolation` branch: bounded non-critical success persistence, preserved fail-closed critical mutations and Redis admission, released successful stream leases before persistence, isolated non-admission Redis work, batched high-cardinality sticky cleanup, removed unnecessary external-pool preflight scans, and singleflighted cached availability refreshes.
- Verified the scheduler fix with real PgSQL/Redis dependencies, including 40 credentials under a fully occupied two-connection PgSQL pool and 10,000 Redis sticky bindings during live admission probes; the final main suite passed `1169/1169` and the loadtest suite passed `26/26`.
- Captured isolated fake-upstream stream/non-stream, slow-first-byte, slow-thinking, and burst-recovery reports under `target/loadtest/scheduler-redis-fix-20260717/`; no real upstream or production listener was used.

## In Progress

- Validate the current usage/error correctness fixes before v0.0.103 release: schema diagnostics, external prompt-too-long handling, official Kiro upstream error message passthrough, stream completion observability, and both UI usage-detail surfaces.
- Close the end-to-end Docker release gate without treating frontend-stage success as a complete image build.
- Review and land the uncommitted scheduler isolation branch, then run a controlled production-like canary before deployment.

## Next

- Run C0, temporary local-service real calls, Claude CLI stream-json checks, and low-concurrency long-context resource smoke for the current dirty tree.
- Rerun the isolated Buildx gate when crates.io throughput can complete `cargo fetch --locked`; require Rust compilation and final image export before recording a pass.
- Before deploying the scheduler fix, run an isolated full-proxy canary with 40 fake credentials, PgSQL, Redis, a local Kiro fake upstream, and an external-pool sentinel; verify zero unexpected fallback and stable TTFB under sub-capacity traffic.
- Close this plan after the complete Docker gate passes.

## Deferred

- Non-root containers, read-only filesystems, capability dropping, TLS termination, Admin network isolation, and application-level database secret encryption.
- Removal of the legacy Admin UI before an explicit product decision.
