# 114 升级后生产问题收敛与修复索引

日期：2026-07-24

本目录记录 114 升级后暴露的问题、生产现象、复现方式、修复方案和已执行验证。所有生产证据均已脱敏，不记录 admin key、数据库 URL、SSH 密码或外部池 API key。

## 问题清单

| 编号 | 问题 | 状态 | 修复点 | 验证 |
| --- | --- | --- | --- | --- |
| P1 | 现网从 113 升 114 后备用池看似丢失、Dashboard 报 `error returned from database` | 已修复/已加防复发保护 | `PostgresStore::connect` 增加当前二进制所需 schema 兼容检查；Compose 模板显式 `KIRO_RS_POSTGRES_MIGRATE_ON_START=true`；Dashboard 总接口降重；旧 admin-ui 改用拆分接口 | Rust schema 单测/聚焦编译；admin-ui tsc/build；新 UI tsc/build |
| P2 | 外部池成功请求大量 0 计费 | 已定位并修复主因 | 流式外部池成功但上游没有 usage event 时，基于请求估算和已输出 SSE 内容生成 `missing_stream_usage` billing，再走现有价格目录计费 | 新增 stream output token estimator 单测；新增 stream missing usage billing 单测；pricing alias 单测 |
| P3 | Usage 中出现 `sampled request rejection` | 已优化文案/保留采样语义 | request API key admission 采样诊断记录不代表上游失败；文案改为 `request rejected before upstream dispatch`，采样细节仍在 metadata | usage 采样单测、realtime 成功/错误细分测试 |
| P4 | 生产 Redis 调度退化后高 RPM 快速失败，并最终触发/暴露整池 `TEMPORARILY_SUSPENDED` 风控禁用 | 已完成核心防放大修复，待隔离高并发矩阵继续验证 | local-pool risk circuit；circuit 打开后 acquire/preflight 不先碰 Redis；风险控制后不继续烧剩余账号；单凭据/全局容量变化 warmup | 风险熔断、warmup、runtime migration、public error 聚焦测试通过 |
| P5 | 并发低、RPM 高、页面卡与调度观测口径 | 已完成源码复核、观测补强和入口防放大，待负载矩阵验证 | 区分 admission 并发、本地账号 lease、usage 完成 RPM、dashboard 聚合；新增 realtime success/error 细分；本地临时调度失败反馈到 per-key admission backoff | usage memory/Redis/PgSQL summary 聚焦测试、request admission backoff 测试和 handler 分类器测试通过 |

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
   - 继续确认 `request_rejection` 数量是否与 request API key 配额设置相符；
   - 观察 realtime `successRequests/errorRequests`，确认高 RPM 是否主要来自错误快失败。
