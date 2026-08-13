# 009: Single-Program Modular Build And Final System Cutover

Role: Architecture delivery decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding implementation and release model for the complete modernization

Scope: The complete Rust core, both maintained Admin frontends, schema and state adoption, validation tooling, release tooling, legacy removal, production activation, and rollback

Affected requirements/findings: Decision 002 delivery clauses, all `R0`-`R10` work, every `MOD-*` module, `QA-MAINT-*`, `QA-COMP-*`, `QA-PERF-*`, and all legacy-deletion obligations

Decision source: Operator instruction on 2026-07-12: do not plan calendar, people, or responsible-person assignments; produce one final complete implementation plan, avoid staged modernization, and organize implementation by module.

Supersedes: [Decision 002](002-complete-module-by-module-rewrite.md) for per-module production switches, per-module canaries, per-module rollback windows, per-module soak, and incremental production acceptance. Decision 002 remains historical authority for the complete-rewrite requirement, dependency-oriented modular work, compatibility characterization, modular-monolith target, and mandatory removal of superseded implementation.

Related: [Plan root](../README.md), [Target architecture](../topics/architecture/target-system-architecture.md), [Target module ledger](../indexes/target-module-ledger.md), [Modular work map](../indexes/execution-slice-map.md), [Complete implementation plan](../topics/delivery/migration-sequence.md), [Verification and final cutover](../topics/delivery/verification-rollout-and-rollback.md), [Roadmap](../roadmap.md)

## Context

The earlier delivery model treated each target module as an independently activated production slice. That model reduced cutover blast radius, but it also created temporary containment implementations, repeated replacement of the same responsibility, long-lived legacy adapters, per-module selectors, and a sequence of partially modernized production states.

The operator requires a different outcome: one complete modernization plan, implemented with modular work units but delivered as one final target system. The plan must not turn personnel assignment, calendar estimates, or intermediate production releases into prerequisites.

This changes delivery mechanics, not the target architecture. Domain ownership, narrow contracts, one immutable request runtime version, bounded resources, retry safety, durable terminal behavior, recoverable state, both frontend rewrites, performance proof, and deletion of legacy code remain mandatory.

## Decision

The modernization is one implementation program with one accepted target architecture and one production activation boundary.

`R0` through `R10` and their exact rows are dependency-oriented **work groups and work packages** inside that program. They organize coding, review, local integration, and evidence. They are not product phases, separately released modernization versions, independently accepted production slices, or separate rollback domains.

The old production version remains the only production authority until the complete target release candidate passes every applicable gate. Incomplete target builds may run only in development, tests, isolated dependencies, cloned-data rehearsal, or explicitly non-authoritative comparison environments.

The target release candidate is activated as a whole. If activation fails, rollback selects the previous complete binary and its compatible data profile as a whole. There is no supported production state in which arbitrary old and new business modules are mixed by request cohort.

## Terminology

- **Module authority** means the code boundary that owns a state, invariant, public contract, migration, or lifecycle. It never means a human assignee.
- **Work package** means one bounded coding and verification unit mapped to one or more target modules. Completion means its target code and focused evidence are ready for integration; it does not mean production rollout.
- **Target candidate** means the integrated target-only system under construction. It is not production-capable until every mandatory module and gate is complete.
- **Legacy baseline** means the previous complete production implementation used only for characterization and whole-system rollback.
- **Final cutover** means one activation of the complete target binary, both target frontend artifacts, target migration profile, and target release manifest.

No plan-level calendar, duration estimate, person, team, maintainer assignment, or staffing gate is required. Runtime deadlines, timeout values, test sample sizes, load durations, recovery objectives, and final observation windows remain technical correctness constraints rather than project scheduling.

## Modular Implementation Rules

1. Implement modules in dependency order and keep every commit/build internally coherent.
2. Freeze and characterize the public and cross-module contract before replacing its implementation.
3. A target module may depend only on accepted target public contracts, the bounded shared kernel, and its own ports. It may not import a legacy God Object, repository, manager, handler, runtime context, or fallback path.
4. An unfinished dependency is represented by a typed fake or contract fixture in tests. It is not satisfied by calling the legacy implementation from target production code.
5. Pure old-versus-target comparison may replay the same sanitized immutable facts offline. It never performs a second upstream POST, Admin mutation, lease acquisition, Files mutation, remote fetch, durable write, or other side effect.
6. Module-level tests, property tests, storage tests, fault tests, architecture checks, and performance microbenchmarks run as soon as the module exists. Their pass state is integration evidence, not release evidence.
7. Cross-module integration is continuous inside the target candidate. A module is not declared complete if downstream integration exposes a contract, lifecycle, resource, or performance defect.
8. Newly discovered problems are added to the candidate/finding and traceability ledgers before they change architecture or acceptance behavior.
9. Both maintained frontends, generated contracts, database/Redis migrations, validation harnesses, Docker/CI/release assets, recovery commands, and documentation are part of the same target candidate.
10. The final target source and artifacts contain no dormant legacy selector, hidden fallback, per-module canary path, or duplicate old implementation. The previous release remains recoverable from version control and release artifacts, not from code embedded in the target binary.

## Dependency Work Graph

The implementation order remains dependency-first:

```text
contracts, architecture checks, and trustworthy harnesses
  -> kernel, protocols, runtime views, observability, diagnostics, secret envelope, resource governor
  -> migration foundation, domain state ports, CAS, auth, catalogs, terminal journal
  -> usage/cache and scheduler/credential/pool/proxy authorities
  -> Kiro/external upstream adapters and replay policy
  -> request planning, artifacts, payload, Files, media, token count, public endpoints
  -> SSE, response, terminal lifecycle, Messages transport
  -> Admin command/query transport, generated contract, both frontend applications
  -> bootstrap, supervision, readiness, recovery, real-client/browser/load/release harnesses
  -> full target integration, legacy removal, rehearsal, final release
```

This graph permits parallel AI work only where accepted public contracts and state boundaries do not conflict. It does not create separate production releases, and it does not authorize omitting a dependency group from the final target.

## Data And Migration Model

- All target schema additions, migration manifests, adoption probes, backfills, Redis namespaces, and compatibility readers are implemented and tested as one complete migration plan.
- Development and rehearsal use fresh databases and sanitized/cloned legacy states. Read-only comparison is permitted; legacy and target runners never execute real DDL concurrently against the same database. No target backfill mutates production before legacy admission, job claim, producers and writers are stopped.
- The final migration uses expand-and-adopt changes that remain readable by the previous binary during the whole-system rollback window.
- The target release never dual-writes through independent old and new owners. A single target authority owns every accepted mutation.
- Destructive schema contraction occurs only after the final full-system observation and rollback window. It is part of completion of this one modernization program, not a new architecture phase.

## Final Cutover Preconditions

Final cutover is prohibited until all of the following are true:

1. every rewrite-inventory row has a final target treatment and no unexplained responsibility remains;
2. all 50 registered target modules are implemented, integrated, and covered by architecture and contract checks;
3. target source/import searches prove legacy execution paths, selectors, compatibility facades, duplicate frontend implementations, obsolete schema writers, and obsolete harness paths are removed;
4. all decisions required by the target behavior are Accepted and no blocking open question remains;
5. fresh, legacy-adoption, partial/interrupted, drift/corruption, concurrent-start, backup/restore, Redis rebuild, and previous-binary migration rehearsals pass;
6. static, Rust, storage, protocol, real Claude Code, low-volume real Kiro, frontend/browser, load/chaos, performance, shutdown/restart, Docker, supply-chain, secret-scan, and artifact-cleanup gates pass where applicable;
7. absolute capacity/SLO, relative regression, resource ceiling, idle recovery, operation budget, and whole-system soak requirements pass on the accepted reference profile;
8. both frontend artifacts and the backend generated contract share one release identity;
9. the target image, SBOM, signature/provenance, configuration/schema hashes, migration plan, evidence manifest, backup checkpoint, rollback artifact, and rollback commands are immutable and verified;
10. a full dress rehearsal executes the exact final cutover and whole-system rollback sequence against an isolated production-shaped environment.

## Final Cutover Procedure

1. Reject new admissions on the legacy deployment and drain or explicitly terminate in-flight work under the accepted shutdown contract.
2. Capture and verify the final database backup/WAL position, Redis reconciliation state, release identity, and rollback checkpoint.
3. Run the inspector and the complete fenced expand/adopt/backfill plan inside the one decision-014 maintenance window. Abort before the first mutation when identity, checksum, rehearsal hash, row/disk/WAL/lock/memory/time budget or previous-reader probe is missing; checkpoint/return to the previous release if an additive step later exceeds its bound.
4. Deploy the complete target backend and both target frontend artifacts to all expected production instances with readiness/load-balancer admission closed.
5. Verify the signed `ReleaseGenerationManifest`, attest every expected instance to its one backend/frontend/schema/config digest set and prove every old generation is stopped or routing-fenced.
6. Through a deployment-owned private validation path, run migration, dependency, auth/catalog, queue/lease, outbox, API/Admin/Files and isolated test-account Claude Code smoke checks while public admission remains closed. These checks never duplicate a real mutating logical operation.
7. Open readiness and all production traffic for the target system once. No stable-hash or percentage cohort selects legacy modules; post-open work is observation, not the first functional smoke.
8. On a rollback trigger, stop target admission and satisfy decision 014's per-authority compatibility/reconciliation/invalidation matrix before deploying the previous complete artifact and opening its contained rollback profile.

## Whole-System Rollback

Rollback is release-level, not module-level. It is triggered by any unexplained compatibility break, duplicate-side-effect risk, durable-state divergence, migration corruption, security exposure, failure to recover capacity/resources, SLO regression, readiness dishonesty, critical shutdown residue, or inability to complete required workflows.

Rollback does not erase target migration ledger rows, reverse committed additive DDL blindly, discard outbox/job state, reuse stale lease tokens, or pretend target writes never occurred. The expected-instance fence, usage/audit and automatic-state compatibility projections, session invalidation, Files degradation, old-UI containment and forward reconciliation follow decision 014 before traffic opens.

After the rollback window closes, the previous binary and additive compatibility profile remain retrievable as release evidence, but they are no longer a supported live selector inside the target system.

## Legacy Removal

Legacy source removal is part of building the target candidate, not a sequence of production post-canary deletions. A legacy responsibility is removed once its target module and all target consumers pass focused integration and no target code imports it. The complete target release candidate cannot be formed while any legacy execution fallback remains.

Legacy database columns, Redis keys, and external artifacts that exist only to keep the previous release rollback-capable are removed after the final observation window and forward-recovery checkpoint. Their removal must pass full post-contraction gates before this modernization is complete.

## Consequences

This model avoids permanent dual paths, repeated temporary containment, selector complexity, and partially modernized production states. It also creates a larger final activation boundary and makes whole-system rehearsal, additive data compatibility, complete evidence, and rollback discipline more important.

Module decomposition still provides fault isolation during implementation, focused tests, bounded context, dependency control, and incremental code review. It no longer provides independent production rollout isolation. That tradeoff is explicit and accepted by the operator's delivery instruction.

## Completion Criteria

The modernization is complete only when the final target system is active, the whole-system observation and rollback window passes, compatibility-only schema/state is contracted safely, all superseded source and artifacts are removed or archived with provenance, every required evidence record is versioned, and the final post-deletion/post-contraction gate set passes.

No module, work group, benchmark, document count, line-count reduction, or intermediate target build can independently claim completion.
