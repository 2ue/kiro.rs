# Account Scheduler Surface Analysis

Role: Product-surface and backend-boundary analysis

Status: Analysis only; no production behavior changed

As of: 2026-08-29

Related:
- [Final target plan](final-target-plan.md)
- [Claude Code base URL + API key passthrough analysis](claude-code-base-url-apikey-passthrough-analysis.md)
- [Plan root](../README.md)

## 1. Goal

The final product is a generic Claude Code-compatible account scheduling system.

The product should:

- accept multiple Claude Code-compatible upstream accounts;
- choose an account by route policy, priority, model capability, health, cooldown, concurrency and RPM;
- keep request processing minimal;
- perform only the model/route mapping that affects scheduling or upstream compatibility;
- leave the actual response content as transparent passthrough;
- shape and write usage by path;
- absorb as many account-side failures internally as possible before surfacing a client failure.

The product should not expose or depend on Kiro-specific product language or Kiro-specific account-manager workflows.

## 2. What Must Stay

These are core scheduling capabilities and should remain part of the product surface or backend implementation:

- account enable/disable;
- account priority;
- per-account concurrency limits;
- per-account RPM / cooldown / probation;
- model support, model filtering and model mapping;
- path-based routing and path-based usage shaping;
- scheduler queueing and retry/failover policy;
- sticky / affinity behavior when it improves stability;
- account health and liveness checks;
- usage capture, raw/effective/reported usage layers;
- response passthrough;
- stream/non-stream compatibility handling;
- observability, audit, scheduler diagnostics, and runtime config for the above.

If a feature affects one of those behaviors, it should be analyzed as a scheduling feature first, not dismissed as a legacy credential feature.

## 3. What Should Go Away

These are legacy product concepts that do not belong to the final Claude Code account scheduler surface:

- KAM JSON import wording;
- `credentials` as the product noun on user-facing pages;
- `external-pool` as the public product noun;
- Kiro-specific account-manager flows;
- subscription/balance/overage UI if they are only historical account-manager concepts;
- AWS/profile ARN/region/auth-region/api-region UI if they are only part of the old account-manager import story;
- refresh-token-centric or IdC/external-IdP-centric UI if the final product is only Claude Code-compatible base URL + API key accounts;
- any Kiro-specific request-body conversion that exists only to support the old upstream product;
- response rewriting beyond usage shaping;
- Kiro-specific CLI / validation / import/export concepts.

The key distinction is:

- keep anything that influences scheduling or protocol compatibility;
- delete anything that only exists because the old system was a Kiro-oriented account manager.

## 4. Frontend: File-by-File Analysis

### 4.1 `ui/src/types/ui.ts`

Current role:

- defines navigation domains, page keys and page metadata;
- still carries a `validation` page;
- still uses `credentials` as a page key and some legacy vocabulary internally.

What to change:

- keep the `accounts`, `usage`, `runtime`, `models`, `security`, `audit`, `account-risk`, `proxies` style pages only if they still represent real scheduler capabilities;
- remove or rename any visible page labels that still suggest `credentials` or Kiro-specific account-manager vocabulary;
- decide `validation` by function:
  - if it is a real account-health / liveness / model-compatibility page, keep it but rename its visible text to account language only;
  - if it is mostly KAM / credentials import and old account-manager validation, delete it from nav.

Impact:

- sidebar layout changes;
- page metadata changes;
- route guards and redirect behavior need to match the chosen surface.

### 4.2 `ui/src/app/router.tsx`

Current role:

- routes `/credentials` and `/external-pools` to `/accounts`;
- still exposes `/validation` as a live page;
- still lazy-loads the validation page chunk.

What to change:

- keep redirects that preserve access to the new account page where needed;
- remove `/validation` route if the page is being retired;
- otherwise keep `/validation` but ensure it is rewritten to pure account semantics and does not mention KAM / credentials / old import formats;
- make sure the router matches the final navigation tree from `ui/src/types/ui.ts`.

Impact:

- direct bookmarks to old paths may need redirects;
- tests that assert route maps will need updates;
- chunk splits may change if `validation` is removed.

### 4.3 `ui/src/features/accounts/accounts-page.tsx`

Current role:

- this should become the main account-management surface;
- it is the page that should remain if the product keeps scheduling controls.

What to keep:

- account list;
- status;
- enable/disable;
- priority;
- concurrency;
- RPM;
- model support / model mapping;
- cooldown / probation / health controls;
- usage summary and scheduler statistics.

What to remove from the user surface if they are only old account-manager baggage:

- refresh-token flow;
- balance / overage;
- subscription wording;
- AWS region / profile ARN / auth-region / api-region wording;
- KAM-specific import/export wording;
- any control that is only there for an old Kiro import pipeline.

Impact:

- this page will become the canonical account scheduler UI;
- many modal dialogs and cards must change together, not one label at a time.

### 4.4 `ui/src/features/credentials/credential-card.tsx`

Current role:

- shows a lot of account state and a lot of account control knobs;
- currently mixes real scheduler controls with old credential-manager controls.

What to keep:

- enable/disable;
- priority;
- concurrency;
- RPM;
- supported models;
- cooldown / rate-limit state;
- test / liveness / current status;
- in-flight clearing if it is still part of scheduler recovery;
- scheduler-facing metrics such as selection pressure, probation, recent errors.

What to delete or hide from the visible surface:

- auth-method switching that only exists for old account-manager import flows;
- refresh-token / token refresh controls;
- subscription / balance / overage controls;
- AWS region / profile ARN controls;
- any UI that is only there because the old product managed Kiro-style credentials.

Impact:

- this is the main place where “credential” wording leaks into the product;
- deleting controls here may also require backend route cleanup if the UI no longer calls them.

### 4.5 `ui/src/features/credentials/credential-dialogs.tsx`

Current role:

- contains add, batch-import, batch-edit, test and export dialogs;
- currently carries KAM import and many old account-manager concepts.

What to keep:

- account add/edit dialogs if they map to real scheduler fields;
- account test dialog;
- batch update if it is used to change real scheduling fields;
- export only if there is still a real operational need.

What to delete or rewrite:

- KAM import dialog;
- `KAM JSON` wording;
- `credentials` import wording;
- subscription / balance / overage import behavior if it is not required by the final account scheduler;
- auth-provider-specific import code that does not belong to a Claude Code-compatible base URL + API key account.

Impact:

- imported file formats and examples will change;
- integration tests for import/export must be rewritten;
- if import is still needed, define a new minimal account schema rather than keeping KAM compatibility.

### 4.6 `ui/src/features/validation/validation-page.tsx`

Current role:

- this page is the clearest Kiro/credential-era artifact in the new UI;
- it still talks about KAM JSON, credentials arrays, and old validation concepts.

What to keep:

- only if the page is repurposed as a real account-health / model-compatibility / liveness check for scheduler decisions;
- if kept, it should validate the final account model, not old KAM or credentials semantics.

What to delete:

- `KAM JSON` placeholder and related copy;
- any `credentials` phrasing in the import UX;
- old import formats that are only meaningful to the old account-manager product;
- subscription-only validation if the final scheduler no longer uses that concept.

What to rewrite:

- make the page speak in account language only;
- keep liveness / usage / model checks only if they directly influence scheduling or operator decisions;
- if the page does not do anything beyond legacy import validation, remove it from navigation and route it to `/accounts`.

Impact:

- this is the highest-signal place to remove old product vocabulary;
- if deleted, router and sidebar must be updated together;
- if kept, the file becomes an account-health page, not a credential-validation page.

### 4.7 `ui/src/features/usage/*`

Current role:

- usage shaping, usage summaries, top dimensions and route breakdowns;
- this is core and must stay.

What to keep:

- path-based usage shaping;
- raw/effective/reported usage distinction;
- account-attempt breakdown;
- model / route / billing dimensions that matter to the scheduler.

What to rename:

- visible `credential` wording in labels should become `account`;
- any UI text that exposes the old product vocabulary should be replaced.

What should not change:

- the actual usage accounting behavior;
- the path shaping math;
- the historical attempt records.

### 4.8 `ui/src/features/audit/*`

Current role:

- already closer to the target than the old credential UI;
- it maps actions into user-facing account language.

What to keep:

- account management actions;
- model sync actions;
- usage actions;
- config changes that still exist.

What to delete:

- audit labels that still expose old product wording if the underlying action is no longer part of the final surface.

### 4.9 `ui/src/features/security/*` and `ui/src/features/runtime/*`

Current role:

- runtime config and security key management.

What to keep:

- Admin Key / request key management;
- route policy config;
- model mapping / scheduler knobs;
- usage projection knobs.

What to delete:

- any runtime config field that exists only because of the old Kiro / credential-manager product model.

## 5. Backend: File-by-File Analysis

### 5.1 `src/admin/router.rs`

Current role:

- exposes both legacy `/credentials` and newer `/accounts` style routes;
- exposes validation and external-pool aliases;
- is the public admin contract surface.

What to keep:

- routes that support the final scheduler: accounts, usage, runtime config, model config, audit, security, route policy.

What to delete or alias away:

- public `/credentials` product routes if the final product is truly account-first;
- `/external-pools` as a public noun;
- `/credential-validation/*` if it only exists for legacy import/validation;
- any route that exists only because of old Kiro account-manager workflows.

Impact:

- route compatibility with old clients will break if aliases are removed;
- API docs and integration tests must be updated;
- admin UI must switch to the surviving route set at the same time.

### 5.2 `src/admin/handlers.rs`

Current role:

- implements the admin API entrypoints;
- still carries a large amount of credential-era handler naming.

What to keep:

- handler logic for account scheduling, usage, model config, route policy, audit and security.

What to delete or rewrite:

- handlers that only exist for KAM import;
- handlers that only exist for subscription/balance/overage;
- handlers that only exist for old account-manager validation;
- handlers that only exist to support legacy credential product screens.

Impact:

- the service layer will need matching shape changes;
- UI and tests will need matching route / DTO updates.

### 5.3 `src/admin/service.rs`

Current role:

- business logic for admin operations and scheduler-supporting account state.

What to keep:

- account selection state;
- priority / concurrency / RPM / cooldown / health state;
- model support and route policy state;
- usage projection and scheduler diagnostics;
- audit and key management.

What to delete:

- Kiro-oriented account-manager features;
- subscription / overage / balance / refresh-token / import-export logic if they are only legacy account-manager operations.

Impact:

- this is where the final product boundary becomes real;
- if service logic is not pruned, the UI rename alone will be cosmetic only.

### 5.4 `src/storage/postgres.rs`

Current role:

- persistence for scheduler state, account state, runtime config, usage and audit.

What to keep:

- durable account records;
- scheduler state;
- usage records;
- route and model policy data;
- audit history.

What to delete or migrate:

- schema pieces that exist only for old credential/import/subscription/account-manager concepts;
- any columns or tables that are never read by the final scheduler surface.

Impact:

- schema migration may be the largest risky step;
- if hard-renaming `credentials` to `accounts`, you need a data migration and backfill window;
- tests that load fixture rows by old names must be rewritten.

### 5.5 `src/storage/redis_cache.rs`

Current role:

- scheduler cache, counters, runtime state, usage summaries.

What to keep:

- queue state;
- cooldown state;
- usage rollups;
- scheduler stats;
- route policy cache.

What to rename or remove:

- any cache keys that are only labeled with the old credential/Account Runtime language;
- any validation or KAM-specific cache branches.

Impact:

- all cache invalidation tests will need to be re-run;
- key names are part of runtime behavior if external tooling relies on them.

### 5.6 `src/anthropic/*`, `src/external_pool/*`, `src/local_upstream_impl/*`

Current role:

- protocol handling, request admission, body preparation, streaming, usage capture, retry and failover.

What to keep:

- model mapping;
- route policy;
- usage shaping;
- raw passthrough;
- error classification;
- retry / failover policy that internally absorbs transient account errors;
- stream handling and canonical event encoding.

What to delete:

- any request-body pipeline that exists only to emulate the old Kiro upstream;
- any legacy envelope or profile injection;
- any old import or validation logic that has nothing to do with Claude Code-compatible account scheduling;
- any response rewriting except usage shaping.

Impact:

- protocol tests must prove that the remaining code is transparent by default;
- if a body rewrite remains, it must be justified as a specific compatibility policy, not as default behavior.

### 5.7 `src/main.rs` and `src/model/arg.rs`

Current role:

- startup wiring and CLI surface.

What to keep:

- account scheduler service startup;
- admin API startup;
- protocol endpoints;
- diagnostics and health checks.

What to delete or rename:

- CLI commands that only exist to manipulate old credential files;
- startup logs that advertise `credentials` as the product noun;
- any local-diagnostic path that still assumes a Kiro-oriented credential file workflow.

Impact:

- scripts and docs that invoke the old CLI command will need migration;
- startup logs should be aligned with the final product vocabulary so operators do not see stale terms.

## 6. Impact Matrix

| Item | Change Type | Impact if Removed | Notes |
| --- | --- | --- | --- |
| `validation` page | delete or repurpose | navigation, bookmarks, import flow | keep only if it becomes a real account-health page |
| KAM JSON import | delete | batch import UX and parser | this is legacy product baggage |
| `credentials` visible wording | rename | user-facing terminology, audit labels | internal types can stay temporarily |
| `/credentials` public route | delete or alias | API clients, tests, docs | keep only as compatibility if necessary |
| `/external-pools` public route | delete or alias | API clients, tests, docs | final noun should be `accounts` |
| `subscription/balance/overage` UI | delete | account detail dialogs, validation page | keep only if actually used by scheduling |
| `profileArn/region/authRegion/apiRegion` UI | delete | import form and validation UI | not part of the target account model unless explicitly required |
| `refresh-token` flows | delete | import / refresh dialogs | not needed for baseUrl + apiKey accounts |
| body normalization default | disable | request pipeline behavior | keep only as explicit compatibility mode |
| response content rewriting | delete | protocol transparency | only usage shaping should mutate response metadata |

## 7. Test Plan

### 7.1 Frontend tests

- build the UI bundle and verify no visible `KAM JSON` / `credentials` / `external-pool` copy remains in the final user surface;
- route tests:
  - `/credentials` -> `/accounts`;
  - `/external-pools` -> `/accounts`;
  - `validation` behavior matches the final decision;
- snapshot or smoke test the account page to ensure the remaining controls are the scheduler controls we kept;
- verify usage pages still render path shaping, model mapping and usage breakdowns.

### 7.2 Backend contract tests

- account CRUD still works;
- priority, concurrency, RPM, cooldown, model mapping and route policy still work;
- `/accounts` should be the primary admin surface;
- legacy routes should either redirect or be removed according to the final cutover decision;
- raw passthrough should remain body-transparent by default;
- usage projection should only rewrite usage fields, not content.

### 7.3 Claude Code protocol tests

Use real protocol-compatible requests and multiple mock accounts:

- normal high concurrency;
- low concurrency burst;
- sudden burst from low to high;
- sustained large burst;
- burst with repeated account errors;
- account recovery after failure period;
- high-priority account must not monopolize bad traffic forever;
- same-account retry vs switch-account retry;
- ensure once semantic output begins, the scheduler does not hop accounts mid-response;
- ensure usage backfill remains correct for path shaping.

### 7.4 Failure-handling matrix

Classify failures by retry destination:

- same-account retry:
  - transient network failures before response commitment;
  - upstream timeouts before first semantic output;
  - connection resets;
  - transient 5xx that are clearly account-local;
- switch-account retry:
  - repeated 401 / 403 / auth failures;
  - stable rate-limit / cooldown conditions;
  - disabled or unhealthy account;
  - repeated protocol mismatch that indicates the account is not fit for the route;
- do not retry or only minimally retry:
  - malformed request body;
  - local admission failure;
  - request guard rejection;
  - irreversible client-side invalid input.

The goal is to absorb as many account failures internally as possible, but not to keep burning the same bad account indefinitely.

## 8. Recommended Execution Order

1. Finalize the product vocabulary: `accounts`, not `credentials`.
2. Decide whether `validation` survives as a real account-health page or is removed.
3. Rewrite the visible account-management UI to keep only scheduling-relevant knobs.
4. Remove KAM JSON and other old account-manager import vocabulary from the UI.
5. Align admin routes and handlers around `/accounts`.
6. Audit backend storage and runtime config for old credential-only concepts.
7. Reduce body processing to raw passthrough + explicit model mapping + explicit usage shaping.
8. Run build, route, protocol, and load tests against the new target shape.

## 9. Final Target Statement

The final system should be:

- a Claude Code-compatible multi-account scheduler;
- capable of model mapping, route selection and path-based usage shaping;
- transparent by default in request/response handling;
- aggressive in internally absorbing account failures through retry and failover policy;
- free of Kiro-specific product semantics and free of credential-manager-centric UI language.

That is the product boundary this repository should converge to.
