# 质量感知调度实现进度

Last updated: 2026-09-15

设计文档：[external-pool-quality-aware-scheduling.md](external-pool-quality-aware-scheduling.md)
测试维度设计：[quality-aware-scheduling-test-matrix.md](quality-aware-scheduling-test-matrix.md)

## 2026-09-15 实现补强与验证

- [x] `external_pool_degrade_window_secs` 已接入 Redis 质量采样 Lua：
      维护窗口内样本/失败计数，劣化判定使用当前窗口失败率，旧状态无窗口字段时兼容回退
      到累计样本与 EWMA。
- [x] 质量采样写入会保留已有更长 Redis TTL，避免避让期内的真实请求用较短样本 TTL
      提前抹掉避让/恢复状态。
- [x] 恢复爬坡完成后显式重置连续劣化层级，避免持续流量刷新质量键 TTL 时继承历史指数退避。
- [x] 流式外部请求的 SSE `error` 事件、响应体读取错误与 post-header 传输错误已回灌质量失败
      样本；客户端主动取消仍保持健康度中性。
- [x] 管理端“清除冷却”同步清除外部池质量键，避免手动恢复后旧质量状态立即重新降权。
- [x] `ui` / `admin-ui` 的外部池状态类型补充质量视图字段；两套池列表展示错误率、首字/总耗时、
      样本数、冷启动、避让与恢复进度。
- [x] 回归验证：质量聚焦测试 `47/47`、恢复层级回归 `2/2`、劣化窗口回归 `1/1`、Redis
      避让层级重置回归 `1/1`、Rust
      `cargo check --all-targets --locked`、`cargo fmt --all -- --check`、UI `pnpm check` /
      `pnpm build` 均通过；完整 Rust 二进制回归覆盖 `2055` 个非忽略测试（首轮
      `2054 passed / 1 flaky failure / 6 ignored`，唯一失败用例隔离复跑
      `1 passed / 0 failed`），`kiro_loadtest` 为 `31/31`，`external_pool::tests` 分组为
      `343/343`。
- [x] `admin-ui pnpm build`：本地 `node_modules` 缺少锁文件中已声明的
      `@radix-ui/react-scroll-area`；按锁文件安装依赖后构建通过，与本次质量调度字段改动无关。

本文件用于防止实现进度丢失。每完成一个子项立即更新。

## 状态图例

- `[ ]` 未开始
- `[~]` 进行中
- `[x]` 完成
- `[!]` 阻塞

---

## P0-a kiro.rs 质量采样（仅采集，不改选择逻辑）

- [x] `ExternalPoolQualityState` 结构定义 `src/storage/redis_cache.rs:325`
      （error_rate / ttft_ewma / latency_ewma / sample_count / probation / recovery）
- [x] Redis Lua：质量状态读取并入 `external_pool_coordinator_snapshots`（**同一次往返**，
      新增 `quality_key` 可选键 + `ARGV` 标志位；未请求时回填空串保证游标算术恒定）
- [x] Redis Lua：`record_external_pool_quality_sample` 原子读改写
      （失败率 EWMA 双向 + 延迟/首字 EWMA 仅成功样本累计）
- [x] Redis Lua：`mark_external_pool_probation`（取最大值，支持指数退避层级）
- [x] `clear_external_pool_quality`（管理端清除用）
- [x] `PoolRuntimeSnapshot` 扩展 `quality` 字段并在 `decode_pool_runtime_snapshot` 透传
- [x] `record_external_success` 接入成功回灌（**此前完全缺失**，是最大缺口）
- [x] 失败路径接入质量采样（`forward_with_failover_result` 错误分支）
- [x] `external_error_affects_pool_quality`：下游请求自身问题不污染健康度
- [x] 配置项：主开关 + 18 个参数 + 默认值 + Default impl
- [x] `cargo check --bins` 通过
- [ ] `ExternalPoolStatus` 暴露质量字段（移到 P1 可观测性一并做）
- [x] **单元测试 15 项全部通过**（`KIRO_RS_TEST_REDIS_URL=redis://127.0.0.1:26379`）
  - 纯逻辑 6 项（`src/storage/redis_cache.rs` tests）：
    损坏载荷退化为 None / 越界字段夹紧与丢弃 / 样本门槛冷启动 /
    避让窗口到期边界（右开区间）/ 恢复爬坡线性且饱和 / 质量键按池隔离
  - Redis Lua 往返 5 项：EWMA 数学 / 损坏与 WRONGTYPE 自愈 /
    避让取最大值不缩短 / **200 并发采样零丢失（读-改-写原子性）** /
    批量快照携带质量且仍是 **1 次往返**
  - 错误归因 4 项（`src/external_pool/tests.rs`）：
    下游超长请求不污染 / 真实上游 400 计入 / 模型路由缺失不污染 /
    上游故障（429/5xx/超时/网络层）全部计入
- [x] **全量回归：2002 passed / 0 failed**，既有逻辑未破坏

### P0-a 测试中发现并修复的问题

- `external_pool_coordinator_snapshot`（单池路径）未带 `quality_key`。
  确认仅测试与诊断使用，调度热路径走批量接口，故显式置 `None` 并加注释说明。
- `cargo check --bins` **不编译测试代码**，必须用 `cargo test --bins --no-run` 才能
  发现结构体新增字段导致的既有测试断裂。后续阶段一律以后者为准。

### P0-a 设计决策

- **质量键始终读取，不按开关条件**：选择运行态快照缓存键若带上开关状态，
  开关切换会让 100ms TTL 的热路径缓存失效并抖动。读取与是否参与评分解耦。
- **质量样本不做合并**（对比 `reset_pool_auto_disable_failure_counts` 的 1s 合并）：
  每个请求都是独立数据点，合并会破坏 EWMA 统计意义。
- **延迟只在成功样本上累计**：失败请求的耗时多为超时或立即拒绝，
  混入会同时污染"快"和"慢"两个方向。
- `ExternalPoolsConfig` 去掉 `Eq` 派生（f64 权重），既有变更检测只依赖 `PartialEq`。

## P0-b kiro.rs 加权评分选择

**可测性约束（来自测试维度设计）**：`select_external_pool_candidate` 必须保持纯函数，
质量状态与**当前时间 `now_ms` 都要作为入参注入**，不得在函数内部读 `Utc::now()`。
否则通路 A 的 `D3-slow-recover` / `D3-never-recover` 无法测试。

- [x] 配置项：主开关 + 权重组 + Top-K（P0-a 已完成）
- [x] 候选集改为 `ExternalPoolCandidate` 结构体，携带质量状态（保持纯函数、`now_ms` 注入）
- [x] `select_external_pool_candidate` 改加权评分 + Top-K 加权随机
- [x] `select_external_pool_candidate_legacy` 保留旧逻辑，主开关关闭时走它
- [x] 相对中位数归一化（首字/耗时/失败率各自独立）
- [x] 降级兜底路径改用 legacy（Redis 不可用时质量数据本不可信）
- [x] **优先级硬分层**（见下方重大修正）
- [x] 配置校验 `validate_external_pools_config`（`src/admin/service.rs`，含跨字段校验
      `max_probation_secs >= degrade_probation_secs`）
- [x] **单元测试 24 项全部通过**
  - 选择逻辑 18 项（`src/external_pool/tests.rs`）：
    **开关关闭与 legacy 逐字节等价（L2-14 闸门）** / 优先级硬分层不被质量分打穿 /
    失败率信号权重高于延迟信号 / 首字与总耗时信号相互独立 /
    **全员劣化时候选池不清空**（"降级时保证可用"）/ 避让池不被饿死 /
    样本不足视为中性 / Top-K 加权随机份额分布
  - 配置校验 6 项（`src/admin/service_tests.rs`）：
    **默认开关为 true** / EWMA alpha 边界 / 权重拒绝负数与非有限值 /
    避让上限不得低于单次避让 / 失败率阈值必须是比率 / probe share 与 Top-K 边界
- [x] **全量回归：2024 passed / 0 failed**，既有逻辑未破坏

### ⚠️ P0-b 重大设计修正：优先级必须是硬分层

**既有测试 `external_pool_candidate_selection_handles_multiple_backup_pools` 捕获了一个
会静默破坏生产语义的缺陷**，这正是"不得破坏既有逻辑"要求的价值所在。

初版实现把优先级当作评分中的**一项权重**（`priority * priority_weight`），
再对全体候选做 Top-K 加权随机。问题：

- 默认 `priority_weight = 1.0`，优先级 1/2/3 的池只差 1.0 分；
- Top-K 权重公式 `worst - score + 1.0` 让优先级 3 的池仍能拿到 1/6 的抽中概率；
- 结果：**用户配置的优先级被质量分打穿**，备用池会抢主池流量。

修正：**先按 `effective_priority` 取最优层过滤，质量评分只在层内重排。**
优先级是硬分层语义，不是可被质量分抵消的一项权重。
质量感知的作用是"同优先级内选更好的"，不是"把差优先级的提上来"。

> 保留评分中的优先级项仅用于可观测性展示（管理端要能看出分数构成），
> 在比较中它对层内候选是常数。

## P1 kiro.rs 劣化 / 避让 / 恢复

- [x] 劣化判定 `evaluate_external_pool_degrade`（纯函数，`now_ms` 注入）
- [x] 避让状态 + 指数退避 + 上限（`base * 2^(n-1)`，封顶 `max_probation_secs`）
- [x] 避让期探测流量（probe share）
- [x] 恢复爬坡（线性衰减罚分，P0-a 已实现数学、P1 接入选择路径）
- [x] 配置项 + 校验（P0-b 已完成）
- [x] UI：质量采样 / 评分权重 / 劣化与恢复 三组（`ui/src/features/runtime/runtime-page.tsx`）
- [x] 可观测性：`ExternalPoolQualityView` 挂到 `ExternalPoolStatus`
- [x] **单元测试 15 项全部通过**（12 劣化/探测/爬坡 + 2 可观测性 + 1 前后端键契约）
- [x] **全量回归：2040 passed / 0 failed**

### P1 可观测性与 UI

- `ExternalPoolQualityView` 与内部 `ExternalPoolQualityState` **刻意分离**：
  内部用绝对时间戳，管理端要的是"还剩多少秒"这类相对量；
  分开后内部字段调整不会破坏管理端 API。
- 视图带 `scoring_active` 字段：样本不足时必须显式告诉运维
  "这个池的数据还没参与调度"，否则会误以为调度器在依据一份冷启动数据决策。
- **主开关关闭时不展示质量数据**：此时它既不参与调度，
  展示出来只会让运维误以为调度正在依据它决策。
- 前端 `clampFloat` 与既有 `toRatio` 分开：`toRatio` 上界是 0.99，
  而失败率阈值必须允许取到 1.0（表示"全失败才降级"），权重更是可以远大于 1。
- 前端对 `maxProbationSecs` 做了与后端一致的跨字段夹紧，
  避免用户提交一个必然被后端拒绝的组合。
- 新增 `quality_config_serializes_with_the_camel_case_keys_the_ui_sends`
  锁定前后端键契约：任一侧改名都会让设置被 serde 默认值**静默吞掉**，
  用户会以为保存成功了。该测试同时断言主开关默认为 `true`。

### 两套前端的分工（调研结论）

`ui/` 与 `admin-ui/` **都被 rust-embed 编译进二进制并对外提供**
（`src/admin_ui/router.rs:23/31`），admin-ui 不是遗留目录。
但外部池表单**只存在于 `ui/`**——admin-ui 对外部池配置只做透传与清洗，
不渲染任何外部池表单项。因此 admin-ui 只需补类型、默认值与清洗逻辑，无需补表单。

> admin-ui 初始 `tsc` 报 `@radix-ui/react-scroll-area` 缺失；该包已在
> `package.json` 与锁文件声明，按锁文件补齐本地依赖后 `pnpm build` 通过。

### ⚠️ P1 重大缺陷修复：避让会变成永久放逐

纯罚分方案有一个致命问题：`top_k = 3` 且有 3 个健康池时，被避让的池
**排在第 4 位，拿到的流量恰好是零**。而质量样本只来自真实流量，
于是它永远产生不了新样本来自证恢复——"临时降低优先级"退化成了永久放逐，
直接违背用户"不是真冷却"的要求。

修复：在 Top-K 加权随机**之前**加一条独立的探测分支，
按 `probe_share_percent` 概率直接从避让池中抽一个放行。
三条边界由测试锁定：

- `probe_share = 0` 时退化为硬避让（尊重配置）；
- **全员避让时不走探测分支**——此时避让罚分对所有池是常数，
  正常评分路径本身就公平，再抽一次反而丢掉质量信号；
- **探测流量不得跨优先级层**：低优先级的避让池不能借探测通道越层抢流量。

### P1 设计决策

- **劣化判定只看单池绝对失败率，不看中位数**：采样回调发生在请求结束时，
  此时拿不到全局视图。安全性由评分层保证——避让在评分中是**常数罚分**，
  全体一起被避让时相对顺序不变、候选集不清空，
  "上游整体故障"因此不会退化成"无池可用"。
- **避让期内不重复延长**：否则持续失败会让避让无限续期，
  池永远等不到到期后的恢复爬坡窗口。到期后才允许再次降级并递增层级。
- **判定复用采样返回的状态，不额外读 Redis**：
  `record_external_pool_quality_sample` 本就返回更新后的状态。
- **避让状态 TTL 必须覆盖"避让窗口 + 恢复爬坡"**：
  TTL 短于避让窗口会让状态随键过期被抹掉，降级形同虚设。

## 测试场景维度设计

- [x] 六维度矩阵已定稿：[quality-aware-scheduling-test-matrix.md](quality-aware-scheduling-test-matrix.md)
      D1 账号规模/优先级 · D2 质量信号形态 · D3 时间与恢复轨迹 ·
      D4 既有因子混合 · D5 并发量级 · D6 断言类型
- [x] 27 个 L2 用例（L2-01 ~ L2-27）按风险优先排定执行顺序
- [x] 测试基建调研完成，矩阵已按调研结果修正

### 调研结论（推翻了两个初始假设）

1. **不存在时间旅行**：全仓库零 `tokio::time::pause()`，且冷却与质量 EWMA 窗口
   都是 **Redis TTL 而非进程内定时器**——暂停运行时也不会让 TTL 前进。
   `now_ms` 注入只对通路 A 纯函数有效；通路 B 必须"压缩 TTL 配置 + 真实 sleep"。
2. **质量采样是 detached `tokio::spawn`**（已核实 `src/external_pool.rs:9100`）：
   wave 跑完后样本可能未落 Redis。必须新建 poll-until-converged 助手，
   **不能**用固定 sleep（既有测试的 150ms sleep 正是 flaky 来源）。

### 基建：可复用远多于预期

现成可抄的两个高并发多池调度测试：`tests.rs:7772`（3 池 128/轮湍流）、
`tests.rs:7922`（4 池三轮 wave）。它们用 **fake server 命中计数轮间差分**
断言流量转移，正是 `A-share` 的现成做法。

mock 账号夹具齐备（`PatternExternalMessagesFakeServer` 的
`always_success`/`always_fail`/`fail_first`/`intermittent`、`TurbulentExternalMessagesFakeServer`、
SSE 的 `ExternalStreamFakeServer`）。

**唯一必须扩展的 mock 能力**：给 `PatternExternalMessagesBehavior`（`tests.rs:908`）
加 `Delay(Duration)` 变体——D2 的 `slow-ttft`/`slow-total` 依赖它。

### 三个 flaky 来源（写用例时逐条规避）

1. `select_external_pool_candidate` 并列打散用**未播种 `fastrand`**
   （已核实 `src/external_pool.rs:10017`）→ 只用统计容差，不断言精确落点。
2. fire-and-forget 采样 → poll-until-converged。
3. 失败注入一律用 `deterministic_failure_percent(index, seed)`（`tests.rs:1080`），
   不用 `fastrand`，保证跨运行可复现。

环境变量是 `KIRO_RS_TEST_POSTGRES_URL` + `KIRO_RS_TEST_REDIS_URL`
（不是 `KIRO_RS_TEST_DATABASE_URL`）；`drop_test_schema()` 无 `Drop` 守卫，panic 会泄漏 schema。

## kiro.rs L2 真实调度测试（mock 账号 + 高并发）

执行顺序见矩阵 §4：**L2-14 开关关闭等价性必须最先通过**。

- [x] 测试基建：`SlowSuccess { delay }` mock 行为 + `slow_success` 构造器
- [x] 测试基建：`wait_for_quality_samples` 轮询收敛（替代固定 sleep）
- [x] 测试基建：`run_quality_wave` / `count_successes` / `l2_quality_config`
- [x] 并发量级 `L2_WAVE_SIZE = 256`（满足"不能只搞几并发"的硬性要求）
- [x] 3.4 既有因子混合组：开关关闭等价性、优先级硬分层、瞬态降权共存
- [x] 3.2 可用性底线组：全体劣化仍可用、唯一池不被饿死
- [x] 3.1 新增因子基线组：同层内按质量分流
- [x] 3.3 时间与恢复组：降级 → 探测 → 恢复全轨迹
- [x] **7 个 L2 用例连续两轮全绿**（真实 PG + Redis，256 并发）
- [ ] 3.5 压力与边界组 L2-25 ~ L2-27（余量，非阻塞）

### ⚠️ L2 捕获的真实缺陷：探测流量被既有连击挡在层外

**这是 L2 测试存在的全部意义**——通路 A 单测全绿，但真实链路上探测流量为 0。

被降级的池几乎总是**同时**背着既有的 `transient_failure_streak`。
连击会抬高有效优先级（`streak × 20`），把池挤出最优层；
而我最初的探测分支写在**分层过滤之后**，于是层内永远抽不到它。
两套降权机制相互独立，探测要救的恰恰是"两者都踩中"的池。

修复：探测候选改为在**分层过滤之前**采集，但必须区分两种"低优先级"：

| 情形 | 是否放行探测流量 | 理由 |
| --- | --- | --- |
| 用户配置的低优先级备用池 | ❌ 否 | 用户明确意图，主池可用时就该闲着 |
| 基础优先级相同、被连击降下去 | ✅ 是 | 系统自己打的惩罚，正是探测要解救的对象 |

判据是"把连击惩罚去掉后它是否属于最优层"（`base_priority == best_base_priority`）。
两个边界各有一个单测锁定：
`probe_traffic_reaches_a_pool_demoted_by_its_failure_streak` /
`probe_traffic_still_respects_user_configured_priority_for_demoted_pools`。

### 历史全量回归结论：8 个失败，0 个由本次改动引起

以下是早期脏工作区基线记录，不代表当前状态。2026-09-15 当前工作区完整 Rust 二进制
回归覆盖 `2055` 个非忽略测试：首轮为 `2054 passed / 1 flaky failure / 6 ignored`，
唯一失败用例隔离复跑为 `1 passed / 0 failed`；`kiro_loadtest` 为 `31/31`。其中下列
5 个曾经的本机红灯也已逐项复验通过。历史归因保留用于解释当时的测试夹具/平台差异，
不再作为当前阻塞。

`cargo test --bins` 全量跑出 8 个失败。逐一归因（用 git worktree 在
`4c7790b`（本次改动）与 `31c947b`（本次改动之前）两个提交上分别复跑）：

| 失败用例 | 归因 |
| --- | --- |
| `no_response_headers_becomes_client_timeout_without_raw_body` | **改动前既有** |
| `retry_send_timeout_uses_remaining_dispatch_deadline` | **改动前既有** |
| `slow_upstream_status_keeps_status_and_error_body_fragment` | **改动前既有** |
| `stream_keepalive_emits_ping_during_silent_gap_before_output` | **改动前既有** |
| `legacy_zero_external_wait_reaches_a_bounded_final_error` | **改动前既有** |
| `high_concurrency_random_mixed_status_turbulence_transfers_to_healthy_pools` | 工作区未提交的 keepalive/超时改动 |
| `mock_error_matrix_limits_repeated_failures_and_preserves_recovery` | 工作区未提交的 keepalive/超时改动 |
| `redis_external_pool_snapshot_and_acquire_are_atomic_across_managers` | 并发争用，`--test-threads=1` 下通过 |

关键证据：前 5 个在 `31c947b`（本特性尚不存在时）以**完全相同的方式失败**；
后 2 个在两个提交上都通过，只在含 keepalive 改动的脏工作区失败。
这些断言全部围绕 `retryable` / `attempts.len()` / 超时归类，
属于重试与超时语义，本特性只影响"在候选中选哪个池"，不触及该路径。

⚠️ 前 5 个是 base 分支上的既有红灯，**不属于本次范围**。

#### 历史既有红灯的进一步定位：是 macOS/Linux 平台差异，不是产品缺陷

用户要求"顺手修掉"，遂展开定位。关键证据：

1. 在**引入该测试的那个提交**（`2f38e53`）上直接复跑，
   `slow_upstream_status` 以**完全相同的方式失败**——
   这个测试在本机**从未通过过**，不存在"回归"一说。
2. CI 跑在 `ubuntu-24.04`（`.github/workflows/build.yaml:52`），
   且 `cargo test --locked --all-targets -- --test-threads=1` 是必跑步骤，
   说明这些用例**在 Linux 上是绿的**。
3. 失败根因是测试假服务器与客户端之间的 TCP 关闭语义：
   客户端拿到的错误链是
   `reqwest::Error{Decode} <- Body <- hyper <- ECONNRESET(os error 54)`。
   即 HTTP 头已成功解析（status 524 已拿到），但**响应体读取时连接被重置**，
   于是触发同池重试 → 假服务器已消费掉唯一一次 accept → 第二次连接失败，
   最终 `attempts.len()` 从期望的 1 变成 2。

已排除的假设（逐一实测证伪，避免后续重复走弯路）：

- ❌ 重试预算：`payload_guard_retry_config` 为 `None`，budget 确为 1
- ❌ 缺 `Content-Type` 头：补上后行为不变
- ❌ 请求未读完导致 RST：已改为完整 drain（实测读满 6910 字节到 JSON 结尾），行为不变
- ❌ reqwest 总超时打断读体：配置 2s，远大于 200ms 延迟
- ❌ `lease.wait_until_lost()` 抢占：该分支返回的是另一种错误
- ❌ 服务端写失败：实测 header 与 body 两次 `write_all` 均 `Ok`，
      且头部字节完全合法（`HTTP/1.1 524 Test\r\nContent-Length: 46\r\n...`）

尝试过的修法：`shutdown()` 半关闭 + 等待对端关闭后再 drop socket。
**在 macOS 上仍未修复**，已回滚，未留在工作区。

结论与建议：这 5 个是**测试夹具的平台可移植性问题**，不影响 CI、不影响生产逻辑。
真正的修法应是让假 HTTP 服务器在 macOS 上也能干净地完成"写完响应再关闭"，
或给这些用例加 `#[cfg_attr(target_os = "macos", ignore)]` 并注明原因。
考虑到它与本次特性无关且 CI 已覆盖，**建议单独排期处理，不阻塞 sub2api 工作**。

> 教训：后台命令若以 `| tail` 结尾，notification 里的 exit code 来自 `tail` 而非 `cargo`，
> 恒为 0。判定绿灯必须读 `test result:` 行，不能看 exit code。

### L2 调参过程中修正的三个测试自身缺陷

1. **等待窗口不足**：只等了新增的避让窗口（2s），
   却漏了既有的 `EXTERNAL_POOL_TRANSIENT_FAILURE_WINDOW_SECS = 30`（硬编码常量）。
   改为轮询"连击清零 + 避让结束"，不固定 sleep。
2. **mock 失败预算过大**（160）：失败渗进探测期与恢复期，池被反复重新降级，
   测的不再是"恢复"而是"上游还在坏"。改为 `FLAKY_FAILURE_BUDGET = 64`
   （< 阶段一打到该池的请求数），并显式断言恢复阶段开始前预算已耗尽。
3. **断言选错了性质**：先后试过"末轮 > 探测期份额"和"逐轮单调上升"，
   实测份额形如 `[137, 123, 130]/256`——池在第一轮就恢复到 ~50% 并在平价附近抖动，
   两个断言都必然 flaky（前者与 20% 固定配额量级相撞，后者没有上升空间）。
   最终断言"**回到与健康池平价**"（份额落在 25%~75%），这才是"恢复"的正确定义。

## P2 sub2api 质量调度

- [ ] openspec change 立项（proposal/design/tasks/specs）
- [x] **S1** 修复 `buildOpenAIAccountSchedulerScoreSnapshot` 硬编码 → `fb125c7d2`
- [x] **S2** 泛化质量因子到 Anthropic/Gemini 路径 → `4eae01456`
- [x] **S3** 观察期降级 + 指数退避 + 探测配额（Redis 状态）→ `4eae01456`
- [ ] **S4** settings 键 + DTO + admin API + 前端开关与权重
- [ ] `ReportResult` 回灌（FirstTokenMs 已有）

### S1 已完成（commit `fb125c7d2`）

`buildOpenAIAccountSchedulerScoreSnapshot` 原本把 errorRate/TTFT 硬编码为常量，
导致 admin 快照里这两个因子对所有账号相同、完全抵消（死因子）。现改为经
`OpenAIAccountSchedulerStatsSource` 接口读取真实 EWMA。
沿用既有 `SetAccountRuntimeBlocker` 反向注册模式，无需改 wire/DI。

### S2 已完成（commit `4eae01456`）

链路：`优先级 →（可选）最早重置 → 负载率 →（新）质量 → LRU`

**关键决策：软过滤而非硬过滤。** 开关默认开启，若做成硬过滤会直接改变所有既有
部署的行为；软过滤仅在质量差距超过 `minGap`(0.15) 时介入，否则原样返回交还 LRU。

- 错误率权重 0.7 > 首字 0.3 —— 满足"报错/重试多的账号判断优先级高于首字曲线"。
- 首字按候选集内 min/max **相对归一化**，上游整体变慢时不清空候选池。
- 无采样数据 → 中性分，避免"没数据→不调度→永远没数据"死锁。
- `filterByMinPriority` 硬分层不变：低优先级账号质量再好也不得越级（有测试钉死）。

**⚠️ 测试捕获的真实缺陷：相对归一化把噪声放大成满分差距。**
500ms 与 520ms 本质无差别，归一化后却变成 1.0 与 0.0，足以越过 minGap 触发误降级。
修复：引入 `qualityTTFTMinSpreadMS = 150` 跨度下限，低于该值一律按中性处理。
已补回归测试 `TestFilterByQualityIgnoresNoiseLevelTTFTSpread`。

### S3 已完成（commit `4eae01456`）

**观察期（软降级）必须与既有 `BlockAccountScheduling`（硬屏蔽）严格区分：**

| | BlockAccountScheduling（既有） | 质量观察期（新增） |
|---|---|---|
| 语义 | 硬屏蔽，完全不可调度 | 软降级，仍可调度只是排后 |
| 触发 | 429 / 鉴权失败等确定性故障 | 被动采样质量持续差 |
| 平台 | **仅 OpenAI/Grok**（`isOpenAIAccount` 门控） | 全平台 |
| 状态 | 进程内 `sync.Map` | **Redis**（多副本共享） |

多副本下状态必须放 Redis：进程内状态会让每个副本独立降级，探测流量变成 N 倍、
降级时长不可控。

- 迟滞阈值（进入 0.35 / 退出 0.15）避免阈值附近反复抖动。
- 指数退避 `base*2^(n-1)`，上限 30min；shift 与乘法**双重钳位**防溢出回绕
  （回绕会让退避时长反而变短，是这类实现的典型缺陷）。
- **观察期内不顺延结束时间** —— 否则持续报错会把软降级演变成永久封禁。
- 保留 5% 探测流量，且**任何退避等级下都开放**，保证账号始终有恢复路径。
- 恢复后重置退避等级，不永久惩罚曾出问题的账号。

### 测试与验证方法

32 个新增用例，全部通过；`internal/service` 全包回归 122s 通过，未破坏既有逻辑。

**用变异测试验证"非空跑"**（避免写出恒绿的无效测试）：分别注入三处缺陷，
确认被对应用例捕获后立即还原——

| 注入的缺陷 | 捕获它的测试 |
|---|---|
| 移除溢出钳位 | `DurationCappedAtMax` / `DurationNeverOverflows` |
| 移除观察期顺延防护 | `DoesNotExtendWhileActive` / `FullLifecycle` |
| 压缩迟滞区间 | `HysteresisBandHoldsState` |
| 整体禁用质量过滤 | 5 个 filterByQuality 用例变红（其余为不变量守卫，本应双向绿） |

### ⚠️ 环境约束（本机）

swap 曾达 22.7G/23.5G、go build cache 膨胀到 **67G**。此后所有 Go 命令一律加
`GOFLAGS="-p=2" GOMAXPROCS=2` 限制并行度，且**每轮测试后 `go clean -cache
-testcache` 并清理残留进程**。注意 `sub2api-kiro` 下有用户自己的 `air` 热重载
dev server（PID 随会话变化），**不要误杀**。

## P3 sub2api 复杂场景测试

- [ ] 同 kiro.rs 场景矩阵
- [ ] 与 sticky session 混合
- [ ] 与优先级硬门槛语义混合
- [ ] 与 rate limit / temp unschedulable 混合
- [ ] 与 failover 排除集合混合

---

## 决策记录

- 质量状态不做 Postgres 持久化（短窗口信号，重启重建）
- 降级兜底路径不做质量感知（Redis 不可用时质量数据本不可信）
- P0 拆 a/b：先采集观察，再影响调度

## 硬性要求（用户明确指定）

1. **必须充分测试**：每个阶段完成后立即补测试，不允许"先实现完再统一补"。
2. **每次动手前先加待办**：用 TaskCreate/TaskUpdate 跟踪，复杂任务先写计划文档。
3. **不得破坏既有逻辑**：新因子必须与既有因子做混合测试，主开关关闭时必须与旧行为逐字节等价。
4. **测试分两层，顺序不可颠倒**：
   - 第一层 **单元测试**：先保证基本正确性（EWMA 数学、判定边界、开关等价）。
   - 第二层 **真实调度测试**：mock 账号 + 真实调度链路跑流量，不是纯单元测试。
5. **真实调度测试必须是高并发**：1 并发 / 几并发的测试没有意义，
   必须达到能暴露分布、抖动、羊群效应的并发量级（见测试场景维度表）。
6. **必须先设计测试场景维度**，再写测试用例（维度表见下方"测试场景维度设计"）。
