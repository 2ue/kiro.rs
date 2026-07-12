# Decision Index

Role: Registry for accepted, superseded, rejected, and still-proposed architecture decisions

Status: Current decision registry

Authority: An individual record becomes binding only when its status is `Accepted`; proposed topic text remains non-binding

As of: 2026-07-12

Read when: A target design choice affects technical authority, compatibility, persistence, final activation, or more than one module

Related: [Plan root](../README.md), [Topic index](../topics/README.md), [Authority and source map](../indexes/authority-and-source-map.md)

## Decision Lifecycle

| Status | Meaning |
| --- | --- |
| Proposed | A concrete choice is under review; implementation must not assume acceptance |
| Accepted | The decision is binding for its stated scope and date |
| Superseded | A newer linked decision replaces all or part of the record |
| Rejected | The choice was considered and explicitly not selected |

Accepted records use the next stable numeric filename, for example `001-runtime-snapshot-publication.md`. Do not renumber records. A superseding decision links the prior record and states precisely which consequences remain valid.

## Required Record Content

Every decision must state:

- date, status, scope, and affected problem or requirement identifiers;
- current context and the exact choice being made;
- considered alternatives and material tradeoffs;
- compatibility and data-migration consequences;
- target integration, whole-system rollback, observability, and verification requirements;
- related dependency group, topics, existing plans, and any superseded records.

Implementation progress, test logs, release exceptions, and benchmark output belong in roadmap/status/history files, not inside an architecture decision.

## Current Registry

| Decision | Status | Scope |
| --- | --- | --- |
| [001: Single-user, single-trust-domain product model](001-single-user-single-trust-domain.md) | Accepted | Identity, authorization interpretation, storage and scheduling non-requirements |
| [002: Complete module-by-module rewrite](002-complete-module-by-module-rewrite.md) | Superseded by 009 | Historical complete-rewrite rationale and retired per-module production rollout model |
| [003: Attempt replay safety and downstream commitment](003-attempt-replay-and-downstream-commitment.md) | Accepted | Upstream execution/replay safety, downstream commitment, retry, fallback, and idempotency |
| [004: Terminal authority and partial-failure recovery](004-terminal-authority-and-partial-failure-recovery.md) | Accepted | One terminal decision, durable terminal/outbox acceptance, module idempotency, and recovery |
| [005: Scheduler queue and lease lifecycle](005-scheduler-queue-and-lease-lifecycle.md) | Accepted | Queue, cancellation, fencing, heartbeat, completion, Redis epoch, and capacity recovery |
| [006: Producer-aware shutdown and residue](006-producer-aware-shutdown-and-residue.md) | Accepted | Producer barriers, writer drain, dependency close order, residue, and process outcome |
| [007: Domain-oriented modular monolith and module ownership](007-domain-oriented-modular-monolith-and-module-ownership.md) | Accepted | Stable module IDs, domain authority, shared-kernel limits, dependency rules, narrow runtime views, and target-only composition |
| [008: Domain-owned migrations and recoverable adoption](008-domain-owned-migrations-and-recoverable-adoption.md) | Accepted | Domain manifest/DDL authority, common migration runner/ledger, recovery separation, legacy adoption, bounded backfills, previous-binary compatibility, and legacy-runner deletion |
| [009: Single-program modular build and final system cutover](009-single-program-modular-build-and-final-cutover.md) | Accepted | One complete implementation program, target-only modular construction, one final activation, whole-system rollback, and legacy removal |
| [010: Fixed operational and acceptance policies](010-fixed-operational-and-acceptance-policies.md) | Accepted | Supported deployment, durability, Files/cache, resource/performance limits, replay, scheduler, shutdown, revocation, recovery, and hardening policies |
| [011: Explicit secret-envelope and resource-governor authorities](011-explicit-secret-envelope-and-resource-governor-authorities.md) | Accepted | Unique crypto/key and process-resource authorities plus exact production ceilings |
| [012: Tool-definition compatibility and reversible schema mapping](012-tool-definition-compatibility-and-reversible-schema-mapping.md) | Accepted | Profile-specific tool boundary normalization, reversible property mapping, raw preservation and stable rejection |
| [013: Owner-transaction audit acceptance](013-owner-transaction-audit-acceptance.md) | Accepted | Atomic domain mutation/audit append through a sealed narrow PgSQL capability |
| [014: Release generation, recovery barrier and rollback state](014-release-generation-recovery-and-rollback-state.md) | Accepted | Expected replica/digest fencing, Redis-loss membership, one-window migration and per-authority rollback compatibility |

Decisions 001 and 003-014 bind the product boundary, target correctness contracts, modular architecture, migration/recovery separation, one-program delivery model, conservative operational policies, explicit shared authorities, tool compatibility, transactional audit and generation/rollback behavior. Decision 002 remains a superseded historical record. Detailed implementation types and source layout may evolve inside these decisions, but they cannot reintroduce per-module production selectors, personnel gates, unresolved permissive defaults, or legacy imports. The 50 target modules and dependency work units are indexed in the [target module ledger](../indexes/target-module-ledger.md) and [modular work map](../indexes/execution-slice-map.md).
