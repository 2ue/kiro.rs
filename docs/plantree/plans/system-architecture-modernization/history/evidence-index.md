# Modernization Evidence Index

Role: Versioned module-integration and complete-system gate evidence registry

Status: Empty registry; no modernization module or gate has run

Authority: Indexes reproducible execution evidence only; plans, current-defect evidence, historical reports and ignored raw artifacts are not closure evidence

As of: 2026-07-12

Read when: Recording a module result, evaluating a candidate/finding, freezing the target candidate, deleting legacy source, rehearsing cutover/rollback, or closing the modernization

Related: [Plan root](../README.md), [Roadmap](../roadmap.md), [Traceability](../indexes/traceability-matrix.md), [Rewrite inventory](../indexes/rewrite-inventory.md), [Verification/evidence manifest](../topics/delivery/verification-rollout-and-rollback.md#g-evid-durable-evidence-manifest), [Repository hygiene](../topics/delivery/repository-cleanup-and-filesystem-plan.md)

## Current State

No target implementation has started, no module is Integrated, no complete candidate exists and no modernization gate has a result. The registry contains no `EVID-*` records.

Historical evidence under other plans may characterize current behavior and constrain compatibility. It does not close this modernization unless a future record identifies the exact applicable source, contract and reproducible result.

## Evidence Identity

Use:

```text
EVID-YYYYMMDD-<WORK-OR-SYSTEM>-<GATE>-<SEQUENCE>
```

Shape examples only; these are placeholders, not scheduled dates or reserved IDs:

```text
EVID-YYYYMMDD-R2-4-AUTH-G-C-NN
EVID-YYYYMMDD-SYSTEM-G-PERF-NN
```

Allocate an ID only after a run/review result exists. Do not reserve planned IDs or create passing records that point only to a test plan.

## Required Record

Each record includes:

- evidence ID, result, date, exact work unit/module or `SYSTEM` scope, gate and affected finding/inventory rows;
- full source commit/version/tree patch identity, target-only candidate or complete release digest and both frontend artifact digests where applicable;
- exact commands, tool/dependency versions, sanitized config hashes, workload/corpus/report-schema versions, host and target-process identity;
- isolated PgSQL/Redis/Files/diagnostic/browser/CLI/network/process resources and cleanup manifest;
- expected thresholds and actual pass/fail/blocked/partial/exception result;
- complete offered/launched/completed/classified counts and valid sample populations;
- protocol/state/operation/latency/resource/recovery/cost/deletion/cutover/rollback summaries applicable to the gate;
- artifact paths, bytes/files/digests/retention and secret scan;
- prior failed/invalid/blocked runs and adjudication;
- links to decisions, requirements, findings, module/work-unit/inventory rows.

No human owner, reviewer name, due date, estimate, canary percentage or module production-selector state is required. Evidence authority comes from exact reproducible identity and results.

## Result Vocabulary

| Result | Meaning |
| --- | --- |
| `Passed` | The complete named gate passed for the exact revision/scope with no unexplained prior failure |
| `Failed` | The gate found a product, target, harness, cleanup or evidence failure |
| `Blocked` | A prerequisite, environment, dependency or safe-run condition prevented a valid result |
| `Partial` | Some scenarios ran, but the complete gate did not; never counts as Passed |
| `Exception` | An explicitly documented scoped deviation; never silently converts the underlying gate to Passed |
| `Superseded` | A newer linked result replaces applicability for a later revision without rewriting history |

## Index

| Evidence ID | Work/System | Gate | Source | Result | Scope / Findings | Artifact / Record |
| --- | --- | --- | --- | --- | --- | --- |
| None | R0-R10 / SYSTEM | All | `e9479df` planning baseline | Not run | No modernization closure evidence | None |

## Acceptance Rules

- Current-code evidence proving a problem belongs in the finding document, not as a passing closure record.
- Raw ignored reports cannot be the only evidence for roadmap/finding/module/system completion.
- One passing retry never erases an unexplained failed run.
- Dirty-tree evidence identifies the exact patch/tree state or remains non-attributable.
- A workbook, screenshot, binary, image or report is not registered until its digest, schema/content summary, source identity and cleanup/retention are recorded.
- Module `Integrated`/`Verified In Candidate` states link focused, integration and legacy-deletion evidence.
- Finding closure and rewrite-inventory completion link exact required evidence.
- Only `SYSTEM` records can prove final cutover, rollback observation, compatibility-state contraction or complete modernization.
- The target release has no module-level production switch/canary/soak evidence because those states are prohibited by decision 009.

## Maintenance

Append evidence; do not rewrite older results to look current. Move detailed superseded logs behind links while preserving this index. Keep the active roadmap/status concise and free of raw run output.
