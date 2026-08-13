# Runtime Correctness And Release Gates

Status: In Progress

## Scope

Fix and reverify the current Rust gateway's Claude Code protocol, prompt/tool/thinking/image/search behavior, retry and admission bounds, scheduler/Redis/external fallback, usage/storage lifecycle, UI, upgrade, CI and release findings. The current execution package is the root-level [`feature/`](../../../../feature/README.md) tree. Production container/TLS/Admin network hardening remains explicitly out of scope for this plan.

## Non-Negotiable Requirements

- Never log raw credentials, tokens, client secrets, API keys, or proxy passwords.
- PgSQL failure thresholds, Redis sticky state, and Redis concurrency leases must remain correct across multiple service instances.
- Storage-backed integration tests must fail when their dependencies are missing; they must not report a skipped body as a passing test.
- Accepted usage and runtime-state writes must have bounded resource use and an explicit shutdown/drain path.
- Tag publication must execute the same release gates as main CI and verify the tag against the Cargo package version.
- Clean request and Claude Code protocol behavior must remain compatible; verified defective behavior may change only through an explicit contract, migration, negative tests and rollback notes.
- Existing uncommitted frontend work must be preserved.
- Usage soft cleanup removes matching detail and its accumulated rollup/cost contribution exactly once; hard cleanup only removes tombstones and must not double-subtract.

## Relationship And Authority

This plan remains the durable plan-tree owner for runtime fixes, lifecycle behavior, storage/Redis correctness and release gates. The current detailed problem, reproduction, implementation and evidence authority is the [`feature/` delivery tree](../../../../feature/README.md); historical results under this plan's `history/` remain dated evidence and must not be treated as current v0.0.109 candidate results. The [Greenfield AI Gateway plan](../greenfield-ai-gateway/README.md) consumes verified outcomes as behavioral constraints and owns later target structural changes; it must not reclassify an incomplete gate as passed or reopen landed behavior without an explicit decision and new evidence.

The production container, TLS, Admin network-isolation and database-secret hardening scope remains deferred for the current Rust maintenance plan; the Greenfield AI Gateway accepts it for the separate target candidate. Cross-replica requirements describe availability for one operator and do not introduce a multi-user or tenant boundary.

## Reading Path

1. [Implementation status](implementation-status.md)
2. [Roadmap](roadmap.md)
3. [Current feature delivery tree](../../../../feature/README.md)
4. [Current verification matrix](../../../../feature/tests/reverification-matrix.md)
5. [Project runtime flow](../../baseline/runtime-flows.md)
6. [Storage and state](../../baseline/storage-and-state.md)
7. [Test and release gates](../../baseline/test-and-release-gates.md)
