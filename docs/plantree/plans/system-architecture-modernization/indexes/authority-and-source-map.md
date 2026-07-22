# Authority And Source Map

Role: Conflict-resolution and legacy-source index for the system architecture modernization plan

Status: Current as of 2026-07-12

Authority: Classifies which durable source answers each question and how older analysis may be reused

Read when: Two documents disagree, an old design appears relevant, or a finding must be traced to its owning plan

Related: [Plan root](../README.md), [Plan Tree authority](../../../README.md), [Topic index](../topics/README.md), [Decision index](../decisions/README.md), [Legacy document disposition](legacy-document-disposition.md)

## Authority Matrix

| Information type | Winning durable source | Rule |
| --- | --- | --- |
| Supported product boundary and business invariant | [Business and product context](../../../baseline/business-context.md), unless explicitly superseded by an accepted decision | A conversation-only assumption is not durable until recorded |
| Exact current behavior | Source code, schema, configuration, and tests at the named revision | Refresh a dated baseline instead of editing history to look current |
| Current component or flow map | Relevant file in the [baseline index](../../../baseline/README.md) | Baseline describes current state, never an unlanded target |
| Target architecture | Accepted decision record | A proposed topic explains options but cannot silently become binding |
| Scope and execution order | Owning plan README and roadmap | One plan owns each deliverable; related plans contribute constraints |
| Current implementation handoff | `implementation-status.md` for an In Progress plan | Handoff does not replace roadmap or evidence |
| Completion or validation | Versioned history/evidence with date and source revision | Ignored local artifacts may support but cannot solely prove the claim |
| Historical rationale | Classified legacy source below | Current code and accepted decisions win when historical behavior conflicts |

## Existing Plan Ownership

| Existing plan | Retained authority | Modernization relationship |
| --- | --- | --- |
| [Request body capability modularization](../../request-body-capability-modularization/README.md) | Landed body capabilities, raw/normalized contracts, compatibility defaults, validation | Reference its contracts; own only later cross-system orchestration and migration |
| [Admin observability, routing model support, and config IA](../../admin-observability-routing-config/README.md) | Exact search, model eligibility, retry semantics, configuration grouping, validation | Reference its behavior; own later control-plane boundaries and generated contracts |
| [Runtime correctness and release gates](../../runtime-correctness-and-release-gates/README.md) | Landed correctness/lifecycle fixes, release gates, dated hardening gaps and evidence | Preserve its outcomes; final modernization hardening is mandatory under decision 010, and an incomplete historical gate is never reinterpreted as passed |

If modernization needs to change a retained contract, create an accepted decision that links the owning plan, identifies the superseded clause, and defines compatibility, target-only integration, final activation, whole-system rollback, and replacement evidence.

## Classified Legacy Sources

This section classifies how retained historical sources may be reused. Keep/archive/delete status is owned by the [legacy document disposition](legacy-document-disposition.md); authority classification and filesystem disposition must remain consistent but answer different questions.

| Source | Classification | Reuse rule |
| --- | --- | --- |
| [Request pipeline modularization analysis](../../../../archive/request-body-modularization-20260706/request-pipeline-modularization-analysis-20260706.md) | Archived historical landed analysis | Use for rationale and earlier file split; current request-body plan and code own behavior |
| [Request body capability plan](../../../../archive/request-body-modularization-20260706/request-body-capability-modularization-20260706.md) | Archived historical companion plan | Use its dated evidence through the registered request-body plan; do not maintain a second active roadmap |
| [Operator UI refactor plan](../../../../archive/ui-planning-2026-06-to-07/REFACTOR_PLAN.md) | Archived, partially landed frontend rationale | Reuse IA/workflow rationale only after current-source characterization; its one-UI, no-backend-change and persistent-Admin-key assumptions are superseded |
| [Dashboard enhancement collection](../../../../archive/ui-planning-2026-06-to-07/dashboard-enhancement/README.md) | Archived frontend candidate source | Re-audit unfinished health/analytics ideas in exact R8 slices; the historical roadmap is neither accepted scope nor completion evidence |
| [Credential scheduler hot-path analysis](../../../../credential-scheduler-hotpath-performance-analysis.md) | Historical implementation/performance analysis | Revalidate hot-path claims against current manager/storage code before reuse |
| [Redis and PgSQL state-model analysis](../../../../redis-pgsql-state-model-full-analysis.md) | Reference analysis, partially stale | Reuse evidence and options; current code, baseline, runtime-correctness plan, and accepted state decisions win |
| [Runtime usage and scheduler performance fix](../../../../runtime-usage-scheduler-performance-fix-20260620.md) | Historical implementation record, partially superseded | Its Redis-failure local fallback and background-only release semantics are superseded by the runtime-correctness plan's fail-closed and critical-release behavior |
| [Scheduler dispatch redesign](../../../../scheduler-dispatch/README.md) | Historical landed behavior index | Preserve external scheduler semantics unless an accepted decision and characterization tests change them |
| [Kiro optimization plans](../../../../kiro-optimization-plans-20260626/README.md) | Historical proposal collection | Use as an idea/source catalog; do not infer that every proposal remains active or unimplemented |
| [External fallback pools design](../../../../external-fallback-pools-design.md) | Historical contract/design source | Use to trace public configuration intent such as `preservePath`; current API contract and code reveal any implementation drift |
| [Prompt-cache strategy family analysis](../../../../prompt-cache-strategy-family-refactor-analysis-20260701.md) | Historical cache analysis | Reuse examples and policy rationale; target usage/cache contracts must restate accepted semantics without copying stale implementation detail |
| [Current cache strategy issues](../../../../current-cache-strategy-issues-readable-20260701.md) | Historical problem analysis | Reproduce unresolved claims before promoting them into the current problem catalog |
| [Kiro proxy study](../../../../kiro-proxy-study-20260626/README.md) | External-project research | Use for alternatives, never as current-project authority |
| [Empty tool description/null schema reproduction](../../../../../feature/issues/empty-tool-description-400-invalid-tool-use-format.md) | Current retained finding evidence | Use for `COR-006` reproduction/source facts; decision 012 overrides its remediation suggestion |
| [Invalid tool property-key reproduction](../../../../../feature/issues/tool-property-key-invalid-400-tool-schema-invalid.md) | Current retained finding evidence | Use for `COR-007` reproduction/source facts; decision 012 overrides its blanket-renaming suggestion |

## Known Drift And Resolution State

| Drift | Current resolution |
| --- | --- |
| The root registry described request-body work as inventory/in progress after implementation and cleanup completed | Root registry, request-body README, and roadmap now classify the scope as implemented and maintained |
| The root registry described the Admin plan as in progress after its scoped implementation completed | Root registry now classifies it as implemented and locally verified |
| Historical scheduler documentation permits implicit local fallback when Redis is unavailable | Superseded for normal multi-replica behavior by the registered runtime-correctness plan; any explicit single-process degradation mode requires a new accepted decision |
| Historical test totals can look like the current suite size | Keep them as dated evidence only; new evidence appends its source revision and does not rewrite old counts |
| Runtime-correctness requires a complete Docker gate, while a later tag exists after an explicitly requested one-time validation exception | The Docker gate remains incomplete; the tag is not evidence of a pass. The exception is durably recorded in [release-exception-v0.0.102.md](../../runtime-correctness-and-release-gates/history/release-exception-v0.0.102.md) and applies only to that version |
| Validation reports under `target/loadtest` are referenced by status documents but ignored by Git | Preserve a sanitized, versioned summary, command, exit status, source revision, and artifact manifest; treat raw reports as ephemeral supporting data |
| Earlier audit discussion treated API keys or file objects as tenant boundaries | Rejected by the current [single-operator product boundary](../../../baseline/business-context.md); retain only capacity, lifecycle, secret-handling, and external-boundary risks |

## Source Promotion Rules

1. Reproduce or verify a legacy claim against the current revision before adding it to the active problem catalog.
2. Assign a stable problem identifier, severity, technical-authority boundary, impact, evidence, and acceptance criteria.
3. Link rather than copy historical rationale.
4. Put alternative solutions in a proposed topic; record the selected choice as a decision only after acceptance.
5. Add implementation sequencing to the roadmap only after dependencies and exit criteria are defined.
6. Add completion evidence to versioned history; do not turn a raw local report directory into the plan's sole record.
