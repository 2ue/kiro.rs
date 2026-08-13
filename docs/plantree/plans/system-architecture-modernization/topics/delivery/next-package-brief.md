# Implementation Entry And Completion Contract

Role: Reusable contract for starting and completing each module work unit inside the one complete modernization

Status: Accepted; no personnel, calendar, or future design approval is required

Authority: Defines the implementation-time audit, coding, integration, deletion, and evidence loop for every exact work unit

As of: 2026-07-12

Read when: Starting any row in the modular work map, resuming interrupted implementation, integrating a target module, or deciding whether its legacy responsibility can be removed

Related: [Decision 009](../../decisions/009-single-program-modular-build-and-final-cutover.md), [Decision 010](../../decisions/010-fixed-operational-and-acceptance-policies.md), [Decision 011](../../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md), [Decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md), [Modular work map](../../indexes/execution-slice-map.md), [Target modules](../../indexes/target-module-ledger.md), [Rewrite inventory](../../indexes/rewrite-inventory.md), [Traceability](../../indexes/traceability-matrix.md), [Verification](verification-rollout-and-rollback.md)

The filename is retained to preserve links from the earlier plan. This is no longer an R0 owner/selected-slice brief. It applies to all work units in one already accepted final scope.

## Meaning Of Ready

The modernization is implementation-ready at plan level because scope, target modules, technical authorities, dependency order, safety defaults, migration/recovery behavior, final cutover, whole-system rollback, and gates are fixed.

The following are implementation outputs, not plan-level blockers:

- exact symbol/responsibility mapping against the revision being changed;
- characterization results and sanitized fixtures;
- concrete source directories and internal type names;
- benchmark and operation-count reports;
- migration/adoption manifests and checksums;
- module and aggregate `EVID-*` results;
- commits, release digests, and final rehearsal artifacts.

No human maintainer, primary person, team, due date, estimate, staffing plan, or one-slice approval is required. `Owner` in architecture documents means a technical authority module only.

## Start Record

When a work unit starts, create or update one concise implementation record under the active plan status. It records:

1. work-unit ID and exact `MOD-*` authority;
2. source commit, dirty-tree identity, toolchain/dependency identity and affected worktree paths;
3. exact legacy symbols/responsibilities, callers, state, configuration reads, tasks, queues, files, network/storage effects and tests;
4. accepted public contract, dependencies, forbidden imports and target-only integration point;
5. current observable behavior, intentional safety corrections and compatibility fixtures;
6. applicable requirements/findings/gates and planned evidence paths;
7. module-specific resource/operation budgets and scoped-handle rules inherited from decisions 010-011 and the performance manifest;
8. legacy deletion search and post-deletion commands.

Discovery may narrow internal source coverage, but it may not silently change accepted product behavior, technical authority, durability, replay, resource, security, or final-cutover rules. A conflicting fact becomes a candidate and explicit plan/decision update before coding continues across that boundary.

## Implementation Loop

For each work unit:

```text
pin revision and map exact symbols
-> characterize public behavior and failure/resource paths
-> implement the final target contract and private internals
-> run focused tests and architecture checks
-> integrate into the target-only candidate
-> run affected aggregate gates
-> remove superseded legacy symbols/config/tests/adapters
-> rerun focused, aggregate, import and residue checks
-> record evidence and mark Integrated or Verified In Candidate
```

This loop is modular coding, not phased product delivery. The old production release remains authoritative throughout. Target modules never receive production traffic until the entire candidate is complete.

## Target-Only Environment

Every stateful or end-to-end work unit uses isolated target resources:

- dedicated PgSQL database/schema and Redis namespace/instance;
- dedicated Files and diagnostic roots;
- dedicated ports, browser profile/storage, Claude CLI HOME/config, logs and artifact manifest;
- deterministic fake Kiro, external-pool, media and fault endpoints by default;
- network deny-by-default except explicit loopback fixtures and separately authorized low-volume real validation;
- bounded process tree, memory, FD, task, disk, duration and cleanup watchdog.

Legacy and target comparisons use two independent state clones derived from the same sanitized baseline. They execute sequentially, never against the same mutable database/Redis/File state and never in resource competition. Results compare normalized external observations and state invariants, not private row layout.

## No Duplicate Side Effects

- Production traffic mirroring to the target candidate is prohibited.
- Legacy and target implementations never execute the same logical Kiro/external POST, media fetch, Admin mutation, Files mutation, lease acquisition or durable write.
- Pure comparison consumes immutable sanitized facts or one captured outcome; it cannot call a real dependency.
- Deterministic fakes own retry, partial-send, SSE, slow stream, redirect, disconnect and malformed-response scenarios.
- Real Kiro A/B uses distinct sequential logical operations and request/attempt IDs under decision-010 caps. It is compatibility evidence, not byte-for-byte model-output equivalence.
- A possibly transmitted POST is not retried without proven idempotency; downstream header commitment prohibits rerouting.

## Final-Code Requirements

A work unit cannot become `Integrated` while any applies:

- target runtime imports a legacy God Object, manager, store, handler, runtime context or implementation adapter;
- an unfinished dependency is provided by a release-enabled fake, stub, `TODO`, `unimplemented!()`, panic placeholder or permissive default;
- two modules can mutate the same authority or a module can reach another module's private repository/queue/lease/worker;
- a queue, cache, body, file, download, diagnostic path, retry, task, lock, script or blocking workload lacks its accepted bound and cancellation outcome;
- a secret, body, prompt, tool, query, credential or high-cardinality identifier can leak into ordinary logs, metrics, frontend persistence or artifacts;
- a real upstream can be executed twice for comparison or retry safety is inferred from an ambiguous error;
- focused evidence is missing/invalid or an unexplained regression is rerun away;
- the superseded legacy symbols and obsolete tests/config remain reachable from target source.

## Module Completion Evidence

The evidence record for a work unit includes:

- source/binary/test identity and exact commands;
- mapped requirements/findings and fixture/corpus hashes;
- unit/property/contract/storage/fault/browser/load gates that apply;
- success/failure/skip/error counts with missing data represented as invalid;
- operation counts, latency distributions and applicable absolute/relative resource results;
- cancellation/restart/shutdown/recovery behavior;
- legacy import/reference/config/schema/key/asset deletion searches;
- secret scan, artifact inventory and cleanup result;
- aggregate target-candidate results affected by the integration.

Module evidence cannot close the modernization or authorize a production activation. It proves only that the target module is ready inside the complete candidate.

## Safe Reversion During Development

An uncommitted or newly integrated target change may be reverted inside the target branch using normal version control while preserving unrelated work. This is development correction, not production rollback.

The target is never made to compile by reintroducing a broad legacy fallback. If a contract fails, correct the target module or its accepted public contract and rerun dependent units. Production rollback is whole-system only under decision 009.

## Legacy Deletion

Delete a legacy responsibility in the same module work loop after:

1. target focused and integration gates pass;
2. every target caller uses the accepted public contract;
3. characterization fixtures no longer require private legacy code;
4. source/import/config/schema/key/test searches identify the exact removal set;
5. rollback remains available through the immutable previous release artifact and additive data profile, not through target source.

After deletion, rerun compile/static, focused, affected aggregate, architecture, resource and cleanup gates. R10 later proves global zero residue; it is not the first deletion point.

## Entry Checklist

- [ ] Exact work-unit and `MOD-*` technical authority selected from the accepted map.
- [ ] Pinned source/dirty-tree identity and exact responsibility/symbol map recorded.
- [ ] Public behavior, failure paths, state/external effects and fixtures characterized.
- [ ] Accepted dependencies, forbidden imports, budgets and gates copied without weakening.
- [ ] Isolated target resources and cleanup manifest prepared.
- [ ] Legacy deletion and post-deletion search defined.

## Integration Checklist

- [ ] Final target implementation contains no release stub, legacy import, duplicate authority or unbounded path.
- [ ] Focused correctness/security/resource/performance evidence passes.
- [ ] Target-only integration and all affected aggregate gates pass.
- [ ] Superseded source/config/test/harness responsibility is deleted.
- [ ] Post-deletion architecture/import/residue and affected full gates pass.
- [ ] Evidence is versioned and the work map/module ledger state is updated.

No `implementation-status.md` is created merely because this plan is ready. Create it only when production-code implementation actually starts, and track the complete modernization rather than inventing independent production phases.
