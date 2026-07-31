# Evidence to claim map

## Claim 1: Docker health was a false positive

Evidence:

- `incident-evidence-20260726T161622+0800/03-http-probes.txt`
  - HTTP probes timed out.
  - TCP check succeeded.
- `incident-evidence-20260726T161622+0800/10-pre-restart-final-traffic-and-runtime.txt`
  - Docker healthcheck is `nc -z 127.0.0.1 8990`.
- `incident-evidence-20260726T161622+0800/06-app-docker-inspect.json`
  - Docker inspect copy of the same healthcheck.

Interpretation:

TCP connect can succeed while axum handlers are not being polled. Docker
healthy does not mean `/healthz` or `/readyz` were responsive during the
incident.

## Claim 2: Completed traffic was not high enough to explain the incident

Evidence:

- `incident-evidence-20260726T161622+0800/10-pre-restart-final-traffic-and-runtime.txt`
  - Completed usage window 06:20-06:45 UTC.
  - Peak completed minute: `256/min`.
  - Average completed rate: about `133.9/min`.

Interpretation:

The completed business request rate was low to moderate. It is not a credible
root explanation for a service-level hang.

## Claim 3: Socket pressure was much larger than completed usage traffic

Evidence:

- `incident-evidence-20260726T161622+0800/10-pre-restart-final-traffic-and-runtime.txt`
  - `total_8990=3417`
  - `CLOSE_WAIT=2451`
  - `ESTABLISHED=963`
  - listen Recv-Q around `3620`
- `incident-evidence-20260726T161622+0800/02-sockets-and-fds.txt`
  - similar socket state summary.

Interpretation:

Usage records are a lower bound. They exclude unaccepted connections, partial
requests, retry attempts that never reached completion, and stuck connection
lifecycles.

## Claim 4: The process did not fail from FD exhaustion

Evidence:

- `incident-evidence-20260726T161622+0800/10-pre-restart-final-traffic-and-runtime.txt`
  - `fd_count=151`
  - `socket_fd_count=145`
  - soft open files limit `1024`
  - process-owned TCP socket records `115`

Interpretation:

FD exhaustion is not the root cause. The large TCP table/listen queue is caused
by stopped connection lifecycle progress, not by the app process owning
thousands of file descriptors.

## Claim 5: Runtime progress stalled

Evidence:

- `incident-evidence-20260726T161622+0800/09-container-internal-targeted.txt`
  - thread wait channel summary: all sampled app threads in
    `futex_wait_queue`.
- `incident-evidence-20260726T161622+0800/10-pre-restart-final-traffic-and-runtime.txt`
  - thread wait channel summary: all app threads in `futex_wait_queue`.
  - listen queue and `CLOSE_WAIT` growth at the same time.

Interpretation:

The app process was alive but was not advancing accept/handler/connection close
work promptly.

## Claim 6: Logs point to token manager PgSQL/Redis scheduler state, not usage QPS

Evidence:

- `incident-evidence-20260726T161622+0800/07-app-logs-0635-0647Z.txt`
  - `原子清理 Redis 会话软失败`
  - `占用 Redis 凭据并发槽`
  - `原子记录 Redis 调度选中次数`
  - `从 PgSQL 重新加载运行时配置`
  - `从 PgSQL 一致性重新加载凭据和运行态`
  - `原子记录 PgSQL 凭据调用成功`
  - `保存凭据统计增量到 PgSQL`

Interpretation:

These operation names are token manager credential scheduler/runtime state
operations. They match source paths in `source-excerpts/`.

## Claim 7: usage writer is separate and not the primary root cause

Evidence:

- `source-excerpts/usage_writer_1605_1818.txt`
  - usage writer uses bounded `mpsc` queues.
  - queue full/closed paths drop persistence records to avoid blocking the main
    request.
- `source-excerpts/storage_task_650_710.txt`
  - token manager storage bridge is separate from usage writer and can block on
    storage.

Interpretation:

PgSQL usage timeout is at most a symptom/amplifier. The main root path is token
manager credential scheduler/runtime state, not ordinary usage record
persistence.

## Claim 8: api_rate_limit is provider/upstream classification

Evidence:

- `source-excerpts/provider_api_rate_limit_176_205.txt`
  - `ApiUpstreamFailureKind::RateLimit` maps to scheduler reason
    `api_rate_limit`.

Interpretation:

`api_rate_limit` is not local requestAdmission. It is an upstream/provider
failure classification fed into scheduler state.

## Claim 9: local requestAdmission was not active

Evidence:

- `ROOT_CAUSE_ANALYSIS.md`
  - current file config had `requestAdmission: null`.
- `source-excerpts/` should be read with original source path references:
  - `/root/code/kiro.rs/src/model/config.rs:3087`
  - `/root/code/kiro.rs/src/anthropic/request_admission.rs:475`

Interpretation:

The incident cannot be explained as local inbound requestAdmission throttling.

## Claim 10: Long LLM streaming is intended normal behavior

Evidence:

- `source-excerpts/handlers_sse_builder_7025_7051.txt`
  - SSE response uses `Body::from_stream`.
  - ping interval is 5 seconds.
- `source-excerpts/handlers_sse_unfold_7596_7651.txt`
  - stream body polling and keepalive path.
- `source-excerpts/handlers_stream_terminal_8060_8205.txt`
  - stream terminal success/error handling.

Interpretation:

The fix must keep long streaming supported. The bug is not long streaming
itself; it is synchronous storage/scheduler work in the stream terminal path.

## Claim 11: 170 reproduced the same implementation failure shape

Evidence:

- `170-live-evidence-summary.md`

Interpretation:

170 is not a cluster peer. It is independent corroboration that the same
revision can wedge in the same way under similar traffic/Redis scheduler
conditions.

## Claim 12: Correct fix is isolation/degradation behavior, not lower business traffic

Evidence:

- Claims 1-11 together.
- `source-excerpts/storage_task_650_710.txt`
  - storage bridge shares runtime.
- `source-excerpts/provider_stream_completion_854_980.txt`
  - stream completion enters token manager before lease release.
- `source-excerpts/manager_report_success_9440_9750.txt`
  - success/session path.
- `source-excerpts/manager_persist_success_8735_8805.txt`
  - synchronous PgSQL success persistence.
- `source-excerpts/manager_redis_blocking_paths_3388_3575.txt`
  - synchronous Redis scheduler bridge.

Interpretation:

The code should release capacity immediately, enqueue durable state writes, use
dedicated storage runtime/workers, and fail open or fail fast when Redis
scheduler coordination is degraded.
