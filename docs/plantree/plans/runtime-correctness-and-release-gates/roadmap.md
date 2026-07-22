# Roadmap

## Done

- Completed the 2026-07-23 final release gate for the current v0.0.109 candidate: Rust scoped C0/release build, full all-target tests, feature docs, Node contracts, `git diff --check` and build inventory pass with frozen binary hashes recorded; Docker dynamic execution remains a user waiver and is not counted as pass.
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
- Recorded the 2026-07-13 `feature` / `tmp/analysis-usage-llm-errors` follow-up status, including `/ha` high-input usage explanation and the remaining real-service validation checklist.

## In Progress

- Publish the current v0.0.109 dirty-tree [remediation and release plan](../../../../feature/plans/remediation-and-release-plan.md) after the final 2026-07-23 release gate pass; record commit/tag/push results in the feature release log.
- Carry the current default-bin `1715/1715` non-ignored unit pass, zero-warning all-target check and cleanup 36/36 x3 outer passes into full PostgreSQL/all-target tests/no-default and writer-performance gates, plus both UI browser wording. Normal usage/scheduler joint pressure, single-instance Redis chaos, single-instance usage-writer+fault and E03 real two-process scheduler/RPM now pass; external takeover focused/runner contract, E01/E02 runner contract, E05 non-Docker runner contract, F06 non-Docker runner contract, request-admission non-Docker runner contract, frozen load/chaos runner contract and Redis storage runner no-FLUSH contract pass, but dynamic external takeover, E01/E02 distribution, E05 full matrix, F06 lifecycle, request-admission dynamic, final L3/L4/L5 rebind and two-instance fault/fallback gates remain open.
- Carry focused WebSearch/MCP and stream fixes into native CLI/frozen gates, and finish Redis/local-first/external-takeover/multi-instance implementation blockers.
- Carry the finite queue lease fix (no periodic renewal for bounded waits, stable per-request deadline, unlimited renewal retained) and shared RPM reservation fix from the 500-guard, 40x15 focused pass, isolated PostgreSQL/Redis runner, normal usage/scheduler burst, single-instance Redis chaos, single-instance usage-writer+fault, E03 real two-process scheduler/RPM, external-takeover focused/runner contract, E01/E02 runner contract, E05 non-Docker runner contract, frozen load/chaos runner contract and r8 frozen L3-L5 fake-upstream pass into the remaining external-takeover dynamic, E01/E02 distribution dynamic, E05 dynamic full matrix, final L3/L4/L5 rebind, two-instance, real-upstream and CLI pressure gates.
- Carry the now-passing r8 real Claude Code CLI 5x20/5x100 long-session/resume gate into native Kiro upstream and MCP/search/image/agent/fault matrices: internal transcript markers are forbidden, but arbitrary user/tool_result fixture text is not itself a leak.
- Carry the generation-bound external authoritative pool-list singleflight, strict malformed-row isolation and prepare-after revision fence from zero-warning/non-storage focused evidence into the dedicated isolated PG/Redis c32/c128 runner and frozen L3-L5 gates.
- Rebind every focused result to one frozen release candidate without treating a narrow filter or historical binary as a full pass.
- Carry the process-local OAuth budget/singleflight/cancellation and isolated Redis state-machine/final-attempt passes into Redis/PostgreSQL two-instance, provider attribution, refresh-specific chaos and final frozen gates.
- Carry the development thinking wire pass (`max` retained through converter/provider) and generic Bash/Read long-session pass into real-upstream delta/usage and active/passive thinking long-session matrices; do not require or probe an unadvertised Kiro `thinking` field.

## Next

- Execute the Git release workflow: fetch remote/tags, commit current work, apply any required version bump, create annotated tag, push branch, then push tag.
- Run the remaining real Claude Code CLI active/passive thinking, image, search, MCP, agent, native-upstream and fault matrices against the frozen candidate; Bash/Read 20/100-tool fake-upstream long-session/resume is already passing.
- Run the remaining 429/500/partial/malformed/client-drop, external takeover dynamic service, E01/E02 distribution/sticky/lease-race dynamic, E05 dynamic full matrix, F06 lifecycle dynamic, request-admission dynamic, final L3/L4/L5 rebind, token-refresh/multi-instance Redis dynamic reruns without runner-level FLUSH, two-instance fault/fallback, cleanup pressure and final-candidate soak gates with resource recovery evidence. Ordinary single-instance Redis latency/disconnect/restart, single-instance usage-writer+Redis fault, E03 real two-process scheduler/RPM, external-takeover runner contract, E01/E02 runner contract, E05 non-Docker runner contract, F06 non-Docker runner contract, request-admission non-Docker runner contract, frozen load/chaos runner contract, Redis storage runner no-FLUSH contract and r8 fake-upstream L3-L5 already pass.
- Compile the Docker/dependency validation programs without executing local Docker, record that explicit user waiver, then complete production read-only recurrence audit, final evidence reconciliation, commit/version/tag/push and post-release observation.
- Close this plan only after [feature/final-report.md](../../../../feature/final-report.md) changes from `NO-GO` to an evidence-backed release result.

## Deferred

- Non-root containers, read-only filesystems, capability dropping, TLS termination, Admin network isolation, and application-level database secret encryption.
- Removal of the legacy Admin UI before an explicit product decision.
