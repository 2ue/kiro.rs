# Complete Implementation Work Graph

Role: Dependency order and completion state for the one greenfield AI Gateway implementation

Status: Planned; implementation Not Started; no target repository exists

As of: 2026-07-13

Authority: Defines the complete implementation dependency graph and whole-system completion conditions

Related: [Plan root](README.md), [complete reconstruction plan](topics/complete-reconstruction-plan.md), [reference projects](topics/reference-projects-and-template-selection.md), [decision 001](decisions/001-greenfield-go-modular-ai-gateway.md)

## Delivery Rule

This is one complete greenfield implementation. The work units below are dependency groups, not staged migrations, release phases, deadlines or partial products. They may be implemented in parallel when contracts and state ownership do not conflict, but none is a separately delivered production system.

The current Rust service remains independent until one complete Go candidate passes every gate. Final activation switches the whole system. Rollback restores the previous whole Rust release and does not mix request execution between architectures.

## Done: Target Direction

- Accepted the greenfield Go backend and one React/Tailwind Admin application.
- Established Kiro as the first complete vertical provider module rather than a generic-core dependency.
- Established versioned contracts, optional shared defaults, compile-time registration and a future process-boundary extension model.
- Selected a reviewed frontend baseline and recorded reference-project research.
- Preserved reusable correctness invariants from the previous plan without retaining its implementation topology.

## Current

The target architecture and complete work graph are ready for review. No source implementation, schema, migration, generated contract, benchmark, deployment artifact or acceptance evidence exists.

## Dependency Work Graph

| Order | Work group | Complete output required inside the final candidate |
| ---: | --- | --- |
| W0 | Behavioral oracle and contract corpus | Sanitized Anthropic/Claude Code/Kiro fixtures; current route, scheduling, usage, error, Files, tool, thinking and streaming behavior matrix; explicit intentional corrections; no copied production code |
| W1 | Repository and architecture fitness | New Go/React repository, package import rules, dependency checks, code generation, CI, license notices, reproducible Node/Go toolchains, reviewed-template source/target compatibility spike, benchmark and integration harness skeletons |
| W2 | Contract kernel | Operation-specific invocation contracts, capability negotiation, canonical event model, delivery evidence, attempt finalizer, attempt/request usage facts, terminal outcomes, error taxonomy and module descriptor/version rules |
| W3 | Platform authorities | PostgreSQL synchronous attempt/terminal journal, migrations/repositories, Redis coordination primitives, secret envelope, immutable configuration revisions, transactional audit/outbox, Files/object storage and bootstrap settings |
| W4 | Execution core and reusable defaults | Admission, routing, provider registry, per-attempt acquire/finalize/release/retry loop, downstream commitment, request terminal reducer, global resource governor, default scheduler and default usage/reporting components |
| W5 | Contract proof modules | Deterministic mock provider and simple compatible HTTP provider proving that routing, scheduling, usage and protocol contracts contain no Kiro dependency |
| W6 | Complete Kiro provider | All required Kiro authentication, IDE/CLI endpoints, accounts, models/quota, scheduler, distributed leases, conversion, transport, EventStream, errors, usage/cache facts, Admin contribution and maintenance jobs |
| W7 | Client protocol surfaces | Anthropic Messages, Claude Code profile, Models, Files and count-tokens ingress/egress; stream and non-stream behavior; generated public/Admin OpenAPI contracts |
| W8 | Control plane and Admin application | Domain Admin API; candidate validation, diff, CAS publish and audit; modern React application with complete provider, account, route, model, scheduler, usage, diagnostics and operations workflows |
| W9 | HA, lifecycle and operations | Data/control/worker roles, readiness, compatible/incompatible release-generation admission barriers, producer-aware shutdown, backups, Redis rebuild, recovery, Compose, Kubernetes, telemetry, dashboards, alerts and release provenance |
| W10 | Complete-candidate verification and cutover | Static, unit, contract, attempt-journal crash, multi-attempt usage, generation-fencing, storage, browser, accessibility, real Claude Code, bounded real Kiro, load, soak, chaos, security, backup/restore, cutover and whole-system rollback evidence for one immutable digest |

## Work-State Rules

| State | Meaning |
| --- | --- |
| `Planned` | Contract, scope and exit conditions exist, but no implementation artifact exists |
| `Implementing` | Target source or focused evidence is being produced in the new repository |
| `Integrated` | The work is present in the target-only candidate and focused gates pass |
| `Verified In Candidate` | Aggregate gates pass for the same immutable candidate digest |
| `Blocked` | A discovered fact contradicts the accepted contract or required evidence cannot be produced |

Only the whole system has production states: `Implementation Not Started`, `Candidate In Progress`, `Complete Candidate Verified`, `Cutover Ready`, `Full-System Observation`, and `Complete`.

## Non-Negotiable Completion Gates

The work is not complete until all of the following are true for one candidate digest:

- no Rust runtime, legacy fallback, dual execution path, Kiro type in generic packages, dynamic Go plugin or broad service locator exists;
- Kiro and non-Kiro proof modules pass the same versioned module contract suite;
- real Claude Code completes the accepted multi-session, 20-plus-turn tools/agents/MCP/Files/thinking matrix;
- bounded real Kiro validation passes without exposing credentials or exceeding the accepted call budget;
- one, two and four replica tests prove lease, configuration, revocation, shutdown and recovery correctness;
- load and soak gates meet the accepted latency, throughput, memory, file-descriptor, task, queue and connection recovery budgets;
- the Admin UI passes complete workflow, responsive, keyboard, screen-reader, axe and browser matrix gates;
- backup/restore, Redis loss/rebuild, dependency failure, process crash, rolling restart and whole-system rollback are rehearsed;
- SBOM, vulnerability results, third-party notices, signatures, provenance, images, manifests and runbooks identify the exact digest;
- old production is switched only after every gate passes, and the rollback artifact remains usable through the observation window.

## Next Target

Accept this plan, choose the new repository name and location, then create the target repository and begin W0/W1/W2 foundations. Create `implementation-status.md` at that point, not before.
