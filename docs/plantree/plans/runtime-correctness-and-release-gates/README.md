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

## Reading Path

1. [Roadmap](roadmap.md)
2. [Implementation status](implementation-status.md)
3. [Project runtime flow](../../baseline/runtime-flows.md)
4. [Storage and state](../../baseline/storage-and-state.md)
5. [Test and release gates](../../baseline/test-and-release-gates.md)

