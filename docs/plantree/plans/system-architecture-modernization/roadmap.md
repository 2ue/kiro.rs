# Roadmap

Role: Historical state of the superseded Rust modernization and its dependency work order

Status: Superseded on 2026-07-13; implementation never started; archive/reference only

As of: 2026-07-13

Related: [Greenfield AI Gateway](../greenfield-ai-gateway/README.md), [supersession record](history/superseded-by-greenfield-ai-gateway-2026-07-13.md), [Plan root](README.md), [Final readiness review](history/final-plan-readiness-review-2026-07-12.md), [Historical work map](indexes/execution-slice-map.md)

## Supersession

This roadmap is no longer an implementation queue. The greenfield plan replaces its Rust target, fixed 50-module topology, two-frontend scope, implementation order and cutover design. Retain the content below only for historical reasoning, source evidence and explicitly inherited semantic invariants.

## Done: Final Planning Specification

- Established the single-operator, single-trust-domain product boundary and explicit non-goals.
- Built a dated current baseline for product, modules, protocols, flows, state, resources, deployment, tests and risks.
- Recorded 47 evidence-backed findings across correctness, security, resources, architecture, performance, operations, frontend, testing, documentation and supply chain; the catalog remains open-ended.
- Defined 100 functional/invariant/quality clauses, 16 exact gate IDs, 16 reconciled finding candidates and complete finding/source/module traceability.
- Accepted conservative retry, terminal, scheduler and producer-aware shutdown contracts in decisions 003-006.
- Accepted the domain-oriented modular monolith and split migration runner from disaster recovery in decisions 007-008.
- Accepted decision 009: one final complete implementation, module-organized target-only construction, one whole-system production cutover and whole-system rollback.
- Accepted decision 010 and resolved former `Q-001`-`Q-013`: multi-replica support, durability, shared Files, cache authority, resource/performance limits, replay safety, scheduler timing, shutdown residue/deadlines, auth revocation, recovery and hardening.
- Accepted decisions 011-014: unique secret-envelope/resource-governor authorities, reversible tool-schema mapping, sealed transaction-local audit append, signed release-generation fencing and per-state rollback compatibility.
- Refined the earlier boundary model by splitting `MOD-MIGRATIONS` from `MOD-RECOVERY`, then registered `MOD-SECRET-ENVELOPE` and `MOD-RESOURCE-GOVERNOR`; the accepted target has 50 authorities.
- Replaced temporary R0 production containment with final constraint/fixture/harness work; each product responsibility is implemented once in its final module.
- Expanded previously parameterized domain, Redis, protocol, body, endpoint, response, Admin, frontend and harness families into exact work units.
- Defined the reusable module entry/implementation/integration/legacy-deletion/post-deletion loop without personnel or calendar gates.
- Defined absolute capacity/SLO, relative regression, operation budgets, resource recovery, stable-sample, real-upstream cap and whole-candidate stability requirements.
- Defined target-only isolation, no duplicate side effects, complete migration/adoption, both frontend rewrites, final candidate closure, exact cutover, rollback and compatibility-state contraction.
- Preserved three reviewed document deletions and two coherent archive batches with provenance/recovery; protected the remaining legacy set from bulk deletion.

## Current

This plan is superseded. No production target code was implemented under it, and no `implementation-status.md` should be created for it.

The [2026-07-12 final-plan review](history/final-plan-readiness-review-2026-07-12.md) proves that the Rust specification was internally reviewed; it is not readiness evidence for the replacement Go system. Use the [supersession record](history/superseded-by-greenfield-ai-gateway-2026-07-13.md) for the authority mapping.

## Historical Implementation Order

The following are dependency groups inside one implementation, not phased releases or separately approved scope. Exact rows live in the [modular work map](indexes/execution-slice-map.md).

| Order | Dependency group | Complete target output |
| ---: | --- | --- |
| 0 | R0 constraints, fixtures and final harnesses | Final architecture/contract/load harnesses and sanitized safety corpora; no temporary legacy production change |
| 1 | R1 kernel/runtime/protocol/observability/diagnostics/secrets/resources | Bounded primitives, one runtime capture/narrow views, Anthropic types, typed telemetry, versioned secret envelope, weighted resource governor and final diagnostics |
| 2 | R2 migrations/state/config/auth/catalog/journal | Separate migration runner and recovery authorities, exact domain manifests/repositories/Redis classes, CAS/auth/catalog/terminal journal/shared Files store |
| 3 | R3 usage/prompt cache | Distinct usage facts, idempotent batching/rebuild and bounded shared cache evidence |
| 4 | R4 proxy/scheduler/credential/pool lifecycles | Final finite local/external schedulers and separated resource/secret/outcome authorities |
| 5 | R5 upstream protocols/adapters/replay | Secure bounded Kiro/external attempts and conservative replay/commitment policy |
| 6 | R6 planning/artifacts/payload/Files/media/endpoints | Route-before-work, lazy revisioned artifacts, exact body profiles, shared Files, bounded media/token and thin endpoints |
| 7 | R7 SSE/response/terminal/Messages transport | Canonical response profiles, one terminal lifecycle and thin public Messages transport |
| 8 | R8 Admin/generated contract/browser/both UIs | Nine backend domains and eleven complete workflows in each maintained app |
| 9 | R9 lifecycle/recovery/real clients/release | Producer-aware lifecycle, honest readiness, signed expected-instance generation fencing, RPO/RTO recovery, real client/browser/load integration and immutable release evidence |
| 10 | R10 final candidate/cutover/rollback/contraction | Zero legacy/stub residue, full post-deletion candidate, rehearsal, one activation, rollback window and post-contract full gates |

AI implementation may overlap work whose public contracts and state authorities do not conflict. Dependency order, exact state authority and target-only integration remain binding. Parallel coding does not create multiple production lanes.

## Work-State Rules

| State | Meaning |
| --- | --- |
| `Ready` | Final work-unit scope, dependencies, policy, gates and deletion conditions are specified |
| `Implementing` | Exact source mapping, target code, fixtures or focused evidence is being produced |
| `Integrated` | Target code is in the target-only candidate, legacy responsibility is removed and focused/post-deletion gates pass |
| `Verified In Candidate` | Applicable aggregate candidate gates also pass |
| `Blocked` | A discovered fact contradicts the accepted contract or required evidence cannot be produced |

Only the whole modernization has production states:

| System state | Meaning |
| --- | --- |
| `Implementation Not Started` | Current state; no target source work recorded |
| `Target Candidate In Progress` | One complete target-only system is being assembled |
| `Complete Candidate Verified` | All 50 modules and full post-deletion gates pass for one digest |
| `Cutover Ready` | Dress rehearsal, backup/migration/rollback artifact and every release gate pass |
| `Full-System Observation` | Complete target handles all production traffic; previous full artifact remains rollback-capable |
| `Complete` | Observation and compatibility-state contraction pass with final evidence |

There is no module-level production `Canary`, `Default On`, `Soaking`, rollback or `Done` state.

## Implementation Start

Do not start implementation from this roadmap. Begin through the [Greenfield AI Gateway work graph](../greenfield-ai-gateway/roadmap.md) in a separate target repository. Current Rust source characterization remains valid only as a pinned behavioral-oracle task.

## Production Cutover Blockers

These are expected because implementation has not started:

1. all 50 modules are Not Started and no target-only candidate exists;
2. no exact implementation audit, symbol mapping, migration/adoption artifact or target source evidence exists;
3. no focused/aggregate Rust/storage/protocol/UI/load/recovery/client/release gate has run;
4. no target backend/frontend/image digest, SBOM/signature/provenance or durable evidence manifest exists;
5. no full migration, previous-binary, backup/restore, Redis rebuild, cutover or rollback rehearsal exists;
6. no 60-minute/100,000-request candidate stability result or 24-hour production observation exists;
7. legacy source and compatibility state remain present because no replacement code has been implemented.

These block production release and completion, not plan-level implementation start.

## Deferred Product Scope

- Multi-user or multi-tenant support.
- Microservice or workspace-crate split before evidence justifies it.
- Public plugin ABI before an independent extension requires it.
- Retirement of either maintained Admin UI without a separate decision.
- Financial-grade billing guarantees beyond accepted usage/accounting requirements.

Production hardening is not deferred; decision 010 includes it in R9/R10.

## Documentation Follow-Up

- Resolve `DOC-002` by creating a current secret-safe Claude Code local-testing authority or removing the root link through an explicit documentation decision.
- Keep `docs/ai-docker-compose-deployment.md` as a dated legacy/current-release reference until the target deployment runbook replaces it; its authority warning prevents legacy examples from governing the target release.
- Keep the remaining `Archive Later` documents protected until a coherent authority-domain batch has provenance, inbound-reference review and recovery instructions.

These documentation items do not change target architecture. A release documentation/link gate still prevents false completion where they apply.
