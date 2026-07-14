# AI Gateway Complete Greenfield Reconstruction Plan

Role: Complete target architecture and implementation specification

Status: Ready for review; implementation Not Started

Authority: Defines target goals, final behavior, module boundaries, contracts, technology stack, Kiro first-release scope, quality attributes, acceptance gates and cutover model

As of: 2026-07-13

Related: [Plan root](../README.md), [work graph](../roadmap.md), [decision 001](../decisions/001-greenfield-go-modular-ai-gateway.md), [reference projects](reference-projects-and-template-selection.md), [current runtime flows](../../../baseline/runtime-flows.md), [current risk hotspots](../../../baseline/risk-hotspots.md)

## 1. Executive Conclusion

The target is a new AI model gateway, not a refactor of the existing Rust package tree. It is implemented in a separate repository with Go, React, TypeScript and Tailwind CSS. The current `kiro.rs` repository remains available only as a behavior and failure oracle until final cutover.

The system is generic in two independent directions:

1. Client protocol modules accept and emit Anthropic, Claude Code, OpenAI, Gemini or future public protocols.
2. Provider modules execute against Kiro, standard model APIs, other API gateways or future upstreams.

The execution core sits between these directions and understands only operation contracts, capabilities, leases, attempts, canonical events, delivery evidence, usage facts and terminal outcomes. It never imports Kiro wire DTOs, credentials, Redis keys or error strings.

Kiro is the first complete provider module. It is more than a parser: it privately owns Kiro authentication, token refresh, IDE/CLI endpoints, accounts, scheduling, distributed leases, transport, AWS EventStream, conversion, errors, models, quota, usage and maintenance workflows.

Other provider modules may need the same capabilities. They may compose shared default implementations or implement those capabilities privately. Interoperability comes from stable contracts and invariants, not mandatory code reuse.

`Owns`, `owner` and `authority` in this document mean code, state and invariant boundaries only. They never identify a person or team. This plan contains no staffing, assignee, calendar or implementation-duration estimates; durations below are runtime, benchmark, drain, recovery or observation correctness gates.

## 2. Why A Greenfield Boundary Is Required

Large files are symptoms of mixed authority. The inspected current tree includes:

| Current file | Approximate size | Mixed responsibilities that must not be copied |
| --- | ---: | --- |
| `src/storage/postgres.rs` | 11,500 lines | schema, repositories, migrations, domain persistence and maintenance |
| `src/kiro/token_manager/manager.rs` | 8,100 lines | config, credentials, refresh, scheduling, Redis, persistence, Admin mutation, health and statistics |
| `src/anthropic/handlers.rs` | 7,100 lines | admission, routing, parsing, provider selection, retries, response handling and persistence |
| `src/anthropic/payload_guard.rs` | 6,600 lines | payload policy, transformations, diagnostics and compatibility behavior |
| `src/admin/service.rs` | 6,600 lines | commands, queries, storage, runtime reload and cross-domain orchestration |
| `src/storage/redis_cache.rs` | 6,300 lines | cache, usage projections, scheduler scripts, leases and invalidation |
| `src/model/config.rs` | 5,700 lines | bootstrap, runtime config, validation, defaults and unrelated domain policy |
| `src/anthropic/stream.rs` | 5,400 lines | provider decoding, protocol events, usage and terminal response behavior |
| `src/external_pool.rs` | 5,000 lines | configuration, scheduling, transport, retries, fallback, usage and health |
| `src/kiro/provider.rs` | 3,900 lines | Kiro transport, scheduling, retries, completion, models and account outcomes |

The target therefore ports behavior through characterization tests and contracts. It does not translate these files into similarly broad Go packages.

Verified current risks become target acceptance requirements:

- configuration updates need candidate validation, expected-version CAS, atomic publication, audit and outbox;
- Redis lease, marker and aggregate operations need bounded atomic transitions and repair;
- every request must use one immutable configuration revision;
- no request or response body, remote materialization, queue, cache, client pool or diagnostic directory may be unbounded;
- multi-replica configuration, key revocation, Files and scheduler behavior must converge;
- upstream execution evidence and downstream response commitment must be separate retry inputs;
- stream completion, resource release, usage and credential mutation need one coordinated terminal decision without one owner absorbing every obligation;
- shutdown must stop and join producers before closing writer ingress.

## 3. Goals

### 3.1 Product Goals

- Provide a self-hosted high-performance AI model gateway for one operator and one trust domain.
- Fully support Kiro in the first complete release while keeping Kiro-specific behavior inside one provider boundary.
- Preserve real Anthropic Messages and Claude Code behavior, including long streaming sessions, tools, thinking, Files, MCP and final usage.
- Add or replace client protocols and upstream providers without editing unrelated modules.
- Configure providers, credentials, routes, models, schedulers, retries, usage and operations from one modern control plane.
- Support one, two or many replicas with correct distributed capacity and configuration behavior.
- Make every request, attempt, retry, usage projection, configuration publication and background obligation observable and auditable.

### 3.2 Architecture Goals

- Small typed contracts with explicit versioning and capability negotiation.
- Vertical provider ownership plus reusable stateless or narrowly stateful default libraries.
- One request configuration snapshot and one route/execution plan.
- One observable attempt at a time and conservative replay safety.
- Bounded streaming with cancellation and backpressure from client through provider transport.
- PostgreSQL durable authority, Redis coordination/projections and explicit recovery for each state class.
- Manual dependency composition at process bootstrap, without global mutable state or a service locator.
- A target-only candidate and one final cutover, without hybrid production execution.

### 3.3 User Experience Goals

- A dense, quiet, operational Admin application designed for repeated management work rather than a marketing dashboard.
- Fast tables with server-side filters, sorting, pagination, bulk actions and persistent column preferences.
- Draft, diff, validate, publish and rollback workflows for configuration.
- Clear provider/account health, queue pressure, quota, usage, latency, error and replica views.
- Secret-safe forms that never re-display reusable credentials.
- Responsive desktop and mobile layouts, full keyboard operation, WCAG 2.2 AA and predictable error recovery.

## 4. Non-Goals

- Incrementally refactor, embed or invoke the current Rust runtime.
- Preserve the current Rust package names, 50-module topology, two Admin frontends or handwritten frontend DTOs.
- Build a tenant/SaaS billing platform in the first release.
- Run model inference in-process.
- Make every provider implement Kiro's scheduler or account model.
- Provide a giant universal request object that silently drops unsupported fields.
- Use Go `.so` runtime plugins, a generic JSON command bus, a global `AppState`, a dependency-injection container or broad generic repositories.
- Introduce Kubernetes, Envoy, service mesh or microservices as mandatory prerequisites for a single-node deployment.
- Claim financial-grade invoicing before reconciliation, currency, tax and immutable ledger requirements are separately accepted.
- Release partially reconstructed production variants.

## 5. Expected Final Effect

### 5.1 Adding A Provider

Adding a new upstream normally requires:

1. a new provider package;
2. a module descriptor and supported operation/capability declarations;
3. module-owned configuration and migration contribution;
4. allocation and attempt implementations, using shared defaults or private logic;
5. raw usage and error mappings;
6. contract tests and optional provider-specific Admin contribution;
7. one compile-time registry entry.

It must not require changes to Anthropic/Claude Code adapters, the default scheduler, generic routing, terminal reduction or existing provider packages.

A provider representing one remote API endpoint can expose that endpoint as one capacity target and return an immediate lease. A provider representing many accounts can implement complex account scheduling. Both satisfy the same allocation lifecycle.

### 5.2 Adding A Client Protocol

Adding OpenAI Responses, Chat Completions, Gemini or another client protocol normally requires only:

- route and authentication binding;
- protocol-specific decode and validation;
- mapping to operation-specific invocation contracts and required capabilities;
- canonical-event and error encoding back to that protocol;
- compatibility fixtures and generated API documentation.

It must not change Kiro transport, account selection or provider state.

### 5.3 Replacing An Internal Algorithm

A provider can replace its scheduler, usage interpreter, model discovery or transport without changing the gateway core when its externally visible contract remains compatible. Shared implementations are libraries with explicit configuration, not global mutable authorities.

## 6. System Topology

```text
                              +-----------------------+
                              | React Admin Control   |
                              +-----------+-----------+
                                          |
                                          v
                              +-----------------------+
                              | Go Control Plane      |
                              | validate/CAS/audit    |
                              +-----------+-----------+
                                          |
                                   config revisions
                                          |
                                          v
+-----------+     +----------------+     +-----------------------+
| Clients   | --> | Protocol Edge  | --> | Execution Kernel      |
| CC/SDKs   | <-- | decode/encode  | <-- | route/attempt/terminal|
+-----------+     +----------------+     +----------+------------+
                                                     |
                                              typed module contract
                                                     |
                     +-------------------------------+------------------+
                     |                               |                  |
                     v                               v                  v
             +---------------+              +---------------+  +---------------+
             | Kiro Provider |              | HTTP Provider |  | Mock Provider |
             | own scheduler |              | default/simple|  | deterministic |
             +-------+-------+              +-------+-------+  +---------------+
                     |                              |
                     +---------------+--------------+
                                     |
                             external upstreams

PostgreSQL: durable config, provider state, audit, outbox, usage and attempts
Redis: bounded queues, leases, RPM/concurrency, cooldown, sticky and projections
Object store: shared Files and bounded diagnostic/release artifacts
```

## 7. Deployable Roles

The target is one repository and one architecture, not premature microservices. The same release can run these selectable roles:

| Role | Responsibility | Deployment use |
| --- | --- | --- |
| Data plane | Public API, request admission, immutable snapshot, routing, provider execution, streaming and terminal completion | Horizontally scaled separately for traffic |
| Control plane | Admin API, UI assets, configuration drafts/publication, queries and operator commands | Isolated listener or deployment for security and load isolation |
| Worker | Outbox delivery, usage projection, refresh, model/quota sync, cleanup and recovery jobs | One or more fenced workers with distributed claims |
| All-in-one | All roles in one process with the same contracts | Simple local and Docker Compose deployment |

Role separation changes process placement, not domain ownership. No role bypasses contracts or directly edits another module's private state.

## 8. Module Plan And Dependency Direction

### 8.1 Contract Layer

| Module | Owns | Must not own |
| --- | --- | --- |
| Invocation kernel | request identity, operation, model intent, deadline, stream mode, session key, resource budget and required capabilities | provider wire DTOs or client HTTP types |
| Operation contracts | typed `messages.v1`, `models.v1`, `count_tokens.v1`, `files.v1` and later operation schemas | one all-operations optional-field object |
| Capability contracts | tools, reasoning, cache control, images, documents, JSON schema, MCP and usage extensions | provider-specific credentials or transport |
| Canonical events | ordered content, thinking, tool, usage, error and terminal events | Anthropic SSE strings or Kiro EventStream frames |
| Outcome contracts | delivery evidence, attempt result, allocation completion, terminal outcome and usage facts | persistence or retry policy implementation |

### 8.2 Client Gateway Layer

| Module | Owns | Must not own |
| --- | --- | --- |
| Public edge | listeners, TLS, body limits, request IDs, public authentication and admission | routing policy or provider selection |
| Anthropic Messages adapter | Anthropic request DTO, validation, stream/non-stream response and error encoding | Kiro accounts, Redis leases or provider retries |
| Claude Code profile | stricter aliases, headers, event order, thinking/signature, tools, MCP, Files and usage behavior | Kiro transport or scheduling |
| Models/count-tokens/Files adapters | public operation semantics and protocol responses | provider-private storage or usage projection |
| Future protocol adapters | OpenAI/Gemini/custom wire behavior | modifications to existing adapters or providers |

### 8.3 Execution Core

| Module | Owns | Must not own |
| --- | --- | --- |
| Runtime capture | one immutable configuration revision and narrow request views | reloads or mutable global config reads during a request |
| Route planner | model aliases, required capabilities, route policies and ordered provider candidates | provider account ranking |
| Module registry | descriptors, operation ports, versions and capability lookup | hidden dependency resolution or runtime Go plugin loading |
| Attempt engine | attempt budget, delivery state, cross-provider replay safety and attempt history | provider token refresh or private retries |
| Downstream commitment | whether headers/body have been committed and whether response replacement remains legal | provider health state |
| Terminal reducer | one terminal decision and obligation intents | direct lease release, credential persistence or usage aggregation |
| Resource governor | weighted process admission for bodies, transformations, streams and workers | provider account capacity or business scheduling |

### 8.4 Provider Layer

| Module | Owns | Must not own |
| --- | --- | --- |
| Provider descriptor | identity, version, operations, capabilities, config schema and health surface | core routing policy |
| Allocation port | acquire, heartbeat, complete and cancel semantics | assumptions about implementation algorithm |
| Shared default scheduler | optional pure ranking, bounded wait coordination and lease lifecycle | mandatory global state for every provider |
| Attempt port | prepare and execute exactly one observable business attempt | hidden multi-account retry loops |
| Kiro provider | complete Kiro vertical behavior | Anthropic HTTP DTOs or generic usage storage |
| Compatible HTTP provider | simple standard upstream proof and optional reusable implementation | becoming another universal external-pool God module |
| Mock provider | deterministic events, faults and timing for contract/load tests | production credentials or network access |

### 8.5 Control Plane

| Module | Owns | Must not own |
| --- | --- | --- |
| Config authority | draft, validate, diff, expected-version CAS, atomic publish and rollback-to-new-revision | mutable in-place runtime objects |
| Provider management | module instances, module-owned config commands and capability/status queries | direct provider table updates |
| Route/model management | aliases, capability constraints, fallback and policy validation | provider account state |
| Secret authority | write-only secret input, envelope encryption, key versions, rotation and recovery | returning reusable plaintext through reads |
| Audit/outbox | same-transaction command audit and durable side-effect intents | best-effort detached mutation logging |
| Admin API | domain commands and queries with generated OpenAPI | generic CRUD over arbitrary table names |

### 8.6 Platform Layer

| Module | Owns | Must not own |
| --- | --- | --- |
| PostgreSQL adapters | domain-owned repositories, transactions and migrations | a single 10,000-line storage facade |
| Redis coordination | versioned atomic scripts/functions, key namespaces, fencing and rebuild | durable secrets or source-of-truth configuration |
| Object storage | shared Files and bounded artifacts | process-local production authority |
| HTTP transport factory | bounded reusable transports keyed by stable proxy/TLS/endpoint identity | route selection or provider error semantics |
| Telemetry | typed logs, metrics, traces and redaction | request bodies, secrets or high-cardinality metric labels |
| Lifecycle | task supervision, readiness, drain, producer barriers and shutdown report | silent detached workers |

### 8.7 Frontend Layer

| Module | Owns | Must not own |
| --- | --- | --- |
| App shell | navigation, responsive layout, command menu, theme and session surface | duplicated API DTOs |
| Domain features | providers, accounts, routes, models, scheduler, usage, audit, diagnostics and operations | direct table-shaped generic pages where workflows differ |
| Generated client | typed OpenAPI transport, errors and query keys | handwritten backend mirrors |
| Design system | Tailwind tokens, shadcn/Radix components and Lucide icons | a second overlapping component library |

## 9. Contract And Extension Model

### 9.1 Operation-Specific Ports

There is no single interface containing messages, embeddings, files, audio, models and every future capability. The registry returns a typed port for a requested operation/version. Unsupported operations are absent and fail before upstream execution.

Conceptually:

```go
type Allocator interface {
    Acquire(context.Context, AllocationRequest) (ExecutionLease, error)
    Heartbeat(context.Context, LeaseToken, LeaseActivity) (HeartbeatAck, error)
    Complete(context.Context, LeaseToken, CompletionID, AllocationOutcome) error
    Cancel(context.Context, LeaseToken, CompletionID, CancelReason) error
}

type AttemptSession interface {
    Events() CanonicalEventReader
    Result(context.Context) (AttemptResult, error)
}

type MessagesExecutor interface {
    Prepare(context.Context, ExecutionLease, MessagesV1) (PreparedAttempt, error)
    Execute(context.Context, PreparedAttempt) (AttemptSession, error)
}
```

`Prepare` cannot send the business request. `AttemptSession.Result` becomes final only after the event stream terminates or is cancelled and carries the final delivery evidence. A direct `Execute` error is legal only when the implementation proves `NotSent`; after any business-request send begins, `Execute` must return a non-nil session and every later error must surface through its final `AttemptResult`. Exact Go signatures are fixed during W2, but the ownership and lifecycle are binding.

### 9.2 Capability Negotiation

Capabilities are declared at module, provider-instance and model level. A provider-level claim is insufficient when individual models differ.

Support levels are:

| Level | Meaning |
| --- | --- |
| `native` | Upstream natively preserves the requested semantics |
| `lossless-transform` | A reversible or semantics-preserving transformation exists |
| `lossy-opt-in` | Loss is explicit, visible and authorized by route policy |
| `unsupported` | The request is rejected before any upstream send |

Unknown required extensions fail closed. Optional extensions can be omitted only when the client protocol and route explicitly permit omission. Silent field loss is forbidden.

### 9.3 Typed Extensions

- Stable core fields remain small.
- Operation schemas own operation-specific fields.
- Capability extensions use versioned schemas and generated Go/TypeScript types.
- `map[string]any`, raw unvalidated JSON and error-string routing are prohibited in cross-module contracts.
- A raw passthrough operation is a separate declared protocol for same-family upstreams. It cannot bypass authentication, resource admission, safe headers, delivery evidence or terminal accounting.

### 9.4 Contract Versioning

- Every operation and extension has an explicit semantic version identifier.
- Additive optional fields require capability discovery and fixture coverage.
- Removing or changing meaning requires a new major contract.
- A module declares the exact contract ranges it supports at registration.
- Startup fails when a required built-in contract cannot be negotiated.
- External process modules, when introduced later, use the same schemas over Protobuf and Connect/gRPC with deadlines, cancellation, backpressure and health contracts.

### 9.5 Provider State Isolation

- Every provider owns a PostgreSQL migration namespace and Redis key prefix.
- Core tables can reference an opaque provider instance ID but not provider-private account IDs or credential shapes.
- Cross-provider reads occur through typed queries or events, never direct table access.
- Shared libraries do not own global mutable state. The composing provider supplies stores, clocks, policies and telemetry.
- Module unload is not supported in the first release. Configuration disablement drains the module safely.

## 10. Request Lifecycle

One admitted request owns a route plan and an attempt loop. Every attempt owns exactly one provider allocation lease:

```text
Ingress Decode
-> Authenticate And Admit
-> Capture Runtime Revision
-> Resolve Route And Required Capabilities
-> Build Ordered Provider Candidate And Attempt Budget
-> Repeat Per Attempt
     Select Module And Operation Port
     -> Acquire This Attempt's Execution Lease
     -> Prepare Without Sending
     -> Durably Accept AttemptStarted
     -> Execute And Decode Canonical Events
     -> Capture AttemptResult, Raw Usage And Delivery Evidence
     -> Compute Retry-Or-Final Disposition
     -> Durably Finalize This Attempt
        And Request Terminal When Final
     -> Complete Or Cancel This Attempt's Lease
     -> Retry With A Fresh Acquire Or Leave The Loop
-> Ensure Request Terminal Envelope Is Durable
-> Commit Final Client Terminal Marker/EOF
-> Project Usage, Audit And Reporting Asynchronously
```

Detailed rules:

1. The edge establishes a request ID, deadline, cancellation and hard body budget before parsing.
2. Authentication and global resource admission occur before expensive transformations.
3. Runtime configuration is captured once by immutable revision. Downstream code receives narrow typed views, not the complete config document.
4. The client adapter validates the operation and declares required capabilities without selecting an upstream.
5. The route planner resolves model aliases, route policy, allowed lossiness, ordered provider candidates and one request-level attempt budget.
6. Capability negotiation rejects unsupported combinations before acquiring capacity or sending upstream.
7. For each attempt, the selected allocator returns a new fenced, time-bounded lease containing an opaque target reference. A prior attempt's lease is never reused for another account or provider.
8. Request materialization, tokenization and provider conversion are lazy, revisioned and separately resource-admitted. `Prepare` performs no business send.
9. Before a possible business send, PostgreSQL synchronously accepts an idempotent `AttemptStarted` fact. If this bounded write fails, the attempt remains `NotSent` and its lease is cancelled.
10. A provider executes one observable attempt. Every network send changes delivery evidence, and any post-send failure remains available through `AttemptSession.Result`.
11. Provider bytes are converted to bounded canonical events. Client encoders apply backpressure rather than collecting an unbounded response, and downstream commitment is recorded when replacement becomes impossible.
12. When the attempt ends, the core computes a pure retry-or-final disposition from delivery evidence, downstream commitment, policy and remaining candidates. PostgreSQL then synchronously accepts the attempt's final evidence, raw/partial usage, normalized outcome, disposition and provider/scheduler obligations.
13. When the disposition is final, one request terminal reducer accepts an idempotent terminal envelope in the same transaction where possible. A later dispatch failure with no `AttemptStarted` uses its own request-terminal transaction.
14. Only after the attempt envelope commits is that attempt's lease completed or cancelled. A retry or fallback begins only after completion is acknowledged, or a durable fenced pending-release obligation continues reserving the capacity. The next attempt performs a fresh acquire.
15. A non-stream response and the final success SSE marker/EOF are not committed until the terminal envelope is durable and lease completion is acknowledged or durably fenced as pending. Stream content may precede this boundary, but failure to accept the envelope cannot be reported as a clean successful completion.
16. Usage rollups, dashboard projections and other derived work may remain asynchronous behind the accepted outbox. Unique attempt facts, request terminal facts and raw usage evidence may not exist only in a volatile writer buffer.
17. Cancellation, disconnect, timeout, malformed streams, storage failure and shutdown follow the same per-attempt finalizer and request-terminal rules.

## 11. Retry, Replay And Response Commitment

Retry correctness uses two independent state dimensions:

1. What may have happened upstream?
2. What has already been committed downstream?

Minimum delivery evidence includes:

| Evidence | Meaning |
| --- | --- |
| `NotSent` | No business request bytes reached an upstream connection |
| `SendStarted` | A connection/write began; execution is uncertain |
| `RequestSent` | The complete request was written; upstream may execute or bill |
| `ResponseStarted` | An upstream response/event was observed |
| `ResponseCompleted` | Upstream completed successfully or with a parsed terminal error |

The downstream side independently tracks `Uncommitted`, `HeadersCommitted`, `BodyCommitted` and `Completed`.

Rules:

- The core owns the attempt loop and cross-provider fallback budget.
- A provider cannot hide a retry that can duplicate model execution, side effects or billing.
- HTTP-client transparent POST retries and redirects are disabled unless an operation-specific policy proves they preserve delivery evidence, credentials and replay safety.
- Authentication refresh and connection setup before `NotSent` may remain provider-internal.
- Timeout, 429, 5xx, EOF or network error names do not prove replay safety.
- Switching accounts or providers requires replay-safe upstream evidence and an uncommitted downstream response.
- Parallel hedging of model/business attempts is disabled in the first release because it can duplicate execution, side effects and usage. A future operation may opt in only after proving cancellation and billing semantics.
- No reroute occurs after client headers or body are committed.
- Tools or future mutating operations can further restrict replay through operation metadata.
- Every attempt is recorded with provider, target pseudonym, evidence, timings and normalized outcome.
- `AttemptStarted`, `AttemptFinalized` and request-terminal transitions use independent idempotency keys. A crash after start but before finalization recovers as an explicit `UnknownMayHaveExecuted` attempt rather than disappearing or being replayed automatically.

## 12. Scheduler And Capacity Model

### 12.1 Shared Contract, Optional Implementation

The gateway defines allocation semantics, not one mandatory algorithm. A provider chooses one of three patterns:

| Pattern | Use case |
| --- | --- |
| Shared default scheduler | Multi-account providers with priority, weights, RPM, concurrency, cooldown, sticky and health behavior |
| Module-private scheduler | Providers whose capacity or quota semantics cannot be represented by the default policy |
| Immediate/simple allocator | One remote endpoint or an upstream gateway that owns all internal balancing |

All patterns return the same lease and completion semantics.

### 12.2 Queue And Lease Lifecycle

```text
New -> Queued -> Granted -> Active -> Completed
              -> Rejected           -> Cancelled
              -> TimedOut           -> Expired
              -> Cancelled
```

- Queue admission is bounded and has an owner token, deadline, epoch and expiry.
- Grant versus cancellation is one atomic transition; late grants are impossible.
- Lease acquire, heartbeat, complete and cancel use ownership tokens, fencing epochs and idempotent completion IDs.
- Redis scripts/functions perform bounded work. Stale cleanup is batch-limited and never scans an unbounded set inside one command.
- TTL is crash recovery, not ordinary release.
- The provider lease represents one attempt's upstream capacity. After upstream production stops or cancellation wins, the synchronous attempt envelope is accepted and the lease is completed immediately; completion does not wait for a bounded downstream buffer, usage rollup or reporting projection. Slow clients cannot retain provider capacity indefinitely, while a crash cannot erase the only attempt outcome before release.
- Attempt terminal state and request terminal state are distinct: the former closes provider capacity and supplies attempt evidence; the latter records client delivery and all durable obligations. Each transition is idempotent and correlated by attempt/request IDs.
- Redis loss fails closed for new shared-capacity admission until the recovery barrier accounts for active replicas and leases.
- The provider receives completion outcomes for health/cooldown/credential decisions. The core does not mutate provider account state.
- The process resource governor is separate from provider capacity. A provider lease does not bypass global memory, stream or transformation ceilings.

## 13. Usage, Cache And Accounting

Usage is a pipeline of explicit fact types:

```text
Provider Raw Facts
+ Effective Request Revision
+ Cache Evidence
+ Route Reporting Policy
+ Pricing Revision
-> Canonical Actual Usage
-> Client Reported Usage
-> Operational Cost/Accounting Record
```

Required distinctions:

| Fact | Owner | Rule |
| --- | --- | --- |
| Provider raw usage | Provider usage interpreter | Preserve what the upstream actually returned, including unknown/partial status |
| Effective request facts | Request artifact/token module | Bind tokens and cache fingerprints to the exact transformed body revision |
| Cache evidence | Provider or cache evidence module | Never invent a hit/creation without identified evidence or an explicitly labeled simulation |
| Canonical actual usage | Usage domain | Normalize known facts without overwriting raw evidence |
| Client-reported usage | Client protocol policy | Produce protocol-compatible fields with a named projection revision |
| Pricing and cost | Pricing module | Bind calculation to currency, units and immutable pricing revision |
| Billable fact | Future accounting policy | Never alias an estimate as financial authority |

Provider modules can use a shared usage interpreter or implement their own. They must return versioned raw facts and confidence/provenance. The generic usage domain owns idempotent persistence, rollups, query, reconciliation and export.

Usage events are keyed by terminal/attempt idempotency IDs. PostgreSQL is authoritative. Redis summaries are rebuildable projections and cannot mark an event seen before its aggregate mutation is atomically secured.

### 13.1 Attempt Cost Versus Delivered Request Usage

- Every real attempt has its own raw usage/cost fact, including failed, timed-out, cancelled and fallback-replaced attempts.
- Any attempt with `SendStarted`, `RequestSent`, stronger evidence or crash-recovered `UnknownMayHaveExecuted` state is treated as potentially executed/billed even when the provider returns no final usage. Missing facts remain `unknown` or `partial`; they are never coerced to zero.
- Operational actual-cost views aggregate all distinct attempts that may have consumed upstream capacity or produced charges.
- Client-reported request usage projects only the attempt whose output was actually delivered. Because fallback is forbidden after downstream commitment, an earlier undisclosed failed attempt never leaks into client token fields but remains visible in operator cost/reconciliation views.
- Request-level usage stores the contributing attempt ID and projection revision. Attempt idempotency prevents duplicate accounting when finalization/outbox delivery repeats.
- Reconciliation exposes the gap between known provider usage, estimated/potential cost and delivered client usage instead of hiding it in one total.

## 14. Complete Kiro First-Release Module

### 14.1 Private Package Shape

The Kiro provider is a vertical module whose internal package layout may include:

```text
providers/kiro/
  module          registration and capability descriptor
  config          typed module configuration and validation
  credentials     credential types and secret references
  auth            Social/IdC/external IdP/API key refresh flows
  catalog         endpoints, regions, models, capability and quota sync
  scheduler       eligibility, ranking, sticky, cooldown and outcomes
  coordination    Redis queue/lease/RPM/concurrency/fencing implementation
  conversion      canonical operations to IDE/CLI requests
  transport       HTTP clients, proxy policy and one-attempt send
  eventstream     AWS EventStream framing, CRC and bounded decoder
  events          Kiro payloads to canonical typed events
  usage           raw token/cache/quota fact extraction
  errors          typed Kiro error classification
  admin           Kiro-specific commands, queries and UI schema
  jobs            refresh, health, model and quota maintenance
```

These are Kiro-private implementation details. Other providers can reuse pure libraries but do not import Kiro packages.

### 14.2 Kiro V1 Acceptance Matrix

| Area | Mandatory behavior |
| --- | --- |
| Endpoints | Kiro IDE and CLI endpoint families, region/URL selection and endpoint-specific envelopes |
| Authentication | Every currently supported Social, IdC, external IdP and API-key mode; refresh CAS/fencing; invalidation and safe secret rotation |
| Accounts | enable/disable, supported models, priority, weight, warmup/probation, proxy, health, cooldown and risk state |
| Capacity | global/per-account RPM, weighted concurrency, bounded wait queue, sticky session, exclusions and controlled fallback |
| Multi-replica | Redis lease acquire/heartbeat/complete/cancel, epoch/fencing, late-grant protection, crash and Redis-loss recovery |
| Catalog | model alias/capability discovery, model refresh, quota/overage synchronization and stale-state behavior |
| Requests | system/history, tools and schemas, tool results, thinking, images, documents, Files materialization, cache points and supported MCP/WebSearch behavior |
| Transport | bounded reusable clients, proxy/TLS policy, cancellation, idle/total deadlines, stream and non-stream paths |
| EventStream | official AWS Go decoder where compatible; otherwise an isolated bounded decoder with CRC, fragmentation, corruption and fuzz tests |
| Responses | canonical content/thinking/tool events, stop reasons, signatures, event order and normalized Kiro errors |
| Retry | exact delivery evidence, no hidden business replay, auth-before-send refresh and safe account/provider switching only |
| Usage | raw input/output/cache facts, partial/unknown representation, final event usage and quota evidence |
| Admin | credential create/test/rotate/disable, account status, scheduler reasons, lease/queue view, model/quota sync, health and diagnostics |
| Lifecycle | exactly-once completion intent across success, error, cancellation, disconnect, malformed stream, timeout, shutdown and crash recovery |

Kiro completion is not claimed from unit tests alone. It requires real Claude Code compatibility and a bounded, secret-safe real Kiro matrix.

## 15. Client Protocol Scope

### 15.1 Mandatory First Surface

- Anthropic Messages, stream and non-stream.
- Claude Code compatibility profile and model aliases.
- Models and count-tokens operations.
- Files-compatible upload/read/list/delete where required by Claude Code and current clients.
- Existing successful thinking/signature, tools/tool results, MCP, agent, image/document and error behavior.
- Route policy equivalents for current default, no-cache, high-cache, named cache and Claude Code routes, expressed as configuration rather than copied handler families.

### 15.2 Generic Contract Proof

The candidate includes a deterministic mock provider and one simple compatible HTTP provider. At least one integration test routes the same canonical message operation to Kiro and the non-Kiro provider without modifying the client adapter or execution core.

OpenAI Responses/Chat and Gemini client adapters are natural next built-ins, but they do not block the first complete Kiro release unless separately promoted. Their extension contracts must already be possible without a kernel redesign.

## 16. Configuration And Control Plane

### 16.1 Page-Configurable Runtime Policy

The Admin UI controls:

- provider modules and provider instances;
- credentials, accounts, proxies, endpoints and health state;
- pools, routes, model aliases, required capabilities and allowed lossiness;
- scheduler algorithm/configuration, priority, weights, RPM, concurrency, queue, sticky, warmup, cooldown and fallback;
- retry budgets and operation replay policies;
- usage, cache evidence, reporting, pricing and retention;
- Files/object storage policy and remote materialization limits;
- diagnostics, redaction, sampling and observability;
- maintenance jobs, model/quota refresh and account testing;
- API keys, Admin sessions/scopes and secret rotation;
- replica, queue, lease, outbox and recovery status.

### 16.2 Bootstrap-Only Settings

These remain process/deployment settings rather than mutable page configuration:

- PostgreSQL and Redis addresses/credentials;
- master encryption key provider and recovery material location;
- initial root Admin bootstrap credential;
- listeners, TLS/key source and trusted proxy boundaries;
- process role and hard safety ceilings that runtime config cannot exceed;
- object store bootstrap credentials;
- telemetry export endpoints required before configuration is available.

### 16.3 Publication Workflow

```text
Create Draft
-> Validate Core And Module Schemas
-> Resolve Cross-References And Capabilities
-> Compile Candidate Runtime Snapshot
-> Show Semantic Diff And Warnings
-> Publish With Expected Version
-> Commit Revision + Audit + Outbox In One Transaction
-> Notify Replicas
-> Each Replica Build/Validate Candidate
-> Atomically Swap Snapshot
-> Report Adoption Or Remain Not Ready
```

No handler mutates a live configuration object. A failed candidate never replaces the active snapshot. Rollback creates and publishes a new revision; history is immutable.

Module-specific configuration can be stored as typed, schema-versioned payloads opaque to the core. Secrets are separate references. The provider validates and compiles its payload before publication.

Candidate schema/domain/capability validation and snapshot compilation are deterministic and side-effect free. Endpoint connectivity, credential tests and model/quota probes are separate explicit commands whose dated results may inform the operator but never execute inside the CAS publication transaction. The transaction performs no remote network call and holds locks only for bounded authoritative writes.

## 17. Modern Admin Application

### 17.1 Selected Baseline

Use [shadcn-admin](https://github.com/satnaing/shadcn-admin) at reviewed commit `e16c87f213a5ba5e45964e9b67c792105ec74d26` as a selective UI pattern source, not a repository fork or architecture authority.

Create a fresh React application and selectively adapt its shell, navigation, command menu, responsive behavior, data-table interaction and form patterns. Remove demonstration authentication and business pages. Preserve the MIT notice and generate `THIRD_PARTY_NOTICES`.

### 17.2 Information Architecture

| Area | Primary workflows |
| --- | --- |
| Overview | health, traffic, latency, error, usage, queue and capacity summaries with direct drill-down |
| Providers | module inventory, instances, capabilities, endpoints, health and configuration |
| Kiro accounts | credentials, auth state, quota, models, priority, concurrency, RPM, cooldown, proxy, test and maintenance actions |
| Routes and models | aliases, capability constraints, ordered providers, fallback, cache/reporting and policy simulation |
| Scheduler | live queue/leases, capacity reasons, sticky bindings, cooldowns and safe diagnostic actions |
| Usage and costs | actual/reported/cache/cost facts, filters, exports, pricing revisions and reconciliation |
| Requests and errors | request/attempt timeline, normalized errors, delivery/commitment evidence and redacted diagnostics |
| Configuration | drafts, validation, semantic diff, publish history, adoption and rollback-to-new-revision |
| Operations | replicas, outbox, jobs, Files, backups, Redis recovery, shutdown and readiness |
| Security and audit | API keys, Admin sessions/scopes, secret rotation, audit trail and security events |

The UI is operational and dense. It does not use oversized hero sections, decorative card nesting or marketing composition. Tables and timelines are optimized for scanning and repeated actions.

### 17.3 Frontend Contract

- Go-owned OpenAPI generates the TypeScript client and error models.
- TanStack Query owns server state. Local UI state is limited and explicit.
- Server-side pagination/filter/sort is mandatory for unbounded data.
- Long logs/timelines use virtualization and bounded live updates.
- OpenAPI plus Go domain validation is the only server contract authority. React Hook Form plus Zod provides client UX validation; parity/drift tests prove that it does not redefine or weaken authoritative fields and cross-field rules.
- Secrets are write-only and never persisted in `localStorage`.
- Authentication uses secure HttpOnly cookies with CSRF protection or an explicitly reviewed equivalent.
- Every mutation supports pending, success, conflict, validation, retry-safe and partial-failure states.

## 18. Storage And State Ownership

| State class | Durable authority | Redis/local role | Recovery rule |
| --- | --- | --- | --- |
| Config revisions | PostgreSQL immutable revisions | local compiled snapshot; Redis/notify invalidation | rebuild from last published revision; failed replicas not ready |
| Provider configuration | provider-owned PostgreSQL schema/payload revision | local typed compiled view | module migration and validation before adoption |
| Secrets | encrypted PostgreSQL envelope plus external master key | decrypted values only in bounded memory | versioned key ring and tested rewrap/recovery |
| Scheduler leases/queues | Redis coordination namespace | local handles and supervised pending completion | fencing, TTL, epoch/release barrier and fail-closed admission |
| Account health/cooldown | provider-owned durable facts where required | Redis fast coordination/expiry | rebuild or reconcile by classified state |
| Attempts/terminal | PostgreSQL synchronous idempotent `Started`/`Finalized`/request-terminal journal | no volatile buffer as sole authority; local buffers are projection-only | recover unfinished starts as explicit unknown/may-have-executed facts; replay final envelopes by idempotency key |
| Usage | attempt raw facts and request-delivery facts accepted with terminal journal; PostgreSQL rollups | Redis rebuildable recent summaries | preserve partial/unknown attempt facts; projection checkpoint and full rebuild |
| Audit/outbox | same PostgreSQL transaction as business mutation | claimed worker state | durable retry with idempotent consumers |
| Files | S3-compatible object store plus PostgreSQL metadata | bounded local streaming buffers | shared across replicas; lifecycle/reconciliation jobs |
| Diagnostics | bounded object/filesystem sink with manifest | bounded in-memory sampling | retention and redaction; never sole release evidence |

Migrations are domain-owned immutable files executed by one common runner with checksums, advisory fencing, backup prerequisites and previous-release compatibility rules. Startup does not perform unbounded ad hoc table scans or overwrite migration checksums.

## 19. High Availability And Multi-Replica Correctness

- Data-plane replicas are stateless except bounded caches and active lease handles.
- Every request uses one published configuration revision from start to terminal completion.
- Configuration invalidation is a hint; revision polling and readiness repair missed notifications.
- API-key revocation, provider disablement and secret rotation publish versioned invalidation visible to every replica.
- Redis queue/lease/RPM/concurrency/cooldown/sticky transitions are atomic, fenced, idempotent and bounded.
- No local scheduling fallback is allowed when shared Redis coordination is required. Degraded single-instance behavior, if ever added, must be an explicit deployment mode.
- PostgreSQL commands use expected versions, transactions and outbox. Admin success is not returned before the authoritative transaction commits.
- Files use shared object storage rather than process memory or a replica-local filesystem.
- Jobs use distributed claims, heartbeat and idempotent execution. A process crash cannot leave a permanently claimed job.
- Readiness reflects database, Redis, configuration adoption, secret availability, module initialization and recovery barriers, not only an open TCP listener.
- Compatible release generations may overlap only after proving that they share the same coordination schema, key semantics, scripts/functions and completion rules for the same provider capacity.
- Incompatible generations cannot both admit against the same upstream accounts. The old generation first closes new acquire, cancels/drains queues, completes or reserves every active lease and passes the generation barrier; only then may the new generation become ready and acquire capacity. A separate Redis namespace is migration storage, not permission for concurrent double allocation.

### 19.1 Startup And Producer-Aware Shutdown

Startup applies migrations through the fenced runner, checks key material and dependencies, loads the last published configuration, compiles every enabled module, reconciles scheduler/release generation, starts supervised workers and opens readiness only after all required authorities acknowledge the same generation.

Shutdown follows one ordered lifecycle:

```text
Running
-> Close Readiness And New Admission
-> Quiesce Periodic/Job Producers
-> Drain Or Cancel In-Flight Request Producers
-> Join Every Registered Producer
-> Close Terminal/Usage/Audit/Outbox Writer Ingress
-> Drain Durable Consumers And Reconcile Leases
-> Close Redis, PostgreSQL, Object Store And HTTP Transports
-> Emit Machine-Readable Shutdown Report And Exit
```

Every goroutine that can enqueue terminal, usage, audit, outbox, job or lease work registers with the lifecycle supervisor. Writer ingress cannot close while any registered producer remains. Critical accepted-but-uncommitted residue produces a non-zero exit and durable recovery record; logging and returning success is forbidden.

## 20. Performance And Resource Plan

### 20.1 Hot-Path Rules

- The request path performs no PostgreSQL query after immutable snapshot capture except the bounded `AttemptStarted` and attempt/request terminal-envelope transactions required around every possible business send, or another explicitly accepted durable operation.
- Routing and capability planning use immutable in-memory indexes.
- Redis round trips are minimized through bounded atomic functions and carefully measured pipelining.
- Stable HTTP transports and connections are reused. Transport caches have capacity, TTL, idle retirement and secret/proxy invalidation.
- Streaming is incremental from upstream decoder to client encoder with fixed buffers and backpressure.
- No token event causes a database write, heap-wide clone or unbounded channel send.
- Request transformations use revisioned artifacts to avoid repeated JSON parse/serialize/canonicalize/tokenize work.
- Expensive PDF, image, remote fetch and tokenizer work is admitted by weighted resource budgets and cancellable worker pools.
- Usage rollups, reporting, audit export and other projections use durable bounded batching outside the latency-critical stream. Attempt starts, raw attempt usage, attempt finalization and request-terminal acceptance are synchronous idempotent journal operations and are included in gateway-overhead benchmarks.
- Standard `net/http` remains the default. `fasthttp`, alternative JSON engines and unsafe pooling require measured end-to-end evidence and compatibility review.

### 20.2 Mandatory Bounds

Every deployment defines finite non-zero limits for:

- inbound connections, requests, request-body bytes and JSON depth;
- active streams and per-stream buffer/event/frame size;
- global and provider wait queues, wait time and lease lifetime;
- remote fetch count, per-object bytes, aggregate bytes, redirects, DNS results and transform work;
- upstream response/error bytes and decompression expansion;
- HTTP transports, idle connections, proxy variants and DNS cache;
- Files count, bytes, metadata and object lifecycle;
- caches, logs, diagnostics, label cardinality and retained traces;
- background jobs, outbox backlog, writer batches and retry age;
- process memory, file descriptors, goroutines and graceful shutdown deadlines.

`0 = unlimited` is not an accepted production default.

### 20.3 Initial Candidate Performance Gates

The exact harness pins hardware, kernel, Go version, PostgreSQL/Redis topology, payloads and mock-upstream latency. Initial minimums on an 8 vCPU/16 GiB Linux data-plane replica are:

| Gate | Initial target, excluding configured upstream latency |
| --- | --- |
| Small request gateway overhead | p95 at most 5 ms and p99 at most 15 ms at 1,000 requests/second with a local deterministic upstream |
| Streaming first-event overhead | p95 at most 10 ms and p99 at most 25 ms with 2,000 concurrent bounded streams |
| Sustained capacity | at least 1,000 small non-stream requests/second or 2,000 concurrent streams per replica without overload errors below configured limits |
| Horizontal scaling | two and four replicas achieve at least 70% efficiency after shared Redis/PostgreSQL cost is included |
| Stability | 60 minutes and at least 100,000 mixed requests with no monotonic goroutine, FD, queue, connection or memory growth |
| Recovery | after load stops, goroutines/FDs/queues/connections return within 10% of pre-run steady state inside five minutes |
| Config adoption | 99% of healthy replicas adopt an accepted revision within two seconds; no request observes mixed revisions |
| Overload | bounded rejection occurs before memory or dependency collapse; admitted request SLO remains within the documented degradation budget |

These are planning gates, not current performance claims. A reproducible baseline may tighten them. Relaxation requires an explicit decision with workload evidence.

### 20.4 Admin UI Performance Gates

The browser harness pins browser version, viewport, CPU/network profile and deterministic datasets. Minimum gates are:

| Gate | Initial target |
| --- | --- |
| Route payload | code-split production build with a documented compressed initial-route budget and no duplicate component/chart framework |
| Interaction latency | p75 Interaction to Next Paint at most 200 ms across the accepted operator workflow corpus |
| Large table | server-side dataset of at least 100,000 rows, page size 100, with post-response render/settle p95 at most 300 ms and no full-dataset browser retention |
| Long timeline/log | at least 50,000 synthetic events through a virtualized bounded window, stable keyboard/focus semantics and no long-task regression above the accepted trace budget |
| Live health | 20 updates/second for 30 minutes with coalescing, a finite pending-update queue, no layout shift and no monotonic heap/listener growth |
| Memory recovery | after closing large/live views and forcing the documented idle/GC observation, retained heap returns within 20% of the pre-view steady state |
| Visual stability | light/dark themes across desktop/tablet/mobile screenshot baselines; loading, empty, error, stale, reconnect and long-text states cause no overlap or layout shift |

The implementation must turn the route-payload and long-task trace budgets into exact numbers after the selected component subset is known. They may not remain subjective release checks.

## 21. Security Plan

- Request API keys use keyed hashes, prefixes and constant-time verification; human passwords use Argon2id.
- Admin uses secure HttpOnly, SameSite cookies, CSRF protection, session rotation and bounded login attempts. Reusable Admin secrets never live in browser storage.
- Upstream credentials use versioned AEAD envelope encryption and an external master-key provider. Reads return only redacted metadata.
- Module config separates secrets from ordinary JSON/schema payloads.
- Remote URLs use allowlisted schemes, dial-time DNS/IP enforcement, private-address policy, redirect limits and cross-origin credential stripping.
- Outbound headers use provider-owned allowlists. Hop-by-hop and inbound authentication headers never leak upstream.
- Request bodies, prompts, tool schemas and provider payloads are redacted or absent by default in logs and traces.
- Admin mutations append audit records in the same transaction and include actor/session, revision, semantic change and result without plaintext secrets.
- Public/Admin listeners can be separated; Kubernetes NetworkPolicy and TLS/mTLS options are documented.
- CI runs dependency, license (including file-level copyleft such as MPL-2.0), secret, SAST, container and SBOM scans. Release artifacts are signed and carry provenance.

## 22. Observability And Operations

### 22.1 Correlation Model

Every request has a stable request ID. Every provider execution has an attempt ID. Leases, terminal outcomes, usage events, configuration revisions and outbox records carry the relevant IDs without exposing secret account identifiers.

### 22.2 Required Metrics

- admission, active requests/streams, body/resource budget and overload reasons;
- route/module/model selections and capability rejection reasons;
- scheduler queue, wait, lease, heartbeat, completion, expiry and pending-release state;
- attempt counts, delivery evidence, retry decisions, upstream first byte/event and total latency;
- downstream commitment and disconnect state;
- actual/reported/cache usage, projection lag and reconciliation failures;
- PostgreSQL pool/wait/query, Redis command/script, outbox/job and object-store health;
- config publication/adoption, replica generation/readiness and shutdown residue;
- goroutines, memory, GC, file descriptors, sockets and transport-cache state.

Metrics use controlled labels. Account IDs, request IDs, paths with arbitrary names and raw error strings belong in logs/traces, not metric labels.

### 22.3 Diagnostics

Diagnostics are sampled, bounded, encrypted or redacted, retention-controlled and linked to an explicit operator action. They cannot default to capturing full tool definitions, prompts or response bodies.

## 23. Technology Stack

Versions are pinned when the new repository is created. The following major choices were verified or selected on 2026-07-13.

### 23.1 Backend And Contracts

| Area | Choice | Reason |
| --- | --- | --- |
| Language | Go 1.26.x, current official patch observed as 1.26.5 | current stable toolchain, strong standard HTTP/concurrency/runtime support |
| HTTP | standard `net/http` plus `go-chi/chi` | HTTP/2, cancellation, streaming, compatibility and small composable router surface |
| Public/Admin contract | OpenAPI 3.x supported subset with `oapi-codegen/oapi-codegen` and `getkin/kin-openapi` | generated strict Go bindings, validation and one HTTP contract authority |
| Future external modules | Protobuf plus Connect/gRPC using `connectrpc/connect-go` | language-neutral, versioned process boundary without Go plugin ABI |
| PostgreSQL | `jackc/pgx/v5`, `sqlc`, `pressly/goose/v3` | mature driver, generated typed queries and immutable migrations |
| Redis | `redis/rueidis` with versioned Lua/Functions | Cluster/Sentinel support, pipelining and explicit atomic coordination |
| IDs | ULID or UUIDv7 behind one typed ID package | sortable, non-secret request/attempt/config identities |
| Validation | generated schema plus explicit domain validation | avoids tag-only validation hiding cross-field rules |
| Concurrency | `context`, `x/sync/errgroup`, weighted semaphores and `x/time/rate` where local only | cancellation and bounded structured concurrency |
| Kiro EventStream | official AWS SDK for Go v2 eventstream decoder where compatible | mature framing/CRC implementation; custom fallback remains isolated and fuzzed |
| Logging | standard `log/slog` with typed redaction adapters | structured standard library logging without global logger state |
| Telemetry | OpenTelemetry Go and Prometheus `client_golang` | standard traces/metrics correlation and ecosystem support |
| Bootstrap config | typed environment/file parser used only at composition root | runtime product config remains versioned in PostgreSQL |
| Code composition | explicit constructors and small interfaces declared by consumers | no DI container or service locator |

### 23.2 Frontend

| Area | Choice | Reason |
| --- | --- | --- |
| Runtime | React 19.x | current stable React line |
| Language | TypeScript strict, current stable pinned at bootstrap | generated types and exhaustive state handling |
| Toolchain | Node.js 24 LTS and pnpm 11, exact versions recorded in `.node-version`/tool manifest and `packageManager` | reproducible builds; reviewed values were Node 24.18.0 and pnpm 11.12.0 |
| Build | Vite 8.x | fast SPA build without an unnecessary server-rendering framework |
| Styling | Tailwind CSS 4.x | required modern utility CSS and design-token workflow |
| Components | shadcn/ui plus Radix primitives | source-owned accessible components and controlled styling |
| Icons | Lucide React | open-source, consistent tool/action icon set |
| Routing | TanStack Router | typed operational routes and search parameters |
| Server state | TanStack Query | cache, invalidation, retries and mutation state |
| Tables | TanStack Table plus virtualization where needed | server-driven large operational datasets |
| Forms | React Hook Form plus Zod as client UX validation only | complex provider/config forms without creating a second server contract authority |
| Charts | Recharts initially | template-aligned operational charts; switch only on measured large-series need |
| API client | `openapi-typescript` plus `openapi-fetch` generated from the Go-owned OpenAPI document | no handwritten duplicate DTOs or broad client framework |
| Tests | Vitest, Testing Library, `axe-core`, `@axe-core/playwright` and Playwright | component, automated accessibility and real-browser workflow coverage; automation is not the complete WCAG gate |

At the verification date, official latest packages included React 19.2.7, Tailwind CSS 4.3.2, TypeScript 7.0.2 and Vite 8.1.4. The repository pins exact compatible versions rather than automatically tracking `latest`.

The reviewed shadcn-admin commit used React 19.2.x, TypeScript 6.0.x, Tailwind 4.2.x and Vite 8.0.x. Before selective source adaptation, W1 records source and target versions and runs a compatibility spike covering install, typecheck, build, unit, accessibility and browser smoke tests. Target versions are locked only after that gate passes; TypeScript 7 or a later Tailwind 4 minor is never assumed compatible from major-version labels alone.

### 23.3 Test, Deployment And Supply Chain

| Area | Choice |
| --- | --- |
| Go unit/property/fuzz | standard `testing`, `go-cmp`, built-in fuzzing and `goleak` where useful |
| Integration | Testcontainers for PostgreSQL/Redis/object store; Toxiproxy or equivalent dependency fault injection |
| Load/soak | k6 plus a Go deterministic upstream and purpose-built scheduler/stream harnesses |
| Browser | Playwright desktop/mobile matrix and visual/accessibility checks |
| Packaging | reproducible multi-stage Docker build, minimal non-root image and read-only filesystem where possible |
| Local deployment | Docker Compose with real health/readiness checks |
| HA deployment | Kubernetes manifests/Helm, PDB, topology spread, NetworkPolicy, probes and separate roles |
| Security/release | `govulncheck`, dependency/license scan, Trivy, Syft SBOM, Cosign signing and provenance |

## 24. Target Repository Shape

```text
ai-gateway/
  cmd/
    gatewayd/                 process and role composition
    gatewayctl/               operator/import/diagnostic CLI
  api/
    openapi/                  public and Admin source contracts
    proto/                    future process-module contracts
  internal/
    kernel/
      invocation/
      capabilities/
      routing/
      attempts/
      terminal/
      resources/
    gateways/
      anthropic/
      claudecode/
    providers/
      kiro/
      compatiblehttp/
      mock/
    scheduling/
      contract/
      defaultimpl/
    usage/
    config/
    secrets/
    audit/
    outbox/
    files/
    admin/
    platform/
      postgres/
      redis/
      objectstore/
      transport/
      telemetry/
      lifecycle/
    bootstrap/                only composition root
  migrations/                indexed domain-owned migration sets
  web/admin/                  one React application
  deploy/
    compose/
    kubernetes/
  tests/
    contract/
    fixtures/
    integration/
    realclient/
    load/
    chaos/
    browser/
  docs/
    architecture/
    operations/
    evidence/
```

Package names such as `common`, `utils`, `services`, `manager` or `helpers` are not allowed to become cross-domain dumping grounds. Shared code must have a narrow semantic name and a stable consumer.

## 25. Architecture Fitness Rules

CI enforces:

- generic kernel packages cannot import any provider or client-protocol implementation;
- providers cannot import other providers;
- client adapters cannot import providers or storage adapters;
- domain packages cannot import PostgreSQL/Redis concrete adapters;
- only bootstrap composes concrete implementations;
- only a provider's package can access its private tables, key prefix and credential types;
- generated contracts are reproducible and the working tree remains clean after generation;
- interfaces remain small and are declared by consumers;
- dependency cycles and forbidden broad package names fail CI;
- no unbounded channel, unlimited production default or detached goroutine is accepted without a named architecture exception.

## 26. Verification And Acceptance

### 26.1 Static And Unit Gates

- `go test`, race detector, vet, lint, dependency boundary and vulnerability gates.
- Unit and property tests for route decisions, capability negotiation, retry matrices, usage conservation and scheduler ranking.
- Fuzz tests for every wire decoder, especially EventStream, SSE, JSON depth and fragmented/corrupt input.
- React typecheck, lint, unit, accessibility and generated-client drift gates.

### 26.2 Contract Gates

- Every provider runs the same module descriptor, allocation, attempt, cancellation, terminal, usage and error suite.
- Kiro, compatible HTTP and mock providers prove the core has no provider-specific assumption.
- Faults before send, during request write, after full write and during response streaming prove that direct `Execute` errors mean `NotSent` and every post-send failure remains available through a non-nil attempt session/result.
- Multi-attempt tests prove one fresh lease per attempt, no overlap with an unreleased prior attempt and no account/provider outcome attributed to the wrong lease.
- Every client adapter runs canonical-event order, errors, stream/non-stream and capability rejection fixtures.
- Unknown required extensions and unauthorized lossy conversions fail before upstream send.

### 26.3 Storage And HA Gates

- Concurrent config publish, stale CAS, partial failure, outbox replay and replica adoption.
- Crash before/after `AttemptStarted`, send start, attempt finalization, request-terminal acceptance, lease completion and final client marker; recover without invisible attempts, unsafe replay or duplicate usage.
- Queue grant/cancel, heartbeat/complete, duplicate completion, stale fencing token, Redis restart/flush and missing-replica recovery.
- Compatible rolling generations share and preserve capacity correctly; incompatible generations prove zero overlapping acquire admission before the new generation becomes ready.
- PostgreSQL failover, pool exhaustion, slow queries, migration failure, backup/restore and previous-release rollback.
- Shared Files behavior across replicas, object-store failure and metadata/payload reconciliation.

### 26.4 Kiro And Claude Code Gates

- IDE/CLI endpoints and every accepted auth mode.
- Streaming/non-stream, thinking/signature, tools/tool results, agents, MCP, images/documents, Files, model aliases, count-tokens, errors and final usage.
- At least three real Claude Code sessions with 20-plus conversational turns and controlled tool/agent/MCP workflows.
- Bounded real Kiro requests with call caps, credential redaction, artifact manifests and explicit skipped-capability results.

### 26.5 Load, Chaos And Lifecycle Gates

- 10/100/1,000 Kiro accounts or synthetic targets; scheduler fairness, Redis work bound and acquire/complete latency.
- Mixed bodies, tools, remote media, long first-token delay, long streams, slow/disconnected clients and malformed upstream events.
- Multi-attempt scenarios prove that every potentially billed attempt appears once in operational cost while only delivered usage appears in the client projection.
- One/two/four replicas, Redis/PostgreSQL/object-store latency/loss, network partitions, process kills and rolling restart.
- SIGTERM at queue wait, connect, request send, stream, terminal, usage write and outbox stages.
- Clean recovery of memory, goroutines, file descriptors, sockets, queues, leases and worker claims.

### 26.6 Admin Browser Gates

- Every domain workflow on Chromium, Firefox and WebKit.
- Desktop, tablet and mobile layouts at normal, 200% and 400% zoom/reflow with no overlap, clipped controls or two-dimensional scrolling except genuinely two-dimensional data regions.
- Keyboard-only navigation, stable focus order, visible focus, labels, dialogs, error summaries, status/live-region announcements and contrast checks.
- `prefers-reduced-motion`, forced-colors/high-contrast and OS/browser text-size settings preserve every operation.
- At least VoiceOver plus Safari and NVDA plus Firefox/Chromium are tested for the complete critical-workflow subset; `axe-core` automation alone is not WCAG 2.2 AA evidence.
- Virtual tables/logs preserve row semantics, reading order, selection and focus while recycling DOM nodes.
- Light/dark by desktop/tablet/mobile screenshot regression covers long provider/model/error text and loading, empty, validation, error, stale, reconnect, conflict, partial-failure and long-running states.
- Large-table, virtual-log and high-frequency health datasets pass the UI performance and bounded-memory gates in section 20.4.
- Every unfamiliar icon-only action has an accessible name and visible hover/focus tooltip; familiar destructive actions still require unambiguous confirmation.
- Conflict, stale revision, partial failure, reconnect and long-running action states.
- Secret non-disclosure in DOM, storage, URL, logs, screenshots and API reads.

## 27. Final Cutover And Whole-System Rollback

The new system uses its own PostgreSQL database/schema, Redis namespace and object-store prefix. This prevents greenfield development from mutating the legacy production state and keeps rollback credible.

Cutover sequence:

1. Freeze and identify the complete candidate digest, generated contracts, migrations, images, SBOM, signatures and evidence manifest.
2. Back up the legacy system and the new target stores.
3. Run the isolated one-time import tool for explicitly selected configuration and credentials; compile and publish one target configuration revision.
4. Start target roles on private listeners, keep public readiness closed, and run storage, provider, Kiro, Claude Code and Admin smoke gates.
5. Stop legacy admission and drain it through its supported shutdown path.
6. Open target readiness and switch the whole public/Admin endpoint to the Go system.
7. Observe the target with fixed error, latency, usage, lease, outbox and resource abort thresholds.
8. Keep the previous whole Rust artifact and untouched legacy stores available through the observation window.

Rollback sequence:

1. Close target readiness and stop new admission.
2. Drain/cancel target producers, record terminal, outbox and lease residue, and preserve the new database for audit.
3. Restore routing to the previous Rust artifact and its unchanged stores.
4. Run previous-system smoke checks before reopening public traffic.
5. Do not attempt request-level dual routing, reverse-migrate target writes during the incident or delete target evidence.

## 28. Legacy Documentation And Evidence Treatment

Keep as behavioral evidence:

- current source, tests and fixtures at a pinned oracle revision;
- current business context, runtime flows, protocol contracts, resource model, storage model and risk hotspots;
- verified Kiro/Claude Code, scheduler, usage/cache, Files and external-pool behavior;
- conservative replay, terminal, scheduler/lease and producer-aware shutdown invariants from previous decisions 003-006;
- dated real-client, load, release and failure evidence that can be reproduced safely.

Keep only as historical planning context:

- the Rust target architecture and 50-module ledger;
- the two-frontend rewrite requirement;
- Rust-to-TypeScript contract generation;
- legacy-compatible schema migration and phased target integration details;
- package/file split instructions tied to current Rust symbols.

Do not delete the old planning tree in this pass. It is marked superseded and remains a searchable source map. A later coherent archive pass may move it after inbound links, source provenance and rollback paths are recorded.

## 29. Failure Patterns To Reject During Implementation

- A `Gateway`, `Provider`, `Manager` or `AppState` object with dozens of unrelated methods.
- Generic packages importing Kiro because it is the first provider.
- One canonical request with hundreds of provider/client optional fields.
- Provider retries that do not report each real send and delivery evidence.
- A terminal reducer that directly owns leases, credential storage, usage rollups and response writing.
- Runtime config held in mutable global structs or read multiple times during one request.
- Generic JSONB used without a schema ID/version, typed decoder and module validation.
- Direct provider-to-provider imports or reads of another module's tables/Redis keys.
- Unbounded channels, body collection, client caches, diagnostics or `0 = unlimited` defaults.
- Database writes or blocking work per streamed token.
- A Go plugin ABI or premature microservice split used as a substitute for good contracts.
- Copying an Admin template wholesale and treating demo CRUD, authentication or charts as completed product workflows.
- Performance claims copied from reference projects rather than reproduced on the target candidate.

## 30. Definition Of Done

The reconstruction is complete only when:

1. The new repository contains the complete target-only Go/React system and no runtime dependency on `kiro.rs`.
2. Kiro passes the full provider, scheduler, protocol, usage, Admin, multi-replica and real-client matrix.
3. A mock provider and a non-Kiro compatible provider prove the contracts are genuinely generic.
4. Adding a provider or client protocol requires only its new module, schemas, registration and tests, with architecture fitness checks proving no forbidden edits.
5. All runtime configuration workflows are available through the modern Admin UI with validation, diff, CAS publication, audit and replica adoption.
6. PostgreSQL/Redis/object storage, retries, terminal completion, usage and shutdown satisfy their consistency and recovery contracts.
7. Performance, load, chaos, browser, accessibility, security, backup/restore, supply-chain and release gates pass for one immutable digest.
8. One final whole-system cutover succeeds, or the rehearsed previous whole-system rollback succeeds without mixed execution.
9. Documentation, runbooks, generated contracts, schema migrations, SBOM, notices, signatures and evidence identify the released digest.
10. The observation window completes without unresolved critical residue, data inconsistency, resource growth or compatibility regression.
