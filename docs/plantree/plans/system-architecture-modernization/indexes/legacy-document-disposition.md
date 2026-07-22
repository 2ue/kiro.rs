# Legacy Document Disposition

Role: Legacy Markdown disposition, archive, and deletion record

Status: Current audit decision as of 2026-07-12

Authority: Records the three approved deletions, two completed archive batches, and the current keep, explicit-disposition, and later-archive boundaries; it does not authorize any unlisted deletion or bulk move

As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-12

Read when: Removing, moving, restoring, or reclassifying documentation outside `docs/plantree`

Related: [Documentation archive](../../../../archive/README.md), [Authority and source map](authority-and-source-map.md), [repository cleanup and filesystem plan](../topics/delivery/repository-cleanup-and-filesystem-plan.md), [plan root](../README.md)

## Audit Scope

The audit covered every Markdown file under `docs/` and separately reviewed active-looking tracked plans under `ui/`, with special attention to:

- documents outside `docs/plantree` that still look like active plans or current-state analysis;
- old `implementation-status.md` files and their proposed history replacements;
- Git tracked/untracked state;
- inbound Markdown links and literal repository references;
- current baseline or plan material that supersedes an older document;
- independent historical measurements, implementation records, test evidence, or external-project research that would be lost by deletion.

Before the approved deletions, `docs/plantree`-external inventory contained 79 Markdown files, all tracked by Git. The active working tree also contained untracked modernization-plan and baseline files. Those untracked files are unrelated to the three deletions and must not be removed by cleanup commands.

## Approved Deletions

Only the following three files are approved for deletion by this audit.

| File | Deletion basis | Last tracked source | Blob before deletion |
| --- | --- | --- | --- |
| `docs/origin-main-strategy-comparison.md` | Compared old HEAD `dcf1b1f9...` with then-uncommitted work; no inbound reference or independent validation evidence; current baseline now owns current-system facts | `ed377a300715b775f9a47534788ab5f61ff27410` | `75d84e0be64357e224cff32dd7239a8b2dfd9a16` |
| `docs/prompt-cache-strategy-refactor-analysis-20260701.md` | Analysis-only early draft; no inbound reference or landed evidence; superseded by the retained strategy-family analysis | `466c6645aa11957488012ea8bab7f8a2eea799ab` | `ff71890e00cc6c6d0d98c116f534c684a6e78cdd` |
| `docs/prompt-cache-strategy-pattern-refactor-analysis-20260701.md` | Analysis-only intermediate draft; no inbound reference or landed evidence; superseded by the retained strategy-family analysis | `466c6645aa11957488012ea8bab7f8a2eea799ab` | `52d113206a9cb6d7c7887b43188ae04cf8d9c0f6` |

The two prompt-cache drafts and the retained family document entered Git in the same commit. The retained document explicitly says that it corrects the preceding analysis, and the active authority/source map points only to the retained family document.

## Retained Family Document

Keep `docs/prompt-cache-strategy-family-refactor-analysis-20260701.md` as the final historical prompt-cache strategy analysis from that sequence. It remains a historical source, not current implementation authority. Current behavior comes from source and the dated baseline; accepted target behavior comes from accepted modernization decisions.

## Git History Rollback

Deletion does not erase Git history. If later evidence shows that one of these files is still required, restore its last tracked version without reverting unrelated work:

```bash
git restore --source=ed377a300715b775f9a47534788ab5f61ff27410 -- docs/origin-main-strategy-comparison.md
git restore --source=466c6645aa11957488012ea8bab7f8a2eea799ab -- docs/prompt-cache-strategy-refactor-analysis-20260701.md docs/prompt-cache-strategy-pattern-refactor-analysis-20260701.md
```

After restoration, re-run inbound-link, authority, and source-revision checks before treating a restored document as active.

## Keep

The following material remains at its current path:

- all of `docs/plantree/**`, including baseline, registered plans, accepted decisions, active roadmap state, and versioned history;
- `docs/plantree/plans/runtime-correctness-and-release-gates/implementation-status.md`, because that plan is still In Progress;
- the historical snapshots replacing the completed Admin and request-body plans' old active status files;
- `docs/frontend-dev-environment.md`, because the repository README links it as the current frontend development entrypoint;
- `docs/testing/loadtest.md`, because the load/chaos validation skill and existing test guidance use it as the current command reference;
- `docs/analysis/prod-slow-first-token-root-cause-20260706.md`, because the active resource/concurrency baseline cites its dated production measurements.
- `feature/issues/empty-tool-description-400-invalid-tool-use-format.md` and `feature/issues/tool-property-key-invalid-400-tool-schema-invalid.md`, because they are newly retained current evidence for `COR-006`/`COR-007`; their proposed fixes are non-authoritative under decision 012.

Moving a Keep item later requires updating its active inbound references in the same change and preserving any evidence identity.

## Keep Until Target Replacement

### AI Docker Compose Deployment Guide

`docs/ai-docker-compose-deployment.md` is an action-oriented guide for the legacy production release, not target-system authority. It remains in place because the legacy release stays production-authoritative throughout target construction and no checked-in target deployment runbook exists yet. A dated authority warning now prevents its `latest`, old-version example, startup-migration, health, secret and hardening guidance from being mistaken for the accepted target contract.

`R9.4` must generate the supported target runbook from checked-in deployment manifests and accepted readiness, backup/restore, upgrade, rollback and security contracts. After the replacement passes `G-OPS`, `G-SUP`, `G-EVID` and inbound-reference checks, archive the legacy guide with source commit/blob/recovery metadata or delete it through a new explicit disposition if it contains no independent evidence. It must not silently become the target runbook through incremental edits.

### Broken Claude Code CLI Link

`README.md` links to `docs/claude-code-cli-local-testing.md`, but that file does not exist. This was the only missing relative Markdown target found by the repository-wide link audit. `MOD-REAL-CLIENT-HARNESS` and the documentation authority must produce the current secret-safe workflow during R9 or remove the unsupported root entry through an explicit documentation decision. This audit does not silently redirect the link to a historical regression report.

## Archived Batches

- [Request body modularization, 2026-07-06](../../../../archive/request-body-modularization-20260706/README.md): archived on 2026-07-12 as one coherent batch. The collection preserves the original paths, source commits, blobs, inbound-reference audit, current authority, and recovery commands for the request-pipeline analysis and request-body capability companion plan.
- [Operator UI planning, 2026-06 to 2026-07](../../../../archive/ui-planning-2026-06-to-07/README.md): archived on 2026-07-12 as one coherent frontend-planning batch. The collection preserves eight original paths/blobs, distinguishes landed/partial/superseded/unresolved material, routes unresolved candidates to R8 entry audits, records deleted companion provenance, and retains scoped reversal commands.

No other `Archive Later` item was moved or deleted in either batch.

## Archive Later

The remaining 70 unarchived plantree-external documents should be archived rather than deleted. They no longer own current facts or active execution order, but they contain dated production observations, implementation records, test evidence, protocol investigations, public-contract rationale, or external-project research.

### Whole Directories Or Evidence Files

- `docs/analysis/*.md`, except the currently retained `prod-slow-first-token-root-cause-20260706.md`;
- `docs/kiro-optimization-plans-20260626/**`;
- `docs/kiro-proxy-study-20260626/**`;
- `docs/scheduler-dispatch/**`;
- `docs/testing/claude-code-cli-full-regression-20260628.md`.

### Request And Protocol History

- `docs/anthropic-tools-signature-compatibility-analysis.md`;
- `docs/claude-code-kiro-dialogue-disconnect-investigation-20260630.md`;
- `docs/claude-code-kiro-dialogue-observability-and-optimization-plan-20260630.md`;
- `docs/kiro-400-improperly-formed-request-analysis.md`;
- `docs/kiro-cli-capture-protocol-completeness-analysis-20260702.md`;
- `docs/kiro-compatible-image-passthrough-analysis-20260705.md`;
- `docs/kiro-context-window-payload-threshold-full-analysis.md`;
- `docs/kiro-official-image-5mb-multimage-investigation-20260702.md`;
- `docs/kiro-protocol-local-before-after-test-runbook.md`;
- `docs/kiro-small-payload-improperly-formed-fix-plan.md`;
- `docs/kiro-upstream-protocol-refactor-analysis-and-test-plan.md`;
- `docs/kiro-upstream-real-protocol-malformed-and-context-20260617.md`;
- `docs/request-entry-errors-and-missing-max-tokens.md`.

### Cache, Usage, And Production Error History

- `docs/cache-behavior-analysis.md`;
- `docs/current-cache-strategy-issues-readable-20260701.md`;
- `docs/external-pool-billing-floor-and-cost-analysis.md`;
- `docs/ha-external-pool-usage-projection-analysis.md`;
- `docs/high-cache-token-amplification-strategy.md`;
- `docs/high-cache-upstream-simulation-analysis.md`;
- `docs/prod-usage-error-evidence-20260630.md`;
- `docs/production-error-optimization-plan.md`;
- `docs/prompt-cache-scope-and-kiro-rs-tool-parity.md`;
- `docs/prompt-cache-simulation-strategy.md`;
- `docs/prompt-cache-strategy-family-refactor-analysis-20260701.md`.

### Scheduler, State, External Pool, And Runtime History

- `docs/credential-list-data-plane-optimization-design.md`;
- `docs/credential-rate-limit-and-scheduler-optimization.md`;
- `docs/credential-scheduler-hotpath-performance-analysis.md`;
- `docs/external-fallback-pools-design.md`;
- `docs/redis-pgsql-migration-optimization-analysis.md`;
- `docs/redis-pgsql-state-model-full-analysis.md`;
- `docs/runtime-usage-scheduler-performance-fix-20260620.md`.

### External Comparison And Implementation History

- `docs/foxfishc-learning-analysis.md`;
- `docs/kiro-account-manager-enhanced-implementation-log.md`;
- `docs/kiro-account-manager-enhanced-learning-analysis.md`;
- `docs/kiro-gateway-account-manager-learning-analysis.md`.

`Archive Later` is a preservation requirement, not permission to move the files now. A later archive change must assign an authoritative history/reference index, retain source commit and evidence context, update every inbound Markdown and literal reference, and keep historical statements visibly distinct from current facts and accepted decisions.

## Why No Bulk Move Was Performed

No bulk move accompanies this audit because:

- the modernization plan is implementation-ready, but it intentionally has not assigned a durable history/reference destination and complete provenance map for every remaining legacy domain;
- several legacy documents are linked by the active authority map, baseline, README, validation skill, or other historical evidence;
- many files contain unique measurements or implementation/test results that have not yet been summarized into a registered history index;
- the working tree already contains unrelated untracked modernization files that must be protected from broad cleanup;
- a mass move would create large path churn, obscure the three evidence-backed deletions, and make link/ownership review harder.

Archive one coherent technical-authority domain at a time. For each batch, inventory inbound references, preserve Git history, add or update a registered index, verify relative links/anchors, and record which active source supersedes the archived material.

## Safety Boundary

This audit authorizes no deletion beyond the three files listed under Approved Deletions. In particular, it does not authorize deleting tracked history, raw evidence that lacks a durable summary, untracked modernization files, registered worktrees, ignored validation data, or operational resources. Those surfaces retain the ownership and manifest requirements of the repository cleanup plan.
