# Deployment And Operations

Role: Project-wide factual baseline
Status: Current startup, state authority, replica, health, shutdown, container, and release behavior
Authority: Defines the implemented operational lifecycle and deployment artifacts; target hardening belongs to an active plan
As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11
Read when: Deploying, scaling replicas, changing durable state, adding workers, modifying health/readiness, handling shutdown, building images, or defining release evidence
Related: [System context](system-context.md), [Storage and state](storage-and-state.md), [Resource and concurrency model](resource-and-concurrency-model.md), [System architecture modernization](../plans/system-architecture-modernization/README.md)

## Supported Operational Context

`kiro-rs` is operated within one owner-controlled trust domain. A deployment may contain one process or multiple replicas, but every replica serves the same operator, request-key set, credential pool, and durable databases. Replication is for availability/capacity; it does not create users or tenants.

The runtime currently requires:

- a readable bootstrap configuration file;
- reachable PgSQL for durable state;
- reachable Redis for coordination and runtime-event health;
- a writable location when tool-format diagnostics or file logs are enabled;
- network access to configured Kiro, external-pool, remote-content, and optional tokenizer endpoints.

PgSQL and Redis are mandatory startup dependencies. There is no supported database-free or Redis-free serving mode in the current executable.

## Startup Sequence

`src/main.rs` is the process composition root. The current startup order is:

1. parse CLI arguments and initialize tracing/log filtering;
2. load bootstrap configuration from the configured file path;
3. connect to and migrate PgSQL and initialize Redis, retrying required dependency startup within a 60-second total bound;
4. import runtime configuration and credentials into PgSQL only when the corresponding durable state is absent;
5. load authoritative runtime configuration, credentials, credential runtime state, proxy resources, external pools, model capabilities, pricing, and supporting state from PgSQL;
6. construct the request API key store and fail startup when no valid request API key is configured;
7. build the usage recorder, prompt/cache state, model and pricing catalogs, `MultiTokenManager`, Kiro provider, external-pool manager, and request application state;
8. start the statistics/runtime-mutation worker, Redis runtime-event listener, catalog synchronization tasks, usage writers, and shared storage workers;
9. optionally assemble and mount the Admin API and two embedded Admin user interfaces according to configuration;
10. mount authenticated route families plus `/healthz` and `/readyz`, bind the listener, and begin serving.

Model/pricing synchronization can continue as startup background work and does not make initial request serving wait for a successful remote catalog refresh.

Evidence: `src/main.rs:45-50,52-217,260-485`.

## Configuration Bootstrap Versus Authority

The configured JSON and credentials files are bootstrap inputs, not the continuing source of truth after initial import. Once PgSQL contains runtime state, startup loads PgSQL authority and does not continuously reconcile arbitrary file edits into the live service.

Operational consequence: editing `config.json` or `credentials.json` on disk after bootstrap is not a reliable runtime-update mechanism. Changes must use the owning Admin/runtime mutation path or an explicitly supported migration procedure.

## State Authority

| State class | Current durable authority | Cross-process coordination/derived state | Process-local state |
| --- | --- | --- | --- |
| Runtime configuration | PgSQL runtime-config document/version | Redis runtime-change event | Request/config snapshots and clones |
| Request API keys | PgSQL-backed runtime config | Runtime-config event | `RequestApiKeyStore` used by auth middleware |
| Admin key | PgSQL-backed runtime config | Incomplete cross-replica refresh | `AdminState` on each process |
| Kiro credentials and durable runtime facts | PgSQL credential/runtime tables | Redis refresh locks, cooldown, RPM, leases, queues, sticky bindings | Credential entries and pending mutation state |
| Proxy resources | PgSQL | Selected runtime events/coordination | Manager snapshots |
| External-pool definitions | PgSQL | Redis leases, queues, cooldown/availability coordination | Pool manager caches |
| Usage records and durable rollups | PgSQL | Redis realtime/derived views and dedup markers | Recent usage deque and writer queues |
| Model capabilities and pricing | PgSQL plus embedded/bootstrap sources | Invalidation is incomplete | In-memory catalogs |
| Prompt-cache tracker | None | None | Per-process bounded tracker |
| Files-compatible uploads | None | None | Per-process live payload bounds; delete-tombstone metadata is not fully bounded |
| Usage-cleanup job control/status | PgSQL data is mutated by the job | None | Running job handle, cancellation, status |
| Tool-format diagnostics | Local filesystem | None | Writer channel and rolling-file state |
| Audit/events | PgSQL when persistence succeeds | None | Best-effort spawned work before completion |

PgSQL is the durable source of truth. Redis contains transient coordination and derived/rebuildable views unless a specific code path documents otherwise. Files and prompt-cache state disappear on restart and are not shared by replicas.

Evidence: `src/main.rs:92-180,325-476`, `src/storage/postgres.rs`, `src/storage/redis_cache.rs`, `src/anthropic/files.rs`, `src/anthropic/prompt_cache.rs`.

## Single-Replica Behavior

In a single process:

- request/config snapshots, Files objects, prompt-cache tracking, model/pricing catalogs, recent usage, and cleanup-job status all refer to that process;
- PgSQL/Redis still remain mandatory and are used even though no other application replica competes for leases;
- local scheduler counters and Redis coordination can both participate depending on configuration;
- a process restart loses ephemeral Files, prompt-cache history, recent-memory-only views, and in-process job status, then reloads durable state.

The single-user product model does not make process-local loss harmless. Claude Code Files references, cache projections, dashboards, and Admin jobs can change behavior after restart even though no cross-user isolation is involved.

## Multi-Replica Convergence

Multiple replicas share PgSQL and Redis within the same operator trust domain. Current convergence mechanisms include:

- Redis Pub/Sub runtime events for selected runtime/config/credential changes;
- a 60-second periodic fallback reload in the runtime-event listener;
- Redis leases, queues, sticky bindings, cooldowns, RPM state, and token-refresh locks;
- PgSQL-backed runtime configuration and credential state reloading;
- request API key refresh when runtime configuration is reloaded.

Current incomplete convergence behavior is material:

- Files objects remain on the receiving process only;
- prompt-cache evidence and creation-control state remain per process;
- usage cleanup job status and cancellation remain on the process that accepted the Admin command;
- Admin key rotation updates the receiving instance's `AdminState`, while the normal runtime-config listener does not fully refresh every other replica's Admin auth state;
- model capability and pricing Admin mutations do not have a complete broadcast/invalidation path;
- some process-local catalogs and snapshots can remain stale until their separate refresh path runs.

These are replica availability/consistency concerns, not tenant-data concerns. No tenant partitioning exists or is required by the current product.

Evidence: `src/main.rs:813-968`, `src/common/auth.rs:63-89`, `src/admin/handlers.rs:1059-1089`, `src/kiro/token_manager/manager.rs:1245-1296`, `src/admin/service.rs:3708-3816`.

## Health And Readiness

The application exposes two unauthenticated operational endpoints:

| Endpoint | Current meaning |
| --- | --- |
| `/healthz` | Process liveness; returns healthy while the HTTP process can handle the probe |
| `/readyz` | Serving readiness; checks PgSQL ping, Redis ping, and health of the Redis runtime-event subscription |

`/readyz` returns dependency/event details and must be used when the caller needs to know whether the replica has its required durable/coordination services and configuration-event path.

Current deployment manifests do not consistently use that contract:

- `docker-compose.deploy.yml` checks only whether TCP port 8990 accepts a connection with `nc`;
- `docker-compose.database.yml` defines PgSQL and Redis health checks, but its application service has no application-level health check;
- the `Dockerfile` defines no image `HEALTHCHECK`.

A successful TCP check therefore proves neither PgSQL readiness, Redis readiness, nor runtime-event subscription health.

Evidence: `src/main.rs:813-898`, `docker-compose.deploy.yml:1-22`, `docker-compose.database.yml:1-67`, `Dockerfile:1-51`.

## Background Work Inventory

| Background owner | Current responsibility | Persistence/shutdown relevance |
| --- | --- | --- |
| Statistics/runtime-mutation worker | Flush credential statistics and pending runtime mutations | Has explicit flush/shutdown reporting |
| Redis runtime-event listener | Subscribe, mark listener health, reload changed runtime state, run 60-second fallback reload | Readiness depends on subscription health; task is aborted during shutdown |
| Model/pricing startup sync | Refresh catalogs from configured/current sources | Failure is logged and serving can continue; task is aborted during shutdown |
| PgSQL usage writer | Persist usage records and rollups from a bounded queue | Drained/stopped during shutdown; saturation can fall back synchronously |
| Redis usage writer | Update realtime/derived usage views | Drained/stopped during shutdown; operations are not one atomic aggregate transaction |
| Shared storage executor | Run best-effort and critical PgSQL/Redis state tasks | Separate bounded lanes; drain/abandon counts reported |
| Tool-format debug writer | Serialize diagnostic records and roll JSONL files | Individual files bounded; directory lifetime unbounded |
| Admin audit spawn | Persist operator audit records | Some callers submit as best-effort detached work |
| Usage cleanup job | Delete/recompute selected durable usage data | Job handle, cancellation, and status are process-local |

An item being queued does not always mean it has become durable. Each queue's accepted, finished, failed, timed-out, rejected, and abandoned semantics must be inspected separately.

Evidence: `src/main.rs:260-485,530-645`, `src/anthropic/usage.rs:1398-1483`, `src/kiro/token_manager/storage_task.rs`, `src/admin/service.rs:1419-1438,3708-3816`.

## Graceful Shutdown

Shutdown begins on the configured process signal and has two top-level time budgets:

- HTTP graceful serving timeout: 30 seconds;
- total background shutdown deadline: 45 seconds.

Within the background deadline, individual drain stages are capped at 10 seconds and shutdown stages at 15 seconds, further reduced by remaining total time. The current sequence:

1. stop accepting new HTTP work and allow in-flight server work to finish until the 30-second server deadline;
2. mark the runtime-event health disconnected;
3. abort/join the Redis runtime-event listener and catalog sync task within their stage budgets;
4. request statistics/runtime-mutation flush and shutdown;
5. drain usage and shared storage executors within drain budgets;
6. stop usage writers and storage workers within shutdown budgets;
7. log drained, failed, timed-out, and abandoned counts.

Current exit-status behavior is asymmetric. Residual or failed statistics/runtime-mutation work triggers a panic/nonzero failure. Usage or shared-storage abandoned work is reported in logs but does not currently make the process exit nonzero.

Consequently, a zero exit status does not prove that every accepted usage/storage item became durable before shutdown.

Evidence: `src/main.rs:45-49,530-645`.

## Container Build And Runtime

The current multi-stage Docker build pins these main tool/runtime versions:

| Stage | Pinned base/tool |
| --- | --- |
| Frontend builder | Node `22.23.0` on Alpine `3.23` |
| Package manager | pnpm `11.11.0` |
| Rust builder | Rust `1.92.0` on Alpine `3.23` |
| Runtime image | Alpine `3.23` |

Both `admin-ui` and `ui` are built, then their output is embedded/consumed by the Rust build. The final image contains the release binary and CA certificates/BusyBox networking utilities, declares `/app/config` and `/app/logs` volumes, exposes port 8990, and starts with bootstrap config/credentials paths.

Current runtime-hardening facts:

- no non-root `USER` is declared, so the image runs as its default user;
- the root filesystem is not declared read-only;
- `/app/config` and `/app/logs` are writable mounts in the Compose examples;
- no Dockerfile health check is defined;
- no explicit capability drop, seccomp profile, resource limit, or `no-new-privileges` setting is present in the supplied Compose deployment.

Evidence: `Dockerfile:1-51`, `docker-compose.deploy.yml:1-22`, `docker-compose.database.yml:1-67`.

## Database Persistence And Recovery

The database Compose manifest provides:

- a named volume for PostgreSQL data;
- a named volume for Redis data;
- Redis append-only persistence;
- service-local PgSQL and Redis health checks.

The repository does not currently implement an automated backup schedule, backup verification, restore command/runbook, restore drill, schema/data recovery test, or declared RPO/RTO. A named volume and Redis AOF improve persistence across container recreation but are not backups and do not prove recoverability.

## Release Artifacts And Evidence

The GitHub Docker workflow builds/publishes images, but the current release artifact chain does not establish all common supply-chain evidence:

- `.github/workflows/docker-build.yaml` explicitly sets `provenance: false` for the relevant build/push step;
- no repository-owned SBOM, image signature, attestation, or verification policy is implemented in the inspected workflow;
- the current full Docker gate evidence is incomplete because a dependency fetch timed out before the Rust build and final image export completed;
- that incomplete run must not be cited as a passing Docker build.

The pinned source revision and dated evidence index remain necessary because a unit-test pass, local binary run, mock load test, real Claude Code workflow, and full container build prove different properties.

Evidence: `.github/workflows/docker-build.yaml:103-109`, `docs/plantree/plans/runtime-correctness-and-release-gates/history/evidence-index.md:24-53`.

## Operational Failure Semantics

| Failure | Current high-level behavior |
| --- | --- |
| PgSQL unavailable during startup | Retry within startup bound, then fail startup |
| Redis unavailable during startup | Retry within startup bound, then fail startup |
| PgSQL/Redis unavailable after startup | `/readyz` fails; request/background paths follow their local timeout/fallback behavior |
| Redis runtime subscription lost | Listener health becomes unready and reconnects; periodic reload is part of the listener loop |
| Kiro/external upstream slow | Request can remain leased until header/idle/request timeout semantics resolve it |
| Replica restart | Durable PgSQL state reloads; process-local Files/cache/recent/job state is lost |
| Usage/storage queue pressure | Bounded queues apply wait, reject, or synchronous fallback depending on path |
| Shutdown deadline exhausted | Remaining work is logged/abandoned; only selected residue currently changes exit status |
| Diagnostic filesystem fills | Writer/logging fails according to I/O path; no directory-wide retention prevents the condition |

This table summarizes current outcomes; it is not an incident response runbook.

## Explicitly Deferred Existing Scope

The [Runtime correctness and release gates plan](../plans/runtime-correctness-and-release-gates/README.md) already records deferred production-hardening scope. This baseline does not reopen or reclassify that scope merely by restating current facts. Any implementation commitment must be accepted and placed in the owning roadmap with evidence and rollback criteria.

## 未实现要求

The following required outcomes are not current baseline behavior. Their target ownership, design, work graph and acceptance criteria belong to the [Greenfield AI Gateway plan](../plans/greenfield-ai-gateway/README.md); current Rust maintenance still respects already deferred scope:

- make deployment health checks use `/readyz` where serving readiness is required, while retaining `/healthz` only for liveness;
- complete cross-replica invalidation/convergence for Admin key, catalogs, cleanup jobs, prompt-cache/Files compatibility decisions, and every runtime mutation advertised as immediately effective;
- assign durable ownership, retry, deduplication, queue-age visibility, and shutdown outcomes to accepted usage, audit, cleanup, and runtime-state work;
- fail shutdown/exit status when any class of required accepted durable work remains failed, timed out, or abandoned;
- define, automate, and drill PgSQL/Redis backup and restoration with explicit RPO, RTO, retention, encryption, and verification evidence;
- harden the runtime container with a non-root user, least privileges, controlled writable paths, resource limits, and deployment-specific security settings;
- publish verifiable SBOM, signature, provenance/attestation, immutable image identity, and a release evidence manifest;
- make full container build, readiness, migration, slow-upstream drain, dependency-failure, restart, and restore verification explicit release gates without leaving unbounded local artifacts.
