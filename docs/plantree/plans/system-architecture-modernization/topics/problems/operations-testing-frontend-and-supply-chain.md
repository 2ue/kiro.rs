# Operations, Testing, Frontend, And Supply Chain

Role: Detailed operational and delivery finding analysis
Status: Open findings; production hardening is required by accepted decisions 010 and 011
Authority: Evidence and acceptance conditions for the listed IDs
As of: `v0.0.102`, commit `e9479df71ee0`, updated 2026-07-12
Read when: Changing deployment, lifecycle, Admin jobs, frontends, CI, release, evidence, or recovery
Related: [Problem index](README.md), [Current deployment](../../../../baseline/deployment-and-operations.md), [Verification](../delivery/verification-rollout-and-rollback.md), [Repository hygiene](../delivery/repository-cleanup-and-filesystem-plan.md), [Decision 010](../../decisions/010-fixed-operational-and-acceptance-policies.md), [Decision 011](../../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md), [Decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md)

## `OPS-001`: Deployment Health Check Does Not Use Application Readiness

Severity: P2
Technical authority area: deployment and readiness

### Evidence

- The application exposes `/healthz` and dependency/runtime-event-aware `/readyz`: `src/main.rs:847-887`.
- `docker-compose.deploy.yml:13-18` checks only `nc -z 127.0.0.1 8990`.

A process can accept TCP while PgSQL, Redis, runtime-event synchronization, or required workers are unhealthy.

### Required Target And Acceptance

Compose/orchestrator health checks use `/readyz` for traffic readiness and `/healthz` only for liveness. Tests verify state transitions during PgSQL/Redis failure, event-listener lag, writer backlog threshold, shutdown, and recovery.

## `OPS-002`: Required Writer Abandonment Can Exit Successfully

Severity: P1/P2
Technical authority area: process lifecycle and data durability

### Evidence

Shutdown logs usage and storage drained/timed-out/abandoned counts at `src/main.rs:595-623`. The explicit process failure condition at `src/main.rs:632-640` covers incomplete credential statistics, not all required usage/storage residue.

### Required Target And Acceptance

Classify writer outputs as required durable, derived/rebuildable, or best effort. Required undrained events cause non-zero exit and a machine-readable residue summary. A deliberate blocked-writer SIGTERM test proves the exit code; ordinary in-flight drain remains successful.

## `OPS-003`: Background Jobs Are Not Uniformly Supervised Or Durable

Severity: P2
Technical authority area: Admin cleanup, audit, background lifecycle

### Evidence

- Usage cleanup job state and cancellation are kept in the current process: `src/admin/service.rs:3708` onward.
- Admin audit writes use untracked `tokio::spawn`: `src/admin/service.rs:1419`.
- Different modules own their own task handles, retry, shutdown, and metrics conventions.

### Required Target And Acceptance

One `TaskSupervisor` registers worker name, criticality, queue/backlog, restart policy, health, drain order, and outcome. Cleanup jobs persist ownership/progress in PgSQL and use a database lease/`SKIP LOCKED`-style claim. Audit events include actor/key fingerprint, request ID, action, object, outcome, and are durably drained without raw secrets.

## `OPS-004`: Recovery Behavior Has No Executable Versioned Runbook

Severity: P2

Technical authority area: PgSQL/Redis operations, schema compatibility, incident recovery

### Evidence

The repository documents startup, migrations, integration tests, and selected restart scenarios, but no single versioned executable runbook proves:

- PgSQL backup plus restore into an isolated target;
- previous-binary compatibility against expanded schema;
- forward reconciliation after a failed/cancelled backfill;
- Redis loss/rebuild behavior by coordination-critical versus derived key class;
- runtime config/credential/pool recovery and secret handling;
- recovery objectives, maximum tolerable loss, and operator verification steps.

### Required Target And Acceptance

- versioned commands/preconditions for backup, restore, validate, reconcile, and abort;
- destructive operations require isolated target identity and explicit confirmation;
- Redis key-class table states rebuildable, TTL-recoverable, or coordination-critical behavior;
- expand-contract recovery tests use the previous binary against the expanded schema rather than requiring destructive down migration;
- release/periodic drills record source commit, backup identity/hash, timing, row/key reconciliation, secret scan, and cleanup.

A successful isolated restore/reconciliation drill is required before `MOD-RECOVERY` can become `Verified In Candidate` and before the complete-system recovery/release gates can pass.

## `OPS-005`: Startup Migration Has Mutable, Non-Atomic Inline Progress

Severity: P2; it blocks R2 state integration and the final candidate

Technical authority area: domain-owned PgSQL manifest/SQL/DDL, `MOD-MIGRATIONS` common runner/ledger, `MOD-RECOVERY` backup/rebuild orchestration, bootstrap invocation

### Evidence

- `PostgresConfig` defaults `migrate_on_start` to true: `src/model/config.rs:2455-2474`.
- Every normal `PostgresStore::connect` runs `migrate_with_options` when enabled: `src/storage/postgres.rs:200-220`.
- Migration first executes the complete current `SCHEMA_SQL` before the separately versioned records: `src/storage/postgres.rs:280-353`.
- The inline executor splits on semicolons and runs statements directly on the pooled connection without one transaction: `src/storage/postgres.rs:3437-3447`.
- The `inline-schema` row overwrites its checksum after execution rather than rejecting drift from the previously applied definition: `src/storage/postgres.rs:342-353`.
- Inline schema includes table-wide updates and usage-rollup backfills such as `src/storage/postgres.rs:6793-6810`, `7032-7283`.

An advisory lock prevents two current processes from running this function simultaneously, but it does not make delimiter splitting atomic, preserve immutable historical migration identity, distinguish structural DDL from online backfill, or make a partially completed startup step resumable and observable.

### Required Target

- every state-owning module owns immutable, ordered, checksummed manifest instances, SQL/DDL, probes and owner-backfill handoff;
- `MOD-MIGRATIONS` owns only common manifest validation, fencing, runner and active/applied/adopted/checkpoint ledger; `MOD-RECOVERY` separately owns backup/restore/Redis rebuild/forward recovery; `MOD-BOOTSTRAP` owns prerequisites, order, public-contract invocation and readiness gating;
- fresh databases self-bootstrap the common ledger, while current databases use a reviewed legacy-to-owner adoption map and catalog/constraint/index/owner probes; partial, corrupt or drifting state remains blocked rather than being marked applied;
- a previously applied migration with a different checksum fails before executing new statements;
- transactional DDL is atomic where PostgreSQL permits it; non-transactional work has explicit resumable checkpoints and failure state;
- large data backfills are separate, rate-limited, cancellable, observable jobs rather than hidden startup work;
- the migration contract includes advisory/fencing behavior, compatible concurrent replica startup, previous-binary expand-contract support and forward reconciliation;
- migration identity, duration, lock wait, rows scanned/changed, failure/resume state and required disk/WAL headroom are evidence fields.

### Acceptance

- Tests inject failure after each step and prove restart either rolls back or resumes without duplicate/corrupt effects.
- Checksum drift is rejected before mutation; two replicas converge on one applied history.
- The previous complete binary runs against the expanded schema for the one whole-system rollback window.
- Fresh, verified-legacy, partial/interrupted, corrupt/drift and concurrent-start fixtures pass, and post-deletion search proves the delimiter runner, mutable inline marker and hidden startup backfills are no longer live.
- A production-scale synthetic usage table demonstrates accepted startup lock/latency and online backfill budgets, followed by isolated restore/forward-reconciliation evidence.

## `API-001`: Frontend Contract Gate Has No Rust Authority

Severity: P2
Technical authority area: Admin API and two maintained frontends

### Evidence

Both `admin-ui` and `ui` maintain large handwritten TypeScript API type files. `scripts/check-frontend-contracts.mjs` compares the TypeScript surfaces with each other, so identical drift from Rust remains undetected.

### Required Target

- define Rust/OpenAPI/schema authority for Admin endpoints;
- generate or schema-validate shared TypeScript client/types;
- both rewritten frontends consume the generated target contract, and no legacy UI adapter remains in either target artifact;
- CI fails when Rust route/DTO changes without regenerated contract;
- sensitive fields and write-only secrets are encoded in the schema.

### Acceptance

A controlled Rust DTO change fails the contract gate before regeneration and compiles both frontends after regeneration. Endpoints used by only one UI are still represented and tested.

## `TEST-001`: Frontend Behavior Has No Automated Test Suite

Severity: P2
Technical authority area: frontend quality

### Evidence

No Vitest/Jest/Playwright/Cypress test/spec suite was found for either maintained React frontend. Build success detects type/bundle failures but not interaction, routing, validation, stale data, conflict, or error behavior.

### Required Target

- component tests for form normalization, secret handling, config conflict, route/pool semantics, and usage filters;
- browser E2E for login/key rotation, credential/pool CRUD, `preservePath`, runtime config conflict, usage queries, readiness/error states;
- both desktop and mobile layout checks for maintained workflows;
- no production secrets or mutable production backend in UI tests.

### Acceptance

CI runs focused component and browser suites against an isolated deterministic backend fixture, then builds both frontends.

## `TEST-002`: No Continuous Performance Regression Gate

Severity: P2; the concrete runtime costs remain classified under `PERF-*`
Technical authority area: CI, loadtest, benchmark evidence

### Evidence

- `src/bin/kiro_loadtest.rs` supports substantial fake-upstream load and chaos scenarios.
- There is no `benches/` Criterion/Divan suite or checked performance threshold.
- Historical load artifacts live mainly under ignored `target/loadtest`, so raw data is not durable.

### Required Target

Two levels of gates:

1. host-safe PR micro/characterization tests for pure scheduler, cache projection, raw planning, serialization counts, and bounded resource behavior;
2. nightly/release load and chaos covering slow upstreams, abrupt bursts, widespread errors, dependency latency/restart, real Claude Code sessions, external pools, tools/files/search/MCP/multi-agent protocol calls, and recovery.

Both levels use versioned canonical workload manifests and the [performance contract](../delivery/performance-contract-and-workloads.md). Its thresholds and hard limits are already accepted. R0 implements the valid measurement/report foundation and final harness, R1 produces the reference-host manifests, evaluator and measured calibration evidence, and R9 integrates those gates into CI/nightly/release orchestration.

### Acceptance

Each performance-affecting target work unit and the complete candidate satisfy the accepted absolute capacity/outcome SLO and same-host old-versus-target regression threshold. Reports include workload/host/schema identity, exact command/config/commit, metric sample populations, offered/launched/completed outcomes, latency/operation/resource/recovery/cost summary, artifact manifest, cleanup result, and no secret-bearing bodies. A report affected by `TEST-004` measurement invalidity cannot close this finding.

## `TEST-003`: Required Real Claude Code Long-Session Gate Is Not Durably Proven

Severity: P2 as a current evidence gap; release-blocking for rewritten `/cc`, Files, cache, tool, MCP, or response paths

Technical authority area: real client validation and durable evidence

### Evidence

Historical runtime evidence states that Claude Code CLI cases passed, but it does not durably record three sessions, at least 20 user/assistant turns each, ccman profile switching/restoration, the full tools/Files/search/MCP/multi-agent matrix, or per-round cache records.

The target gate is specified in the verification document, but no existing evidence should be read as proof of that stronger target.

### Required Target And Acceptance

- three independent real CLI sessions, each with at least 20 user/assistant conversational turns; tool/MCP subrequests do not count as turns;
- record CLI version, proxy commit/version, sanitized ccman profile ID/hash, selected `/cc/v1` base URL, and request IDs proving traffic hit the test proxy;
- restore the original ccman profile and verify it after every session/run;
- cover thinking, sequential/parallel tools, Bash/file operations, search, MCP, Files/image/document recognition, and multi-agent-triggered calls;
- verify content recognition using unique synthetic markers, not HTTP 200 alone;
- export and durably summarize cache miss/creation/read/shaping fields and usage conservation for every round;
- retain no prompts, tool contents, file bodies, or credentials.

The release evidence manifest must make session/turn/feature counts independently auditable.

## `TEST-004`: Current Loadtest Measurements Can Misidentify The Target And Produce Invalid Gate Evidence

Severity: P2; blocks `G-PERF` acceptance until contained and verified

Technical authority area: performance harness, metric validity, load/chaos evidence

### Evidence

- `--target-pid` is optional, and the default target is `std::process::id()`, which is the `kiro_loadtest` process rather than the proxy: `src/bin/kiro_loadtest.rs:88`, `335`.
- Required RSS, FD, and CPU samples use `unwrap_or_default()`, so a missing/failed `ps`, `/proc`, or `lsof` measurement becomes numeric zero rather than an invalid run: `src/bin/kiro_loadtest.rs:1277-1282`.
- When `execute_request` returns an error, the recorded request latency is `run_started.elapsed()` for the whole run instead of that request's elapsed time: `src/bin/kiro_loadtest.rs:410-426`.
- A failed `JoinSet` task is logged but no `RequestResult` is appended, so launched work can disappear from completed/error counts: `src/bin/kiro_loadtest.rs:428-430`.
- The resource sampler is aborted and `resource_end` is captured immediately after requests join; no idle cooldown proves that RSS/FD/tasks/connections return within a recovery band/deadline: `src/bin/kiro_loadtest.rs:434-436`.
- Empty percentile populations are emitted as zero and the report does not record per-metric sample counts: `src/bin/kiro_loadtest.rs:218-223`, `1256-1265`.

### Impact

A report can measure the driver instead of the service, represent an unavailable metric as an excellent zero, corrupt failure tail latency, undercount panicked tasks, and claim end-state recovery before a cooldown occurs. These are not merely missing features: they can make an automated performance/resource gate produce false or non-attributable evidence.

### Required Target

- require or derive and verify the target proxy PID/start-time/command/port/binary identity for any resource gate;
- keep load-generator, fake-upstream, proxy, PgSQL, and Redis process identities separate;
- represent unavailable measurements as typed invalid/missing values and fail any gate that requires them;
- account exactly once for every offered, launched, completed, failed, cancelled, timed-out, and panicked operation;
- use per-operation timestamps and separate success, expected-failure, and unexpected-failure latency populations;
- report percentile sample counts and never equate an empty population with zero latency;
- separate warmup, measured work, drain, accepted idle cooldown, and final recovery sampling;
- version and validate the report/workload schema before threshold evaluation.

### Acceptance

- A fixture with separate driver and proxy PIDs proves the report measures the proxy and rejects a stale/wrong PID.
- Forced metric-command failure yields a blocked/failed measurement, not zero-valued success.
- Injected request errors and task panics leave `launched = completed terminal classifications` and preserve request-local latency.
- A delayed resource-release fixture fails before cooldown and passes only after the accepted recovery band/deadline is met.
- The R0 deterministic smoke manifest passes the harness-validity checks, after which R1 produces measured calibration evidence against the already accepted thresholds; no pre-fix report closes `TEST-002` or any `PERF-*` finding.

## `DOC-001`: Plan Status And Evidence Drift From Durable Reality

Severity: P2
Technical authority area: plan tree and validation evidence

### Evidence

- The previous root authority order placed the current conversation above durable requirements, making detached interpretation impossible.
- Request-body and Admin plan statuses in the root index lagged their plan-local completion state.
- Runtime evidence records `1079/1079` and `19/19` as dated results, while later runs reported different counts; historical counts lacked a consistent commit qualifier in every reference.
- Several status files reference ignored `target/loadtest` paths as their primary evidence.
- At audit time, `target/loadtest` contained approximately 28,967 files and 197 MiB; one modular evidence subtree accounted for about 151 MiB. The broader ignored project inventory was approximately `target` 10 GiB/68,802 files and `.local-run` 228 MiB/15,087 files, including protected/active categories that cannot be broadly deleted.
- `v0.0.102` was intentionally released under a one-time user exception that skipped local compilation verification, while the registered Docker end-to-end gate remained incomplete.
- The release instruction referred to an Excel export, but no versioned evidence manifest identifies the requested cache-test workbook, its path/hash/sheets/rows, or cleanup disposition. A local `tmp/usage_2026-07-02_to_2026-07-03.xlsx` exists, but no durable record proves it is the requested artifact.
- The historical release-binary hashes in runtime evidence do not name their exact source commit/version, so they cannot be attributed to the later version-only `v0.0.102` release commit.
- At audit time, `docs/ai-docker-compose-deployment.md` was an unclassified active-looking operations document with stale version examples. It is now explicitly classified as a legacy/current-release reference; the target deployment runbook still does not exist, and the legacy guide cannot govern target release identity or evidence.

### Required Target

- durable requirements/accepted decisions replace chat as product authority;
- every evidence statement includes version/commit/date and distinguishes historical from current;
- raw artifacts are ephemeral; versioned evidence retains sanitized summary, commands, exit codes, hashes, manifest, and cleanup status;
- perform a bounded, project-only adjudication of existing ignored run directories by run ID: protect worktrees/active data, summarize evidence still needed, delete only manifest- or provenance-proven residue, and record unresolved provenance; never run a global prune;
- one-time release exceptions are dated, scoped, and never generalized to future releases;
- roadmap owns complete-program, work-unit and system state; implementation status owns only the active handoff; history owns completed evidence.

### Acceptance

Automated link/status checks find no contradictory active state. A fresh reader can identify current facts, the accepted target, accepted and superseded decisions, active work, and historical evidence without the original conversation or ignored files.

## `DOC-002`: The Primary README Links To A Missing Claude Code Test Guide

Severity: P2

Technical authority area: public documentation and real-client validation

### Evidence

`README.md:55` links to `docs/claude-code-cli-local-testing.md`, but that file does not exist in the current tree. The historical regression report and protocol before/after runbook do not automatically replace a maintained local-testing guide: both contain dated environment assumptions and historical evidence rather than one supported current workflow.

### Impact

The primary project entrypoint sends operators to a dead path for a compatibility-critical workflow. Replacing the link with an arbitrary historical report could also cause stale ports, credentials, ccman state, or verification claims to be treated as current instructions.

### Required Target

- `MOD-REAL-CLIENT-HARNESS` and the current documentation authority jointly define the local Claude Code validation workflow;
- create or regenerate a safe runbook from the accepted real-client gate, or remove the README entry when the workflow is intentionally unsupported;
- isolate CLI HOME/config and restore the original ccman profile;
- avoid hard-coded credentials, ordinary development state, and historical pass claims;
- validate all public Markdown links in the static gate.

### Acceptance

The README points to a versioned, current, secret-safe workflow whose commands and authority are explicit, and the full Markdown link check passes from a clean checkout.

## `SUP-001`: Release Supply-Chain Evidence Is Incomplete

Severity: P2
Technical authority area: release workflow

### Evidence

- Docker images use a mutable `latest` default in Compose.
- `.github/workflows/docker-build.yaml:109` explicitly disables provenance.
- No SBOM, signature, or attestation publication gate was found.

### Required Target

- immutable version/digest deployment examples;
- generated SBOM for binary and image;
- signed image/tag or equivalent verifiable release identity;
- build provenance/attestation;
- dependency and license/security scan policy with documented exception handling;
- release manifest connecting Git tag, Cargo version, commit, image digest, binary hashes, SBOM, and verification evidence.

### Acceptance

A release consumer can verify artifact origin and exact source commit without trusting mutable tags.

## Required Production Hardening (Decisions 010 And 011)

The following items were deferred by the older runtime-correctness plan. Accepted decisions 010 and 011 now make them binding target requirements and final-release gates; they are not optional follow-up work or separate modernization phases:

- non-root container user;
- read-only root filesystem;
- Linux capability drop;
- TLS termination/application TLS policy;
- Admin network isolation;
- application-level database secret encryption through `MOD-SECRET-ENVELOPE`.

Planning acceptance does not mark these items implemented. The target candidate must provide their exact configuration, negative tests and release evidence before the complete-system gates can pass.

## Recovery And Runbook Gaps

The repository has strong startup/integration/load evidence but no complete, versioned operational runbook proving:

- PgSQL backup and point-in-time restore;
- Redis loss/rebuild behavior by key class;
- schema expand/contract rollback across binary versions;
- runtime config and credential export/import recovery;
- executable proof of the accepted disaster-recovery objectives and maximum tolerable data loss;
- diagnostic/log retention enforcement;
- external pool credential rotation and revoke procedure.

These remain P2 operational findings and final-candidate/final-release blockers. Decisions 010 and 014 already fix the supported recovery objectives, release-generation barrier, one production migration window and previous-release rollback matrix; executable recovery evidence is still missing and must pass before the modernization can complete.

## Release Exception Record

`v0.0.102` at commit `e9479df71ee0044cfa0da8acbf69d98c2259a66f` was published on 2026-07-11 under a one-time explicit instruction to update the version, tag, and push without local compilation verification. This exception applies only to that release. It does not supersede the normal release gates for later versions and does not imply the incomplete end-to-end Docker gate passed.
