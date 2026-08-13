# Open Questions

Role: Registry of architecture questions that still block the final implementation specification

Status: No blocking architecture question is open as of 2026-07-12

As of: 2026-07-12

Authority: Contains unresolved choices only; implementation measurements and evidence gaps are not architecture questions

Related: [Roadmap](roadmap.md), [Decision index](decisions/README.md), [Fixed operational policies](decisions/010-fixed-operational-and-acceptance-policies.md), [Requirements](topics/requirements-and-quality-attributes.md)

## Current State

There are no unresolved architecture questions blocking target implementation.

The former `Q-001` through `Q-013` set is resolved by [decision 010](decisions/010-fixed-operational-and-acceptance-policies.md), with replay, terminal, scheduler, shutdown, modular architecture, migration, and delivery details also bound by decisions 003-009. Decisions 011-014 bind the later-discovered secret/resource authorities, tool-schema compatibility, transaction-local audit and release-generation/rollback-state details without reopening an architecture question. The `Q-*` identifiers remain stable in their decision headings and traceability references.

Implementation still has to produce pinned-revision symbol maps, characterized fixtures, workload manifests, measured reports, migration/adoption artifacts, and release evidence. Those are required execution outputs with fail-closed acceptance rules; they are not invitations to redesign the system or choose permissive defaults while coding.

Any newly discovered fact that would change a core contract is first recorded in the finding-candidate ledger. Add a new `Q-*` entry here only when no accepted conservative behavior can preserve correctness and the choice materially changes product or architecture scope.
