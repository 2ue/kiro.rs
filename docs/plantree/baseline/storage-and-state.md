# Current Storage And State Model

Role: Project-wide factual baseline
Status: Current
Authority: Current state ownership, durability, and consistency behavior
As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11
Read when: Changing configuration, credentials, scheduling state, cache, usage, audit, Files, or shutdown behavior
Related: [System context](system-context.md), [Runtime flows](runtime-flows.md), [Risk hotspots](risk-hotspots.md)

## Storage Roles

The current server requires both PgSQL and Redis.

- **PgSQL** is the durable source of truth for configuration, credentials, durable credential state, external pool definitions, usage records/rollups, catalogs, audit logs, and events.
- **Redis** owns cross-process coordination and derived/ephemeral state: locks, sticky bindings, cooldown, leases, queues/wakeup, runtime notifications, cached records, and realtime usage summaries.
- **Process memory** owns request snapshots, credential selection state, caches, queues, catalogs, cleanup jobs, uploaded Files-compatible data, and recent usage.
- **Local filesystem** provides bootstrap files, embedded/build assets, ordinary process logs, and tool-format diagnostic JSONL.

This is a single-operator state model. Keys and records are not partitioned by tenant.

## Durable PgSQL Domains

`src/storage/postgres.rs` creates and accesses these principal domains:

| Domain | Main tables or records | Current mutation style |
| --- | --- | --- |
| Runtime configuration | `runtime_config` | Whole JSON document, monotonically incremented version |
| Credentials | `credentials` | CRUD plus supported models, priority, proxy, RPM, regions |
| Credential statistics | `credential_stats`, `credential_stats_delta_batches` | Batched deltas and drainable worker |
| Credential runtime | `credential_runtime_state`, `credential_runtime_mutations` | Generation/revision-aware mutations and deduplication IDs |
| Credential account data | `credential_account_info`, credential events | Periodic refresh and event history |
| Proxy resources | `proxy_resources` | Admin CRUD |
| External pools | `external_upstream_pools` | Admin CRUD and runtime reads |
| Usage records | `usage_records` | Per-request records with JSON diagnostics and soft deletion |
| Usage rollups | totals, time buckets, cache-read, duration, credential cost | Multiple upserts per record/batch |
| Model/pricing catalogs | model capability/pricing tables and sync status | Catalog snapshot persistence |
| Admin audit | `admin_audit_logs` | Best-effort spawned write from Admin service |

Migrations are embedded in the same source file as repository operations.

## Redis Domains

`src/storage/redis_cache.rs` currently combines generic cache primitives and domain operations:

| Domain | Purpose | Failure posture |
| --- | --- | --- |
| Refresh locks | Prevent concurrent token refresh across replicas | Required coordination for duplicate-refresh safety |
| Credential in-flight leases | Cross-process concurrency accounting | Critical release with timeout, retry, and tombstone behavior |
| Dispatch queue leases | Fair/bounded waiting across replicas | Lease renewal and stale cleanup |
| Sticky bindings | Session-to-credential affinity | Redis lookup/bind/complete operations in acquire/completion flow |
| Cooldown and transient state | Exclude unhealthy capacity temporarily | Influences eligibility |
| External pool leases | Cross-process external capacity | Touch/release via bounded storage executor |
| Runtime events | Notify replicas of config/credential changes | Listener health participates in readiness |
| Usage record cache | Recent/query acceleration | Derived from durable usage |
| Usage realtime summaries | Dashboard counters and buckets | Derived and rebuildable in principle, but rebuild path is incomplete |

Redis state should be considered either coordination-critical or derived. The current API does not consistently expose that classification to callers.

## Process-Local State

| State | Owner | Bound or lifetime |
| --- | --- | --- |
| Credential entries and scheduler facts | `MultiTokenManager` | Lives for process; protected by broad mutexes |
| Pending runtime/stat mutations | token manager and storage executor | Bounded queues plus drain deadline |
| Runtime configuration copies | token manager, `AppState`, per-request config | Replaced after reload; not one shared immutable version |
| Request API key index | `RequestApiKeyStore` | Replaced on runtime-config reload |
| Admin key | `AdminState` | Created at startup; current-process update only |
| Model and pricing catalogs | catalog services | Process-local snapshot plus PgSQL persistence |
| Prompt cache tracker | `PromptCacheTracker` | Count, per-account, TTL, and estimated-byte bounds |
| Cache creation controller | `PromptCacheCreationController` | Process-local frequency state |
| Recent usage | `UsageRecorder.records` | `usage_record_limit`, default 5,000 |
| Uploaded files | `AnthropicFileStore` | Live payload: 50 MiB/file, 128 files, 256 MiB total; ordering tombstones can grow after explicit delete; lost on restart |
| External availability cache | `ExternalPoolManager` | Short-lived snapshot |
| Usage cleanup job | `AdminService` | Current process only |

## Configuration Lifecycle

1. `config.json` is parsed at startup.
2. On first startup, the file config is inserted into PgSQL if the runtime row is absent.
3. Thereafter PgSQL is authoritative; the file is not a continuously reconciled source.
4. Admin config mutation clones the current complete `Config`, applies a closure, persists the complete JSON, and replaces local config.
5. PgSQL increments `runtime_config.version` but does not compare it with the version read by the updater.
6. A Redis event tells other replicas to reload selected runtime state.

The missing expected-version condition means two same-process or cross-process Admin writes can both succeed while the later full document silently overwrites fields from the earlier write.

## Credential State Classes

A credential combines data with different consistency needs:

| Class | Examples | Desired current durability |
| --- | --- | --- |
| Operator configuration | auth material, disabled, priority, endpoint, proxy, supported models | PgSQL durable |
| Durable runtime outcome | failure count, last success/failure, generation, revision | PgSQL mutation tables/state |
| High-frequency statistics | requests, errors, tokens, latency | Batched PgSQL deltas |
| Cross-process exclusion | refresh lock, in-flight lease, queue lease | Redis lease with TTL |
| Transient scheduling | short cooldown, EWMA, recent selection pressure | Redis and/or local memory depending on field |
| Session affinity | conversation/session binding | Redis sticky record plus local request snapshot |

These classes currently coexist in `CredentialEntry` and `MultiTokenManager`, which makes transaction, lock, and failure semantics difficult to reason about independently.

## Usage State Layers

Usage has at least five distinct representations:

1. upstream raw usage/events;
2. effective request token/cache facts after shaping;
3. downstream reported usage;
4. durable per-request `usage_records` data;
5. PgSQL and Redis dashboard rollups.

Current PgSQL `usage_records` retains the detailed per-request record, while Redis summaries serve dashboard/cache reads. The implementation does not yet expose an accepted event-authority/rebuild contract. Current Redis recording writes a snapshot, then a separate `SET NX` seen marker, then an atomic aggregate pipeline. If the marker succeeds and the aggregate pipeline fails, a retry sees the marker and skips the missing aggregate update.

PgSQL batch processing still performs repeated per-record and per-dimension operations. This is durable but creates write amplification and makes usage queue saturation observable in request completion.

## Cache State

- Prompt-cache fingerprints and creation-frequency state are process-local and bounded.
- They are simulation/evidence state, not proof of actual upstream cache residency.
- A process restart resets local prompt-cache knowledge.
- Multiple replicas do not share the same local cache tracker.
- Route-specific reported-usage policy can transform raw usage independently of body mode.

Current code and stored fields do not provide one explicit authority boundary between cache evidence and billing facts. The proposed separation and replica policy live in the modernization state/requirements documents.

## Files And Diagnostic State

### Uploaded Files

The Files-compatible store bounds live payload bytes and live map entries and evicts oldest live entries. It does not bound all metadata: explicit delete removes the map entry/bytes but leaves the ID in the FIFO order queue, so repeated upload/delete churn can grow tombstones and list-scan cost. It has no durable or Redis backing. Restart and cross-replica misses are expected. The risks are metadata growth, availability, large-object copies, and memory pressure, not cross-user access.

### Tool-Format Debug JSONL

The recorder uses a bounded channel, per-window record limits, per-record limits, rotation intervals, and a 100-MiB per-file cap. Defaults currently enable recording and request-body capture. The writer creates additional rolled files but has no total-directory byte, file-count, or age retention enforcement. It is therefore a bounded-rate but unbounded-lifetime filesystem state.

## Cross-Replica Invalidation

Current runtime events reload token-manager configuration, request API keys, and credential snapshots. Incomplete propagation remains for:

- `AdminState` key rotation;
- in-memory model capability and pricing catalog changes;
- process-local usage cleanup job state;
- prompt cache and Files-compatible objects by design.

These are high-availability consistency issues. They do not imply multiple users.

## Shutdown And Acceptance Semantics

The process has explicit drain/report APIs for credential statistics, runtime mutations, usage writers, and the bounded storage executor. However, acceptance semantics are not uniform:

- queue acceptance can mean memory acceptance rather than durable commit;
- usage writers can report abandoned records on timeout;
- general storage abandonment does not independently force non-zero exit;
- some Admin audit writes are untracked `tokio::spawn` tasks;
- process-local cleanup jobs are outside a unified task registry.

There is no current project-wide definition of `accepted`, `committed`, `retrying`, `dropped`, and `drained` across all mutation classes. The modernization state model proposes one; it is not a current implementation fact.

## Confirmed Consistency Gaps

| ID | Gap | Current consequence |
| --- | --- | --- |
| `COR-002` | Runtime config whole-document write lacks expected-version CAS | Concurrent Admin writes can lose fields silently |
| `COR-003` | Redis usage seen marker and aggregate update are separate | Derived dashboard can undercount after partial failure |
| `HA-001` | Admin key/catalog in-memory state lacks complete broadcast | Replicas can disagree until restart/reload |
| `OPS-003` | Cleanup job ownership is process-local | Another replica cannot reliably query or cancel it |
| `REL-001` | Accepted background mutations lack one durable outbox contract | Timeout/restart can abandon accepted work |
| `COR-005` | Request can read dynamic config more than once | One request can use mixed policy versions |

These gaps are analyzed and prioritized in the system-architecture modernization plan. This baseline only records current behavior.
