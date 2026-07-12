# Verification, Final Cutover, And Whole-System Rollback

Role: Complete target validation strategy, host-safety contract, final system cutover runbook, rollback runbook, deletion gate, and evidence schema

Status: Accepted specification; no modernization gate has run

Authority: Defines evidence required to integrate target modules, freeze the complete candidate, activate the complete system, roll back the complete system, and remove compatibility state

As of: 2026-07-12

Read when: Implementing or validating a module, comparing legacy and target behavior, running storage/load/client/browser/recovery gates, freezing the target candidate, or executing final cutover/rollback

Related: [Decision 009](../../decisions/009-single-program-modular-build-and-final-cutover.md), [Decision 010](../../decisions/010-fixed-operational-and-acceptance-policies.md), [Decision 011](../../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md), [Decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md), [Complete plan](migration-sequence.md), [Work map](../../indexes/execution-slice-map.md), [Performance](performance-contract-and-workloads.md), [Evidence index](../../history/evidence-index.md), [Repository cleanup](repository-cleanup-and-filesystem-plan.md)

## Verification Principles

1. Compatibility, correctness, security, consistency, bounded resources, performance, recovery and evidence validity are all release gates; smaller files are not.
2. Module-focused evidence is produced during implementation, but only the complete target candidate can pass release/cutover gates.
3. Legacy and target comparisons execute sequentially against independent state clones or consume the same immutable sanitized facts. They never duplicate a side effect.
4. Production traffic mirroring to target code is prohibited. Deterministic fakes cover retry, partial-send, SSE, slow stream, faults, redirects and disconnects.
5. Real Kiro operations are distinct bounded logical requests; real Claude Code sessions are isolated. Neither is used as high-volume load.
6. Missing/unavailable metrics, skipped required cases, wrong process identity, incomplete task counts, corrupt artifacts or unexplained prior failures fail closed.
7. Every run has source, binary, dependency, configuration, workload/corpus, isolated-resource, threshold, result, artifact, secret-scan and cleanup identity.
8. A test is not accepted because it eventually passed after retries. Earlier failures remain and require adjudication.
9. No module-level production switch/canary/soak/rollback exists. `G-ROLL` is the one final full-system activation and rollback gate.
10. Legacy source is deleted during module work; `G-DEL` proves global zero residue before release and again after compatibility-state contraction.

## Evidence Layers

| Layer | Purpose | May authorize production? |
| --- | --- | --- |
| Module focused | Unit/property/contract/storage/fault/resource proof for one `MOD-*` authority | No |
| Target integration | Cross-module contract, lifecycle, state and aggregate regression after each integration | No |
| Complete candidate | Full static/Rust/storage/protocol/UI/load/recovery/client/release proof after all legacy source deletion | Only enables dress rehearsal |
| Dress rehearsal | Exact migration/cutover/whole-system rollback against isolated production-shaped state | Enables final cutover when all other gates pass |
| Final system | Production smoke, full-traffic observation, rollback readiness and post-window contraction proof | Yes, for the complete system only |

No `EVID-*` record is created until a run exists. Planning tables and historical results are not modernization evidence.

## Characterization And Comparison Corpus

The versioned black-box corpus covers:

- `/v1/messages`, `/cc/v1/messages`, `/na/v1/messages`, `/ha/v1/messages`, configured `/dfcache/*/v1/messages`, Models, Files and count-tokens;
- stream/non-stream, thinking, tools, MCP, images/media/PDF, cache controls, aliases and usage;
- external raw byte/header/event passthrough and external normalized behavior;
- validation, auth, overload, no-capacity, timeout, upstream error, malformed/truncated stream, disconnect and shutdown;
- all Admin commands/queries plus both frontend workflow sets;
- fresh/legacy/partial/drift/concurrent migration and restore/rebuild cases.

Fixtures contain no production prompt, tool body, query, key, credential, unredacted error or uncontrolled network dependency. Fixture provenance, corpus hash and expected externally observable result are mandatory.

## `G-S`: Static And Architecture Gate

Required on every module integration and the final candidate:

- format, lint, compile, unit/property tests and dependency-cycle checks;
- exact coverage of 50 module IDs and every first-party rewrite-inventory row;
- no target-runtime import of legacy managers/stores/handlers/context/facades;
- no cross-module private domain/adapter/record/queue/lease/worker import;
- no global service locator, mega context/prelude, untyped command/event/repository bypass or broad runtime snapshot leak;
- no release fake/stub/TODO/`unimplemented!()`/panic placeholder, hidden fallback or old/new selector;
- no domain import of Axum/reqwest/sqlx/Redis/filesystem/frontend DTO outside its owned edge/adapter;
- no secret-bearing logs/metrics/artifacts and no high-cardinality labels;
- plan IDs/links/states and generated contract are consistent.

Static success cannot replace runtime gates.

## `G-C`: Configuration, State, Migration, And Storage Gate

- One authenticated capture per request; every downstream view has the same runtime version and no later provider read.
- Concurrent config writes conflict through typed CAS; transaction rollback leaves no partial mutation.
- Missed Redis invalidation, replica restart and polling converge to durable PgSQL generation.
- Auth epoch follows the 2/5-second fail-closed policy; rotation/revocation and envelope encryption recover across replicas.
- Domain repositories cannot reach another authority's tables/records except through public contracts.
- Terminal/audit/Admin mutation transaction and outbox boundaries match decisions 004/010.
- Fresh, verified legacy, partial/interrupted, corrupt/drifting, concurrent, lock-loss and resume migrations pass.
- Applied checksums are immutable; unknown state fails before mutation; large backfills are bounded jobs.
- Previous binary starts or is deliberately blocked exactly as its profile states and cannot run its old mutation executor over target history.
- PgSQL/Redis latency/disconnect/restart faults preserve idempotency, bounded pools/backlogs and honest readiness.

## `G-SCH`: Scheduler Gate

Pure policy tests cover eligibility, priority, sticky, cooldown, RPM, concurrency, warmup/probation, fallback/rescue and deterministic reasons at 10/100/1,000 candidates.

Integration/fault/load tests cover:

- finite process/local/external admission and queue bounds;
- FIFO inside priority/eligibility classes;
- grant/cancel/timeout/shutdown races with no late capacity leak;
- fenced lease acquire/15-second heartbeat/60-second expiry/default 30-minute absolute lifetime and explicit progressing-stream extension;
- idempotent complete/cancel 1, 2 and 100 times;
- 1/1,000/100,000 stale entries with at most 128 processed per request-path call;
- Redis disconnect/restart/flush and epoch barrier before safe re-admission;
- no broad lock held across Redis, PgSQL, network, wait or sleep;
- accepted latency/operation/resource/recovery limits and zero unexplained queue/lease residue.

## `G-U`: Usage, Terminal, Cache, Audit, And Job Gate

- Actual, estimated, reported, accounting and actual/derived/simulated cache facts remain distinct.
- Equivalent stream/non-stream terminal facts produce equivalent usage without sharing response/terminal authority.
- Terminal/usage/credential/audit/job IDs persist and replay 1, 2 and 100 times to one effect.
- Failure before/after insert, commit, enqueue, dequeue, acknowledgement, Redis projection and restart converges.
- Required terminal/audit mutation persistence obeys clean-response and shutdown criticality rules.
- Lease completion latency is independent of usage/dashboard rollup latency.
- Usage batches meet fixed statement/transaction/row budgets; dashboard rebuild has no marker gap.
- Prompt-cache shared Redis TTL/capacity/reset behavior is bounded and never relabels simulation as upstream fact.
- Durable jobs claim once, checkpoint, cancel, resume and bound rows/WAL/disk/memory/lock/runtime.

## `G-A`: Request, Artifact, Media, Files, And Token Gate

- Route/target selection occurs before target-specific heavy work.
- Raw external paths execute zero full parse, media/PDF, Kiro conversion, payload guard and unconditional token-count operations.
- Request facts and payload revisions invalidate correctly; parse/count/serialization/copy counts meet budgets.
- Public/Admin transport obtains a governor admission handle before body allocation; `Content-Length` reservation, chunked incremental upgrades and `BoundedRawBody` rejection/release paths are exact.
- JSON depth, tools/messages/content/schema cardinality, source count, per-source/aggregate/transform bytes and scoped handles follow decisions 010-011.
- Slow headers/uploads, idle keepalive and HTTP/2 stream floods remain inside listener/task/FD/memory ceilings without starving readiness/shutdown control paths.
- DNS/IP/redirect/proxy behavior binds the validated destination to the actual connection; unsupported proof fails closed.
- Media/PDF/tokenizer cancellation releases bytes, permits, clients, tasks, FDs and memory.
- Shared Files upload/list/get/delete/materialize works from every supported replica and after restart/failover; churn stays bounded and checksums/TTL/deletion/backup restore pass.
- Count-tokens preserves protocol behavior and bounded cancellation/resource recovery.

## `G-DIAG`: Diagnostics, Sensitive Logging, And Artifact Gate

- Request-body capture is off by default and ordinary WebSearch/MCP tracing contains no raw content.
- Explicit capture enforces redaction/allowlist, safe root/symlink rules, restrictive permissions, bounded queue, 4 MiB record, 1,024 file, 256 MiB directory and 24-hour maxima.
- Quota/retention/write failure disables further capture observably and never falls back to an unlimited path.
- Restart cleanup, concurrent rotation, cancellation and process shutdown leave bounded residue.
- Metrics/logs/artifacts pass secret/body/high-cardinality scans.
- Validation output uses owned manifests and never globally prunes user/runtime files.

## `G-P`: Upstream, Retry, Protocol, Response, And Backpressure Gate

- Kiro and external prepared request, URL/path, safe headers and response facts match golden vectors.
- External raw retains accepted byte/header/event semantics; normalized paths retain accepted transformation semantics.
- Kiro/external success/error/stream bodies enforce 32/64 MiB and 64 KiB error-prefix limits incrementally, including chunked/no-length/slow bodies.
- Kiro client cache stays within 256 entries, 10-minute idle retirement, active protection and secret-safe keys under churn.
- External destination/redirect/rebinding/proxy-DNS and cross-origin credential rules fail closed.
- Connect-before-send, partial/unknown send, response, timeout/reset, malformed/truncated, cancellation and completion facts classify correctly.
- Ambiguous POST is not retried; any idempotency claim is proven for key scope/retention/payload binding.
- Downstream header commitment prohibits reroute even with zero body bytes.
- SSE content/event order, thinking/tools/cache/usage/errors and stream/non-stream behavior match fixtures.
- Slow/stopped readers, disconnects and cancellation bound buffers/tasks/connections and still complete terminal obligations.

## `G-KIRO`: Low-Volume Real Kiro Gate

Run only after deterministic fake transport gates pass. Use an isolated accepted profile, distinct logical requests/attempt IDs, sanitized metadata and decision-010 maximum 20 requests or a lower account cap.

Verify endpoint/auth/proxy/TLS, request acceptance, stream/non-stream parsing, terminal usage/error class and cleanup. Do not compare stochastic output text for equality. Do not mirror one logical request through legacy and target. Unknown cost/quota is bounded conservatively and never reported as zero.

## `G-CLI`: Real Claude Code Gate

Use an isolated CLI HOME/profile and version identity. Run three independent sessions with at least 20 turns each, covering stream order, thinking, tools, agents, MCP, Files/media where supported, model aliases, cache reporting, errors, cancellation, reconnect and final usage.

Record every session/turn result, request count, token/cost/duration caps, CLI/proxy identity and cleanup. Transcript artifacts are sanitized and bounded. Missing turns or interactive failures remain failures; a short smoke cannot substitute.

## `G-UI`: Admin And Both Frontend Applications Gate

- Rust schema generation and client/type drift gate pass from a clean checkout.
- All nine backend Admin authorities plus validation and overview/system contracts pass auth/validation/error/conflict/audit tests.
- Both `admin-ui` and `operator-ui` implement all eleven exact workflows.
- Loading, empty, stale/conflict, partial failure, retry/cancel, destructive confirmation and job progress/restart states are tested.
- Routine reads mask secrets; reveal-once/keep/replace/clear works; no reusable key/revealed secret persists in browser durable state, URL, log or artifact.
- Component, browser, cross-page/state, keyboard/focus, accessibility and responsive tests pass at supported desktop/mobile viewports with no overlap/clipping.
- Browser sessions, screenshots/videos/traces, ports and test data are isolated, bounded, secret-scanned and cleaned.

## `G-OPS`: Startup, Shutdown, Readiness, Recovery, And Deployment Gate

- Startup invokes `MOD-MIGRATIONS` through bootstrap and remains unready on unknown/partial/blocked state.
- Readiness reports dependency, auth epoch, migration, scheduler epoch, release-generation membership/digest, writer backlog, worker failure and critical residue honestly; liveness remains distinct.
- Decision-006 shutdown ordering passes before/during queue, connect, long stream, post-upstream/pre-terminal and blocked-writer cases.
- Producers join before writer ingress closes; PgSQL/Redis remain open until last durable/reconciliation need; critical residue exits non-zero.
- Clean shutdown fits the 120-second outer policy and recovers ports/RSS/FD/tasks/queues/files/connections.
- Encrypted backup plus WAL restore meets state-specific RPO <=5 minutes/RTO <=60 minutes; Redis rebuild reaches safe admission <=30 minutes.
- Files checksum, config/auth/catalog generation, terminal/outbox/usage/audit/job replay, Redis epoch and forward reconciliation pass.
- Non-root, capability-drop, no-new-privileges, writable/read-only root policy, Admin exposure/TLS guidance and envelope-key rotation pass.
- Docker/Compose/start/restart/upgrade/rollback use the same immutable release identity and no hidden mutable latest dependency.

## `G-PERF`: Performance, Load, Chaos, And Resource Recovery Gate

The [performance contract](performance-contract-and-workloads.md) and decision 010 are binding. The harness first proves target-process identity, exact offered/launched/completed/classified accounting, per-operation timing, valid metric sources, sample populations, alternating order, watchdog and cleanup.

Pass requires all of:

- absolute ordinary/raw throughput, success, local-overhead, scheduler, Redis, terminal, overload, RSS and recovery values from decision 010;
- at least five alternating legacy/target rounds on one accepted reference host and independent equivalent state clones;
- on `REF-HOST-PRIMARY` single-replica comparisons, throughput regression <=5%, p95 <=10%, p99 <=15%, peak RSS <=15%, and no unbudgeted DB/Redis operation increase;
- on `REF-HOST-MULTI` and the declared production replica topology, aggregate throughput scaling >=1.7x from one to two replicas, p95/p99 increase <=15%/20%, and per-launched-request PgSQL/Redis operations increase <=5% with zero-operation launches included, with fairness/lease/resource recovery intact;
- at least 10,000 ordinary successful samples and 1,000 applicable tail/fault samples;
- finite queues/caches/clients/files/permits/backlogs, no monotonic RSS/FD/task/connection/file growth and decision-010 idle recovery;
- 60-minute and 100,000-completed-request full-candidate stability, three process restarts, Redis restart/rebuild and PgSQL connection-loss/recovery;
- exact cleanup and no impact on protected user services/data.

Real upstream latency is compatibility evidence, not local capacity evidence.

## `G-SUP`: Supply-Chain And Release Gate

- Pinned Rust/Node/pnpm/base-image/dependency identity and reproducible release builds.
- Backend, both frontend assets, migrations/config/examples and validation tools share one signed `ReleaseGenerationManifest` with the expected instance set and exact digest identities.
- Image export/load/run/health/client smoke succeeds from the produced digest.
- SBOM, vulnerability/license policy, signature and provenance are generated and consumer-verified.
- Build context contains no runtime/user secrets, logs, local caches, test credentials or unregistered artifacts.
- Final binary/image has no legacy selector/fake/debug capture default or unsupported root/capability profile.

## Host-Safe Execution Procedure

Every storage, browser, load, recovery, real-client or release run:

1. allocates a run ID, exact workload/corpus/schema version and accepted host profile;
2. records protected ports/processes/databases/Redis prefixes/files and refuses collisions;
3. creates dedicated target resources and explicit byte/file/process/duration limits;
4. verifies the target PID/binary/digest and dependency identities;
5. runs warmup, measurement, drain and idle recovery as separate intervals;
6. accounts every task/request/result and preserves invalid/missing values;
7. enforces an outer watchdog and real-upstream request/token/cost/error stops;
8. terminates only validation-owned processes and removes only manifest-owned resources;
9. checks protected resources remain untouched and records cleanup residue;
10. hashes, bounds and secret-scans retained artifacts before evidence registration.

## `G-ROLL`: Final System Cutover And Rollback Gate

There is one production activation boundary. Percentage or stable-hash selection between internal legacy and target modules is prohibited.

### Preconditions

- all 50 modules are `Verified In Candidate`;
- `G-S`, `G-C`, `G-SCH`, `G-U`, `G-A`, `G-DIAG`, `G-P`, `G-KIRO`, `G-CLI`, `G-UI`, `G-OPS`, `G-PERF`, `G-DEL`, `G-EVID` and `G-SUP` pass for one frozen digest;
- full migration and previous-binary profiles, backup/WAL checkpoint, Redis reconciliation, evidence manifest and rollback artifact are verified;
- exact cutover and whole-system rollback dress rehearsal passes in a production-shaped isolated environment.

### Cutover

1. Close legacy public/Admin admission and job claim.
2. Drain/cancel in-flight producers, join producer barriers, then close writer ingress and reconcile durable/lease residue.
3. Capture and verify backup/WAL, Redis epoch/lease, source/release and rollback checkpoints.
4. Acquire the fenced migration lock; verify rehearsal/identity/resource/time budgets and execute the complete target expand/adopt/backfill plan inside the decision-014 one-window limit.
5. Deploy the complete target backend and both target frontend artifacts to every expected instance with readiness and load-balancer admission closed.
6. Require per-instance attestation of one release generation/backend/frontend/schema/config digest set, bind the migration fence to that generation, and prove every old generation is stopped/routing-fenced; any missing/unexpected/mismatched instance blocks traffic.
7. Through a deployment-private path, verify migrations, auth epoch, catalog, queues/leases, outbox, Files, API/Admin and an isolated test-account Claude Code workflow without duplicate real side effects while public admission remains closed.
8. Open readiness and all new production traffic once, then start the 24-hour full-system observation window; post-open checks observe the already-smoked release.

### Rollback Triggers

Rollback the complete release on any unexplained:

- protocol/client/Admin/UI compatibility break or duplicate-side-effect possibility;
- durable-state divergence, migration/adoption/postcondition failure or unrecoverable outbox/job residue;
- auth/secret/egress/security exposure;
- queue/lease/capacity leak, readiness dishonesty, shutdown critical residue or recovery failure;
- absolute/relative performance or resource-recovery failure;
- release/image/provenance mismatch or inability to execute required operator workflows.

### Whole-System Rollback Procedure

1. Close target admission and do not move in-flight requests between binaries.
2. Drain/cancel target producers, checkpoint durable work and reconcile target leases/Redis epoch.
3. Evaluate every row in the rollback-state gate below; any undeclared or failed state keeps previous traffic closed.
4. Verify the previous-binary additive-schema/contained rollback profile, disabled old migration writers and absence of an active target migration.
5. Deploy and attest every expected immutable previous backend/frontend instance as one generation while target instances remain stopped/routing-fenced.
6. Do not reverse DDL blindly, delete target ledger/outbox/job rows, rewrite checksums or run the legacy migration executor over target history.
7. Run previous-release private smoke/readiness, including old-UI containment and characterized target-File non-success, before opening old public traffic; preserve failed target evidence.
8. Before retrying the target, forward-reconcile every target-written durable/coordination fact and rerun the complete failed gate set plus dependent gates.

### Rollback-State Gate

| State authority | Required verdict before previous traffic opens |
| --- | --- |
| Migration/schema | Additive postconditions and previous-reader probes pass; target run inactive; old runner/repair/backfill disabled |
| Terminal/outbox/usage/audit | Critical obligations reconciled; target writers fenced; durable cursors checkpointed; usage/audit previous-query projections caught up |
| Jobs and automatic credential/pool outcomes | Claims stopped/checkpointed; previous selection-state projection reconciled or the mutation class is paused |
| Redis/schedulers/prompt-cache | No unaccounted target lease; target generation fenced; old epoch rebuilt with missing-instance capacity accounted; derived facts reset/rebuilt without false authority |
| Auth/config/catalog/proxy/pool | Manual mutation freeze held; previous-readable version/epoch probes pass; target sessions/CSRF invalidated |
| Secrets | Automatic refresh compatibility projection current; no unsupported manual mutation; external key/backup manifest recoverable |
| Files | Target rows/checksums preserved; previous artifact's characterized non-success for target-created IDs is accepted/documented; no empty/success result |
| Backend/frontends/instances | One previous generation attested; target instances fenced; old Admin UI/login network-blocked by default under the rollback containment profile |

## `G-DEL`: Legacy And Compatibility Deletion Gate

Before the target candidate freezes:

- every rewrite-inventory responsibility has a target implementation or explicit accepted non-runtime reuse/removal;
- target source has zero old manager/handler/store/service/facade execution, selector or fallback;
- old frontend workflow/type/client implementations, old migration runner, hidden startup backfills and invalid harness paths are removed;
- compile/import/reference/config/route/schema/key/feature/asset searches and full post-deletion gates pass;
- rollback depends on the immutable previous artifact, not code embedded in the target.

After the 24-hour final observation:

- contract compatibility-only columns/indexes/Redis keys/projections under immutable manifests;
- retain required audit/migration/evidence history;
- rerun migration fresh/legacy/restore, previous-state interpretation, full static/storage/protocol/UI/load/recovery/release gates;
- record zero unexplained references or residues.

## `G-EVID`: Durable Evidence Manifest

Each record includes:

- `EVID-*` ID, result, date, work unit/module or full-system scope and gate;
- source commit/tree patch identity, binary/image/frontend digests and release selector (`target-only` or complete final release, never module percentage);
- exact commands, tool/dependency versions, config hashes, host/dependency/network profile and isolated resources;
- fixture/workload/corpus/report-schema identities, offered/launched/completed/result counts and sample populations;
- expected thresholds and actual correctness/latency/operation/resource/recovery/cost result;
- fault/cancellation/restart/rollback/deletion behavior where applicable;
- artifact paths, bytes/files/digests/retention, secret scan and cleanup result;
- all prior failed/blocked/partial runs and adjudication;
- links to requirements, findings, decisions, module/work-unit/inventory rows.

No reviewer name, accountable person, due date or calendar estimate is required. Evidence authority comes from reproducible identity and results.

## Current Verification State

All gate specifications are accepted. No Rust/frontend build, storage drill, Docker run, browser test, load/chaos run, real Kiro request, real Claude Code session, migration rehearsal, cutover or rollback has been performed for this modernization plan. Every gate remains Not Run and production cutover remains Not Ready.
