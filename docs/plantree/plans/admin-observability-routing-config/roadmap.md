# Roadmap

## Done

- Confirmed current usage records UI sends only generic `q` plus exact `model`, `endpoint`, `conversationId`, route, status, source, stream, and cache-read filters.
- Confirmed PgSQL generic `q` search includes `data::text ILIKE '%q%'`, which can scan large JSON payloads even when the user searches a request id.
- Confirmed Redis paged query scans cached records in batches and falls back to PgSQL after a scan limit, so selective filters can be slow despite pagination.
- Confirmed Redis model filtering only compares `record.model`, while memory and PgSQL also check upstream/external outbound model fields.
- Confirmed local credential model eligibility is currently only the Opus/free heuristic in `credential_is_usable_for_model`.
- Confirmed external pool selection currently filters enabled/auto-disabled/body-mode/capacity/cooldown, but not supported models.
- Confirmed `KiroProvider::list_available_models` can list upstream models, but it chooses any enabled credential; a per-credential sync entrypoint is needed.
- Confirmed API 400 handling only retries `profile_arn_bad_request`; prompt/tool/body logic bad requests fail immediately.
- Confirmed request/body backend is partially modularized, but config UI still mixes switches and subordinate settings across large sections.
- Implemented exact `requestId` query handling in admin DTOs, usage recorder, Redis cache, and PgSQL.
- Removed default PgSQL generic search over `data::text`; explicit lightweight JSON fields remain searchable.
- Added PgSQL expression indexes for `upstreamModel`, `externalOutboundModel`, and `externalPoolId`.
- Aligned model filtering across memory, Redis, and PgSQL for reported model, upstream model, and external outbound model.
- Added normalized supported-model lists to local credentials and external pools. Empty list means unrestricted.
- Wired supported-model lists into local credential eligibility and external pool selection before dispatch.
- Added admin APIs to manually set supported models and sync from a chosen local credential.
- Updated `ui` with local credential and external pool supported-model controls.
- Added opt-in runtime config for selected prompt/protocol 400 retry classes with an untried credential and bounded max attempts.
- Improved external pool form grouping so dispatch eligibility, body processing, model processing, usage projection, and error handling are visually separate.
- Completed local regression: Rust tests, frontend builds, and fake upstream smoke/chaos.
- Added `/api/admin/usage-dashboard/accounts` as a bounded, read-only, rollup-first full-account aggregate with zero-fill, coverage metadata, pagination, and explicit degraded status.
- Updated both `ui` and `admin-ui` account statistics views to use the full-account paginated aggregate instead of Top 10 plus per-account usage requests.
- Implemented overview redesign with four independent tabs (summary, local accounts, external pools, rankings), top-positioned realtime load, time-range and drill-down controls, local account metrics, external pool financials, usage-detail deep links, deterministic demo seed data, and split overview APIs (`summary`, `series`, `local`, `external`, `rankings`). Rankings now cover models, accounts, request API keys, paths, and errors with request/error percentages. Local demo evidence is recorded in [overview redesign evidence](topics/overview-redesign-local-external-split-20261002.md#10-本地实现与验证证据2026-10-02).
- Corrected usage cleanup semantics and UI: default cleanup now removes usage detail rows while preserving historical rollups, an explicit unchecked option clears all history including summaries, row-count limits replace batch-count configuration, optional physical detail deletion is explicit and unchecked by default, split overview cache invalidation is complete, and evidence is recorded in [Usage 清理语义修正](topics/overview-redesign-local-external-split-20261002.md#11-usage-清理语义修正2026-10-02).
- Completed the follow-up display pass: account cards now use full-number concurrency/RPM presentation with merged account health counts; credit details separate historical consumption from current spendable balance and apply the per-account remaining-cost estimate; local overview cards put consumed credits and estimated cost first, fold original cost into the estimated-cost card, and add summary/per-account credit conversion rates with three decimal places. Local overview rows show only accumulated usage, costs, neutral estimated-vs-original cost differences, conversion rates, remaining credits, estimated remaining cost and error rate; duplicated account-count/runtime summaries were removed. External-pool overview now follows the main-branch cost-split table fields instead of showing routing/configuration detail, while duplicate section summaries were removed. Demo seed writes deterministic account balance snapshots and includes disabled accounts with historical usage. Evidence: [账号积分与外部池展示补全](topics/overview-redesign-local-external-split-20261002.md#12-账号积分与外部池展示补全2026-10-03) and [总览 Tab 信息去重](topics/overview-redesign-local-external-split-20261002.md#13-总览-tab-信息去重2026-10-03).
- Account-list filters now use a model-directory dropdown and a region dropdown built from the full credential catalog. `Auto` is excluded from the model-filter options; region filtering covers explicit and effective auth/API regions. Local verification recorded in [账号筛选模型与区域](topics/overview-redesign-local-external-split-20261002.md#14-账号筛选模型与区域2026-10-03).
- Subscription filtering and display now follow Kiro's official names: `Kiro Free`, `Kiro Students`, `Kiro Pro`, `Kiro Pro+`, `Kiro Pro Max`, and `Kiro Power`. Historical titles and legacy trial labels normalize to the canonical filter set; substring matching no longer makes `Pro` include `Pro+` or `Pro Max`. The change is scoped to admin display/filtering/validation and does not affect request dispatch. Evidence: [订阅名称归一化](topics/overview-redesign-local-external-split-20261002.md#15-订阅名称归一化2026-10-03).
- 对齐远端最新发布 `v0.0.183` 的外部池成本口径：总览卡片以“外部池可计费”作为最终管理指标，表格保留“展示计费”和“补偿后计费”作为成本拆分字段，并补充两者的中间口径/最终上报口径说明；零值回退改为与发布版一致的空值回退。证据见 [外部池计费字段对齐](topics/overview-redesign-local-external-split-20261002.md#16-外部池计费字段对齐2026-10-03).

## In Progress

- None for this scope.

## Next

- Optional low-volume real upstream smoke for model sync and supported-model dispatch, only when explicitly requested.
- Optional UI follow-up: add a dedicated request-id field or explanatory placeholder in usage search.
- Dashboard follow-up: run an isolated PostgreSQL dataset regression with 20+ accounts, zero-usage accounts, partial-hour boundaries, and timeout/degradation injection; design and implementation are documented in [Dashboard 运营统计设计](topics/dashboard-statistics-design.md).

## Deferred

- Full plugin ABI for body/model/usage/retry processing.
- Heavy full-text search across arbitrary usage `data` JSON by default. If needed, implement as an explicit deep-search mode.
- Replacing existing legacy admin UIs with only the new React surface.
- A separate diagnostics page for writer health, sticky fallback, usage-source distribution, and status distribution; those APIs remain available.
