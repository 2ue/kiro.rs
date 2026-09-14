# 外部账号质量感知调度（被动采样）方案

Status: `design / implementation-pending`

Last reviewed: 2026-09-15 Asia/Shanghai

适用范围：`kiro.rs`（外部池）与 `sub2api`（Anthropic/Gemini 调度路径）。

Related:

- [外部池高可用调度执行计划](external-pool-ha-scheduler-execution-plan-20260805.md)
- [当前符合度矩阵](scheduler-target-compliance-matrix.md)（本方案落地 S-09、S-10）
- [Decision 001：本地账号与外部池统一调度目标契约](../decisions/001-local-external-scheduler-target-contract.md)

---

## 0. 结论先行

一句话目标：**尽可能最优，降级时保证可用。**

翻译成可执行的调度语义：

> 在一组健康候选里，按"被动观测到的质量"倾斜流量；当质量变差时**降权**而不是**摘除**；只有在"足够多、足够独立、足够持续"的失败证据下才临时避让；任何避让都必须能自动恢复。

**重要前提（先纠正一个说法）**：你描述里的"冷却调度（不是真冷却，相当于临时调度优先级变低）"是准确且正确的直觉，本方案完全采用它。但需要明确区分三个**不同强度**的状态，不能混为一谈——这是整个方案能否"降级时保证可用"的关键：

| 状态 | 触发 | 效果 | 恢复 |
| --- | --- | --- | --- |
| **健康降权**（soft） | 质量分下滑 | 连续降低被选中概率，**仍然可被选中** | 质量回升即自动恢复，无需等待 |
| **临时避让**（probation） | 短窗口内多次独立失败 | 大幅降权 + 限流探测，**保留少量探测流量** | 到期自动恢复，探测成功后逐步回流 |
| **冷却**（cooldown，已存在） | 429/5xx 等明确信号且连续达阈值 | 完全不可调度 | 到期或手动清除 |

**为什么必须保留"仍然可被选中"**：如果降权等价于摘除，那么全部账号都变差时（上游整体抖动）就会无池可用，直接违背"降级时保证可用"。降权只改变**相对倾向**，绝对可用性由"至少保留最优的那个"兜底。

---

## 1. 关键发现：两个系统都已有一半实现

这是本方案最重要的工程结论——**不需要从零设计，而是把已经验证过的模型补齐到缺失的那条路径上**。

### 1.1 kiro.rs

| 能力 | 本地账号调度 | 外部池调度 |
| --- | --- | --- |
| 加权评分函数 | ✅ `src/kiro/token_manager/strategy.rs:33` `scheduler_score_with_config` | ❌ 只有 `优先级 + streak 罚分 + 负载` 的字典序排序 |
| 延迟 EWMA | ✅ `SchedulerHealthState.latency_ewma_ms` | ❌ 无 |
| 错误率 EWMA | ✅ `recent_error_rate` | ❌ 只有整数 `transient_failure_streak` |
| 临时避让 | ✅ `probation_until_ms` + `scheduler_probation_weight` | ❌ 只有硬冷却 |
| 首字延迟采集 | — | ✅ **已采集** `src/external_pool.rs:1018` `first_token_latency_ms`，已落库 |

外部池现状选择函数 `select_external_pool_candidate`（`src/external_pool.rs:9915`）：

```rust
let a_effective_priority =
    a.priority.max(0) as u64 + (*a_streak as u64).saturating_mul(transient_failure_penalty);
a_effective_priority.cmp(&b_effective_priority)
    .then_with(|| a_load.cmp(&b_load))
    .then_with(|| a.id.cmp(&b.id))
```

问题正是符合度矩阵 S-10 标记的"不符合"：**字典序**意味着质量维度永远无法参与决策——只要有效优先级差 1，负载和质量就完全不被考虑。

**结论：把本地账号已验证的加权评分模型移植到外部池。** 首字延迟数据已经在采集，无需新增埋点。

### 1.2 sub2api

| 能力 | OpenAI 路径 | Anthropic/Gemini 路径 |
| --- | --- | --- |
| 加权评分 | ✅ `openai_account_scheduler.go:1013` 八因子加权 | ❌ `gateway_scheduling.go:738` 字典序级联过滤 |
| 错误率 / TTFT EWMA | ✅ `openAIAccountRuntimeStats` (alpha=0.2) | ❌ 无 |
| 质量反馈回路 | ✅ `ReportResult(accountID, success, firstTokenMs)` | ❌ **`FirstTokenMs` 已算出并落库，但从不回灌调度器** |
| 配置化权重 + UI | ✅ 10 个权重已可在后台编辑 | ❌ 无 |

**结论：把 OpenAI 路径的评分器泛化到 Anthropic/Gemini 路径。** TTFT 在 `gateway_upstream_response.go:1101` 已经算出，只差一次 `ReportResult` 调用。

---

## 2. 质量信号设计

### 2.1 为什么首字延迟需要"分模型、分流式"归一化

直接比较原始首字毫秒数是错误的：`haiku` 的首字天然快于 `opus`，非流式请求的"首字"实际是整包返回。若不归一化，调度会系统性地偏向跑小模型的账号，与账号质量无关。

**采用相对化处理**：对每个 `(账号, 模型, 是否流式)` 维度维护 EWMA，评分时与**同一维度下所有候选的中位数**比较，只使用相对比值：

```
ttft_factor = clamp(ttft_ewma / median_ttft_across_candidates, 0.5, 4.0)
```

这样"慢"始终意味着"比同场景下的其他账号慢"，而不是"比某个绝对阈值慢"。sub2api 的 OpenAI 评分器已用 min-max 归一化达成类似效果（`openai_account_scheduler.go:982-1011`），kiro.rs 侧沿用同样思路。

样本不足时（`< min_samples`，默认 5）使用中性值，**不惩罚也不奖励**——避免新账号或冷启动账号因无数据被永久边缘化。这是 sub2api 现有实现的 `默认 0.5` 语义。

### 2.2 失败权重必须高于延迟权重

你的要求是明确的："如果有些账号重试次数或者报错次数很多，这个的判断优先级要高于首字等质量曲线"。

实现方式不是做成两级排序（那会退回字典序的老问题），而是**在同一个加权和里给失败项显著更大的权重**。沿用 kiro.rs 本地调度器已校准的量级（`src/model/config.rs:4155-4180`）：

```
error_weight   = 100.0     // 错误率（0..1）满值贡献 100
probation_weight = 50.0    // 临时避让
load_weight    = 100.0     // 负载率（0..1）
latency_weight = 0.01      // 每毫秒 0.01 → 1000ms 贡献 10
priority_weight = 1.0      // 每级优先级贡献 1
```

量级关系是刻意的：**错误率从 0 涨到 30% 贡献 30 分，相当于 3000ms 的首字劣化或 30 级优先级差**。失败信号天然压过延迟信号，同时两者仍在一个连续空间里可比——不会出现"错误率 1% 就把一个快 10 倍的账号永久排到后面"的硬切换。

**重试次数纳入错误率**：把"同池重试"记为一次部分失败（权重 0.5 的错误样本），而不是单独维度。重试多 = 错误率高 = 自然降权，无需额外参数。

### 2.3 不污染健康度的错误类别

沿用已确认的执行口径（HA 执行计划 §4.2）：**下游请求自身的问题不计入账号健康**。

- 请求体非法、工具 schema 非法、输入超长 → 不记错误样本，不降权，不换池
- 客户端主动断开 → 不记错误样本
- 上游 4xx/5xx、网络错误、协议错误、超时 → 记错误样本
- **慢成功（首字慢但最终成功）→ 只影响延迟分，绝不触发错误冷却**（HA 计划明确要求）

---

## 3. 评分模型

### 3.1 统一公式

对每个候选账号计算分数（**分数越低越优先**，与 kiro.rs 本地调度器一致）：

```
score = priority        * W_priority          //  静态优先级，运营意图
      + load_rate       * W_load              //  当前负载率 0..1
      + error_rate      * W_error             //  失败 EWMA 0..1
      + ttft_relative   * W_latency_rel       //  相对首字，中位数=1.0
      + probation       * W_probation         //  是否处于临时避让 0/1
      + selection_press * W_selection_pressure//  公平性，防止赢家通吃
```

前四项分别对应你提出的四个诉求：运营优先级、负载、报错率、首字质量。第五项是避让状态，第六项防止单账号被打爆。

### 3.2 从分数到选择：Top-K + 加权随机

**不能直接选最低分**。原因是"惊群"：所有并发请求在同一瞬间看到同一份快照，会全部涌向同一个账号，把最优账号瞬间打成最差账号，引发振荡。

采用 sub2api OpenAI 路径已验证的做法（`openai_account_scheduler.go:704`、`:794`）：

1. 取分数最优的 **Top-K**（默认 K=3）
2. 在 K 个里按 `weight = (max_score - score) + 1.0` **加权随机**

效果：最优账号拿到最大份额但不是全部；次优账号持续获得真实流量，因此其质量数据保持新鲜——这对"故障账号恢复后能被发现"至关重要。若次优账号完全无流量，就永远不会有新样本证明它已恢复。

### 3.3 恢复与逐步回流

避让到期后**不直接全量回流**（HA 计划 §5.2 明确要求"先少量探测，再逐步回流"）。

实现：避让到期后账号进入 `warmup` 状态，持续 `recovery_ramp_secs`（默认 60 秒）。其间在评分上附加一个线性衰减的额外罚分，从满值线性降到 0。流量随时间平滑爬升，而不是阶跃。若 warmup 期间再次失败，直接回到避让状态并**加倍避让时长**（指数退避，上限 `max_probation_secs`）。

---

## 4. 时间参数设计

你要求"加时间参数"并让我给出设计。以下是各时间常量及其取值理由。

| 参数 | 默认值 | 作用 | 取值理由 |
| --- | --- | --- | --- |
| `quality_ewma_alpha` | `0.2` | EWMA 平滑系数 | 约等于最近 ~10 次请求的有效窗口。sub2api OpenAI 路径已用此值并经生产验证 |
| `quality_sample_ttl_secs` | `600` | 样本过期 | 10 分钟无请求则质量数据视为陈旧，回到中性值，避免用半小时前的数据做决策 |
| `quality_min_samples` | `5` | 生效门槛 | 少于 5 个样本不参与质量评分，防止单次抖动造成误判 |
| `degrade_window_secs` | `120` | 持续劣化判定窗口 | **对应你说的"如果长时间质量差"**。2 分钟足以区分瞬时抖动与真实劣化 |
| `degrade_probation_secs` | `180` | 触发后的避让时长 | **对应你说的"直接对账号降级多少分钟"**。3 分钟，短到能快速恢复，长到能跳过一波故障 |
| `max_probation_secs` | `900` | 避让上限 | 连续劣化指数退避的封顶，15 分钟 |
| `recovery_ramp_secs` | `60` | 恢复爬坡 | 避免优先级 1 的账号恢复后瞬间重新打满（HA 计划明确列为风险项"优先级抢占"） |
| `probe_share_percent` | `5` | 避让期探测流量 | 保留 5% 流量做被动探测，**这是"不是真冷却"的具体落地** |

**触发避让的判据（必须同时满足，防止误伤）**：

```
在 degrade_window_secs 窗口内：
    样本数 >= quality_min_samples
AND error_rate >= degrade_error_rate_threshold (默认 0.5)
AND 该账号 error_rate 明显劣于候选集中位数（避免全局故障时把所有账号都避让）
```

最后一条是关键的**相对性保护**：当上游整体故障时，所有账号错误率都高，此时不应把任何账号打入避让——否则就无账号可用。只有"明显比同伴差"才避让。这直接保证了"降级时可用"。

---

## 5. 开关与配置项

### 5.1 主开关

`external_pool_quality_aware_scheduling_enabled`，**默认 `true`**（按你的要求）。

关闭时行为：完全退回当前的 `优先级 → 负载` 字典序排序，不读也不写质量状态。这提供了一条确定的回退路径——线上若出现非预期调度行为，关掉开关即恢复旧行为，无需回滚版本。

### 5.2 完整配置清单

分三组，对应后台"外部池 → 外部账号策略 → 调度策略"下的新增 `FormSection`（kiro.rs UI 约定见 `ui/src/features/external-pools/external-pools-page.tsx:469`）。

**组一：质量采样（`质量采样`）**

| 页面文案 | 字段 | 默认 |
| --- | --- | --- |
| 启用质量感知调度 | `..._quality_aware_scheduling_enabled` | 开启 |
| 平滑系数 | `..._quality_ewma_alpha` | 0.2 |
| 样本有效期 | `..._quality_sample_ttl_secs` | 600 秒 |
| 最少样本数 | `..._quality_min_samples` | 5 |

**组二：评分权重（`评分权重`）**

| 页面文案 | 字段 | 默认 |
| --- | --- | --- |
| 优先级权重 | `..._quality_priority_weight` | 1.0 |
| 负载权重 | `..._quality_load_weight` | 100.0 |
| 错误率权重 | `..._quality_error_weight` | 100.0 |
| 首字延迟权重 | `..._quality_latency_weight` | 10.0（相对值制，1.0=中位数） |
| 临时避让权重 | `..._quality_probation_weight` | 50.0 |
| 候选池大小 | `..._quality_top_k` | 3 |

**组三：劣化与恢复（`劣化与恢复`）**

| 页面文案 | 字段 | 默认 |
| --- | --- | --- |
| 持续劣化判定窗口 | `..._degrade_window_secs` | 120 秒 |
| 劣化错误率阈值 | `..._degrade_error_rate_threshold` | 0.5 |
| 临时降级时长 | `..._degrade_probation_secs` | 180 秒 |
| 最长降级时长 | `..._max_probation_secs` | 900 秒 |
| 降级期探测流量 | `..._probe_share_percent` | 5% |
| 恢复爬坡时长 | `..._recovery_ramp_secs` | 60 秒 |

所有权重为 0 即关闭对应维度，便于按场景裁剪。

---

## 6. 状态存储

### 6.1 kiro.rs

复用现有 Redis 协调层。扩展 `PoolRuntimeSnapshot`（`src/external_pool.rs:4326`）——它目前只有 `transient_failure_streak` 一个 `u32`——补上 `recent_error_rate`、`latency_ewma_ms`、`probation_until_ms`、选择计数，字段定义直接镜像本地调度器已有的 `SchedulerHealthState`（`src/storage/redis_cache.rs:79`）。

**必须扩展现有批量 Lua，不能新增一次往返**：`external_pool_coordinator_snapshots`（`src/storage/redis_cache.rs:4806`）已经在**单次**批量 Lua 里读取 epoch、ZCARD 并发、cooldown GET+PTTL、transient GET+PTTL。质量字段必须并入这个脚本。外部池选择运行态快照 TTL 只有 100ms（`EXTERNAL_POOL_SELECTION_RUNTIME_SNAPSHOT_TTL`），是真正的热路径，多一次 Redis 往返会直接抬高调度延迟。

**必须走 Redis 而非进程内存**：外部池已经是多实例共享 Redis 协调并发槽的模型，质量状态若只在进程内，两个实例会各自学习、互相打架，并且 HA 计划 §4.3 明确把"多实例放大"列为必须验证的风险项。

EWMA 更新沿用现有 Lua 脚本模式（`src/storage/redis_cache.rs:3753` 已有一份本地账号的 EWMA 更新脚本可直接参照），保证读-改-写原子性。

**Redis 不可用时的降级**：质量状态读取失败**不得阻塞主请求**。读不到就用中性值，退化为"优先级 + 负载"排序。这与既有的 `coordinator_breaker` 降级语义一致——调度质量是优化项，不是可用性依赖项。

### 6.3 两个必须显式处理的边界（源码核查发现）

**(1) 成功路径当前不回灌任何健康信号——这是最大的实现缺口。**

`record_external_success`（`src/external_pool.rs:9064`）目前只调用 `reset_pool_auto_disable_failure_counts`，它仅删除 5 个 `auto_disable_failures:{reason}` 键。它**不衰减 `transient_failure_streak`，也不记录任何延迟**。也就是说外部池的失败信号目前只能靠 30 秒 TTL 自然过期，成功请求对调度**零正反馈**。

对比本地账号的 `record_success_local`（`src/kiro/token_manager/manager.rs:~10025`）：

```rust
health.recent_error_rate *= 1.0 - alpha;
health.transient_failure_streak = health.transient_failure_streak.saturating_sub(1);
health.latency_ewma_ms = Some(prev + alpha * (latency_ms - prev));
```

**必须新增外部池的成功回灌**，把 `route.first_token_latency_ms` 与 `route.started_at.elapsed()` 写回质量状态。没有这一步，"质量曲线"只有下降没有回升，账号一旦变差就再也不会恢复——直接违背你的恢复要求。

**(2) 降级兜底路径丢弃全部运行态信号。**

`select_degraded_fallback_local_pool_from_snapshot`（`src/external_pool.rs:7470`）向选择函数传入的是 `(pool, 0, 0)`——in_flight 与 streak 都硬编码为 0。这条路径在 Redis 不可用时启用，此时本就读不到质量状态。

**处置决定**：该路径保持"无质量感知"，仅按静态优先级选择，并在文档与代码注释中显式说明。理由是 Redis 降级时质量数据本就不可信，此时的首要目标是**可用**而非**最优**——这与本方案 §0 的分层目标一致。

**(3) 质量状态没有 Postgres 持久化。**

外部池没有本地账号那样的 `credential_stats` / `credential_runtime_state` 表，全部运行态只在 Redis 且窗口只有 30 秒。**本方案不新增持久化表**：质量数据是短窗口的实时信号，重启后用几十个请求即可重建，为它引入跨重启持久化会增加写放大与一致性负担，收益不成比例。但必须在 UI 与文档中明确"质量数据为近期窗口统计，实例重启或 Redis 清空后重新累积"，避免运维误解。

### 6.2 sub2api

现状 `openAIAccountRuntimeStats` 是进程内 `sync.Map`（`openai_account_scheduler.go:184`），存在同样的多副本与重启丢失问题。

本方案分两步：

1. **先**把评分器泛化给 Anthropic/Gemini 路径，沿用现有进程内存储（改动小、可快速验证收益）
2. **再**把质量状态后移到 Redis（与既有 RPM / window-cost 缓存同层），解决多副本与重启失忆

第 2 步独立成单独变更，不阻塞第 1 步交付。

---

## 7. 可观测性

没有可观测性的调度策略无法调参，也无法在事故中自证。新增：

- 账号列表页展示每个账号的：错误率、相对首字、当前分数、状态（健康 / 降权 / 避让 / 恢复中）
- 请求明细记录**为什么选了这个账号**：候选集、各自分数、被排除原因（HA 计划 P1 的"候选排除与尝试账本"要求）
- 修复 sub2api 的 `buildOpenAIAccountSchedulerScoreSnapshot`（`openai_account_scheduler.go:2664`）——它当前硬编码 `errorFactor=1.0, ttftFactor=0.5`，后台展示的分数并非真实观测值，会误导调参

**usage 边界不变**：质量采样、评分、降权、避让全部属于调度域，不得影响最终 usage 计算与计费口径（Decision 001 / 符合度矩阵 S-16 的硬约束）。质量数据从 usage 记录**读取**首字延迟，但绝不反向写入。

---

## 7.1 kiro.rs 精确改动点

| 需求 | 位置 | 现状 |
| --- | --- | --- |
| 评分函数 | `select_external_pool_candidate` `src/external_pool.rs:9914` | 字典序两键排序；替换为 f64 加权和 + Top-K 加权随机，可直接照搬 `strategy.rs:33` 与 `select_health_weighted` |
| 健康字段 | `PoolRuntimeSnapshot` `src/external_pool.rs:4326` | 仅 `transient_failure_streak`；补 EWMA 字段，镜像 `SchedulerHealthState` `redis_cache.rs:79` |
| 批量读取 | `external_pool_coordinator_snapshots` `src/storage/redis_cache.rs:4806` | 已是单次批量 Lua；**扩展它**，不要新增往返 |
| 失败写入 | `record_external_pool_soft_failure` `src/external_pool.rs:8530` | 仅 INCR；补 EWMA 更新，参照 `redis_cache.rs:3573` |
| **成功写入（缺失）** | `record_external_success` `src/external_pool.rs:9064` | **只清 auto-disable 计数；必须新增 streak 衰减与延迟 EWMA** |
| 延迟数据源 | `mark_first_token_if_output` `src/external_pool.rs:9441` | TTFB **已采集**，但只进 `usage_records.data` JSONB，从不回流调度器 |
| 配置项 | `ExternalPoolsConfig` `src/model/config.rs:2798` | 仅 `transient_failure_priority_penalty`；新增权重组 |
| 校验 | `validate_external_pools_config` `src/admin/service.rs:5288` | 就绪（上界示例见 `:5933`） |
| 可观测性 | `ExternalPoolStatus` `src/external_pool.rs:818`，由 `status()` `:5684` 提供 | 在此暴露计算分数与各分项 |
| UI | `ui/src/features/external-pools/external-pools-page.tsx:470`（"调度策略"） | 在 `:492` 旁新增权重控件 |

## 8. 落地顺序

| 阶段 | 内容 | 风险 |
| --- | --- | --- |
| **P0-a** | kiro.rs：**先补成功回灌**——扩展 Redis 质量状态 + Lua，在 `record_external_success` 写入 streak 衰减与延迟 EWMA；仅采集与展示，**不改选择逻辑** | 低——纯增量，无行为变更 |
| **P0-b** | kiro.rs：`select_external_pool_candidate` 改为加权评分 + Top-K 加权随机，主开关控制 | 中——核心调度路径 |
| **P1** | kiro.rs：劣化判定、避让、恢复爬坡；配置项 + UI + 可观测性 | 低——增量 |
| **P2** | sub2api：泛化 OpenAI 评分器到 Anthropic/Gemini 路径，补 `ReportResult` 回灌 | 中——需按 openspec 流程立项 |
| **P3** | sub2api：质量状态后移 Redis；修复分数快照硬编码 | 低 |

**P0 必须拆成 a/b 两步**。先让质量数据跑起来并在后台可见，用真实流量观察一段时间，确认曲线符合预期（快账号确实分低、故障账号确实分高）之后，再让它影响调度决策。反过来做等于用未经验证的信号直接改核心调度路径——一旦归一化或权重量级有偏差，会在生产上表现为流量分布异常，且难以快速归因。

`sub2api` 是 spec-driven 仓库，P2/P3 需先经 `openspec new change` 产出 `proposal.md` / `design.md` / `tasks.md` / `specs/` 再进入实现。

---

## 9. 验收标准

沿用 HA 执行计划 §7 的口径——**按时间窗口的流量分布判断，不按单次请求成败判断**。

必须证明：

1. 全部健康时，优先级 1 的账号承接主要流量（最优调度）
2. 优先级 1 首字显著劣化时，流量**自动**向优先级 10/20 倾斜，且无需人工干预
3. 优先级 1 持续报错时，错误率维度压过延迟维度，流量更快迁走
4. **全部账号都劣化时，仍有账号被选中并成功服务**（相对性保护生效，这是"降级保证可用"的核心验收项）
5. 故障账号恢复后，经探测流量被发现，并在 `recovery_ramp_secs` 内平滑回流，不出现瞬时打满
6. 关闭主开关后，行为与当前版本完全一致
7. Redis 不可用时，调度退化为优先级+负载排序，主请求不受阻塞
8. 全程 usage 计算与计费口径不受调度路线影响

---

## 10. 明确不做

- 不做主动探测/健康检查请求——**严格按你的要求，只用真实请求产生的数据**
- 不把质量分做成硬门槛（会破坏"降级可用"）
- 不因单次失败降权（必须有窗口内的多次独立证据）
- 不引入新的等待队列或重试预算（沿用既有有界预算，避免重试放大）
- 不改 usage、计费、整形链路
