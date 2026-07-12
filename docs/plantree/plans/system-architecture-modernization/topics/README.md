# Topic Index

Role: Retrieval index for system architecture modernization topics

Status: Accepted final-plan index

Authority: Navigates current problem analysis, accepted decisions and final implementation documents; decisions remain the binding authority

As of: `v0.0.102` / `e9479df` / plan updated 2026-07-12

Read when: Selecting the minimum durable context for analysis, design, implementation planning, or verification

Related: [Plan root](../README.md), [Index registry](../indexes/README.md), [Authority and source map](../indexes/authority-and-source-map.md), [Decision index](../decisions/README.md)

Rewrite coverage: [Complete rewrite inventory](../indexes/rewrite-inventory.md)

Problem-to-landing coverage: [Traceability matrix](../indexes/traceability-matrix.md)

Active planning surface: [100-requirement baseline](requirements-and-quality-attributes.md), [47-finding problem catalog](problems/README.md), [16-candidate discovery ledger](../indexes/finding-candidate-ledger.md), [50-module target ledger](../indexes/target-module-ledger.md), and [modular work map](../indexes/execution-slice-map.md)

## Requirements And Problems

- [Requirements and quality attributes](requirements-and-quality-attributes.md): durable functional invariants, quality requirements, constraints, explicit non-goals, and measurable success criteria.
- [Problem catalog](problems/README.md): severity model, status vocabulary, summary, ownership, dependencies, and links to evidence-backed problem groups.
- [Correctness, security, and resource bounds](problems/correctness-security-and-resource-bounds.md): logging/content leakage and retention, concurrent configuration, Redis aggregation, path preservation, remote sources, SSRF, key/catalog refresh, and bounded work.
- [Architecture, performance, and state](problems/architecture-performance-and-state.md): broad ownership, request-path I/O, scheduling cost, duplicated snapshots, storage coupling, usage write amplification, and blocking work.
- [Operations, testing, frontend, and supply chain](problems/operations-testing-frontend-and-supply-chain.md): shutdown, readiness, job ownership, audit durability, contract generation, test gaps, load-harness validity, performance gates, artifacts, and release provenance.
- [Continuous audit and finding lifecycle](problems/continuous-audit-and-finding-lifecycle.md): repeatable audit axes, candidate promotion, work-unit entry/integration review, unknown-problem discovery, and closure evidence.

Read the problem catalog before a solution topic. A problem severity describes observed impact and risk; it does not grant solution authority. Only an accepted decision or contract makes a target rule binding.

## Target Architecture

- [Target system architecture](architecture/target-system-architecture.md): domain-oriented modular-monolith context, module responsibilities, public-contract dependency direction, target-only composition, and non-goals.
- [Module boundaries and contracts](architecture/module-boundaries-and-contracts.md): module-internal roles, narrow runtime views, transport/protocol/application contracts, neutral terminal facts, technical-authority ports, test-only comparison boundaries, and forbidden dependencies.
- [Runtime, control, and data flows](architecture/runtime-control-and-data-flows.md): Messages, count-tokens, Files, scheduling, retries, streaming, Admin updates, background writes, startup, reload, readiness, and shutdown.
- [State ownership and consistency](architecture/state-ownership-and-consistency.md): PgSQL authority, Redis coordination, immutable runtime snapshots, CAS, outbox/event semantics, in-memory state, and filesystem lifecycle.
- [Admin and frontend architecture](architecture/admin-and-frontend-architecture.md): control-plane services, Rust-authoritative schema, both maintained frontend rewrites, state/security/accessibility, module work, and one whole-system release.

The single-user boundary, retry/terminal/scheduler/shutdown contracts, 50-module target, migration/recovery separation, one-program delivery model and operational policies are **Accepted** through decisions 001 and 003-014. Decision 002 is historical and **Superseded** for delivery. Implementation details may evolve only inside those binding boundaries. Documents distinguish current baseline, accepted target, target-only validation tooling and implementation evidence. `R0`-`R10` name dependency groups only; they carry no staffing, ownership, calendar, production stage, rollout wave or release authority.

## Delivery And Verification

- [Final complete implementation plan](delivery/migration-sequence.md): one-program target-only modular construction, dependency order, all-system integration, legacy removal and final activation.
- [Implementation entry and completion contract](delivery/next-package-brief.md): reusable pinned-audit/coding/integration/deletion/evidence loop with no personnel or phased-release gate.
- [Migration subsystem contract](delivery/migration-foundation-brief.md): `MOD-MIGRATIONS` versus domain SQL and `MOD-RECOVERY`, fresh/legacy/partial adoption, previous-binary behavior, bounded backfills and old-runner deletion.
- [Performance contract and canonical workloads](delivery/performance-contract-and-workloads.md): absolute capacity/outcome plus relative regression, reference-host identity, workload manifests, metric semantics, harness validity, recovery, and cost gates.
- [Verification, final cutover, and whole-system rollback](delivery/verification-rollout-and-rollback.md): characterization, protocol/storage/load/browser/recovery gates, final release evidence, one complete cutover and whole-system rollback.
- [Repository cleanup and filesystem plan](delivery/repository-cleanup-and-filesystem-plan.md): ownership, keep/move/archive/delete rules, ignored test artifacts, diagnostic retention, safety checks, and rollback for cleanup.
- [Legacy document disposition](../indexes/legacy-document-disposition.md): reviewed tracked-document deletion, keep-until-replacement, later-archive, and Git-history recovery rules.
- [Modernization evidence index](../history/evidence-index.md): currently empty registry for future package/gate evidence; no modernization gate is recorded as passed.

## Reader Shortcuts

| Reader task | Minimum reading set |
| --- | --- |
| Understand why modernization exists | Requirements, problem catalog, current system context |
| Change request handling | Target system architecture, module contracts, runtime flows, request-body plan |
| Change scheduler or retries | Module contracts, runtime flows, state ownership, runtime-correctness plan |
| Change runtime configuration, Admin backend, or either frontend | Admin/frontend architecture, state ownership, runtime/control flows, Admin existing plan, relevant accepted decisions |
| Change PgSQL schema, migration, startup repair, or backfill behavior | `R2.0.migration-foundation` contract, accepted decision 008, state ownership, module contracts, verification, and the selected domain-authority row |
| Change usage or cache accounting | Requirements, state ownership, runtime/data flows, verification and rollback |
| Change remote media, Files, PDF, or tokenizer behavior | Correctness/resource problems, module contracts, resource baseline, verification |
| Change performance, load/chaos, scheduler/storage hot paths, or resource limits | Performance contract, decision 010, verification, performance findings, and traceability matrix |
| Start module implementation | Implementation entry contract, one target-module row, one modular-work row, accepted decisions and selected traceability rows/gates |
| Audit for problems not yet reported | Continuous audit/finding lifecycle, finding candidate ledger, current baseline, problem catalog, traceability matrix, and selected source inventory |
| Clean generated artifacts | Repository cleanup plan, evidence references, current `git status`, and authority/manifest rules |

## Maintenance Rules

- Keep one responsibility per topic; split by reader task or authority, not arbitrary part numbers.
- Use `MUST`, `SHOULD`, and `MAY` only in requirements or accepted contract sections, and define their scope.
- Do not copy the same issue narrative into the roadmap, architecture, and verification files; link the stable problem identifier.
- When a topic grows beyond a useful retrieval unit, add a local index before adding deeper than two topic levels.
- Update this index whenever a durable topic is added, renamed, archived, or superseded.
