# Plan Tree

Role: Durable planning registry and authority entrypoint

Status: Current as of 2026-07-18

Authority: Defines how project-wide facts, accepted decisions, plan state, and historical evidence are retrieved

Read when: Starting architecture, implementation, validation, release, or plan-maintenance work

This directory is the single durable planning entrypoint for active architecture and validation work in this repository. A live user instruction governs the work in that conversation, but any requirement or exception that must survive the conversation must be recorded in the relevant requirement, decision, roadmap, or history document before the work is considered durably specified.

## Durable Authority By Question

| Question | Primary authority | Supporting material |
| --- | --- | --- |
| What product is this and which invariants must hold? | [Business and product context](baseline/business-context.md), followed by accepted plan decisions that explicitly supersede it | Plan requirements and protocol contracts |
| What does the current implementation do? | Current source code, schema, configuration, and tests at the referenced commit | Dated [baseline](baseline/README.md) maps and reproducible evidence |
| What should a target design do? | Accepted decision records in the owning registered plan | Proposed architecture topics, requirements, and roadmap |
| What is planned, active, done, or deferred? | The owning plan's `roadmap.md` | `implementation-status.md` is only a short handoff for work that is currently in progress |
| Which validation result proves a claim? | Versioned history or evidence indexed with its date and source revision | Raw reports under ignored or temporary directories are supporting artifacts, not durable authority |
| How should an older analysis document be used? | The nearest registered authority/source map | Unregistered documents under `docs/` are reference-only until classified |

Current-state facts and target-state decisions answer different questions and must not silently overwrite each other. When two durable sources conflict, record the drift in the owning plan, link both sources, and resolve it through a refreshed baseline or an explicit superseding decision.

## Reading Path

1. Read the [business and product context](baseline/business-context.md) for the current Rust product, single-operator trust model, business capabilities and compatibility invariants.
2. Read the [current system context](baseline/system-context.md), then only the baseline maps relevant to the change.
3. Select the owning registered plan below and read its `README.md` before its roadmap, topics, questions, decisions, or evidence.
4. For the new system target, begin with the [Greenfield AI Gateway plan](plans/greenfield-ai-gateway/README.md).
5. Use the [superseded Rust modernization plan](plans/system-architecture-modernization/README.md) only for historical design, verified findings and explicitly inherited semantic invariants.
6. Treat older analysis files as historical input unless the owning plan's source map gives them stronger authority; moved historical material is retrieved through the [documentation archive](../archive/README.md).

## Registered Plans

| Plan | Status | Current Phase | Last Landed | Next Target |
| --- | --- | --- | --- | --- |
| [Greenfield AI Gateway](plans/greenfield-ai-gateway/README.md) | Architecture Plan Ready For Review | New-repository Go/React target, module contracts, Kiro V1 scope, technology stack, references, complete work graph and acceptance gates documented; implementation Not Started | 2026-07-13: [complete reconstruction plan](plans/greenfield-ai-gateway/topics/complete-reconstruction-plan.md), [reference review](plans/greenfield-ai-gateway/topics/reference-projects-and-template-selection.md) and [decision 001](plans/greenfield-ai-gateway/decisions/001-greenfield-go-modular-ai-gateway.md) created | Accept the plan, choose the new repository name/location and create the target repository |
| [System architecture modernization](plans/system-architecture-modernization/README.md) | Superseded / Historical Reference | Rust target implementation never started; findings and selected invariants remain reference inputs for the greenfield plan | 2026-07-13: [supersession record](plans/system-architecture-modernization/history/superseded-by-greenfield-ai-gateway-2026-07-13.md) maps retained evidence and rejected target topology | No target implementation; preserve until a link-safe archive pass |
| [Runtime correctness and release gates](plans/runtime-correctness-and-release-gates/README.md) | In Progress | Runtime usage/error correctness follow-up before v0.0.103 release validation | 2026-07-13: [feature follow-up status](../feature/runtime-usage-error-followup-2026-07-13.md) records current fixes, `/ha` usage explanation, and remaining real-service gates; earlier static/storage/protocol/load/shutdown evidence remains indexed | Finish C0 + temporary local-service real-call validation, then complete the end-to-end Docker gate after the crates.io fetch timeout |
| [Request body capability modularization](plans/request-body-capability-modularization/README.md) | Implemented Base; Reasoning Fidelity Plan Ready | Current Rust protocol-correctness maintenance; implementation not started | 2026-07-06: capability plans, converter split, configuration, UI, and fake-upstream regression landed | Implement schema-driven per-attempt reasoning materialization, exact effort/thinking fidelity, operator injection/force controls, and full IDE/CLI validation |
| [Admin observability, routing model support, and config IA](plans/admin-observability-routing-config/README.md) | Implemented And Locally Verified | Maintenance | 2026-07-07: exact usage search, supported-model routing, bounded prompt retry, and UI grouping landed | Optional low-volume real-upstream smoke only when explicitly requested |

## Baseline

- [Baseline index](baseline/README.md)
- [Business and product context](baseline/business-context.md)
- [Current system context](baseline/system-context.md)
- [Module map](baseline/module-map.md)
- [Runtime flows](baseline/runtime-flows.md)
- [Storage and state](baseline/storage-and-state.md)
- [Protocol and API contracts](baseline/protocol-and-api-contracts.md)
- [Resource and concurrency model](baseline/resource-and-concurrency-model.md)
- [Deployment and operations](baseline/deployment-and-operations.md)
- [Test and release gates](baseline/test-and-release-gates.md)
- [Risk hotspots](baseline/risk-hotspots.md)

## Ideas

- [Inbox](ideas/inbox.md)

## Historical Documentation

- [Documentation archive](../archive/README.md): moved historical rationale/evidence that no longer owns current facts or active execution order.
- [Rust modernization supersession record](plans/system-architecture-modernization/history/superseded-by-greenfield-ai-gateway-2026-07-13.md): explains which old findings/invariants remain useful and which target choices were replaced.
- [Modernization authority and source map](plans/system-architecture-modernization/indexes/authority-and-source-map.md): classification of retained and archived legacy sources.
- [Legacy document disposition](plans/system-architecture-modernization/indexes/legacy-document-disposition.md): reviewed keep/archive/delete decisions, provenance and recovery rules.
