# Business And Product Context

Role: Project-wide product context and current-capability baseline
Status: Current product definition and accepted trust boundary as of 2026-07-11
Authority: Defines current Rust product scope, accepted single-user boundary, business capabilities, and compatibility inputs; detailed target requirements live in the Greenfield AI Gateway plan
As of: `v0.0.102`, commit `e9479df71ee0`, 2026-07-11
Read when: Evaluating a feature, architecture change, compatibility decision, or severity classification
Related: [System context](system-context.md), [Runtime flows](runtime-flows.md), [Storage and state](storage-and-state.md)

## Product Definition

`kiro-rs` is a self-hosted, single-operator compatibility gateway. It accepts Anthropic Messages API traffic from clients such as Claude Code CLI and Anthropic-compatible SDKs, translates eligible requests to Kiro upstream protocols, and can route selected traffic to configured external Anthropic-compatible pools.

The product is not only a format converter. Its business responsibilities include:

- preserving client-visible Anthropic and Claude Code protocol behavior;
- managing multiple Kiro credentials owned by one operator;
- selecting healthy capacity, refreshing tokens, applying retry and cooldown policy, and maintaining session affinity;
- translating requests and responses between Anthropic, Claude Code, Kiro IDE/CLI, and external-compatible protocols;
- applying route-specific cache and reported-usage policy;
- collecting operational usage, cost, latency, error, and audit data;
- exposing an Admin API and two maintained Admin user interfaces;
- remaining stable when upstreams are slow, partially unavailable, or return malformed data.

The current public description and startup contract are in `README.md`. The executable is assembled in `src/main.rs`; authenticated API routes are mounted in `src/anthropic/router.rs` and `src/admin/router.rs`.

## Business Problem

The gateway allows one operator to use Kiro-backed and external-backed model capacity through Anthropic-compatible clients without requiring those clients to understand:

- Kiro authentication and token refresh;
- Kiro IDE versus CLI request envelopes;
- credential health, cooldown, RPM, concurrency, and priority;
- upstream model aliases and capability differences;
- local versus external routing and failover;
- cache simulation and route-specific usage projection;
- usage persistence and operational diagnostics.

The gateway therefore owns correctness at both boundaries: downstream Anthropic compatibility and upstream Kiro/external compatibility.

## Trust And Deployment Model

### Single User And Single Trust Domain

The supported product model has one owner/operator and no tenant or per-user data-isolation boundary.

- Multiple request API keys are access credentials for the same operator-owned service. They are not tenant identifiers.
- Multiple Kiro credentials and external pools are capacity resources owned by the same operator. They are not user accounts in the product-domain sense.
- The Admin key grants a higher-privilege management surface, but Admin and request clients still belong to the same operator trust domain.
- Files uploaded through the Anthropic Files-compatible API are not partitioned by user or tenant.

Consequently, the architecture must not introduce tenant IDs, tenant repositories, per-tenant schedulers, or tenant authorization checks without a future explicit product decision.

### Process Count Is Independent From User Count

The service can be launched as one process or multiple replicas, and the current code contains cross-process coordination mechanisms. Multiple replicas still serve the same single operator. Cross-replica consistency, leases, invalidation, and key rotation are high-availability concerns, not multi-user concerns. Whether multi-replica operation is a formally supported production mode remains an explicit modernization question.

### External Trust Boundaries Still Exist

Single-user operation does not eliminate security boundaries:

- downstream clients send untrusted request bodies and URLs;
- Kiro and external pools are remote systems with independent failure behavior;
- remote image/document URLs may resolve to unsafe network destinations;
- PgSQL, Redis, logs, and diagnostics contain operator-sensitive data;
- external request and response headers cross a provider boundary and require explicit control; the current implementation is denylist-oriented and the proposed target uses allowlists.

## Actors And Dependencies

| Actor or dependency | Responsibility | Trust level |
| --- | --- | --- |
| Operator | Configures credentials, routes, cache policy, models, pricing, and diagnostics | Trusted owner |
| Anthropic-compatible client | Sends Messages, count-tokens, models, and Files requests | Authenticated but request content is untrusted |
| Claude Code CLI | Exercises strict streaming, thinking, tool, usage, Files, and agent workflows | Authenticated compatibility client |
| Admin UI/API | Manages the same operator-owned service state | Privileged operator surface |
| Kiro upstream | Executes local-pool model requests and refreshes credential state | Remote dependency |
| External pool upstream | Executes configured fallback/direct Anthropic-compatible requests | Remote dependency with separate credential/header boundary |
| PgSQL | Authoritative durable configuration, credentials, runtime state, catalogs, usage, and audit data | Required infrastructure |
| Redis | Cross-process coordination, leases, sticky bindings, cooldowns, derived usage summaries, and invalidation | Required infrastructure |
| Local filesystem | Bootstrap configuration, embedded assets, live-payload-bounded file staging with a known ordering-tombstone defect, and diagnostic logs | Process-local storage |

## Primary Use Cases

1. A Claude Code or SDK client sends a streaming or non-streaming Messages request and receives Anthropic-compatible output.
2. The gateway resolves the route policy, model, body-processing capabilities, and cache/usage behavior for the request path.
3. The scheduler selects an eligible Kiro credential, applies session affinity and concurrency controls, refreshes authentication when needed, and retries bounded failures.
4. A configured external pool can serve direct traffic or fallback traffic, using raw passthrough or normalized body preparation according to pool capability.
5. Images, documents, and uploaded file references are materialized. Current per-file/per-source limits exist, while aggregate remote budgets and complete Files metadata bounds remain known gaps.
6. Usage projection produces client-compatible usage while preserving separate raw-upstream and operator-reporting facts.
7. The operator inspects credentials, routing, usage, pricing, model capabilities, errors, audit events, and service health through Admin surfaces.
8. The process starts from durable state, reloads runtime changes, drains accepted work on shutdown, and recovers after dependency or upstream failures.

## Route Families And Business Meaning

| Route family | Intended policy meaning |
| --- | --- |
| `/v1/*` | Default Anthropic-compatible route; currently uses the default/high-cache policy family |
| `/na/v1/*` | No-cache-oriented route and corresponding reported-usage policy |
| `/ha/v1/*` | Explicit high-cache route with independent path overrides |
| `/dfcache/{name}/v1/*` | Operator-defined high-cache route with longest-prefix policy resolution |
| `/cc/v1/*` | Claude Code compatibility route with stricter protocol behavior |
| `/api/admin/*` | Privileged management API for the same operator-owned service |
| `/healthz`, `/readyz` | Process liveness and dependency/runtime readiness |

`preservePath` means the selected external pool must receive the caller's route path when enabled. It is a public behavioral contract, not a cosmetic UI setting.

## Current Cache And Usage Semantics

The business domain has three distinct fact layers:

1. **Raw upstream usage** is what Kiro or an external upstream actually returned.
2. **Cache evidence and simulation state** describe local prefix creation/hit observations and route policy.
3. **Reported usage** is the client-visible and operator-visible projection after route policy.

Current code spreads these layers across `src/anthropic/prompt_cache.rs`, `src/anthropic/cache.rs`, `src/anthropic/handlers.rs`, `src/external_pool/usage_projection.rs`, and `src/anthropic/usage.rs`. The code does not yet expose one authoritative domain type separating actual, cache evidence, reported, and billable facts.

For the current local high-cache policy, input sampling that moves a delta to cache read is suppressed when no cache-read evidence exists; raw input remains and local read/creation stay zero. No-cache and external pass-through policies have separate behavior. Exact current formulas are described in [protocol and API contracts](protocol-and-api-contracts.md).

## Current Compatibility Commitments

- Existing successful Anthropic/Claude Code content blocks, stop reasons, thinking, tools, event order, errors, models, Files routes, and final usage are compatibility references for the rewrite.
- Raw external passthrough preserves original bytes except explicitly configured lightweight transformations.
- Credential and pool eligibility currently considers configuration, model/body compatibility, cooldown, RPM, concurrency, health, and exclusions.
- Lease/completion guards are intended to release capacity on success, failure, cancellation, timeout, and disconnect.
- Slow model upstream behavior is part of the real workload: first byte may exceed 30 or 60 seconds and a progressing stream may exceed 180 seconds.

These are current behavior/compatibility statements, not proof that every failure path is defect-free.

## Target Reconstruction Relationship

Runtime config CAS, immutable request snapshots, durable outbox semantics, exact cache conservation, header allowlists, aggregate resource budgets, bounded diagnostics, performance gates, and strict shutdown residue behavior are target requirements, not current facts. The [Greenfield AI Gateway plan](../plans/greenfield-ai-gateway/README.md) now owns their target form. The superseded Rust [requirements](../plans/system-architecture-modernization/topics/requirements-and-quality-attributes.md) and [decision index](../plans/system-architecture-modernization/decisions/README.md) remain historical rationale and behavioral input.

## Current Rust Product Non-Goals

These statements describe the current Rust product/maintenance boundary at the baseline revision. [Greenfield decision 001](../plans/greenfield-ai-gateway/decisions/001-greenfield-go-modular-ai-gateway.md) explicitly supersedes them where it defines a separate target repository, one new Admin application and one characterized whole-system cutover.

- Multi-user or multi-tenant identity, authorization, billing, quotas, or data partitioning.
- Replacing Kiro or external upstream business logic with an in-process model runtime.
- A public plugin ABI for every request stage before a second independently developed extension requires it.
- An uncharacterized all-system cutover without complete compatibility, recovery and rollback gates. The greenfield target uses one fully characterized whole-system cutover.
- Removing either maintained Admin UI from current Rust maintenance without a separate product decision. Greenfield decision 001 supplies that decision for the separate target only.
- Treating simulated cache usage as proof of real upstream cache behavior.
- Hiding lossy or approximate accounting behind a field named as authoritative usage.

## Glossary

| Term | Meaning in this project |
| --- | --- |
| Local pool | Operator-owned Kiro credentials selected by `MultiTokenManager` |
| External pool | Operator-configured Anthropic-compatible upstream target |
| Raw passthrough | Forwarding original request bytes, with only explicitly enabled lightweight transformations |
| Normalized body | Parsed and normalized Anthropic request serialized for an external upstream |
| Payload shaping | Controlled request reduction or repair before an upstream call |
| Cache evidence | Local or upstream facts indicating cache creation or cache read |
| Usage projection | Transformation from raw/effective facts to downstream reported usage |
| Runtime snapshot | Immutable, versioned configuration used consistently for one request |
| Lease | Time-bounded Redis/local capacity ownership released at request completion |
| Outbox | Durable, replayable record of accepted background mutations |
| Replica | Another process serving the same single operator; not another user or tenant |
