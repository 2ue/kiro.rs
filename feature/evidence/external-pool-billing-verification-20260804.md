# 外部池 usage 原始成本与 Dashboard 计费验证（2026-08-04）

Status: `focused-verification-complete / production-observation-pending`

Scope: 当前工作树中外部池 usage 捕获、原始成本、PgSQL rollup、Redis Dashboard
materialization、Admin usage 文案和文档索引合同。

本轮没有修改生产配置、PostgreSQL、Redis、容器或服务进程；生产三台机器的历史
统计仍使用此前只读 evidence，当前验证只证明本地工作树的修复语义。

## 验证结论

当前工作树通过 focused gate：

- 流式 OpenAI-compatible usage 会被归一化并捕获为外部池原始 usage；
- 外部池成功记录的“原始成本”优先来自上游返回的真实 usage，只有缺失 usage
  时才退回本地估算；
- 本地 usage 整形继续独立生成“展示计费”“补偿后计费”“上报费用”，不会反向
  覆盖“原始成本”；
- PgSQL usage/pricing 持久化、外部池 billing rollup、Redis usage summary 与
  Dashboard materialization 都能通过本地 loopback 测试；
- Admin usage 文案已说明“上游未返回 usage 时才使用本地估算 fallback”。

## 命令结果

```text
git diff --check
=> pass
```

```text
node feature/tests/check-feature-docs.mjs
=> PASS: 74 issue documents satisfy the section contract; 323 relative links resolve.
```

```text
pnpm --dir admin-ui build
=> tsc -b && vite build passed
```

```text
feature/tests/run-cargo-scoped.sh usage-billing-external-pool -- cargo test --locked external_pool::tests
=> external_pool::tests: 214 passed / 0 failed
=> kiro_loadtest filtered target: 0 tests run / 0 failed
```

```text
KIRO_RS_REQUIRE_STORAGE_TESTS=1
KIRO_RS_TEST_POSTGRES_URL=<local loopback postgres>
feature/tests/run-cargo-scoped.sh usage-billing-storage-pg -- cargo test --locked postgres_persists_runtime_config_credentials_stats_usage_and_pricing -- --nocapture --test-threads=1
=> storage::postgres::tests::postgres_persists_runtime_config_credentials_stats_usage_and_pricing: 1 passed / 0 failed
```

```text
KIRO_RS_REQUIRE_STORAGE_TESTS=1
KIRO_RS_TEST_POSTGRES_URL=<local loopback postgres>
feature/tests/run-cargo-scoped.sh usage-billing-storage-pg-rollup -- cargo test --locked postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup -- --nocapture --test-threads=1
=> storage::postgres::tests::postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup: 1 passed / 0 failed
```

```text
KIRO_RS_REQUIRE_STORAGE_TESTS=1
KIRO_RS_TEST_REDIS_URL=<local loopback redis>
feature/tests/run-cargo-scoped.sh usage-billing-storage-redis -- cargo test --locked redis_usage_summary_and_dashboard_are_materialized -- --nocapture --test-threads=1
=> storage::redis_cache::tests::redis_usage_summary_and_dashboard_are_materialized: 1 passed / 0 failed
```

```text
cargo fmt --check
=> pass
```

## 边界

- 本轮没有重新发起三台生产服务的 Messages 请求；生产复发观察仍待升级后确认。
- “原始成本”仍是 `kiro.rs` 本地价格目录按上游 usage 估算出的参考成本，不是
  外部供应商返回的美元账单字段。
- 如果外部供应商未来返回真实金额或账单接口，应新增“外部供应商真实费用”字段，
  不应把当前“原始成本”改名后继续使用本地价格目录。
