# Feature Workspace

Role: Active issue/evidence workspace for current `kiro.rs` investigations, validations, and release records

Status: Current workspace index as of 2026-07-28

Authority: This directory preserves issue documents, evidence summaries, tests, audits, and release records. Current planning authority lives in [`docs/plantree/`](../docs/plantree/README.md), especially the [Rust Runtime Scheduler Stabilization](../docs/plantree/plans/rust-runtime-scheduler-stabilization/README.md) plan.

## Current Reading Order

1. [Rust Runtime Scheduler Stabilization plan](../docs/plantree/plans/rust-runtime-scheduler-stabilization/README.md): current scheduler/runtime/fallback/signature/document-disposition authority.
2. [Single issue index](issues/README.md): active and retained issue documents.
3. [Evidence index](evidence/README.md): validation summaries, build hashes, production evidence references, and dated test results.
4. [Audit index](audits/README.md): historical fact matrices, migration maps, and production read-only summaries.
5. [Test index](tests/README.md): validation runners and documentation checks.
6. [Release index](releases/README.md): published version records and release notes.

## Historical Root Files

The old root-level feature final report, implementation handoff, and remediation plans were archived because their status headers referred to earlier v0.0.114-v0.0.118 checkpoints and could be mistaken for current state:

- [`feature/final-report.md`](../docs/archive/runtime-correctness-feature-workspace-history/final-report.md)
- [`feature/implementation-status.md`](../docs/archive/runtime-correctness-feature-workspace-history/implementation-status.md)
- [`feature/plans/**`](../docs/archive/runtime-correctness-feature-workspace-history/plans/README.md)
- [`feature/release-114-hardening/**`](../docs/archive/release-114-hardening-history/release-114-hardening/README.md)

Use those archived files only for provenance. Do not use their old `NO-GO`, candidate, or release-status wording as current truth.

## Retention Rules

- Keep `feature/issues/**` for current or retained problem documents until each issue is either resolved with evidence or explicitly archived.
- Keep `feature/evidence/**` as dated evidence; do not rewrite old evidence as current pass status.
- Keep `feature/audits/**` as historical/current-fact inputs. Promote only current conclusions into plan-tree topics or issue files.
- Keep `feature/tests/**` for validation scripts and documentation link checks.
- Archive completed release-specific bundles once their active follow-up items have moved into issues/evidence/plan-tree.
