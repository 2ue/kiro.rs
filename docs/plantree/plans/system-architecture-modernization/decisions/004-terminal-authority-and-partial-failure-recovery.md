# 004: Terminal Authority And Partial-Failure Recovery

Role: Architecture decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding contract for one request-terminal decision, durable terminal acceptance, module-specific idempotency, and partial-failure recovery

Scope: Stream and non-stream terminal outcomes, usage, credential outcomes, scheduler lease completion, PgSQL/outbox persistence, Redis coordination, retries, process crash, and response-tail acknowledgement

Affected requirements/findings: `FUN-012`, `FUN-016`, `FUN-034` through `FUN-036`, `INV-002`, `INV-003`, `INV-007`, `INV-011`, `COR-003`, `REL-001`, `OPS-002`, and R2-R4/R7/R9 completion gates

Decision source: Architecture-contract reconciliation and final-plan convergence on 2026-07-12; durability policy is fixed by decision 010 and delivery mechanics are fixed by decision 009

Related: [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Runtime flows](../topics/architecture/runtime-control-and-data-flows.md), [State ownership](../topics/architecture/state-ownership-and-consistency.md), [Rewrite sequence](../topics/delivery/migration-sequence.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md), [Open questions](../open-questions.md)

## Context

The proposed rewrite currently assigns stream/non-stream final-usage reduction to R3 while R7 also introduces a terminal outcome reducer that coordinates lease release, credential persistence, and usage. It further uses language that can be read as exactly-once completion across Redis, PgSQL, and process-local queues.

Those resources do not share one transaction. A process can fail after one side effect and before another. The design therefore needs one authority for deciding the terminal facts, stable identities for every durable effect, explicit owner boundaries, and replayable recovery. It must not promise cross-system exactly-once delivery.

## Decision

R7 owns one request-local terminal reduction. The first valid terminal signal atomically changes the request lifecycle from active to finalizing and freezes an immutable `TerminalPlan` with a stable `terminal_id`. Duplicate, racing, or late terminal callbacks return the already selected ID and cannot alter the frozen facts.

The `TerminalPlan` is a neutral decision record. It contains the request identity, request/attempt summary reference, selected response outcome, downstream commitment, runtime version, and stable child IDs allocated for possible owner obligations. It explicitly contains no `UsageFinalizationInput`, `CredentialOutcomeEvent`, `LeaseCompletion`, owner-private request state, repository, Redis handle, queue, sink, writer, arbitrary JSON, or heterogeneous command collection.

After reduction, each owner projects its own typed obligation from its private request-local state, the neutral terminal facts, and its assigned stable ID. R3 retains the usage accumulator and usage policy; R4 retains the credential-attempt state and lease handle. The terminal coordinator invokes those typed owner ports but cannot inspect, reconstruct, or persist owner-private state through a generic payload.

The durable path accepts a fixed terminal append batch containing the minimal neutral envelope plus only the required owner-produced durable records. It commits the envelope and accepted durable dispatch obligations in one PgSQL transaction, deduplicated by stable IDs, or commits none of them. Owner-specific consumers then deliver or project those obligations at least once:

- the R3 usage owner creates the typed idempotent usage obligation, appends the usage event, and derives rollups/dashboard projections;
- the R4 credential owner creates and applies a typed idempotent durable credential outcome or statistic event;
- the R4 scheduler owner receives the stable completion ID and neutral facts, completes/cancels the active lease through its immediate coordination path, and owns any accepted secure pending-release record or reconciliation state;
- derived Redis usage/dashboard state is rebuilt or replayed from durable PgSQL authority.

The reducer does not own these effects. A terminal application service may orchestrate owner ports, but each owner defines its idempotency key, acknowledgement, retry, and reconciliation semantics.

There is no cross-system exactly-once claim. The target guarantees are:

1. at most one terminal decision and stable terminal identity per request;
2. idempotent durable acceptance under unique IDs;
3. at-least-once delivery from durable outbox obligations to idempotent owners;
4. observable eventual convergence or an explicit failed/abandoned residue;
5. bounded scheduler capacity recovery through immediate completion, supervised retry, and TTL as the final mechanism.

Scheduler completion uses an immediate idempotent fast path as soon as the upstream attempt stops producing or is cancelled. Failure remains in the scheduler owner's pending-release/reconciliation state. Usage rollup or dashboard work never delays lease release. A process crash can leave a Redis lease until the accepted recovery/TTL bound; that bounded state is reported rather than described as atomic with PgSQL.

The required acknowledgement policy is that a minimal terminal event is durably accepted before clean downstream terminal completion is reported. TTFB and intermediate stream events do not wait for rollups or Redis projection. Decision 010 fixes failure, backlog and terminal-latency acceptance.

## Ownership

- R2 owns the PgSQL terminal-envelope/outbox transaction, uniqueness, replay cursor, and adapter contract.
- R3 owns usage facts, pure projection, usage event schema, usage persistence acknowledgement, rollups, and Redis dashboard projection.
- R4 owns lease lifecycle, volatile scheduler effects, pending lease-release recovery, and durable credential-outcome consumption.
- R7 owns downstream response state, one terminal reduction, stable terminal/child ID creation, and orchestration through ports.
- R9 owns writer/outbox supervision, readiness impact, shutdown drain, residue reporting, and recovery commands.
- No handler, response adapter, scheduler, or usage projector may directly perform another owner's fallback write.

## Partial-Failure Semantics

- Failure before the PgSQL terminal transaction commits leaves no durable acceptance; retry uses the same IDs.
- Failure after commit but before an owner consumes an outbox row leaves that row pending and replayable.
- Duplicate delivery to usage or credential owners returns an idempotent duplicate acknowledgement without applying the effect again.
- Redis lease completion failure does not roll back the PgSQL terminal event; scheduler retry/reconciliation continues and TTL bounds abandoned capacity.
- Redis dashboard projection failure leaves durable usage/outbox authority intact and rebuildable.
- If the required terminal ingress is full or PgSQL is unavailable, response behavior, readiness, and shutdown follow the accepted durability policy; no implementation may silently substitute Redis-only authority.
- A downstream disconnect does not cancel already accepted terminal persistence or lease completion.

## Alternatives And Tradeoffs

### Sequentially call lease, credential, and usage writers without a durable journal

Rejected. A crash between calls loses knowledge of the remaining effects and cannot distinguish incomplete work from a completed duplicate.

### Let the usage writer own request terminality

Rejected. Usage is one projection of a terminal request. Making it the lifecycle owner couples response, scheduler, credential, and accounting behavior back into R3.

### Use distributed two-phase commit across PgSQL and Redis

Rejected. It adds availability and operational complexity while Redis lease TTL and idempotent reconciliation already provide a bounded coordination recovery model.

### Treat every terminal effect as best effort

Rejected for required usage/credential outcomes. It lowers tail latency but permits silent loss after a successful client-visible completion.

The proposed durable terminal envelope adds schema, writer, and terminal-tail cost. It removes ambiguous ownership and makes partial completion recoverable and measurable.

## Compatibility And Data Consequences

- Additive PgSQL terminal/outbox schema uses stable unique IDs and expand-contract migration. Previous binaries must ignore additive rows/columns safely during the rollback window.
- Existing usage records remain readable. Backfill or reconciliation maps them only when stable identity and semantics can be proven; it must not invent terminal outcomes.
- Old and new paths must not both persist the same side-effecting request unless a single idempotent adapter deliberately accepts both under the same IDs.
- Public response and SSE shapes remain compatible. The accepted acknowledgement policy may add bounded terminal-tail latency or turn a would-be clean completion into an explicit/truncated failure when durable acceptance is unavailable.
- Lease tokens and secrets are not placed in general observability or usage rows. Any durable release obligation that needs an opaque token requires an explicit secure schema and retention rule.

## Target Integration And System Rollback

1. Implement additive terminal/outbox infrastructure and replay tooling before target consumers integrate.
2. Integrate usage, credential, scheduler, response, and terminal modules through their typed target contracts and stable IDs.
3. Compare terminal reduction offline from the same sanitized facts; comparison writes nothing and completes no lease.
4. Exercise response and terminal contracts separately in tests, then together in the target-only candidate. The response module never persists terminal effects.
5. The release binary contains one response implementation and one terminal-lifecycle implementation; it contains no per-profile legacy selector.
6. Whole-system rollback stops target admission, preserves/reconciles terminal IDs, outbox rows, and scheduler releases, then selects the previous complete binary against additive schema.

New durable events remain replayable and must not be emitted again under different IDs after forward recovery.

## Verification

- Race success, upstream error, timeout, downstream disconnect, shutdown cancellation, malformed stream, and duplicate terminal callbacks; assert one frozen terminal ID.
- Inject failure before/after terminal insert, transaction commit, queue acknowledgement, usage consumption, credential consumption, Redis lease completion, and Redis projection.
- Kill and restart the process at every durable boundary; assert pending obligations resume with the same IDs.
- Replay each terminal/child event 1, 2, and 100 times; assert one durable effect per owner and convergent derived state.
- Saturate terminal/usage writer queues and verify bounded memory, stable overload behavior, readiness impact, and residue reporting.
- Verify lease completion latency is independent of usage rollup latency and reaches zero through fast path, retry, or the accepted TTL bound.
- Verify stream and non-stream paths produce equivalent usage from equivalent final facts without sharing terminal ownership.

## Implementation Parameters

- Decision 010 resolves terminal durability (`Q-002`) and prompt-cache authority (`Q-007`).
- Exact envelope columns, indexes, retention partitioning, and child-record encoding are module-internal choices only if they preserve stable IDs, atomic acceptance, idempotency, bounded retention, and replay.
- Lease release remains an immediate Redis effect with supervised retry and accepted TTL crash recovery; it is not represented as one PgSQL transaction.
- R2/R3/R4/R7/R9 focused and integrated evidence remains mandatory before final cutover.
