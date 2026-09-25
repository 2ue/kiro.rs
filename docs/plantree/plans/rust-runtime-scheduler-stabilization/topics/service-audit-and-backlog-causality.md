# Service Audit And Backlog Causality

Role: Source-verified assessment, implementation plan, and acceptance contract for the v0.0.170 service audit and long-request backlog

Status: `In Progress`

Authority: Owns the current remediation scope and validation contract for the linked audit inputs; source code and tests at the implementation revision remain authoritative for current behavior

Read when: Continuing the service audit fixes, backlog diagnosis, or their verification in a later session

Related:

- [Service problem audit v0.0.170](../../../../analysis/service-problem-audit-v0.0.170-20260924.md)
- [Production concurrency backlog causality input](../../../../analysis/prod-concurrency-backlog-causality-20260925.md)
- [Project test instance policy](../../../../testing/project-test-instance.md)
- [Load and chaos matrix](../../../../../.codex/skills/kiro-load-chaos-validation/references/load-chaos-matrix.md)
- [Sustained scheduling validation](sustained-scheduling-validation.md)
- [Scheduler target state machine and test contract](scheduler-target-state-machine-and-test-contract.md)

## Scope

Assess every numbered item in the service audit except item 7, which is explicitly excluded by the user. Assess the concurrency/backlog analysis, correct claims that are not supported by current source, implement the low-risk fixes below, and keep analysis, tests, and outcomes durable in this plan tree.

The source audit identifies a plausible but not yet production-proven performance issue. The
concurrency input is dated `2026-09-25`; it is user-provided analysis input, not proof that a
production observation or test occurred on that date.

## Hard Invariants

- Do not add or lower any request, dispatch, upstream, stream-idle, queue, or total-duration limit in this work.
- Do not cancel a request that is otherwise accepted under current configuration merely because it has streamed for a long time.
- Do not move request API-key RPM reservation after concurrency admission. The current order is an ingress rate limiter that protects the admission queue; it is not an upstream-success counter.
- Do not change account concurrency, queue capacity, FIFO order, queue timeout, dispatch timeout, retry count, retry boundary, or client-visible error behavior.
- A stream that continues to produce valid upstream data remains governed by the existing stream-idle policy; this plan adds no total wall-clock deadline.
- Do not load production or another project's service. Fake-upstream and load validation follow the single project-owned test-instance policy.
- Preserve the provided untracked audit documents, `.DS_Store` files, `.kilo` data/worktree, and untracked `docs/refactor-plan/`; do not delete, stage, or rewrite them as part of this work.

## Assessment

| Audit item | Source-verified assessment | Disposition |
| --- | --- | --- |
| 1. Sync-over-async bridges | The broad claim that 63 bridges cap work at two concurrent operations is not established: the two-thread runtimes multiplex async I/O and `block_in_place` does not by itself prove runtime starvation. A real request-path bridge does exist when the local dispatcher synchronously acquires a Redis queue lease under contention. | Fix that queue-lease path to await Redis directly. Defer wider bridge removal until measured evidence identifies another hot path. |
| 2. Cluster Admin Key rotation | The Redis runtime-config listener refreshes request API keys and admission settings but has no Admin key handle. Other instances can therefore retain the old Admin key after a persisted rotation. | Fix by sharing an Admin key store with the listener and refreshing it on pub/sub reload and periodic reload. |
| 3. Admin lock poison / empty key | A write-lock panic is not presently demonstrated and the setter's critical section is minimal, but returning an empty configured key and accepting an empty presented key would be fail-open if poison ever occurred. | Use the existing `parking_lot` convention and make empty configured or presented keys unconditionally unauthorized. |
| 4. Paired sync/async Redis implementations | This is maintenance risk attached to the legacy bridge; no independent behavior defect is proven. | Do not duplicate a broad cleanup here. Removing the queue-path sync call reduces one live instance; inventory the remaining pairs only if another fix needs them. |
| 5. Large modules / provider coupling | Real maintainability concern, not a runtime incident. An untracked `docs/refactor-plan/` exists in the supplied worktree and must be preserved; its untracked state is not authorization to stage or rewrite it. | Defer module extraction to the existing architecture/refactor work after its authority and current-source baseline are reviewed. |
| 6. Clippy baseline | The high allowance is quality debt, not evidence of a current runtime defect. The `result_large_err` subset may merit a separately measured cleanup; mechanical lint cleanup is broad and should have its own diff and baseline update. | Record the current lint result with final gates; do not mass-edit unrelated modules or loosen/tighten the baseline opportunistically. |
| 8. Repository hygiene | `.DS_Store` should be ignored prospectively. The `.kilo` worktree/data, tracked `feature/` evidence, and user-provided untracked planning docs have ownership or provenance uncertainty. | Add a `.DS_Store` ignore rule only. Preserve all other listed content; a worktree cleanup needs its own inventory and owner decision. |

Concurrency findings:

- Ten credentials with six unit-weight slots each imply up to 60 local upstream leases only when those settings and unit weights are current. This is not the same quantity as total inbound HTTP requests or requests waiting in either admission queue.
- Long streams can hold scheduler leases while data continues to arrive. There is no total wall-clock stream deadline, intentionally; an active long request must not be cut off by this optimization.
- The normal configured credential dispatch wait is five seconds, but `credential_dispatch_max_wait_secs = 0` means no elapsed-time cutoff. The dispatch queue is still count-bounded (default 30); zero does not mean an unlimited number of queued requests. The production value was not supplied and must not be guessed.
- Layer A request API-key admission and Layer B credential dispatch are distinct queues. Their existing wait and capacity rules remain unchanged.
- The existing `upstream_header_ms` is measured from request usage-context creation, before credential selection, so it is not a pure upstream-header duration. Usage currently lacks a dedicated credential-dispatch duration.
- RPM reservation occurs before concurrency waiting by design. It limits arrivals and protects the queue; rejected/cancelled arrivals may consume a reservation. Do not reinterpret this field as successful upstream throughput or change its position without a separate product decision.
- The supplied arithmetic (`60 slots / 300 seconds = 12 completions per minute`) is correct as a steady-state illustration, not a production measurement. The observed request shape, account settings, active slot count, actual provider latency, and retry amplification still require timestamped evidence.
- Static inspection makes the reported pattern plausible but cannot prove its cause in production. The expected causal order is long provider/acquisition occupancy -> fewer free slots -> queueing/rejection; backlog can add delay and retry contention, but current production queue wait is unobservable.

## Implementation Plan

1. Share an `AdminApiKeyStore` between `AdminState` and the Redis runtime-config listener. Replace the key after every successful runtime-config reload, whether triggered by pub/sub or the 60-second fallback. Keep startup route mounting behavior unchanged.
2. Make Admin authentication fail closed when the presented key is empty or the configured key is empty. Use a non-poisoning lock for the shared key.
3. Convert Redis dispatch-queue lease acquisition to async/await in the credential-acquisition path. Retain the same local queue reservation, Redis lease, release reconciliation, breaker, timeout, and error semantics.
4. Add optional usage latency fields with explicit definitions:
   - `credentialDispatchElapsedMs`: summed wall time spent inside credential acquisition calls for this inference, including scheduler lookup, queue/capacity waits, coordination, and token acquisition/refresh; excludes body preparation and provider HTTP sends.
   - `upstreamHeaderWaitMs`: summed time awaiting upstream response headers for actual send attempts, including failed transport/header waits and permitted retries; excludes credential acquisition and body preparation.
   - Preserve existing `upstreamHeaderMs` semantics for compatibility; do not silently rename or reinterpret historical values.
5. Leave RPM reservation ordering, request admission, queue sizes/timeouts, dispatch maximum-wait semantics, stream-idle timeout, retry policy, and all successful response handling unchanged.
6. Add `.DS_Store` to `.gitignore`; do not delete existing files.

## Acceptance Criteria

- With two runtime instances sharing PostgreSQL/Redis, a valid persisted Admin key rotation reaches the second instance after the Redis event or no later than its existing periodic reload; old-key requests fail and new-key requests authenticate.
- Empty configured keys and empty presented keys never authenticate, including after key replacement.
- The Redis dispatch queue lease is acquired with `.await` on the request acquisition path; no synchronous `block_on` is used for that operation.
- Queue, breaker, cancellation, and lease-release tests prove no leaked queue count/Redis lease on success, full queue, Redis failure, timeout, or cancellation.
- The new latency fields serialize in camelCase, remain optional for historical records, and report delayed credential acquisition separately from delayed upstream headers.
- A fake upstream test holds requests beyond a short dispatch delay, then releases capacity; queued normal requests complete under existing policy and stream bodies are not cut off by a new elapsed-time cap.
- No production traffic is generated. Any dynamic load uses the repository's owned `127.0.0.1:19023` instance only after ownership/config/storage preflight, plus fake upstreams.

## Verification Sequence

1. Focused Admin key-store/authentication tests, key-reload helper tests, queue-lease manager tests, and latency trace serialization/propagation tests through `feature/tests/run-cargo-scoped.sh`.
2. Fake-upstream concurrent backlog matrix: one occupied slot followed by queued normal requests; account/queue saturation; release and recovery; cancellation; slow but active long stream; mixed fast/slow upstream headers. Record offered, admitted, queued, selected, sent, completed, rejected, and cancelled counts.
3. Compare the saturation scenario with and without Redis lease coordination where existing test fixtures allow it. Assert all requests accepted by current queue policy either complete or receive only the same pre-existing configured timeout/capacity outcome; no newly introduced rejection is allowed.
4. Run `cargo fmt --all -- --check`, focused and full default/no-default all-target Rust tests, release build, Clippy baseline check, UI/API contract checks if shared types change, `git diff --check`, and build-artifact inventory.
5. If the designated test instance is available and passes ownership preflight, run fake-upstream L1 and bounded L3 recovery steps. Do not use real upstream pressure for this task.
6. Record exact commands, counts, durations, candidate identity, resource cleanup, and remaining gaps in the evidence index and a dated evidence report.

## Open Questions

- Production `credential_dispatch_max_wait_secs`, queue capacities, weighted-capacity flag, active accounts, and active lease snapshot were not included. Confirm them from a redacted production config/snapshot before attributing a specific incident to unbounded dispatch wait.
- No production request timeline with receive, credential-selected, upstream-send, header, first-chunk, terminal, and cancellation timestamps was included. Local fake-upstream evidence can validate observability and behavior, not production root cause.
- Admin API routes are mounted only when startup configuration has a non-empty key. Enabling a previously disabled Admin API without restart is outside the key-rotation fix.
- Do not select a total stream duration or move RPM accounting without an explicit product policy defining which otherwise-valid requests may be terminated or how ingress shaping should behave.

## Landing Record

As of 2026-09-25, the scoped implementation is complete and locally validated:

- Admin key sharing/reload and fail-closed empty-key behavior are implemented.
- The Redis dispatch queue lease request path awaits the scheduler Redis breaker instead of
  synchronously blocking a runtime worker.
- `credentialDispatchElapsedMs` and `upstreamHeaderWaitMs` are optional, camelCase usage
  fields; historical `upstreamHeaderMs` remains present with its prior meaning.
- Default and no-default full Rust tests passed `2170/0/6 ignored` for the main binary and
  `31/31` for `kiro_loadtest`; release build, Clippy (`764 <= 849`), format, diff and
  artifact inventory gates passed.
- Frozen L3 fake-upstream validation passed `9/9`.
- Three independent fresh-database low-capacity HTTP runs (`run8`, `run9`, `run10`) each
  delivered 47 valid long-stream chunks without early cutoff, completed queued normal
  requests after natural capacity release, cancelled one queued request without an
  upstream hit, completed a recovery request, and cleaned Redis to zero owned keys.

This closes the implementation acceptance criteria without changing request/queue/stream/RPM
limits. Production attribution remains open: obtain redacted
`credential_dispatch_max_wait_secs`, queue/account/lease snapshots and a receive-to-terminal
request timeline before changing capacity or timeout policy. The full evidence report is
[service-audit-backlog-remediation-20260924.md](../../../../../feature/evidence/service-audit-backlog-remediation-20260924.md).
