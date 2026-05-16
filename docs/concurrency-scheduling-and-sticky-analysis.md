# High Concurrency Scheduling and Sticky Session Analysis

Date: 2026-05-16

## Purpose

This document records the background, current implementation, observed risks, and recommended strategy for credential scheduling in this project. It is intended to be readable without the chat history.

The immediate question was:

- If the service has 10 credentials and receives 100 requests, how will requests be scheduled?
- What is the difference between `priority` mode and `balanced` mode?
- What does sticky scheduling do?
- Does the current system support sticky scheduling?
- Are there defects or risks under high concurrency?

The broader context is the Claude Code CLI compatibility and prompt-cache work:

- The service exposes Anthropic/Claude-compatible APIs and converts calls to Kiro upstream requests.
- Kiro upstream accounts are real model accounts, and prompt cache behavior depends heavily on stable conversation and account continuity.
- Earlier tests showed many cache creations but few cache reads. One important cause is unstable routing: if the same Claude Code session is sent through different upstream credentials or unstable `conversationId` values, upstream/local cache continuity is weakened.
- Therefore scheduling must balance two goals that naturally conflict:
  - maximize cache/session continuity with sticky routing;
  - avoid overloading a single credential under high concurrency.

## Current Implementation Summary

Main files:

- `src/kiro/token_manager.rs`
- `src/kiro/provider.rs`
- `src/model/config.rs`
- `src/anthropic/converter.rs`

Relevant configuration:

- `Config.load_balancing_mode` supports `"priority"` and `"balanced"`.
- Default mode is `"priority"`.

Relevant constants in `src/kiro/token_manager.rs`:

- `MAX_FAILURES_PER_CREDENTIAL = 3`
- `SESSION_BINDING_TTL_SECS = 6 * 60 * 60`
- `MAX_SESSION_BINDINGS = 10_000`
- `MAX_SESSION_SOFT_FAILURES = 2`

Relevant retry constants in `src/kiro/provider.rs`:

- `MAX_RETRIES_PER_CREDENTIAL = 3`
- total request retry cap is `min(total_credentials * 3, 9)`

## Request Scheduling Flow

The scheduling entry is:

- `MultiTokenManager::acquire_context_for_session(model, session_id, excluded_ids)`

The provider extracts routing information before calling it:

- model from `conversationState.currentMessage.userInputMessage.modelId`
- session id from `conversationState.conversationId`

For Anthropic/Claude Code requests, the converter tries to preserve a stable conversation id:

- It first extracts a session UUID from `metadata.user_id`.
- Supported examples include Claude Code-style `session_<uuid>` strings and JSON metadata containing `session_id`.
- If no stable session id is found, a new UUID is generated. That makes sticky routing ineffective across requests.

Scheduling order:

1. If `session_id` exists and has a usable binding, return the bound credential.
2. If no sticky hit exists, select a credential according to the configured mode.
3. Ensure the selected credential has a valid token.
4. On successful context acquisition, bind the session to that credential if there is no incompatible existing binding.
5. The provider sends the upstream request.
6. On success, credential `success_count` is incremented.
7. For streaming calls, success is reported only after the SSE/event-stream body completes.

## Priority Mode Behavior

`priority` mode chooses the available credential with the smallest `priority` number. Lower number means higher priority.

For new sessions:

- The manager prefers `current_id` if it is usable.
- `current_id` is normally the highest-priority available credential.
- Successful calls do not rotate `current_id`.

For 10 credentials and 100 requests:

- If these are 100 new sessions and the top-priority credential is healthy, most or all requests will choose that credential.
- Distribution can look like `100/0/0/...`, not `10/10/10/...`.
- Other credentials are mainly used after the preferred credential becomes unavailable, is excluded during a retry, or is disabled by hard failures/quota handling.

Interpretation:

- This is closer to primary/standby routing than load balancing.
- It is useful when one credential is preferred for quality, subscription tier, region, or operational control.
- It is risky when the user expects aggregate throughput from all credentials.

## Balanced Mode Behavior

`balanced` mode chooses the usable credential with the smallest tuple:

```text
(success_count, priority)
```

That means:

- fewer historical successes wins;
- priority is only the tie-breaker.

For sequential traffic:

- New sessions should gradually spread across credentials as success counts diverge.
- After enough completed requests, distribution tends to become more even.

For high-concurrency burst traffic:

- The current implementation has no in-flight/request-reservation counter.
- `success_count` increments only after a request succeeds.
- Long streaming requests increment success only when the stream completes.
- Therefore many simultaneous requests can observe the same old `success_count` and select the same credential.

For 10 credentials and 100 simultaneous new sessions:

- The implementation does not guarantee a 10/10/10 distribution.
- It may stampede onto one or a few credentials, especially when counts are tied.
- Later traffic may rebalance after successes are recorded, but the initial burst can still overload a credential.

Interpretation:

- Current `balanced` is least-historical-success routing.
- It is not real-time load balancing.
- It is not concurrency-aware.

## Sticky Scheduling

Sticky scheduling means:

```text
same conversationId -> same upstream credential
```

Current support:

- Supported through `session_bindings: HashMap<String, SessionBinding>`.
- Binding contains credential id, last-used time, and soft-failure count.
- Binding TTL is 6 hours.
- Binding table is capped at 10,000 sessions.
- Bindings are in-memory only and are lost on service restart.

Sticky hit behavior:

- If the session has a bound credential and that credential is usable for the requested model, it is reused.
- This happens before `priority` or `balanced` selection.
- Therefore sticky takes precedence over load balancing.

Sticky fallback behavior:

- If a bound credential has repeated soft failures in the current session, the provider may add that credential to `excluded_ids` for the current request.
- The current request can temporarily fallback to another credential.
- If the original binding remains usable, the next request can return to the original credential.

Why sticky matters:

- It preserves Kiro upstream conversation/account continuity.
- It improves prompt-cache continuity because cache scope is effectively tied to stable session/account/model behavior.
- It avoids the "many cache creations, few cache reads" pattern caused by repeatedly changing account or conversation identity.
- It keeps tool-use history and continuation behavior more consistent for Claude Code-style long sessions.

Tradeoff:

- Sticky improves cache and session correctness.
- Sticky can overload a single credential when one conversation has high request concurrency or very long-running workflows.

## High-Concurrency Scenarios

### Scenario A: 100 requests from one existing Claude Code session

Expected behavior:

- The first request binds the session to one credential.
- Subsequent requests mostly reuse the same credential.
- `priority` and `balanced` mode do not matter much after sticky binding.

Result:

- Good cache continuity.
- Bad distribution.
- One credential can become hot while the other 9 are idle.

Risk:

- A single real Claude Code task with many tool calls, think turns, retries, or parallel internal calls can overload the bound credential.

### Scenario B: 100 requests from 100 different stable sessions in priority mode

Expected behavior:

- Most requests use the highest-priority available credential.

Result:

- Strong hot-spotting.
- Other credentials are underused.

Risk:

- More 429/high-traffic responses on the preferred credential.
- Uneven quota burn.
- Aggregate capacity of 10 credentials is not effectively used.

### Scenario C: 100 requests from 100 different stable sessions in balanced mode

Expected behavior under sequential load:

- Requests gradually spread by `success_count`.

Expected behavior under simultaneous burst:

- Distribution is not guaranteed.
- Requests may stampede because selection does not reserve capacity before upstream completion.

Result:

- Better than priority for normal sequential traffic.
- Still risky for burst concurrency.

### Scenario D: Requests without stable conversation id

Expected behavior:

- Anthropic converter generates a new UUID.
- Every request looks like a new session.
- Sticky does not persist across turns.

Result:

- Lower cache-read probability.
- More cache creation.
- In balanced mode, each request enters global balancing instead of session continuity.

Risk:

- This can directly reproduce the user's observed symptom: cache creation is common, cache read is rare.

## Failure and Retry Behavior

Provider retry behavior:

- 400: treated as request problem; no credential switching.
- 401/403: treated as credential/auth/permission problem; may force refresh once, then count hard failure.
- 402 monthly quota: disables the credential and unbinds sessions for it.
- 408/429/5xx/network errors: treated as transient soft failures; do not hard-disable the credential.

Soft failure behavior:

- Session soft-failure count is tracked only for the bound session/credential.
- After `MAX_SESSION_SOFT_FAILURES = 2`, the credential can be excluded for the current request.
- This fallback is temporary unless the credential is hard-disabled or explicitly unbound.

Important implication:

- Avoiding hard-disable on 429/5xx is correct because upstream high-load errors can be transient.
- But under hot-spotting, the same credential may continue receiving traffic until enough per-session soft fallback happens.

## Current Risks and Defects

### 1. Balanced mode is not in-flight aware

There is no per-credential active request counter.

Impact:

- Burst traffic can select the same credential repeatedly before any success is recorded.
- Long streaming calls make this worse because success is delayed until stream completion.

### 2. Priority mode is not load balancing

Priority mode intentionally concentrates traffic on the highest-priority credential.

Impact:

- It is suitable for primary/standby.
- It is not suitable for maximizing total throughput across 10 credentials.

### 3. Sticky can overload one credential

Sticky is session-correct but can concentrate load.

Impact:

- A single heavy Claude Code session can monopolize one credential.
- Other credentials cannot help unless fallback or future strategy allows controlled session migration.

### 4. No per-credential concurrency limit

The system does not appear to enforce:

- max active requests per credential;
- max active streams per credential;
- queue/backpressure when all credentials are saturated.

Impact:

- Upstream 429/high-load responses can increase under burst.
- Local service can amplify retries.

### 5. Success counts are lifetime-style counters

`success_count` is loaded from persisted stats and increases over time.

Impact:

- It is not a recent-load metric.
- Old history may skew balancing.
- New credentials with zero successes can receive disproportionate traffic until they catch up.

### 6. Sticky bindings are memory-only

Bindings are lost on restart.

Impact:

- Cache continuity can drop after restart.
- Existing Claude Code sessions may be rebound to different credentials.

### 7. Global token refresh lock

Token refresh is guarded by a single async mutex.

Impact:

- If many credentials expire at the same time, refresh operations serialize.
- This can add latency under high concurrency.

### 8. Retry cap may not cover every credential

Total retries are capped at 9.

Impact:

- With 10 credentials, a single request is not guaranteed to try every credential.
- This is reasonable for latency control but should be understood.

### 9. Model support filtering affects distribution

Some credentials may not support all models, especially Opus-class models.

Impact:

- Effective credential pool can be much smaller than the configured credential count.
- A "10 credential" pool may behave like a "2 credential" pool for certain models.

## Recommended Strategy

The recommended direction is not to remove sticky scheduling. Sticky is important for Claude Code compatibility and cache reads.

Instead, scheduling should become sticky-first and load-aware:

```text
1. Preserve sticky routing when the bound credential is healthy and below load limits.
2. For new sessions, choose by real-time load, not only historical success_count.
3. For overloaded sticky sessions, use controlled fallback or queueing.
4. Add observability so distribution, cache behavior, and fallback reasons are measurable.
```

### Strategy 1: Add in-flight accounting

Track per credential:

- active non-stream requests;
- active streaming requests;
- active total requests;
- recent success count;
- recent failure/429 count;
- last 429/high-load timestamp.

New-session selection should consider:

```text
(active_requests, recent_429_penalty, recent_latency, success_count_window, priority)
```

This makes `balanced` real-time aware.

### Strategy 2: Add per-credential concurrency caps

Configurable examples:

- `maxConcurrentRequestsPerCredential`
- `maxConcurrentStreamsPerCredential`
- `maxGlobalConcurrentRequests`

When a credential is saturated:

- new sessions should choose another credential;
- sticky sessions can either wait briefly, fallback if allowed, or fail fast with a clear local overload error.

### Strategy 3: Keep sticky, but define overload behavior

Recommended sticky policy:

- Default: keep sticky binding.
- If bound credential has active load above threshold or recent repeated 429/5xx, temporarily fallback for this request.
- Do not permanently rebind on one transient failure.
- Rebind only when:
  - credential is disabled;
  - quota is exhausted;
  - model is unsupported;
  - repeated soft failures cross a larger threshold;
  - explicit admin reset occurs.

This preserves cache continuity while avoiding pathological hot-spotting.

### Strategy 4: Use decaying/windowed metrics

Avoid using lifetime `success_count` as the main balancing signal.

Prefer:

- active in-flight count;
- moving-window success/failure counts;
- moving-window p95 latency;
- short cooldown after 429/high-load.

Lifetime `success_count` can remain a tie-breaker or dashboard metric.

### Strategy 5: Improve observability

Each request should log or expose:

- selected credential id;
- model;
- conversation id hash or short safe id;
- scheduling mode;
- sticky hit/miss;
- fallback reason;
- active count before/after selection;
- retry attempt;
- upstream status;
- stream completed or dropped;
- cache create/read simulated values.

Admin/status API should expose per credential:

- active requests;
- active streams;
- sticky sessions count;
- recent 429/5xx;
- recent average latency;
- success/failure window;
- quota-disabled/manual-disabled state.

### Strategy 6: Validate stable Claude Code session id

Add or keep tests verifying:

- Claude Code metadata produces a stable `conversationId`.
- Same Claude Code session maps to same upstream credential.
- Missing metadata intentionally creates new sessions and lowers cache continuity.

## Suggested Implementation Plan

### Phase 1: Measurement only

Goal:

- Add metrics/logs without changing scheduling behavior.

Deliverables:

- per-credential active counters;
- scheduling decision logs;
- admin snapshot fields;
- tests proving counters increment/decrement on success, error, and stream drop.

### Phase 2: Real-time balanced mode

Goal:

- Change balanced selection from least historical success to least active load plus health penalty.

Candidate scoring:

```text
score = active_requests * A
      + active_streams * B
      + recent_429_penalty * C
      + recent_error_penalty * D
      + priority * E
      + success_count_window * F
```

Keep this simple at first. Avoid too many user-facing knobs.

### Phase 3: Sticky overload policy

Goal:

- Preserve sticky while preventing one session from endlessly overloading one credential.

Policy:

- If sticky credential is healthy and below limit, use it.
- If saturated, wait briefly or fallback according to config.
- Temporary fallback should not immediately overwrite the original binding.
- Permanent rebind should require hard failure, quota exhaustion, unsupported model, or repeated sustained soft failures.

### Phase 4: Stress tests

Required tests:

- 10 credentials / 100 concurrent new sessions in balanced mode.
- 10 credentials / 100 concurrent requests in one sticky session.
- long streaming requests do not leak active counters after completion/drop.
- 429 cooldown reduces immediate reselection.
- priority mode remains deterministic primary/standby.
- missing conversation id causes new-session behavior.
- model filtering reduces pool correctly.

## Expected Target Behavior After Optimization

For 10 credentials and 100 different sessions:

- `priority`: still intentionally favors top-priority credential unless configured otherwise.
- `balanced`: should distribute according to active load and health, not perfectly but reasonably.

For 100 requests from the same session:

- default should remain sticky to protect cache and conversation continuity.
- if one credential is overloaded, controlled fallback or backpressure should occur.

For Claude Code cache behavior:

- stable session id plus sticky credential should increase cache-read opportunities.
- cache-read ratio should not be expected to be a fixed exact number on every request.
- simulated cache behavior should have a configured target range, but actual per-request values should vary naturally.

## Bottom Line

The current system supports sticky scheduling and two selection modes, but only at a basic level.

- `priority` is primary/standby, not true load balancing.
- `balanced` is least-historical-success, not high-concurrency load balancing.
- sticky is useful and should be preserved because it supports Claude Code session continuity and cache reads.
- the main missing piece is load-aware scheduling with in-flight accounting, concurrency caps, cooldowns, and observability.

The best next step is to implement measurement first, then upgrade `balanced` to use active load while keeping sticky-first behavior.
