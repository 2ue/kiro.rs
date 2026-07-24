# 114 升级后生产问题收敛与修复索引

日期：2026-07-24

本目录记录 114 升级后暴露的问题、生产现象、复现方式、修复方案和已执行验证。所有生产证据均已脱敏，不记录 admin key、数据库 URL、SSH 密码或外部池 API key。

## 问题清单

| 编号 | 问题 | 状态 | 修复点 | 验证 |
| --- | --- | --- | --- | --- |
| P1 | 现网从 113 升 114 后备用池看似丢失、Dashboard 报 `error returned from database` | 已修复/已加防复发保护 | `PostgresStore::connect` 增加当前二进制所需 schema 兼容检查；Compose 模板显式 `KIRO_RS_POSTGRES_MIGRATE_ON_START=true`；Dashboard 总接口降重；旧 admin-ui 改用拆分接口 | Rust schema 单测/聚焦编译；admin-ui tsc/build；新 UI tsc/build |
| P2 | 外部池成功请求大量 0 计费 | 已定位并修复主因 | 流式外部池成功但上游没有 usage event 时，基于请求估算和已输出 SSE 内容生成 `missing_stream_usage` billing，再走现有价格目录计费 | 新增 stream output token estimator 单测；新增 stream missing usage billing 单测；pricing alias 单测 |
| P3 | Usage 中出现 `sampled request rejection` | 已解释，暂不作为代码缺陷处理 | 这是 request API key admission 采样诊断记录，设计上 `model=unknown/tokens=0/cost=0`，用于低开销记录 RPM/队列/入口拒绝，不代表上游模型调用失败 | 生产聚合显示 24h 只有 41 条，不是 0 计费主因 |
| P4 | 生产 Redis 调度退化后高 RPM 快速失败，并最终触发/暴露整池 `TEMPORARILY_SUSPENDED` 风控禁用 | 已记录，待后续修复验证 | 当前 main 已有 Redis fault-domain 分离、scheduler degraded fallback、dashboard 降重；仍需新增 local-pool risk circuit、导入/调参 ramp-up、per-key admission/backoff | 待执行 Redis latency 注入、mock 上游风控、批量导入 ramp-up、request key admission 与前端 dashboard 压测 |

## 本轮代码改动范围

- `src/storage/postgres.rs`
  - 启动后执行轻量 schema compatibility check。
  - 覆盖 114 后立即会被运行路径使用的关键表/列，例如 `external_upstream_pools.revision`、`usage_records.rollup_active`、`model_capabilities_sync_status.reasoning_fields`、credential revision/generation、usage cost rollup 字段等。
  - `/usage-dashboard` 后端总接口不再给所有窗口附带高成本 breakdown/外部池逐池拆分，改为复用基础窗口、series、top。

- `docker-compose.database.yml`
  - app 环境显式增加 `KIRO_RS_POSTGRES_MIGRATE_ON_START: ${KIRO_RS_POSTGRES_MIGRATE_ON_START:-true}`。

- `docs/ai-docker-compose-deployment.md`、`README.md`
  - 明确生产升级必须保持启动迁移开启，或用环境变量覆盖挂载配置。

- `admin-ui/src/api/usage.ts`、`admin-ui/src/hooks/use-usage.ts`、`admin-ui/src/components/usage-dashboard-panel.tsx`
  - 旧管理 UI 改为拆分加载 `/usage-dashboard/windows`、`/series`、`/top`、`/breakdown`、`/external-pool-billing`。
  - 单个慢分片不再导致整个总览不可用。

- `src/external_pool.rs`、`src/external_pool/tests.rs`
  - 流式外部池 guard 累计 downstream SSE 输出 token。
  - 流式成功但没有 captured usage 时，构造 estimated `ExternalPoolBilling`。

## 已执行验证

### 前端

- `admin-ui`: `pnpm exec tsc -b --pretty false` 通过。
- `admin-ui`: `pnpm run build` 通过。
- `ui`: `pnpm run check` 通过。
- `ui`: `pnpm run build` 通过，仅保留既有大 chunk warning。

### Rust

所有 Cargo 命令均通过 `feature/tests/run-cargo-scoped.sh` 运行并清理 scoped target，避免堆积构建产物。

- `cargo fmt --all -- --check` 通过。
- `cargo test --locked --all-targets required_postgres_schema` 通过。
- `cargo test --locked --all-targets postgres_schema_compatibility_check_rejects_missing_upgrade_column` 编译通过；本机无 `KIRO_RS_TEST_POSTGRES_URL` 时测试体按既有约定跳过真实 PgSQL。
- `cargo test --locked --all-targets stream_missing_usage_builds_estimated_billable_external_pool_billing` 通过。
- `cargo test --locked --all-targets stream_output_token_estimator_counts_text_thinking_and_tool_events` 通过。
- `cargo test --locked --all-targets --no-default-features required_postgres_schema` 通过。
- `cargo test --locked --all-targets estimate_matches` 通过，覆盖 `claude-opus-4-8` 与 `claude-opus-4.8` 的计价 alias 互配。
- `git diff --check` 通过。

## 后续发版注意点

1. 当前 114 镜像仍是 `18b286e`，生产 0 计费流式问题需要新镜像版本才会生效。
2. 不建议重打已经发布并部署过的 `v0.0.114`；应按当前仓库状态发下一个版本，除非明确要求删除远端 tag 和镜像并重发。
3. 发布后在生产验证：
   - `/api/admin/external-pools` 仍返回现有池数量；
   - `/api/admin/usage-dashboard/windows`、`/series`、`/top` 200；
   - 观察新流式外部池成功记录：`externalPoolBilling` 应存在，`usageEstimateReason=missing_stream_usage`，`pricingAvailable=true`，`estimatedCostUsd>0`；
   - 继续确认 `sampled request rejection` 数量是否与 request API key 配额设置相符。
