# Baseline

Role: Index of project-wide current-state facts

Status: Current as of 2026-07-11

Authority: Dated retrieval map for the implementation at `v0.0.102` / `e9479df`; source code, schema, configuration, and tests remain authoritative for exact current behavior

Read when: Understanding the current Rust product or implementation before opening a target reconstruction topic

Related: [Plan Tree](../README.md), [Greenfield AI Gateway](../plans/greenfield-ai-gateway/README.md), [superseded Rust modernization](../plans/system-architecture-modernization/README.md)

This baseline records project-wide facts that plans can reference without duplicating context. It describes the current system, not the proposed target architecture. Refresh the affected map from code before a large refactor and update its `As of` revision when current behavior changes materially.

## Reading Paths

### Product And Architecture

1. [Business and product context](business-context.md): product definition, single-operator trust model, actors, route meaning, cache/usage semantics, invariants, quality requirements, and non-goals.
2. [Current system context](system-context.md): deployed components, runtime composition, current ownership, data authority, and stable/unstable boundaries.
3. [Module map](module-map.md): current source modules and their responsibilities.

### Runtime And State

1. [Runtime flows](runtime-flows.md): request, routing, upstream, usage, Admin, startup, reload, and shutdown paths.
2. [Storage and state](storage-and-state.md): PgSQL, Redis, process memory, filesystem state, consistency, and lifecycle boundaries.
3. [Protocol and API contracts](protocol-and-api-contracts.md): Anthropic, Claude Code, Kiro, external-pool, Admin, health, and compatibility contracts.
4. [Resource and concurrency model](resource-and-concurrency-model.md): queues, leases, request/body budgets, background work, memory, file descriptors, and backpressure.

### Deployment And Assurance

1. [Deployment and operations](deployment-and-operations.md): process topology, required dependencies, health/readiness, observability, artifacts, shutdown, and recovery.
2. [Test and release gates](test-and-release-gates.md): static, storage, frontend, protocol, load/chaos, Docker, and release verification.
3. [Risk hotspots](risk-hotspots.md): current correctness, security, performance, compatibility, lifecycle, and maintainability concentration points.

## Current System Summary

- `kiro-rs` is a self-hosted, single-operator Anthropic/Claude Code compatibility gateway; it has no multi-user or multi-tenant product boundary.
- Requests can use operator-owned Kiro credentials or configured external compatible pools with raw or normalized body behavior.
- PgSQL is the durable authority for configuration and operational records; Redis coordinates cross-process transient state and derived realtime views.
- Multiple replicas are an availability and capacity concern inside the same operator trust domain, not a tenant boundary.
- `/v1/messages`, `/na/v1/messages`, `/ha/v1/messages`, `/dfcache/*/v1/messages`, and `/cc/v1/messages` share an entry path but apply distinct route, cache, usage, and compatibility policies.
- The implementation already has meaningful modules; the primary structural problem is broad state ownership, dependency direction, request-path I/O, and orchestration concentrated in a few objects.

## Maintenance Rules

- Label every baseline claim with a dated revision through the file header or linked evidence.
- Describe observed current behavior here; put proposed target behavior in an active plan topic.
- Preserve historical evidence rather than rewriting an old test result as if it ran against the latest revision.
- Do not use ignored `target/`, `logs/`, or local temporary files as the sole proof of a durable claim.
- When a plan changes an external contract, state owner, or failure policy, update the affected baseline map in the same landing change.
