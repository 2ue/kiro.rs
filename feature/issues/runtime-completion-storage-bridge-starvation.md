# Runtime completion storage bridge starvation

Status: `fixed-in-working-tree / v0.0.120 candidate`

Severity: `critical`

## 现象

`docs/kiro-rs-root-cause-package-20260726T170519+0800` 记录了 2026-07-26 的生产事故：

- Docker health 显示 healthy，但 `/healthz`、`/readyz`、业务 API 超时。
- app 进程仍存活，CPU/内存/FD 没有打满。
- 端口 socket 大量堆积，出现高 `CLOSE_WAIT`、高 listen backlog。
- usage 完成记录停止或明显滞后。
- 本地部署和 152.53.194.170 独立部署都复现同类形态。

这不是“几百 RPM 太高”，而是主业务 completion / scheduler 路径和 PgSQL/Redis 同步存储桥耦合导致 runtime 进展被拖住。

## 权威证据

- `docs/kiro-rs-root-cause-package-20260726T170519+0800/ROOT_CAUSE_ANALYSIS.md`
- `docs/kiro-rs-root-cause-package-20260726T170519+0800/EVIDENCE_CLAIM_MAP.md`
- `docs/kiro-rs-root-cause-package-20260726T170519+0800/170-live-evidence-summary.md`
- `docs/kiro-rs-root-cause-package-20260726T170519+0800/incident-evidence-20260726T161622+0800/`

关键证据链：

1. TCP connect healthcheck 仍可成功，但 HTTP handler 不再响应。
2. 完成 usage QPS 不高，但 listen backlog / `CLOSE_WAIT` 远高于完成请求量。
3. 线程采样显示 runtime 未推进正常 HTTP lifecycle。
4. 日志中的慢路径是 token manager PgSQL/Redis 调度/运行态路径，不是独立 usage writer。

## 根因

事故版本 `0.0.118` 中，流式/非流式 completion 会在请求终结路径上同步进入 token manager 状态写：

- stream EOF / success
- stream drop / soft failure
- upstream stream failure
- non-stream body validated success
- MCP/WebSearch validated success

这些路径会触发：

- `report_success_for_session_with_latency`
- `report_success_with_latency`
- `persist_success_state`
- `clear_session_soft_failure`
- `record_session_soft_failure`
- `block_on_credential_pgsql`
- `block_on_scheduler_redis_affinity`
- `block_on_storage`

同步 PgSQL/Redis bridge 在 PgSQL pool 压力、Redis 抖动、连接生命周期堆积时，会阻塞 stream terminal path 和 Tokio worker，放大成整个 HTTP runtime 假死。

## 同类问题清单

同类问题不是一个点，而是一类工程模式：

1. `block_on_storage` 在 runtime 中同步等待 PgSQL/Redis future。
2. best-effort storage executor 曾优先复用当前 runtime handle。
3. completion terminal path 曾在释放 in-flight lease 前同步写 PgSQL/Redis。
4. session sticky soft-failure/clear 曾同步等待 Redis affinity 操作。
5. provider 请求内 session retry/unbind 曾同步等待 Redis affinity 操作。
6. usage/admin store 也存在类似 `block_on_*` 同步桥，主要影响管理/观测面。

## 修复设计

本次修复选择不破坏既有直接管理/测试同步语义，而是把真实请求 completion 路径改为 deferred：

1. completion 先释放 in-flight lease。
2. 本地 token manager 状态立即更新，保证账号容量、成功计数、健康 EWMA、dispatchability 立刻恢复。
3. PgSQL 凭据运行态写入改为 FIFO runtime mutation 排队，复用已有幂等 operation id、generation fence、coalesce 和 flush worker。
4. Redis scheduler success health 保持 best-effort task。
5. session soft-failure / clear 先更新本地 sticky cache，再异步 best-effort 写 Redis。
6. 请求 retry/error 中的 session soft-failure / unbind 同样改为本地 sticky cache 先行，Redis affinity 异步 best-effort。
7. storage executor 默认使用 dedicated `kiro-storage-task` runtime，不再把 best-effort/critical worker 绑定到 HTTP runtime。
8. 保留直接 `report_success`、凭据禁用/强一致错误状态写的同步语义，避免破坏跨 manager 顺序依赖；生产请求 completion 和 session affinity 不再走这些同步等待版本。

## 兼容性

- 成功计数、failure reset、warmup decrement 仍本地立即生效。
- PgSQL 持久化仍通过原有 FIFO mutation 机制完成，支持 coalesce。
- 若 PgSQL 暂时不可用，凭据只进入 `runtime_persistence_degraded`，不会因 success mutation 被调度隔离。
- 直接管理/测试调用的 `report_success` 仍可同步等待 PgSQL，用于需要强顺序的场景。
- quota/risk/invalid refresh token 等凭据禁用路径仍保留同步 durable 写，但已通过 dedicated storage runtime 隔离，不再复用 HTTP runtime handle。

## 复现方案

最小复现：

1. 使用 PgSQL store 创建本地凭据。
2. 占满 PgSQL pool。
3. 触发 terminal success report。
4. 旧行为：terminal success 会等到 PgSQL timeout 量级。
5. 新行为：terminal deferred success 在 500ms 内返回，账号仍 dispatchable；释放 PgSQL pool 后 FIFO mutation flush 成功。

生产级复现：

1. fake upstream 创建大量长流。
2. 让 100 条流在短时间内 EOF/drop。
3. 注入 Redis/PgSQL latency。
4. 持续探测 `/healthz`、listen backlog、FD/RSS、成功恢复请求。

## 验收矩阵

- Rust unit/integration：
  - `terminal_deferred_success_does_not_wait_for_pgsql_pool_pressure_for_five_rounds`
  - `stream_completion_reports_success_once`
  - `stream_completion_soft_failure_does_not_count_success`
  - `stream_completion_upstream_failure_cools_down_credential`
  - API/MCP completion release tests
- Static：
  - `cargo fmt --all -- --check`
  - `cargo check --locked --all-targets --no-default-features`
- Load/chaos：
  - fake upstream long stream EOF/drop burst
  - PgSQL/Redis latency injection
  - health remains responsive
  - FD/socket count returns near baseline

## 修复后结果

2026-07-26 当前工作树已完成代码修复与聚焦验证：

- `KiroApiCompletion` / `KiroStreamCompletion` / `McpCallCompletion` success path 改为 deferred success report。
- stream success / soft failure / upstream stream failure 均先释放 in-flight lease，再处理本地状态和异步存储副作用。
- session soft-failure / clear 增加 deferred 版本，先更新本地 sticky cache，Redis 写入改为 best-effort。
- provider 请求 retry/error 中的 session soft-failure / unbind 改为 deferred，真实请求链路不再同步等待 Redis affinity。
- storage executor 默认固定使用 dedicated `kiro-storage-task` runtime。

验证结果：

- `cargo +1.92.0 fmt --all -- --check`：通过。
- `cargo +1.92.0 test --locked --bin kiro-rs completion -- --test-threads=1`：`11 passed / 0 failed`。
- `cargo +1.92.0 test --locked --bin kiro-rs terminal_deferred_success_does_not_wait_for_pgsql_pool_pressure_for_five_rounds -- --test-threads=1`：`1 passed / 0 failed`。
- `cargo +1.92.0 test --locked --bin kiro-rs postgres_pool_pressure_backlogs_non_terminal_success_without_quarantine_for_five_rounds -- --test-threads=1`：`1 passed / 0 failed`，确认保留的直接同步 success 仍维持 FIFO/backlog 语义。
- `cargo +1.92.0 test --locked --bin kiro-rs test_deferred_session_soft_failure_and_unbind_use_local_state -- --test-threads=1`：`1 passed / 0 failed`。
- `cargo +1.92.0 check --locked --all-targets --no-default-features`：通过。
- `rustup run 1.92.0 node scripts/ci/check-clippy-baseline.mjs`：通过，warning count `815`，低于 baseline `849`。
- `node feature/tests/inventory-build-artifacts.mjs --gate`：通过，`targets=0 reservations=0 target_processes=0 blockers=0`。
- 所有 Cargo 命令均通过 `feature/tests/run-cargo-scoped.sh` 执行，scoped target 清理后仓库 `target/` 为 `0B`。

## 残余风险

1. usage/admin store 的同步桥属于相同模式，但不是本事故主业务 completion 根因；应单独排期拆分。
2. TCP-only Docker healthcheck 仍会掩盖 HTTP runtime 假死；应改为 HTTP `/healthz` 或 `/readyz`。
3. 跨实例强顺序仍依赖直接同步路径或 FIFO flush；不能把所有 manager 状态 mutation 一刀切 fire-and-forget。
4. 禁用类错误路径仍会等待 durable 状态写，但已经不使用 HTTP runtime 执行器；如后续生产证据显示禁用风暴仍会拖慢请求，应单独增加 admission-safe disabled mutation queue。
