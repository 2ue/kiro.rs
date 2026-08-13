# Credential List Data Plane Optimization Design

## Background

The current credential list path mixes several very different data concerns into one response:

- Credential base configuration and identity fields.
- Runtime scheduler state such as cooldown, rate limit, in-flight leases, health score, and recent selection counts.
- Account information snapshots such as subscription, usage, remaining quota, and credit fields.
- Usage and cost summary fields.
- Bulk-operation target discovery.

This makes the list endpoint expensive and fragile. A UI action that only needs a visible list, a count, or a set of target credentials can be forced to wait for PgSQL, Redis, usage aggregation, scheduler diagnostics, and full response serialization.

The optimized design separates credential list data by source, refresh rate, and cost.

## Design Goal

The base credential list must be fast and must not be blocked by Redis, usage aggregation, or account quota reads.

The target shape is:

```text
Base list      -> first render and normal paging/filtering
Runtime state  -> current page hydration, high-frequency refresh
Account info   -> current page hydration, low-frequency or manual refresh
Usage summary  -> demand-loaded usage/cost fields
Bulk operation -> server-side execution by ids or filters
```

## Data Planes

### 1. Base Plane

The base list is the only data needed for initial rendering.

Endpoint:

```text
GET /api/admin/credentials?page=1&limit=30&q=&status=&authMethod=&subscription=&proxyResourceId=&sortBy=&sortOrder=
```

Response:

```ts
type CredentialListResponse = {
  page: number
  limit: number
  total: number
  filteredTotal: number
  totalPages: number
  items: CredentialListItem[]
}
```

Base item fields:

```ts
type CredentialListItem = {
  id: number
  createdAt?: string
  updatedAt?: string

  priority: number
  disabled: boolean
  disabledReason?: string

  authMethod?: string
  provider?: string
  email?: string
  subscriptionTitle?: string

  region?: string
  authRegion?: string
  apiRegion?: string
  effectiveAuthRegion: string
  effectiveApiRegion: string
  hasProfileArn: boolean

  refreshTokenHash?: string
  apiKeyHash?: string
  maskedApiKey?: string

  hasProxy: boolean
  proxyResourceId?: number
  proxyResourceName?: string
  effectiveProxySource: string

  maxConcurrentRequestsOverride?: number
  warmupRemaining: number
}
```

The base list must not include:

- `accountInfo`
- `estimatedCostUsd`
- `pricedRequests`
- `unpricedRequests`
- `successCount`
- `lastUsedAt`
- `cooledDown`
- `cooldownRemainingSecs`
- `rateLimited`
- `inFlightRequests`
- `schedulerScore`
- `schedulerSelectionPressure`
- `recentErrorRate`
- `latencyEwmaMs`
- `lastErrorKind`
- `lastErrorReason`
- `probationRemainingSecs`

Backend constraints:

- Do not call the full `token_manager.snapshot()` from the base list.
- Do not call `refresh_stats_from_postgres()`.
- Do not call `refresh_scheduler_state_from_redis_best_effort()`.
- Do not call `usage_recorder.credential_cost_summary()`.
- Do not call `load_credential_account_info()`.
- Do not construct a full `CredentialStatusItem` collection and then paginate it.

The implementation should use a lightweight token manager base snapshot that reads in-memory credential configuration and returns only base data needed by the list.

### 2. Summary Plane

Credential counts and global capacity are not the same thing as the page list.

Endpoint:

```text
GET /api/admin/credentials/summary
```

Response:

```ts
type CredentialSummary = {
  total: number
  available: number
  disabled: number
  currentId: number | null

  globalInFlightRequests: number
  queuedRequests: number
  globalMaxConcurrentRequests: number
  maxQueuedRequests: number

  updatedAt: string
  runtimeFresh: boolean
}
```

Rules:

- `total`, `available`, and `disabled` come from in-memory base state.
- Global capacity may use a cached/background-refreshed runtime value.
- Redis slowness must not block this endpoint; stale values can be returned with `runtimeFresh: false`.

Frontend follow-through:

- Main credential pages use the base list endpoint for first render and page changes.
- Legacy frontend `getCredentials()` callers that only need labels, hashes, or proxy bindings now aggregate the lightweight list endpoint instead of calling the heavyweight `/credentials` status endpoint.
- The old heavyweight endpoint can remain for external/manual compatibility, but current Admin UI list, usage-label, proxy-binding, and import duplicate-check flows should not depend on it.

### 3. Runtime Plane

Runtime state changes frequently and should hydrate only the current page.

Endpoint:

```text
GET /api/admin/credentials/runtime?ids=1,2,3,4
```

Response:

```ts
type CredentialRuntimeResponse = {
  items: CredentialRuntimeItem[]
  updatedAt: string
  fresh: boolean
}
```

Runtime item fields:

```ts
type CredentialRuntimeItem = {
  id: number
  isCurrent: boolean

  failureCount: number
  refreshFailureCount: number
  successCount: number
  lastUsedAt?: string

  cooledDown: boolean
  cooldownRemainingSecs: number
  cooldownReason?: string

  rateLimited: boolean
  rateLimitRemainingSecs: number

  inFlightRequests: number
  oldestInFlightAgeSecs: number
  newestInFlightIdleSecs: number

  transientFailureStreak: number
  recentErrorRate: number
  latencyEwmaMs?: number
  lastErrorKind?: string
  lastErrorReason?: string
  lastErrorAtMs?: number

  inProbation: boolean
  probationRemainingSecs: number

  schedulerSelectionCount: number
  recentSchedulerSelectionCount10s: number
  recentSchedulerSelectionCount60s: number
  recentSchedulerSelectionCount5m: number
  schedulerSelectionPressure: number
  schedulerScore: number
}
```

Rules:

- Runtime hydration refreshes every 3-5 seconds for current page IDs only.
- Redis scheduler state reads must be scoped to requested IDs.
- A runtime read failure must not invalidate or block the base list.
- Responses are merged by credential ID on the frontend.

### 4. Account Plane

Account quota and subscription details change slowly and should not block the list.

Endpoint:

```text
GET /api/admin/credentials/account-info?ids=1,2,3,4
```

Response item:

```ts
type CredentialAccountInfoItem = {
  id: number
  subscriptionTitle?: string
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  creditLimit: number
  creditRemaining: number
  creditBase: number
  creditBonus: number
  nextResetAt?: string
  checkedAt: string
}
```

Rules:

- Load by requested IDs, not by full-table scan.
- Refresh at a low frequency, for example 30-60 seconds.
- Manual "query current page info" can force refresh.

### 5. Usage Plane

Usage/cost summary belongs to the usage view and should be demand-loaded in the list.

Endpoint:

```text
GET /api/admin/credentials/usage-summary?ids=1,2,3,4
```

Response item:

```ts
type CredentialUsageSummaryItem = {
  id: number
  estimatedCostUsd: number
  pricedRequests: number
  unpricedRequests: number
}
```

Rules:

- Do not load by default for every list view.
- Load only when cost fields are visible, cost sorting is requested, or usage views need it.
- Query by requested IDs.

## Sorting And Filtering

Sorting/filtering should be split by the plane that owns the data.

Base-plane fields:

- `id`
- `createdAt`
- `updatedAt`
- `priority`
- `disabled`
- `authMethod`
- `provider`
- `email`
- `subscriptionTitle`
- `proxyResourceId`
- `region`

Runtime-plane fields:

- `current`
- `cooldown`
- `rateLimited`
- `inFlightRequests`
- `failureCount`
- `successCount`
- `lastUsedAt`
- `schedulerScore`
- `recentErrorRate`
- `latencyEwmaMs`

Account/usage-plane fields:

- `usagePercentage`
- `remaining`
- `estimatedCostUsd`
- `pricedRequests`
- `unpricedRequests`

Runtime/account/usage sorting can use read-model cache values and should report freshness:

```ts
type SortFreshness = {
  sortDataFresh: boolean
  sortDataUpdatedAt?: string
}
```

The key safety rule is:

```text
Display data may be stale; write operations must re-check server-side state.
```

## Internal Read Model

Add an admin read model that is optimized for UI reads:

```rust
struct AdminCredentialReadModel {
    base: HashMap<u64, CredentialListItem>,
    runtime: HashMap<u64, CredentialRuntimeItem>,
    account: HashMap<u64, CredentialAccountInfoItem>,
    usage: HashMap<u64, CredentialUsageSummaryItem>,
    summary: CredentialSummary,
    revision: u64,
}
```

Update strategy:

- `base`: updated synchronously after credential add/edit/delete/disable/proxy changes.
- `runtime`: refreshed in the background or hydrated by current-page ID reads.
- `account`: loaded at startup, updated after account-info refreshes, optionally refreshed at low frequency.
- `usage`: updated from usage writes or refreshed periodically; not required for first render.

HTTP requests should read this model and should not block on external storage for the base list.

## Frontend Data Flow

Use separate queries and merge by ID:

```ts
const base = useCredentialList(query)
const ids = base.items.map(item => item.id)

const summary = useCredentialSummary()
const runtime = useCredentialRuntime(ids)
const accountInfo = useCredentialAccountInfo(ids, { enabled: showAccountFields })
const usage = useCredentialUsageSummary(ids, { enabled: showCostFields })

const rows = base.items.map(item => ({
  ...item,
  runtime: runtime.byId[item.id],
  accountInfo: accountInfo.byId[item.id],
  usage: usage.byId[item.id],
}))
```

UI behavior:

- Render base list immediately.
- Hydrate runtime/account/usage fields independently.
- Keep stale per-ID values until fresh data arrives.
- Do not block page transitions on hydration queries.
- Do not let a late response from a previous page overwrite unrelated rows; merge by ID only.

## Bulk Operations

Bulk operations should be server-side and should not require the frontend to fetch all credentials.

Suggested endpoint:

```text
POST /api/admin/credentials/bulk-action
```

Request:

```ts
type BulkCredentialActionRequest = {
  action:
    | 'delete'
    | 'deleteDisabled'
    | 'resetFailure'
    | 'refreshToken'
    | 'setDisabled'
    | 'setPriority'
    | 'setProxy'
    | 'setConcurrency'

  target:
    | { type: 'ids', ids: number[] }
    | { type: 'filter', query: CredentialListQuery }

  params?: Record<string, unknown>
}
```

Response:

```ts
type BulkCredentialActionResponse = {
  totalMatched: number
  totalAttempted: number
  success: number
  failed: number
  skipped: number
  errors: Array<{
    id: number
    message: string
  }>
}
```

Rules:

- Server re-evaluates filters at execution time.
- Server re-checks current credential state before destructive operations.
- Frontend never fetches the full credential list just to find operation targets.

## Safety Constraints

- Base state is authoritative for list membership and must be updated synchronously.
- Runtime/account/usage state is display enhancement and can be stale.
- Every write operation validates current server-side state.
- Every bulk operation reports attempted, success, failed, skipped, and per-ID errors.
- Responses carry `updatedAt`, `fresh`, or `revision` metadata when data can be stale.
- Redis/PgSQL failures must not block the base list.
- Deleted credentials must be removed from all read-model maps.
- Added or edited credentials must update the base read model and invalidate affected frontend queries.
- Stale sort data must be surfaced explicitly when sorting depends on runtime/account/usage fields.

## Testing Requirements

- With 1,000 credentials, the base list must not access PgSQL or Redis.
- Redis delays must not block the base list or summary count fields.
- Large usage tables must not affect the base list.
- Runtime hydration must request only current page IDs.
- Account and usage hydration must query by IDs, not full tables.
- Deleted credentials must disappear from base, runtime, account, and usage maps.
- Bulk actions by IDs must re-check server state per ID.
- Bulk actions by filter must re-evaluate the filter server-side.
- Frontend hydration results must merge by ID and must not corrupt rows after page changes.

Suggested performance budgets:

```text
Base list, 1,000 credentials: p95 < 50ms
Base list, 5,000 credentials: p95 < 150ms
Summary: p95 < 20ms
Runtime hydration, 30 IDs: p95 < 100ms
Account hydration, 30 IDs: p95 < 100ms
Usage hydration, 30 IDs: p95 < 100ms
```

## Final Architecture

```text
Credential Page
├─ Summary
│  └─ total / available / disabled / current / global capacity
├─ Base List
│  └─ current page base credential data
├─ Runtime Hydration
│  └─ cooldown / rate limit / in-flight / failure / scheduler health
├─ Account Hydration
│  └─ subscription / usage / remaining quota
├─ Usage Hydration
│  └─ cost / priced requests / unpriced requests
└─ Bulk Operations
   └─ server-side execution by IDs or filters
```

The credential list should list credentials. It should not diagnose the scheduler, query account quotas, aggregate usage cost, and discover bulk-operation targets in the same request.

## Implemented Changes

Backend endpoints added:

- `GET /api/admin/credentials-list`
- `GET /api/admin/credentials/list`
- `GET /api/admin/credentials/summary`
- `GET /api/admin/credentials/runtime?ids=1,2,3`
- `GET /api/admin/credentials/account-info?ids=1,2,3`
- `GET /api/admin/credentials/usage-summary?ids=1,2,3`
- `DELETE /api/admin/credentials/disabled`

Backend data-plane changes:

- `MultiTokenManager::base_snapshot()` now returns credential base/configuration data without PgSQL account-info reads, usage aggregation, or Redis runtime sync.
- `MultiTokenManager::runtime_snapshot_for_ids()` hydrates only requested credential IDs and returns `fresh=false` if Redis runtime state could not be synchronized.
- Account info hydration uses `PostgresStore::load_credential_account_info_for_ids()`.
- Usage hydration uses `UsageRecorder::credential_cost_summary_for_ids()`.
- `get_credentials_runtime()` returns an empty result for empty `ids`, preventing accidental full-runtime responses.
- `delete_disabled_credentials()` executes on the server using current base state and reports matched/attempted/success/failed/errors.

Frontend changes:

- The credential dashboard now renders from the base list and summary first.
- Runtime, account-info, and usage/cost data hydrate only the current page IDs.
- The "clear disabled credentials" confirmation uses the summary disabled count and no longer fetches the full credential list before showing `confirm()`.
- Main list sorting is limited to base fields: default, ID, created time, updated time, and priority.
- Runtime-owned filters were removed from the main base-list filter UI to avoid pretending that base-list filtering can precisely filter hydrated runtime state.

## Verification Run

Commands run:

```bash
cargo fmt -- --check
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo check
pnpm --dir ui build
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test credential_admin_list_options_include_account_info_snapshot
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test credential_default_sort_keeps_enabled_then_newest_created
CC=/usr/bin/clang RUSTFLAGS='-C linker=/usr/bin/clang' cargo test credential_custom_sort_runs_before_pagination_order
```

Results:

- Rust formatting check passed.
- Rust `cargo check` passed.
- Admin UI TypeScript/Vite production build passed.
- The three targeted credential admin list tests passed.
