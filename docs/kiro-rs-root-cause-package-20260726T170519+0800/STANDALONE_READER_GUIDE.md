# Standalone reader guide

This package is intended to be readable without the chat history.

## What happened

On 2026-07-26, a `kiro-rs` service deployment became externally unhealthy:

- Docker still reported the app container as healthy.
- TCP connection to the service port still succeeded.
- HTTP endpoints timed out.
- The app process remained alive but stopped advancing HTTP accept/connection
  lifecycle work.

A second independent deployment on `152.53.194.170` showed the same failure
shape shortly afterwards. That host is not treated as a cluster node and not as
a mutual caller. It is only used as an independent same-version reproduction.

## Key conclusion in one sentence

The completed request rate was not high enough to explain the failure. The
failure was caused by synchronous PgSQL/Redis credential scheduler state work in
the token manager sharing the same Tokio runtime that must drive HTTP accept,
health handlers, and long SSE streams.

## Confidence and limitations

Confidence: high.

Why high:

- Local raw evidence was captured before restart.
- The same service revision reproduced on an independent 170 deployment.
- Local evidence and 170 evidence had the same shape:
  HTTP timeout, TCP health false positive, app threads waiting, `CLOSE_WAIT`
  growth, listen backlog growth, Redis scheduler/PgSQL state latency before
  stall.
- Running source revision explains the exact logged operation names.

Limitations:

- There is no Rust async backtrace or pprof capture in this package.
- Raw 170 pre-restart dump files were not landed into the local evidence
  directory; 170 is represented by `170-live-evidence-summary.md`.
- Some source paths in `ROOT_CAUSE_ANALYSIS.md` refer to the original host path.
  The relevant source excerpts are also included under `source-excerpts/` so
  the package remains useful away from the host.

## How to read the package

Start with:

1. `ROOT_CAUSE_ANALYSIS.md`
2. `EVIDENCE_CLAIM_MAP.md`
3. `incident-evidence-20260726T161622+0800/03-http-probes.txt`
4. `incident-evidence-20260726T161622+0800/10-pre-restart-final-traffic-and-runtime.txt`
5. `incident-evidence-20260726T161622+0800/07-app-logs-0635-0647Z.txt`
6. `source-excerpts/`

## Terms

- local deployment: `/root/docker-compose/kiro-rs-2ue-59137`
- 170 deployment: independent same-service host `152.53.194.170`
- dominant peer: `152.53.242.178`, the largest remote peer in socket state
  samples on both local and 170
- completed business traffic: rows visible in usage records after a request
  reached terminal recording
- socket pressure: kernel/container TCP socket state, including unaccepted
  connections and connections not recorded in usage
- requestAdmission: local inbound API-key admission/rate/concurrency layer
- Redis scheduler: token manager Redis coordination for credential RPM,
  in-flight leases, sticky session state, and scheduler runtime state
- usage writer: asynchronous usage persistence queue in
  `src/anthropic/usage.rs`

## Disambiguation

The package does not claim:

- "4.27 RPS is high traffic".
- "long LLM streaming is abnormal".
- "170 and local form a cluster".
- "api_rate_limit is local ingress throttling".
- "usage writer is the primary root cause".

The package does claim:

- Completed usage QPS is too low to explain thousands of socket records.
- Socket pressure grew because the service stopped advancing connection
  lifecycle work.
- The source contains synchronous token manager PgSQL/Redis paths that match the
  exact operation names in the logs.
- Those paths share the runtime used by HTTP/stream handling.
- The root fix is code-level isolation/asynchronization and correct degraded
  behavior, not lowering already-low business traffic.

## Verification checklist

Use `SHA256SUMS.txt` to verify package file integrity after extraction.

To validate the main conclusion, check:

- `03-http-probes.txt`: HTTP timeout but TCP check success.
- `10-pre-restart-final-traffic-and-runtime.txt`: Docker TCP healthcheck,
  thread wait state, FD count, TCP state summary, completed usage rates.
- `07-app-logs-0635-0647Z.txt`: token manager PgSQL/Redis sync operation
  latency/timeouts immediately before stall.
- `source-excerpts/storage_task_650_710.txt`: `block_on_storage()`.
- `source-excerpts/provider_stream_completion_854_980.txt`: stream completion
  reports success before lease take.
- `source-excerpts/manager_report_success_9440_9750.txt`: success/session
  report path.
- `source-excerpts/manager_persist_success_8735_8805.txt`: synchronous PgSQL
  success persistence.
- `source-excerpts/manager_redis_blocking_paths_3388_3575.txt`: synchronous
  Redis scheduler bridge.
- `source-excerpts/usage_writer_1605_1818.txt`: usage writer is a non-blocking
  queue and is separate from token manager scheduler state.

## Sensitive contents

This package includes raw operational evidence. It may contain internal URLs,
request IDs, log labels, Docker inspect output, or other environment details.
Do not publish the archive broadly.
