# Problem-To-Landing Traceability Matrix

Role: Cross-index from verified problems to requirements, target technical authorities, modular work units, decisions, verification gates, and closure evidence

Status: Accepted traceability baseline; all listed findings are Open, all implementation is Not Started, and no modernization closure evidence exists

Authority: Cross-reference only; this file does not override the problem catalog, requirements, accepted decisions, roadmap, rewrite inventory, verification contract, or versioned evidence

As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-12

Read when: Starting module work, reviewing scope, running a gate, attaching evidence, or closing a finding

Related: [Plan root](../README.md), [problem catalog](../topics/problems/README.md), [requirements](../topics/requirements-and-quality-attributes.md), [decision index](../decisions/README.md), [open questions](../open-questions.md), [roadmap](../roadmap.md), [target module ledger](target-module-ledger.md), [modular work map](execution-slice-map.md), [rewrite inventory](rewrite-inventory.md), [complete plan](../topics/delivery/migration-sequence.md), [verification](../topics/delivery/verification-rollout-and-rollback.md), [performance contract](../topics/delivery/performance-contract-and-workloads.md)

## Authority And State Rules

1. The detailed problem documents own finding evidence, severity, required target, acceptance conditions, and closure state. The catalog currently contains all 47 matrix IDs, including tool-boundary/schema, secret-at-rest, response-bound, egress, client-cache, admission, Redis-script, migration, and Admin-secret findings promoted by the 2026-07-12 audit.
2. The requirements document owns durable requirement and invariant text. `QA-*` labels are binding through decisions 003-014 and the accepted implementation/gate contracts.
3. Accepted decisions own target choices. Module references identify technical authority, never a human assignee.
4. The roadmap owns work/system state. The module ledger owns authority identities, the modular work map owns target-only implementation units, and the rewrite inventory owns source replacement/deletion coverage.
5. The verification document owns gate behavior. All 16 `G-*` contracts are Accepted but Not Run.
6. Only versioned history evidence tied to a source commit, exact command/configuration, result, artifact manifest, secret scan, and cleanup can close a gate. Current-code evidence that establishes a defect is not closure evidence.
7. Every row inherits accepted `D009`: one complete target-only modular implementation, legacy deletion, one final cutover and whole-system rollback. Rows marked `D001` also depend on the single-trust-domain boundary.
8. `Dependency work -> dependent work/gate` is implementation order, not phased production activation.
9. Status shorthand: `O` = Open finding, `NS` = target implementation Not Started, and `E-` = no closure evidence.
10. A row closes only after target behavior, focused/applicable complete-system gates, required cutover/rollback compatibility, legacy deletion and exact evidence all pass.

## Requirement And Decision Labels

| Label | Meaning and authority |
| --- | --- |
| `FUN-*`, `INV-*` | Stable identifiers already defined by [requirements and quality attributes](../topics/requirements-and-quality-attributes.md) |
| `QA-COMP-*`, `QA-REL-*`, `QA-PERF-*`, `QA-RES-*` | Binding compatibility, reliability, performance, and resource-safety clauses in the accepted requirements authority |
| `QA-SEC-*`, `QA-OBS-*`, `QA-MAINT-*` | Binding security, observability, and maintainability clauses in the accepted requirements authority |
| `QA-TEST-*`, `QA-EVID-*` | Binding test, documentation, link-integrity, and durable-evidence clauses in the accepted requirements authority |
| `QA-OPS-*`, `QA-SUP-*` | Binding operations/recovery and supply-chain clauses in the accepted requirements authority |
| `D001` | Accepted [single-user, single-trust-domain decision](../decisions/001-single-user-single-trust-domain.md) |
| `D002` | Superseded historical [module-by-module rollout decision](../decisions/002-complete-module-by-module-rewrite.md) |
| `D003`-`D008` | Accepted retry/commitment, terminal recovery, scheduler lifecycle, shutdown, module authority and migration/adoption decisions |
| `D009` | Accepted [single-program modular build and final cutover decision](../decisions/009-single-program-modular-build-and-final-cutover.md); inherited by every row |
| `D010` | Accepted [operational and acceptance policies](../decisions/010-fixed-operational-and-acceptance-policies.md); resolves `Q-001` through `Q-013` |
| `D011` | Accepted [secret-envelope/resource-governor authorities and exact ceilings](../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md) |
| `D012` | Accepted [tool-definition compatibility and reversible schema mapping](../decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md) |
| `D013` | Accepted [owner-transaction audit append contract](../decisions/013-owner-transaction-audit-acceptance.md) |
| `D014` | Accepted [release generation, recovery barrier and rollback-state contract](../decisions/014-release-generation-recovery-and-rollback-state.md) |

## Verification Gate Registry

These IDs provide stable traceability names. Gate contracts are Accepted; every runtime result remains Not Run.

| Gate | Current authority | Status |
| --- | --- | --- |
| `G-S` | [Static and architecture](../topics/delivery/verification-rollout-and-rollback.md#g-s-static-and-architecture-gate) | Accepted / Not Run |
| `G-C` | [Configuration, state, migration and storage](../topics/delivery/verification-rollout-and-rollback.md#g-c-configuration-state-migration-and-storage-gate) | Accepted / Not Run |
| `G-SCH` | [Scheduler](../topics/delivery/verification-rollout-and-rollback.md#g-sch-scheduler-gate) | Accepted / Not Run |
| `G-U` | [Usage, terminal, cache, audit and jobs](../topics/delivery/verification-rollout-and-rollback.md#g-u-usage-terminal-cache-audit-and-job-gate) | Accepted / Not Run |
| `G-A` | [Request, artifacts, media, Files and token](../topics/delivery/verification-rollout-and-rollback.md#g-a-request-artifact-media-files-and-token-gate) | Accepted / Not Run |
| `G-P` | [Upstream, retry, protocol, response and backpressure](../topics/delivery/verification-rollout-and-rollback.md#g-p-upstream-retry-protocol-response-and-backpressure-gate) | Accepted / Not Run |
| `G-CLI` | [Real Claude Code](../topics/delivery/verification-rollout-and-rollback.md#g-cli-real-claude-code-gate) | Accepted / Not Run |
| `G-UI` | [Admin and both frontends](../topics/delivery/verification-rollout-and-rollback.md#g-ui-admin-and-both-frontend-applications-gate) | Accepted / Not Run |
| `G-PERF` | [Performance/load/chaos/recovery](../topics/delivery/verification-rollout-and-rollback.md#g-perf-performance-load-chaos-and-resource-recovery-gate) and [performance contract](../topics/delivery/performance-contract-and-workloads.md) | Accepted / Not Run; current harness cannot pass until `TEST-004` is fixed |
| `G-ROLL` | [Final system cutover and rollback](../topics/delivery/verification-rollout-and-rollback.md#g-roll-final-system-cutover-and-rollback-gate) | Accepted / Not Run |
| `G-DEL` | [Legacy and compatibility deletion](../topics/delivery/verification-rollout-and-rollback.md#g-del-legacy-and-compatibility-deletion-gate) | Accepted / Not Run |
| `G-EVID` | [Durable evidence](../topics/delivery/verification-rollout-and-rollback.md#g-evid-durable-evidence-manifest) and [empty registry](../history/evidence-index.md) | Accepted / Not Run |
| `G-OPS` | [Startup, shutdown, readiness, recovery and deployment](../topics/delivery/verification-rollout-and-rollback.md#g-ops-startup-shutdown-readiness-recovery-and-deployment-gate) | Accepted / Not Run |
| `G-DIAG` | [Diagnostics, logging and artifacts](../topics/delivery/verification-rollout-and-rollback.md#g-diag-diagnostics-sensitive-logging-and-artifact-gate) | Accepted / Not Run |
| `G-KIRO` | [Low-volume real Kiro](../topics/delivery/verification-rollout-and-rollback.md#g-kiro-low-volume-real-kiro-gate) | Accepted / Not Run |
| `G-SUP` | [Supply chain and release](../topics/delivery/verification-rollout-and-rollback.md#g-sup-supply-chain-and-release-gate) | Accepted / Not Run |

## Traceability Matrix

| Problem | Requirement / invariant | Target technical authority | Dependency work -> dependent work/gate | Accepted decision / resolved policy | Required gates | Evidence / state |
| --- | --- | --- | --- | --- | --- | --- |
| `COR-001` | `FUN-043`, `FUN-044`, `INV-010`, `QA-RES-001`, `QA-SEC-003` | `MOD-DIAGNOSTICS` | R0.1 -> R1.7/R9 | D007/D010 Accepted (`Q-004`) | `G-DIAG`, `G-S`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `COR-002` | `FUN-021`, `FUN-022`, `INV-006` | `MOD-RUNTIME-CONFIG`, `MOD-AUDIT` | R2 -> R8 | D004/D007/D010 Accepted (`Q-001`) | `G-C`, `G-U`, `G-UI`, `G-EVID` | `O / NS / E-` |
| `COR-003` | `FUN-035`, `FUN-036`, `INV-007` | `MOD-USAGE`, `MOD-TERMINAL-JOURNAL` | R2 -> R3 | D004/D010 Accepted (`Q-002`) | `G-U`, `G-EVID` | `O / NS / E-` |
| `COR-004` | `FUN-003`, `FUN-006` | `MOD-MESSAGES`, `MOD-EXTERNAL-POOLS`, `MOD-EXTERNAL-UPSTREAM` | R4.4/R5 -> R8 | D007/D010 Accepted | `G-P`, `G-UI`, `G-EVID` | `O / NS / E-` |
| `COR-005` | `FUN-020`, `INV-001` | `MOD-RUNTIME-CONFIG`, `MOD-MESSAGES` | R1 -> R6 | D007 Accepted | `G-C`, `G-A`, `G-EVID` | `O / NS / E-` |
| `COR-006` | `FUN-004`, `FUN-005`, `FUN-045` | `MOD-PROTO-ANTHROPIC`, `MOD-TRANSPORT-PUBLIC`, `MOD-PAYLOAD`, `MOD-CONTRACT-HARNESS` | R0.6/R1.5 -> R5.0.external/R6.3/R7.1 | D012 Accepted | `G-A`, `G-P`, `G-CLI`, `G-EVID` | `O / NS / E-` |
| `COR-007` | `FUN-046`, `INV-012`, `QA-COMP-001`, `QA-COMP-003` | `MOD-PROTO-ANTHROPIC`, `MOD-PROTO-KIRO`, `MOD-PROTO-EXTERNAL`, `MOD-PROTO-SSE`, `MOD-REQUEST-ARTIFACTS`, `MOD-PAYLOAD`, `MOD-RESPONSE` | R5.0.kiro/R5.0.external -> R6.2/R6.3/R7.0/R7.1 | D012 Accepted | `G-A`, `G-P`, `G-CLI`, `G-EVID` | `O / NS / E-` |
| `SEC-001` | `FUN-041`, `QA-SEC-002`, `QA-SEC-004` | `MOD-MEDIA` | R0.2 -> R6.5 | D001/D010 Accepted | `G-A`, `G-P`, `G-EVID` | `O / NS / E-` |
| `RES-001` | `FUN-042`, `FUN-047`, `QA-RES-001`, `QA-RES-002`, `QA-PERF-004` | `MOD-KERNEL`, `MOD-RESOURCE-GOVERNOR`, `MOD-TRANSPORT-PUBLIC`, `MOD-TRANSPORT-ADMIN`, `MOD-MEDIA`, `MOD-TOKEN-COUNT` | R0.2/R1.3/R1.9 -> R6.5/R6.6.count-tokens/R8.5 | D010/D011 Accepted (`Q-004`) | `G-A`, `G-P`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `RES-002` | `FUN-040`, `QA-RES-001` | `MOD-FILES` | R0.3 -> R6.4/R9 | D001/D010 Accepted (`Q-003`, `Q-004`) | `G-A`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `RES-003` | `FUN-018`, `QA-RES-001`, `QA-RES-002`, `QA-PERF-004` | `MOD-RESOURCE-GOVERNOR`, `MOD-KIRO-UPSTREAM`, `MOD-EXTERNAL-UPSTREAM` | R0.7/R0.8/R1.9 -> R5.1/R5.2 | D010/D011 Accepted (`Q-004`) | `G-P`, `G-A`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `RES-004` | `QA-RES-001`, `QA-RES-004`, `QA-SEC-003` | `MOD-KIRO-UPSTREAM` | R0.7 -> R5.1 | D010 Accepted (`Q-004`) | `G-KIRO`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `RES-005` | `FUN-017`, `FUN-047`, `QA-RES-001`, `QA-RES-005`, `QA-PERF-004` | `MOD-RESOURCE-GOVERNOR`, `MOD-SCHEDULER-LOCAL`, `MOD-SCHEDULER-EXTERNAL` | R0.9/R0.10/R1.9 -> R4.2/R4.5 | D005/D010/D011 Accepted (`Q-004`, `Q-005`, `Q-009`) | `G-SCH`, `G-PERF`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `HA-001` | `FUN-022`, `FUN-023` | `MOD-RUNTIME-CONFIG`, `MOD-AUTH`, `MOD-MODEL-CATALOG`, `MOD-READINESS` | R2 -> R8/R9 | D001/D010 Accepted (`Q-001`, `Q-011`) | `G-C`, `G-UI`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `HA-002` | `FUN-040`, `QA-COMP-001` | `MOD-FILES`, `MOD-READINESS` | R6 -> R9 | D001/D010 Accepted (`Q-001`, `Q-003`) | `G-A`, `G-CLI`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `HA-003` | `FUN-030`, `FUN-031`, `QA-REL-004` | `MOD-PROMPT-CACHE`, `MOD-USAGE` | R3 -> R9 | D001/D010 Accepted (`Q-001`, `Q-007`) | `G-U`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `SEC-002` | `QA-SEC-001` | `MOD-EXTERNAL-UPSTREAM` | R5 | D007/D010 Accepted | `G-P`, `G-EVID` | `O / NS / E-` |
| `SEC-003` | `INV-010`, `QA-SEC-003`, `QA-SEC-005`, `QA-OBS-004` | `MOD-MESSAGES`, `MOD-DIAGNOSTICS`, `MOD-OBSERVABILITY` | R0.1 -> R1.6/R1.7/R6.1/R9 | D010 Accepted (`Q-004`) | `G-DIAG`, `G-CLI`, `G-S`, `G-EVID` | `O / NS / E-` |
| `SEC-004` | `QA-SEC-002`, `QA-SEC-004`, `QA-SEC-006` | `MOD-EXTERNAL-UPSTREAM` | R0.8 -> R5.2 | D010 Accepted | `G-P`, `G-S`, `G-EVID` | `O / NS / E-` |
| `SEC-005` | `FUN-023`, `FUN-025`, `QA-SEC-003`, `QA-SEC-007` | `MOD-AUTH`, `MOD-RUNTIME-CONFIG`, `MOD-CREDENTIALS`, `MOD-PROXY-RESOURCES`, `MOD-TRANSPORT-ADMIN`, `MOD-FRONTEND-CONTRACT`, `MOD-ADMIN-UI`, `MOD-OPERATOR-UI` | R2.4.auth/R2.4.runtime-config/R2.4.credentials/R2.4.proxy-resources -> R2.6/R4.0 -> R8.1.auth/R8.1.runtime-config/R8.1.credentials/R8.1.proxy-resources -> R8.2/eight exact UI workflows/R8.5 | D007/D010 Accepted (`Q-011`) | `G-C`, `G-UI`, `G-S`, `G-EVID` | `O / NS / E-` |
| `SEC-006` | `QA-SEC-003`, `QA-SEC-008` | `MOD-SECRET-ENVELOPE`, `MOD-AUTH`, `MOD-RUNTIME-CONFIG`, `MOD-CREDENTIALS`, `MOD-PROXY-RESOURCES`, `MOD-EXTERNAL-POOLS`, `MOD-MIGRATIONS`, `MOD-RECOVERY` | R1.8 -> secret-owning R2.4 units/R2.8 -> R9.2/R10.3 | D011/D014 Accepted | `G-C`, `G-S`, `G-OPS`, `G-ROLL`, `G-DEL`, `G-EVID` | `O / NS / E-` |
| `REL-001` | `FUN-016`, `FUN-035`, `INV-002`, `INV-007`, `INV-011`, `QA-REL-001` | `MOD-TERMINAL-JOURNAL`, `MOD-SUPERVISOR`, effect-authority modules | R2 -> R3/R7/R8/R9 | D004/D010 Accepted (`Q-002`) | `G-U`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `REL-002` | `FUN-007`, `FUN-014`, `INV-005` | `MOD-ATTEMPT-POLICY`, `MOD-RESPONSE` | R5 -> R7 | D003/D010 Accepted (`Q-008`) | `G-P`, `G-ROLL`, `G-EVID` | `O / NS / E-` |
| `ARCH-001` | fixed constraint 6, `QA-MAINT-001`, `QA-MAINT-002`, `QA-MAINT-004`, `QA-MAINT-005` | `MOD-ARCH-FITNESS` plus each affected authority | R2/R4/R6/R8 -> R10 | D007/D009 Accepted | `G-S`, `G-ROLL`, `G-DEL`, `G-EVID` | `O / NS / E-` |
| `ARCH-002` | `QA-MAINT-001`, `QA-MAINT-003` | `MOD-ARCH-FITNESS` plus each repository/adapter authority | R1/R2 -> R10 | D007/D008/D009 Accepted | `G-S`, `G-DEL`, `G-EVID` | `O / NS / E-` |
| `PERF-001` | `FUN-012`, `FUN-016`, `FUN-017`, `INV-003`, `INV-011`, `QA-PERF-003` | `MOD-SCHEDULER-LOCAL`, `MOD-CREDENTIALS`, `MOD-TERMINAL-LIFECYCLE` | R2/R4 -> R7/R9 | D004/D005/D010 Accepted (`Q-002`, `Q-009`) | `G-SCH`, `G-U`, `G-PERF`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `PERF-002` | `FUN-011`, `QA-PERF-003` | `MOD-SCHEDULER-LOCAL`, `MOD-SCHEDULER-EXTERNAL` | R4 -> R5 | D005/D010 Accepted (`Q-001`, `Q-009`) | `G-SCH`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `PERF-003` | `FUN-010`, `FUN-011`, `INV-004`, `QA-PERF-003`, `QA-PERF-005` | `MOD-SCHEDULER-LOCAL` | R4 | D005/D010 Accepted (`Q-001`, `Q-005`, `Q-009`) | `G-SCH`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `PERF-004` | `FUN-020`, `INV-001`, `QA-PERF-005` | `MOD-RUNTIME-CONFIG`, `MOD-MESSAGES` | R1 -> R6 | D007/D010 Accepted (`Q-005`) | `G-C`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `PERF-005` | `FUN-032`, `INV-009`, `QA-PERF-002`, `QA-PERF-006` | `MOD-REQUEST-ARTIFACTS`, `MOD-PAYLOAD` | R3/R6 | D007/D010 Accepted (`Q-005`) | `G-U`, `G-A`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `PERF-006` | `FUN-035`, `FUN-036`, `INV-007`, `QA-PERF-003`, `QA-PERF-005` | `MOD-USAGE` | R2 -> R3 | D004/D010 Accepted (`Q-002`, `Q-005`) | `G-U`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `PERF-007` | `FUN-042`, `QA-RES-001`, `QA-RES-002`, `QA-PERF-004` | `MOD-MEDIA`, `MOD-TOKEN-COUNT` | R1 -> R6 | D010 Accepted (`Q-004`, `Q-005`) | `G-A`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `PERF-008` | `FUN-015`, `QA-PERF-003`, `QA-PERF-005` | `MOD-KIRO-UPSTREAM` | R5 | D003/D010 Accepted (`Q-008`) | `G-KIRO`, `G-PERF`, `G-EVID` | `O / NS / E-` |
| `PERF-009` | `FUN-019`, `QA-PERF-003`, `QA-PERF-010`, `QA-RES-001` | `MOD-SCHEDULER-LOCAL`, `MOD-SCHEDULER-EXTERNAL` | R0.9/R0.10 -> R4.2/R4.5 | D005/D010 Accepted (`Q-005`, `Q-009`) | `G-SCH`, `G-PERF`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `OPS-001` | `QA-OPS-001`, `QA-OBS-003` | `MOD-READINESS`, `MOD-TRANSPORT-HEALTH` | R9.1.readiness/R9.1.health | D006/D010/D014 Accepted (`Q-001`, `Q-010`) | `G-OPS`, `G-EVID` | `O / NS / E-` |
| `OPS-002` | `FUN-016`, `INV-002`, `INV-011`, `QA-REL-002` | `MOD-SUPERVISOR`, `MOD-TERMINAL-JOURNAL` | R2/R7 -> R9 | D004/D006/D010 Accepted (`Q-002`, `Q-010`) | `G-U`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `OPS-003` | `QA-REL-001`, `QA-REL-003` | `MOD-SUPERVISOR`, `MOD-MAINTENANCE-JOBS`, `MOD-AUDIT` | R2/R8 -> R9 | D006/D010 Accepted (`Q-001`, `Q-002`, `Q-010`) | `G-UI`, `G-OPS`, `G-EVID` | `O / NS / E-` |
| `OPS-004` | `QA-REL-005`, `QA-OPS-002` | `MOD-RECOVERY`, `MOD-READINESS` | R9.1.readiness -> R9.2/R10.3 | D010/D014 Accepted (`Q-001`, `Q-010`, `Q-012`) | `G-OPS`, `G-ROLL`, `G-EVID` | `O / NS / E-` |
| `OPS-005` | `INV-007`, `QA-REL-005`, `QA-OPS-003` | `MOD-MIGRATIONS`, `MOD-RECOVERY`, `MOD-BOOTSTRAP`, each durable authority | exact R2.0/R2.4 units -> R9.1.bootstrap/R9.2 | D008/D010 Accepted (`Q-012`) | `G-C`, `G-OPS`, `G-S`, `G-EVID` | `O / NS / E-` |
| `API-001` | `FUN-024` | `MOD-TRANSPORT-ADMIN`, `MOD-FRONTEND-CONTRACT` | R8 | D007/D009 Accepted | `G-S`, `G-UI`, `G-EVID` | `O / NS / E-` |
| `TEST-001` | `QA-TEST-001` | `MOD-BROWSER-HARNESS` | R8 -> R9 | D009/D010 Accepted (`Q-006`) | `G-UI`, `G-EVID` | `O / NS / E-` |
| `TEST-002` | `QA-TEST-002`, `QA-PERF-005`-`QA-PERF-010`, and canonical workload contract | `MOD-LOAD-CHAOS-HARNESS` plus each measured authority | R0.5/R1.4 -> R9.3.load-chaos/final candidate | D010 Accepted (`Q-005`) | `G-PERF`, `G-ROLL`, `G-EVID` | `O / NS / E-` |
| `TEST-003` | `QA-COMP-002`, `QA-EVID-001` | `MOD-CONTRACT-HARNESS`, `MOD-REAL-CLIENT-HARNESS` | R0.6 -> R5-R7 -> R9.3.real-client/final release | D009/D010 Accepted | `G-CLI`, `G-EVID` | `O / NS / E-` |
| `TEST-004` | `QA-TEST-003`, `QA-PERF-008`, `QA-EVID-001` | `MOD-LOAD-CHAOS-HARNESS` | R0.5 -> R1.4/R9.3.load-chaos | D010 Accepted (`Q-005`); measurement correctness is mandatory | `G-PERF`, `G-S`, `G-EVID` | `O / NS / E-`; current reports cannot independently pass `G-PERF` |
| `DOC-001` | `QA-EVID-001`, `QA-EVID-002` | plan-tree authority, `MOD-ARCH-FITNESS` | R0.4 -> R9/R10.2 | D009/D010 Accepted (`Q-006`) | `G-S`, `G-EVID`, `G-DEL` | `O / NS / E-` |
| `DOC-002` | `QA-EVID-002` | documentation authority, `MOD-REAL-CLIENT-HARNESS` | R9 | No additional architecture decision | `G-S`, `G-EVID` | `O / NS / E-`; `README.md:55` links to absent `docs/claude-code-cli-local-testing.md` |
| `SUP-001` | `QA-SUP-001`, `QA-SUP-002` | `MOD-RELEASE-HARNESS` | R9.4 -> R10.2/R10.3 | D009/D010/D014 Accepted (`Q-013`) | `G-SUP`, `G-ROLL`, `G-EVID`, `G-DEL` | `O / NS / E-` |

## Requirements Without A Direct Finding

These accepted clauses are product/system obligations rather than evidence of an additional current defect. They receive explicit landing coverage without inventing a problem ID.

| Requirement | Target technical authority | Dependency work | Accepted decision / required gates | Evidence / state |
| --- | --- | --- | --- | --- |
| `FUN-001` | `MOD-PROTO-ANTHROPIC`, `MOD-TRANSPORT-PUBLIC`, `MOD-MESSAGES`, `MOD-RESPONSE` | R1.5 -> R6.7/R7.1/R7.4 | D007/D009; `G-P`, `G-CLI`, `G-EVID` | `NS / E-` |
| `FUN-002` | `MOD-PROTO-ANTHROPIC`, `MOD-PROTO-KIRO`, `MOD-PROTO-SSE`, `MOD-MESSAGES`, `MOD-FILES`, `MOD-RESPONSE` | R1.5/R5 -> R6/R7/R9.3.real-client | D009/D012; `G-P`, `G-CLI`, `G-KIRO`, `G-EVID` | `NS / E-` |
| `FUN-013` | `MOD-SCHEDULER-LOCAL`, `MOD-SCHEDULER-EXTERNAL`, `MOD-READINESS`, `MOD-RECOVERY` | R4.2/R4.5 -> R9.1/R9.2 | D005/D010/D014; `G-SCH`, `G-OPS`, `G-PERF`, `G-EVID` | `NS / E-` |
| `FUN-033` | `MOD-USAGE`, `MOD-PROMPT-CACHE` | R3.1/R3.4 | D004/D010; `G-U`, `G-EVID` | `NS / E-` |
| `FUN-034` | `MOD-USAGE`, `MOD-RESPONSE`, `MOD-TERMINAL-LIFECYCLE` | R3.1 -> R7.1/R7.3 | D004; `G-U`, `G-P`, `G-EVID` | `NS / E-` |
| `INV-008` | `MOD-USAGE` | R3.1/R3.2 | D004/D010; `G-U`, `G-EVID` | `NS / E-` |
| `QA-OBS-001` | `MOD-KERNEL`, `MOD-OBSERVABILITY`, `MOD-TRANSPORT-PUBLIC`, `MOD-TRANSPORT-ADMIN` | R1.1/R1.6 -> R6.7/R7.4/R8.5 | D007/D011; `G-DIAG`, `G-P`, `G-UI`, `G-EVID` | `NS / E-` |
| `QA-OBS-002` | `MOD-OBSERVABILITY` plus each timed authority | R1.6 -> R4-R9 integrations | D007/D010/D011; `G-DIAG`, `G-PERF`, `G-EVID` | `NS / E-` |
| `QA-PERF-001` | `MOD-ARCH-FITNESS`, `MOD-LOAD-CHAOS-HARNESS` | R0.4/R0.5 -> R1.4/R9.3.load-chaos | D009/D010/D011; `G-S`, `G-PERF`, `G-EVID` | `NS / E-` |
| `QA-PERF-007` | `MOD-LOAD-CHAOS-HARNESS`, `MOD-RELEASE-HARNESS`, all performance-affecting authorities | R9.3.load-chaos -> R10.2/R10.3 | D009-D011; `G-PERF`, `G-ROLL`, `G-EVID` | `NS / E-` |
| `QA-PERF-009` | `MOD-LOAD-CHAOS-HARNESS`, `MOD-RELEASE-HARNESS` | R1.4 -> R9.3.load-chaos/R9.4 | D010/D011/D014; `G-PERF`, `G-EVID`, `G-SUP` | `NS / E-` |
| `QA-RES-003` | `MOD-KERNEL`, `MOD-RESOURCE-GOVERNOR`, `MOD-TRANSPORT-PUBLIC`, `MOD-TRANSPORT-ADMIN`, `MOD-RESPONSE` | R1.1/R1.9 -> R6.7/R7.1/R8.5 | D010/D011; `G-A`, `G-P`, `G-DIAG`, `G-EVID` | `NS / E-` |

## Canonical Module Coverage Cross-Check

This cross-check is independent from finding ownership: supporting modules need not be the primary authority for a current defect, but every accepted module must still land through a work unit, requirement/finding coverage and required gates. The union of the cells below must equal the 50 canonical definitions in `target-module-ledger.md`, with no duplicate or unknown ID.

| Coverage surface | Canonical target modules |
| --- | --- |
| Foundation, protocol, transport and telemetry | `MOD-KERNEL`, `MOD-RESOURCE-GOVERNOR`, `MOD-SECRET-ENVELOPE`, `MOD-OBSERVABILITY`, `MOD-DIAGNOSTICS`, `MOD-PROTO-ANTHROPIC`, `MOD-PROTO-KIRO`, `MOD-PROTO-EXTERNAL`, `MOD-PROTO-SSE`, `MOD-TRANSPORT-PUBLIC`, `MOD-TRANSPORT-ADMIN`, `MOD-TRANSPORT-HEALTH` |
| Durable/control authorities | `MOD-RUNTIME-CONFIG`, `MOD-AUTH`, `MOD-MODEL-CATALOG`, `MOD-CREDENTIALS`, `MOD-PROXY-RESOURCES`, `MOD-EXTERNAL-POOLS`, `MOD-TERMINAL-JOURNAL`, `MOD-USAGE`, `MOD-PROMPT-CACHE`, `MOD-FILES`, `MOD-AUDIT`, `MOD-MAINTENANCE-JOBS`, `MOD-MIGRATIONS` |
| Request/data plane | `MOD-SCHEDULER-LOCAL`, `MOD-SCHEDULER-EXTERNAL`, `MOD-MESSAGES`, `MOD-REQUEST-ARTIFACTS`, `MOD-PAYLOAD`, `MOD-KIRO-UPSTREAM`, `MOD-EXTERNAL-UPSTREAM`, `MOD-ATTEMPT-POLICY`, `MOD-RESPONSE`, `MOD-TERMINAL-LIFECYCLE`, `MOD-MEDIA`, `MOD-TOKEN-COUNT` |
| Lifecycle and recovery | `MOD-BOOTSTRAP`, `MOD-SUPERVISOR`, `MOD-READINESS`, `MOD-RECOVERY` |
| Maintained frontends | `MOD-FRONTEND-CONTRACT`, `MOD-ADMIN-UI`, `MOD-OPERATOR-UI` |
| Validation and release | `MOD-ARCH-FITNESS`, `MOD-CONTRACT-HARNESS`, `MOD-LOAD-CHAOS-HARNESS`, `MOD-REAL-CLIENT-HARNESS`, `MOD-BROWSER-HARNESS`, `MOD-RELEASE-HARNESS` |

## Current Traceability Blockers

- All architecture/policy questions are resolved by accepted decisions. Findings remain Open because target implementation and evidence do not exist.
- `TEST-004` prevents current loadtest output from independently passing `G-PERF`; R0.5 must implement and self-test the final valid harness.
- R0.1-R0.3/R0.7-R0.10 now produce final fixtures only. The affected findings close only after the final R1/R4/R5/R6 modules, legacy deletion and applicable complete-system gates pass.
- `OPS-005` requires the accepted `MOD-MIGRATIONS` boundary, exact domain manifests, adoption map, previous-binary profile and bounded backfills; `MOD-RECOVERY` remains separate.
- `SEC-005` requires all exact auth/config/credential/proxy backend and two-app workflow units plus generated-contract/browser/security evidence; masking one response cannot close it.
- `R0.6` contributes fixtures to protocol/real-client gates but closes no product finding by itself.
- `ARCH-001` spans state stores, schedulers, Messages and Admin. Closing one large manager/handler portion remains insufficient.
- All 16 gate contracts are Accepted but Not Run. No finding can close from a planned scenario.
- `DOC-002` remains open until a current secret-safe workflow and clean-checkout link gate pass.

## Maintenance Checks

A document-only consistency check should enforce:

1. every problem-catalog ID appears exactly once in this matrix;
2. every matrix ID exists in the problem catalog;
3. every P1 finding has a requirement or explicit gap, target technical authority, work unit, accepted decision/policy and focused gate;
4. every accepted `FUN-*`, `INV-*` and `QA-*` appears in either one finding row or the independent-requirement table, and every referenced `Q-*`, decision, gate, work unit, target `MOD-*` and evidence ID exists;
5. the canonical-module cross-check is exact set equality with the 50 definitions in `target-module-ledger.md`, with no duplicate or unknown module ID;
6. no row says Closed unless its problem authority is closed and a passing versioned `EVID-*` record exists;
7. no work unit says `Verified In Candidate` while a required row remains Open without an accepted scoped exception or deletion evidence;
8. no rewrite-inventory row is complete without target integration, import/reference search, legacy deletion and post-deletion evidence;
9. Markdown relative links resolve, including root documentation links such as the one tracked by `DOC-002`.
