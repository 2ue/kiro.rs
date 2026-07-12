# Documentation Archive

Role: Registry for preserved historical documentation outside the active plan tree

Status: Current archive index as of 2026-07-12

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
