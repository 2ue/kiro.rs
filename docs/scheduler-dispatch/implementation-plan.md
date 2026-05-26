# Implementation record

This file records the implemented scheduler redesign. The work was completed as a combined backend,
Redis, API, and dual-UI change set.

## Backend config

`src/model/config.rs` now includes scheduler tuning fields:

- `credentialRateLimitCooldownSecs`
- `credentialServerErrorCooldownSecs`
- `credentialNetworkErrorCooldownSecs`
- `credentialStreamErrorCooldownSecs`
- `credentialProtocolErrorCooldownSecs`
- `credentialAuthErrorCooldownSecs`
- `credentialCooldownBackoffMultiplier`
- `credentialCooldownJitterPercent`
- `credentialProbationSecs`
- `dispatchGlobalMaxConcurrentRequests`
- `dispatchMaxQueuedRequests`
- `schedulerErrorEwmaAlpha`
- `schedulerPriorityWeight`
- `schedulerLoadWeight`
- `schedulerErrorWeight`
- `schedulerLatencyWeight`
- `schedulerProbationWeight`
- `schedulerTopK`

`loadBalancingMode` now accepts:

- `priority`
- `balanced`
- `health_balanced`

The existing defaults are backward-compatible. Global concurrency and global queue limits are
unlimited when set to `0`.

## Token manager

`src/kiro/token_manager.rs` implements the new scheduler behavior:

- `TransientFailureKind` classifies rate-limit, server, network, stream, protocol, and auth
  scheduler failures.
- `reportTransientFailureKind` calculates category-specific cooldown, backoff, jitter, max cap, and
  probation.
- Success reporting accepts optional latency and updates health state without clearing an active
  cooldown.
- A single usable credential is no longer exempt from cooldown when it fails transiently.
- `balanced` prefers lower in-flight load before historical counters.
- `priority` remains strict by priority, with lower in-flight load as a tie-breaker.
- `health_balanced` scores candidates by priority, current load, recent error rate, latency EWMA,
  and probation penalty.
- `schedulerTopK` weighted sampling reduces concurrent concentration on one candidate.
- Dispatch waiting uses an optional bounded queue and rejects locally when the queue is full.
- Snapshots include per-credential health fields and aggregate global capacity fields.

## Redis scheduler state

`src/storage/redis_cache.rs` now stores and updates shared scheduler state:

- monotonic cooldown state per credential;
- health state per credential;
- per-credential in-flight leases;
- global in-flight lease indexes;
- global queued dispatch count.

Important Redis behaviors:

- cooldown extension is monotonic;
- transient failure health update and cooldown write are atomic;
- dispatch lease acquisition checks per-credential and global capacity in one Lua path;
- stale lease cleanup also keeps global indexes consistent;
- queue enter/leave is shared across instances.

## Provider integration

`src/kiro/provider.rs` reports structured scheduler outcomes:

- network send failures report `Network`;
- `429` reports `RateLimit`;
- retryable/unclear `402` reports `RateLimit`;
- definite quota/monthly `402` disables the credential;
- `408` and retryable `5xx` report `Server`;
- auth failures report `Auth` before refresh/disable policy;
- retryable non-eventstream or protocol responses report `Protocol`;
- successful non-stream, stream, and MCP calls report latency-aware success.

`src/anthropic/handlers.rs` now treats upstream stream read errors, upstream stream error events, and
upstream idle timeouts as scheduler-relevant stream failures. Downstream-only cancellation remains a
soft failure.

## Admin API

`src/admin/types.rs` and `src/admin/service.rs` expose:

- new runtime config fields;
- `health_balanced` mode validation;
- global in-flight and queued request state;
- per-credential transient failure streak;
- recent error rate;
- latency EWMA;
- last scheduler error kind/reason/time;
- probation state;
- scheduler selection count;
- scheduler score.

Runtime config validation prevents invalid cooldowns, invalid EWMA alpha, invalid weights, and
invalid `schedulerTopK`.

## Old Admin UI

Files changed under `admin-ui/`:

- `src/types/api.ts`
- `src/api/credentials.ts`
- `src/components/dashboard.tsx`
- `src/components/credential-card.tsx`
- `src/components/runtime-config-panel.tsx`

Implemented UI behavior:

- load-balancing selector includes `健康均衡模式`;
- dashboard shows global dispatch capacity and queue state;
- credential cards show probation, transient streak, error rate, latency, score, selection count,
  and last scheduler error;
- runtime config exposes category cooldowns, backoff, jitter, probation, global capacity, queue
  limit, health-score weights, EWMA alpha, and top-K.

## Daisy Admin UI

Files changed under `admin-ui-daisy/`:

- `src/types/api.ts`
- `src/api/credentials.ts`
- `src/lib/runtime-config-defaults.ts`
- `src/components/CredentialsPanel.tsx`
- `src/components/ConfigPanel.tsx`

Implemented behavior mirrors the old Admin UI:

- `健康均衡模式` selector;
- global capacity stat;
- per-card health/error/score display;
- runtime controls for the new scheduler fields.

## Tests added or updated

Rust coverage includes:

- transient failure does not shorten an existing longer cooldown;
- success does not clear active transient cooldown;
- structured transient failure updates health and backoff;
- success updates latency health without clearing cooldown;
- `health_balanced` prefers the best scored candidate when `schedulerTopK = 1`;
- global capacity limits dispatch;
- bounded queue rejects excess waiter;
- Redis cooldown monotonicity;
- Redis scheduler health round trip;
- Redis global lease capacity state.

Redis integration tests continue to skip unless `KIRO_RS_TEST_REDIS_URL` is configured.

## Operational rollout

Recommended rollout order:

1. Deploy with the existing mode first if a conservative rollout is needed.
2. Confirm Redis is configured for multi-instance deployments.
3. Set per-credential concurrency caps.
4. Set global capacity and bounded queue if overload needs deterministic behavior.
5. Switch `loadBalancingMode` to `health_balanced`.
6. Observe credential cards and scheduler logs for error kind, cooldown, probation, queue, and score.
7. Tune category cooldowns only after identifying the noisy failure category.

## Remaining extension points

Future work can add:

- per-model health/cooldown scope;
- proxy/endpoint health;
- TTFT EWMA;
- cached quota-credit headroom as a scoring signal;
- scheduler metrics export for Prometheus or another telemetry sink;
- incident-level global upstream cooldown.
