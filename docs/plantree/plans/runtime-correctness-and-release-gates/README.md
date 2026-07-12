# Runtime Correctness And Release Gates

Status: In Progress

## Scope

Fix the verified runtime, HA, lifecycle, CI, release, and maintainability findings from the 2026-07-10 project audit. Production container/TLS/Admin network hardening is explicitly out of scope for this plan.

## Non-Negotiable Requirements

- Never log raw credentials, tokens, client secrets, API keys, or proxy passwords.
- PgSQL failure thresholds, Redis sticky state, and Redis concurrency leases must remain correct across multiple service instances.
- Storage-backed integration tests must fail when their dependencies are missing; they must not report a skipped body as a passing test.
- Accepted usage and runtime-state writes must have bounded resource use and an explicit shutdown/drain path.
- Tag publication must execute the same release gates as main CI and verify the tag against the Cargo package version.
- Existing request and Claude Code protocol behavior must remain unchanged.
- Existing uncommitted frontend work must be preserved.

## Relationship And Authority

This plan remains authoritative for the runtime fixes, lifecycle behavior, storage/Redis correctness, release-gate requirements, and dated evidence landed from the 2026-07-10 audit. The [system architecture modernization plan](../system-architecture-modernization/README.md) consumes those outcomes as constraints and owns later structural changes; it must not reclassify an incomplete gate as passed or reopen landed behavior without an explicit decision and new evidence.

The deferred production container, TLS, Admin network-isolation, and database-secret hardening scope remains deferred here until a registered plan explicitly accepts ownership. Cross-replica requirements describe availability for one operator and do not introduce a multi-user or tenant boundary.

## Reading Path

1. [Roadmap](roadmap.md)
2. [Implementation status](implementation-status.md)
3. [Project runtime flow](../../baseline/runtime-flows.md)
4. [Storage and state](../../baseline/storage-and-state.md)
5. [Test and release gates](../../baseline/test-and-release-gates.md)
