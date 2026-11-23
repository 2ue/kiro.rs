---
name: kiro-route-profit-audit
description: Per-route profit audit for a claude-pool (c1) upstream fronted by sub2api. Use when the user asks how much profit a pool made, whether a route/cache policy is worth keeping, how to exclude admin or internal traffic from revenue, or to pick which dfcache routes to keep/disable based on real margin rather than hit rate. Read-only; outputs a ranked route table and keep/disable recommendation.
---

# Kiro Route Profit Audit

Compute real profit for a claude-pool instance by joining two systems that never share a request id:

- **Cost side**: claude-pool `usage_records.original_cost_usd` is the official-list-price charge for the tokens actually sent upstream. The supplier bills **0.3 × original**. `estimated_cost_usd` is the cache-shaped amount reported downstream (the "list price" sub2api sees).
- **Revenue side**: sub2api `usage_logs.actual_cost` is what the end user was actually debited (`total_cost × rate_multiplier`). `total_cost` is sub2api's list price and should match claude-pool `estimated_cost_usd` within ~2%.

Profit = `Σ actual_cost (non-admin users)` − `0.3 × Σ original_cost_usd (attributed to those users)`.

Read `references/cost-model.md` before running anything. Everything here is read-only SQL; never write to either database.

## Why hit rate is the wrong filter

Route names on c1 carry author annotations like `差额+103% 命中81%`. Both numbers are from small calibration runs and drift under real traffic. More importantly, **hit rate and margin are not correlated**: on 2026-11-20 the route with 96% hit rate lost money while a 58% hit-rate route had 66% gross margin. Rank routes by realised margin, use hit rate only as a health signal.

Break-even for a route is `差额 ≥ (0.3 / rate_multiplier) − 1`. At the common 0.21 user multiplier that is **43%**; 25% gross margin needs ~90%.

## Inputs

- Target host (SSH alias) and the two Postgres containers: claude-pool `<alias>-postgres` (user/db `app`) and sub2api `<name>-postgres` (user/db `sub2api`).
- Window: default "today, Asia/Shanghai". Always state the window and the capture time in the report; the two sides must be queried within the same minute.
- The pool's container alias as it appears in sub2api `accounts.credentials->>'base_url'` (e.g. `http://claude-pool-1:8990`).

## Workflow

1. **Reconcile totals first.** Σ sub2api `total_cost` vs Σ c1 `estimated_cost_usd` for the same window and route set. If they differ by more than ~3%, stop and find out why (missing routes, deleted accounts, clock skew) before trusting per-route numbers.
2. **Attribute routes on the c1 side**, not from sub2api `accounts.base_url`. sub2api account `base_url` can be edited mid-day; c1 `usage_records.endpoint` is immutable per request.
3. **Carve out admin and internal traffic.**
   - Admin: sub2api `users.role = 'admin'`. Their cost share = (admin `total_cost` on that route / total `total_cost` on that route) × c1 `original_cost_usd` of the route. Request ids do not match across systems, so attribute by list-price share, never by count.
   - Internal: c1 routes with zero sub2api revenue (ops sessions, `tune-*`, `cctune` test routes). Report them as a separate line; do not fold into user profit.
4. **Compute per-route**: requests, 差额% = (est − orig)/orig, actual cost = 0.3 × orig, revenue = user `actual_cost`, profit, gross margin, reported-usage hit rate.
5. **Recommend**: disable routes with negative margin; flag routes under ~5% margin that carry a large request share (they soak up sessions via sub2api sticky routing and drag the pool); keep the rest, noting which still have small samples.
6. Present one table sorted by margin, one totals block (overall / users-only / internal), and the recommendation. Say explicitly that nothing was changed.

## Script

`scripts/route_profit.sh <ssh-alias> <pool-alias> <sub2api-pg-container> [cost-ratio] [window-sql]` prints every number above for "today Asia/Shanghai" by default. Pipe through `LC_ALL=C cut -c1-220` on macOS to avoid locale errors. The header of the script documents the attribution model and the exact SQL per column.

Revenue per route is derived as `c1 est(route) × user_share(route) × m_user`, where `m_user` is the observed users-only `Σactual/Σtotal` across the pool. This sidesteps sub2api's mutable `accounts.base_url`: when an account is re-pointed mid-window its historical rows would otherwise be credited to the new route. Routes where sub2api list price and c1 `est` disagree by more than 15% are tagged `attrib?` so you know an account was renamed; totals are unaffected because they are summed from the c1 side.

Example (2026-11-20 10:20 CST, claude-pool-1 via sub2api-b): users-only profit +221 on 1144 revenue (19.4%); `r8-5m-d103-h86` at −2% margin despite 96% hit rate; `r1-d134-h60` at 3% margin carrying 35% of all requests; six routes above 25% margin.

## Pitfalls seen in practice

- `round(double precision, int)` does not exist in Postgres; cast to `numeric` first.
- Postgres refuses `GROUP BY 1` when the select list is a single concatenated string containing an aggregate; group by the underlying columns or wrap in a subquery.
- Hit rate must use `data->'accountBilling'->'reportedUsage'` tokens to match the author's annotation convention; `rawUsage` gives a lower number (what the upstream actually saw).
- sub2api sticky session hashing pins a conversation to one account. One hot automation session can put 70% of a group's traffic on a single route; per-route request counts therefore say little about route quality.
- The claude-pool DB may have been (re)created mid-window. Check `min(created_at)` before claiming a full-day figure.
