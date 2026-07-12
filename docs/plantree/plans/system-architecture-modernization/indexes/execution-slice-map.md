# Modular Implementation Work Map

Role: Authoritative dependency graph for module-organized work inside one complete modernization program

Status: Accepted implementation map; every work unit is Ready and none has started

Authority: Defines exact coding, integration, and evidence units under decisions 007-014. It does not define independent production switches or releases.

As of: 2026-07-12

Read when: Selecting target module work, resolving dependencies, preparing fixtures, integrating the target-only candidate, checking full coverage, or removing legacy source

Related: [Decision 009](../decisions/009-single-program-modular-build-and-final-cutover.md), [Target module ledger](target-module-ledger.md), [Complete implementation plan](../topics/delivery/migration-sequence.md), [Implementation entry contract](../topics/delivery/next-package-brief.md), [Rewrite inventory](rewrite-inventory.md), [Verification](../topics/delivery/verification-rollout-and-rollback.md)

The filename is retained to preserve existing references. The old independently switchable production-slice model is superseded by decision 009.

## Work-Unit Contract

`R0` through `R10` are dependency groups inside one implementation. They are not product phases, separately accepted scope, deployment waves, module canaries, or module rollback domains.

Every work unit has:

1. one or more exact technical authority modules and a bounded final responsibility;
2. exact accepted public inputs, outputs, errors, lifecycle, and state authority;
3. pinned-revision legacy symbol/responsibility mapping produced when work starts;
4. characterized current behavior and explicit intentional safety corrections;
5. focused unit/property/contract/storage/fault/resource evidence;
6. a target-only integration boundary and exact dependencies;
7. legacy source/config/test/harness deletion conditions and post-deletion checks;
8. no production selector, percentage, cohort, old/new fallback, or independent release state.

Unfinished target dependencies use test-only typed fakes. Fakes, stubs, `TODO`, `unimplemented!()`, legacy imports, and duplicate authorities are prohibited from the final release candidate.

## R0: Final Constraints, Fixtures, And Harnesses

R0 produces final reusable validation assets. It does not patch or activate temporary production containment. The affected product modules are implemented once in their later dependency group using these accepted constraints.

| Work unit | Technical authority | Final output | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R0.1` | `MOD-CONTRACT-HARNESS` for `MOD-DIAGNOSTICS` | Diagnostics/default-sensitive-log fixtures and decision-010 quotas/redaction expectations | Current diagnostic characterization | default-off, opt-in, redaction, quota, permissions, restart/failure vectors | Ready |
| `R0.2` | `MOD-CONTRACT-HARNESS` for `MOD-MEDIA` | Remote-source, DNS/redirect, byte/permit and cancellation fixtures | Current media characterization | 0/1/8/over-limit, IPv4/IPv6, rebinding, slow/large/cancel vectors | Ready |
| `R0.3` | `MOD-CONTRACT-HARNESS` for `MOD-FILES` | Shared Files API/churn/restart/replica fixture corpus | Current Files characterization | upload/list/get/delete/materialize, repeated churn, checksum and failover vectors | Ready |
| `R0.4` | `MOD-ARCH-FITNESS` | Final module/import/cycle/public-surface/legacy/stub/artifact rules | Decisions 007-014 and both ledgers | self-tests that reject known forbidden graphs and false plan states | Ready |
| `R0.5` | `MOD-LOAD-CHAOS-HARNESS` | Final valid manifest/report engine, process identity, result accounting, metric validity, watchdog and cleanup | Current harness characterization and `TEST-004` | harness self-test, wrong/dead PID rejection, missing-is-invalid, exact counts, cooldown/recovery | Ready |
| `R0.6` | `MOD-CONTRACT-HARNESS` | Final sanitized black-box contract manifest and fixture provenance system | Public invariant inventory | deterministic replay, corpus hashes, secret scan, complete pass/fail/skip/error accounting | Ready |
| `R0.7` | `MOD-CONTRACT-HARNESS` for `MOD-KIRO-UPSTREAM` | Kiro oversized/chunked/slow response and proxy-client churn corpus | Current Kiro transport characterization | body limits, error-prefix limit, 1k-10k proxy identities, RSS/FD/connection recovery | Ready |
| `R0.8` | `MOD-CONTRACT-HARNESS` for `MOD-EXTERNAL-UPSTREAM` | External destination/redirect/header/response corpus | Current external-pool characterization | SSRF/rebinding/proxy-DNS, redirects, credentials, oversized success/error/stream vectors | Ready |
| `R0.9` | `MOD-CONTRACT-HARNESS` for `MOD-SCHEDULER-LOCAL` | Local admission/queue/lease/stale-set workload corpus | Current local scheduler characterization | finite overload, 1/1k/100k stale entries, cancel/grant races, recovery | Ready |
| `R0.10` | `MOD-CONTRACT-HARNESS` for `MOD-SCHEDULER-EXTERNAL` | External admission/fallback/rescue/lease/stale-set workload corpus | Current external scheduler characterization | finite overload, fallback/rescue bounds, cancel/grant races, recovery | Ready |

## R1: Kernel, Runtime Views, Protocol, Observability, Diagnostics

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R1.1` | `MOD-KERNEL` | Stable IDs, versions, time, cancellation, deadlines and bounded errors used by target modules | `R0.4` | unit, dependency, allocation and compile checks | Ready |
| `R1.2` | `MOD-RUNTIME-CONFIG` | One raw versioned snapshot authority and one authenticated capture producing narrow immutable views | `R1.1` | single-capture, version coherence, clone/allocation, forbidden provider reads | Ready |
| `R1.3` | `MOD-KERNEL` | Decision-010/011 byte/count/task/queue/client/timeout budget value types; global/weighted permit state belongs to `MOD-RESOURCE-GOVERNOR`, while domains hold only scoped handles and semantic operation limits | `R1.1` | validation, overflow, cancellation and fail-closed defaults | Ready |
| `R1.4` | `MOD-LOAD-CHAOS-HARNESS` | Accepted workload manifests and threshold evaluator added to the already final R0 harness | `R0.5`, decision 010 | reference-host validity, five alternating rounds, sample/threshold/recovery self-tests | Ready |
| `R1.5` | `MOD-PROTO-ANTHROPIC` | One dependency-light Anthropic Messages/Models/Files/token/error wire authority | `R0.6`, `R1.1` | golden vectors, round trips, malformed input, compile/dependency and allocation | Ready |
| `R1.6` | `MOD-OBSERVABILITY` | Typed redacted events, bounded labels and required stage/resource metric sources | `R1.1`, `R1.3` | cardinality, secret scan, backpressure/drop, metric identity | Ready |
| `R1.7` | `MOD-DIAGNOSTICS` | Final explicit opt-in redaction/queue/quota/retention/restart/failure implementation | `R0.1`, `R1.1`, `R1.6` | diagnostics corpus, filesystem safety, bounded recovery and cleanup | Ready |
| `R1.8` | `MOD-SECRET-ENVELOPE` | Versioned XChaCha20-Poly1305 envelope, external key-ring provider, redacted/zeroizing secret types, rewrap and restore-key manifest | `R1.1`, `R1.6`, decision 011 | golden/tamper/wrong-key/associated-data/rotation/rewrap/zeroization/restore tests and direct-crypto import rejection | Ready |
| `R1.9` | `MOD-RESOURCE-GOVERNOR` | Final memory-profile validation, weighted pre-heavy-work admission/stage permits, one combined wait queue, connection ceilings and recovery metrics | `R0.5`, `R1.1`-`R1.4`, `R1.6`, decision 011 | invalid-limit fail-closed, cost classes, queue/cancel races, pool/socket saturation, permit leak and idle recovery | Ready |

## R2: Migrations, State Authorities, CAS, Auth, Catalog, Journal

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R2.0.migration-foundation` | `MOD-MIGRATIONS` | Common immutable manifest validator, fenced runner, active/applied/adopted/checkpoint ledger and migration reconciliation; no domain SQL or disaster recovery | `R1.1`, decision 008, migration audit/adoption map | fresh/legacy/partial/corrupt/concurrent, checksum, lock, transaction/resume, previous binary | Ready |
| `R2.1` | `MOD-RUNTIME-CONFIG` | PgSQL typed patch/CAS transaction; concurrent writes conflict instead of losing fields | `R1.2`, `R1.8`, `R2.0.migration-foundation` | CAS races, rollback, schema/adoption and Admin compatibility | Ready |
| `R2.2` | `MOD-RUNTIME-CONFIG` | Generation publication and missed-event/restart convergence | `R2.1`, decision 010 | multi-replica invalidation loss, polling, restart and readiness | Ready |
| `R2.3` | `MOD-TERMINAL-JOURNAL` | Stable PgSQL terminal envelope/outbox append, replay and acknowledgement | `R2.0.migration-foundation`, decision 004 | fault injection, duplicates, crash/replay, backlog and residue | Ready |
| `R2.4.runtime-config` | `MOD-RUNTIME-CONFIG` | Domain-owned manifest, SQL/DDL, probes and repository port | `R2.0.migration-foundation`, `R2.1` | port/schema/adoption/drift/previous-binary | Ready |
| `R2.4.auth` | `MOD-AUTH` | Auth manifest/repository, epoch/version and keyed-verifier/fingerprint storage for Request/Admin API keys; no reversible key ciphertext | `R1.8`, `R2.0.migration-foundation` | migration, constant-time verification, reveal-once creation/rotation, revocation and previous-binary | Ready |
| `R2.4.model-catalog` | `MOD-MODEL-CATALOG` | Catalog manifest/repository and validated versioned rows | `R2.0.migration-foundation` | migration, alias/capability/pricing and drift | Ready |
| `R2.4.credentials` | `MOD-CREDENTIALS` | Credential manifest/repository, encrypted secret and generation CAS | `R1.8`, `R2.0.migration-foundation` | migration, encryption, CAS, masking and prior binary | Ready |
| `R2.4.proxy-resources` | `MOD-PROXY-RESOURCES` | Proxy manifest/repository, encrypted secrets and binding metadata | `R1.8`, `R2.0.migration-foundation` | migration, CRUD, referential guards, masking and prior binary | Ready |
| `R2.4.external-pools` | `MOD-EXTERNAL-POOLS` | Pool manifest/repository, encrypted API key and capability/manual-state authority | `R1.8`, `R2.0.migration-foundation` | migration, encryption, state transitions, URL profile and prior binary | Ready |
| `R2.4.usage` | `MOD-USAGE` | Usage event/rollup manifest and repository ports | `R2.0.migration-foundation`, `R2.3` | migration, replay, batching and prior binary | Ready |
| `R2.4.files` | `MOD-FILES` | Shared PgSQL FileObjectStore manifest, streaming payload/metadata/checksum/TTL repository | `R2.0.migration-foundation`, decision 010 | migration, bounded streaming, failover, backup/restore and cleanup | Ready |
| `R2.4.audit` | `MOD-AUDIT` | Audit manifest/repository and sealed owner-transaction append function/contract | `R2.0.migration-foundation`, decision 013 | migration, PgSQL privilege boundary, transactional acceptance/idempotency, query and retention | Ready |
| `R2.4.maintenance-jobs` | `MOD-MAINTENANCE-JOBS` | Durable job/checkpoint/lease manifest and repository | `R2.0.migration-foundation` | migration, claim races, restart, cancellation and bounded backfill | Ready |
| `R2.5.runtime-config-invalidation` | `MOD-RUNTIME-CONFIG` | Versioned Redis invalidation hint class | `R2.2` | loss, duplicate, reorder, rebuild and cardinality | Ready |
| `R2.5.scheduler-local` | `MOD-SCHEDULER-LOCAL` | Versioned local queue/lease/RPM/cooldown/sticky Redis classes | `R0.9`, `R2.0.migration-foundation` | Lua races, batch bounds, epoch/restart and rebuild | Ready |
| `R2.5.scheduler-external` | `MOD-SCHEDULER-EXTERNAL` | Versioned external queue/lease/cooldown Redis classes | `R0.10`, `R2.0.migration-foundation` | Lua races, batch bounds, epoch/restart and rebuild | Ready |
| `R2.5.usage-projection` | `MOD-USAGE` | Versioned derived dashboard/rollup Redis projection | `R2.4.usage` | atomicity, replay, rebuild and bounded cardinality | Ready |
| `R2.5.prompt-cache` | `MOD-PROMPT-CACHE` | Versioned bounded shared cache-evidence Redis state | `R2.0.migration-foundation`, decision 010 | TTL/capacity, atomic transitions, restart/reset and fact labels | Ready |
| `R2.6` | `MOD-AUTH` | Request/Admin verification, rotation/revoke, bounded HttpOnly browser sessions/CSRF and cross-replica epoch convergence | `R2.2`, `R2.4.auth`, `R2.5.runtime-config-invalidation` | auth compatibility, session TTL/capacity/loss, CSRF, 2/5-second revocation, replica/restart/readiness | Ready |
| `R2.7` | `MOD-MODEL-CATALOG` | Alias/capability/pricing query and validated refresh with one immutable view | `R2.2`, `R2.4.model-catalog` | catalog parity, invalid refresh, replica/restart and publication | Ready |
| `R2.8` | `MOD-SECRET-ENVELOPE`, secret-owning modules, `MOD-MAINTENANCE-JOBS` | Domain-owned legacy adoption, bounded CAS rewrap/key rotation, rollback-window compatibility projection and post-window plaintext contraction | all secret-owning `R2.4.*` rows, `R2.4.maintenance-jobs`, decision 011 | mixed-key reads, rotation/resume, backup restore, prior binary, frozen mutations, plaintext residue zero after contraction | Ready |

## R3: Usage And Prompt-Cache Accounting

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R3.1` | `MOD-USAGE` | Distinct actual, estimated, reported, accounting and cache facts with pure projection | `R2.3`, `R2.4.usage` | properties, golden facts and stream/non-stream parity | Ready |
| `R3.2` | `MOD-USAGE` | Stable idempotent event persistence and explicit terminal-tail acknowledgement | `R3.1`, decision 004 | PgSQL replay, batching, queue saturation and terminal latency | Ready |
| `R3.3` | `MOD-USAGE` | Redis/dashboard rebuild from durable events without marker gaps | `R3.2`, `R2.5.usage-projection` | atomicity, duplicate replay, rebuild and query parity | Ready |
| `R3.4` | `MOD-PROMPT-CACHE` | Shared bounded evidence transitions that never mislabel simulated values as upstream facts | `R2.5.prompt-cache`, `R3.1` | replica/restart, TTL/capacity, actual/derived/simulated labeling | Ready |

## R4: Proxy, Scheduler, Credential, And Pool Lifecycles

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R4.0` | `MOD-PROXY-RESOURCES` | Commands/queries/tests, reveal-once/masking, versioned catalog and binding resolution | `R2.4.proxy-resources`, `R2.6` | CRUD/test, secret lifecycle, binding precedence, replica/restart and invalidation | Ready |
| `R4.1` | `MOD-SCHEDULER-LOCAL` | Pure deterministic eligibility/ranking | `R1.1`, `R2.4.credentials`, `R4.0` | offline parity, scale/complexity and reason stability | Ready |
| `R4.2` | `MOD-SCHEDULER-LOCAL` | Final finite post-process-admission FIFO-class queue, lease/heartbeat/fencing/complete/cancel/cleanup lifecycle | `R0.9`, `R1.9`, `R2.5.scheduler-local`, `R4.1`, decision 005 | overload, races, timing, crash/restart, Redis loss and resource recovery | Ready |
| `R4.3` | `MOD-CREDENTIALS` | Refresh generation, encrypted secret lifecycle and durable credential outcomes | `R4.0`, `R4.2`, `R2.4.credentials` | refresh CAS, proxy binding, duplicate outcomes, faults and restart | Ready |
| `R4.4` | `MOD-EXTERNAL-POOLS` | Pool catalog/capabilities/manual-auto state and durable outcomes | `R2.4.external-pools`, `R2.7` | catalog/state parity, replay and disable/re-enable recovery | Ready |
| `R4.5` | `MOD-SCHEDULER-EXTERNAL` | Final external eligibility and post-process-admission FIFO-class queue/lease/fallback/rescue lifecycle | `R0.10`, `R1.9`, `R2.5.scheduler-external`, `R4.4`, decision 005 | overload, fallback/rescue, races, timing, Redis loss and recovery | Ready |

## R5: Upstream Protocols, Adapters, And Replay Policy

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R5.0.kiro` | `MOD-PROTO-KIRO` | Kiro prepared-request and transport-outcome contract/codecs | `R1.1`, `R1.5` | compile/static and golden vectors | Ready |
| `R5.0.external` | `MOD-PROTO-EXTERNAL` | External prepared-request, raw/normalized boundary and outcome contract | `R1.1`, `R1.5` | compile/static and golden vectors | Ready |
| `R5.1` | `MOD-KIRO-UPSTREAM` | One bounded client/cache/endpoint/proxy/auth/connect/response/stream adapter per logical attempt using only scoped governor connection/response handles | `R0.7`, `R1.9`, `R4.0`, `R4.3`, `R5.0.kiro` | fake upstream, limits/cache churn/proxy rotation and low-volume independent real Kiro operations | Ready |
| `R5.2` | `MOD-EXTERNAL-UPSTREAM` | One safe destination/redirect/path/header/client/response adapter for raw and normalized attempts using only scoped governor connection/response handles | `R0.8`, `R1.9`, `R4.4`, `R5.0.external` | egress/SSRF, redirect/header/proxy, limits, stream and recovery | Ready |
| `R5.3` | `MOD-ATTEMPT-POLICY` | One conservative execution-possibility/replay/commitment/retry/fallback classifier | `R5.1`, `R5.2`, decision 003 | ambiguous send, stable idempotency, bounded attempts and no post-commit reroute | Ready |

## R6: Request Planning, Artifacts, Payload, Files, Media, Public Endpoints

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R6.1` | `MOD-MESSAGES` | Route intent, target selection sequence, resource cost plan/admission and processing plan before heavy work | `R1.9`, `R4.0`-`R4.5`, `R5.0.kiro`, `R5.0.external` | offline plan parity, weighted admission and zero unselected work | Ready |
| `R6.2` | `MOD-REQUEST-ARTIFACTS` | Lazy raw/parsed/revisioned facts, token facts and serialized artifact cache | `R1.3`, `R6.1` | parse/copy/count/serialize operation budgets and invalidation | Ready |
| `R6.3.external-raw` | `MOD-PAYLOAD` | Raw path with zero forbidden parse/media/conversion/token work | `R6.2`, `R5.0.external` | byte golden, zero-heavy-stage and resource gates | Ready |
| `R6.3.external-normalized` | `MOD-PAYLOAD` | External normalized validation/repair/shaping plan, including target-specific reversible tool-schema policy | `R6.2`, `R5.0.external`, decision 012 | semantic golden, tool schema round trip/local reject, revision, limits and allocation | Ready |
| `R6.3.kiro-local` | `MOD-PAYLOAD` | Local Kiro validation/repair/shaping plan, including empty/null tool boundaries and reversible property mapping | `R6.2`, `R5.0.kiro`, decision 012 | semantic golden, tool schema/response round trip, tools/thinking/media/cache and allocation | Ready |
| `R6.4` | `MOD-FILES` | Complete shared `BoundedRawBody` upload/list/get/delete/materialize/retention implementation using only scoped governor streaming handles | `R0.3`, `R1.9`, `R2.4.files`, decision 010 | all routes, restart/failover, churn, streaming copy/bytes and backup/restore | Ready |
| `R6.5` | `MOD-MEDIA` | Complete bounded remote media/PDF clients and executors using only scoped governor byte/task/connection handles | `R0.2`, `R1.3`, `R1.9` | slow/large/cancel, SSRF/rebinding, allocator/RSS/task/FD recovery | Ready |
| `R6.6.models` | `MOD-MODEL-CATALOG` | Public Models/resolve/capability/pricing use case | `R2.7` | API parity and refresh convergence | Ready |
| `R6.6.count-tokens` | `MOD-TOKEN-COUNT` | Independent bounded count-tokens use case using only scoped governor tokenizer/connection handles | `R1.9`, `R6.2`, `R6.5`, `R2.7` | compatibility, timeout/cancel and resource recovery | Ready |
| `R6.7.models` | `MOD-TRANSPORT-PUBLIC` | Thin Models route/header-auth/runtime-capture/resource-admission/wire mapping; any body-bearing profile exposes only `BoundedRawBody` | `R1.5`, `R1.9`, `R6.6.models` | public golden, auth, declared/chunked limit and error mapping | Ready |
| `R6.7.files` | `MOD-TRANSPORT-PUBLIC` | Thin Files route that reserves declared `Content-Length`, atomically upgrades before retaining each chunk, stops on failure and passes only `BoundedRawBody` | `R1.5`, `R1.9`, `R6.4` | public golden, auth, declared/chunked limit, permit release and error mapping | Ready |
| `R6.7.count-tokens` | `MOD-TRANSPORT-PUBLIC` | Thin count-tokens route that reserves declared `Content-Length`, atomically upgrades before retaining each chunk, stops on failure and passes only `BoundedRawBody` | `R1.5`, `R1.9`, `R6.6.count-tokens` | public golden, auth, declared/chunked limit, permit release and error mapping | Ready |

## R7: SSE, Response, Terminal Lifecycle, Messages Transport

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R7.0` | `MOD-PROTO-SSE` | Canonical SSE/content-block codec and transition state machine | `R0.6`, `R1.5`, `R5.0.kiro` | event-order golden, malformed/truncated stream and allocation | Ready |
| `R7.1` | `MOD-RESPONSE` | Canonical response session, commitment, backpressure, tool-argument reverse mapping and neutral terminal facts | `R5.1`, `R5.2`, `R6.1`, `R6.3.*`, `R7.0`, decision 012 | response properties, tool schema round trip, protocol golden, slow client and cancellation | Ready |
| `R7.2.kiro-nonstream` | `MOD-RESPONSE` | Kiro non-stream response profile | `R7.1` | headers/body/error/usage and body limits | Ready |
| `R7.2.kiro-stream` | `MOD-RESPONSE` | Kiro Anthropic-compatible SSE profile | `R7.1` | event order, thinking/tools/usage, slow reader and disconnect | Ready |
| `R7.2.claude-code-stream` | `MOD-RESPONSE` | Claude Code `/cc` streaming profile | `R7.1` | exact CLI events, usage/cache/thinking/tools and disconnect | Ready |
| `R7.2.external-raw` | `MOD-RESPONSE` | External raw response passthrough profile | `R7.1` | byte/header/event passthrough, limits and commitment | Ready |
| `R7.2.external-normalized` | `MOD-RESPONSE` | External normalized response profile | `R7.1` | event/body/error/usage parity and limits | Ready |
| `R7.3` | `MOD-TERMINAL-LIFECYCLE` | One terminal reduction with stable IDs and typed journal/usage/credential/scheduler acknowledgements | `R2.3`, `R3.2`, `R4.2`-`R4.5`, all `R7.2.*`, decision 004 | duplicate callbacks, partial failures, restart/replay and lease independence | Ready |
| `R7.4.messages-transport` | `MOD-TRANSPORT-PUBLIC` | Thin Messages route that authenticates/captures once, reserves declared `Content-Length`, atomically upgrades chunked bodies before retention and passes only `BoundedRawBody` | all required `R7.2.*`, `R7.3`, `R1.5`, `R1.9` | every route family, auth, declared/chunked limits, stream/non-stream, errors, permit release and one capture | Ready |

## R8: Admin Authorities, Generated Contract, Browser Harness, Both UIs

### Backend And Harness Units

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R8.1.runtime-config` | `MOD-RUNTIME-CONFIG` | Runtime-config Admin commands/queries and owner-authoritative Rust schema fragment without broad AdminService | `R2.1`, `R2.2`, `R2.4.audit`, decision 013 | API/storage/conflict/audit/schema | Ready |
| `R8.1.auth` | `MOD-AUTH` | Masked/reveal-once auth commands/queries, revocation and owner-authoritative Rust schema fragment | `R2.6`, `R2.4.audit`, decision 013 | API/security/revocation/audit/schema | Ready |
| `R8.1.model-catalog` | `MOD-MODEL-CATALOG` | Catalog commands/queries/refresh and owner-authoritative Rust schema fragment | `R2.7`, `R2.4.audit`, decision 013 | API/refresh/conflict/audit/schema | Ready |
| `R8.1.credentials` | `MOD-CREDENTIALS` | Credential commands/queries/refresh/outcomes and owner-authoritative Rust schema fragment | `R4.3`, `R2.4.audit`, decision 013 | API/secret/refresh/audit/schema | Ready |
| `R8.1.proxy-resources` | `MOD-PROXY-RESOURCES` | Proxy CRUD/test/binding/masking commands/queries and owner-authoritative Rust schema fragment | `R4.0`, `R2.4.audit`, decision 013 | API/secret/binding/audit/schema | Ready |
| `R8.1.external-pools` | `MOD-EXTERNAL-POOLS` | Pool commands/queries/manual-auto state and owner-authoritative Rust schema fragment | `R4.4`, `R2.4.audit`, decision 013 | API/state/audit/schema | Ready |
| `R8.1.usage` | `MOD-USAGE` | Usage/accounting/dashboard queries, audited rebuild commands and owner-authoritative Rust schema fragment | `R3.2`, `R3.3`, `R2.4.audit`, decision 013 | exact search/query/rebuild/audit/schema | Ready |
| `R8.1.audit` | `MOD-AUDIT` | Audit query/retention/export commands and owner-authoritative Rust schema fragment | `R2.4.audit` | transaction/query/retention/schema | Ready |
| `R8.1.maintenance-jobs` | `MOD-MAINTENANCE-JOBS` | Audited submit/query/cancel/progress/checkpoint workflows and owner-authoritative Rust schema fragment | `R2.4.maintenance-jobs`, `R2.4.audit`, decision 013 | race/restart/cancel/resource/audit/schema | Ready |
| `R8.2` | `MOD-FRONTEND-CONTRACT` | Compose the `R8.1.*` owner schema fragments plus accepted transport metadata and generate TypeScript client/types for both apps; the generator is independent of final `R8.5` wiring | all `R8.1.*` | generation, schema diff, no handwritten drift, secret semantics and dependency-cycle rejection | Ready |
| `R8.3` | `MOD-BROWSER-HARNESS` | Final isolated component/browser/accessibility/responsive harness | `R8.2` | harness self-test, browser matrix, bounded screenshots/artifacts and cleanup | Ready |
| `R8.5` | `MOD-TRANSPORT-ADMIN` | Wire the generated `R8.2` contract into thin Admin auth/validation/error/dispatch; capture one runtime version, reserve declared `Content-Length`, atomically upgrade chunked bodies before retention and pass only `BoundedRawBody`, without feeding schema back into `R8.2` | `R1.2`, `R1.9`, all `R8.1.*`, `R8.2` | complete Admin API, single runtime capture, auth, declared/chunked limits, permit release, schema, errors, no dependency cycle and no broad service import | Ready |

### Exact Frontend Workflow Units

Both applications are mandatory. Every row depends on `R8.2`, `R8.3`, and the named backend units; all workflows integrate into the same final frontend artifacts and are never separately released.

| Workflow | Backend dependencies | `MOD-ADMIN-UI` unit | `MOD-OPERATOR-UI` unit | Minimum evidence | State |
| --- | --- | --- | --- | --- | --- |
| runtime config | `R8.1.runtime-config` | `R8.4.admin-ui.runtime-config` | `R8.4.operator-ui.runtime-config` | component/browser/conflict/accessibility/responsive | Ready |
| auth | `R8.1.auth` | `R8.4.admin-ui.auth` | `R8.4.operator-ui.auth` | reveal-once/no persistent key/revoke/browser/security | Ready |
| model catalog | `R8.1.model-catalog` | `R8.4.admin-ui.model-catalog` | `R8.4.operator-ui.model-catalog` | refresh/error/stale/accessibility/responsive | Ready |
| credentials | `R8.1.credentials`, `R8.1.proxy-resources` | `R8.4.admin-ui.credentials` | `R8.4.operator-ui.credentials` | keep/replace/clear/binding/reveal/accessibility | Ready |
| proxy resources | `R8.1.proxy-resources` | `R8.4.admin-ui.proxy-resources` | `R8.4.operator-ui.proxy-resources` | CRUD/test/masking/binding/destructive confirmation | Ready |
| external pools | `R8.1.external-pools` | `R8.4.admin-ui.external-pools` | `R8.4.operator-ui.external-pools` | state/test/failure/retry/accessibility | Ready |
| usage | `R8.1.usage` | `R8.4.admin-ui.usage` | `R8.4.operator-ui.usage` | exact search/loading/empty/error/performance | Ready |
| audit | `R8.1.audit` | `R8.4.admin-ui.audit` | `R8.4.operator-ui.audit` | query/filter/retention/export/error | Ready |
| maintenance jobs | `R8.1.maintenance-jobs` | `R8.4.admin-ui.maintenance-jobs` | `R8.4.operator-ui.maintenance-jobs` | progress/restart/cancel/partial failure | Ready |
| validation | credentials/proxy/pools/catalog backend units | `R8.4.admin-ui.validation` | `R8.4.operator-ui.validation` | test workflow, cancellation, errors and bounded results | Ready |
| overview/system | credentials/proxy/pools/usage plus later readiness facts | `R8.4.admin-ui.overview-system` | `R8.4.operator-ui.overview-system` | accurate states, degraded/readiness, responsive/accessibility | Ready |

## R9: Lifecycle, Recovery, Real Clients, Release

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R9.1.supervisor` | `MOD-SUPERVISOR` | Named tasks, producer barriers, ordered drain and exact residue report | all module drain contracts, decision 006 | full SIGTERM/fault matrix and non-zero critical residue | Ready |
| `R9.1.readiness` | `MOD-READINESS` | Bounded health/degraded/readiness authority plus expected-instance/release-generation PgSQL registry: each expected instance commits one manifest-bound attestation heartbeat per 5-second interval, freshness TTL is 15 seconds, current/rollback-capable expected rows are never pruned to manufacture quorum, and a no-longer-rollback-capable terminal generation's heartbeat rows prune in batches of at most 128 only after 24 hours and manifest/evidence retention | stable module health contracts, decision 014 | dependency/backlog/writer/scheduler/auth/migration/recovery states; 5-second heartbeat, 10-second local self-proof failure, 15-second TTL, bounded 24-hour terminal prune, missing/stale/unexpected instances, and PgSQL read/write failure that closes readiness/admission with no local or Redis fallback | Ready |
| `R9.1.health` | `MOD-TRANSPORT-HEALTH` | Thin `/healthz` and `/readyz` projection | `R9.1.readiness` | API truthfulness and dependency/backlog/worker states | Ready |
| `R9.1.bootstrap` | `MOD-BOOTSTRAP` | Thin composition and startup invoking `MOD-MIGRATIONS`; no business or domain DDL | `R2.0.migration-foundation`, all target modules, `R9.1.supervisor`, `R9.1.readiness`, `R9.1.health` | fresh/legacy startup, blocked migration, assembly and shutdown | Ready |
| `R9.2` | `MOD-RECOVERY` | Backup/restore verification, expected-instance Redis rebuild/epoch barrier, previous-binary state matrix and forward-recovery orchestration through the public readiness generation-registry view | `MOD-MIGRATIONS`, `R9.1.readiness`, all state authorities, decisions 010-011/014 | isolated RPO/RTO restore, missing/partitioned replica rebuild, generation-registry failure, per-authority rollback and forward-reconcile drills | Ready |
| `R9.3.contract` | `MOD-CONTRACT-HARNESS` | Final-candidate invocation manifests; harness implementation remains the R0 version | stable public contracts | complete deterministic compatibility matrix and cleanup | Ready |
| `R9.3.load-chaos` | `MOD-LOAD-CHAOS-HARNESS` | Final-candidate load/chaos manifests; harness implementation remains the R0 version | all performance-affecting modules | absolute/relative/resource/recovery/stability gates | Ready |
| `R9.3.real-client` | `MOD-REAL-CLIENT-HARNESS` | Real Claude Code and bounded independent real Kiro validation | public target candidate, R0 fixtures | 3x20-turn Claude sessions, request/token/cost-capped Kiro, sanitized artifacts | Ready |
| `R9.3.browser` | `MOD-BROWSER-HARNESS` | Complete two-app system browser manifest including readiness | both final apps, `R9.1.readiness` | workflows, cross-page state, accessibility, responsive and cleanup | Ready |
| `R9.4` | `MOD-RELEASE-HARNESS` | Produce and sign one `ReleaseGenerationManifest` binding backend/image, both frontends, schema/migration/config hashes, expected instances/resource profiles, previous artifact and transition identity; replace/register the obsolete deployment guide and missing secret-safe Claude Code local-testing runbook with supported target runbooks | all target modules and harness summaries, decision 014 | clean build/export/signature/consumer verification, target deployment and Claude Code runbook command/link checks from a clean checkout, secret scan and artifact budget | Ready |

## R10: Final Candidate Closure

Legacy code is normally deleted within the module work that replaces it, before that work reaches `Integrated`. R10 is a global proof that nothing escaped those module-level deletion obligations; it is not a delayed second rewrite.

| Work unit | Technical authority | Final output and integration boundary | Depends on | Required evidence | State |
| --- | --- | --- | --- | --- | --- |
| `R10.1` | all 50 `MOD-*` authorities plus `MOD-ARCH-FITNESS` | Zero target-runtime legacy imports/fallbacks/selectors, zero duplicate old UI/workflow, zero obsolete writer/reader/schema path, zero release stubs/fakes | every prior work unit | exhaustive source/import/config/schema/key/asset/harness inventory and deletion checks | Ready |
| `R10.2` | `MOD-ARCH-FITNESS`, `MOD-RELEASE-HARNESS` | Frozen complete target candidate and verified signed `ReleaseGenerationManifest` identity | `R10.1`, `R9.4` | full post-deletion static/Rust/storage/protocol/frontend/load/recovery/release gates | Ready |
| `R10.3` | complete system | Dress rehearsal, one final cutover, generation-fenced whole-system rollback, observation and post-window schema/secret compatibility contraction | `R10.2`, decisions 009-014 | exact one-window migration/cutover/rollback-state matrix, 60-minute/100k preproduction stability, 24-hour observation, post-contract full gates | Ready |

## State Vocabulary

| State | Meaning |
| --- | --- |
| `Ready` | Final scope, contracts, dependencies, conservative policy, gates, and deletion conditions are specified; implementation may start |
| `Implementing` | Target code, exact symbol mapping, fixtures, or focused evidence is being produced |
| `Integrated` | Target code is connected to the target-only candidate, legacy source for that responsibility is removed, and focused/post-deletion checks pass |
| `Verified In Candidate` | The integrated module also passes all currently applicable aggregate target-candidate gates |
| `Blocked` | A discovered fact contradicts an accepted contract or required evidence cannot be produced; it is recorded explicitly |

There is no module-level `Canary`, `Default On`, `Soaking`, or production `Done` state. Only the complete target system can become production-authoritative and complete under decision 009.
