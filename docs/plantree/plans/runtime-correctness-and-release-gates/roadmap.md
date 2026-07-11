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
- Scanned the final runtime evidence for credential-shaped data and removed generated frontend, debug, gate, Docker, Redis, database, port, and temporary-file residue owned by this validation.
- Indexed the landed validation and cleanup evidence in [history/evidence-index.md](history/evidence-index.md).

## In Progress

- Close the end-to-end Docker release gate without treating frontend-stage success as a complete image build.

## Next

- Rerun the isolated Buildx gate when crates.io throughput can complete `cargo fetch --locked`; require Rust compilation and final image export before recording a pass.
- Close this plan after the complete Docker gate passes.

## Deferred

- Non-root containers, read-only filesystems, capability dropping, TLS termination, Admin network isolation, and application-level database secret encryption.
- Removal of the legacy Admin UI before an explicit product decision.
