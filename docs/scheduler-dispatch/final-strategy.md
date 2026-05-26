# Final scheduler strategy

## Goals

The final design makes dispatch conservative under failure and efficient under load:

1. Remove unhealthy credentials from scheduling immediately for scheduler-relevant transient
   failures.
2. Keep permanent account failures disabled until explicit recovery or refresh logic proves they are
   usable.
3. Prefer healthy, low-load credentials.
4. Share cooldown, in-flight leases, queue state, and health across instances through Redis.
5. Preserve backward compatibility for existing `priority` and `balanced` deployments.
6. Add an explicit `health_balanced` mode for production high-concurrency routing.
7. Expose enough state in Admin APIs and UIs to explain scheduler decisions.

## Dispatch pipeline

Each request passes through the same dispatch pipeline:

1. Build the candidate set from configured credentials.
2. Apply hard filters:
   - disabled account;
   - unsupported model;
   - active cooldown;
   - local RPM pacing;
   - per-credential concurrency capacity;
   - optional global concurrency capacity;
   - explicit retry-chain exclusions.
3. Prefer a sticky credential only if it passes the same hard filters.
4. Choose a candidate according to the configured mode.
5. Acquire the Redis/local dispatch lease atomically.
6. If no lease is available, wait until state changes or timeout.
7. If the bounded queue is full, reject locally instead of adding more waiters.
8. Report success, permanent failure, or structured transient failure after the attempt outcome is
   known.

## Dispatch modes

### priority

`priority` keeps the existing intent: lower configured priority value wins first. Within the same
priority, lower current in-flight count and lower success count are used as tie-breakers. This mode
is useful when operators need strong manual routing preference.

### balanced

`balanced` remains the lightweight load-spreading mode. It prefers lower current in-flight count,
then lower success count, then priority. It does not use health score weights, so it remains easy to
reason about and backward-compatible.

### health_balanced

`health_balanced` is the recommended mode for high-concurrency production traffic. It calculates a
score for each candidate, where lower score is better:

```text
score =
  configured_priority * schedulerPriorityWeight
  + current_in_flight * schedulerLoadWeight
  + recent_error_rate * schedulerErrorWeight
  + latency_ewma_ms * schedulerLatencyWeight
  + probation_penalty * schedulerProbationWeight
```

The scheduler then selects from the best `schedulerTopK` candidates using weighted random sampling.
This keeps traffic away from unhealthy credentials while avoiding all concurrent requests stampeding
onto the same single best candidate.

## Shared health state

Each credential has scheduler health state:

- `transientFailureStreak`
- `recentErrorRate`
- `latencyEwmaMs`
- `lastErrorKind`
- `lastErrorReason`
- `lastErrorAtMs`
- `probationUntilMs`
- `schedulerSelectionCount`

With Redis configured, this state is stored in Redis and read into scheduler snapshots. Without
Redis, the same concepts are maintained locally in the process.

Success handling:

- increments success counters;
- updates latency EWMA when latency is known;
- reduces recent error-rate EWMA;
- reduces transient failure streak gradually;
- does not clear an active cooldown created by a concurrent failure.

Failure handling:

- updates last error kind/reason/time;
- increments transient streak;
- increases recent error-rate EWMA;
- writes monotonic cooldown;
- extends probation through cooldown end plus `credentialProbationSecs`;
- wakes waiting dispatchers.

## Cooldown policy

Configured base cooldowns:

| Failure kind | Config field | Default |
| --- | --- | ---: |
| Rate limit / retryable unclear 402 | `credentialRateLimitCooldownSecs` | 30s |
| Server / 408 / 5xx | `credentialServerErrorCooldownSecs` | 5s |
| Network send/connect | `credentialNetworkErrorCooldownSecs` | 5s |
| Stream read/error/idle timeout | `credentialStreamErrorCooldownSecs` | 5s |
| Retryable protocol/unknown | `credentialProtocolErrorCooldownSecs` | 10s |
| Auth refresh protection | `credentialAuthErrorCooldownSecs` | 10s |

Cooldown duration is calculated from:

- `Retry-After` when present and applicable;
- otherwise the failure-kind base duration;
- exponential backoff using `credentialCooldownBackoffMultiplier`;
- random jitter using `credentialCooldownJitterPercent`;
- cap using `credentialMaxCooldownSecs`.

Redis cooldown writes are monotonic. If one request writes a 120 second cooldown and another request
later computes a 5 second cooldown, the 5 second value does not shorten the active 120 second
cooldown.

## Global capacity and queue

Per-credential concurrency still exists through `credentialMaxConcurrentRequests`.

The redesign adds optional global controls:

- `dispatchGlobalMaxConcurrentRequests`: maximum in-flight dispatches across all credentials.
  `0` means unlimited.
- `dispatchMaxQueuedRequests`: maximum requests allowed to wait for dispatch capacity. `0` means
  unlimited.

When Redis is configured, per-credential and global lease acquisition happen in one Lua operation.
That prevents multi-instance oversubscription under concurrency. Queue enter/leave is also kept in
Redis so all instances see the same waiting count.

When Redis is not configured, the same controls are enforced locally per process.

## Error handling matrix

| Outcome | Scheduler action |
| --- | --- |
| Success | report success, update latency/error EWMA, keep active cooldown if another failure set it |
| `429` | rate-limit cooldown, use `Retry-After` if available |
| retryable or unclear `402` | rate-limit cooldown |
| definite quota/monthly `402` | disable credential |
| `408`, `5xx` | server cooldown |
| network send/connect error | network cooldown |
| stream read error | stream cooldown |
| upstream stream error event | stream cooldown |
| upstream stream idle timeout | stream cooldown |
| retryable protocol mismatch | protocol cooldown |
| `401`/`403` invalid bearer | auth cooldown, then force refresh policy |
| risk-control/suspended/locked | disable credential |
| `400` caller validation | return to caller, no credential punishment |
| downstream cancellation/drop only | soft failure, release lease, no cooldown |

## Observability

Admin credential snapshots expose:

- cooldown and cooldown remaining;
- per-credential in-flight count and limit;
- transient failure streak;
- recent error rate;
- latency EWMA;
- last error kind/reason/time;
- probation flag and remaining seconds;
- scheduler selection count;
- scheduler score;
- estimated local dollar cost and request pricing counters from the usage recorder.

Aggregate snapshots expose:

- global in-flight request count;
- queued dispatch request count;
- configured global concurrency limit;
- configured global queue limit.

Both the old Admin UI and the Daisy Admin UI show these values and expose runtime controls for the
new scheduler fields.

## Recommended settings

For high-concurrency production:

- Use `health_balanced`.
- Set per-credential concurrency according to upstream/account capacity.
- Set global concurrency if the gateway, upstream proxy, or downstream SLA has a known limit.
- Set bounded queue size if overload should fail fast instead of waiting.
- Keep Redis enabled in multi-instance deployments.
- Start with default cooldowns and raise only the category that is actually noisy in logs.

## Non-goals in this implementation

The following are intentionally not included in this change:

- per-model health state;
- per-proxy or per-endpoint health state;
- global upstream incident detection;
- quota-credit headroom in hot-path score;
- local estimated dollar cost in hot-path score;
- TTFT-specific scoring.

These can be added later without changing the core pipeline because the implemented score and
health state already have extension points.
