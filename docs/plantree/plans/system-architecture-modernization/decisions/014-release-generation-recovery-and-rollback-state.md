# 014: Release Generation, Recovery Barrier, And Rollback State

Date: 2026-07-12

Status: Accepted

Scope: Expected replica identity, whole-release fencing, Redis-loss recovery membership, one-window production migration, previous-binary state compatibility and degraded rollback behavior

Affected requirements/findings: `FUN-012`, `FUN-017`, `FUN-022`, `INV-003`, `QA-REL-002`, `QA-REL-005`, `QA-OPS-001`-`QA-OPS-003`, `OPS-001`, `OPS-004`, `OPS-005`

Refines: [Decision 005](005-scheduler-queue-and-lease-lifecycle.md), [decision 008](008-domain-owned-migrations-and-recoverable-adoption.md), [decision 009](009-single-program-modular-build-and-final-cutover.md), and [decision 010](010-fixed-operational-and-acceptance-policies.md)

Related: [Migration subsystem](../topics/delivery/migration-foundation-brief.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Verification and final cutover](../topics/delivery/verification-rollout-and-rollback.md), [Release harness](../indexes/target-module-ledger.md)

## Context

The earlier plan said every replica would acknowledge a Redis recovery barrier and every previous-binary state would be compatible, but it did not define the expected replica set or what happens to state the immutable previous release cannot read. It also mixed pre-completed production backfills with a rule that target migration work cannot mutate the production database while legacy remains authoritative.

## Release Generation Manifest

Every supported multi-replica deployment has one immutable `ReleaseGenerationManifest` containing:

- release generation ID and manifest revision;
- backend image/binary digest, both frontend digests, schema/migration-plan hash and configuration-schema hash;
- expected instance IDs or an exact expected replica count plus deployment-platform membership attestation;
- each instance's maximum local/external scheduler capacity and resource-governor profile;
- previous complete artifact identity, rollback profile and generation transition ID.

`MOD-RELEASE-HARNESS` produces/signs the manifest. `MOD-BOOTSTRAP` receives one instance ID and the manifest. Each instance attests generation/digests and heartbeats through a narrow PgSQL generation registry owned by `MOD-READINESS`; `MOD-RECOVERY` owns transition/recovery barrier runs. No arbitrary application module reads or mutates the registry.

The generation registry contract is fixed rather than left for implementation choice:

- each instance commits one attestation heartbeat every 5 seconds containing generation ID, instance ID, boot UUID, exact digest set, monotonic heartbeat counter and non-secret readiness bits;
- an attestation is live only when its digest/generation match, its counter advances and its committed age is at most 15 seconds;
- an instance that cannot commit and read back its own current heartbeat for 10 seconds closes new public/Admin/job admission; in-flight work follows normal bounded completion, while liveness remains a separate process check;
- PgSQL registry unavailability or stale/missing/unexpected membership makes the generation not ready and prohibits cutover, Redis-loss recovery completion and rollback traffic opening;
- rows for the current and previous rollback-capable generations are never pruned. A bounded maintenance job may delete at most 128 heartbeat rows per run only after an older generation is terminal, no longer rollback-capable and has been terminal for 24 hours, while signed manifests, transition verdicts and release evidence remain under their longer retention policy.

Before traffic opens, every expected target instance must attest the same generation/digests and pass readiness. Deployment evidence must prove every old-generation process/container is stopped or fenced from the load balancer and cannot re-register. A mixed generation, missing expected target, unexpected old target or digest mismatch keeps the complete generation closed.

## Redis-Loss Recovery Barrier

On Redis epoch loss:

1. every expected instance closes scheduler admission and publishes its local active-lease counts/identities under the generation recovery run;
2. an acknowledged instance re-registers matching active leases into the new epoch before it reopens;
3. a missing/partitioned instance is safe only when the deployment platform attests it terminated/fenced, or the new epoch reserves that instance's full declared scheduler capacity until its maximum possible lease lifetime expires;
4. admission may reopen only for capacity remaining after acknowledged leases plus conservative missing-instance reservations; if no safe capacity remains, readiness stays closed;
5. a stale generation/instance token cannot heartbeat, complete or acquire in the new epoch.

The supported production platform must be able to fence an unresponsive instance within the Redis recovery RTO. Without that attestation, the plan does not pretend the 30-minute recovery objective passed; conservative reservation/fail-closed behavior remains in force.

## One Production Migration Window

No target migration or backfill mutates production before legacy public/Admin admission, job claim and all legacy producers/writers are stopped. All structural expand/adoption, data backfills and the bounded tail execute under one fenced final maintenance window.

A recent production-shaped clone rehearsal must prove the exact plan finishes within 45 minutes, leaving 15 minutes for target boot/readiness/smoke inside a 60-minute cutover restoration ceiling. Before the first production mutation, the inspector verifies database/backup identity, row/cardinality estimates, disk/WAL/lock/memory budgets and the rehearsal hash. If any step lacks a bounded, checkpointed plan or cannot meet the ceiling, cutover does not mutate production.

If the deadline or a postcondition fails after additive work begins, stop/checkpoint the idempotent plan and return to the previous release against the compatible additive state. Never run reverse destructive DDL. A later attempt forward-reconciles from the recorded ledger; it does not pre-run target jobs while legacy serves traffic.

## Rollback-Window Mutation Policy

The 24-hour observation window is one release safety state, not a second product rollout. To keep the immutable previous artifact usable:

- manual Admin mutations to auth keys, reusable secrets, runtime config, model/pricing catalog, proxy resources and external pools are frozen with a stable `rollback_window_frozen` response;
- new maintenance jobs/catalog sync that can change previous-reader state are paused; automatic credential refresh, credential outcome/auto-disable and external-pool auto-disable either update an idempotent previous-reader compatibility projection in the same authority transaction or are disabled for the window. Rollback reconciliation restores the latest compatible selection state before old admission;
- target data-plane requests, usage, terminal/audit facts and shared Files may continue under the matrix below;
- post-window contraction removes compatibility projections and unfreezes target-only mutations only after the full observation gate passes.

## Per-Authority Rollback Matrix

| Authority/state | Observation-window rule | Condition before previous traffic opens |
| --- | --- | --- |
| Schema/migration ledger | Additive only; target runner fenced and old runner disabled | No active target migration; checksums/postconditions valid; previous-reader probes pass |
| Terminal/outbox/usage/audit | Terminal/outbox remain target-only; `MOD-USAGE`/`MOD-AUDIT` maintain idempotent previous-query compatibility projections during the window | Producers stopped; critical terminal/lease obligations reconciled; usage/audit projections caught up to the durable cursor; residual target backlog checkpointed and target writers fenced |
| Maintenance jobs | No incompatible new jobs; running work drains/cancels/checkpoints | No active target claim; checkpoint rows preserved for forward recovery |
| Scheduler/Redis | Versioned target namespace and generation tokens | No unaccounted target lease; old namespace rebuilt under a new epoch; target generation cannot complete/acquire |
| Runtime config/catalog/pools/proxies/auth keys and automatic health/outcomes | Manual mutation frozen; required automatic state uses one owner-written old projection or is paused | Generation/version and automatic selection state reconcile to a previous-readable projection and probes pass |
| Reusable secrets | Target reads ciphertext; minimum legacy projection exists only for rollback | Projection is current for automatic refresh, old reader succeeds, key manifest is recoverable; no unsupported manual mutation occurred |
| Browser sessions/CSRF | Target-only hashed Redis state | All target sessions/tokens invalidated; operator reauthenticates with the frozen previous Admin key |
| Shared Files | Target objects remain durable; previous binary cannot materialize target-created IDs | Checksums/rows preserved; the characterized previous binary returns its existing stable non-success/not-found class, never empty/success; release notes declare temporary unavailability and target reactivation restores access |
| Prompt-cache/dashboard/other derived Redis | Rebuildable and non-authoritative | Target namespace fenced; old projection rebuilt/reset without claiming target facts |
| Release/backend/frontends | One generation only; previous browser UI is not treated as target session security | Every expected old instance attests the previous digest; target instances are stopped/fenced; previous Admin UI/login is network-blocked by default and emergency Admin uses a restricted non-browser channel |

The declared previous-release rollback containment profile disables ordinary sensitive diagnostics, sets finite admission/queue ceilings available to the old binary, disables its migration/rollup-on-start writers, restricts Admin networking, blocks the old browser UI/login by default and pins the exact compatibility configuration. If emergency use of the old UI is explicitly authorized during an incident, it uses an isolated temporary browser profile and verifies removal of reusable keys/storage afterward. Target-created Files are preserved but temporarily unavailable, and old interactive Admin UI workflows are intentionally unavailable by default. These and the previous release's already registered correctness/security/resource limitations are explicit temporary rollback risks, not claims that the old artifact satisfies target gates. Any undeclared state incompatibility blocks final cutover.

If any matrix condition cannot be proved, old production traffic remains closed. An emergency data-loss or no-rollback exception requires a separate live operator instruction and incident record; it is not pre-authorized by this plan.

## Verification

- Cutover tests omit, partition, duplicate and mismatch expected instances; traffic opens only for one complete attested generation.
- Redis-loss tests include a responding instance, a partitioned live stream, an attested-dead instance, full-capacity reservation, stale token and recovery-RTO failure.
- The exact one-window migration runs against fresh/recent production-shaped clones at boundary size and aborts before mutation when its identity/budget/rehearsal proof is absent.
- Rollback tests exercise every matrix row, target session invalidation, old-UI containment, frozen Admin mutations, automatic credential/pool outcome compatibility, usage/audit projection catch-up and degraded-but-preserved target Files.
- Evidence records generation/instance/digest sets, barrier acknowledgements/reservations, migration timing/checkpoints and per-authority rollback verdicts without secrets.
