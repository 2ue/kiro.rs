# 152.53.194.170 live evidence summary

This file summarizes the live evidence observed on `152.53.194.170` during the
same incident. The host is an independent same-service deployment, not a cluster
node and not a mutual caller of the local service.

No password or interactive credential is included in this package.

## Identity

- Service: same `kiro-rs` image/service family as local deployment.
- Running image version: `0.0.118`.
- Running image revision: `3bfab8c9dc138062cad3c3cd1682c410bd6a263b`.

## Failure shape

- Docker health was green because the healthcheck was TCP `nc -z`, not HTTP.
- Host HTTP probes timed out:
  - `/healthz`: timeout
  - `/readyz`: timeout
  - `/v1/models`: timeout
- App process was alive but not advancing HTTP.

## Container/process state during failure

- app pid: `7`
- fd count: `228`
- socket fd count: `222`
- thread count: `26`
- sampled thread wait channel:
  `26 S futex_wait_queue`
- process-owned TCP states:
  - `ESTABLISHED=32`
  - `CLOSE_WAIT=185`
  - `LISTEN=1`

## Port 8990 socket state during failure

- `total_8990=480`
- `CLOSE_WAIT=372`
- `ESTABLISHED=105`
- `FIN_WAIT2=2`
- listen Recv-Q: `278`
- dominant remote peer:
  - `152.53.242.178`: about `330 CLOSE_WAIT`, `99 ESTABLISHED`

## Usage/log symptom

- No usage rows were recorded in the sampled last 10-minute window.
- Latest usage row at the failure sample was around `2026-07-26 08:25:50Z`.
- Logs mostly stopped after about `2026-07-26 08:27Z`.
- Logs around that time showed Redis scheduler capacity timeout:
  `占用 Redis 凭据并发槽超过共享总期限 250ms`.
- Redis scheduler breaker opened after consecutive failures.
- A PgSQL usage timeout also appeared, but it is treated as a symptom/amplifier,
  not the primary root cause, because the main token manager Redis/PgSQL state
  path was already failing.

## Recovery

After evidence was sampled, the app was restarted.

Post-restart checks recovered:

- `/healthz`: `200`
- `/readyz`: `200`
- `/v1/models`: `401`
- port 8990 socket state dropped back to a small baseline.

## Interpretation

The 170 host reproduced the same core failure shape as the local host:

- low completed usage visibility;
- HTTP handlers not responding;
- TCP health false positive;
- app threads waiting;
- listen queue and `CLOSE_WAIT` growth;
- Redis scheduler/PgSQL state latency immediately before stall.

This supports a service implementation root cause rather than a local host-only
resource problem.
