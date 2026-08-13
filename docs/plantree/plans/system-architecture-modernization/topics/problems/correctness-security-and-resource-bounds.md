# Correctness, Security, And Resource Bounds

Role: Detailed verified-finding analysis
Status: Open findings
Authority: Evidence and acceptance conditions for the listed IDs
As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11
Read when: Implementing target modules or reviewing correctness/security/resource changes
Related: [Problem index](README.md), [Requirements](../requirements-and-quality-attributes.md), [State design](../architecture/state-ownership-and-consistency.md), [Repository hygiene](../delivery/repository-cleanup-and-filesystem-plan.md), [Decision 010](../../decisions/010-fixed-operational-and-acceptance-policies.md), [Decision 011](../../decisions/011-explicit-secret-envelope-and-resource-governor-authorities.md), [Decision 012](../../decisions/012-tool-definition-compatibility-and-reversible-schema-mapping.md), [Decision 014](../../decisions/014-release-generation-recovery-and-rollback-state.md)

## `COR-001`: Default Request-Body Diagnostics Have Unbounded Directory Lifetime

Severity: P1
Technical authority area: diagnostics, configuration, filesystem lifecycle

### Evidence

- `default_tool_format_debug_enabled()` returns `true`: `src/model/config.rs:3374`.
- `default_tool_format_debug_capture_request_body()` returns `true`: `src/model/config.rs:3414`.
- One captured body may contain up to 384 KiB: `src/model/config.rs:3418`.
- One rolled file may reach 100 MiB: `src/model/config.rs:3430`.
- The writer rolls files by interval and sequence and appends JSONL: `src/anthropic/tool_format_debug.rs:369-492`.
- Captured body text is stored in a `content` field: `src/anthropic/tool_format_debug.rs:649-689`.
- The module has no production retention worker enforcing total directory bytes, file count, or maximum age.

### Impact

The write rate is bounded, but runtime duration is not. Long-running deployments can accumulate an arbitrary number of files. Diagnostic records may contain prompts, tool content, or other sensitive operator data. The risk exists in a single-user deployment because it concerns disk exhaustion and secret retention, not cross-user access.

### Required Target

- recorder and body capture disabled by default;
- metadata-only structured diagnostics in ordinary operation;
- explicit break-glass enablement with automatic expiry;
- field allowlist/redaction and secret scanning;
- `0600` files inside a validated, non-symlink root;
- simultaneous limits for record bytes, file bytes, total bytes, file count, and age;
- stop/drop with metrics at quota rather than creating another file;
- startup and periodic retention enforcement.

### Acceptance

- Default configuration produces no tool-format JSONL after representative errors.
- A capture-enabled test reaches the configured quota without exceeding it or creating unbounded files.
- Restart applies retention before accepting new diagnostic records.
- Fixtures containing API keys, cookies, refresh tokens, prompt text, tool results, and images are either excluded, redacted, or hashed according to the allowlist.

## `COR-002`: Runtime Configuration Updates Can Lose Concurrent Changes

Severity: P1
Technical authority area: runtime configuration, Admin control plane, PgSQL

### Evidence

`MultiTokenManager::update_runtime_config` clones the complete current `Config`, mutates it, persists a complete clone, and then replaces local state: `src/kiro/token_manager/manager.rs:1246-1268`.

`PostgresStore::save_runtime_config_returning_version` increments `version` on conflict but has no `WHERE version = expected_version` predicate: `src/storage/postgres.rs:390-408`.

### Failure Scenario

1. Request A reads version 10 and changes cache policy.
2. Request B reads version 10 and changes an API key.
3. A stores its complete document as version 11.
4. B stores its stale complete document as version 12.
5. B silently restores the old cache policy while successfully changing the key.

This can occur with one operator issuing concurrent Admin actions, with background updates, or across replicas.

### Required Target

- typed field/domain patches rather than closure mutation of a complete config;
- expected-version CAS in the PgSQL transaction;
- HTTP `409 Conflict` containing current version but no sensitive config values;
- optional explicit reread/reapply for commutative patches;
- durable change event after commit, Redis notification, and periodic version reconciliation;
- one immutable request snapshot version.

### Acceptance

- A deterministic concurrent-update test proves that two writes from one version cannot both silently succeed.
- Disjoint patches either serialize safely or one returns a conflict and can be reapplied.
- Every request trace contains exactly one config version.
- Event loss followed by periodic polling still converges all healthy replicas.

## `COR-005`: One Request Can Mix Runtime Policy Versions

Severity: P1

Technical authority area: request entry, runtime snapshot publication, handler orchestration

### Evidence

- Messages request entry obtains a runtime configuration near `src/anthropic/handlers/request_entry.rs:12`.
- The main parsed request flow obtains runtime configuration again near `src/anthropic/handlers.rs:4305`.
- Runtime configuration can be replaced concurrently through Admin update/reload.

No request-scoped immutable version is passed through every stage. A request can therefore use one version for early raw/external or missing-token decisions and another version for model, cache, payload, retry, or external behavior.

### Impact

This is separate from clone cost (`PERF-004`) and from lost updates (`COR-002`). Even when every Admin write is CAS-safe, a single in-flight request can still apply an incoherent combination of two valid versions.

### Required Target

- capture one immutable `RuntimeSnapshot` at authenticated request entry;
- store its version in `RequestEnvelope` and every attempt/usage trace;
- pass it explicitly to raw facts, routing, processing, scheduler policy, response, usage, and diagnostics;
- forbid downstream runtime reload calls in the request path through interface/dependency tests.

### Acceptance

- A barrier-controlled test updates config between early entry and parsed processing and proves the request remains entirely on its original version.
- Instrumentation reports exactly one snapshot acquisition and one version per request.
- Raw, local, external, stream, non-stream, count-tokens, and Files-materialization paths are covered.

## `COR-003`: Redis Usage Deduplication And Aggregation Are Not Atomic

Severity: P1
Technical authority area: usage, Redis adapter, derived dashboards

### Evidence

`record_usage_summary` performs:

1. a record snapshot write at `src/storage/redis_cache.rs:685-690`;
2. an independent `SET ... NX EX` seen marker at `src/storage/redis_cache.rs:692-704`;
3. a later atomic aggregate pipeline beginning at `src/storage/redis_cache.rs:716`.

If step 2 succeeds and step 3 fails, retry returns early because the marker exists. The aggregate remains short by one event for at least the marker/cache lifetime, and there is no complete authoritative rebuild workflow.

### Required Target

- one Lua operation that checks event ID and updates all aggregates atomically; or
- derive Redis dashboards asynchronously from idempotent durable PgSQL usage events;
- stable `event_id` uniqueness and replay metrics;
- a rebuild/reconciliation command that never treats Redis as the primary usage fact.

### Acceptance

- Fault injection at every Redis command boundary results in either all increments or none.
- Replaying the same event one or many times produces one aggregate contribution.
- Rebuilding from PgSQL matches expected dashboard totals and cache-read buckets.

## `COR-004`: `preservePath` Does Not Affect External Requests

Severity: P1
Technical authority area: external pool URL policy and API/UI contract

### Evidence

- The UI exposes “保留请求路径” and describes its behavior: `ui/src/features/external-pools/external-pool-form-modal.tsx:162-165`.
- The field is persisted on external pool DTOs.
- `external_pool_url(pool, _endpoint, config)` ignores `_endpoint` and delegates to a fixed Messages URL: `src/external_pool.rs:3400-3405`.

### Impact

Requests arriving through `/cc`, `/ha`, `/na`, or `/dfcache` can lose path-specific semantics at an external upstream even when the operator enabled path preservation. Existing tests that expect the endpoint to be ignored protect the defect rather than the advertised contract.

### Required Target

- define a canonical validated inbound path type;
- when enabled, join normalized base URL and the allowed route path without path traversal or duplicated `/v1` segments;
- when disabled, use the documented canonical external Messages endpoint;
- do not infer usage projection from body mode or path preservation.

### Acceptance

E2E tests cover enabled/disabled behavior for `/v1`, `/cc/v1`, `/ha/v1`, `/na/v1`, and a configured `/dfcache/{route}/v1`, including base URLs with and without a trailing `/v1`.

## `COR-006`: Tool Boundary Values Cause Avoidable Request-Wide 400s

Severity: P1/P2; the source mechanism and local reproduction are verified, while the reported production-frequency total still needs a secret-safe versioned evidence summary

Technical authority area: public protocol parsing, profile-specific payload normalization and contract validation

### Evidence

- `Tool.description` defaults a missing field to an empty string and `input_schema` is a non-optional map with only `#[serde(default)]`: `src/anthropic/types.rs:314-326`.
- Explicit JSON `null` therefore fails map deserialization, while a missing `input_schema` becomes an empty map.
- The Kiro converter copies description unchanged and performs only suffix/length handling before sending it upstream: `src/anthropic/converter/tools.rs:293-337`.
- [The retained reproduction report](../../../../../../feature/issues/empty-tool-description-400-invalid-tool-use-format.md) records deterministic empty/missing-description and explicit-null cases plus a recent production sample summary. Its remediation proposal is non-binding; decision 012 owns the accepted behavior.

The defect is shared by every route that enters the same typed request parser. It is distinct from payload-size failures: small requests can fail before or at the first upstream attempt solely because a boundary value was not given a profile-specific meaning.

### Required Target

- preserve raw external request bytes and do not parse/repair tool definitions for that profile;
- after target/profile selection, normalize a missing/blank description to a deterministic neutral nonempty value for Kiro/local and explicitly normalized external profiles;
- treat absent and explicit-null `input_schema` identically as the accepted empty object schema for those normalized profiles, while rejecting other malformed non-map values locally;
- record bounded repair/rejection reason counters without raw schema/description content;
- ensure the public parser does not force normalized semantics on raw passthrough.

### Acceptance

- Missing, empty, whitespace and normal descriptions plus absent/null/map/non-map schemas run across every public route and raw/normalized target profile.
- Raw-mode body bytes remain identical and no unselected profile performs tool conversion.
- Normalized/local repair reaches deterministic fake upstream successfully; malformed unsupported values return one stable local error before any upstream attempt.
- Real-client evidence is bounded and independent; no production traffic mirror or duplicate POST comparison is used.

## `COR-007`: Tool Property-Key Compatibility Has No Reversible Mapping Contract

Severity: P1/P2 conditional on client schema; upstream rejection and current pass-through are verified, while traffic frequency depends on tool providers

Technical authority area: Anthropic/Kiro/external tool schema codecs, payload policy and response reverse translation

### Evidence

- `normalize_properties` recursively normalizes property values but never validates or maps object property names: `src/anthropic/converter/schema.rs:287-311`.
- [The retained reproduction report](../../../../../../feature/issues/tool-property-key-invalid-400-tool-schema-invalid.md) records Kiro/Anthropic-compatible upstream rejection for names outside `^[a-zA-Z0-9_.-]{1,64}$`.
- The report's original blanket replacement suggestion is unsafe as written: two names may collide, `required`/dependency keywords may diverge, and returned `tool_use.input` keys may no longer match the downstream client's names. `patternProperties` keys are regular expressions and `$defs` keys are definition identifiers, not ordinary property names.

### Required Target

- valid names remain byte-identical;
- normalized target capability validation runs before an upstream attempt;
- repair is permitted only through the decision-012 deterministic one-to-one map, complete property-reference updates and streaming/non-streaming reverse translation;
- nested properties receive scoped maps; regex/definition/reference/dynamic-key semantics are preserved or cause a stable local rejection when a round trip cannot be proved;
- raw external passthrough never receives this normalization.

### Acceptance

- Valid, invalid, empty, long, Unicode and colliding names cover nested objects, `required`, `dependentRequired`, `dependentSchemas`, legacy `dependencies`, `$defs`, `$ref`, union/recursive and dynamic-property cases.
- Every accepted mapped request returns original downstream argument keys for stream and non-stream output; tool-use/result pairing remains valid.
- Unsupported semantics fail locally before upstream execution and expose only an error ID plus bounded reason.
- Claude Code tool/MCP/multi-agent workflows and target-specific fake upstreams pass without lossy rename.

## `SEC-001`: DNS Rebinding Window In Remote Source Fetching

Severity: P1
Technical authority area: remote media adapter, resolver, HTTP transport

Current Rust containment: the 2026-07-16 dirty tree replaces the independent pre-resolution with a filtering resolver used by the actual reqwest transport, disables inherited system proxies and revalidates every redirect. Focused transport tests pass; handler/load/release-candidate evidence remains open in [the active feature issue](../../../../../../feature/issues/remote-multimodal-resource-and-ssrf-bounds.md). The greenfield target requirements below remain authoritative for the later architecture.

### Evidence

- The code calls `ensure_safe_remote_url_resolves` before sending: `src/anthropic/body_processing.rs:317-319`.
- That function parses and resolves the host independently: `src/anthropic/body_processing.rs:557` onward.
- reqwest then resolves the hostname again when `client.get(...).send()` runs: `src/anthropic/body_processing.rs:321-325`.

The checked IP is not bound to the actual connection. An attacker-controlled hostname can return a public address during validation and a blocked/private address during connection. Post-response checks do not prevent the connection from already reaching the unsafe target.

### Required Target

- parse and validate scheme/host/port;
- resolve once per connection attempt;
- reject loopback, private, link-local, unspecified, multicast, and metadata ranges;
- bind the validated address to the actual connection/resolver result while preserving host/SNI validation;
- repeat the complete procedure for every redirect;
- define behavior when an HTTP/SOCKS proxy performs DNS resolution remotely.

### Acceptance

- deterministic resolver tests simulate address changes between lookups and prove the connection cannot switch to a blocked address;
- redirect tests reject a safe-to-private transition;
- IPv4, IPv6, mixed-record, encoded host, alternative port, and proxy modes are covered;
- no response bytes are required before rejection.

## `RES-001`: Remote Multimodal Work Has No Aggregate Or Global Budget

Severity: P1
Technical authority area: body processing, media fetch, resource governance

Current Rust containment: the 2026-07-16 dirty tree adds source-count, aggregate downloaded/base64, shared HTTP-attempt, workflow-deadline and four-workflow process bounds. This closes the unbounded current-version path at module level but does not replace the target container-aware weighted resource governor; final handler/load/RSS evidence remains open in [the active feature issue](../../../../../../feature/issues/remote-multimodal-resource-and-ssrf-bounds.md).

### Evidence

- Safe mode enables remote source download by default: `src/model/config.rs:86-93`.
- A new reqwest client is built per request: `src/anthropic/body_processing.rs:139-150`.
- Content items are processed serially: `src/anthropic/body_processing.rs:255-270`.
- Each source has a 20-MiB limit: `src/anthropic/body_processing.rs:15`.
- Download bytes are accumulated before base64 materialization: `src/anthropic/body_processing.rs:377-425`.
- There is no source-count, aggregate-download, aggregate-transformed-byte, or global in-flight media budget.
- `count_tokens` can enter the same preprocessing path.

The 50-MiB inbound HTTP body limit does not constrain many short URLs that later expand into hundreds of MiB of downloaded and base64-encoded data.

### Required Target

- per-request maximum source count;
- per-source and aggregate downloaded bytes;
- aggregate transformed/materialized bytes;
- media-fetch deadline distinct from model-upstream timeouts;
- shared client and fixed-address resolver;
- global weighted semaphore for in-flight downloaded/transformed bytes;
- separate bounded blocking pools for PDF and tokenization;
- rejection before allocation when a budget cannot be acquired.

The initial hard limits in accepted decisions 010 and 011 and the target resource model are binding. Compatibility fixtures verify their behavior and may justify a safely lower limit, but they do not delay acceptance or permit an implementation to silently raise, disable, or remove a bound.

### Acceptance

- Many-source tests cannot exceed the request/global budget even when all sources stay below 20 MiB.
- Concurrent media/count-token requests remain within the accepted RSS envelope.
- Budget exhaustion returns a stable error, leaves no partial cache/file entry, and releases all permits.
- Slow source fetches do not consume model-upstream timeout allowance.

## `RES-002`: Files Delete Leaves An Unbounded FIFO Tombstone Queue

Severity: P1

Technical authority area: Files-compatible staging and process-local resource governance

### Evidence

- Insert appends every new ID to `FileStoreInner.order`: `src/anthropic/files.rs:87-90`.
- Live-file/byte eviction pops from the front only while `files.len() > 128` or `total_bytes > 256 MiB`: `src/anthropic/files.rs:92-102`.
- Explicit delete removes the object from `files` and subtracts bytes but does not remove its ID from `order`: `src/anthropic/files.rs:111-117`.
- List scans the complete `order` queue and filters tombstones through the live map: `src/anthropic/files.rs:120-126`.

### Failure Scenario

A client repeatedly uploads a small file and deletes it before the next upload. Live file count and bytes return below their limits after each delete, so insertion does not enter the eviction loop and old IDs remain in `order`. Memory and list-scan time grow with total historical uploads even though live content remains near zero.

This corrects the earlier broad statement that the complete Files store was bounded. Only live payload bytes and live map entries are bounded in the current implementation.

### Required Target

- use one ordering/index structure whose delete operation removes both payload and ordering entry, or compact tombstones under a strict metadata bound;
- expose live entries, ordering entries/tombstones, live bytes, evictions, and compactions;
- define Files TTL, restart, and replica behavior through `FileObjectStore`;
- perform metadata cleanup without scanning unbounded history on every request;
- R0 creates the deterministic tombstone/regression fixture, and `MOD-FILES` implements the final shared bounded behavior once during R6; the modernization adds no legacy production containment patch or temporary runtime wrapper.

### Acceptance

- Repeated small upload/delete cycles leave ordering metadata within a constant bound relative to live entries.
- List latency and retained memory do not grow monotonically after all files are deleted.
- Insert, FIFO eviction, explicit delete, restart, cancellation, and concurrent access preserve byte/count accounting.
- The test uses aggregate metrics rather than writing one artifact per upload.

## `RES-003`: Complete Upstream Responses Have No Byte Budget

Severity: P1

Technical authority area: Kiro/external upstream adapters, response processing, request resource governance

### Evidence

- The shared helpers apply only a body-read timeout before calling reqwest `text()` or `bytes()`: `src/http_client.rs:107-131`.
- The Kiro non-stream path collects the complete response before event-stream decoding: `src/anthropic/handlers.rs:6317-6347`.
- External-pool error responses are collected completely before classification: `src/external_pool.rs:1753-1758`.
- External-pool successful non-stream responses are collected completely before HTML/error detection and usage projection: `src/external_pool.rs:1994-2049`.

The inbound request limit and remote-media source limit do not bound bytes returned by a model/provider endpoint. A chunked response or a response without `Content-Length` can grow resident memory until the timeout, allocator, process, or host becomes the effective limit.

### Required Target

- define per-profile success, error and streaming response byte budgets as part of the captured request resource view;
- reject an already oversized `Content-Length` before allocation and enforce the same ceiling incrementally when length is absent or dishonest;
- retain only a small bounded error prefix needed for safe classification and never include the complete body in logs or public errors;
- count bytes while translating/passing through streams and define an accepted long-stream policy that does not use unlimited output as availability;
- release leases, permits, response buffers and connections on limit, timeout, cancellation and downstream disconnect.

### Acceptance

- Fake-upstream tests cover oversized success/error bodies, chunked/no-length bodies, slow bodies, declared-length mismatch and cancellation for Kiro and external profiles.
- Rejection occurs at the accepted byte boundary and returns a stable normalized error without echoing body content.
- Peak/end/idle RSS, FD, task, connection and lease metrics remain inside the accepted absolute/recovery envelope.
- Normal long Claude Code streams within the accepted profile remain compatible.

## `RES-004`: Kiro HTTP Client Cache Retains Every Proxy Configuration

Severity: P1/P2 conditional on proxy/configuration churn

Technical authority area: Kiro upstream transport and reusable-client lifecycle

### Evidence

- `KiroProvider` stores `HashMap<Option<ProxyConfig>, Client>` behind a mutex: `src/kiro/provider.rs:76-83`.
- `client_for` inserts a client for every previously unseen effective proxy and has no capacity, TTL, idle retirement or credential-deletion path: `src/kiro/provider.rs:947-960`.
- `ProxyConfig` is the map key, so obsolete usernames/passwords remain reachable with their old connection pools until process exit.

### Required Target

- own clients in `MOD-KIRO-UPSTREAM` behind a bounded cache with capacity, idle TTL, active-reference protection and deterministic eviction;
- key clients by a non-secret canonical transport identity while keeping credentials in secret/redacted types;
- deduplicate concurrent construction for the same identity and retire clients after proxy rotation or credential deletion when no active request references them;
- expose entry, active, idle, construction, hit/miss, eviction and retirement metrics without proxy credentials or URLs as labels.

### Acceptance

- Rotate/add/delete 1,000-10,000 synthetic unique proxy configurations and prove cache entries, RSS, FDs and connections return to accepted bounds.
- Concurrent requests for one key construct at most the accepted number of clients and active requests are not evicted prematurely.
- Deleted proxy secrets are not recoverable from cache keys, logs, diagnostics or retained evidence.

## `RES-005`: Default Global Admission And Wait Queues Are Unlimited

Severity: P1/P2; the high-amplification mechanism is verified and production magnitude depends on workload/host

Technical authority area: request resource admission and local/external schedulers

### Evidence

- Local dispatch global concurrency and queue fields define `0` as unlimited: `src/model/config.rs:2703-2709`.
- Their defaults are both zero: `src/model/config.rs:3668-3670`.
- External-pool global concurrency and maximum queued requests also default to zero: `src/model/config.rs:2294-2307`, `2390-2397`.
- Local admission rejects only when `max_queued > 0`: `src/kiro/token_manager/manager.rs:2027-2040`; the Redis admission script uses the same condition: `src/storage/redis_cache.rs:178-201`.

A finite wait timeout is not a finite admission bound. Under a burst or slow upstream, request tasks, parsed bodies, queue/lease metadata and connections can accumulate until an unrelated downstream or host limit fails first.

### Required Target

- every supported production profile has finite process-wide admitted, in-flight and queued request ceilings for local and external paths;
- admission occurs before expensive parsing/materialization where protocol behavior permits and uses stable overload errors plus `Retry-After` policy;
- `0 = unlimited` is allowed only in an explicitly named development/unsupported profile and readiness discloses that state;
- queue, request-byte, task and connection budgets compose rather than each owner independently accepting the same request;
- final fairness, priority and lease timing remain owned by the scheduler decision, not by a generic global semaphore.

### Acceptance

- Slow-upstream and sudden-burst workloads fill the accepted ceiling, shed excess requests predictably and never exceed absolute queue/task/RSS/FD/connection bounds.
- Cancellation, timeout, disconnect, Redis restart and shutdown return every queue/admission slot within the accepted recovery deadline.
- Local, external, fallback and rescue routes cannot bypass the process-wide admission budget.

## `HA-001`: Selected Runtime State Does Not Converge Across Replicas

Severity: P1 in the supported multi-replica production profile
Technical authority area: Admin authentication, catalogs, runtime events, release-generation readiness

### Evidence

- `AdminState` receives an in-memory key during startup: `src/main.rs:460`.
- Admin key update changes only the state serving the current request: `src/admin/handlers.rs:1059-1068`.
- Runtime config event reload replaces request API keys but does not update `AdminState`: `src/main.rs:933-937`.
- Model capability and pricing Admin changes update current catalogs/PgSQL without one complete replica broadcast contract.

### Required Target

Decisions 010 and 014 fix multi-replica production as a supported mode. The target must:

- version Admin auth state and catalogs independently from the broad config document;
- read from atomically replaceable shared auth/catalog snapshots;
- commit durable version changes, publish lossy wakeups, and reconcile periodically from durable authority;
- fence each release generation to its signed expected replica membership and required digest/config/schema/auth/catalog versions;
- keep public readiness closed for missing, stale, duplicate or mismatched generation members and expose applied-version lag without secrets.

### Acceptance

- Rotate an Admin key while two expected replicas are live: both reject the old and accept the new within the accepted convergence interval.
- Drop the notification: periodic reconciliation still converges, and a stale member remains unready until it catches up.
- Catalog updates produce the same `/models`, routing eligibility, and pricing snapshot on every ready replica.
- Missing/replaced replicas and Redis-loss recovery cannot open readiness until the decision-014 generation barrier is satisfied.

## `HA-002`: Files Objects Are Process-Local Across Replicas

Severity: P1 in the supported multi-replica production profile; not a multi-user issue

Technical authority area: Files application service, PgSQL `FileObjectStore`, and deployment contract

### Evidence

`AnthropicFileStore` lives inside process `AppState`, has no PgSQL/Redis/object-store backing, and returns process-local IDs. Upload followed by Messages content materialization on another replica cannot find the object. Restart has the same availability effect.

### Required Target

Decision 010 fixes one shared, bounded PgSQL `FileObjectStore` for the supported production profile, with explicit count, byte, age, checksum, streaming, durability, cleanup, restart and cross-replica semantics. Sticky routing may reduce reads, but it is only an optimization and can never become the Files authority or a correctness requirement.

### Acceptance

- Upload/list/metadata/content/delete/materialization tests run across multiple expected replicas and restart/failover.
- A request never silently treats a missing cross-replica object as empty content.
- Quotas, TTL cleanup, checksum validation and concurrent delete/materialize behavior remain correct through PgSQL failure/recovery.
- Readiness/deployment docs describe the shared store and do not rely on affinity for correctness.

## `HA-003`: Prompt-Cache Evidence Is Replica-Local

Severity: P2 in the supported multi-replica production profile

Technical authority area: shared prompt-cache evidence and reported-usage projection

### Evidence

`PromptCacheTracker` and cache-creation control are stored in process `AppState`; they have no shared authority. Two replicas can observe different prefix history and emit different creation/read projection for equivalent conversation traffic, especially without session affinity.

### Required Target

Decision 010 fixes shared, bounded, versioned Redis prompt-cache evidence for the supported production profile. Actual upstream usage, local estimates and simulated projections remain explicitly labeled and cannot be conflated. Replica-local heuristics may assist non-authoritative optimization only; they never become durable or accounting facts.

### Acceptance

- Equivalent conversations crossing expected replicas produce the same shared evidence transition and labeled projection.
- Raw actual, estimated, reported, billable and simulated fields preserve their distinct provenance.
- Redis loss/rebuild, restart and failover do not fabricate authoritative cache hits; replica-local heuristics cannot populate authoritative fields.
- TTL, entry/byte capacity, atomic transition and rebuild behavior stay within accepted resource and recovery bounds.

## `SEC-002`: External Header Boundary Uses Broad Forwarding

Severity: P1/P2 pending exploitability/compatibility tests
Technical authority area: external HTTP adapter

### Evidence

Request and response header filtering in `src/external_pool.rs:3541-3575` is primarily exclusion-based. Headers outside the denylist may cross the provider boundary, including credentials or cookie-related vendor extensions not anticipated by the list.

### Required Target

- request allowlist for content negotiation, Anthropic version/beta fields, tracing/request IDs, and explicitly supported vendor headers;
- always remove downstream `Authorization`, `x-api-key`, `Cookie`, `Host`, proxy, and hop-by-hop fields;
- inject only the selected pool's credentials;
- response allowlist must exclude `Set-Cookie` and hop-by-hop fields;
- compatibility fixtures enumerate every intentionally forwarded header.

### Acceptance

Property/fixture tests inject common and arbitrary credential-shaped headers and prove only the documented allowlist crosses either direction.

## `SEC-003`: WebSearch And MCP Tracing Records Raw Operator Content

Severity: P1

Technical authority area: WebSearch/MCP adapter, tracing policy, diagnostic redaction

### Evidence

- The service defaults to an `info` tracing filter when no environment override is present: `src/main.rs:58-62`.
- WebSearch request handling records the complete extracted operator query at `info`: `src/anthropic/websearch.rs:475-486`.
- The complete serialized MCP request, including the WebSearch query, is recorded at `debug`: `src/anthropic/websearch.rs:516-520`.
- The complete MCP response body is also recorded at `debug` before parsing: `src/anthropic/websearch.rs:522-531`.
- These log calls do not use the bounded diagnostic recorder, field allowlist/redaction, content hash, length cap, automatic expiry, or secret-scanning path required for explicit sensitive capture.

### Impact

Ordinary default logs contain raw search queries, which can include operator prompts, filenames, project terms, URLs, tokens pasted into a query, or other sensitive content. Enabling debug expands exposure to the complete MCP request and upstream response, including search-result content and error text, and can amplify log volume. The risk exists in a single-user deployment because logs and collectors are independent retention/failure boundaries.

### Required Target

- ordinary info/debug tracing records only bounded metadata such as request ID, operation, query length or one-way bounded fingerprint, result count, status/error class, and latency;
- raw query, MCP request body, MCP response body, result snippets, prompt/tool/file content, credentials, cookies, and tokens never enter ordinary tracing;
- any explicitly approved content capture goes through the same break-glass, expiring, permission-restricted, quota-bound, allowlisted/redacted diagnostic path as other sensitive diagnostics;
- error reporting preserves a typed internal class and public error without embedding an upstream response body;
- the rewritten WebSearch/MCP adapter retains protocol behavior and observable metadata without content-bearing labels.

### Acceptance

- Info and debug log-capture tests submit unique query/request/response secret markers and prove none appears in logs, traces, metric labels, errors, or retained evidence.
- The same tests prove bounded request ID, status/error class, latency, query length/fingerprint, and result-count metadata remains available.
- A break-glass capture test, if the capability is retained, proves expiry, redaction, byte/file/age limits, permissions, secret scan, and cleanup.
- Real Claude Code WebSearch compatibility still passes through `G-CLI` without storing query or result bodies in durable evidence.

## `SEC-004`: External-Pool Egress Is Not Bound To A Safe Destination Policy

Severity: P1/P2 conditional; external pools are opt-in, but an enabled unsafe destination can reach protected networks

Technical authority area: external upstream transport, configured outbound URL policy

### Evidence

- External-pool input validation parses the URL and accepts any `http` or `https` target: `src/storage/postgres.rs:6083-6101`.
- `ExternalPoolManager` builds a reqwest client without an explicit redirect or resolver policy: `src/external_pool.rs:1235-1243`.
- The real request constructs the configured URL and sends the POST directly: `src/external_pool.rs:1698-1727`, `3380-3415`.
- No owner binds the validated DNS result to the connection, revalidates each redirect, restricts private/link-local/metadata destinations, or defines cross-origin body/auth behavior.

This is distinct from `SEC-001`, which covers remote media fetched from request content and is owned by `MOD-MEDIA`. External pools are provider endpoints configured by the operator, but configuration mistakes, imported state and compromised Admin/browser contexts still make the egress boundary security- and availability-relevant.

### Required Target

- `MOD-EXTERNAL-UPSTREAM` owns an explicit destination policy for initial URL, DNS result, actual connection, proxy DNS mode and every redirect;
- default production policy rejects loopback, private, link-local, multicast, unspecified and metadata ranges unless a separate supported local-pool deployment profile explicitly permits a narrow destination;
- redirect policy defines allowed status codes, method/body preservation, maximum hops, origin changes and credential/header stripping;
- the same audit covers every other configured outbound URL, including remote token counting and credential refresh, without creating one broad egress God service;
- logs and errors contain a bounded destination class/fingerprint, never credentials or complete sensitive URLs.

### Acceptance

- Tests cover safe-to-private/metadata redirects, DNS rebinding, IPv4/IPv6/mixed answers, encoded hosts, alternative ports, proxy DNS and redirect loops.
- 307/308 tests prove request bodies and pool credentials cannot cross to an unaccepted origin.
- Explicit local-pool support passes only through its accepted deployment policy; the default remains fail closed.

## `SEC-005`: Admin Reads And Browser Storage Retain Reusable Secrets

Severity: P2

Technical authority area: auth/secret contracts, Admin schema, both maintained frontends

### Evidence

- `AccessKeysResponse` and each `RequestApiKeyItem` serialize reusable request/Admin keys: `src/admin/types.rs:1366-1386`.
- The normal response builder clones every request key and the current Admin key into the response: `src/admin/service.rs:432-456`.
- Credential, proxy-resource and runtime-config responses can serialize proxy passwords: `src/admin/types.rs:393-403`, `987-1000`, `1410-1418`.
- Both maintained frontends store the reusable Admin key in JavaScript-readable `localStorage`: `ui/src/lib/storage.ts:1-13`, `admin-ui/src/lib/storage.ts:1-13`.

Admin authentication limits who may call the endpoint, but it does not make repeated plaintext recovery or long-lived browser storage harmless. XSS, browser extensions, screenshots, support capture, response logging and stale sessions can turn a read-only UI workflow into credential disclosure.

### Required Target

- technical-authority read contracts return only stable ID, mask/fingerprint, presence, version and lifecycle metadata;
- create/rotate/import may reveal a generated secret exactly once through an explicitly typed response, after which it is not recoverable through ordinary reads;
- Rust-derived schema marks submitted secret fields `writeOnly` and reveal-once fields separately from ordinary response DTOs;
- both UIs use an accepted session/auth design that does not leave a reusable Admin credential indefinitely in JavaScript-readable storage, and logout/revocation clears all retained state;
- proxy, credential and runtime-config owners use keep/replace/clear commands without echoing stored plaintext.

### Acceptance

- Ordinary GET/list/reload responses contain only masked/presence metadata; unique secret markers never appear in response snapshots, logs, browser artifacts or exported evidence.
- Create/rotate reveals once, reload cannot recover old plaintext, and revocation/session convergence follows the accepted decision-010 epoch and session policy.
- Generated-contract tests enforce `writeOnly`/reveal-once semantics and both frontends pass browser/CSP/secret-leak checks.

## `SEC-006`: Reusable Upstream Secrets Are Stored Without Application Encryption

Severity: P1/P2; database or backup disclosure exposes reusable upstream/proxy credentials, while exploitability depends on deployment access controls

Technical authority area: secret-envelope mechanics, credential/proxy/pool/runtime secret authorities, migrations and recovery

### Evidence

- Credential structs, including reusable tokens/keys, are serialized into plaintext JSONB `credentials.data`: `src/storage/postgres.rs:1028-1054`, with the column defined at `src/storage/postgres.rs:6458-6469`.
- Reusable proxy passwords are ordinary `TEXT` and are bound directly on insert: `src/storage/postgres.rs:6511-6528`, `1597-1619`.
- External-pool API keys are ordinary required `TEXT` and are returned/read as plaintext: `src/storage/postgres.rs:6549-6554`, `486-523`.
- No first-party AEAD/envelope/key-provider implementation or crypto dependency exists in the current runtime.

Hash columns used for duplicate detection do not protect the reusable value stored elsewhere. Database snapshots, WAL, support queries or a read-only SQL compromise can expose credentials that remain valid outside this process.

### Required Target

- `MOD-SECRET-ENVELOPE` exclusively implements the decision-011 versioned AEAD/key-provider/rewrap contract;
- API/Admin request keys that are only verified store a constant-time versioned keyed verifier and fingerprint, never reversible ciphertext or recoverable plaintext after create/rotate;
- credentials, proxy passwords, external-pool keys and runtime secrets that must be replayed to an upstream store only envelope ciphertext plus key/version metadata;
- domain owners retain lifecycle/CAS/audit authority and execute bounded legacy adoption/rewrap through immutable migrations/jobs;
- key material has independent off-host recovery and restore drills; evidence never contains key bytes/plaintext;
- rollback-window plaintext compatibility projection is explicit, frozen/bounded and deleted by the post-window contraction gate.

### Acceptance

- Database, WAL-derived restore, Admin reads, logs and evidence contain no reusable plaintext after contraction; unique secret markers appear only inside encrypted blobs.
- Tamper, wrong associated data/key, mixed key versions, crash-resume rotation and backup-key restore behave fail closed without orphaning usable ciphertext.
- Request/Admin key creation reveals once; ordinary reads and database contents cannot recover it, and constant-time verification/revocation works across replicas.
- Previous-binary rollback-window probes and final plaintext-residue-zero searches both pass at their distinct gates.

## `REL-002`: Ambiguous External POST Send Errors Can Be Retried

Severity: P1/P2; duplicate execution/cost is plausible but not reproduced against every upstream

Technical authority area: external transport delivery state and retry policy

### Evidence

- `forward_once` maps any reqwest send error to a retryable network error: `src/external_pool.rs:1727-1751`.
- The external attempt loop records `retry_next` for retryable errors and can select another pool: `src/external_pool.rs:1539-1603`.
- A send/body/connection error after request bytes reached the upstream cannot prove the upstream did not accept or begin model execution.

### Impact

The gateway can send an equivalent model POST to another pool after the first upstream may already be executing it. This can duplicate cost and side effects even though only one response reaches the client.

### Required Target

- represent upstream delivery certainty independently from downstream response commitment;
- retry only when delivery is known `NotSent`, or when an effective upstream idempotency key covers the attempt;
- treat `SentOutcomeUnknown`/possibly accepted as terminal by default and expose an explicit error;
- never retry after downstream headers/body are committed;
- record delivery certainty and idempotency decision without body content.

### Acceptance

- A transport fixture fails before connect, before write, after partial write, after full write/before response, and after response headers.
- Only known-not-sent cases retry without idempotency.
- Ambiguous cases execute at most one upstream call in the default policy and produce a stable diagnosable outcome.

## `REL-001`: Accepted Background Work Has No Single Durable Completion Contract

Severity: P1/P2
Technical authority area: usage, runtime mutations, audit, lifecycle

### Evidence

- Usage and storage use bounded writers with retries and shutdown reports.
- Queue overload can enter synchronous fallback: `src/anthropic/usage.rs:1064` and `src/anthropic/usage.rs:1486`.
- Shutdown reports `postgres_abandoned`, `redis_abandoned`, and storage `abandoned`: `src/main.rs:599-623`.
- Only credential-stat shutdown failure independently triggers a panic/non-zero exit: `src/main.rs:632-640`.
- Some Admin audit writes use untracked `tokio::spawn`: `src/admin/service.rs:1419`.

### Required Target

- classify every mutation as required durable, derived/rebuildable, or best-effort diagnostic;
- assign stable IDs and idempotency to required durable events;
- persist required accepted events to an outbox before acknowledging durable acceptance;
- supervise writers and expose backlog/oldest-age/retry/drop state;
- make shutdown outcome and exit status reflect required undrained data;
- derive Redis dashboards asynchronously from durable usage rather than blocking response completion.

### Acceptance

- Kill/restart and dependency-failure tests deliver required accepted events at least once to idempotent owners and converge to one durable effect per stable ID; they do not claim a cross-Redis/PgSQL exactly-once transaction.
- Derived Redis loss is rebuilt without changing durable totals.
- Shutdown with deliberately blocked writers exits non-zero and reports the precise residue.
- Ordinary success does not synchronously wait for dashboard rollups.
