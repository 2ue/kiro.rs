# 质量感知调度：测试场景维度设计

Last updated: 2026-09-15

设计文档：[external-pool-quality-aware-scheduling.md](external-pool-quality-aware-scheduling.md)
进度文档：[quality-aware-scheduling-progress.md](quality-aware-scheduling-progress.md)

---

## 0. 为什么要先设计维度

调度是一个**统计行为**，不是确定性行为。它的正确性不体现在"某一次请求选了哪个账号"，
而体现在"N 次请求在账号间的分布是否符合预期"。因此：

- **低并发测试没有意义**。1~2 并发下，Top-K 加权随机、负载均衡、羊群效应、
  避让抖动全部观察不到；测试会在"恰好选中了预期账号"上通过，却在生产上翻车。
- **必须先定维度再写用例**。否则会写出一堆互相重叠的用例，却漏掉真正危险的组合
  （例如"全部账号劣化"与"Redis 降级"同时发生）。

测试分两层，顺序不可颠倒：

| 层 | 目的 | 手段 | 判定 |
|---|---|---|---|
| L1 单元测试 | 基本正确性 | 纯函数 + Redis Lua 往返 | 精确值断言 |
| L2 真实调度测试 | 统计行为正确性 | mock 账号 + 真实调度链路 + 高并发 | 分布/区间断言 |

L1 已完成（15 项，全绿）。本文件定义 L2 的维度。

---

## 1. 六个正交维度

L2 用例 = 在这六个维度上各取一个值的组合。不追求笛卡尔全覆盖（会爆炸），
而是按"风险优先"挑选组合，见 §3 用例矩阵。

### D1 账号规模与优先级分布

| 取值 | 说明 | 为什么要测 |
|---|---|---|
| `D1-a` | 3 账号，同优先级 | 最小可观测分布的规模 |
| `D1-b` | 5 账号，优先级 1/1/2/2/3 | 验证优先级分层未被质量分打穿 |
| `D1-c` | 10 账号，优先级全不同 | 验证大候选集下 Top-K 不退化成单点 |
| `D1-d` | 2 账号 | 边界：Top-K > 候选数 |
| `D1-e` | 1 账号 | 边界：唯一账号劣化也必须继续被选中（**可用性底线**） |

### D2 质量信号形态（新增因子）

每个账号可独立配置，这是本次变更的核心因子。

| 取值 | mock 行为 | 预期效果 |
|---|---|---|
| `D2-healthy` | 200 OK，首字 200ms，总耗时 1s | 基准 |
| `D2-slow-ttft` | 200 OK，首字 5s，总耗时 6s | 首字曲线劣化 → 降权 |
| `D2-slow-total` | 200 OK，首字 200ms，总耗时 30s | 耗时曲线劣化 → 降权（**但首字正常**） |
| `D2-erroring` | 50% 返回 500 | 失败率主导，**优先级高于延迟** |
| `D2-hard-fail` | 100% 返回 500 | 触发避让 |
| `D2-rate-limited` | 429 + Retry-After | 与既有冷却路径交互 |
| `D2-downstream-fault` | 400 "input is too long" | **不得**污染健康度（已在 L1 覆盖，L2 验证端到端） |
| `D2-flapping` | 交替成功/失败 | 验证 EWMA 不产生剧烈抖动 |

> **关键分离**：`slow-ttft` 与 `slow-total` 必须分开测。
> 用户明确要求区分"首字高"与"耗时长"，两者权重不同，
> 合并测试会掩盖其中一个权重配错。

### D3 时间与恢复轨迹

这一维度决定测试必须能**控制时间**，否则"很久才恢复"要跑十几分钟。

| 取值 | 轨迹 | 断言重点 |
|---|---|---|
| `D3-steady` | 全程稳定 | 分布稳定，无抖动 |
| `D3-fast-recover` | 劣化 30s 后立刻恢复健康 | 避让到期后按**爬坡**回流，不是瞬间打满 |
| `D3-slow-recover` | 劣化持续 10min 后恢复 | 指数退避生效，避让时长递增但**不超过上限** |
| `D3-never-recover` | 全程劣化 | 避让被反复续期，且**不得**把账号彻底踢出（探测流量仍要到达） |
| `D3-recover-then-fail` | 恢复后立刻再次劣化 | 退避层级不被恢复重置得太快（防止震荡） |

### D4 既有因子混合（防回归）

**这是最重要的维度**——用户明确要求"不要破坏了之前的逻辑"。

| 取值 | 既有机制 | 交互风险 |
|---|---|---|
| `D4-cooldown` | 硬冷却 | 冷却中的账号不应出现在候选集；质量分不得让它复活 |
| `D4-auto-disable` | 自动禁用 | 禁用优先级高于一切质量信号 |
| `D4-capacity` | `max_concurrent_requests` / 租约 / 全局并发 | 满载账号不得因质量分高而被硬塞 |
| `D4-model-routing` | `supported_models` / 模型映射 | 不支持该模型的账号不进候选集 |
| `D4-same-pool-retry` | 同池重试 | 重试不应被记成多次独立质量样本而过度惩罚 |
| `D4-cross-pool-retry` | 跨池重试 | 失败转移后原账号被采样、新账号也被采样 |
| `D4-local-first` | 本地优先 + 一次本地救援 | 外部池全劣化时本地兜底路径不受影响 |
| `D4-redis-degraded` | Redis 不可用 | 质量数据不可得 → **退化为旧行为**，绝不阻塞 |
| `D4-usage` | 用量计算 | 质量采样不得影响计费/用量口径 |
| `D4-toggle-off` | 主开关关闭 | **与旧行为逐字节等价** |

### D5 并发量级

用户明确要求：1 并发/几并发的测试没有意义。

| 取值 | 并发 | 总请求 | 用途 |
|---|---|---|---|
| `D5-low` | 1 | 10 | **仅用于**确定性断言（如"唯一账号必被选中"），不作为主力 |
| `D5-mid` | 32 | 2,000 | 分布断言主力档 |
| `D5-high` | 128 | 10,000 | 羊群效应 / 抖动 / 尾延迟 |
| `D5-burst` | 0→256 瞬时 | 5,000 | 冷启动无样本时不得雪崩到单一账号 |

统计断言需要足够样本量：判定"账号 A 的份额显著高于 B"，
在 2,000 次请求下才有足够的置信度；因此 `D5-mid` 是最低主力档。

### D6 断言类型

每个用例必须明确它**证明了什么**，避免写出只会"跑通不报错"的测试。

| 取值 | 断言形式 | 示例 |
|---|---|---|
| `A-share` | 份额区间 | 健康账号份额 ∈ [0.55, 0.85]，劣化账号 ∈ [0.02, 0.20] |
| `A-order` | 相对序 | share(healthy) > share(slow) > share(erroring) |
| `A-floor` | 可用性下限 | 任一账号份额 > 0（**永不饿死**），失败请求数 = 0 |
| `A-no-starve` | 探测流量 | 避让账号仍收到 ≈ probe_share_percent 的流量 |
| `A-equiv` | 等价性 | 开关关闭时分布与基线**统计等价** |
| `A-invariant` | 不变量 | 并发数 ≤ max_concurrent、样本数无丢失、无 panic |
| `A-latency` | 端到端 | p95 不因质量调度而劣化 |

---

## 2. 测试基建：分两条通路

调研既有代码后确认，L2 不需要"全链路真实 HTTP"才能做高并发调度测试。
关键发现：

> `select_external_pool_candidate(Vec<(ExternalPool, u32, u32)>, &ExternalPoolsConfig)`
> （`src/external_pool.rs:9977`）是一个**纯函数**，
> 且已有现成的池夹具 `test_pool(base_url, preserve_path)`
> （`src/external_pool/tests.rs:11375`）。
> 既有选择测试 `src/external_pool/tests.rs:3223` 已经直接调用它。

因此拆成两条通路，各司其职：

### 通路 A：调度器级高并发分布测试（**主力**）

- **驱动方式**：直接以 mock 账号 + mock 质量状态调用选择函数，
  跑 2,000 ~ 10,000 次，统计落点分布。
- **覆盖**：D1 / D2 / D3 / D5 / D6 的绝大部分，以及 D4 中
  cooldown / auto-disable / capacity / toggle-off 等可用入参表达的因子。
- **优点**：无 Postgres、无网络、无真实等待；毫秒级跑完上万次；
  时间轴可直接用参数注入（`now_ms`），**在这条通路上**解决 D3 的"10 分钟后恢复"。
- **代价**：不覆盖真实转发/重试/用量链路。
- ⚠️ **陷阱**：`select_external_pool_candidate` 现有实现用
  **未播种的 `fastrand::usize(..)`** 做同分并列打散（`src/external_pool.rs:10016`）。
  分布断言只能用统计容差区间，**不能**断言精确落点或精确计数。

> 这条通路能做到用户要求的"高并发、有统计意义"。
> 注意 `now_ms` 注入只在**这条通路**上等价于时间旅行——见下方通路 B 的重要修正。

### 通路 B：端到端链路测试（**补充**）

- **驱动方式**：既有 `test_external_pool_manager()`（`src/external_pool/tests.rs:305`）
  + **真实 Postgres 与 Redis 双依赖**。
  环境变量是 `KIRO_RS_TEST_POSTGRES_URL` 和 `KIRO_RS_TEST_REDIS_URL`
  （**不是** `KIRO_RS_TEST_DATABASE_URL`）。无变量时静默跳过；
  `KIRO_RS_REQUIRE_STORAGE_TESTS=1` 可让 CI 强制要求。
- **覆盖**：D4 中必须走真实链路才有意义的因子——
  same-pool retry / cross-pool retry / usage 口径 / downstream-fault 端到端 /
  Redis 降级。
- **并发**：中等即可（这条通路验证的是**正确性**而非**分布**）。
- **收尾**：成功路径最后必须 `postgres.drop_test_schema().await.unwrap()`。
  注意它**没有** `Drop` 守卫，测试 panic 会泄漏 schema。

### ⚠️ 重要修正：通路 B 不存在时间旅行

调研确认全仓库 **零** `tokio::time::pause()`，没有任何时钟抽象。
更关键的是：

> **冷却与质量 EWMA 窗口都是 Redis TTL，不是进程内定时器。**
> 即使暂停 tokio 运行时，Redis 的 TTL 也不会前进。
> `now_ms` 注入只对通路 A 的纯函数有效，对通路 B **完全无效**。

因此 D3 在两条通路上手段不同：

| 通路 | D3 手段 |
|---|---|
| A | `now_ms` 作为入参直接注入，任意跳跃，零等待 |
| B | **压缩 TTL 配置 + 真实 sleep**：把 `quality_sample_ttl_secs`、各类 `cooldown_secs` 压到 1 秒，再真实 sleep ~1.2s。既有测试已有先例（`tests.rs:1358`、`:1394`） |

`D3-never-recover` 在通路 B 上用 `always_fail` + 多轮 wave 断言命中数始终有界来表达，
不需要真的等很久。

### ⚠️ 质量采样是 fire-and-forget，断言前必须收敛等待

`record_pool_quality_sample`（`src/external_pool.rs:9087`）是 detached `tokio::spawn`。
一个 wave 跑完后，质量样本**可能尚未落 Redis**。
既有测试用固定 `sleep(150ms)` 掩盖这一点，但那是 flaky 的来源。

**必须新建一个 poll-until-converged 助手**（仿照既有
`wait_for_static_pool_pg_loads`，`tests.rs:3969`），轮询直到样本数达到预期再断言。

### 基建：复用 vs 新建

**可直接复用**（全在 `src/external_pool/tests.rs`）：

| 用途 | 标识符 | 位置 |
|---|---|---|
| 纯内存池夹具 | `test_pool(base_url, preserve_path)` | `:11375` |
| 路由夹具 | `test_route(model)` | `:11931` |
| 多池建库 | `create_messages_pool_with_concurrency(...)` | `:1307` |
| mock 账号（成功/失败/前 N 次失败/按比例间歇） | `PatternExternalMessagesFakeServer`：`always_success` / `always_fail` / `fail_first` / `intermittent` | `:942` |
| mock 账号（混合状态码湍流） | `TurbulentExternalMessagesFakeServer::start` | `:831` |
| mock SSE 流 | `ExternalStreamFakeServer` + `ExternalStreamFakeStep::{chunk,delay,error}` | `:658` / `:627` |
| **可复现**随机 | `deterministic_failure_percent(index, seed)` | `:1080` |
| 高并发 wave 驱动闭包 | `run_batch` / `run_wave` 惯用法 | `:7838` / `:7996` |
| 分位数统计 | `external_perf_percentile_micros` | `:2164` |
| 真·同时爆发 | `tokio::sync::Barrier` 惯用法 | `:3978` |
| ≥10k 操作的分批扇出形状（120×84） | — | `redis_cache.rs:10840` |

> **两个现成的高并发多池调度测试几乎就是我们要的形状**，可近乎照抄：
> `external_pool_high_concurrency_random_mixed_status_turbulence_...`（`:7772`，3 池、128/轮）
> 与 `external_pool_mock_error_matrix_...`（`:7922`，4 池、三轮 wave）。
> 它们通过**对 fake server 命中计数做轮间差分**来断言流量转移——这就是 `A-share` 的现成做法。

**必须新建**：

1. **可控延迟的 mock 上游**：现有 axum fake server **都不支持在普通 JSON 响应上按请求配置延迟**
   （只有 SSE 的 `ExternalStreamFakeStep::delay` 支持块间延迟，
   而 `spawn_test_raw_http_response::DelayedFixed` 是**单连接**的，并发下不可用）。
   D2 的 `slow-ttft` / `slow-total` 依赖它。
   需给 `PatternExternalMessagesBehavior` 加 `SlowSuccess { delay }` 变体。
   **这是唯一必须扩展的 mock 能力。**
2. **poll-until-converged 助手**（见上）。
3. **分布断言助手**：既有测试只断言粗边界（`<= 4`、`>= 220`），
   没有任何跨 N 个健康池的份额分布计算。
4. **并发量级突破 128**：外部池测试套件中**没有任何单轮超过 128 并发**的用例
   （10k 那个数字是 Redis 租约专属，且按 120/批分了 84 批）。
   `D5-high`(128) 是现成的；`D5-burst`(256) 需自建，参考
   `redis_cache.rs:10840` 的分批扇出形状。

### 更好的选择探针（修正 §2 早前结论）

`ExternalPoolManager::pool_availability_snapshot(&excluded, &config)`
（`src/external_pool.rs:7789`，**已 `#[cfg(test)]` 门控**）返回
`PoolSelectionSnapshot { selected_pool, availability, degraded_fallback_local_lease }`
（`:4323`）——**直接给出被选中的池，不必转发请求**。

这比"fake server 命中计数差分"更精确，且不受重试放大影响：
- **纯分布断言**（D6 的 `A-share`/`A-order`）→ 用 `pool_availability_snapshot` 循环采样
- **端到端行为断言**（重试、故障转移、用量）→ 才用命中计数差分

> 注意它仍是 `&self` 方法，需要构造好的 manager，因此 **Postgres + Redis 依旧是硬依赖**。
> 真正零依赖的只有 `select_external_pool_candidate` 纯函数（通路 A）。

### 其它易踩的坑

- `test_route` 默认 `recorder: UsageRecorder::new(1)`——**环形缓冲区容量为 1**。
  高并发下若要查用量记录（D4-usage），必须显式换成 `UsageRecorder::new(N)`。
- 既有湍流测试把 `transient_failure_cooldown_threshold` 设为 `0` 来保持失败"软化"
  （只影响排序、永不硬冷却），把 `transient_failure_priority_penalty` 设为 `20`
  （一次失败压过 20 点优先级差）。做 D4 混合测试时要有意识地选择这两个值。
- `manager.with_static_pool_snapshot_timing(refresh, ttl, stale, background)`
  （`tests.rs:3951`）是**唯一**可注入的计时参数，用于压缩快照 SWR 缓存。

### 仍待确认

- [ ] 份额断言容差：先跑基线取经验值，避免 flaky（见 §5）

---

## 3. 用例矩阵（按风险优先，非全笛卡尔）

> 记号：`用例 = D1 / D2 / D3 / D4 / D5 / 断言`

### 3.1 新增因子基线组（证明质量调度有效）

| # | 组合 | 断言 |
|---|---|---|
| L2-01 | D1-b / 全 healthy / steady / — / D5-mid | `A-share` 按优先级分层；同层内均匀 |
| L2-02 | D1-a / 1×healthy + 2×slow-ttft / steady / — / D5-mid | `A-order` healthy 份额显著最高 |
| L2-03 | D1-a / 1×healthy + 2×slow-total / steady / — / D5-mid | `A-order`；且与 L2-02 的份额**不同**（证明两个权重独立生效） |
| L2-04 | D1-b / 2×erroring + 3×healthy / steady / — / D5-mid | `A-order` 失败率主导，erroring 份额 < slow 份额 |
| L2-05 | D1-b / 混合 slow + erroring / steady / — / D5-high | **失败率优先级 > 延迟**：erroring 排在 slow 之后 |

### 3.2 可用性底线组（证明"降级时保证可用"）

| # | 组合 | 断言 |
|---|---|---|
| L2-06 | D1-c / **全部 hard-fail** / never-recover / — / D5-high | `A-floor` 候选集**永不为空**；相对中位数保护生效 |
| L2-07 | D1-e / 唯一账号 hard-fail / never-recover / — / D5-mid | `A-floor` 唯一账号必被继续选中 |
| L2-08 | D1-c / 全部 slow-total / steady / — / D5-mid | 全体劣化 ⇒ 无人被降级（相对比较，非绝对阈值） |
| L2-09 | D1-b / 9 健康 + 1 hard-fail / never-recover / — / D5-high | `A-no-starve` 劣化账号仍收到探测流量 |

### 3.3 时间与恢复组

| # | 组合 | 断言 |
|---|---|---|
| L2-10 | D1-a / 1×hard-fail / fast-recover / — / D5-mid | 爬坡回流，**不瞬间打满** |
| L2-11 | D1-a / 1×hard-fail / slow-recover / — / D5-mid | 指数退避递增，且 ≤ `max_probation_secs` |
| L2-12 | D1-a / 1×flapping / recover-then-fail / — / D5-high | 无震荡：份额变化平滑，避让层级不被过早重置 |
| L2-13 | D1-b / 部分 hard-fail / never-recover / — / D5-high | 长期劣化下分布稳定收敛，不持续抖动 |

### 3.4 既有因子混合组（防回归，**最高优先级**）

| # | 组合 | 断言 |
|---|---|---|
| L2-14 | D1-b / 混合 / steady / **D4-toggle-off** / D5-high | `A-equiv` 与基线统计等价 ← **必须最先通过** |
| L2-15 | D1-b / 混合 / steady / D4-cooldown / D5-mid | 冷却账号零流量；质量分不得复活它 |
| L2-16 | D1-b / 混合 / steady / D4-auto-disable / D5-mid | 禁用优先于质量分 |
| L2-17 | D1-b / 全 healthy / steady / D4-capacity / D5-high | `A-invariant` 并发不超限；高分账号不被超发 |
| L2-18 | D1-c / 混合 / steady / D4-model-routing / D5-mid | 不支持模型的账号零流量 |
| L2-19 | D1-a / erroring / steady / D4-same-pool-retry / D5-mid | 同池重试不被重复计为多次惩罚 |
| L2-20 | D1-b / erroring / steady / D4-cross-pool-retry / D5-mid | 转移后两端采样都正确 |
| L2-21 | D1-c / 全 hard-fail / never-recover / D4-local-first / D5-mid | 本地兜底不受影响 |
| L2-22 | D1-b / 混合 / steady / **D4-redis-degraded** / D5-high | 退化为旧行为，零失败，不阻塞 |
| L2-23 | D1-b / 混合 / steady / D4-usage / D5-mid | 用量口径不变 |
| L2-24 | D1-b / downstream-fault / steady / — / D5-mid | 下游错误端到端不污染健康度 |

### 3.5 压力与边界组

| # | 组合 | 断言 |
|---|---|---|
| L2-25 | D1-c / 全 healthy / — / — / **D5-burst** | 冷启动无样本 ⇒ 不雪崩到单账号 |
| L2-26 | D1-d / 1 healthy + 1 slow / steady / — / D5-high | Top-K > 候选数的边界 |
| L2-27 | D1-c / 混合 / steady / — / D5-high | `A-latency` p95 不劣化；`A-invariant` 无 panic/死锁 |

---

## 4. 执行顺序

1. **L2-14（开关关闭等价性）必须最先通过**——它是"不破坏既有逻辑"的总闸门。
   在它通过前，其余 L2 用例的结果都不可信。
2. 然后 3.4 其余混合组（防回归）。
3. 然后 3.2 可用性底线组（用户的核心诉求"降级时保证可用"）。
4. 最后 3.1 / 3.3 / 3.5（证明新功能有效 + 压力边界）。

> 注意顺序刻意把"防回归"和"保可用"排在"证明新功能好用"之前：
> 新功能不生效只是没收益，旧逻辑被破坏或可用性丢失是**事故**。

---

## 5. 待确认项

- [x] mock 上游基建 → 大量可复用，仅需加 `Delay(Duration)` 一项（见 §2）
- [x] 虚拟时钟可行性 → **不可行**，改用"压缩 TTL + 真实 sleep"（见 §2 重要修正）
- [x] Postgres 是否为 L2 硬依赖 → 通路 A 否、通路 B 是（且 Redis 也是硬依赖）
- [x] 选择探针 → 通路 A 不需要；通路 B 用 fake server 命中计数差分
- [ ] 份额断言的容差区间（需先跑基线取经验值，避免 flaky）

### 防 flaky 清单

本次调研暴露了三个具体的 flaky 来源，写用例时必须逐条规避：

1. **未播种的 `fastrand` 并列打散** → 只用统计容差，不断言精确落点。
2. **fire-and-forget 质量采样** → 断言前 poll-until-converged，不用固定 sleep。
3. **真实随机的失败注入** → 一律用 `deterministic_failure_percent(index, seed)`，
   不用 `fastrand`，保证跨运行可复现。

另有一条非 flaky 但会拖垮后续测试的隐患：
**`drop_test_schema()` 无 `Drop` 守卫**，panic 会泄漏 Postgres schema。
