# Kiro credential scheduler dispatch redesign

This folder records the production scheduler redesign for `kiro.rs` credential dispatch.
It covers the original problem analysis, the final strategy, and the implementation that
has been applied in this repository.

## Documents

- [Analysis](./analysis.md): why the old scheduler could still hit unhealthy credentials
  under concurrency, what was learned from `sub2api`, and which production invariants must hold.
- [Final Strategy](./final-strategy.md): the dispatch model, hard filters, health scoring,
  cooldown policy, concurrency controls, observability, and deliberate non-goals.
- [Implementation Record](./implementation-plan.md): what was changed in backend, Redis state,
  Admin APIs, the old UI, the current `/ui`, and tests.

## Implemented scope

The redesign has been implemented as one combined change set instead of only a first phase.

Implemented backend behavior:

1. Credentials that receive scheduler-relevant transient failures are removed from dispatch
   immediately, including `429`, retryable `402`, `408`, `5xx`, network send failures, retryable
   protocol mismatches, token refresh protection, upstream stream read errors, stream error events,
   and upstream stream idle timeouts.
2. Permanent account failures still disable credentials instead of using short cooldowns. This
   includes clear quota/monthly exhaustion, risk-control, suspended, and locked states.
3. Cooldown is shared through Redis, and cooldown writes are monotonic:
   a short later cooldown cannot shorten a longer active cooldown.
4. Scheduler health is shared through Redis: transient streak, recent error-rate EWMA, latency
   EWMA, last error kind/reason/time, probation state, and recent selection windows.
5. Success updates health and latency, but it no longer clears an active cooldown created by a
   concurrent failure.
6. The existing `priority` and `balanced` modes remain supported, and a new `health_balanced`
   mode selects among healthy candidates using score, recent selection pressure, and top-K weighted
   sampling.
7. Redis dispatch leases now support both per-credential capacity and optional global capacity in
   one atomic acquisition path.
8. A bounded global dispatch queue can reject overload early instead of allowing unbounded waiters.
9. Total scheduler selection count is persisted in Postgres, while 10s/60s/5m recent selection
   counts are kept in Redis for scheduling pressure and UI diagnosis.
10. Warmup selection is target-share based: each warming credential receives a small target share,
    capped by a total warmup traffic ceiling, so batch imports do not wait indefinitely for a tiny
    fixed probability.
11. Admin API snapshots expose per-credential health state and aggregate global capacity state.
12. Both Admin UIs expose the new scheduler mode, runtime controls, and health/capacity state.

## Recommended runtime posture

For high-concurrency production traffic, use:

- `loadBalancingMode = "health_balanced"`
- a non-zero `credentialMaxConcurrentRequests`
- a non-zero `dispatchGlobalMaxConcurrentRequests` when total process/upstream capacity is known
- a non-zero `dispatchMaxQueuedRequests` when callers should receive fast overload feedback
- Redis enabled; it is a required runtime dependency for shared scheduler state

The default values preserve backward compatibility: global capacity and queue limits are unlimited
when set to `0`, and existing `priority`/`balanced` behavior remains available.

## Deliberate limitations

The implementation intentionally keeps several signals out of the hot dispatch path:

- Official quota credits and local estimated dollar cost are persisted and displayed elsewhere, but
  they are not used for hot-path scoring because querying database-backed quota/cost data on every
  dispatch would add latency and contention. They can be added later through a cached snapshot.
- Health state is credential-level. It is not yet scoped by model, proxy endpoint, region, or
  upstream incident bucket.
- Downstream client cancellation is treated as a soft failure and does not punish a credential
  unless the upstream/proxy path has actually produced an error.
- `health_balanced` uses total latency EWMA, not first-token latency/TTFT. TTFT can be added when
  stream instrumentation records it consistently.
