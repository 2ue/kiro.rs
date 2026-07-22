# Current Module Map

Role: Project-wide factual baseline
Status: Current
Authority: Current source tree and dependency map, not a target package design
As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11
Read when: Locating implementation ownership or assessing the blast radius of a change
Related: [System context](system-context.md), [Runtime flows](runtime-flows.md), [Storage and state](storage-and-state.md)

## Executable Shape

The repository is currently a binary crate. `src/main.rs` declares the top-level modules and performs dependency construction, route assembly, background task startup, signal handling, and shutdown. There is no `src/lib.rs`, so most integration tests are white-box unit tests attached to binary modules rather than black-box library contract tests.

```text
src/
  main.rs                 process composition and lifecycle
  anthropic/              downstream Anthropic/Claude Code compatibility
  kiro/                   upstream Kiro protocol, transport, and credential scheduling
  external_pool.rs        external compatible pool orchestration
  external_pool/          extracted external body/model/retry/usage stages
  admin/                  Admin API, service, middleware, DTOs
  admin_ui/               embedded Admin UI routing/assets
  storage/                PgSQL and Redis implementations
  model/                  runtime configuration, CLI arguments, model processing/support
  common/                 shared request authentication
  http_client.rs          outbound serialization/compression helpers
  token.rs                local and optional remote token counting
  bin/kiro_loadtest.rs    fake upstream plus load/chaos driver
```

The project contains real submodules and is not literally unmodularized. The main issue is that dependency ownership remains broad: several extracted files still use parent-private state, and central objects directly depend on both domain logic and infrastructure.

## Downstream Protocol Surface

| Module | Current responsibility | Boundary concern |
| --- | --- | --- |
| `src/anthropic/router.rs` | Route families, authentication middleware, body limit, CORS, dependency-to-state conversion | Route policy is materialized as cloned `AppState` variants |
| `src/anthropic/middleware.rs` | `AppState`, request API key middleware integration, route-specific runtime values | `AppState` contains more than 40 service/config fields |
| `src/anthropic/handlers/request_entry.rs` | Raw request entry, depth validation, raw external direct/preflight attempts, parsing | Fetches a dynamic runtime snapshot before main processing |
| `src/anthropic/handlers.rs` | Main request orchestration, local/external routing, stream/non-stream handling, retry, usage | Approximately 7,025 lines plus 3,979-line sidecar tests; application responsibilities are mixed |
| `src/anthropic/handlers/parsed_body_pipeline.rs` | Parsed-body capability staging | Uses parent orchestration state rather than a stable application contract |
| `src/anthropic/handlers/local_body_pipeline.rs` | Local Kiro body preparation | Narrower file, but still compiled inside the large handler module boundary |
| `src/anthropic/types.rs` | Anthropic request/response DTOs | DTOs are imported by storage and other infrastructure modules |
| `src/anthropic/envelope.rs` | Error and response envelope compatibility | Cross-cutting protocol dependency |
| `src/anthropic/stream.rs` | Kiro event to Anthropic SSE state machine | Compatibility-critical and heavily stateful |

## Request Body And Conversion

| Module | Current responsibility | Important behavior |
| --- | --- | --- |
| `src/anthropic/body_capabilities.rs` | Body-processing capability plans | Separates parsed/local/external body modes at configuration level |
| `src/anthropic/body_processing.rs` | File/remote materialization and base64 media normalization | Safe mode downloads remote sources; current dirty tree adds request source/download/base64/attempt/deadline bounds, connection-time DNS filtering and process workflow admission; final load evidence is pending |
| `src/anthropic/converter.rs` | Conversion entrypoint and shared helpers | Still owns cross-stage conversion coordination |
| `src/anthropic/converter/content.rs` | Content and PDF conversion | PDF extraction runs in the request conversion path |
| `src/anthropic/converter/history.rs` | Message history normalization | Compatibility-sensitive history mutation |
| `src/anthropic/converter/model.rs` | Model conversion helpers | Interacts with model mapping and endpoint behavior |
| `src/anthropic/converter/schema.rs` | Tool input-schema normalization | Recursively walks untrusted JSON |
| `src/anthropic/converter/thinking.rs` | Thinking normalization | Must stay aligned with stream and non-stream responses |
| `src/anthropic/converter/tool_pairing.rs` | Tool use/result pairing repair | Client-visible mutation with diagnostic implications |
| `src/anthropic/converter/tools.rs` | Tool conversion and built-ins | Handles WebSearch and Kiro tool constraints |
| `src/anthropic/payload_guard.rs` | Body sizing, repair, shaping, trimming, diagnostics | Can clone and serialize large request structures repeatedly |
| `src/anthropic/payload_guard_runtime.rs` | Runtime adapters for payload guard | Couples guard execution to current request/runtime structures |
| `src/anthropic/request_facts.rs` | Lightweight facts from raw JSON bytes | Enables pre-parse routing/model decisions |

## Cache, Usage, Files, And Catalogs

| Module | Current responsibility | State type |
| --- | --- | --- |
| `src/anthropic/prompt_cache.rs` | Prefix canonicalization, cache fingerprint tracking, simulated usage | Bounded process-local state |
| `src/anthropic/prompt_cache_creation_control.rs` | Cache creation frequency policy | Process-local control state |
| `src/anthropic/cache.rs` | Cache/usage helper behavior | Request-path calculations |
| `src/anthropic/usage.rs` | Usage DTOs, recent records, writers, queries, dashboards, cost/latency diagnostics | In-memory plus concrete PgSQL and Redis dependencies |
| `src/anthropic/pricing.rs` | Pricing catalog and cost calculation | In-memory catalog persisted through PgSQL |
| `src/anthropic/model_capabilities.rs` | Model capabilities and synchronization status | In-memory catalog persisted through PgSQL |
| `src/anthropic/files.rs` | Anthropic Files-compatible staging | Live payload max 50 MiB/file, 128 files, 256 MiB total; delete leaves ordering tombstones |
| `src/anthropic/tool_format_debug.rs` | Tool-format diagnostic sampling and JSONL writer | Bounded channel; directory retention is not bounded |
| `src/anthropic/websearch.rs` | WebSearch conversion behavior | Protocol conversion helper |

Usage currently crosses several semantic layers in the same types: raw upstream values, local cache evidence, downstream projection, billing, persistence, and dashboard DTOs.

## Kiro Upstream And Scheduling

| Module | Current responsibility | Boundary concern |
| --- | --- | --- |
| `src/kiro/provider.rs` | Kiro HTTP calls, retries, credential attempt loop, client cache, completion guards | Transport and scheduler lifecycle are directly coupled |
| `src/kiro/endpoint/ide.rs` | Kiro IDE request envelope | Parses and rewrites serialized JSON |
| `src/kiro/endpoint/cli.rs` | Kiro CLI request envelope | Parses and rewrites serialized JSON |
| `src/kiro/parser/*` | Event-stream framing, CRC, headers, decoder, errors | Low-level protocol implementation |
| `src/kiro/protocol.rs` | Kiro protocol DTOs | Shared upstream contract |
| `src/kiro/token_manager/manager.rs` | Credential catalog, selection, refresh coordination, sticky logic, mutations, Admin operations | Approximately 8,178 lines and 28 state fields; central God Object |
| `src/kiro/token_manager/account_state.rs` | Per-credential mutable runtime record | Mixes durable and transient scheduler facts |
| `src/kiro/token_manager/capacity.rs` | Capacity calculations | Mostly algorithmic extraction |
| `src/kiro/token_manager/strategy.rs` | Candidate ordering/scoring | Mostly algorithmic extraction |
| `src/kiro/token_manager/rpm.rs` | RPM window logic | State is still owned by manager entries |
| `src/kiro/token_manager/sticky.rs` | Session-affinity helpers | Redis access remains coordinated by manager |
| `src/kiro/token_manager/concurrency.rs` | Local/Redis leases, guards, release and renewal | Normal release can synchronously wait for Redis |
| `src/kiro/token_manager/queue.rs` | Dispatch queue records and wait semantics | Manager owns orchestration |
| `src/kiro/token_manager/refresh.rs` | Refresh result/state helpers | Manager/provider own actual refresh workflow |
| `src/kiro/token_manager/storage_task.rs` | Bounded normal/critical storage executor and shutdown | Shared infrastructure hidden under token-manager namespace |
| `src/kiro/token_manager/redis_runtime.rs` | Redis runtime helpers | Infrastructure-specific behavior inside scheduler module |

The extracted scheduler files improve navigation and isolate some algorithms, but most `impl MultiTokenManager` orchestration, state ownership, locks, storage calls, and Admin mutation behavior remain in `manager.rs`.

## External Pool

| Module | Current responsibility | Boundary concern |
| --- | --- | --- |
| `src/external_pool.rs` | Pool DTOs/config, availability, selection, capacity, HTTP, retry, streaming, billing, usage | Approximately 4,978 lines plus 3,943-line sidecar tests |
| `src/external_pool/body_pipeline.rs` | Raw versus normalized body construction | Correctly separates body mode but depends on parent request types |
| `src/external_pool/model_pipeline.rs` | Outbound model selection/rewrite | Must remain independent from body mode |
| `src/external_pool/retry_pipeline.rs` | Retry route reconstruction | Clones payload/route state for guard retry |
| `src/external_pool/usage_projection.rs` | External upstream usage observation and route-policy projection | Still integrated through broad external route context |

`ExternalPoolManager` directly owns concrete PgSQL, Redis, an HTTP client, notifications, and an availability cache. Selection, transport, capacity, and accounting are therefore not independent test boundaries.

## Admin Control Plane

| Module | Current responsibility | Boundary concern |
| --- | --- | --- |
| `src/admin/router.rs` | Admin route catalog | Broad surface across every runtime domain |
| `src/admin/middleware.rs` | `AdminState` and key authentication | Admin key is process-local after construction |
| `src/admin/handlers.rs` | HTTP extraction/response conversion | Mostly thin, but invokes synchronous service facades |
| `src/admin/service.rs` | Credentials, proxies, pools, usage, catalogs, config, keys, cleanup, audits | Approximately 6,602 lines and more than 80 public operations |
| `src/admin/types.rs` | Handwritten Admin API DTOs | Duplicated manually in two frontend codebases |
| `src/admin_ui/*` | Embedded static Admin applications | Maintains legacy and newer UI surfaces |

`AdminService` is a facade and a broad domain service at the same time. Some synchronous methods bridge into async storage through `block_in_place`, making request-path I/O less visible.

## Configuration And Model Processing

| Module | Current responsibility | Boundary concern |
| --- | --- | --- |
| `src/model/config.rs` | All runtime configuration DTOs, defaults, normalization, patches, loading | Approximately 5,529 lines; top-level `Config` has about 101 fields |
| `src/model/arg.rs` | CLI flags and credential diagnostic commands | Process entry concern |
| `src/model/model_processing.rs` | Request model transformation | Used by local and external paths |
| `src/model/model_support.rs` | Supported-model matching | Used by credential and external eligibility |
| `src/token.rs` | Token estimation and optional remote token API | Optional remote path blocks through sync bridge |

Configuration is copied into `MultiTokenManager`, `AppState`, and `RequestRuntimeConfig`. A request currently may materialize more than one dynamic configuration snapshot.

## Infrastructure

| Module | Current responsibility | Boundary concern |
| --- | --- | --- |
| `src/storage/postgres.rs` | Connection, schema migration, runtime config, credentials, pools, usage, catalogs, audit, events | Approximately 11,372 lines including embedded tests/migrations; imports domain/UI-facing DTOs |
| `src/storage/redis_cache.rs` | Connection, generic cache helpers, locks, leases, queues, sticky state, cooldown, usage summaries | Approximately 6,357 lines; imports scheduler and dashboard-specific types |
| `src/http_client.rs` | JSON whitespace compression and outbound helpers | May parse/serialize a body already processed elsewhere |
| `src/common/auth.rs` | Request API key store and constant-time checks | Access control for one trust domain |

The infrastructure layer imports Anthropic usage, model, pricing, external-pool, and credential types. Those higher-level modules also directly own concrete stores, producing bidirectional compile-time coupling.

## Test Surfaces

- The repository contains extensive Rust unit and integration-style tests attached to modules.
- Large sidecar suites exist for handlers, token manager, external pools, Admin service, and loadtest behavior.
- Real PgSQL/Redis tests are required by current CI configuration.
- `src/bin/kiro_loadtest.rs` provides fake-upstream, latency, error, load, chaos, and resource scenarios.
- There is no `benches/` performance suite or performance regression gate.
- No frontend `test`/`spec` suites were found for either maintained React UI; current frontend gates are builds and a handwritten contract comparison.

## High-Blast-Radius Files

File size alone is not a defect, but these files combine enough responsibilities that a small change can cross unrelated behavior:

| File | Current total lines | Main reason for high blast radius |
| --- | ---: | --- |
| `src/storage/postgres.rs` | 11,372 | All durable domains, migrations, queries, and embedded tests |
| `src/kiro/token_manager/manager.rs` | 8,178 | Scheduler state, persistence, refresh, Admin, queues, cross-process coordination |
| `src/anthropic/handlers.rs` | 7,025 | Full Messages request lifecycle and both response modes |
| `src/admin/service.rs` | 6,602 | Entire control plane behind one service |
| `src/storage/redis_cache.rs` | 6,357 | All Redis coordination and derived data domains |
| `src/model/config.rs` | 5,529 | All runtime configuration and patches |
| `src/external_pool.rs` | 4,978 | Selection, transport, streaming, retry, usage, billing |

The target module design is intentionally not defined here. See the registered [Greenfield AI Gateway plan](../plans/greenfield-ai-gateway/README.md) for target ownership and dependency direction; the Rust modernization plan is historical reference.
