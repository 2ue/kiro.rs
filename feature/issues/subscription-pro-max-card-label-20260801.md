# Subscription Pro Max card label - 2026-08-01

Status: `root-cause-confirmed / ui-and-backend-fix-implemented / focused-tests-passed / scoped-release-gate-passed / browser-pending`

Severity: P1. Account cards can display a `Pro Max` subscription as `Pro`, which misleads operators and also makes subscription filtering and tier-change grouping ambiguous.

Last observed: 2026-08-01 Asia/Shanghai

## 范围与结论

The issue is not caused by screenshot masking or image cropping. The subscription title reaches the UI as a normal string, but both UI and backend classifiers lacked an explicit `Pro Max` branch:

- UI `subscriptionBadgeMeta()` checked generic `pro` after `Pro+`, so `Kiro Pro Max` fell through to `Pro`.
- Backend `subscription_key()` recognized `pro` and `pro_plus` but not `pro_max`, so filtering and validation grouping could classify the same account as generic `pro` or fail to group it correctly.
- Backend `subscription_rank()` had no `pro_max` rank, so subscription change comparison could lose the intended order.

The fix now recognizes `Pro Max` before generic `Pro` in the UI, adds `pro_max` to backend keys and rank ordering, and exposes `Pro Max` and `Power` in the UI subscription filter.

2026-08-01 scoped release gate: included in [Final release gate - 2026-08-01](../evidence/final-release-gate-20260801.md). Full Rust default/no-default all-target tests, release build, UI/admin-ui build, Node contracts, real Claude CLI fake-upstream suite, feature docs, diff hygiene, fmt, and artifact inventory passed. A live browser screenshot remains useful UI evidence but is not required to prove this deterministic classifier fix.

## 用户可见现象与影响

- An account whose upstream/account-info title is `KIRO PRO MAX`, `Kiro Pro Max`, `pro-max`, `pro_max`, or `promax` can show the card badge `Pro`.
- The API still retains the raw title, so this is a classification/display issue rather than evidence that the upstream account was downgraded.
- Filtering by subscription has no `Pro Max` option and can therefore mix it with generic `Pro` behavior.
- Subscription validation/upgrade-downgrade grouping can rank `Pro Max` as unknown or generic `Pro`.

## 根因与源码链

1. `ui/src/features/credentials/credential-utils.ts` normalizes separators and then tested `normalized.includes('pro')` without a preceding `pro max` branch.
2. `ui/src/features/credentials/credential-card.tsx` renders the `subscriptionBadgeMeta()` label directly, so the misclassification is visible on the account card.
3. `ui/src/features/credentials/credentials-page.tsx` had `pro_plus`, `pro`, `trial`, and `free` filter options, but no `pro_max` or `power` option.
4. `src/admin/service.rs::subscription_key()` and `subscription_rank()` did not encode the `pro_max` tier.

This chain is deterministic and independent of screenshots, rasterization, or browser scaling.

## 复现

UI classifier input:

```text
subscriptionTitle = "KIRO PRO MAX"
```

Before the fix:

```text
subscriptionBadgeMeta(...).label == "Pro"
```

After the fix:

```text
subscriptionBadgeMeta(...).label == "Pro Max"
```

Backend classifier inputs covered by the regression:

- `Kiro Pro Max`
- `KIRO PRO_MAX`
- `pro-max`
- `promax`
- `Kiro Pro`
- `Kiro Pro+`
- `Kiro Power`

## 修复

- UI checks `pro max` before the generic `pro` branch and preserves the raw title in the badge tooltip.
- Backend compacts separators, recognizes `promax` as `pro_max`, and assigns ranks:
  - `free=1`
  - `trial=2`
  - `pro=3`
  - `pro_plus=4`
  - `pro_max=5`
  - `power=6`
- UI subscription filter now includes `Power` and `Pro Max` in addition to existing options.

## 验证与证据

- `feature/tests/run-cargo-scoped.sh subscription-tier-focused -- cargo test --bin kiro-rs subscription_key_and_rank_distinguish_pro_max_from_pro -- --nocapture`: `1 passed / 0 failed`.
- `feature/tests/run-cargo-scoped.sh admin-service-focused -- cargo test --bin kiro-rs admin::service::tests -- --nocapture`: `31 passed / 0 failed` (PgSQL integration test skipped because no test URL was provided).
- `feature/tests/run-cargo-scoped.sh fmt-subscription -- cargo fmt --check`: passed.
- `npm run check` in `ui`: passed.
- `npm run build` in `admin-ui`: passed.

## 残余风险与边界

- The focused test directly validates the backend classifier and ordering. A browser screenshot of a live account card is not required to prove the deterministic bug, but final candidate UI/browser validation remains part of the broader release gate.
- Raw upstream titles remain visible in the badge tooltip/title and API payload; the user-facing short label is intentionally normalized to `Pro Max`.
- Existing records whose title was persisted as generic `Pro` cannot be reconstructed by this fix; they require a fresh account-info query or credential refresh to obtain the upstream `Pro Max` title.

## 回滚

Revert the UI `pro max` branch, filter options, and backend `pro_max` key/rank changes together if a downstream consumer has an incompatible subscription enum. No credential or production data migration is required.
