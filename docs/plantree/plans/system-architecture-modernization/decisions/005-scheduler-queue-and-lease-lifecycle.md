# 005: Scheduler Queue And Lease Lifecycle

Role: Architecture decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding scheduler queue admission, acquisition, heartbeat, completion, cancellation, fencing, and Redis recovery contract

Scope: Local credential and external-pool scheduling coordination, queue wait, global/per-target capacity, RPM, cooldown, sticky state, stream activity, cancellation, Redis failure, restart, and shutdown

Affected requirements/findings: `FUN-010` through `FUN-013`, `FUN-017`, `INV-003`, `INV-004`, `ARCH-001`, `PERF-001` through `PERF-003`, and the R4 scheduler gates

Decision source: Architecture-contract reconciliation and final-plan convergence on 2026-07-12; fairness/timing values are fixed by decision 010 and delivery mechanics are fixed by decision 009

Related: [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Runtime flows](../topics/architecture/runtime-control-and-data-flows.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Rewrite sequence](../topics/delivery/migration-sequence.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md), [Open questions](../open-questions.md)

## Context

The proposed scheduler interface exposes `acquire`, while the state adapter exposes only batch snapshot, atomic acquisition, and `release`. The surrounding requirements already depend on queue admission/cancellation, long-stream activity, idempotent completion, wake behavior, Redis epoch recovery, and shutdown cancellation. Leaving those transitions outside the public contract would force handlers, guards, or Redis adapters to invent lifecycle behavior independently.

Queue and active capacity are different states. Cancellation can race with grant, heartbeat can race with completion, and a stale owner can operate after lease expiry or Redis restart. Every transition therefore needs an ownership token, fencing rule, and idempotent result.

## Decision

The scheduler consists of a pure ranking core and one I/O coordinator per scheduling domain. The coordinator is the only application-facing owner of queue and lease lifecycle. The Redis state port exposes atomic transitions sufficient to implement this interface:

```rust
pub trait CredentialScheduler: Send + Sync {
    async fn acquire(
        &self,
        request: DispatchRequest,
        cancellation: CancellationSignal,
    ) -> Result<CredentialLease, DispatchError>;

    async fn heartbeat(
        &self,
        lease: &LeaseToken,
        activity: LeaseActivity,
    ) -> Result<HeartbeatAck, CoordinationError>;

    async fn complete(
        &self,
        lease: LeaseToken,
        completion_id: CompletionId,
        outcome: LeaseCompletion,
    ) -> Result<CompletionAck, CoordinationError>;

    async fn cancel_wait(
        &self,
        queue: QueueToken,
        reason: QueueCancelReason,
    ) -> Result<CancelAck, CoordinationError>;

    async fn cancel_active(
        &self,
        lease: LeaseToken,
        completion_id: CompletionId,
        reason: LeaseCancelReason,
    ) -> Result<CompletionAck, CoordinationError>;
}
```

`acquire` may internally expose a queue handle to the coordinator, but ordinary callers receive either a live lease or a terminal dispatch error. Cancellation remains linked for the complete wait so dropping a caller cannot leave an unowned queue entry.

The queue state machine is:

```text
New -> Queued -> Granted
              -> Rejected
              -> TimedOut
              -> Cancelled
```

Admission has a hard capacity, deadline, ownership token, epoch, and expiry. Grant atomically removes or consumes the queue token and creates one active lease. Grant and cancellation race in one atomic transition: if cancellation wins, no later grant can appear; if grant wins, cancellation is redirected to `cancel_active`. Queue renewal cannot recreate a missing, expired, cancelled, or granted token.

The lease state machine is:

```text
Active -> Completed
       -> Cancelled
       -> Expired
```

Each lease has an unguessable ownership token, fencing/epoch value, target, kind, acquired time, last activity, TTL, and absolute maximum lifetime policy. `heartbeat` operates only on the matching active token and cannot extend a lease past the accepted absolute bound. `complete` and `cancel_active` atomically release every global/per-target capacity counter, apply the classified coordination effect once, and publish/wake waiters. Repeating the same completion ID returns an idempotent acknowledgement without applying counters, cooldown, health, or wake effects twice. A stale or wrong token cannot mutate a newer lease.

TTL is the final crash-recovery mechanism, not the normal completion path. Long streams heartbeat only while the upstream attempt is active. Completion occurs when upstream production stops or cancellation wins; it does not wait for usage rollup or a slow downstream to consume already bounded output.

Redis failure is fail closed for new coordinated acquisition. Existing work can continue under local lease handles according to policy, while completion is retried by the scheduler-owned pending-release registry. After Redis epoch change or data loss, decision 014's release-generation manifest defines the expected replica set: acknowledged replicas re-register active leases, while any missing/partitioned replica is platform-fenced or reserves its full declared capacity until the absolute lease bound. New admission uses only proven residual capacity or remains not ready.

Local credential and external-pool coordinators implement the same lifecycle semantics but retain separate candidate, health, cooldown, automatic-disable, refresh, and credential-state domains.

## Ownership

- `SchedulerCore` owns pure eligibility, ranking, and reason output. It performs no queue, lease, Redis, refresh, usage, or HTTP work.
- `SchedulerCoordinator` owns queue wait, cancellation linking, lease handles, heartbeat scheduling, completion/cancel invocation, and pending-release retry.
- The Redis scheduler adapter owns atomic queue/lease/RPM/capacity/cooldown/sticky transitions and fencing validation.
- R7 terminal orchestration supplies one classified completion intent but does not release Redis state directly.
- The credential-outcome owner persists business outcomes. Scheduler completion stores only coordination state needed for future selection.
- R9 supervisor owns lifecycle, readiness, drain, and residue of heartbeat/release workers.

## Alternatives And Tradeoffs

### Keep one generic `release` operation

Rejected. It cannot distinguish normal completion, cancellation, stale authority, duplicate completion, queue cancellation or the coordination effect that must be applied atomically.

### Rely on guard drop and detached async cleanup

Rejected as the primary contract. Drop may be a last local safety net, but asynchronous network cleanup must be supervised, acknowledged, retried, and visible.

### Let TTL perform ordinary release

Rejected. It creates avoidable capacity loss and burst latency and hides lifecycle defects until load.

### Fail over to process-local scheduling when Redis is unavailable

Rejected by default. It can oversubscribe shared limits. An explicit single-instance degradation mode would require a separate accepted decision.

The proposed lifecycle adds tokens, scripts, state, and tests. It replaces implicit cleanup with bounded, observable coordination and permits constant-bounded Redis operations.

## Compatibility And Data Consequences

- Changed Redis semantics use a versioned key/script namespace. Old and new scheduler state coexist only for comparison and rollback; they do not both grant capacity for one request.
- Queue and lease keys have explicit TTL and bounded metadata. Old keys expire or are reconciled after the rollback window.
- PgSQL credential rows and secrets are not copied into Redis. Completion IDs and bounded scheduler effects contain no secret material.
- Public overload/no-capacity/timeout errors remain normalized. Timing or fairness may change only through an accepted policy and characterization evidence.
- Existing sticky, cooldown, RPM, concurrency, probation, warmup, exclusion, and external automatic-disable behavior remains characterized during migration.

## Target Integration And System Rollback

1. Characterize pure eligibility/ranking and the full legacy acquire/wait/release behavior.
2. Compare only pure decisions offline; comparison performs no queue admission, lease acquisition, heartbeat, completion, or refresh.
3. Exercise the complete target namespace against isolated fake traffic and real Redis fault injection.
4. Integrate one acquire-through-complete/cancel lifecycle into the target candidate. No release build contains dual scheduler selectors, and one request never acquires from both schedulers.
5. Whole-system rollback stops target admission, cancels/drains queued target requests, completes active leases where possible, records pending-release residue, and deploys the previous complete binary without deleting additive Redis keys.
6. TTL/reconciliation closes the target namespace; a later target attempt first runs the epoch/recovery barrier.

## Verification

- Race queue grant against cancellation, timeout, shutdown, renewal, and Redis disconnect; assert one terminal queue transition.
- Race heartbeat against complete, cancel, expiry, epoch change, and duplicate completion; stale tokens must not renew or release a newer lease.
- Repeat completion/cancel 1, 2, and 100 times; counters and scheduler effects apply once for the token/completion ID.
- Test queue capacity, wait deadline, wake behavior, fairness policy, and no stale grant after caller cancellation.
- Test success, error, timeout, malformed stream, client disconnect, shutdown, and process crash for both local and external leases.
- Restart or flush Redis while active local handles exist; inject responding, missing, partitioned, stale-generation and platform-fenced replicas, and keep admission blocked/reduced until the accepted recovery barrier accounts for their maximum capacity.
- Measure candidate scans, Redis commands, queue wait, acquire/complete lag, lock wait, p50/p95/p99, stale keys, RSS, FD, and task recovery at 10/100/1,000 candidates.
- Prove no broad in-process lock is held across Redis, PgSQL, network, wait, or sleep awaits.

## Implementation Parameters

- Decision 010 resolves multi-replica support (`Q-001`), resource limits (`Q-004`), and fairness/lease timing (`Q-009`).
- Redis key/script details may vary only inside the fixed atomicity, fencing, batch, timing, and recovery-barrier contract.
- Fault injection must quantify bounded capacity loss and recovery before final system cutover.
- R4 and scheduler-dependent R5-R7/R9 integration gates remain required evidence, not open design decisions.
