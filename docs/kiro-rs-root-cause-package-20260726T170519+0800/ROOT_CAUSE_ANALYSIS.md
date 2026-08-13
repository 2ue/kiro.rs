# kiro.rs 2026-07-26 incident root cause analysis

## Scope

- Local deployment: `/root/docker-compose/kiro-rs-2ue-59137`
- Source tree: `/root/code/kiro.rs`
- Running image revision: `3bfab8c9dc138062cad3c3cd1682c410bd6a263b`
- Running image version: `0.0.118`
- Local evidence directory included in this package:
  `incident-evidence-20260726T161622+0800/`
- Secondary independent deployment observed during the same incident:
  `152.53.194.170`

This analysis deliberately does not conclude that a few QPS is "high traffic".
The completed business request rate was low to moderate. The root cause is a
code/runtime coupling problem in the token manager storage and scheduler paths.

## Executive conclusion

The service did not fail because the completed request QPS was high. It failed
because business-path credential scheduler state writes and Redis coordination
were allowed to synchronously wait on PgSQL/Redis from the same Tokio runtime
that must accept HTTP connections, poll health handlers, and advance long SSE
streams.

When PgSQL/Redis latency or scheduling jitter appeared, stream completion/drop
paths and background credential state paths piled up on synchronous storage
bridges. The runtime then stopped polling HTTP accept and existing connection
lifecycles. The visible symptom was a large listen backlog and many
`CLOSE_WAIT` sockets, while usage records stopped because new work no longer
reached the normal completion/recording path.

This is a main-business token manager problem, not an ingress requestAdmission
rate limit problem and not a usage writer root cause.

## Local incident evidence

Useful local evidence files:

- `incident-evidence-20260726T161622+0800/03-http-probes.txt`
- `incident-evidence-20260726T161622+0800/09-container-internal-targeted.txt`
- `incident-evidence-20260726T161622+0800/10-pre-restart-final-traffic-and-runtime.txt`
- `incident-evidence-20260726T161622+0800/07-app-logs-0635-0647Z.txt`
- `incident-evidence-20260726T161622+0800/12-log-derived-provider-traffic-counts.txt`
- `incident-evidence-20260726T161622+0800/04-postgres-incident-summary.txt`

Important local observations:

- Docker reported the app as healthy, but the app HTTP endpoints timed out.
- The compose healthcheck only used TCP connect:
  `nc -z 127.0.0.1 8990 || exit 1`.
- Host HTTP probes timed out:
  `/` timed out after 3s, `/v1/messages` timed out after 5s.
- TCP connect still succeeded. This proves Docker health could be a false
  positive while HTTP handlers were not being polled.
- Process resources did not show FD exhaustion:
  `fd_count=151`, `socket_fd_count=145`, soft open files limit `1024`.
- All app threads in the sampled process were sleeping in `futex_wait_queue`.
- Process-owned TCP socket records were only `115`, but the container TCP table
  for port 8990 had `3417` entries.
- App port socket summary:
  `CLOSE_WAIT=2451`, `ESTABLISHED=963`, `FIN_WAIT2=2`, `LISTEN=1`.
- Listen queue was very high:
  `tcp 3620 0 0.0.0.0:8990 ... LISTEN`.
- Dominant remote peer in the socket table was `152.53.242.178`:
  about `2105 CLOSE_WAIT` and `1033 ESTABLISHED`.
- `152.53.194.170` appeared only as a small peer count locally and was not the
  root source of the local socket pile-up.

Completed business traffic was not high:

- Window: `2026-07-26 06:20:00Z` to `2026-07-26 06:45:00Z`.
- Total completed usage rows in that window: `3482`.
- Average completed request rate: about `133.9/min`.
- Peak completed minute: `256/min`, about `4.27 RPS`.
- Many requests were streams, and p95 durations were often tens to hundreds of
  seconds, which is normal for LLM streaming.

The important comparison is not "4.27 RPS is too high". It is:

- completed request peak: `256/min`;
- simultaneous socket pressure at failure: `3417` TCP records and listen
  Recv-Q around `3620`.

Those are not the same phenomenon. Usage rows count requests that completed and
were recorded. They do not count unaccepted connections, partial reads, stuck
tasks, or retries opened after the runtime stopped polling.

## 170 deployment comparison

The 170 host was treated only as an independent same-service deployment, not as
a cluster node and not as a mutual caller.

Observed shape on 170 matched the local failure:

- Same image revision and version:
  `3bfab8c9dc138062cad3c3cd1682c410bd6a263b`, `0.0.118`.
- Docker health was still TCP `nc -z`.
- Host `/healthz`, `/readyz`, and `/v1/models` timed out during failure.
- Container app process:
  `fd_count=228`, `socket_fd_count=222`, `thread_count=26`.
- All sampled app threads were sleeping in `futex_wait_queue`.
- Process-owned TCP states:
  `ESTABLISHED=32`, `CLOSE_WAIT=185`, `LISTEN=1`.
- Full port 8990 socket summary:
  `total_8990=480`, `CLOSE_WAIT=372`, `ESTABLISHED=105`,
  `FIN_WAIT2=2`, listen Recv-Q `278`.
- Dominant peer again was `152.53.242.178`:
  about `330 CLOSE_WAIT` and `99 ESTABLISHED`.
- Usage records stopped around the failure; there were no recent records in the
  sampled 10-minute window, and the latest usage row was around
  `2026-07-26 08:25:50Z`.
- Logs around `08:27Z` showed Redis scheduler capacity timeouts:
  `占用 Redis 凭据并发槽超过共享总期限 250ms`, followed by breaker open.

This independent reproduction makes a host-specific explanation unlikely. The
shared factor is the same service revision and the same implementation pattern.

## What api_rate_limit really is

`api_rate_limit` is not the local ingress requestAdmission layer.

In the running revision, `src/kiro/provider.rs` maps upstream failure kinds to
scheduler reasons:

- `ApiUpstreamFailureKind::RateLimit` -> `rate_limit`
- scheduler reason -> `api_rate_limit`

Relevant source location:

- `/root/code/kiro.rs/src/kiro/provider.rs:193`

So `api_rate_limit` is provider/upstream failure classification used by the
scheduler. It is not proof that local inbound request rate was limited by
requestAdmission.

Local requestAdmission was not active for this incident:

- `requestAdmission.enabled()` only returns true when `rpm > 0` or
  `maxConcurrentRequests > 0`.
- When disabled, `request_admission.acquire()` returns a disabled permit.
- Current file config had `requestAdmission: null`.

Relevant source locations:

- `/root/code/kiro.rs/src/model/config.rs:3087`
- `/root/code/kiro.rs/src/anthropic/request_admission.rs:475`

The real failing layer was the token manager's Redis scheduler and PgSQL
credential runtime/stat persistence paths.

## Usage writer versus main business path

Usage writing is intentionally separated:

- Usage recorder creates `mpsc` writer queues.
- Per-request usage recording enqueues records.
- If a queue is full or closed, usage persistence records are dropped to avoid
  blocking the main request.

Relevant source locations:

- `/root/code/kiro.rs/src/anthropic/usage.rs:1612`
- `/root/code/kiro.rs/src/anthropic/usage.rs:1743`
- `/root/code/kiro.rs/src/anthropic/usage.rs:2755`

Therefore the PgSQL usage timeout seen in logs is not the primary root cause.
It is a symptom or possible amplifier after the runtime/storage subsystem was
already unhealthy.

The important logs are not ordinary usage writes. They are token manager
business state writes:

- `原子记录 PgSQL 凭据调用成功`
- `保存凭据统计增量到 PgSQL`
- `从 PgSQL 重新加载运行时配置`
- `从 PgSQL 一致性重新加载凭据和运行态`
- `原子清理 Redis 会话软失败`
- `占用 Redis 凭据并发槽`
- `原子记录 Redis 调度选中次数`

These belong to credential scheduling/runtime state, not the separate usage
writer.

## Source path that explains the failure

The app uses the default Tokio runtime and serves axum directly:

- `/root/code/kiro.rs/src/main.rs:114`
- `/root/code/kiro.rs/src/main.rs:734`
- `/root/code/kiro.rs/src/main.rs:1034`

The router only applies CORS and body size limit around the Anthropic routes;
there is no separate Tower concurrency/timeout/load-shed layer for the whole
server:

- `/root/code/kiro.rs/src/anthropic/router.rs:407`

Long streaming is an intended normal path:

- SSE response is built with `Body::from_stream`.
- Stream creation uses `stream::unfold`.
- Ping keepalive interval is 5 seconds.
- Upstream idle timeout handling exists.

Relevant source locations:

- `/root/code/kiro.rs/src/anthropic/handlers.rs:7044`
- `/root/code/kiro.rs/src/anthropic/handlers.rs:7598`
- `/root/code/kiro.rs/src/anthropic/handlers.rs:7639`
- `/root/code/kiro.rs/src/anthropic/handlers.rs:8177`

On stream EOF, the stream path reports success:

- `/root/code/kiro.rs/src/anthropic/handlers.rs:8099`
- `/root/code/kiro.rs/src/kiro/provider.rs:897`

`KiroStreamCompletion::report_success()` calls
`token_manager.report_success_for_session_with_latency(...)` before taking the
in-flight lease:

- `/root/code/kiro.rs/src/kiro/provider.rs:897`

The token manager success/session path then performs state work:

- `report_success_for_session_with_latency()`:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:9737`
- `report_success_with_latency()`:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:9451`
- `persist_success_state()`:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:8747`
- `clear_session_soft_failure()`:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:5402`

The key bridge is `block_on_storage()`:

- `/root/code/kiro.rs/src/kiro/token_manager/storage_task.rs:681`

It calls `tokio::task::block_in_place(|| handle.block_on(future))` on the
current multi-thread Tokio runtime.

The storage executor also defaults to the current runtime handle if called
inside a runtime:

- `/root/code/kiro.rs/src/kiro/token_manager/storage_task.rs:662`

That means token manager storage tasks and synchronous bridges are not isolated
from the runtime that must accept HTTP, poll health, and drive streams.

Redis scheduler hot paths also affect admission and capacity:

- record scheduler selection:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:4259`
- acquire Redis in-flight lease:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:4417`
- wait in dispatch queue/capacity path:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:6177`
- wait for RPM availability:
  `/root/code/kiro.rs/src/kiro/token_manager/manager.rs:6332`

This explains why Redis scheduler capacity timeouts can turn into long-held
request tasks instead of quick local degradation.

## Root cause timeline

1. Normal long streaming traffic runs for tens to hundreds of seconds.
2. Streams complete or clients drop in clusters.
3. Each completion/drop enters token manager success/soft-failure cleanup.
4. Token manager performs synchronous PgSQL/Redis state operations on the main
   runtime.
5. PgSQL/Redis operations start exceeding their small shared budgets
   (`75ms`, `250ms`, `500ms`, and up to `5s` for PgSQL sync).
6. These waits accumulate on runtime worker threads and background state tasks.
7. The runtime stops promptly polling HTTP accept/health/stream close.
8. Listen backlog and `CLOSE_WAIT` grow. Clients retry, amplifying socket count.
9. Usage records stop because requests no longer reach normal terminal paths.
10. Docker health stays green because it only checks TCP connect.

## What this is not

This is not:

- proof that `4.27 RPS` is high traffic;
- proof that long streaming is invalid;
- local requestAdmission ingress rate limiting;
- primarily a usage writer problem;
- an FD exhaustion problem;
- a 170-to-local cluster/mutual-call problem.

## Correct root fix

The fix is not to lower the business request rate. The implementation should be
changed so storage and scheduler degradation cannot stop the HTTP runtime.

Required changes:

1. Release stream in-flight leases immediately at stream terminal state, before
   PgSQL/Redis persistence.
2. Split token manager success reporting into:
   local in-memory state update and capacity release on the request path;
   durable PgSQL/Redis persistence on a bounded background worker.
3. Make `report_success_for_session_with_latency()`, `persist_success_state()`,
   and `clear_session_soft_failure()` non-blocking with respect to HTTP stream
   completion.
4. Force token manager storage executor to use a dedicated runtime, not
   `Handle::try_current()`.
5. In single-instance deployments, when Redis scheduler coordination is
   degraded, fail open to local scheduling state where correctness permits.
   If fail-open is not allowed for a specific operation, fail fast with
   `503`/`Retry-After`; do not let requests sit in a 300-second dispatch wait
   because Redis coordination is unavailable.
6. Keep long SSE streams supported. Add body-read timeout only for request
   upload/body extraction, not for response stream duration.
7. Change Docker healthcheck to real HTTP `/healthz` or `/readyz`, not TCP
   connect.
8. Add regression tests/chaos tests:
   Redis latency >250ms, PgSQL latency >5s, 100 long streams completing or
   dropping together; assert `/healthz` remains responsive and listen backlog
   does not grow continuously.

## Current recovery state

After evidence was fixed, the local service was restarted and verified:

- `/healthz` -> `200`
- `/readyz` -> `200`
- `/v1/models` -> `401`
- all returned in milliseconds during the final check.

The 170 deployment was also sampled before restart and then restarted. Post
restart it recovered with `/healthz` and `/readyz` returning `200` and
`/v1/models` returning `401`.

## Package notes

This package intentionally includes raw local incident evidence. Some evidence
files may contain environment details, internal URLs, request IDs, or log labels.
Do not publish the archive broadly.
