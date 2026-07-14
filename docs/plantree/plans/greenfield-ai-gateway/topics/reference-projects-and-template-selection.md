# Reference Projects And Admin Template Selection

Role: Dated external-project research, reference boundaries and frontend-template decision

Status: Reviewed on 2026-07-13; refresh before implementation if upstream state materially changes

Authority: Defines which external projects may inform the target and what must not be copied or treated as evidence

Related: [Plan root](../README.md), [complete reconstruction plan](complete-reconstruction-plan.md), [decision 001](../decisions/001-greenfield-go-modular-ai-gateway.md)

## 1. Research Method

The review used official GitHub repositories, default-branch history, license files, release feeds, READMEs and package manifests available on 2026-07-13. Repository activity dates are point-in-time maintenance signals, not guarantees of future support or code quality.

Selection criteria:

- relevance to AI gateway routing, provider abstraction, streaming, usage, accounts, control/data plane or Admin workflows;
- Go or transferable high-performance gateway architecture;
- open-source license and enterprise/free boundary clarity;
- recent maintenance and visible release history;
- typed contracts, multi-replica behavior, dynamic configuration and observability;
- compatibility breadth and test corpus value;
- risk of reproducing a God object, lowest-common-denominator request model or operationally heavy dependency stack.

No project below is target performance, correctness or security evidence. Every borrowed idea must be revalidated against this plan's contracts and target tests.

## 2. Overall Recommendation

There is no suitable repository to fork and turn into the target system.

Use a composite reference strategy:

| Reference | Primary use |
| --- | --- |
| Bifrost | Closest Go LLM Gateway implementation and primary source-layout/transport/provider study |
| Envoy AI Gateway | Control/data plane separation, versioned resources, model routing, policy and Kubernetes HA study |
| LiteLLM | Provider/model/endpoint/capability/error/cost compatibility matrix and behavior corpus |
| Higress and APISIX | Streaming data plane, dynamic config, plugin isolation, distributed rate limiting and hard resource ceilings |
| Portkey Gateway | Human-readable declarative routing, fallback and guardrail configuration semantics |
| New API | Chinese operator workflows, channels/models/usage/pricing UI and product requirements only |
| One API | Historical compatibility and domestic deployment expectations only |

The new project independently defines module contracts, execution lifecycle, delivery evidence, replay safety, Kiro leases, usage facts, configuration publication and terminal semantics.

## 3. Backend And Gateway References

### 3.1 Comparison

| Project | Technology and license | Maintenance signal at review | What to study | What not to copy |
| --- | --- | --- | --- | --- |
| [Envoy AI Gateway](https://github.com/envoyproxy/ai-gateway) | Go control plane plus Envoy data plane; Apache-2.0 | default branch updated 2026-07-12; [v1.0.0 GA](https://github.com/envoyproxy/ai-gateway/releases/tag/v1.0.0) released 2026-06-23 | `AIGatewayRoute`, provider backends, `BackendSecurityPolicy`, model virtualization, fallback, usage-based rate limits, stable resource APIs, xDS updates and Kubernetes multi-replica deployment | mandatory Kubernetes Gateway API/Envoy/CRD/extProc stack; it does not provide Kiro account auth/refresh/lease or the required page control plane |
| [Bifrost](https://github.com/maximhq/bifrost) | Go; Apache-2.0 | default branch updated 2026-07-13; component tag `transports/v2.0.0-prerelease1` released 2026-07-07 while the stated product baseline remained v1.6.3 | provider packages, core schemas, transports, config/log/vector layers, Web UI, Anthropic/OpenAI/Bedrock/EventStream, streaming, provider key selection, benchmarks and tests | features presented by the README as enterprise unlocks require exact source/license/OSS-availability checks; growing shared schema/context coupling; project benchmark claims without independent reproduction |
| [LiteLLM](https://github.com/BerriAI/litellm) | Python; main repository MIT with separately licensed `enterprise/` content | updated 2026-07-12; v1.93.0-rc.1 released 2026-07-12 | broad provider, model and endpoint matrix; Anthropic/OpenAI/Responses/Files/audio/batch/rerank/MCP/A2A; error normalization, cost tables, virtual keys and compatibility fixtures | Python runtime/dependency graph; do not assume an OpenAI-centered normalized model preserves thinking/cache/tools/Files semantics without explicit lossless/lossy fixtures and capability negotiation; verify enterprise/OSS boundaries per file/feature |
| [Portkey Gateway](https://github.com/Portkey-AI/gateway) | TypeScript/Hono for Node and Workers; MIT | updated 2026-05-25; v1.15.2 released 2026-01-12; Gateway 2.0 still described as prerelease | declarative provider/fallback/load-balance/conditional routing, guardrails, adapter layout and lightweight edge concepts | Node/Workers state and connection assumptions; 1.x/2.0 transition; automatic retry as a substitute for delivery/commitment evidence |
| [Higress](https://github.com/higress-group/higress) | Go control plane plus Envoy/Istio and Wasm plugins; Apache-2.0; CNCF Sandbox | updated 2026-07-09; v2.2.3 released 2026-06-25 | streaming, no-loss config updates, Wasm isolation, LLM/MCP integration, token limits, caching, load balancing, observability and console workflows | mandatory Istio/Envoy/Wasm complexity; Wasm provider granularity for stateful Kiro auth, scheduler and EventStream lifecycle |
| [Apache APISIX](https://github.com/apache/apisix) | OpenResty/NGINX, Lua and etcd; Apache-2.0 | updated 2026-07-13; 3.17.0 released 2026-06-16 | stateless data plane, etcd config, hot plugins, health checks, `ai-proxy`, multi-provider fallback, Redis usage-based rate limits and explicit body/response/stream limits | non-Go runtime stack; NGINX/OpenResty/etcd operations; proxy-plugin model as a replacement for a full Kiro vertical module; naive fallback after uncertain execution |
| [New API](https://github.com/QuantumNous/new-api) | Go plus React; AGPL-3.0 | updated 2026-07-11; v1.0.0-rc.21 released 2026-07-11 | modern Chinese Admin IA; channels, models, tokens, usage, cache billing, pricing, groups, logs and operator workflows | copying or combining source before a recorded license/legal review; tenant/resale/billing data model driving the generic kernel |
| [One API](https://github.com/songquanpeng/one-api) | Go plus React; MIT | last main update/release observed 2025-02, about 17 months before review | historical channel/weight/model-map/token/quota/Admin expectations | inactive architecture base, old Go/dependencies, weak multi-instance synchronization assumptions, default security patterns and traditional coupled monolith structure |

### 3.2 Study Priority

#### A: Read Before Contract Implementation

1. Bifrost provider directories, core schemas, transports, config store, streaming/fallback tests and Web API configuration.
2. Envoy AI Gateway route/backend/security/quota APIs and its control-plane/data-plane update model.

The review must explicitly record where Bifrost's shared schemas/context are too broad and where Envoy's Kubernetes/Envoy assumptions do not fit the self-hosted all-in-one product.

#### B: Read By Topic

- LiteLLM for compatibility and error/cost/provider matrices.
- Higress for streaming, hot update and plugin-isolation lessons.
- APISIX for distributed rate limiting, stateless nodes and hard resource limits.
- Portkey for routing/fallback/guardrail control-plane language.

#### C: Product And Interaction Only

- New API for page IA, channel/account/model/pricing/usage and Chinese operator workflows. Do not copy or combine AGPL source before a recorded license/legal review determines derivative-work scope, network source-offer obligations, notices and distribution terms.
- One API for historical user expectations. Do not use it as an implementation base.

## 4. General Go Gateway References

The implementation may also study:

| Project | License/review signal | Reusable lesson | Boundary |
| --- | --- | --- | --- |
| [Caddy](https://github.com/caddyserver/caddy) | Apache-2.0; updated 2026-07-12 | Go module registration, structured config, lifecycle and composable handlers | do not copy its build-time plugin ecosystem as the provider contract |
| [Traefik](https://github.com/traefik/traefik) | MIT; updated 2026-07-10 | dynamic configuration snapshots, providers, observability and Go proxy operations | general HTTP proxy behavior does not solve AI semantics or replay safety |
| [KrakenD](https://github.com/krakend/krakend-ce) | Apache-2.0; updated 2026-07-08 | stateless Go API gateway composition and performance discipline | aggregation/fan-out model is not the target request lifecycle |
| [Envoy Gateway](https://github.com/envoyproxy/gateway) | Apache-2.0; updated 2026-07-12 | versioned control-plane resources, status/admission and Kubernetes deployment | avoid making Kubernetes mandatory for local/self-hosted operation |

These are architecture studies, not dependencies unless a focused decision demonstrates the need.

## 5. Selected Go Community Components

These are implementation candidates, not architecture authorities. Exact versions are pinned and vulnerability/license checked at repository bootstrap.

| Component | License/activity signal | Intended use and constraints |
| --- | --- | --- |
| [go-chi/chi](https://github.com/go-chi/chi) | MIT; updated 2026-07-05 | standard `net/http` public/Admin routing; no private all-system HTTP abstraction |
| [jackc/pgx](https://github.com/jackc/pgx) | MIT; updated 2026-06-30 | PostgreSQL driver, transactions, batching and notifications |
| [sqlc-dev/sqlc](https://github.com/sqlc-dev/sqlc) | MIT; updated 2026-07-05 | generated typed SQL query APIs; domain query files remain small and owned |
| [pressly/goose](https://github.com/pressly/goose) | MIT; updated 2026-06-30 | immutable embedded migrations with a project-owned runner/ledger policy |
| [redis/rueidis](https://github.com/redis/rueidis) | Apache-2.0; updated 2026-07-05 | Redis Cluster/Sentinel, pipelining and Lua/Function invocation; lease/fencing correctness remains project-owned |
| [connectrpc/connect-go](https://github.com/connectrpc/connect-go) | Apache-2.0; updated 2026-07-06 | future out-of-process module transport; not required for first-release in-process modules |
| [OpenTelemetry Go](https://github.com/open-telemetry/opentelemetry-go) | Apache-2.0; updated 2026-07-10 | traces, metrics and logs correlation across request/attempt/module boundaries |
| [Prometheus client_golang](https://github.com/prometheus/client_golang) | Apache-2.0; updated 2026-07-10 | low-cardinality process, scheduler, stream, resource and dependency metrics |
| [AWS SDK for Go v2](https://github.com/aws/aws-sdk-go-v2) EventStream | Apache-2.0; updated 2026-07-10 | preferred Kiro framing/CRC decoder if compatibility tests pass |

Do not select `fasthttp` merely for microbenchmark throughput. Standard `net/http` provides stronger HTTP/2, cancellation, streaming and ecosystem compatibility. Optimize connection reuse, buffer bounds, backpressure, JSON hot paths and Redis/PostgreSQL behavior with target workload evidence first.

## 6. Admin Template Review

### 6.1 Selection

Select [satnaing/shadcn-admin](https://github.com/satnaing/shadcn-admin) as the UI baseline at reviewed commit:

```text
e16c87f213a5ba5e45964e9b67c792105ec74d26
```

Use it selectively. Create a fresh React application, then adapt the application shell, navigation, command menu, data-table interactions, forms, responsive behavior and theme patterns. Do not fork the repository as the product architecture.

Reasons:

- React, TypeScript, Tailwind and Vite match the required stack;
- TanStack Router, Query and Table match a dense operational data application;
- React Hook Form and Zod support complex provider/config workflows;
- shadcn/Radix and Lucide provide accessibility-oriented, source-owned component/icon primitives whose final composition still requires full accessibility verification;
- responsive, dark/light, RTL, skip-to-main, command menu and table patterns already exist;
- the visual language is closer to a modern operational control plane than marketing-style dashboard templates.

Risks and controls:

- The author states that it is not a starter project. Import patterns deliberately and own the resulting architecture.
- Some shadcn components are customized. Track local deltas and do not expect automatic upgrades.
- The reviewed source uses React 19.2.x, TypeScript 6.0.x, Tailwind 4.2.x and Vite 8.0.x. Record source/target versions and pass install, typecheck, build, unit, accessibility and browser compatibility gates before locking newer target versions.
- Remove the partial/demo Clerk authentication and implement the Go-owned secure Admin session contract.
- Treat all business pages and data as demonstrations, not functionality or acceptance evidence.
- Preserve the MIT notice and the exact reviewed source reference.

### 6.2 Template Comparison

| Candidate | Stack, license and review signal | Strength | Reason not selected as base |
| --- | --- | --- | --- |
| [shadcn-admin](https://github.com/satnaing/shadcn-admin) | React 19.2, TypeScript 6, Tailwind 4.2, Vite 8; MIT; default branch last touched 2026-06-11 by dependency automation; last substantive human change observed 2026-04-21 | closest modern operational shell; TanStack/RHF/Zod/Recharts/Radix/Lucide; themes, responsive, RTL and table actions | selected, but only as a pattern source; source-to-target version compatibility must be proven |
| [TailAdmin React](https://github.com/TailAdmin/free-react-tailwind-admin-dashboard) | React 19, TypeScript, Tailwind 4, Vite; MIT; 2026-04-28 touch was version/changelog material; package 2.3.0; no Git tag/release anchor observed | broad page/visual/chart catalog and explicit Admin-template positioning | free source has basic table capability, lacks the selected query/table/form/accessibility foundation, has free/pro asset boundary and local SVG icons |
| [Refine](https://github.com/refinedev/refine) | headless CRUD framework; MIT; updated 2026-06-05; core 5.0.12 released 2026-04-02 | mature auth, access control, data provider, realtime, audit and i18n abstractions | overlaps TanStack/OpenAPI/domain actions; official shadcn example lagged on Refine 4/Tailwind 3/Vite 5; reconsider only if the UI proves predominantly generic CRUD |
| [Flowbite React](https://github.com/themesberg/flowbite-react) | React 18/19, Tailwind 3/4; MIT; 2026-06-27 last touch was README media; 0.12.17/component-code activity was observed earlier in 2026 | forms, table, pagination, sidebar, modal and theme components | lacks production data grid, query layer, schema forms and charts; mixing it with shadcn/Radix creates two component systems |
| [Flowbite Admin](https://github.com/themesberg/flowbite-admin-dashboard) | Hugo, Webpack and native Flowbite; MIT; updated 2025-03-20 | CRUD/table/chart layout references | not a React project and therefore cannot be the frontend base |
| [shadcn/ui](https://github.com/shadcn-ui/ui) | component source/blocks; MIT; active on review date | long-term component source for the selected approach | not a complete Admin template |
| [Tremor](https://github.com/tremorlabs/tremor) | Tailwind 4, Radix and Recharts; Apache-2.0; last observed update 2025-10-10 | accessibility-oriented dashboard/chart component patterns | not a complete Admin template, no formal tags observed, slower maintenance and overlapping selected components |

## 7. Final Frontend Stack

```text
React 19
+ TypeScript strict
+ Node.js 24 LTS and pnpm 11, exactly pinned
+ Vite 8
+ Tailwind CSS 4
+ shadcn/ui and Radix UI
+ Lucide React
+ TanStack Router, Query, Table and Virtual
+ React Hook Form and Zod
+ Recharts
+ generated OpenAPI TypeScript client
+ Vitest, Testing Library, `axe-core` and `@axe-core/playwright`
+ Playwright
```

The template does not supply production features. The project still implements server pagination/filter/sort, large-log virtualization, configuration draft/diff/preflight/publish, secure sessions, audit, real-time health, provider-specific workflows, conflict handling and complete accessibility.

## 8. License And Provenance Rules

- Pin every copied template/component source to an exact commit and record its source path.
- Inventory and comply with every dependency/source license, including MIT, ISC, Apache-2.0 and MPL-2.0 (`axe-core`/`@axe-core/playwright`), and generate a release `THIRD_PARTY_NOTICES` file.
- Keep copied source modifications traceable where the upstream license requires notices.
- Do not copy TailAdmin Pro or other paid assets into the repository.
- Do not copy or combine New API AGPL source until a recorded license/legal decision determines derivative-work scope, network source-offer obligations, notices and distribution terms for the intended use.
- Treat projects with enterprise directories or feature claims as mixed-license until the exact file and feature license is verified.
- Produce an SBOM and license scan for every release artifact.
- Re-run a focused maintenance/license review before importing code when the implementation begins; this dated document is not perpetual approval.

## 9. Final Reference Decision

The project will not fork, embed or organize itself around an existing AI Gateway. External projects are independent sources for behavior, architecture and interaction patterns. All target contracts are defined locally and proven by Kiro, a deterministic mock provider and at least one simple standard provider.

AGPL implementations and features presented by upstream projects as enterprise unlocks may inform publicly observable requirements and product semantics, but their source is not copied or relied upon without a separate file/feature license decision.
