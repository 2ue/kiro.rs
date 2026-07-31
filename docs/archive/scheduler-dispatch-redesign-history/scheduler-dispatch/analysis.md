# Scheduler analysis

## Problem statement

The service acts as a downstream-facing Anthropic-compatible gateway and dispatches requests to
Kiro credentials upstream. The production requirement is that an unhealthy credential must stop
receiving new requests as soon as scheduler-relevant errors are observed, especially under high
concurrency.

The motivating production symptom was an upstream call failing with:

```text
Post "http://152.53.194.170:59137/v1/messages?beta=true": context canceled
```

At roughly 50 RPM this kind of error can be caused by either side of the stream:

- downstream caller canceled the request or closed the connection;
- gateway context was canceled while waiting for or reading upstream;
- upstream proxy/instance connection was interrupted;
- scheduler kept routing new requests to a credential or endpoint that was already failing.

The scheduler should not punish credentials for pure downstream cancellation, but once upstream
HTTP status, network send, protocol, or stream-read failure is confirmed, the credential must be
cooled down and excluded from selection for other concurrent requests.

## Existing kiro.rs foundations

Before this redesign, `kiro.rs` already had useful scheduler primitives:

- disabled credentials are excluded;
- model-incompatible credentials are excluded;
- local transient cooldown is checked before selection;
- local credential RPM pacing is checked before selection;
- per-credential concurrency capacity can be enforced;
- Redis can share in-flight leases across instances;
- sticky sessions exist, but the selected credential still needs to pass dispatch checks;
- provider code already recognized several permanent failures such as quota exhaustion and
  risk-control states.

These foundations meant the correct path was to strengthen feedback and shared state, not replace
the entire scheduler.

## Gaps in the old behavior

The old behavior still had several high-concurrency gaps:

1. Single usable credential protection could avoid applying transient cooldown. That preserved
   availability in small setups, but it also allowed repeated traffic to hit an upstream account
   that had already returned `429`, `502`, or similar errors.
2. Some retryable send/protocol/unknown failures were local to the current request. Other
   concurrent requests could still select the same credential before shared state changed.
3. Redis cooldown writes could overwrite the current value, so a short fallback cooldown could
   shorten a longer `Retry-After` cooldown from another request.
4. Success reporting could clear active cooldown created by a concurrent failure, causing early
   re-entry.
5. `balanced` considered historical counters but did not make current in-flight load and recent
   health first-class signals.
6. `priority` mode was strict enough for manual priority routing, but it was not a health-aware
   high-concurrency scheduler.
7. There was no shared error-rate EWMA, latency EWMA, probation state, or selection count.
8. There was no optional global dispatch capacity or bounded global waiting queue.
9. Admin views did not explain why a credential was avoided, degraded, or preferred.
10. Warmup was controlled by one small global probability. If a ready credential already existed,
    a batch of warming credentials shared that tiny probability, so importing 10 accounts with
    `warmupRemaining = 3` could keep almost all traffic on the ready credential for a long time.
11. Historical success count was not enough to express dispatch pressure. Fast requests can finish
    before `inFlight` remains visible, so short-window selection count is needed to avoid repeatedly
    selecting the same credential.

## sub2api reference points

The reference implementation in `/Users/yuanfeijie/Desktop/procode/sub2api` confirmed several
useful patterns:

- account schedulability should be decided by hard filters before scoring;
- temporary unschedulable state should be stored with an `until` timestamp;
- cooldown extension should be monotonic;
- runtime health should include error-rate and latency EWMAs;
- candidate scoring should include priority, load, error rate, and latency;
- selecting from the top K candidates reduces concentration when many requests arrive at once.

The implemented `kiro.rs` design adopts those ideas while keeping its existing Redis lease and
sticky-session architecture.

## Error classification requirements

The scheduler must distinguish transient, permanent, and neutral outcomes.

Transient scheduler failures:

- `429` rate limit;
- retryable or unclear `402`;
- `408`;
- `502`, `503`, `504`, and other retryable `5xx`;
- network send/connect errors;
- retryable protocol mismatch, including non-eventstream responses where a stream was expected;
- unknown retryable upstream response;
- stream read error;
- upstream stream error event;
- upstream stream idle timeout;
- auth refresh protection while a credential is being refreshed or judged.

Permanent credential failures:

- definite monthly quota or official quota exhaustion;
- risk-control state;
- suspended or locked account;
- clear unrecoverable auth state after refresh/failure policy is exhausted.

Neutral outcomes:

- normal success;
- `400` and request validation errors caused by caller payload;
- downstream cancellation/drop when no upstream account/proxy failure is confirmed.

## Scheduling invariants

The redesigned scheduler must maintain these invariants:

1. A credential in cooldown receives no new dispatches until cooldown expires.
2. A credential at per-credential capacity receives no new dispatches.
3. Optional global capacity applies across all credentials and all service instances when Redis is
   enabled.
4. Sticky routing never overrides disabled, cooldown, rate-limit, or capacity filters.
5. Scheduler feedback is shared before the next request selection whenever Redis is configured.
6. Error-specific cooldown and backoff avoid hot-looping on repeated failures.
7. Recovery is gradual through a probation penalty after cooldown.
8. Selection prefers healthy and low-concurrency credentials.
9. The scheduler avoids a thundering herd by sampling among top healthy candidates.
10. Operators can see the reason, health state, queue state, and scoring inputs from Admin APIs/UI.

## Quota and cost signals

Official Kiro quota is measured in credits. Local usage cost is estimated in dollars. Those are
different dimensions and should not be mixed in card labels or scheduler scoring.

The current scheduler implementation does not use official quota credits or local estimated dollar
cost in the hot dispatch score. The reason is practical: quota/cost persistence is database-backed
and not designed to be queried on every request dispatch. A future version can add a Redis cached
quota snapshot as a scoring signal, but the hot path should stay fast and predictable.

## Selection count signals

Recent selection count and total selection count have different uses:

- 10s/60s/5m recent selection counts are scheduler signals. They capture short-window pressure and
  prevent a credential from receiving a disproportionate share when it is otherwise healthy.
- Total selection count is a persisted statistics signal. It is useful for audit, UI, and very weak
  long-term tie-breaking, but it should not dominate scheduling because old credentials would be
  permanently penalized and new credentials could be over-selected.

The implemented scheduler therefore uses recent 60s selection pressure in `health_balanced`, uses
recent selection counts as tie-breakers in `balanced`/same-priority selection, and keeps total
selection count persisted for display and optional weak weighting.
