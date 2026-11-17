# 任务 s9：负载/混沌验收工具适配新架构（阶段 9，第一步：可观测性与脚本）

## 背景

8 个阶段已完成（见 `docs/claude-pool-refactor/13-实施验收记录.md`）。最终验收要用 mock 上游跑完整的负载与混沌矩阵（突然大并发、大并发持续、长流会话、大并发中报错后恢复、大并发后持续不恢复、单账号故障、连接重置、中途断流、客户端放弃、慢上游、挂起流、队列溢出、长时间浸泡）。本地已有三套脚本，但它们仍按旧架构写。本任务只做**工具适配与可观测性补齐**，不改调度算法、不改 usage 整形。

## 先读

- `scripts/loadtest/overload-run.sh`、`overload-probe.mjs`、`overload-verdict.py`、`scale-profile.py`：开环到达率探针与判定（最重要）
- `scripts/loadtest/scheduler-scenarios.mjs`：闭环分阶段场景
- `scripts/loadtest/anthropic-mock-upstream.mjs`：mock 上游，`/__control` 可注入故障，`GET /__control` 返回 `counters.perToken`
- `src/main.rs` 的 `readyz`（`AppHealthState`）、`src/account/mod.rs` 的 `status()`、`enter_account_queue`、`AccountReleaseDispatcher`、`src/storage/redis_cache.rs` 的 `account_dispatch_queue_size`
- `src/bin/pool_loadtest.rs`（内置 fake 上游，Anthropic SSE；`--target-pid` 采 RSS/FD）

## A. `/readyz` 补调度可观测字段（后端）

`AppHealthState` 增加 `account_manager: Option<Arc<AccountManager>>`，`readyz` 响应新增：

```json
"dispatch": {
  "queuedRequests": <Redis dispatch queue 当前长度，account_dispatch_queue_size()，Redis 不可用时 null>,
  "globalInFlight": <全局在飞数，来自 Redis global in-flight 键或进程内计数>,
  "pendingRedisReleases": <AccountReleaseDispatcher.pending_len()>,
  "dispatchedTotal": <进程启动以来成功选中账号并发出上游请求的累计数（新增 AtomicU64）>,
  "dispatchRatePerSec": <最近 5 秒 dispatchedTotal 的增量 / 5，进程内滑动窗口>,
  "secsSinceLastDispatch": <距最近一次成功 dispatch 的秒数，从未有过则 null>,
  "progressing": <queuedRequests == 0 || secsSinceLastDispatch < 5>
}
```

`readyz` 的 ready 判定**不变**（只看 PG/Redis/事件订阅）；`dispatch` 只是观测。这些计数器放在 `AccountManager` 上，`forward_with_failover_inner` 在拿到账号 lease 并即将发上游请求的位置 `fetch_add`。给 `readyz` 加单测：无 manager 时 `dispatch` 为 null；有 manager 且无流量时 `queuedRequests=0, progressing=true`。

## B. `/api/admin/accounts/status` 加汇总与调度计数

`AccountsStatusResponse` 新增 `summary: { total, enabled, dispatchable, inCooldown, autoDisabled, inFlightTotal, globalInFlight, queuedRequests }`，每个 `AccountStatus` 新增 `dispatchedTotal: u64`（该账号被选中次数，进程内 `HashMap<u64, AtomicU64>` 或 `DashMap`）。前端 `ui/src/types/accounts.ts` 同步加可选字段（不要求页面展示）。

## C. `overload-run.sh` 适配

1. 容器名：`claude-pool-postgres-local`/`claude-pool-redis-local` 改为通过环境变量 `PG_CONTAINER`（默认 `kiro-rs-postgres-local`）、`REDIS_CONTAINER`（默认 `kiro-rs-redis-local`）、`PG_USER`（默认 `kiro_rs`）指定；`BIN` 默认 `/tmp/kiro-rs-rust-analyzer-target/release/claude-pool`。
2. `prepare()`：不再用模板库 `pool_identity_e2e` 和 `credentials` 表。改为：`CREATE DATABASE $DB`（空库），先启动 claude-pool 让它建 schema 并写默认 `runtime_config`，再通过**管理 API** 创建 `ACCOUNTS` 个账号（`POST /api/admin/accounts`，`baseUrl=http://127.0.0.1:$MOCK_PORT`，`apiKey=mock-access-70NN`，`authType=x_api_key`，`maxConcurrentRequests` 按 `PER_CONC` 的 spread，`priority` 在 `tiers_*` 用例中前 1/4 为 0 其余 10），再 `PUT /api/admin/config/runtime` 应用调度设置（`accounts.maxQueuedRequests=$QUEUE`、`accounts.dispatchMaxWaitSecs=$WAIT_SECS`、`accounts.globalMaxConcurrentRequests=$GLOBAL_MAX`、`accounts.streamIdleTimeoutSecs=$IDLE_SECS`、`accounts.requestTimeoutSecs=$MAX_DURATION_SECS`、`requestAdmission` 全 0 表示不限）。账号 token 命名保持 `mock-access-70NN` 以便 `limits.json` 与 mock 的 `perToken` 对齐。`PER_RPM` 在新系统无账号级 RPM，置 0 并在日志中说明；`rpm_bound`/`pool_rpm_*` 用例标记为 `not_applicable` 跳过。
3. mock 以 `x-api-key` 识别 token（已支持 `bearerToken()` 读两种头）。
4. `redis leases after` 一行改读 `account:global:inflight:*` 键（前缀用 config 的 keyPrefix）。
5. 服务日志过滤里的 `credential_id` 正则改 `account_id|pool_id`。
6. 其余结构（CPU/RSS 采样、shutdown 计时、TIME_WAIT 保护、负载等待）保留。

## D. `overload-probe.mjs` 适配

1. 收尾读账号调度计数：`/api/admin/credentials/runtime?ids=` 改为 `GET /api/admin/accounts/status`，用 `accounts[].account.apiKey`（管理端返回的是掩码，所以改用 `account.name`，账号创建时 `name` 设为与 token 相同的 `mock-access-70NN`）映射 `dispatchedTotal`。
2. `/readyz` 的 `dispatch.*` 字段按 A 节读取（字段名已对齐，无需改）。
3. 状态码分类：新系统容量类为 **529 overloaded_error**，不是 429；`rejected` 的定义改为 `statusCode === 429 || statusCode === 529`，`rejectKinds` 按 `error.type` 区分 `overloaded_error`/`rate_limit_error`。sub2api 风格重试对 529 同样重试。
4. 其它逻辑不变。

## E. `overload-verdict.py` 适配

`UPSTREAM_DOWN_PHASES` 保留；增加对 `summary`（若报告含 `accountsSummary`）的检查：drain 结束后 `inFlightTotal == 0` 且 `queuedRequests == 0`，否则 problems 加一条。其余不变。

## F. `scheduler-scenarios.mjs` 适配

1. 删除 `protocols_with_errors`、`openai_overload`（OpenAI 入口已删）与 `external_overflow`、`external_on_524`（已无本地/外部之分）四个场景；删除 `--external-mock` 参数与 `external` 字段。
2. `failTokens` 里的 `mock-access-5001` 改为参数化：新增 `--account-token-prefix`（默认 `mock-access-70`），场景里用 `${prefix}01`。
3. 结论判定：`stalls` 用 `ready.dispatch.progressing === false`（A 节已提供）。
4. 新增场景（按用户要求补全）：
   - `sudden_burst_sustained`: baseline c=3 6s → burst c=80 持续 90s → after c=3 10s。
   - `long_stream_sessions`: 同一组 20 个会话（`metadata.user_id` 固定 20 个），每个会话连续 30 轮流式长输出（`streamChunks=120, chunkDelayMs=250`，单流约 30s），持续 10 分钟；断言每轮 `message_stop` 完整、无 `stream_cut`。
   - `burst_errors_then_recover`: baseline → c=60 且 `errorRate=1,errorStatus=500` 30s → 恢复 c=60 20s → after c=3。断言恢复阶段首 10 秒内 ok 率回到 >90%。
   - `burst_errors_never_recover`: baseline → c=60 且 `errorRate=1,errorStatus=500` 持续 120s → 不恢复直接 after c=3 仍 `errorRate=1` 20s。断言：期间对外全部是 `overloaded_error`/`api_error` 且 p95 响应时间有界（< dispatchMaxWaitSecs+10s）、`/readyz` 始终 200、无 `stall`（有 dispatch 在进行只是都失败）、无进程崩溃。
   - `mixed_chaos_soak`: 10 分钟：每 60s 轮换一种故障（500 30%、429、reset 20%、midStream 30%、hang 5%、slow header 3s），c=40；最后 60s 全部恢复 c=6。
5. 每个场景结束输出 `verdict: PASS|FAIL` 行，判定规则写在场景定义里（`expect` 字段）：`ok_rate_min`（恢复/正常阶段最小 ok 率）、`max_stall_secs`、`require_terminal`（流式必须 `message_stop`）。缺省：正常阶段 ok ≥ 99%、恢复阶段 ok ≥ 90%、stall ≤ 2s。
6. 结尾采样服务进程 RSS/FD（通过 `--target-pid`，用 `ps -o rss=` 与 `lsof -p | wc -l`）在场景开始、峰值、结束，写进报告；`mixed_chaos_soak` 与 `long_stream_sessions` 断言结束 RSS ≤ 峰值 × 0.8 或 ≤ 开始 + 150MB，FD 回到开始 + 10 以内。

## G. 一键编排脚本 `scripts/loadtest/run-acceptance-matrix.sh`

```
Usage: scripts/loadtest/run-acceptance-matrix.sh [quick|full]
```

- 起一次隔离实例（复用 `overload-run.sh` 的 prepare 逻辑抽成 `scripts/loadtest/lib/isolated-instance.sh`：`start_instance`/`stop_instance`/`seed_accounts`/`apply_dispatch_settings`），端口 19381、mock 39181、Redis db 13、库 `claude_pool_acceptance`。
- `quick`（约 15 分钟）：scheduler-scenarios 的 `normal sudden_burst sustained_overload burst_with_errors_recovering burst_with_short_outage burst_with_long_outage single_account_failing connection_resets long_stream_burst mid_stream_resets client_timeouts burst_errors_then_recover burst_errors_never_recover sudden_burst_sustained` + overload-run 的 `steady_ok spike_sub2api storm_5xx storm_429 queue_overflow`。
- `full`（约 60 分钟）：quick 全部 + `long_stream_sessions mixed_chaos_soak` + overload-run 的 `sustained_sub2api long_spike square_sub2api storm_reset storm_midstream spike_with_errors slow_wave slow_accounts_spike spike_impatient long_overload pool_hang pool_trickle`。
- 每个用例输出到 `$OUT_DIR/<case>.json` 与 `.log`；结束跑 `overload-verdict.py` 与各场景 `verdict` 汇总成 `$OUT_DIR/SUMMARY.md`（表格：用例、阶段、状态分布、ok 率、p95、stall、RSS/FD、PASS/FAIL）；任一 FAIL 则脚本退出码 1。
- 结束必须：kill 服务与 mock、drop 库、FLUSHDB、删除临时目录（`KEEP=1` 保留）。

## 允许改动的文件

`src/main.rs`（readyz 与 AppHealthState）、`src/account/mod.rs`（只加计数器与 summary，不改调度/整形逻辑）、`src/admin/{handlers,service,types}.rs`（status 汇总）、`ui/src/types/accounts.ts`、`scripts/loadtest/**`、`tests/`。**不得改** `src/anthropic/**`、`src/account/usage_projection.rs`、golden。

## 自检

```
cargo fmt --all && cargo build -q --release --bins
cargo test --bin claude-pool -q -- readyz
cargo test --bin claude-pool -q -- golden_usage
cargo test --bin claude-pool -q 2>&1 | tail -3
pnpm --dir ui check
node --check scripts/loadtest/overload-probe.mjs && node --check scripts/loadtest/scheduler-scenarios.mjs
bash -n scripts/loadtest/overload-run.sh scripts/loadtest/run-acceptance-matrix.sh scripts/loadtest/lib/isolated-instance.sh
# 冒烟：起隔离实例跑一个最短用例，确认链路通
KEEP=0 scripts/loadtest/run-acceptance-matrix.sh smoke   # smoke = scheduler-scenarios normal + overload steady_ok（缩短到 steady:5:20）
```

`smoke` 模式必须在任务结束前跑通并把 SUMMARY.md 路径写进汇报。
