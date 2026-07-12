# Current Risk Hotspots

Role: Project-wide current-risk summary

Status: Current audit snapshot

Authority: Concise current-state risk map; stable finding IDs and acceptance live in the modernization problem catalog

As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-12

Read when: Determining which detailed finding or target topic to open

Related: [Problem catalog](../plans/system-architecture-modernization/topics/problems/README.md), [Resource model](resource-and-concurrency-model.md), [Storage and state](storage-and-state.md)

## Severity Position

No current finding has enough evidence for P0. The service has successful protocol, storage, shutdown, and moderate-load evidence. The highest-priority risks are verified correctness, filesystem-lifecycle, consistency, public-contract, SSRF, and aggregate-resource defects; performance magnitude still requires controlled benchmark and dependency-latency evidence.

## Verified P1 Hotspots

| Finding | Current behavior | Primary evidence |
| --- | --- | --- |
| `COR-001` | Tool-format diagnostics and request-body capture default on; files roll but directory retention is unbounded | `src/model/config.rs:3374-3431`, `src/anthropic/tool_format_debug.rs:369-492` |
| `COR-002` | Runtime config is cloned and stored as a full document without expected-version CAS | `src/kiro/token_manager/manager.rs:1246-1268`, `src/storage/postgres.rs:390-408` |
| `COR-003` | Redis usage snapshot, seen marker, and aggregate update are separate operations | `src/storage/redis_cache.rs:685-716` |
| `COR-004` | External `preservePath` is exposed but `_endpoint` is ignored | `src/external_pool.rs:3400-3405` |
| `SEC-001` | DNS safety lookup is independent from reqwest's actual connection lookup | `src/anthropic/body_processing.rs:317-325`, `557` onward |
| `RES-001` | Remote source limit is per item only; request/global materialization budgets are absent | `src/anthropic/body_processing.rs:15`, `139-150`, `255-270`, `377-425` |
| `RES-002` | Files live bytes/count are bounded, but explicit delete leaves IDs in the FIFO order queue | `src/anthropic/files.rs:87-126` |
| `HA-001` | Admin key and selected catalogs do not fully converge across replicas | `src/main.rs:460`, `933-937`, `src/admin/handlers.rs:1059-1068` |

`HA-001` is P1 only when multiple replicas are a supported deployment. It becomes P2 for an explicitly guaranteed single-process deployment. This classification is unrelated to user count.

## Resource, Security, Migration, And Redis Hotspots

| Finding | Severity | Current behavior | Primary evidence |
| --- | --- | --- | --- |
| `RES-003` | P1 | Kiro/external non-stream and error paths can collect a complete upstream response with a time limit but no response-byte ceiling | `src/http_client.rs:107-131`, `src/anthropic/handlers.rs:6317-6347`, `src/external_pool.rs:1753-1758`, `1994-2049` |
| `RES-004` | P1/P2 conditional on proxy/config churn | Kiro clients are cached by complete proxy configuration without capacity, TTL, idle retirement, or deletion invalidation, retaining old pools and proxy secrets until exit | `src/kiro/provider.rs:76-83`, `947-960` |
| `RES-005` | P1/P2 conditional on workload/host | Local and external global concurrency/wait-queue defaults use `0 = unlimited`, so a burst or slow upstream can make the host the admission limit | `src/model/config.rs:2294-2307`, `2390-2397`, `2703-2709`, `3668-3670`; `src/kiro/token_manager/manager.rs:2027-2040` |
| `SEC-004` | P1/P2 conditional on enabled pool destinations | External-pool URLs accept HTTP(S), but no owner binds DNS/IP approval to the actual connection or defines safe redirect and cross-origin credential behavior | `src/storage/postgres.rs:6083-6101`, `src/external_pool.rs:1235-1243`, `1698-1727`, `3380-3415` |
| `SEC-005` | P2 | Ordinary Admin reads return reusable keys/proxy passwords and both maintained UIs retain the reusable Admin key in JavaScript-readable `localStorage` | `src/admin/types.rs:393-403`, `987-1000`, `1366-1386`, `1410-1418`; `src/admin/service.rs:432-456`; `admin-ui/src/lib/storage.ts`, `ui/src/lib/storage.ts` |
| `OPS-005` | P2; release-blocking before the first modernization schema slice | Normal startup runs mutable inline schema, semicolon-split non-atomic statements, checksum overwrite, and table-wide backfill work | `src/model/config.rs:2455-2474`, `src/storage/postgres.rs:200-220`, `280-353`, `3437-3447`, `6793-6810`, `7032-7283` |
| `PERF-009` | P1/P2 conditional on stale-lease cardinality | Local/external acquire Lua reads and loops over every expired member without a per-invocation batch limit, blocking Redis command progress proportionally to stale-set size | `src/storage/redis_cache.rs:2538-2566`, `2681-2691` |

The conditional classifications identify where production magnitude still needs workload or deployment evidence; they do not retract the verified unbounded mechanism.

## Architecture And Performance Hotspots

| Area | Verified mechanism | What still needs measurement |
| --- | --- | --- |
| Credential scheduler | One broad credential mutex, repeated candidate/lease scans, mixed PgSQL/Redis/refresh/Admin ownership | Lock wait and scheduler p99 at 10/100/1,000 credentials |
| Request completion | PgSQL runtime mutation and Redis critical lease release can execute through sync bridges | p95/p99 amplification at 20/100/500ms dependency latency |
| External availability | Direct/preflight and per-pool state can produce repeated PgSQL/Redis calls | Commands/request as pool count grows |
| Runtime config | Large overlapping `Config`, `AppState`, and request copies; more than one snapshot read | Allocations and mixed-version reproduction |
| Payload/cache | Repeated JSON cloning, canonicalization, sizing, tokenization, serialization | CPU/RSS for 1/20-MiB and tool-heavy requests |
| Usage | Batches still perform substantial per-record/per-rollup work | SQL statements/event, pool wait, writer backlog and request tail |
| PDF/tokenizer | PDF work is serialized by a standard mutex; configured remote tokenizer uses a blocking bridge | Tokio heartbeat, unrelated request p99, cancellation |
| Kiro HTTP | Explicit `Connection: close` prevents ordinary HTTP/1.1 reuse | Real Kiro compatibility and handshake A/B |

These costs are not evidence that the current service is universally slow. They are evidence that dependency latency and higher scale can be amplified by the current architecture.

## Compatibility Hotspots

- Claude Code requires stable SSE event order, final non-zero usage, thinking/signature behavior, tool pairing, Files, MCP/tool/search workflows, model aliases, and normalized errors.
- Kiro IDE and CLI have distinct envelopes and parser behavior.
- External raw passthrough must remain byte-preserving and must not enter normalized/local processing.
- Cache simulation, actual upstream usage, downstream projection, and billable usage must remain distinguishable.
- Payload shaping must invalidate token/cache facts derived from an older body revision.
- An SSE response that has started cannot safely switch upstream.

## Operational Hotspots

- Compose checks TCP instead of `/readyz`.
- Usage/storage abandonment can be logged without independently forcing non-zero process exit.
- Usage cleanup job state is process-local; audit writes are not uniformly supervised.
- `OPS-004`: PgSQL backup/restore, Redis rebuild-by-key-class, previous-binary/expanded-schema compatibility, and disaster-recovery exercises are not versioned runbooks.
- Two handwritten frontend contracts have no Rust-derived authority and neither frontend has automated behavior tests.
- `TEST-002`: Performance evidence is extensive but not a continuous regression gate.
- `TEST-003`: Existing evidence does not prove the target real ccman/Claude Code three-session, 20+ conversational-turn workflow matrix.
- Ignored `target/loadtest` data is too ephemeral to be the only durable proof and has historically accumulated many files.
- SBOM, signing, and provenance are not part of the release artifact set.

## Bounded Or Retracted Concerns

- The system is single-user and single-trust-domain. There is no tenant isolation requirement.
- Files live payload is bounded at 50 MiB/file, 128 files, and 256 MiB total. The FIFO ordering metadata is not fully bounded under explicit-delete churn; that defect is `RES-002`.
- JSON request depth has an independent 192-level scan before unbounded-depth serde parsing.
- Storage, usage, debug writer, cache, and Files data structures have important local bounds; risks arise from fallback/lifetime/aggregate gaps rather than every structure being unbounded.
- Ordinary SSE transformation does not use a known unbounded intermediate channel.
- Production container/TLS/Admin isolation/database-secret items remain the explicitly deferred `6.P1` scope, not new findings.

## Retrieval

Use the [problem catalog](../plans/system-architecture-modernization/topics/problems/README.md) for stable IDs, detailed evidence, required target behavior, and acceptance conditions. Do not close a risk in this baseline; close it through the owning roadmap item and durable evidence, then refresh this current-state snapshot.
