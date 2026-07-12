# Protocol And API Contracts

Role: Project-wide factual baseline
Status: Current protocol and API behavior
Authority: Defines the externally observable API surface and current request/response processing contracts; target changes belong to an active plan
As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11
Read when: Changing authentication, routes, Messages handling, model resolution, Files, count-tokens, external passthrough, streaming, cache reporting, or public errors
Related: [Business context](business-context.md), [Runtime flows](runtime-flows.md), [Resource and concurrency model](resource-and-concurrency-model.md), [System architecture modernization](../plans/system-architecture-modernization/README.md)

## Scope And Trust Model

This document records the protocol behavior implemented by the current `kiro-rs` server. It is a revision-pinned baseline, not a target architecture and not a claim that every current behavior is desirable.

The product has one operator and one trust domain. Multiple request API keys are equivalent ingress credentials for that operator's devices, clients, or key rotation. They do not identify users or tenants, do not select isolated data, and do not create separate authorization scopes. Kiro credentials and external pools are operator-owned upstream capacity, not product users.

## Common HTTP Contract

### Authentication

Authenticated Anthropic-compatible routes accept either of these credentials:

- `x-api-key: <request-key>`;
- `Authorization: Bearer <request-key>`.

The key is matched against the current in-memory request-key set. All matching request keys grant the same request-plane authority. Missing, malformed, or unknown credentials are rejected before the handler executes. Request keys are distinct from the Admin key and do not grant Admin API authority.

Evidence: `src/common/auth.rs:14-89`, `src/anthropic/router.rs:257-348`.

### Shared Limits And Response Envelope

- Every Anthropic-compatible route family is behind a 50 MiB HTTP body limit.
- Messages request JSON is rejected when nesting exceeds 192 levels.
- The Messages request must resolve to a non-empty model name.
- Public errors use Anthropic-compatible error envelopes. Where an upstream request ID is available, normalization retains it for client and operator diagnosis.
- Streaming responses use SSE. Non-streaming responses return one Anthropic-compatible JSON message.

The 50 MiB transport limit is not a promise that every body below 50 MiB can be processed within a fixed memory amount. Base64 decoding/encoding, JSON materialization, body rewriting, Files lookup, PDF conversion, and upstream envelopes can create additional allocations. Those resource facts are documented in [Resource and concurrency model](resource-and-concurrency-model.md).

Evidence: `src/anthropic/router.rs:40-41`, `src/anthropic/handlers/request_entry.rs:5-66`, `src/anthropic/envelope.rs:52-142`.

## Route Families

`src/anthropic/router.rs:257-348` mounts the same API shape under route-specific state and policy.

| Route family | Current routing/cache meaning | Special behavior |
| --- | --- | --- |
| `/v1` | Default local/external route with the current high-cache policy | General Anthropic compatibility |
| `/na/v1` | No-cache route policy | Does not apply local prompt-cache simulation |
| `/cc/v1` | High-cache route with Claude Code-oriented compatibility | Stricter SSE ordering and terminal usage behavior |
| `/ha/v1` | High-cache route | Has an independent path-level reported-usage policy |
| `/dfcache/{route}/v1` | Named high-cache policy | `{route}` must be configured; an unknown route returns 404 |

Each family exposes the following functional surface under its prefix:

| Capability | Endpoint shape | Current semantics |
| --- | --- | --- |
| Model catalog | `GET .../models` | Returns the process-local resolved catalog |
| Messages | `POST .../messages` | Executes the raw or parsed request flow described below |
| Token counting | `POST .../messages/count_tokens` | Preprocesses eligible multimodal content, then performs remote-or-local estimation |
| Files collection | upload/list endpoints below `.../files` | Uses process-local Files staging with bounded live payload but an unbounded delete-tombstone metadata defect |
| File metadata/content | `GET .../files/{file_id}` and `GET .../files/{file_id}/content` | Reads the current process-local object |
| File deletion | `DELETE .../files/{file_id}` | Removes the current process-local object |

The route prefix is policy-bearing. It is not a tenant, user, or authorization namespace.

## Admin And Operational API Boundaries

The Admin API is mounted at `/api/admin` only when a non-empty Admin API key is configured. It accepts the same header forms as request-plane authentication, but compares them against the separate per-process Admin key:

- `x-api-key: <admin-key>`;
- `Authorization: Bearer <admin-key>`.

The Admin key grants the complete mounted management surface; the current API has no per-operation roles. That surface manages credentials and their runtime controls, proxy resources, external pools, request/Admin keys, runtime configuration, model capabilities and pricing, usage records/dashboards/cleanup, audit logs, and system version information. Request API keys do not authorize this surface.

Admin mutations can update PgSQL, Redis-derived state, and process-local managers through different code paths. A successful Admin response means the owning handler completed its current mutation contract; it does not universally prove that every other replica has already refreshed every process-local view. Cross-replica behavior is detailed in [Deployment and operations](deployment-and-operations.md).

`/healthz` and `/readyz` are unauthenticated operational endpoints. `/healthz` reports process liveness. `/readyz` reports 200 only when PgSQL ping, Redis ping, and the Redis runtime-event subscription are healthy; otherwise it reports 503. These endpoints are not nested under a request route family and do not apply cache or model policy.

Evidence: `src/admin/router.rs:63-239`, `src/admin/middleware.rs:19-64`, `src/main.rs:433-485,813-898`.

## Messages Entry Contract

### Authoritative Input

The handler receives the HTTP body as raw `Bytes`. Those bytes remain the authoritative input for an eligible raw external-pool path. The implementation deliberately performs only bounded lightweight probing before deciding whether raw direct/preflight routing can finish the request. A raw path can therefore execute before full `MessagesRequest` deserialization.

When the raw path does not complete the request, entry processing:

1. scans JSON nesting depth with a hard limit of 192;
2. probes top-level facts needed for early routing, including the model and relevant request shape;
3. applies the configured missing-`max_tokens` policy, including raw top-level insertion when that path requires it;
4. deserializes the body into `MessagesRequest`;
5. rejects a missing or empty model;
6. hands the parsed request to the main Messages orchestration flow.

The request-entry and parsed orchestration layers each obtain runtime configuration. A concurrent Admin update can therefore make the two reads observe different versions in the current implementation.

Evidence: `src/anthropic/handlers/request_entry.rs:5-66,134-196`, `src/anthropic/request_facts.rs`, `src/anthropic/handlers.rs:4277-4490`.

### Parsed Request Processing

For a parsed request, the current pipeline performs these logically ordered operations, although orchestration is distributed across large handler functions:

1. resolve path-specific cache and reported-usage policy;
2. normalize or trigger thinking behavior required by the selected compatibility mode;
3. materialize supported `file_id`, image URL, and document URL sources when enabled;
4. resolve the requested model through the current catalog and alias policy;
5. detect the pure WebSearch special case;
6. decide local Kiro execution, external direct execution, external fallback, or rejection;
7. for local execution, convert Anthropic system/messages/tools/content into the selected Kiro IDE or CLI request envelope;
8. run the payload guard and enabled body-shaping operations;
9. acquire upstream capacity, execute bounded retries/failover, and translate the result;
10. project terminal usage and record the operational result.

Body materialization and shaping mean the effective request used for token/cache projection can differ from the inbound body. Raw upstream usage, effective-request facts, cache evidence, and reported usage are related but distinct facts.

Evidence: `src/anthropic/handlers/parsed_body_pipeline.rs:13-64`, `src/anthropic/handlers/local_body_pipeline.rs:21-227`, `src/anthropic/handlers.rs:4277-4490,6936-7021`.

## Local Kiro Protocol Contract

Local execution converts the Anthropic request to either a Kiro IDE or Kiro CLI upstream envelope according to credential/endpoint configuration. Conversion covers:

- system and conversation history;
- text, image, document, and materialized Files content;
- tool definitions, tool-use blocks, and tool-result pairing;
- JSON schema normalization;
- thinking configuration and thinking content;
- cache-control positions and effective prompt facts;
- mapped model identifiers and endpoint-specific metadata.

The selected credential must support the resolved model and survive scheduler eligibility checks before transport begins. On retry, failed credentials are excluded for the current request so a retry can choose different eligible capacity. A terminal response is translated back to Anthropic-compatible JSON or SSE, then lease/cooldown/runtime and usage state are updated.

WebSearch is a separate upstream capability only when the request contains exactly one tool and that tool is `web_search`. A request containing WebSearch plus any other tool does not enter the pure WebSearch path.

Evidence: `src/anthropic/converter.rs`, `src/anthropic/converter/content.rs:263-328`, `src/anthropic/websearch.rs:103-109`, `src/kiro/provider.rs`.

## Claude Code Compatibility Contract

`/cc/v1` is intended for real Claude Code CLI behavior, including long-running tool and agent workflows. Its client-visible stream must preserve the protocol relationships expected by Claude Code:

- one `message_start` before content blocks;
- ordered `content_block_start`, matching deltas, and `content_block_stop` events;
- thinking blocks and their signature material where supplied by the upstream path;
- tool-use blocks whose JSON input can be reconstructed from ordered deltas;
- ping/no-op keepalive events that do not terminate or corrupt content state;
- a terminal `message_delta` and `message_stop` in valid order;
- nonzero terminal `message_delta.usage` when the completed request has reportable usage;
- normalized public errors rather than leaking incompatible upstream envelopes.

The terminal usage event is authoritative for the completed stream from the downstream client's perspective. Intermediate metering or upstream-specific events are inputs to projection, not alternate terminal Anthropic messages.

This contract applies to compatibility behavior, not to latency. Slow upstream first byte and long total execution are normal operating cases; timeout and resource behavior are recorded in [Resource and concurrency model](resource-and-concurrency-model.md).

Evidence: `src/anthropic/handlers.rs:6936-7021`, `src/anthropic/envelope.rs:52-142`, `src/kiro/parser/*`.

## Model Catalog And Resolution

The model surface is assembled from multiple current sources:

- an embedded seed catalog;
- durable PgSQL model capability rows;
- models learned by Kiro synchronization;
- operator-managed additions and aliases.

Resolution is affected by route, credential support, external-pool support, thinking capability, and the configured catalog mode:

| Mode | Current interpretation |
| --- | --- |
| Compatible | Accept compatible known forms and resolve aliases/canonical forms according to catalog rules |
| Alias-only | Permit configured aliases while rejecting unlisted exact forms outside that policy |
| Exact-only | Require an exact supported catalog/model identifier |

A model being present in a displayed catalog does not by itself guarantee that every local credential or external pool can execute it. Final eligibility is checked against the selected upstream resource.

Evidence: `src/anthropic/model_capabilities.rs:12-15,129-230`, `src/model/config.rs:2032-2134,2220-2449`.

## External Pool Body Contracts

External body preparation and usage projection are independent choices. Selecting a raw body mode does not force raw usage passthrough, and selecting normalized body handling does not require simulated usage.

### Raw Passthrough

Raw passthrough retains inbound bytes except for an explicitly configured top-level model operation:

| Model handling | Effect |
| --- | --- |
| None | Forward the authoritative inbound body unchanged |
| Probe | Read enough top-level data for routing without rewriting the body |
| Rewrite | Replace the top-level model while preserving the remainder of the raw body representation |

Raw direct or preflight selection can occur before full serde parsing. Parsed-body materialization, local conversion, and payload shaping do not run on a completed raw path.

### Normalized Body

Normalized mode deserializes the request and may apply:

- model mapping;
- thinking normalization;
- enabled multimodal/file materialization;
- external payload-guard processing;
- serialization into a new outbound JSON body.

Both modes use external-pool eligibility, priority/load selection, capacity leases, queue limits, cooldowns, retries, stream-idle handling, and result recording.

### Current `preservePath` Defect

The Admin/config contract exposes `preservePath`, but current URL construction does not honor it. `external_pool_url` ignores its `_endpoint` parameter and always derives `/v1/messages` (or `/messages` when the base URL already ends in `/v1`). Callers cannot rely on path preservation in `v0.0.102`.

This paragraph records a current defect. The intended correction is tracked under [未实现要求](#未实现要求), not treated as current behavior.

Evidence: `src/external_pool/body_pipeline.rs:11-168`, `src/external_pool.rs:303-349,3329-3363,3400-3405`.

## Files Compatibility Contract

The Files API is a process-local compatibility staging area, not durable object storage.

- Upload returns a generated `file_id` for non-empty content no larger than 50 MiB.
- The process keeps at most 128 live files and at most 256 MiB of live stored bytes; insertion evicts oldest live entries to satisfy those bounds.
- Explicit delete removes the payload/map entry but leaves its ID in the FIFO order queue. Repeated upload/delete churn can grow metadata and list scan time without violating the live limits.
- Metadata, content read, list, and delete operate only against the receiving process's memory.
- A restart loses all uploaded Files objects.
- Replicas do not share Files objects, so a later request routed to another replica cannot resolve the first replica's `file_id`.
- When a Messages request references a supported `file_id`, its bytes are materialized into request content, including base64 representation where required by the upstream envelope.

These are availability and compatibility limitations in the single-user product. They are not tenant-isolation defects because no tenant boundary exists.

Evidence: `src/anthropic/files.rs:21-128,255-268,286-341`.

## Count-Tokens Contract

Count-tokens endpoints parse the request and run enabled multimodal/file preprocessing before estimation. Counting covers text, system content, tools and schemas, thinking, and media-derived estimates.

When a remote tokenizer URL is configured, the implementation attempts that service. On remote failure it falls back to local estimation. The returned count is clamped to at least one token. The remote path currently clones the payload, creates a client per call, and bridges asynchronous HTTP through synchronous counting code; those are resource/implementation facts rather than public token-accuracy guarantees.

Evidence: `src/token.rs:107-185`, `src/anthropic/body_processing.rs`.

## Cache And Usage Contract

The local prompt cache is not a response cache. It does not store or replay model completions, and every successful Messages request still executes an upstream. It tracks stable prompt-prefix evidence and projects cache-related token fields according to route policy.

Current route policy chooses among no-cache, current/high-cache, and `kiro-rs-tool`-oriented projection behavior. The prompt-cache tracker is bounded process-local state. Candidate cache state is committed only after a successful upstream completion; a failed attempt does not create a successful local cache observation.

The following facts must be read separately when diagnosing a record:

| Fact | Meaning |
| --- | --- |
| Raw upstream usage | Token/metering fields observed from Kiro or an external upstream |
| Compatibility usage | Fields normalized into Anthropic-compatible meanings before final route policy |
| Effective/billable usage | Usage used for client reporting, cost, or operator accounting after policy |
| Cache read | Tokens projected or observed as read from a stable prompt prefix |
| Cache creation/write | Tokens projected or observed as newly cached |
| `UsageSource` | Provenance describing which current projection path produced the record |

Stream and non-stream paths both produce final projected usage. External pools can preserve compatible upstream usage or re-project it according to pool and route configuration, independently of raw/normalized body mode.

Current `CacheUsage` uses these meanings:

```text
reported_total_input_tokens
  = input_tokens                 # uncached reported portion
  + cache_read_input_tokens
  + cache_creation_input_tokens
```

`cache_creation_5m_input_tokens` and `cache_creation_1h_input_tokens` are internal breakdown fields capped by total cache creation; current upstream data may provide an incomplete breakdown, so they are not always both present in downstream JSON.

For local high-cache projection that is configured to move an input reduction into cache read, the current code does not sample/reduce input when no cache-read evidence exists. In that case raw/selected input remains and local cache-read/cache-creation can both remain zero. A no-cache route and an external pass-through route follow their own raw/stripping policies rather than inventing local evidence.

Evidence: `src/anthropic/cache.rs:9-23,87-159,211-233`, `src/anthropic/prompt_cache.rs:13-23,264-330,347-494`, `src/anthropic/handlers.rs:2826-2872`, `src/anthropic/usage.rs:173-198,301-407`, `src/external_pool/usage_projection.rs`.

## Current Contract Boundaries

The following distinctions are necessary when changing the protocol implementation:

- route family is policy, not identity;
- request API key is authentication, not a tenant ID;
- raw body authority applies only to eligible raw external handling;
- model catalog visibility and upstream eligibility are separate checks;
- prompt cache evidence is not cached completion data;
- raw usage and reported usage are not interchangeable;
- Files compatibility storage is process-local and ephemeral;
- SSE keepalive is transport progress, not content or completion;
- an accepted upstream request can remain active for minutes without violating the current latency contract.

## 未实现要求

The following outcomes are required but are not current baseline behavior. Their design, sequencing, acceptance criteria, rollout, and rollback belong to the [System architecture modernization plan](../plans/system-architecture-modernization/README.md):

- make one immutable runtime/config snapshot authoritative for the complete request lifecycle;
- restore the public `preservePath` behavior with route-specific compatibility tests for raw and normalized external bodies;
- represent raw upstream usage, effective request facts, cache evidence, compatibility usage, and reported/billable usage as explicit non-interchangeable contracts;
- preserve byte-authoritative raw passthrough while isolating it from parsed-body transformations;
- define and verify one terminal stream/non-stream usage contract across local, external, retry, tool, thinking, and error paths;
- provide durable or explicitly replica-affine Files behavior before advertising cross-replica Files compatibility;
- keep Claude Code compatibility covered by real CLI tests for tools, agents, Files, search, thinking, event order, errors, and cache usage.
