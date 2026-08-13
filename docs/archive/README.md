# Documentation Archive

Role: Registry for preserved historical documentation outside the active plan tree

Status: Current archive index as of 2026-07-28

Authority: Locates archived rationale and evidence; archived documents do not override current source, baseline, registered plans, or accepted decisions

Read when: Retrieving a moved historical analysis, auditing archive provenance, or considering later deletion

Related: [Plan Tree](../plantree/README.md), [legacy document disposition](../plantree/plans/system-architecture-modernization/indexes/legacy-document-disposition.md)

## Archive Rules

- Archive one coherent owner domain at a time and preserve original paths in the collection index.
- Record the source commit or blob and a recovery command before moving tracked material.
- Update every repository inbound reference in the same change.
- Keep historical status and evidence intact; do not turn an archive into a second active roadmap.
- Current code and dated baseline own current behavior. Registered plans and accepted decisions own active scope and target choices.
- Archive placement is not permission to delete. Later deletion requires a fresh reference, authority, and independent-evidence review.

## Collections

| Collection | Archived | Scope | Current authority |
| --- | --- | --- | --- |
| [Request body modularization, 2026-07-06](request-body-modularization-20260706/README.md) | 2026-07-12 | File-level request-pipeline split and explicit body-capability plan | [Registered request-body plan](../plantree/plans/request-body-capability-modularization/README.md) and current source |
| [Operator UI planning, 2026-06 to 2026-07](ui-planning-2026-06-to-07/README.md) | 2026-07-12 | Partially landed frontend refactor rationale plus unresolved dashboard/analytics candidates | Current `ui`/`admin-ui` source and the [modernization Admin/frontend architecture](../plantree/plans/system-architecture-modernization/topics/architecture/admin-and-frontend-architecture.md) |
| [Slow first token and stream fluidity analysis, 2026-06-29 to 2026-07-09](slow-first-token-and-stream-fluidity-20260629-20260709/README.md) | 2026-07-28 | Historical slow-first-token, stream-fluidity, and Kiro/sub2api correlation analysis | [Rust Runtime Scheduler Stabilization](../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, and dated evidence |
| [Kiro proxy study and optimization plans, 2026-06-26](kiro-proxy-study-and-optimization-20260626/README.md) | 2026-07-28 | Historical external-project comparison plus derived June optimization/implementation records | [Rust Runtime Scheduler Stabilization](../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, and dated evidence |
| [Scheduler dispatch redesign history](scheduler-dispatch-redesign-history/README.md) | 2026-07-28 | Historical implemented local credential scheduler dispatch strategy and July follow-up links | [Rust Runtime Scheduler Stabilization](../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, and dated evidence |
| [Request and protocol history](request-and-protocol-history/README.md) | 2026-07-28 | Historical request conversion, malformed payload, image, thinking/tool-signature, and protocol investigations | Current source, active `docs/analysis/*signature*`, and registered request/runtime plans |
| [Cache, usage, and production history](cache-usage-and-production-history/README.md) | 2026-07-28 | Historical cache, usage, cost, high-cache, external-pool billing, and production-error analysis | Current source, production evidence, active dashboard/usage issues, and runtime plan |
| [Scheduler, state, external pool, and runtime history](scheduler-state-external-pool-runtime-history/README.md) | 2026-07-28 | Historical local scheduler, external pool, Redis/PgSQL state, runtime usage scheduler, and credential data-plane analysis | [Rust Runtime Scheduler Stabilization](../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, and dated evidence |
| [External project learning history](external-project-learning-history/README.md) | 2026-07-28 | Historical FoxFishC/Kiro Account Manager/Kiro Gateway learning notes | Current source and any fresh comparison performed for a current issue |
| [Release 114 hardening history](release-114-hardening-history/README.md) | 2026-07-28 | Historical v0.0.114 post-upgrade production hardening package | [Rust Runtime Scheduler Stabilization](../plantree/plans/rust-runtime-scheduler-stabilization/README.md), current source, active issues, and current evidence |
| [Runtime correctness feature workspace history](runtime-correctness-feature-workspace-history/README.md) | 2026-07-28 | Former `feature/` root final report, implementation handoff, and remediation plans | [Rust Runtime Scheduler Stabilization](../plantree/plans/rust-runtime-scheduler-stabilization/README.md), active issues, active evidence, and current source |
