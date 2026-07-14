# Greenfield AI Gateway

Role: Greenfield Go AI model gateway target-plan entrypoint

Status: Architecture plan ready for review; implementation Not Started; cutover Not Ready

Authority: Owns the new product target, module boundaries, technology direction, complete implementation work graph, acceptance gates, and relationship to the current Rust system

As of: 2026-07-13

Supersedes: [System architecture modernization](../system-architecture-modernization/README.md) as target implementation authority

Related: [Plan Tree](../../README.md), [current business context](../../baseline/business-context.md), [current runtime flows](../../baseline/runtime-flows.md), [current risks](../../baseline/risk-hotspots.md)

## Purpose

Build a new extensible, high-performance, highly available AI model gateway in a new repository. This is not an incremental refactor of `kiro.rs`, a mechanical Rust-to-Go translation, or a repackaging of the current large files.

The current repository is retained as a behavioral oracle for Kiro, Anthropic and Claude Code compatibility, scheduling semantics, usage/cache behavior, error normalization, fixtures, failure cases, and real-client validation. New production code must not import or embed the current Rust runtime.

## Final Goal

Deliver one complete greenfield system with:

- a Go data plane and control plane built as a domain-oriented modular monolith;
- stable, typed and versioned contracts between client-protocol modules, the execution core, provider modules, and platform services;
- Kiro as the first fully implemented provider module rather than the architectural center of the system;
- Anthropic Messages and real Claude Code compatibility as the mandatory first client protocol surface;
- a module model in which future upstream APIs or API gateways may reuse shared scheduling, usage and conversion implementations or provide their own implementations behind the same contracts;
- one modern React, TypeScript and Tailwind CSS Admin application based on a reviewed open-source template and open-source icons;
- PostgreSQL durable authority, Redis distributed coordination, immutable request configuration snapshots, multi-replica correctness, bounded resources, streaming backpressure, observability and recovery;
- Docker Compose and Kubernetes deployment assets;
- one final whole-system cutover after the complete candidate passes all contract, real-client, load, chaos, recovery, security and browser gates.

Implementation is organized by module and dependency inside one program. Dependency groups are not staged product migrations, partial releases or deadlines. The old Rust system remains independent and production-authoritative until the complete Go candidate is accepted.

## Target Product Boundary

The planning defaults are deliberately conservative and may be changed only by an explicit decision:

| Question | Target default |
| --- | --- |
| Product model | Self-hosted, single operator and single trust domain; multiple API keys, accounts and replicas are not tenants |
| First client protocols | Anthropic Messages plus the Claude Code compatibility profile; Models, Files and count-tokens behavior retained where required by current compatibility |
| First provider | Complete Kiro IDE and CLI integration, including all currently supported authentication and scheduling behavior |
| Generic proof | A deterministic mock provider and one simple OpenAI/Anthropic-compatible HTTP provider exercise the same contracts without importing Kiro types |
| Module loading | Compile-time registration in the first release; optional future out-of-process modules use Protobuf plus Connect/gRPC |
| Admin surface | One new React application; neither current frontend is retained as an implementation base |
| Deployment | One codebase with selectable data-plane, control-plane and worker roles; all-in-one Compose and separated Kubernetes deployments |
| Usage objective | Exact operational facts and auditable reporting; financial invoicing and multi-tenant billing are not first-release claims |
| Migration | No legacy runtime or schema compatibility layer; an isolated one-time import/export tool may transfer explicitly selected operator configuration |

## Final Runtime Shape

```text
Clients
  -> Client protocol adapters
  -> Admission and immutable runtime capture
  -> Route and capability planning
  -> Provider module selection
  -> Provider-owned or shared-default allocation
  -> One observable upstream attempt
  -> Canonical typed events and delivery evidence
  -> Client protocol response encoder
  -> Terminal, usage and durable outbox completion

Control plane
  -> Draft and validate candidate configuration
  -> CAS publish one immutable configuration revision
  -> Audit and outbox in the same transaction
  -> Replicas atomically adopt the published revision
```

## Binding Architecture Rules

1. Shared contracts define semantics and invariants; they do not force every provider to share one scheduler, usage parser or retry implementation.
2. A provider may compose shared default components or provide module-private implementations, but the core sees only declared capabilities, typed inputs, leases, canonical events, delivery evidence, raw usage facts and terminal outcomes.
3. Client protocols and upstream providers are independent extension axes. Adding one must not require modifying the other or the scheduler core.
4. Contracts are operation-specific and capability-specific. There is no giant optional-field `Provider` interface and no `map[string]any` command bus.
5. Provider modules own their credentials, wire codecs, transport, model discovery, error classification, scheduling policy and provider-specific state. They cannot read another provider's tables, Redis keys or private types.
6. The execution core owns cross-provider routing, replay safety, downstream response commitment, attempt history and one terminal decision. Providers cannot hide business retries that may duplicate execution or billing.
7. Raw upstream usage, effective request facts, cache evidence, client-reported usage and billable facts are distinct types and persistence fields.
8. PostgreSQL is durable authority; Redis owns only explicitly classified coordination or rebuildable projections. Every distributed mutation has atomicity, idempotency, fencing and recovery semantics.
9. Every queue, body, buffer, stream, connection, cache, worker, file and diagnostic artifact has an explicit bound and cancellation path.
10. No global `AppState`, service locator, dependency-injection container, generic repository, runtime Go `.so` plugin or package named `common` may become the new dependency center.
11. Every real attempt owns a fresh lease and durable `AttemptStarted`/`AttemptFinalized` facts; request terminal state is outside the retry loop, and no lease or potentially billed usage disappears when fallback occurs.
12. Compatible release generations share one proven coordination schema. Incompatible generations never acquire capacity concurrently against the same upstream accounts.

## Plan Documents

- [Complete reconstruction plan](topics/complete-reconstruction-plan.md): goals, final behavior, architecture, modules, contracts, Kiro scope, technology stack, HA, performance, security, UI, validation and cutover.
- [Reference projects and template selection](topics/reference-projects-and-template-selection.md): reviewed open-source gateways, reusable architectural patterns, license constraints, Admin template comparison and selected frontend baseline.
- [Complete implementation work graph](roadmap.md): dependency-ordered module construction inside one final candidate, without phased product delivery.
- [Decision 001](decisions/001-greenfield-go-modular-ai-gateway.md): accepted replacement of the Rust modernization target with a greenfield Go modular gateway.

## Current State

The architecture plan and working defaults are documented. No target repository, Go source, React source, schema, container, migration, benchmark or validation artifact exists yet. Planning content is not implementation or performance evidence.

The next target is review and acceptance of this plan, followed by creation of a separate repository and implementation of the complete dependency work graph. An `implementation-status.md` must be created only when target source implementation actually begins.
