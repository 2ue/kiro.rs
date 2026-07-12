# 007: Domain-Oriented Modular Monolith And Module Ownership

Role: Architecture decision record

Status: Accepted

Date: 2026-07-12

Authority: Binding target modular-monolith shape, stable module identities, technical authority rules, dependency constraints, and implementation boundaries

Scope: All first-party backend, frontend, lifecycle, worker, adapter, and validation modules created or replaced by the modernization plan

Affected requirements/findings: Fixed constraint 6, `ARCH-001`, `ARCH-002`, `COR-005`, `QA-MAINT-001` through `QA-MAINT-005`, every target-module boundary, and every R0-R10 dependency-group integration/deletion gate

Decision source: Architecture-target conformance review and final-plan convergence on 2026-07-12; modular construction and final activation follow decision 009

Related: [Target architecture](../topics/architecture/target-system-architecture.md), [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Target module ledger](../indexes/target-module-ledger.md), [Execution slice map](../indexes/execution-slice-map.md), [Rewrite inventory](../indexes/rewrite-inventory.md), [Rewrite sequence](../topics/delivery/migration-sequence.md), [Decision index](README.md)

## Context

Superseded decision 002 captured the original complete-rewrite intent, while decisions 007/009 now bind its modular construction and one-program delivery mechanics. Mechanical file splitting is insufficient. A target tree expressed only as horizontal technical layers such as `application`, `domain`, `ports`, `adapters`, and `workers` would let each directory become a new dumping ground and reproduce the coupling currently concentrated in `AppState`, `MultiTokenManager`, `AdminService`, `PostgresStore`, and `RedisStore`.

The rewrite inventory tracks old source paths, while the roadmap tracks R0-R10 dependency groups. Neither alone defines the stable replacement unit. A module needs an identity independent of its directory and legacy files, one responsibility, owned state, a public contract, allowed dependencies, a target-integration boundary, and deletion evidence.

Target `RuntimeSnapshot`, `RequestEnvelope`, `ProcessingPlan`, and `TerminalPlan` contracts also need limits. Immutable data is not automatically modular: a broad immutable context can still expose every policy to every module, and a terminal structure containing owner-specific commands can couple response lifecycle code to scheduler, credential, and usage internals.

## Decision

The target is a domain-oriented modular monolith: one deployable binary and one composition root, containing independently owned capability modules. Logical domain, application, port, adapter, and worker roles exist inside or immediately beside an owning capability module; they are not global horizontal ownership containers.

Each of the 50 target modules has one stable `MOD-*` identifier registered in the [target module ledger](../indexes/target-module-ledger.md). Directory and type names may evolve, but a module ID changes only through a superseding decision or an explicit ledger split/merge that preserves migration and evidence links.

A target module is valid only when it has:

1. one bounded responsibility and named invariant owner;
2. explicit owned durable, coordination, cached, request-local, or no mutable state;
3. one documented public contract and private implementation surface;
4. allowed module dependencies and forbidden reverse/private imports;
5. no direct access to another module's repository, queue, lease, worker, or adapter;
6. a legacy responsibility mapping, characterization contract, target integration boundary, and deletion evidence slot;
7. focused tests that do not construct unrelated application state.

## Module And Layer Rules

Cross-module calls use only the callee's public application/query contract or typed event contract. A module cannot import another module's private domain implementation, adapter records, worker queues, or internal state handles. Cycles between public module contracts are prohibited; orchestration that needs more than one module belongs to a named application coordinator whose inputs remain owner-specific.

Inside a stateful module, dependency direction is:

```text
transport or worker driver -> public application contract -> domain policy + owned ports
owned adapters -> owned ports
bootstrap -> concrete construction only
```

Pure protocol codecs and the bounded shared kernel may be shared. A domain module may depend on protocol boundary types only where its public contract genuinely carries that protocol; transport-specific DTOs are otherwise mapped at the edge.

The shared kernel is limited to dependency-light identity, time/version, cancellation/deadline, bounded error, and small value primitives with semantics used unchanged by multiple modules. It owns no business policy, mutable state, repository trait, service registry, route behavior, usage formula, scheduler rule, or provider-specific DTO. A type is not promoted to the shared kernel merely to break a dependency cycle.

## Runtime Configuration Views

The runtime-config module exclusively owns the raw complete immutable versioned snapshot. Authenticated public request entry calls its public `capture()` contract exactly once and receives a `CapturedRuntime` bundle containing only typed immutable views or already derived plans for downstream owners. Transport passes that bundle with `RequestEnvelope` to Messages orchestration; neither transport nor Messages receives or retains the raw complete snapshot.

No target module may call `RuntimeSnapshotProvider::capture()` after authenticated entry, and no second provider read is permitted during the request. Routing receives a routing view, scheduler receives a scheduler view, processing receives a target-processing view, usage receives a usage view, and resource governors receive a resource view, all carrying the same captured version. A view contains data, not repositories, clients, queues, locks, or callbacks.

## Terminal Ownership

The terminal-lifecycle module owns one request-local terminal decision and stable obligation IDs. Its neutral terminal plan records identity, terminal outcome, downstream commitment, attempt-summary reference, runtime version, and child IDs. It does not contain `LeaseCompletion`, `CredentialOutcomeEvent`, `UsageFinalizationInput`, repositories, sinks, worker handles, arbitrary JSON, or a heterogeneous command collection.

Scheduler, credential, and usage owners project their own commands/events from their request-local owner state plus neutral terminal facts. The terminal application coordinator may collect typed owner acknowledgements, but it does not become the authority for their state or imply a cross-system transaction.

## Legacy Anti-Corruption Boundary

Legacy code is reachable only from characterization and validation tooling through a package-specific adapter registered in the ledger. The adapter translates one accepted public contract, has a technical authority and deletion gate, and cannot be compiled into the final target runtime. Target domain/application modules cannot import `AppState`, `KiroProvider`, `MultiTokenManager`, `ExternalPoolManager`, `UsageRecorder`, broad legacy stores, or legacy handler modules.

Compatibility direction is explicit during development: validation may invoke the legacy baseline and target candidate separately through the same black-box contract, but target composition never selects a legacy implementation. New modules do not call a broad legacy facade as an internal dependency. Adding methods or state to a legacy facade is outside this modernization unless it is an independent incident hotfix and cannot establish a target contract.

## Prohibited Containers

The target architecture prohibits:

- a service locator or dependency map retrieved by type/name at runtime;
- a `Services`, `Context`, `State`, or `AppState` object exposing unrelated repositories, clients, workers, or policies to arbitrary modules;
- a full runtime snapshot or configuration graph used as a general module dependency;
- a mega prelude or root re-export that makes private cross-module dependencies invisible;
- a generic repository/event bus whose untyped payload or broad method set bypasses owner ports;
- `serde_json::Value`, `Box<dyn Any>`, stringly typed commands, or arbitrary maps as the standard cross-module contract;
- a terminal or processing plan that accumulates owner-specific services or mutable state;
- a shared worker that contains business branching for unrelated modules.

Composition may hold all concrete dependencies, but only bootstrap can see that graph. Each constructed module receives the narrow dependencies declared in the ledger.

## Models And Catalog Ownership

The model-catalog module is the single owner of versioned model capabilities, aliases, public Models query semantics, pricing metadata, catalog refresh validation, and publication of an immutable catalog view. Public transport only maps the Models API. Routing, processing, usage, and Admin consume narrow catalog query/command contracts; none maintains a second mutable catalog.

Catalog synchronization is a driver of the model-catalog application contract. It cannot write PgSQL, replace process snapshots, or publish invalidation independently of the catalog owner's transaction and publication rules.

## Alternatives And Tradeoffs

### Keep global horizontal layers as ownership boundaries

Rejected. Logical layering remains useful, but global `application`, `ports`, `adapters`, and `workers` authority encourages broad imports and storage/worker God Objects.

### Use one shared application context with immutable fields

Rejected. Immutability prevents races but does not prevent compile-time coupling, unrelated policy access, or construction of every dependency for focused tests.

### Create a workspace crate per module immediately

Not selected. Module visibility and architecture checks should prove boundaries inside the modular monolith first. A later crate split may be justified by compile or ownership evidence.

### Put every cross-module type in the shared kernel

Rejected. It hides cycles and turns the kernel into a semantic dependency hub. Boundary DTOs remain owned by the providing module or a protocol module.

The structure creates more explicit mapping and adapter code. That cost is accepted only if it removes state ambiguity, hidden dependencies, and broad construction requirements; directory count alone is not evidence of success.

## Compatibility And Data Consequences

- Public Anthropic, Claude Code, Kiro, external-pool, Admin, Models, Files, usage, and frontend behavior remains governed by characterization and accepted defect decisions.
- Domain-oriented modules do not imply separate processes, databases, Redis instances, or user partitions.
- PgSQL/Redis schemas may remain physically shared while queries, scripts, records, transactions, and migrations are owned by one target module.
- Expand-contract data changes and previous-binary compatibility remain required through the final whole-system rollback window.
- The target module ledger complements rather than replaces the source-path rewrite inventory and problem traceability matrix.

## Modular Construction And System Rollback

1. Register target module IDs, technical authorities, contracts, dependencies, legacy responsibility mappings, and evidence slots before creating replacement code.
2. Add architecture checks for public/private imports, prohibited containers, unregistered module paths, and target-to-legacy imports before target implementation expands.
3. Build bounded target modules behind their registered contracts; do not populate target directories by moving old files without changing authority.
4. Integrate modules only into the target candidate. A legacy baseline and target candidate may be invoked separately by validation, but they never execute the same side-effecting logical operation.
5. Delete legacy source and test-only adapters before the final release candidate freezes, then run full post-deletion gates.

Rollback selects the previous complete release under decision 009. A failed module is corrected inside the target candidate and does not authorize a broad shared context or a production module selector.

## Verification

- Static checks reject every target-runtime import of legacy modules; test-only characterization adapters are isolated from release features.
- Static checks reject domain imports of Axum, reqwest, sqlx, Redis clients, frontend DTOs, and another module's private implementation.
- A module contract test constructs only the module, declared shared primitives, and fake owner ports.
- Dependency-cycle and public-surface reports are recorded for every module integration and the final candidate.
- Runtime instrumentation proves one configuration capture per request and no downstream snapshot-provider reads.
- Tests prove two modules cannot mutate the same authority and that each cross-module side effect travels through the registered owner contract.
- Ledger checks require every target source path and rewrite-inventory responsibility slice to resolve to one module ID.
- Post-deletion search rejects hidden legacy imports, fallback calls, mega preludes, stubs, and unregistered execution selectors.

## Implementation Parameters

- Exact target source directories and whether selected modules later become crates remain implementation choices inside these binding rules.
- Responsibility/symbol mapping is the first task for each module work package, not a plan-level personnel or rollout blocker.
- Decisions 003-006 and 008-014 bind runtime snapshot, state, terminal, scheduler, shutdown, migration, operational, security/resource, protocol, audit and delivery behavior.
- Module IDs and dependency rules are binding now; implementation evidence remains absent until code and gates actually run.
