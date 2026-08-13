# Complete Rewrite Inventory

Role: Coverage ledger for the accepted complete target-only modular rewrite

Status: Accepted source-coverage inventory; every implementation row is Not Started

Authority: Defines what “complete rewrite” covers under decisions 007-014 and what implementation/deletion evidence closes each row

As of: `v0.0.102`, commit `e9479df71ee0`, updated 2026-07-12

Read when: Starting module work, checking target coverage, deleting legacy source, freezing the complete candidate, or declaring completion

Related: [Decision 009](../decisions/009-single-program-modular-build-and-final-cutover.md), [Target module ledger](target-module-ledger.md), [Modular work map](execution-slice-map.md), [Module contracts](../topics/architecture/module-boundaries-and-contracts.md), [Complete plan](../topics/delivery/migration-sequence.md), [Roadmap](../roadmap.md)

## Coverage Rule

Every first-party executable source file, maintained frontend source file, and validation/release harness must match exactly one inventory row. Adding a new first-party path requires updating this inventory or placing it directly inside an already defined target module.

This is a **source coverage ledger**, not the work-unit ledger. A current file may contain several unrelated responsibilities and therefore name several target technical authorities or dependency groups. When the first affected work unit starts, its pinned audit expands that row into exact responsibility/symbol mappings and maps every symbol to one target module ID. That version-specific map and deletion checklist are implementation outputs.

Rules for multi-authority rows:

- each responsibility/symbol has exactly one target technical authority even when it consumes public contracts from other modules;
- a broad old file is never copied wholesale into a target directory to avoid producing a new God Object;
- the old file row remains `Not Started` or `In Progress` until every responsibility is integrated in the target-only candidate and its legacy symbols/fallbacks are deleted;
- completion counts are derived from landed module/work-unit evidence, not files moved;
- adding a responsibility to an old file during modernization is allowed only as a separately authorized incident hotfix and cannot silently change target authority.

An inventory row is Done only when:

1. its new target module owns the responsibility;
2. current behavior and accepted defect changes are covered by tests;
3. the target implementation passes focused and affected target-candidate integration gates;
4. the old implementation, selector and hidden fallback are deleted from target source;
5. old white-box tests are removed or rewritten against the new owner;
6. import/dependency search finds no live old reference;
7. durable implementation/integration/deletion evidence is linked;
8. every responsibility/symbol mapping for a multi-authority row is closed in the module and modular-work ledgers.

Copying an old production file wholesale into a new directory does not count as a rewrite. Old code may be read for behavior discovery. Protocol vectors, fixtures, property invariants, and sanitized golden data may be retained as test evidence. A pure algorithm or codec is still reimplemented inside the new boundary or replaced by a proven library; any exception requires an accepted inventory-specific decision.

## Canonical Module Coverage Cross-Check

This appendix makes omission mechanically detectable without turning this source-path ledger into a duplicate module ledger. The row-level pinned symbol map remains an implementation output, but every one of the 50 accepted module IDs must remain associated with exactly one inventory surface below and with an exact modular-work row. The union of these cells must equal the canonical definition set in `target-module-ledger.md`, with no duplicate or unknown ID.

| Inventory surface | Canonical target modules |
| --- | --- |
| Foundation, protocol, transport and telemetry | `MOD-KERNEL`, `MOD-RESOURCE-GOVERNOR`, `MOD-SECRET-ENVELOPE`, `MOD-OBSERVABILITY`, `MOD-DIAGNOSTICS`, `MOD-PROTO-ANTHROPIC`, `MOD-PROTO-KIRO`, `MOD-PROTO-EXTERNAL`, `MOD-PROTO-SSE`, `MOD-TRANSPORT-PUBLIC`, `MOD-TRANSPORT-ADMIN`, `MOD-TRANSPORT-HEALTH` |
| Durable/control authorities | `MOD-RUNTIME-CONFIG`, `MOD-AUTH`, `MOD-MODEL-CATALOG`, `MOD-CREDENTIALS`, `MOD-PROXY-RESOURCES`, `MOD-EXTERNAL-POOLS`, `MOD-TERMINAL-JOURNAL`, `MOD-USAGE`, `MOD-PROMPT-CACHE`, `MOD-FILES`, `MOD-AUDIT`, `MOD-MAINTENANCE-JOBS`, `MOD-MIGRATIONS` |
| Request/data plane | `MOD-SCHEDULER-LOCAL`, `MOD-SCHEDULER-EXTERNAL`, `MOD-MESSAGES`, `MOD-REQUEST-ARTIFACTS`, `MOD-PAYLOAD`, `MOD-KIRO-UPSTREAM`, `MOD-EXTERNAL-UPSTREAM`, `MOD-ATTEMPT-POLICY`, `MOD-RESPONSE`, `MOD-TERMINAL-LIFECYCLE`, `MOD-MEDIA`, `MOD-TOKEN-COUNT` |
| Lifecycle and recovery | `MOD-BOOTSTRAP`, `MOD-SUPERVISOR`, `MOD-READINESS`, `MOD-RECOVERY` |
| Maintained frontends | `MOD-FRONTEND-CONTRACT`, `MOD-ADMIN-UI`, `MOD-OPERATOR-UI` |
| Validation and release | `MOD-ARCH-FITNESS`, `MOD-CONTRACT-HARNESS`, `MOD-LOAD-CHAOS-HARNESS`, `MOD-REAL-CLIENT-HARNESS`, `MOD-BROWSER-HARNESS`, `MOD-RELEASE-HARNESS` |

## Treatment Vocabulary

| Treatment | Meaning |
| --- | --- |
| Rewrite | Implement the final responsibility in the target module, integrate target-only, then delete old code before release |
| Rewrite or proven-library replacement | Reimplement behind the target contract or adopt a separately justified maintained library; old custom implementation is deleted |
| Regenerate | Produce from the authoritative schema/build input; handwritten duplicate is deleted |
| Rewrite as black-box harness | Preserve behavior scenarios/data, replace implementation-specific harness/tests |
| Preserve as evidence/data | Non-executable historical material may remain; it is not part of runtime completion |
| Excluded third-party/generated | Dependency/build output is not rewritten merely for completeness; it remains governed by dependency/artifact policy |

## Rust Runtime Inventory

| Current source | Current responsibility | Target authority | Dependency group | Treatment | State |
| --- | --- | --- | --- | --- | --- |
| `src/main.rs` | Composition, startup, health, shutdown | `MOD-BOOTSTRAP`, `MOD-SUPERVISOR`, `MOD-READINESS` plus thin binary; construction does not transfer governor, secret-envelope or migration authority | R9 | Rewrite | Not Started |
| `src/common/*` | Request authentication helpers | `MOD-TRANSPORT-PUBLIC` header-auth boundary and `MOD-AUTH`; keyed verification uses only `PUBLIC(MOD-SECRET-ENVELOPE)` | R1/R2/R8 | Rewrite | Not Started |
| `src/model/arg.rs` | CLI/bootstrap arguments | `bootstrap::cli` | R9 | Rewrite | Not Started |
| `src/model/config.rs` | Boot/runtime config, policy DTOs/defaults/patches, including direct/default proxy secret, binding and resource-limit fields | `MOD-BOOTSTRAP` boot config plus owner-internal `MOD-RUNTIME-CONFIG`; secret mechanics move to `MOD-SECRET-ENVELOPE`, mutable permit state to `MOD-RESOURCE-GOVERNOR`, and credential/reusable-proxy concepts to their domain contracts during the required symbol audit | R1/R2/R4 | Rewrite | Not Started |
| `src/model/model_processing.rs`, `model_support.rs` | Model mapping/support policy | `domain::model` | R1/R5/R6 | Rewrite | Not Started |
| `src/model/mod.rs` | Old model module surface | target module exports | R1/R2 | Delete after rewritten owners exist | Not Started |
| `src/anthropic/router.rs`, `middleware.rs` | Public route/auth/state assembly and request-body collection | `MOD-TRANSPORT-PUBLIC` consuming `PUBLIC(MOD-RESOURCE-GOVERNOR)` before body retention and handing only `BoundedRawBody` to use cases | R1/R6/R7 | Rewrite | Not Started |
| `src/anthropic/types.rs`, `envelope.rs` | Anthropic wire DTO/error envelope | `protocol::anthropic` | R1/R7 | Rewrite or proven-library replacement | Not Started |
| `src/anthropic/handlers.rs`, `src/anthropic/handlers/{local_body_pipeline,parsed_body_pipeline,request_entry}.rs` | Messages entry/orchestration/local pipeline | `MOD-MESSAGES`, `MOD-REQUEST-ARTIFACTS` and `MOD-PAYLOAD`; only scoped `MOD-RESOURCE-GOVERNOR` handles cross the transport boundary | R6 | Rewrite | Not Started |
| `src/anthropic/body_capabilities.rs` | Body capability plan | target processing-plan policy | R6 | Rewrite; preserve characterized semantics | Not Started |
| `src/anthropic/body_processing.rs` | Files/remote media materialization | `MOD-FILES`/`MOD-MEDIA` owner code consuming scoped `MOD-RESOURCE-GOVERNOR` byte/task/connection handles | R0/R6 | Rewrite | Not Started |
| `src/anthropic/converter.rs`, `converter/*` | Anthropic-to-Kiro conversion | `protocol::kiro` and target body pipelines | R6 | Rewrite; preserve golden behavior | Not Started |
| `src/anthropic/payload_guard.rs`, `payload_guard_runtime.rs` | Sizing, repair, shaping, diagnostics | `domain::payload`, `application::messages::artifacts` | R6 | Rewrite | Not Started |
| `src/anthropic/request_facts.rs` | Lightweight raw facts/model rewrite | `application::messages::raw_facts` | R6 | Rewrite | Not Started |
| `src/anthropic/stream.rs` | Kiro event to Anthropic SSE state machine | `protocol::sse`, response application service | R7 | Rewrite; preserve golden event vectors | Not Started |
| `src/anthropic/cache.rs` | Cache/usage helpers | `domain::cache` | R3 | Rewrite | Not Started |
| `src/anthropic/prompt_cache.rs` | Fingerprints/tracker/simulation | `domain::cache`, accepted cache-state adapter | R3/R6 | Rewrite | Not Started |
| `src/anthropic/prompt_cache_creation_control.rs` | Creation frequency state | `domain::cache` and cache state port | R3 | Rewrite | Not Started |
| `src/anthropic/usage.rs` | Usage types, recorder, writers, query/dashboard | `domain::usage`, usage ports/workers/query services | R2/R3/R8 | Rewrite | Not Started |
| `src/anthropic/pricing.rs`, `model_capabilities.rs` | Catalog state/sync | `domain::model`, catalog repositories/workers | R2/R8 | Rewrite | Not Started |
| `src/anthropic/files.rs` | Process-local Files-compatible store | `MOD-FILES` shared store/port consuming `BoundedRawBody` or an owner-scoped governor streaming handle | R6/R9 | Rewrite | Not Started |
| `src/anthropic/tool_format_debug.rs` | Diagnostic capture/writer | `observability::diagnostics`, filesystem adapter/retention worker | R0/R9 | Rewrite | Not Started |
| `src/anthropic/websearch.rs` | WebSearch conversion/special path | target protocol/body/tool module | R6 | Rewrite | Not Started |
| `src/anthropic/mod.rs` | Old Anthropic module surface | target transport/protocol/application exports | R6/R7/R10 | Delete after replacement | Not Started |
| `src/kiro/model/**` | Credentials, proxy-resource binding IDs, token refresh, models, usage limits, IDE/CLI request and event wire families | `MOD-CREDENTIALS`, public `MOD-PROXY-RESOURCES` binding facts, `MOD-MODEL-CATALOG`, `MOD-PROTO-KIRO`, and Kiro auth adapters after responsibility-level mapping | R1/R4/R5 | Rewrite | Not Started |
| `src/kiro/token_manager/manager.rs` | Scheduler/refresh/state/persistence/Admin God Object, including proxy-resource catalog load/reload and binding resolution | `MOD-SCHEDULER-LOCAL`, `MOD-CREDENTIALS`, and `MOD-PROXY-RESOURCES` public catalog/binding contracts; exact symbols split in the entry audit | R4 | Rewrite | Not Started |
| `src/kiro/token_manager/{account_state,admin_snapshot,capacity,cooldown,queue,refresh,route_state,rpm,sticky,strategy,types}.rs` | Scheduler algorithms/state helpers, proxy-resource runtime records/availability and credential binding projections | `MOD-SCHEDULER-LOCAL`, `MOD-CREDENTIALS`, and `MOD-PROXY-RESOURCES`; schedulers consume the proxy owner's immutable public view | R4 | Rewrite; retain only characterization vectors | Not Started |
| `src/kiro/token_manager/{concurrency,redis_runtime,storage_task}.rs` | Lease/Redis/background executor and legacy process-local concurrency | scheduler-state adapters/supervised workers; process admission and global permit state move to `MOD-RESOURCE-GOVERNOR` | R1/R2/R4/R9 | Rewrite | Not Started |
| `src/kiro/token_manager/mod.rs` | Old manager module surface | target scheduler/credential exports | R4/R10 | Delete after replacement | Not Started |
| `src/kiro/provider.rs` | Kiro transport/retry/completion and proxy-keyed reusable-client lifecycle | `MOD-KIRO-UPSTREAM` consuming bounded resolved transport facts from `MOD-CREDENTIALS` and `MOD-PROXY-RESOURCES`; terminal behavior moves to R7 owners | R5/R7 | Rewrite | Not Started |
| `src/kiro/endpoint/*` | IDE/CLI request envelopes | `protocol::kiro::{ide,cli}` and Kiro adapters | R5 | Rewrite | Not Started |
| `src/kiro/parser/*` | AWS/Kiro event framing, CRC, decoding | `protocol::kiro::event_stream` | R5/R7 | Rewrite or proven-library replacement; retain test vectors | Not Started |
| `src/kiro/protocol.rs` | Kiro wire DTOs | `protocol::kiro` | R5/R7 | Rewrite | Not Started |
| `src/kiro/call_trace.rs` | Kiro attempt tracing | typed attempt/observability events | R1/R5/R7 | Rewrite | Not Started |
| `src/kiro/machine_id.rs` | Upstream identity helper | Kiro auth/endpoint adapter | R5 | Rewrite | Not Started |
| `src/kiro/mod.rs` | Old Kiro module surface | target protocol/domain/adapters | R5/R7/R10 | Delete after replacement | Not Started |
| `src/external_pool.rs` | External DTOs/selection/transport/stream/usage | domain/application/external adapter modules | R3/R4/R5/R6/R7 | Rewrite | Not Started |
| `src/external_pool/{body_pipeline,model_pipeline,retry_pipeline,usage_projection}.rs` | Extracted external stages | target processing/model/error/usage modules | R3/R5/R6 | Rewrite; preserve characterized semantics | Not Started |
| `src/admin/router.rs`, `handlers.rs`, `middleware.rs`, `error.rs`, `types.rs` | Admin HTTP/auth/body/DTO surface, including proxy-resource CRUD/test and plaintext-secret response DTOs | `MOD-TRANSPORT-ADMIN` consuming `MOD-RESOURCE-GOVERNOR` before body retention plus Rust-authoritative `MOD-FRONTEND-CONTRACT`; proxy-resource commands/queries map only to `MOD-PROXY-RESOURCES` | R8 | Rewrite/regenerate | Not Started |
| `src/admin/service.rs` | Broad Admin application service, including proxy-resource repository access, test, audit, reload and credential binding logic | domain-owned command/query contracts, explicitly including `MOD-PROXY-RESOURCES`, rather than a replacement Admin facade | R4/R8 | Rewrite | Not Started |
| `src/admin/mod.rs` | Old Admin module surface | target Admin transport/application exports | R8/R10 | Delete after replacement | Not Started |
| `src/admin_ui/*` | Embedded frontend asset routing | target bootstrap/static-asset adapter | R8/R9 | Rewrite | Not Started |
| `src/storage/postgres.rs` | All durable repositories/migrations, plaintext secret serialization, `ProxyResourceRow`, table/CRUD, current shared runner and startup repair/backfill work | PgSQL adapters by domain including `MOD-PROXY-RESOURCES`; domains consume `PUBLIC(MOD-SECRET-ENVELOPE)` for secret fields, `MOD-MIGRATIONS` owns common runner/ledger only, and `MOD-MAINTENANCE-JOBS` executes bounded owner backfills | R1/R2/R3/R8 | Rewrite | Not Started |
| `src/storage/redis_cache.rs` | All Redis coordination/derived cache | `adapters::redis/*` by consistency class | R2/R3/R4 | Rewrite | Not Started |
| `src/storage/mod.rs` | Old broad storage surface | target port/adapter exports | R2/R10 | Delete after replacement | Not Started |
| `src/http_client.rs` | Serialization/compression HTTP helper | target upstream/body adapters consuming scoped `MOD-RESOURCE-GOVERNOR` connection/response handles | R1/R5/R6 | Rewrite | Not Started |
| `src/token.rs` | Local/remote token counting | `MOD-TOKEN-COUNT` and tokenizer adapter consuming scoped governor handles | R1/R6 | Rewrite | Not Started |
| `src/debug.rs` | Debug helpers | bounded observability diagnostics | R0/R9 | Rewrite or delete if obsolete | Not Started |

## Rust Test And Validation Inventory

| Current source | Target | Dependency group | Treatment | State |
| --- | --- | --- | --- | --- |
| `src/anthropic/handlers/tests.rs` | Black-box Messages/route/body contracts plus new module tests | R6/R7 | Rewrite as black-box harness; retain sanitized fixtures | Not Started |
| `src/external_pool/tests.rs` | External adapter/route/path/header/usage contracts | R3/R5/R6/R7 | Rewrite as black-box harness | Not Started |
| `src/kiro/token_manager/manager_tests.rs` | Pure scheduler property/parity plus real Redis/PgSQL coordination tests | R4 | Rewrite as black-box/pure harness | Not Started |
| `src/admin/service_tests.rs` | Domain/API tests, including `MOD-PROXY-RESOURCES` CRUD/test/binding/masking and owner-specific secret-response contracts | R4/R8 | Rewrite as domain/API tests | Not Started |
| Embedded `#[cfg(test)]` sections in rewritten modules (section scope, not a second whole-file coverage row) | Tests owned by new module boundaries | Corresponding dependency group | Rewrite; protocol vectors may be retained as data | Not Started |
| `src/test.rs` | Shared isolated fixture/build helpers | R9 | Rewrite | Not Started |
| `src/bin/kiro_loadtest.rs` | R0-valid target/outcome/resource measurement, R1 canonical workload/metric harness, R9 CI/release integration | R0/R1/R9 | Rewrite as black-box harness | Not Started |

Old white-box tests are not retained merely to keep test counts high. Useful behavior is moved to contract/property/integration fixtures owned by the replacement module.

## Frontend Inventory

| Current source | Target | Dependency group | Treatment | State |
| --- | --- | --- | --- | --- |
| `admin-ui/src/**` except `admin-ui/src/types/api.ts` | `MOD-ADMIN-UI` rewritten `/admin` workflows with generated client, including exact `R8.4.admin-ui.proxy-resources` and credential-binding consumers | R8 | Rewrite | Not Started |
| `ui/src/**` except `ui/src/types/api.ts` | `MOD-OPERATOR-UI` rewritten `/ui` workflows with generated client, including exact `R8.4.operator-ui.proxy-resources` and credential-binding consumers | R8 | Rewrite | Not Started |
| `admin-ui/src/types/api.ts`, `ui/src/types/api.ts` | Rust-authoritative schema and generated TypeScript client/types | R8 | Regenerate; delete handwritten duplicates | Not Started |
| `admin-ui/public/**`, `ui/public/**` | Reviewed static assets owned by rewritten apps | R8 | Recreate/retain only explicitly approved source assets | Not Started |
| `admin-ui/{.npmrc,index.html,package.json,pnpm-lock.yaml,pnpm-workspace.yaml,postcss.config.js,tailwind.config.js,tsconfig.json,vite.config.ts}`, `ui/{.npmrc,index.html,package.json,pnpm-lock.yaml,pnpm-workspace.yaml,tsconfig.json,vite.config.ts}` | Reproducible pinned builds for both rewritten apps | R8/R9 | Rewrite/update as required; no blind duplication | Not Started |

Both frontends remain in scope unless a separate accepted product decision retires one before its rewrite. A retirement decision must prove workflow parity/migration and then changes that row from Rewrite to Accepted Removal.

## Script, CI, Deployment, And Release Inventory

| Current source | Target | Dependency group | Treatment | State |
| --- | --- | --- | --- | --- |
| `scripts/loadtest/*` | One manifest-driven, host-safe load/fake-upstream harness founded in R0/R1 and integrated into CI/release in R9 | R0/R1/R9 | Rewrite | Not Started |
| `scripts/analyze_claude_transcript_queue.js` | Real-client transcript/result-accounting helper with bounded, redacted input/output | R9 | Rewrite under `MOD-REAL-CLIENT-HARNESS` or delete if the accepted harness makes it obsolete | Not Started |
| `scripts/check-frontend-contracts.mjs` | Rust-schema generation/drift gate | R8 | Replace and delete old comparison | Not Started |
| `scripts/ci/*` | Dependency, lint, schema, evidence, performance, artifact gates | R8/R9/R10 | Rewrite/update | Not Started |
| `scripts/dev-ui.sh` | Isolated reproducible frontend development launcher | R8/R9 | Rewrite/update | Not Started |
| `.codex/skills/kiro-claude-cli-validation/**` | Operator-facing real Claude Code/Kiro validation workflow aligned with the accepted contract manifest | R9 | Rewrite/update under `MOD-REAL-CLIENT-HARNESS`; preserve only current sanitized matrices | Not Started |
| `.codex/skills/kiro-load-chaos-validation/**` | Operator-facing load/chaos workflow aligned with the accepted workload/report schema | R0/R1/R9 | Rewrite/update under `MOD-LOAD-CHAOS-HARNESS`; delete stale instructions | Not Started |
| `tools/event-viewer.html` | Local AWS/Kiro event inspection tool and potential sensitive diagnostic surface | R0/R5/R9 | Review; rewrite under bounded diagnostics/contract tooling or delete if obsolete | Not Started |
| `.github/workflows/build.yaml` | Complete static/storage/frontend/protocol/perf gate orchestration | R9 | Rewrite | Not Started |
| `.github/workflows/docker-build.yaml` | Complete image build/export, SBOM, signing, provenance and signed `ReleaseGenerationManifest` | R9 | Rewrite | Not Started |
| `Dockerfile`, `docker-compose*.yml` | Target bootstrap/readiness, immutable release-generation examples, expected-instance identity and accepted hardening scope | R9 | Rewrite/update | Not Started |
| `docs/ai-docker-compose-deployment.md`, absent `docs/claude-code-cli-local-testing.md` entrypoint | Supported target deployment runbook and secret-safe Claude Code local-testing runbook registered from maintained entrypoints and bound to the release manifest | R9 | Replace the obsolete deployment guide; create/restore the maintained CLI guide; remove or update stale links | Not Started |
| `.cargo/config.toml`, `.dockerignore`, `.gitignore` | Reproducible build inputs plus protected/generated artifact boundaries | R0/R9/R10 | Update with target build and cleanup ownership; preserve user/runtime protection rules | Not Started |
| `.claude/settings.local.json` | Tracked project-local AI-tool network/permission policy that can affect validation behavior | R0/R9 | Review, minimize, document or delete as project tooling; never treat it as product/runtime configuration or a place for secrets | Not Started |
| `config.example.json`, `credentials.example.*.json`, `data/kiro-upstream-models.seed.json` | Version-compatible configuration/credential examples and model-catalog seed data | R2/R8/R9 | Regenerate or update from accepted schema/catalog contracts; never copy runtime secrets | Not Started |
| `Cargo.toml`, `Cargo.lock` | Target Rust module/dependency/build authority | All/R9 | Update as required; third-party code is excluded | Not Started |

## Excluded Or Preserved Material

| Material | Rule |
| --- | --- |
| `docs/plantree`, historical `docs/**` | Preserve active specifications and unique evidence; classify tracked legacy documents through the reviewed disposition policy, allowing deletion only for superseded analysis-only material with no active reference or independent evidence; documentation is not runtime code |
| Sanitized protocol vectors and deterministic fixtures | Preserve as data when still valid; no executable old implementation is retained through them |
| Dependency lockfile contents | Covered by the corresponding Rust/frontend build row; regenerated or updated by reviewed dependency changes rather than rewritten as runtime source |
| Third-party crates/npm packages/toolchains | Excluded from first-party rewrite; select/replace through dependency and security decisions |
| `target/**`, `.local-run/**`, `tmp/**`, `logs/**`, `node_modules/**`, frontend `dist/**` | Generated/runtime artifacts governed by repository cleanup policy, not rewrite completion |
| Config/credential/runtime data | Migrated through compatibility rules; never discarded to claim a clean rewrite |

## Completion Ledger

`R0` through `R10` are dependency groups inside one implementation. Counts summarize target-only integration and legacy deletion; they do not represent phased production activation.

| Dependency group | Inventory rows integrated | Old rows deleted | Evidence | State |
| --- | ---: | ---: | --- | --- |
| R0 | 0 | 0 | None | Not Started |
| R1 | 0 | 0 | None | Not Started |
| R2 | 0 | 0 | None | Not Started |
| R3 | 0 | 0 | None | Not Started |
| R4 | 0 | 0 | None | Not Started |
| R5 | 0 | 0 | None | Not Started |
| R6 | 0 | 0 | None | Not Started |
| R7 | 0 | 0 | None | Not Started |
| R8 | 0 | 0 | None | Not Started |
| R9 | 0 | 0 | None | Not Started |
| R10 | 0 | 0 | None | Not Started |

The completion ledger is updated only with landed code and durable evidence. Planning documents do not increase these counts.
