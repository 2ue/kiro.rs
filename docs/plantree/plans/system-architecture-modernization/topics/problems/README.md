# Problem Catalog

Role: Stable index for verified findings and measured risks
Status: Current audit catalog
Authority: Finding IDs, severity, and affected technical authority; implementation state is owned by the roadmap
As of: `v0.0.102`, commit `e9479df71ee0`, audit updated 2026-07-12
Read when: Prioritizing work, reviewing scope, or closing a finding
Related: [Requirements](../requirements-and-quality-attributes.md), [Continuous audit and finding lifecycle](continuous-audit-and-finding-lifecycle.md), [Traceability matrix](../../indexes/traceability-matrix.md), [Roadmap](../../roadmap.md), [Decision index](../../decisions/README.md), [Current risks](../../../../baseline/risk-hotspots.md)

## Severity Model

| Level | Meaning |
| --- | --- |
| `P0` | Active catastrophic failure, broadly exploitable compromise, or unavoidable major data loss requiring immediate emergency action |
| `P1` | Verified correctness, security, availability, data-loss, or high-amplification resource defect that must be contained immediately or fixed by the earliest applicable target work unit |
| `P1/P2` | Verified hot-path or structural cost whose production magnitude still requires benchmark/fault-injection evidence |
| `P2` | Maintainability, observability, test, operational, or conditional deployment risk with no evidence of immediate severe impact |

No finding in this audit is classified as P0. Static evidence alone is not used to promote a performance cost to P0.

Immediate production containment for a P1, when technically necessary and separately authorized as an incident action, is not a modernization dependency group, phase, target module, partial release, or substitute for the final replacement. The structural finding stays open until its target module and selected complete-system gates satisfy the closing rule.

The catalog is open-ended. It records verified findings at the named revision; it does not claim that the operator's initial observations or the current IDs exhaust all possible problems. Every module work unit repeats the [continuous audit](continuous-audit-and-finding-lifecycle.md) before entry, target integration, and legacy deletion; the one production cutover is audited only at the complete-system gate.

Current planning-registry checkpoint: 47 verified findings, 50 target technical-authority modules, 100 requirement/invariant/quality clauses, 16 candidate records, 16 gate IDs, and accepted decisions 001 and 003-014; decision 002 remains Superseded. These counts detect cross-document drift and do not prove that any finding is implemented or closed.

## Catalog

### Correctness, Security, And Resource Bounds

See [Correctness, security, and resource bounds](correctness-security-and-resource-bounds.md).

| ID | Severity | Summary |
| --- | --- | --- |
| `COR-001` | P1 | Tool-format debug and request-body capture are enabled by default; directory lifetime is unbounded |
| `COR-002` | P1 | Whole-document runtime config updates lack expected-version CAS and can lose concurrent changes |
| `COR-003` | P1 | Redis usage dedupe marker and aggregate updates are not atomic |
| `COR-004` | P1 | External pool `preservePath` is exposed and persisted but ignored at runtime |
| `COR-005` | P1 | One request can read dynamic runtime configuration more than once and mix policy versions |
| `COR-006` | P1/P2 | Empty/missing tool descriptions and explicit-null input schemas cause avoidable request-wide 400s before profile-specific normalization |
| `COR-007` | P1/P2 conditional | Upstream-rejected tool property names have no collision-free reversible request/response mapping contract |
| `SEC-001` | P1 | Remote source DNS validation is not bound to the actual connection, leaving a rebinding window |
| `RES-001` | P1 | Remote multimodal processing lacks per-request aggregate and global budgets |
| `RES-002` | P1 | Files live bytes/count are bounded, but delete leaves FIFO tombstones and permits unbounded metadata growth |
| `RES-003` | P1 | Kiro and external non-stream/error response bodies can be collected without a byte ceiling |
| `RES-004` | P1/P2 conditional | Kiro HTTP clients are cached forever by proxy configuration and retain obsolete pools and proxy secrets |
| `RES-005` | P1/P2 | Supported defaults leave process-wide local/external admission and wait queues unlimited |
| `HA-001` | P1 in the supported multi-replica production profile | Admin key and selected catalogs do not fully converge across replicas |
| `HA-002` | P1 in the supported multi-replica production profile | Process-local Files objects can be missing when upload and use reach different replicas |
| `HA-003` | P2 in the supported multi-replica production profile | Process-local prompt-cache evidence can produce replica-dependent cache projection |
| `SEC-002` | P1/P2 | External request/response headers use denylist-style forwarding at a provider boundary |
| `SEC-003` | P1 | Default WebSearch info logs record the raw query, while debug logs record complete MCP request/response bodies |
| `SEC-004` | P1/P2 conditional | External-pool destinations and redirects lack a bound DNS/connection/SSRF policy |
| `SEC-005` | P2 | Admin read APIs return reusable plaintext secrets and both frontends persist the Admin key in `localStorage` |
| `SEC-006` | P1/P2 | Reusable credential, proxy and external-pool secrets are stored in PgSQL without application-level encryption |
| `REL-001` | P1/P2 | Accepted usage/audit/storage work lacks one durable, replayable completion contract |
| `REL-002` | P1/P2 | Retryable external send errors do not distinguish “not sent” from “possibly executed”, risking duplicate POST execution/cost |

### Architecture, Performance, And State

See [Architecture, performance, and state](architecture-performance-and-state.md).

| ID | Severity | Summary |
| --- | --- | --- |
| `ARCH-001` | P1/P2 | `MultiTokenManager`, Messages handlers, Admin service, and stores combine unrelated ownership |
| `ARCH-002` | P2 | Domain and storage layers depend on each other's concrete DTOs and implementations |
| `PERF-001` | P1/P2 | PgSQL runtime mutation and Redis lease release enter successful request completion paths |
| `PERF-002` | P1/P2 | Sticky/external availability logic can perform repeated Redis/PgSQL round trips per request |
| `PERF-003` | P1/P2 | Scheduler uses one broad credential lock and repeated O(N) scans |
| `PERF-004` | P2 | Runtime configuration and request state are repeatedly cloned and can mix versions |
| `PERF-005` | P2 | Large request JSON, cache blocks, and diagnostics are repeatedly cloned/canonicalized/serialized |
| `PERF-006` | P1/P2 | Usage batching still performs substantial per-record and per-rollup I/O |
| `PERF-007` | P1/P2 conditional | PDF and configured remote tokenization can block or serialize request work |
| `PERF-008` | P2 conditional | Kiro upstream requests explicitly send `Connection: close`; benefit/risk requires real A/B |
| `PERF-009` | P1/P2 conditional | Lease-acquire Lua scripts can scan and remove every stale member in one Redis invocation |

### Operations, Testing, Frontend, And Supply Chain

See [Operations, testing, frontend, and supply chain](operations-testing-frontend-and-supply-chain.md).

| ID | Severity | Summary |
| --- | --- | --- |
| `OPS-001` | P2 | Compose health check tests only TCP despite application readiness support |
| `OPS-002` | P1/P2 | Usage/storage abandonment can still exit with success status |
| `OPS-003` | P2 | Cleanup and audit tasks are not uniformly supervised or cross-replica durable |
| `OPS-004` | P2 | Backup, restore, Redis rebuild, and forward-recovery behavior lacks an executable versioned runbook |
| `OPS-005` | P2 | Startup runs mutable delimiter-split inline schema and large backfills without immutable atomic migration progress |
| `API-001` | P2 | Two handwritten frontend contracts can agree with each other while both disagree with Rust |
| `TEST-001` | P2 | Maintained frontends have no component or browser E2E suites |
| `TEST-002` | P2 | No repeatable performance regression gate covers hot-path and recovery metrics |
| `TEST-003` | P2, release-blocking for rewritten protocol paths | No durable evidence currently proves the required ccman/real Claude Code 3x20+ turn workflow matrix |
| `TEST-004` | P2, gate-blocking | Current loadtest can measure the wrong PID, encode invalid metrics as zero, corrupt error latency, lose task failures, and skip idle recovery |
| `DOC-001` | P2 | Historical evidence/status and ignored artifacts drift from durable plan truth |
| `DOC-002` | P2 | The primary README links to a missing local Claude Code testing guide |
| `SUP-001` | P2 | Release artifacts lack SBOM, signature, and provenance attestation |

## Explicitly Retracted Or Bounded Findings

These items MUST NOT be reintroduced without new evidence:

- There is no multi-user or multi-tenant boundary, so Files and request keys are not evaluated for cross-tenant isolation.
- Files live content is bounded to 50 MiB/file, 128 live files, and 256 MiB, but the FIFO `order` metadata is not fully bounded because delete leaves tombstone IDs. That remaining defect is `RES-002`.
- JSON nesting is not an unguarded serde stack-overflow path: request entry performs an independent 192-level depth scan before unbounded-depth parsing.
- Current performance is not classified as generally inadequate. Existing 64-concurrency evidence was successful with acceptable resource recovery; broader scale and fault magnitude remain to be measured.
- Storage, usage, and debug channels are bounded. The risks are synchronous fallback, data-loss semantics, and unbounded debug-directory lifetime, not unbounded channels.
- Local SSE has no known unbounded intermediate channel in the ordinary path.

## Closing A Finding

A finding can move to `Closed` only when:

1. its target behavior is implemented;
2. focused unit/contract tests pass;
3. relevant real-storage, protocol, load, chaos, resource, or browser gates pass;
4. rollback and compatibility behavior are documented;
5. durable evidence is linked from plan history;
6. the problem document records the landed commit and no longer describes the target as unimplemented.

Candidate, verified, contained, fixed-pending-verification, closed, and retracted/bounded transitions are defined in the [finding lifecycle](continuous-audit-and-finding-lifecycle.md). A containment does not close the structural finding, and a source-path rewrite does not close a finding without its selected gates and durable evidence.
