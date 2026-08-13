# Superseded By Greenfield AI Gateway

Role: Target-authority supersession and preservation record

Status: Accepted

Date: 2026-07-13

Source plan: [System architecture modernization](../README.md)

Replacement plan: [Greenfield AI Gateway](../../greenfield-ai-gateway/README.md)

## Reason

The source plan was completed as a specification for a different target: a complete first-party Rust rewrite, two retained Admin frontends, a fixed 50-module technical-authority ledger, legacy-compatible schema/state migration and one final Rust-system cutover.

The user replaced that target with a new general AI model gateway implemented in a separate repository using Go plus one React/TypeScript/Tailwind Admin application. Kiro remains mandatory in the first release but becomes one vertical provider module. Future upstream providers or API gateway integrations may reuse default scheduling/usage/conversion implementations or supply their own implementations behind versioned contracts.

No production implementation had started under the source modernization plan, so no target source or migration artifact is abandoned by this authority change.

## Authority Mapping

| Source material | New classification | Replacement/use rule |
| --- | --- | --- |
| Current Rust source, tests, fixtures and baseline maps | Current-system behavioral oracle | Characterize behavior at a pinned revision; do not copy packages as target architecture |
| Problem catalog and risk evidence | Reference and acceptance input | Reproduce relevant risks and convert them into Go target gates |
| Decisions 003-006 on replay, terminal, lease and shutdown | Semantic invariant source | Translate the invariants into new typed contracts; old Rust ownership/module names are non-binding |
| Rust target architecture and 50-module ledger | Superseded target design | Do not implement |
| Two-frontend target and Rust-to-TypeScript generation | Superseded target design | Replaced by one new React/Tailwind application and Go-owned generated OpenAPI client |
| Legacy schema migration and target-only Rust integration graph | Superseded delivery design | Replaced by a separate target store, isolated import tool and whole-system Go cutover |
| Historical real-client/load/release evidence | Dated reference evidence | Reproduce applicable scenarios on one immutable Go candidate |

## Filesystem Disposition

The complete source plan remains in place as archive/reference material because it contains dense source links, problem evidence and decisions that are still useful for behavior characterization. This pass does not delete or mass-move it.

A later archive pass may move the plan only after:

1. inbound Markdown links are inventoried and updated or bridged;
2. every retained behavioral source is linked from the greenfield plan or baseline;
3. Git source/recovery information is recorded;
4. no active runtime-correctness plan still depends on its current path.

Until then, the replacement plan wins every target-architecture, technology, frontend, module and implementation-order conflict.
