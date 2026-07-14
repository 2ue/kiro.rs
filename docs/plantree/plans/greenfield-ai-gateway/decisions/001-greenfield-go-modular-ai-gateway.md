# 001: Greenfield Go Modular AI Gateway

Role: Architecture decision record

Status: Accepted by user direction

Date: 2026-07-13

Authority: Binding product architecture, language, extension, frontend and legacy relationship decision

Supersedes: The target implementation model of [System architecture modernization](../../system-architecture-modernization/README.md), including its Rust target, 50-module ledger, two-frontend target and legacy-compatible implementation graph

Related: [Plan root](../README.md), [complete reconstruction plan](../topics/complete-reconstruction-plan.md), [work graph](../roadmap.md)

## Context

The current system contains successful Kiro, Anthropic and Claude Code behavior, but its architecture concentrates unrelated responsibilities in large state and service authorities. Examples include `src/storage/postgres.rs` at more than 11,000 lines, `src/kiro/token_manager/manager.rs` at more than 8,000 lines, `src/anthropic/handlers.rs` at more than 7,000 lines, and `src/kiro/provider.rs` at nearly 4,000 lines as of this decision.

The earlier modernization plan proposed a complete Rust rewrite with two retained frontends and a fixed 50-module target. The user has replaced that target with a new Go system whose primary product is a general AI model gateway. Kiro must be fully supported in the first release but must not define the generic architecture.

Future upstream providers or API gateway integrations may require scheduling, account allocation, usage extraction, protocol conversion and retry behavior similar to Kiro. They may also require different implementations. Sharing concrete algorithms is therefore optional; sharing typed semantics and lifecycle contracts is mandatory.

## Decision

1. Build the target as a new repository and new implementation. The current Rust runtime is a behavioral oracle, not a code dependency or package template.
2. Use Go for the backend and a domain-oriented modular monolith for the first complete system.
3. Use one React, TypeScript and Tailwind CSS Admin application. Selectively adapt a reviewed open-source Admin template, use Lucide icons, and generate its API client from the Go-owned OpenAPI contract.
4. Separate client protocol adapters, the execution kernel, provider modules, reusable default components, the control plane and platform infrastructure through typed, versioned contracts.
5. Make Kiro a vertical provider module that privately owns Kiro authentication, endpoints, account scheduling, transport, EventStream decoding, models, quota, errors and raw usage extraction.
6. Allow every provider module to use shared default scheduler/usage/conversion libraries or replace them with its own implementation. The core depends on `Acquire/Complete`, attempt, event, usage-fact and terminal contracts rather than internal algorithms.
7. Use compile-time module registration in the first release. If independently deployable modules are later required, preserve the semantics over a Protobuf and Connect/gRPC process boundary instead of using Go runtime plugins.
8. Support multiple replicas through PostgreSQL authority, Redis leases and fencing, immutable configuration revisions, idempotent outbox processing and producer-aware shutdown.
9. Construct modules in dependency order but release only one complete candidate. Do not create a hybrid Rust/Go runtime, staged production replacement, dual scheduler or request-level old/new selector.

## Retained Invariants From The Previous Plan

The new implementation retains the following semantic requirements after translating them into Go-owned contracts:

- separate `upstream may have executed` evidence from `downstream response committed` state;
- retry or provider switching only when replay is proven safe;
- one terminal decision while lease, credential state and usage remain separate authorities;
- candidate configuration validation before transactional CAS publication and outbox append;
- bounded queue and lease lifecycle with cancellation, heartbeat, fencing and idempotent completion;
- stop all producers before closing writer ingress during shutdown;
- explicit state ownership, resource ceilings, recovery and real-client/load evidence.

The previous Rust package topology, module ledger, generated Rust-to-TypeScript contract, two-frontend requirement and legacy migration sequence are not retained.

## Consequences

- No target implementation can start by moving Rust files into similarly named Go packages.
- Kiro-specific DTOs, credentials, errors, Redis keys and scheduling state cannot appear in generic gateway packages.
- Generic behavior must be proven with at least one non-Kiro contract implementation, not inferred from a Kiro-only design.
- The first implementation has more explicit contract and characterization work, but future protocol/provider additions no longer require changes across the entire request path.
- The old modernization tree remains searchable historical evidence and a source of reusable invariants, but it no longer authorizes target implementation.
