# Continuous Audit And Finding Lifecycle

Role: Systematic problem-discovery method and finding-governance contract

Status: Accepted planning and module-entry policy

Authority: Defines how this plan searches for, verifies, routes, and closes problems; finding severity remains in the problem catalog and implementation state remains in the roadmap

As of: `v0.0.102` / `e9479df` / 2026-07-12

Read when: Auditing the current system, starting a module work unit, discovering an implementation risk, changing severity, or claiming target integration/final closure

Related: [Problem catalog](README.md), [Finding candidate ledger](../../indexes/finding-candidate-ledger.md), [Requirements](../requirements-and-quality-attributes.md), [Traceability matrix](../../indexes/traceability-matrix.md), [Rewrite inventory](../../indexes/rewrite-inventory.md), [Verification](../delivery/verification-rollout-and-rollback.md), [Roadmap](../../roadmap.md), [Decision 009](../../decisions/009-single-program-modular-build-and-final-cutover.md), [Decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md)

## Purpose

The operator's initial observations, the current problem catalog, and static code review are starting evidence, not a closed list of defects. This plan must continue to discover problems that were not named in the original request and were not visible in the first audit.

No work unit may claim that its problem set is complete merely because:

- every source path appears in the rewrite inventory;
- every previously known problem has a target technical authority;
- a large file was split;
- unit tests pass;
- one benchmark or one successful real request passed;
- no new issue was noticed during implementation.

Audit completeness is expressed as completed, evidenced audit obligations for a stated revision and workload. It is never a claim that unknown defects cannot exist.

Current planning-registry checkpoint: 47 verified findings, 50 target technical-authority modules, 100 requirement/invariant/quality clauses, 16 candidate records, 16 gate IDs, and accepted decisions 001 and 003-014; decision 002 remains Superseded. These are drift-detection sets for the current plan, not implementation, closure, performance, or release evidence.

## Separation Of Ledgers

Five ledgers answer different questions:

| Ledger | Question | Authority |
| --- | --- | --- |
| Rewrite inventory | Does every first-party implementation path have a target treatment? | [Rewrite inventory](../../indexes/rewrite-inventory.md) |
| Finding candidate ledger | Which observations still require confirmation, retraction, or promotion? | [Finding candidate ledger](../../indexes/finding-candidate-ledger.md) |
| Problem catalog | Which verified current gaps exist, how severe are they, and what closes them? | [Problem catalog](README.md) and its grouped finding files |
| Traceability matrix | Does every finding reach a requirement, technical authority, work unit, decision, gate, and evidence slot? | [Traceability matrix](../../indexes/traceability-matrix.md) |
| Evidence history | What actually passed for which source revision and configuration? | Versioned plan history created when package execution begins |

Coverage in one ledger cannot substitute for another. In particular, source-path coverage does not prove failure-mode coverage, and accepted target text does not prove a current defect is fixed.

## Audit Axes

Every work unit reviews all axes below and marks each one `Applicable`, `Not Applicable` with a reason, or `Blocked` with a technical cause. A work unit may go deep only where its module has authority, but it may not silently omit an axis.

### Technical Authority And Dependencies

- state owner, invariant owner, mutation owner, and lifecycle owner;
- compile-time dependency direction and forbidden imports;
- broad facades, shared mutable state, circular callbacks, and hidden global access;
- duplicate policy or wire types maintained by different modules;
- test-only characterization adapters and their deletion conditions; no compatibility adapter may enter target runtime code.

### Request And Protocol Behavior

- success, validation failure, upstream failure, partial response, cancellation, timeout, and shutdown;
- stream and non-stream event order, final usage, errors, thinking, tools, Files, MCP, and model aliases;
- raw versus normalized body semantics and target-specific work;
- retry replay safety, downstream commitment, attempt limits, and fallback loops;
- slow first byte, long progressing stream, slow reader, and disconnect.

### State And Consistency

- durable authority, cache/projection role, version, CAS, transaction, and outbox boundary;
- duplicate delivery, lost acknowledgement, stale snapshot, missed invalidation, and restart;
- single- and multi-replica convergence according to the accepted deployment mode;
- schema compatibility with the previous binary and forward reconciliation;
- backup, restore, Redis rebuild, and derived-state reconstruction.

### Concurrency And Lifecycle

- lock scope and locks held across await;
- queue admission, fairness, cancellation, timeout, capacity, and late grants;
- task ownership, panic behavior, restart policy, and shutdown ordering;
- lease acquire, heartbeat, complete, cancel, fencing, expiry, and reconciliation;
- producer barriers, writer ingress closure, drain, and critical residue.

### Resource And Performance

- count, byte, age, concurrency, queue, timeout, and retry bounds;
- allocation before permit acquisition, repeated cloning/parsing/serialization, and retained data;
- PgSQL/Redis operations and pool waits per request or event;
- O(N) or worse work as credentials, pools, tools, messages, files, or cache entries grow;
- CPU/blocking work, executor starvation, HTTP client reuse, FD/task/RSS recovery, and filesystem growth;
- performance under normal, burst, partial-failure, widespread-failure, and recovery scenarios.

### Security And Sensitive Data

- authentication and secret rotation without inventing tenant semantics;
- header allowlists, URL parsing, SSRF, DNS rebinding, redirects, and proxy DNS behavior;
- request bodies, prompts, tools, images, files, tokens, keys, cookies, and credentials in logs or artifacts;
- filesystem roots, symlinks, permissions, retention, exports, and cleanup scope;
- dependency, image, SBOM, signature, provenance, and release identity.

### Control Plane And Frontends

- Admin command versus query ownership and transaction boundaries;
- Rust API authority versus generated frontend contract;
- loading, conflict, stale data, partial failure, retry, cancellation, and destructive confirmation states;
- accessibility, browser behavior, both maintained applications, and migration parity;
- long-running job durability and cross-replica status.

### Operations And Evidence

- startup, readiness, degraded state, liveness, shutdown, and non-zero failure outcomes;
- deployment examples, active runbooks, rollback, restore, and previous-version compatibility;
- deterministic fixtures, real storage, fake upstream, real client, load, chaos, and browser coverage;
- source revision, configuration hashes, commands, thresholds, artifact manifests, secret scans, and cleanup;
- stale, missing, contradictory, ignored, or unregistered documentation and evidence.

## Audit Triggers

Work-unit entry is mandatory but not the only trigger. A bounded audit or candidate record also starts when any of the following occurs:

- a production incident, unexplained error cluster, latency/resource regression, or capacity/recovery anomaly;
- an upstream Kiro, Anthropic, Claude Code, external-pool, proxy, PgSQL, Redis, browser, or operating-system behavior/version change;
- a dependency vulnerability, license/security advisory, image/base-toolchain change, or release exception;
- a new route, configuration field, storage schema, Redis key class, task, queue, cache, file, external URL, retry loop, or blocking workload;
- a failed or contradictory characterization, load, chaos, real-client, recovery, browser, Docker, or supply-chain gate;
- a new broad import, service locator, shared mutable context, compatibility fallback, or ownership exception;
- documentation/evidence drift, a missing artifact, or a cleanup action whose manifest/provenance cannot be proven.

An external trigger routes to the module that owns the affected contract. When technical authority is unclear, record the candidate and make authority clarification the first verification action.

## Triage Discipline

This plan assigns no human role or calendar deadline. Triage priority is technical:

- suspected P0 stops ordinary target integration and requires an explicit incident/containment decision;
- suspected data loss, secret exposure, unsafe remote access or duplicate side effect is resolved before the affected target module integrates;
- any implementation-blocking candidate is resolved before the affected work unit advances;
- other candidates remain visible and must be reconciled before the complete candidate freezes;
- no candidate silently expires because time passed or no person was assigned.

## Finding Lifecycle

### Candidate

A candidate is an observation that still needs reproduction or source verification. It is recorded in the [finding candidate ledger](../../indexes/finding-candidate-ledger.md) and, when work-unit-specific, in the implementation audit record. It is not immediately promoted as a durable defect.

Required candidate fields:

- short description and affected behavior;
- source revision and location or reproducible observation;
- suspected impact and affected audit axes;
- what would confirm or retract it;
- possible overlap with an existing problem ID;
- affected technical authority, opened date, current state and next verification action.

Candidates are not roadmap commitments and do not use a permanent problem ID until triage. Promotion or retraction updates the ledger in the same reviewable change so a candidate cannot disappear between a chat, worksheet, catalog, and roadmap.

### Verified Open Finding

A candidate becomes a verified finding only when current code, a focused test, a runtime observation, or a reproducible evidence gap supports it. Promotion assigns:

- stable ID and concise title;
- severity with evidence-based rationale;
- affected contract, requirement, or missing requirement;
- current code/runtime evidence and impact;
- primary target technical authority and work unit;
- compatibility, target-integration and whole-system rollback impact;
- focused acceptance and verification path;
- traceability-matrix row.

Potential security or data-loss findings may be privately contained before full publication, but the durable catalog still records a safe description and technical authority.

### Planned Or Incident-Contained

`Planned` means an accepted module work unit includes the remediation and its dependencies are explicit. An emergency production containment is a separate incident hotfix, not a modernization phase; it leaves the structural finding open until the final target replaces it.

### Fixed Pending Verification

The target implementation exists, but one or more compatibility, fault, load, target-integration, recovery, deletion or aggregate gates remain incomplete. Production still uses the previous complete release; the target has no module fallback.

### Closed

A finding closes only when the catalog closing rule is satisfied and the traceability row links versioned evidence for the exact source revision. Closure requires target behavior, focused and applicable complete-system gates, final cutover/rollback result where relevant and removal of the superseded path under decision 009.

### Retracted Or Bounded

A finding is retracted when evidence disproves it. It is bounded when the original broad statement is false but a narrower verified issue remains. The catalog preserves the reason so future audits do not repeatedly reintroduce the same unsupported claim.

## Module Work-Unit Audit Cycle

### Entry Audit

When a Ready work unit starts:

1. pin the source revision and refresh relevant baseline facts;
2. generate or verify exact-one source/symbol responsibility coverage for the work-unit inventory;
3. map entry contracts, state, external effects, tasks, queues, files, and configuration reads;
4. walk every audit axis and create candidates for unexplained behavior;
5. enumerate success, failure, cancellation, timeout, restart, and shutdown transitions;
6. capture current operation counts, latency/resource baseline, and compatibility fixtures;
7. triage candidates into verified findings, retractions or a genuinely new architecture question;
8. ensure every P1/P1-P2 finding has a requirement, technical authority, work unit, decision state and gate.

Work is blocked if core behavior cannot be characterized safely or a discovered fact contradicts the accepted technical boundary.

### Design Audit

Before integrating the module contract:

- prove one technical authority for every state and terminal transition;
- prove dependencies point toward domain/ports rather than concrete infrastructure;
- define retry, idempotency, cancellation, timeout, and partial-failure behavior;
- assign hard resource bounds and overload outcomes;
- specify observability without high-cardinality or sensitive labels;
- identify data migration, target-only comparison, whole-system rollback and legacy deletion behavior;
- update the traceability matrix and the applicable accepted or superseding decisions.

### Implementation Audit

During implementation, discoveries are recorded before they become silent architecture changes. A newly found issue may change the work-unit mapping, require an incident hotfix or block target integration/final cutover. It must not be hidden by broadening an interface or adding a fallback without a documented contract.

Review checks include forbidden imports, new tasks/queues/caches/files, new configuration reads, new persistence paths, retry loops, blocking work, and growth with input cardinality.

### Exit Audit

Before a module becomes `Integrated`:

1. rerun every applicable audit axis against the replacement;
2. execute focused, integration, protocol, storage, load/chaos, browser, and recovery gates selected by traceability;
3. compare operation counts, latency, RSS, FD, task, queue, and file recovery to the accepted baseline;
4. reconcile every candidate and verified finding discovered during the work unit;
5. prove whole-system rollback remains compatible with additive schema, Redis keys, events and configuration;
6. verify evidence manifests and cleanup ownership;
7. remove the superseded legacy responsibility and rerun post-deletion checks before advancing.

### Post-Deletion Audit

After the legacy implementation is deleted, search imports, execution selectors, fallback branches, config fields, tests, scripts, docs and dependencies. Run focused and affected target-candidate gates again. A hidden legacy fallback or duplicate authority reopens the work unit.

## Severity And Escalation

- Static maintainability evidence alone does not make a performance issue P0 or P1.
- Reproducible lost updates, duplicate effects, secret exposure, unsafe remote access, unbounded high-amplification growth, or false-success durability may justify P1.
- Multi-replica production is an accepted supported profile under decisions 010 and 014; related severity is evaluated against that binding profile rather than left conditional on a future decision.
- A benchmark changes severity only when the workload, source revision, configuration, samples, and recovery are reproducible.
- Any active P0 discovery interrupts normal work-unit order and requires an explicit incident-containment decision.

Severity changes update the problem catalog, traceability matrix, roadmap priority, and affected decision/gate in one reviewable change.

## Evidence And Data Safety

Audit evidence uses synthetic or redacted data by default. It records hashes, counts, bounded examples, and stable error classes instead of prompts, credentials, raw headers, file contents, or production identifiers.

Raw artifacts are supporting material, not durable authority. Before cleanup, a small versioned summary preserves source revision, commands, configuration identity, result, thresholds, artifact hashes/counts, secret scan, and cleanup state.

## Minimum Automation

R0.4 establishes a reproducible local planning check and R1 makes it blocking for target-module implementation. The planning and CI checks enforce:

- every catalog problem ID appears exactly once in the traceability matrix;
- every P1/P1-P2 row has a requirement, technical authority, work unit, decision state and gate;
- every referenced requirement, invariant, decision, question, work unit and gate exists;
- every active candidate has an affected technical authority or explicit authority gap and next verification action;
- a `Closed` finding links passing versioned evidence;
- a work unit cannot be `Verified In Candidate` while a required finding remains open without an accepted scoped exception;
- an inventory row cannot be complete without target integration, deletion and post-deletion evidence;
- Markdown links, anchors, source paths, and problem IDs resolve;
- every target module ID exists, every active work unit names exact technical authorities, and no target-runtime module imports a legacy God Object;
- no audit command creates unbounded artifacts or reads ordinary user/production state without explicit scope.

The check derives the finding, module, clause, candidate, gate and accepted-decision sets from their authoritative registries and fails on missing, extra or duplicate members. The current checkpoint counts detect planning drift only; matching `47/50/100/16/16` is never implementation or release evidence.

Before R0.4 lands, each started work unit records the same assertions in its review checklist and evidence manifest. The manual form expires when the final architecture check exists; later work cannot treat "not automated yet" as a standing waiver.

## Recovery-Audit Disposition

The 2026-07-12 recovery audit found planning gaps whose document state has since diverged. A drafted artifact closes a documentation-shape gap only; it does not accept the proposal, implement production behavior, or provide passing evidence.

| Recovery discovery | Current disposition | Remaining closure |
| --- | --- | --- |
| Retry wording disagreed on whether committed headers or only body/SSE bytes prohibit rerouting, and attempt delivery certainty mixed upstream execution risk with upstream response progress | Accepted [decision 003](../../decisions/003-attempt-replay-and-downstream-commitment.md) separates `DownstreamCommitment` from upstream execution possibility and prohibits rerouting once headers commit | Implement the fail-closed capability matrix and prove it through R5/R7 gates |
| Terminal completion implied a cross-Redis/PgSQL exactly-once effect, while R3 and R7 overlapped on final usage versus request terminal ownership | Accepted [decision 004](../../decisions/004-terminal-authority-and-partial-failure-recovery.md) defines one neutral terminal decision with technical-authority idempotent obligations rather than one distributed transaction | Implement and prove terminal/outbox/lease/usage recovery through R2-R4/R7/R9 |
| Scheduler contracts omitted queue cancellation, heartbeat, fencing, complete/cancel and Redis-epoch recovery | Accepted [decision 005](../../decisions/005-scheduler-queue-and-lease-lifecycle.md), decision 010 and [decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md) bind lifecycle, fairness, timing, Redis-loss generation membership and recovery readiness | Implement both final schedulers, generation/recovery barriers and pass `G-SCH`/`G-PERF`/`G-REC` |
| Shutdown ordering did not separate stopping producers from closing and draining consumers | Accepted [decision 006](../../decisions/006-producer-aware-shutdown-and-residue.md) and decision 010 bind producer barriers, ordered drain, deadlines and residue | Implement the complete supervisor and pass `G-OPS` |
| No end-to-end problem-to-evidence traceability matrix existed | The [traceability matrix](../../indexes/traceability-matrix.md) covers the verified catalog | Make exact-one and reference checks blocking through `R0.4`; no planned evidence slot counts as a passed result |
| The repository README linked to a missing local Claude Code testing document | Still open as `DOC-002` | Restore a maintained guide or update the README to an accepted replacement |
| Tracked historical documents lacked a reviewed disposition policy | The [legacy document disposition](../../indexes/legacy-document-disposition.md) records reviewed delete/archive/keep boundaries and recovery data | Continue only coherent technical-authority-domain archive batches; do not bulk-move protected material |

These are planning discoveries and disposition records, not evidence that the corresponding production defects have already been fixed.
