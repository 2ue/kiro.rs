# Operator UI Planning Archive

Role: Provenance, disposition, and retrieval index for the June-July 2026 `ui/` refactor and dashboard-enhancement plans

Status: Archived on 2026-07-12; historical rationale and candidate source, not an active roadmap

Authority: Preserves dated frontend design reasoning and unresolved ideas without overriding current source, registered plans, or accepted decisions

Read when: Tracing the current `ui/` information architecture, reviewing old dashboard ideas, or auditing an R8 frontend workflow work unit

Related: [Documentation archive](../README.md), [modernization Admin/frontend architecture](../../plantree/plans/system-architecture-modernization/topics/architecture/admin-and-frontend-architecture.md), [R8 sequence](../../plantree/plans/system-architecture-modernization/topics/delivery/migration-sequence.md#dependency-group-r8-admin-backend-contract-browser-both-frontends), [legacy disposition](../../plantree/plans/system-architecture-modernization/indexes/legacy-document-disposition.md)

## Current Authority

- Exact current behavior: the current `ui/`, `admin-ui`, Rust Admin API, schema, and tests at the revision being reviewed.
- Landed Admin search/routing/config behavior: the registered [Admin observability and routing plan](../../plantree/plans/admin-observability-routing-config/README.md).
- Current rewrite target and dependency order: the [system architecture modernization plan](../../plantree/plans/system-architecture-modernization/README.md), its accepted decisions, and the accepted R8 target/work units.
- Product boundary: both maintained applications serve the same single operator. Neither application may be silently retired, and neither old document may reintroduce multi-user or tenant scope.

The archived files are inputs to an R8 entry audit. Their unfinished ideas are not accepted requirements, backlog commitments, or evidence that a feature landed.

## Archived Documents

| Archived document | Original path | Historical role | Last source commit | Pre-move blob |
| --- | --- | --- | --- | --- |
| [Frontend refactor plan](REFACTOR_PLAN.md) | `ui/REFACTOR_PLAN.md` | Partially landed IA/design plan with later-superseded backend and secret assumptions | `39c14ee64c0fd055c7d55f68e805c280f9c07357` | `0dbba1c4759de64b0ae3feb824d90377f0968bbb` |
| [Dashboard collection index](dashboard-enhancement/README.md) | `ui/docs/dashboard-enhancement/README.md` | Historical enhancement plan and reading map | `29480fa2a563b49cde3af9ff4d1e361d7715339c` | `042cd74ccac2e6bba139a592e05386541a59579e` |
| [Background and contract inventory](dashboard-enhancement/00-background.md) | `ui/docs/dashboard-enhancement/00-background.md` | Dated Admin/UI capability inventory | `29480fa2a563b49cde3af9ff4d1e361d7715339c` | `ad9e8729aa01f2e70f8601f6f16f532bd08d871c` |
| [Page architecture](dashboard-enhancement/01-architecture.md) | `ui/docs/dashboard-enhancement/01-architecture.md` | Proposed overview/analytics split | `466c6645aa11957488012ea8bab7f8a2eea799ab` | `1190f97d172b3ca67619fb2350b6d7ff3633ee62` |
| [Overview health modules](dashboard-enhancement/02-overview-health.md) | `ui/docs/dashboard-enhancement/02-overview-health.md` | Proposed health/alert capabilities | `466c6645aa11957488012ea8bab7f8a2eea799ab` | `fdca560eb792e343c159028557d6910d12030232` |
| [Analytics modules](dashboard-enhancement/03-analytics.md) | `ui/docs/dashboard-enhancement/03-analytics.md` | Proposed operations/cost analysis capabilities | `466c6645aa11957488012ea8bab7f8a2eea799ab` | `68704f41fdbac8e2cf5143556fe7afa80291fea4` |
| [Cross-cutting behavior](dashboard-enhancement/04-cross-cutting.md) | `ui/docs/dashboard-enhancement/04-cross-cutting.md` | Proposed freshness/threshold/refresh/navigation behavior | `466c6645aa11957488012ea8bab7f8a2eea799ab` | `34b1305eec3855a45436a3522b933b58309196ab` |
| [Historical dashboard roadmap](dashboard-enhancement/05-roadmap.md) | `ui/docs/dashboard-enhancement/05-roadmap.md` | Unaccepted implementation batches and acceptance notes | `466c6645aa11957488012ea8bab7f8a2eea799ab` | `82ea497a2868b9ec427e977cb236ff77046e835f` |

All blobs were verified against their recorded commits before the move.

## Feature Disposition

| Historical claim or idea | Disposition on 2026-07-12 | Durable route |
| --- | --- | --- |
| Four-domain information architecture, theme support, charts, and task-oriented navigation | Partially landed; exact behavior is owned by current `ui` source | Characterize both applications during the relevant `R8.4.<app>.<workflow>` entry audit; preserve useful behavior through browser fixtures |
| Only `ui/` is the maintained console | Superseded | Both `MOD-ADMIN-UI` and `MOD-OPERATOR-UI` remain in rewrite scope unless a separate accepted retirement decision proves migration |
| Backend API and business logic must never change | Superseded | R8 replaces accepted backend technical authorities and transport only inside the target-only candidate; they activate in the one whole-system cutover and roll back only with the previous complete release, never through a per-module selector |
| Persist the reusable Admin key in `localStorage` | Superseded and unsafe | `SEC-005`, `FUN-025`, `QA-SEC-007`, and decisions 010/011 fix the target session/reveal-once policy and removal of indefinite JavaScript-readable retention |
| Usage-writer health and data-loss visibility | Unresolved candidate; a TypeScript shape exists but no complete API/hook/workflow was confirmed | Re-audit under the usage/Admin domain and both affected R8 UI workflow work units; promote a finding only if current evidence and requirements justify one |
| External-pool realtime health, aggregate health/alerts, top conversations, cost drill-down, and a separate analytics workflow | Unresolved or partially landed candidates | Re-characterize current routes/components and instantiate exact R8 backend/UI work units before accepting scope |
| Browser-local thresholds, refresh tiers, freshness indicators, and linked filters | Unresolved candidates subject to bounded browser-state and generated-contract rules | Decide per R8 workflow with accessibility, browser, secret-storage, resource, and rollback gates |

Archival does not reject unresolved candidates. It prevents a stale roadmap from competing with the active plan while keeping the ideas retrievable for structured triage.

## Deleted Companions

The archived dashboard documents referred to `ui/DESIGN_SYSTEM_BRIEF.md` and `ui/FEATURE_PARITY_AUDIT.md`. Both were deleted in commit `c7ee7cba9f1ca783eff3e8afba7bc581175940e2`; their last pre-delete blobs were respectively `fc7368543d4b7a9e354668162fe7fcf0b3466589` and `5fe5db23f4ddfa128a7885fe309657c02f4ecab3`.

They were not silently restored for this archive. If an audit proves that unique rationale must be recovered, inspect it first:

```bash
git show c7ee7cba9f1ca783eff3e8afba7bc581175940e2^:ui/DESIGN_SYSTEM_BRIEF.md
git show c7ee7cba9f1ca783eff3e8afba7bc581175940e2^:ui/FEATURE_PARITY_AUDIT.md
```

Any recovered material remains historical until it is classified and linked from a current authority.

## Inbound Reference Audit

Before the move, the refactor plan had no repository inbound reference. The dashboard collection had one collection-external literal reference in `docs/prompt-cache-scope-and-kiro-rs-tool-parity.md`; it is updated to this archive index. Internal dashboard links remain relative within the preserved subdirectory.

The missing companion literals were replaced with this index rather than reviving deleted files. A post-move search must find old UI plan paths only in provenance or recovery text.

## Recovery And Reversal

To restore the former paths while preserving current archived content and Git history:

```bash
mkdir -p ui/docs
git mv docs/archive/ui-planning-2026-06-to-07/REFACTOR_PLAN.md ui/REFACTOR_PLAN.md
git mv docs/archive/ui-planning-2026-06-to-07/dashboard-enhancement ui/docs/dashboard-enhancement
```

After moving them back, exact pre-archive content can be recovered from the planning baseline:

```bash
git restore --source=e9479df71ee0044cfa0da8acbf69d98c2259a66f -- ui/REFACTOR_PLAN.md ui/docs/dashboard-enhancement
```

Do not run restore over a new document that has reused either old path. Reversing the filesystem move does not restore active authority; re-audit disposition, inbound references, current code, and R8 decisions first.
