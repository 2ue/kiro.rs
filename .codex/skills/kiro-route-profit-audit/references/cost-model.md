# Cost model: claude-pool behind sub2api

## Money flow for one request

```
upstream (official list price)  ──►  claude-pool            ──►  sub2api              ──►  user
  original_cost_usd  (A)              estimated_cost_usd (B)      total_cost ≈ B           actual_cost = B × m
  supplier bills 0.3 × A              (cache-shaped, "差额")       rate_multiplier m
```

- **A** `usage_records.original_cost_usd`: tokens really sent upstream × official per-token price (`model_pricing`). Also stored as `data->'accountBilling'->>'rawCostUsd'`. `account_cost_usd` equals A when `accounts.cost_multiplier = 1`.
- **Actual supplier cost** = 0.3 × A. This is a commercial term, not stored anywhere; pass it in.
- **B** `usage_records.estimated_cost_usd`: the amount reported downstream after prompt-cache simulation reshapes the usage (`data->'accountBilling'->>'reportedCostUsd'`). `差额% = (B − A) / A`.
- **m** `usage_logs.rate_multiplier`: per-request multiplier sub2api applied. Comes from `groups.rate_multiplier` unless overridden in `user_group_rate_multipliers (user_id, group_id)`. Admin accounts typically run at 0.01.

## Per-route profit

```
profit(route) = Σ actual_cost(users, route) − 0.3 × Σ A(route) × (1 − admin_share(route))
admin_share(route) = Σ total_cost(admin, route) / Σ total_cost(all, route)     -- list-price share
margin = profit / Σ actual_cost(users, route)
break_even_差额 = 0.3 / m − 1          -- 43% at m = 0.21, 25% margin needs ~90%
```

## Tables and columns

### claude-pool (`docker exec <alias>-postgres psql -U app -d app`)

`usage_records`
| column | meaning |
|---|---|
| `created_at` timestamptz | request time (UTC) |
| `endpoint` | `/dfcache/<route>/v1/messages`; strip prefix/suffix to get the route key |
| `status` | `success` / `error` / `client_dropped` / `upstream_timeout`; only `success` carries cost |
| `original_cost_usd` | A |
| `estimated_cost_usd` | B |
| `data->'accountBilling'->'reportedUsage'->>'cacheReadInputTokens'` / `'totalInputTokens'` | hit-rate numerator / denominator (author convention) |
| `data->'rawUsage'` | what upstream actually saw; lower hit rate |
| `data->>'requestApiKeyId'` | hashed inbound key; identical for all sub2api accounts sharing one key, so useless for attribution |
| `deleted_at` | soft delete; filter `IS NULL` |

Route definitions: `runtime_config.config->'routes'` (JSON array, `prefix`, `name`, `enabled`, `cachePolicy.cacheTtl`). The author's calibration numbers live in `name` as `差额+NN% 命中NN%`.

### sub2api (`docker exec <name>-postgres psql -U sub2api -d sub2api`)

| table.column | meaning |
|---|---|
| `usage_logs.account_id` → `accounts.id` | which upstream account served it |
| `accounts.credentials->>'base_url'` | `http://<pool>:8990/dfcache/<route>`; **mutable**, do not trust for historical attribution |
| `usage_logs.total_cost` | list price shown to user (≈ B) |
| `usage_logs.actual_cost` | real debit (= total_cost × rate_multiplier) |
| `usage_logs.rate_multiplier`, `account_rate_multiplier` | multipliers applied |
| `usage_logs.user_id` → `users.role` | `admin` / `user` |
| `usage_logs.group_id` → `groups` | pricing group (e.g. `cursor70缓` id 250) |
| `account_groups (account_id, group_id, priority)` | account membership; sticky-session scheduling happens within a group |
| `ops_error_logs` | per-request failures with `status_code` (499 = client gave up) |

## Known traps

- Window conversion: Beijing day = `created_at >= date_trunc('day', now() AT TIME ZONE 'Asia/Shanghai') AT TIME ZONE 'Asia/Shanghai'`. Both DBs store timestamptz in UTC.
- sub2api and c1 request counts differ by a few percent (retries, failed requests, simulated cache entries). Compare money, not counts.
- Two sub2api accounts can point at the same c1 route (e.g. a duplicate named `-2`). Group by route key, not account.
- If a c1 route shows revenue 0 but cost > 0, it is being called outside sub2api (direct key use, ops tooling). On 2026-11-20 `tune-ss7-wr-5m` was the operator's own Claude Code session.
- Pool instances migrated on 2026-11-18 have no `usage_records` before UTC 17:17 that day; earlier traffic lives in the old 87 host.
