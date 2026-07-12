# 006: Producer-Aware Shutdown And Residue

Role: Architecture decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding shutdown admission, producer barriers, writer drain, dependency close order, residue classification, and process outcome contract

Scope: HTTP/Admin admission, in-flight requests and streams, jobs and periodic tasks, terminal/usage/audit/mutation writers, scheduler leases, outbox/projectors, PgSQL/Redis/HTTP clients, readiness, deadlines, and exit status

Affected requirements/findings: `FUN-012`, `FUN-016`, `FUN-017`, `INV-002`, `INV-003`, `INV-011`, `QA-REL-002`, `OPS-001` through `OPS-003`, and R9 lifecycle gates

Decision source: Architecture-contract reconciliation and final-plan convergence on 2026-07-12; deadlines/residue classes are fixed by decision 010 and delivery mechanics are fixed by decision 009

Related: [Runtime flows](../topics/architecture/runtime-control-and-data-flows.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Rewrite sequence](../topics/delivery/migration-sequence.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md), [Open questions](../open-questions.md)

## Context

The proposed shutdown flow correctly says writer inputs must remain open for in-flight requests, but its sequence closes and drains those inputs before refresh, cleanup, catalog, and invalidation workers stop. Some of those tasks can still produce terminal, audit, mutation, job, or coordination work. Closing a queue when request producers finish is therefore insufficient: every producer of that queue must be quiesced and joined first.

Dropping a sender, aborting a task, or observing an empty queue does not prove that accepted work finished. Shutdown needs explicit accepted/finished/abandoned counts, a producer barrier, dependency close order, and a policy that turns critical residue into a non-success process outcome.

## Decision

Every task that can enqueue lifecycle-significant or durable work registers as a named producer with the task supervisor before accepting work. Registration is rejected after the supervisor enters producer-closing state. Writer ingress is closed only by the supervisor after the producer barrier for that ingress reaches zero.

Shutdown proceeds through these ordered phases:

```text
Running
-> QuiescingAdmissions
-> DrainingAndCancellingProducers
-> ClosingWriterIngress
-> DrainingDurableConsumers
-> ReconcilingDerivedAndCoordinationWork
-> ClosingDependencies
-> ReportingAndExit
```

The phase contract is:

1. Mark application readiness false before stopping traffic admission. Stop new public HTTP requests, Admin mutations, job claims, periodic refresh/cleanup/catalog production, and creation of unregistered background tasks.
2. Allow bounded in-flight requests, streams, Admin commands, and already claimed jobs to finish. Keep terminal, usage, audit, mutation, heartbeat, and lease-completion paths available.
3. At the grace deadline, cancel remaining producers through their typed cancellation path. Each request/attempt reaches one terminal reduction; queued scheduler waits cancel; active leases complete/cancel through the scheduler owner.
4. Join all producers or record their explicit failed/abandoned state. Only when the relevant producer barrier reaches zero may the supervisor close terminal, usage, audit, and mutation ingress.
5. Drain durable writers until their accepted items are committed, idempotently rejected as duplicates, or classified as residue at the accepted deadline.
6. Drain or checkpoint durable outbox work. Rebuildable projections may leave a durable, measurable backlog only if the accepted policy permits it. Drain scheduler pending-release work and account for every active lease before stopping heartbeat/release workers.
7. Close reusable upstream clients and remaining non-producing workers, then Redis after coordination release/recovery state is settled, and PgSQL only after durable writers/outbox checkpoints no longer need it.
8. Aggregate one machine-readable `ShutdownReport` and choose the process exit status from accepted criticality rules.

Stopping a periodic task from accepting new work and stopping its consumer/drain capability are separate actions. A worker may be quiesced early as a producer and remain alive later to finish or reconcile work.

Each queue/output class is declared as one of:

- required durable: accepted work must commit or produce critical residue and non-zero exit;
- durable deferred/replayable: a committed outbox/backlog may remain only within an accepted age/count policy and recovery checkpoint;
- derived/rebuildable: loss of the projection is permitted only when durable authority remains and rebuild is explicit;
- best effort: drop is allowed only with bounded counts and an accepted reason.

The report includes phase/deadline, producer registered/joined/failed counts, queue accepted/finished/retried/rejected/dropped/abandoned counts, oldest backlog age, active/pending lease counts, task panics, dependency-close failures, and exact critical residue. Logging residue without changing the exit result is not completion.

## Ownership

- R9 `TaskSupervisor` owns shutdown phase transitions, producer registration/barriers, deadlines, dependency close order, report aggregation, and exit status.
- Public/Admin transports own admission shutdown and completion of already admitted handler futures.
- R7 owns terminal reduction for admitted requests and streams; it cannot close terminal/usage ingress.
- R3 owns usage writer/projector drain and reports its accepted/committed/residue counts.
- R4 owns queue cancellation, lease heartbeat/completion, pending-release drain, and lease residue.
- R2/R8 owners report durable mutation, outbox, audit, and job residue through named drain contracts.
- Only bootstrap closes PgSQL, Redis, and shared clients after their owners report the required drain/checkpoint state.

## Alternatives And Tradeoffs

### Close channels immediately after HTTP admission stops

Rejected. In-flight requests and non-request workers can still produce required terminal and audit work.

### Drop all senders and treat receiver completion as drain

Rejected. Detached or cloned senders and aborted producers make channel closure an unreliable proof of accepted-versus-finished work.

### Abort all tasks at one global deadline

Rejected as the normal path. It can leak capacity until TTL, lose required writes, and hide which owner failed to drain. A final process-kill deadline may remain as an outer guard after residue is captured.

### Require every derived projection to finish before exit

Not selected as a universal rule. It can make shutdown depend on rebuildable Redis/dashboard work. Durable authority and a checkpointed backlog may be sufficient when explicitly accepted.

The proposed phases add supervisor bookkeeping and can extend shutdown. They make data loss, capacity residue, and dependency ordering visible and testable.

## Compatibility And Data Consequences

- Public APIs do not change, but readiness becomes false before listener/process exit and new admissions receive the existing normalized unavailable behavior.
- Exit status may become non-zero in cases that currently only log usage/storage residue. Deployment policy and runbooks must treat that as a correctness signal rather than an automatic clean restart.
- Additive shutdown metrics and a bounded machine-readable report contain no request bodies, credentials, arbitrary IDs as metric labels, or secrets.
- Durable outbox/job rows survive process restart; shutdown does not delete or downgrade them to make the report look clean.
- A previous binary remains compatible with additive schema, but it may not provide the new producer/residue guarantees during rollback.

## Target Integration And System Rollback

1. Inventory every producer, queue, worker, task handle, dependency, and shutdown callback; classify output criticality before implementation.
2. Implement all drain/report interfaces and producer registration in the target-only supervisor.
3. Verify accepted/finished counts and reject detached or unregistered producers.
4. Exercise the complete ordered lifecycle under isolated SIGTERM/restart and writer-fault matrices; do not activate writer families separately in production.
5. Final cutover uses this complete lifecycle. Whole-system rollback is allowed only after target producers are joined or accounted, durable work is checkpointed, and pending coordination work is reconciled.
6. Rollback never discards residue or forces a zero exit to make cleanup appear successful.

## Verification

- Send SIGTERM before admission, while queued, while connecting, during a long stream, after upstream terminal but before final usage, and while durable writers are blocked.
- Prove a producer can enqueue its final accepted item after admission closes but never after its producer barrier is joined and writer ingress closes.
- Race grace completion with forced cancellation; assert one request terminal decision and one queue/lease terminal transition.
- Block each writer independently and verify bounded backlog, readiness state, exact residue report, deadline behavior, and required non-zero exit.
- Crash/restart with committed outbox backlog and prove replay; derived Redis projection may lag but durable authority remains intact.
- Verify Redis remains available through lease reconciliation and PgSQL remains available through the last durable commit/checkpoint.
- Assert no send-on-closed failures, detached producer tasks, silent task panics, orphaned queue tickets, or unexplained lease count.
- Verify clean ordinary shutdown exits zero and recovers RSS, FD, tasks, queues, files, and ports within policy.

## Implementation Parameters

- Decision 010 resolves durability/residue (`Q-002`), multi-replica behavior (`Q-001`), and shutdown deadlines (`Q-010`).
- The implementation inventory must still classify every concrete producer and output; unknown criticality fails closed.
- Per-module drain acknowledgements and the aggregate machine-readable report are required R9/full-system evidence.
- No implementation may reinterpret the fixed outer deadline or critical residue classes as best effort.
