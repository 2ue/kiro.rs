# Request Body Modularization Archive

Role: Provenance and retrieval index for the 2026-07-06 request/body modularization documents

Status: Archived on 2026-07-12; not an active roadmap or current-state baseline

Authority: Preserves historical rationale, implementation notes, and dated validation context only

Read when: Tracing why the earlier request/body boundaries were introduced or comparing them with the registered plan and current source

Related: [Documentation archive](../README.md), [registered request-body plan](../../plantree/plans/request-body-capability-modularization/README.md), [modernization authority map](../../plantree/plans/system-architecture-modernization/indexes/authority-and-source-map.md)

## Current Authority

- Current implementation behavior: source, schema, configuration, and tests at the revision being reviewed.
- Current project-wide facts: the relevant [baseline](../../plantree/baseline/README.md) documents.
- Landed request/body capability contracts and maintenance state: the [registered request-body plan](../../plantree/plans/request-body-capability-modularization/README.md).
- Later cross-system request planning, ownership, and rewrite sequencing: the [system architecture modernization plan](../../plantree/plans/system-architecture-modernization/README.md) and accepted decisions.

The archived documents must not be read as a second active implementation plan.

## Archived Documents

| Archived document | Original path | Historical status | Last source commit | Pre-move blob |
| --- | --- | --- | --- | --- |
| [Request pipeline modularization analysis](request-pipeline-modularization-analysis-20260706.md) | `docs/request-pipeline-modularization-analysis-20260706.md` | Historical landed analysis for the first file-level split | `29480fa2a563b49cde3af9ff4d1e361d7715339c` | `de9080c4fb7b37db6c0dc3ee88c1c7c88554f2bc` |
| [Request body capability modularization plan](request-body-capability-modularization-20260706.md) | `docs/request-body-capability-modularization-20260706.md` | Implemented and validated historical companion plan | `ab2def5208b080866395328dd6127ada0ec80575` | `69ce9ad67d7ebc9283fb42e60eb0654c910fa6a5` |

## Inbound Reference Audit

The pre-move repository audit found these inbound references:

- the body-capability document referenced the pipeline analysis;
- the modernization authority/source map linked both documents;
- the legacy-document disposition listed both documents under `Archive Later`;
- no repository README, skill, source file, script, CI workflow, or active runbook referenced either old path.

The archive change updates all three reference locations. A post-move repository search must find no active link to either original path; old paths may remain only as provenance metadata and recovery commands inside this collection.

## Recovery And Reversal

To reverse the path move while preserving the archived content and Git history:

```bash
git mv docs/archive/request-body-modularization-20260706/request-pipeline-modularization-analysis-20260706.md docs/request-pipeline-modularization-analysis-20260706.md
git mv docs/archive/request-body-modularization-20260706/request-body-capability-modularization-20260706.md docs/request-body-capability-modularization-20260706.md
```

To recover the exact pre-archive tracked content after moving it back:

```bash
git restore --source=29480fa2a563b49cde3af9ff4d1e361d7715339c -- docs/request-pipeline-modularization-analysis-20260706.md
git restore --source=ab2def5208b080866395328dd6127ada0ec80575 -- docs/request-body-capability-modularization-20260706.md
```

Do not run the restore commands over a newly created document that has reused an original path without first reviewing that file.
