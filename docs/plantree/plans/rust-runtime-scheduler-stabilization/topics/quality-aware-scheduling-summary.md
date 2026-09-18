# 外部账号质量感知调度 — 改造总览

Last updated: 2026-09-18
适用项目：**kiro.rs（本仓库）**

相关文档：
- 方案设计：[external-pool-quality-aware-scheduling.md](external-pool-quality-aware-scheduling.md)
- 实现进度流水：[quality-aware-scheduling-progress.md](quality-aware-scheduling-progress.md)
- 测试维度矩阵：[quality-aware-scheduling-test-matrix.md](quality-aware-scheduling-test-matrix.md)

---

## 一、改造背景

### 1.1 原始需求（用户原话）

> 在外部账号调用中，我希望增加一个开关，这个开关默认打开：打开之后会对账号（启用的）
> 采样（**注意不是主动采样**，实际是记录调用成功的首字和耗时时间，形成一个质量曲线，
> 如果质量曲线高的优先调度）。
>
> 对了还要加时间参数，如果长时间（这个时间你来设计）质量差，就**冷却调度**
> （不是真冷却，相当于临时调度优先级变低）。
>
> 但是前提是都是正常请求的比较，如果有些账号**重试次数或者报错次数很多**，
> 这个的判断**优先级要高于**前面说的首字等质量曲线（或者可以把这个指标融合到一起）。
>
> 保证做到：
> 1. 在有外部池账号调度的情况下，保证**最优调度**
> 2. 在外部池账号不稳定的情况下，尽可能**保证稳定**
>
> 总结一句话就是：**尽可能最优，降级时要保证可用**。
>
> 当然你可以除了这个开关设计更多的参数可以进行设置，
> 比如持续多少分钟、直接对账号降级多少分钟这种。

### 1.2 改造前的问题

外部池的账号选择只看**优先级 + 负载**，不看这个池实际"跑得好不好"：

- 一个池首字 5s、另一个 300ms，调度器一视同仁；
- 一个池在持续报 502，只要还没触发熔断，它照样按轮转拿到流量；
- 没有任何机制把"最近表现差"的池**临时**排到后面，
  只有"熔断/不熔断"这种非黑即白的硬开关。

结果：用户侧体感是"有时候快有时候慢"，而系统内部并没有任何信号在纠正它。

### 1.3 几个关键约束（决定了方案形态）

| 约束 | 来源 | 对设计的影响 |
| --- | --- | --- |
| **不主动探测** | 用户明确要求 | 质量数据只能来自真实用户流量的被动采样 |
| **开关默认关闭** | 升级兼容与事故止血决定 | 显式开启才改变调度并启动被动质量采样 |
| **不是真冷却** | 用户明确要求 | 降级必须是"临时调低优先级"，必须能自动恢复，不能变成永久放逐 |
| **错误率优先于首字** | 用户明确要求 | 错误率权重必须显著高于延迟权重 |
| **降级时保证可用** | 用户明确要求 | 全员劣化时候选池**不得清空**，必须仍能选出账号 |

---

## 二、最终目标

一句话：**尽可能最优，降级时要保证可用。**

拆成可验收的四条：

1. **同优先级内择优** — 质量好的池拿更多流量。
2. **优先级是硬分层** — 质量分只能在层内重排，**绝不能**把低优先级的池提上来。
3. **持续变差 → 临时降级** — 自动进入避让期，指数退避，到期自动恢复。
4. **降级不等于放逐** — 避让期内保留探测流量，池必须有机会自证恢复。

以及一条贯穿全程的底线：

5. **不破坏既有逻辑** — 主开关关闭时，选择结果与改造前**逐字节等价**。

---

## 三、方案设计

### 3.1 整体链路

```
候选池
  ↓
① 准入过滤            ← 启用/自动禁用/路径/模型/cooldown/并发/runtime
  ↓
② 相对质量 cohort      ← 用当前请求可派发的整个候选池计算质量基线
  ↓
③ 优先级硬分层         ← 质量只在最优有效优先级层内重排
  ↓
④ 避让池探测分支       ← 按 probe_share 概率直接放行一个避让池
  ↓
⑤ 质量加权评分         ← 失败率 + 首字 + 总耗时，各自相对中位数归一化
  ↓
⑥ Top-K 加权随机       ← 不钉死在单个"最优"上，避免羊群效应
```

> ② 必须在 ① **之前**采集候选（见 §5.2 的真实缺陷），否则探测流量恒为 0。

### 3.2 采样（被动，不主动探测）

在**真实请求结束时**回灌一条样本：

- **失败率 EWMA**：成功/失败双向更新；
- **首字 EWMA / 总耗时 EWMA**：**只在成功样本上累计**
  （失败请求的耗时多为超时或立即拒绝，混进去会同时污染"快"和"慢"两个方向）。

**错误归因**：下游自身的问题不得污染池的健康度。
`external_error_affects_pool_quality` 负责区分——
下游超长请求、模型路由缺失 → 不计入；真实上游 400/429/5xx/超时/网络层故障 → 计入。

### 3.3 评分（错误率优先）

各信号**各自独立**相对中位数归一化，只惩罚"比中位数差"的一侧。
质量 probation 的中位数基准来自当前请求已经通过路径和模型准入的整个候选池；
评分重排仍只发生在最优有效优先级层内，低优先级池不能被质量分提拔。
失败率权重显著高于延迟权重，满足"报错多的判断优先级高于首字曲线"。

**样本不足按中性处理** —— 否则新池会陷入
"没数据 → 不被调度 → 永远没数据"的死锁。若整个候选池都没有达到
`quality_min_samples` 的样本，则选择函数严格回退 legacy，冷启动流量分布不变。

### 3.4 降级 / 避让 / 恢复（"不是真冷却"）

| 机制 | 说明 |
| --- | --- |
| 触发 | 当前请求准入 cohort 中，单池窗口失败率相对中位数的差值 ≥ 阈值且样本量达标 |
| 退避 | `base × 2^(n-1)`，封顶 `max_probation_secs` |
| **不顺延** | 避让期内**不重复延长**，否则持续失败会让避让无限续期，永远等不到恢复窗口 |
| 探测 | 避让期内按 `probe_share_percent` 概率放行，保证有恢复路径 |
| 恢复 | 到期后线性衰减罚分（恢复爬坡），平滑回到正常竞争 |

**避让在评分中是常数罚分** —— 这是"降级时保证可用"的数学保证：
全体一起被避让时，相对顺序不变、候选集不清空。

---

## 四、实现进度

### 4.1 总体状态

| 阶段 | 内容 | 状态 |
| --- | --- | --- |
| **P0-a** | 质量采样（仅采集，不改选择逻辑） | ✅ 完成 |
| **P0-b** | 加权评分选择 + 优先级硬分层 | ✅ 完成 |
| **P1** | 劣化 / 避让 / 探测 / 恢复爬坡 + UI + 可观测性 | ✅ 完成 |
| **L2** | 真实调度测试（mock 上游账号 + 256 并发） | ✅ 完成（3.5 压力组为余量，未做） |

对应提交：
- `4c7790b` feat(external-pool): quality-aware scheduling for external accounts
- `5d49a6e` test(external-pool): L2 real-scheduling suite for quality-aware scheduling

2026-09-15 补强落地：
- 劣化判定窗口已真正参与失败率计算，不再只是配置字段；
- 质量采样写入会保留更长的避让/恢复 TTL，且恢复爬坡完成后连续劣化层级显式归零；
- 外部流式响应在已发出 2xx 头之后发生 SSE error/read/transport failure 时也会回灌失败样本；
- 管理端清除冷却会同时清除质量状态；
- 两套池列表展示质量状态、冷启动、避让倒计时与恢复进度。

验证结果：
- 质量聚焦 Rust 测试：`47 passed / 0 failed`；恢复层级回归：`2 passed / 0 failed`；
- 劣化窗口回归：`1 passed / 0 failed`；
- `external_pool::tests` 分组：`343 passed / 0 failed`；
- Rust `cargo check --all-targets --locked` 与 `cargo fmt --all -- --check`：通过；
- `ui` `pnpm check` 与 `pnpm build`：通过；
- `admin-ui pnpm build`：按现有锁文件补齐本地依赖后通过；
- 质量调度提交基线的完整 Rust 二进制回归为 `2055` 个非忽略测试（首轮
  `2054 passed / 1 flaky failure / 6 ignored`，唯一失败用例隔离复跑
  `1 passed / 0 failed`）。随后在当前共享工作区（含并发的 region rotation 改动）复跑
  为主二进制 `2074 passed / 0 failed / 6 ignored`、`kiro_loadtest 31/31`，合计
  `2105` 个非忽略测试，全部通过；
- sub2api 的 settings/DTO/admin API/前端开关与 `ReportResult` 回灌仍是独立未完成项。

### 4.2 已实现清单

**存储层**（`src/storage/redis_cache.rs`）
- `ExternalPoolQualityState`：error_rate / ttft_ewma / latency_ewma / sample_count / probation / recovery
- Lua `record_external_pool_quality_sample`：原子读改写，EWMA 更新
- Lua `mark_external_pool_probation`：取最大值，支持指数退避层级
- Lua `clear_external_pool_quality`：管理端清除
- 质量状态并入 `external_pool_coordinator_snapshots`，**仍是同一次 Redis 往返**

**调度层**（`src/external_pool.rs`）
- `select_external_pool_candidate`：加权评分 + Top-K 加权随机（**纯函数**，`now_ms` 注入）
- `select_external_pool_candidate_legacy`：主开关关闭时走它，保证行为等价
- `evaluate_external_pool_degrade`：劣化判定（纯函数，`now_ms` 注入）
- 探测分支：在分层过滤**之前**采集候选
- 成功与失败路径均接入采样回灌
- `external_error_affects_pool_quality`：错误归因

**配置层**（`src/model/config.rs`）
- 主开关（**默认 `false`**）+ 参数组（采样 / 权重 / 劣化 / 探测 / 恢复 / Top-K）
- `validate_external_pools_config`（`src/admin/service.rs`），含跨字段校验

**可观测性 / UI**
- `ExternalPoolQualityView` 挂到 `ExternalPoolStatus`，带 `scoring_active` 标志
- `ui/src/features/runtime/runtime-page.tsx`：质量采样 / 评分权重 / 劣化与恢复 三组表单
- `admin-ui`：补类型、默认值与清洗逻辑（该目录不渲染外部池表单）

2026-09-18 事故修复补充：

- 质量采样和 probation 写入使用独立 Redis connection manager，并由 32 个后台任务硬上限
  保护；达到上限直接丢弃样本，不阻塞正式 coordinator/lease 链路。
- 普通瞬态失败不再升级为池级 cooldown；`externalPoolTransientFailureCooldownThreshold`
  仅保留为兼容字段。
- 相对 probation 基线改为当前请求已通过路径/模型准入的整个候选池；全体候选一起变差时
  不因绝对红线全部进入 probation。
- 开关显式打开但完全没有合格样本时选择严格回退 legacy；新增冷启动负载语义回归测试。
- selector 出现“有可用池但无 selected pool”的不变量破坏时，返回有界 503，不再无条件空转。

### 4.3 未完成

- L2-25 ~ L2-27 压力与边界组（矩阵 §3.5）—— 列为余量，非阻塞。

---

## 五、测试覆盖

测试分两层，顺序不可颠倒：**先单测保证基本正确，再真实调度测试**。

### 5.1 第一层：单元测试

#### 存储与数学（`src/storage/redis_cache.rs`）

*纯逻辑*
| 用例 | 覆盖点 |
| --- | --- |
| `external_pool_quality_decode_rejects_corrupt_payloads_as_neutral` | 损坏载荷退化为中性，不炸 |
| `external_pool_quality_decode_clamps_and_drops_invalid_fields` | 越界字段夹紧与丢弃 |
| `external_pool_quality_sample_threshold_guards_cold_start` | 样本门槛，冷启动不参与评分 |
| `external_pool_quality_probation_window_is_exclusive_at_expiry` | 避让窗口到期边界（右开区间） |
| `external_pool_quality_recovery_ramp_is_linear_and_saturating` | 恢复爬坡线性且饱和 |
| `external_pool_quality_key_is_namespaced_per_pool` | 质量键按池隔离 |

*Redis 真实往返*
| 用例 | 覆盖点 |
| --- | --- |
| `redis_external_pool_quality_sample_ewma_round_trip` | EWMA 数学正确 |
| `redis_external_pool_quality_sample_recovers_from_corrupt_state` | 损坏与 WRONGTYPE 自愈 |
| `redis_external_pool_probation_takes_max_and_never_shortens` | 避让取最大值，绝不缩短 |
| `redis_external_pool_quality_concurrent_samples_are_atomic` | **200 并发采样零丢失**（读-改-写原子性） |
| `redis_external_pool_quality_degradation_window_resets_old_failures` | 劣化窗口淘汰旧失败证据 |
| `redis_external_pool_quality_sample_does_not_shorten_probation_ttl` | 采样不缩短避让/恢复 TTL |
| `redis_external_pool_quality_probation_level_can_reset_after_recovery` | 恢复后 Redis 避让层级可显式归零 |
| `redis_external_pool_coordinator_snapshot_carries_quality_in_one_round_trip` | 批量快照携带质量，**仍是 1 次往返** |

#### 选择逻辑（`src/external_pool/tests.rs`）

*不破坏既有逻辑*
| 用例 | 覆盖点 |
| --- | --- |
| `quality_scheduling_disabled_matches_legacy_selection_exactly` | **开关关闭与 legacy 逐字节等价** |
| `quality_scheduling_disabled_preserves_legacy_load_balancing` | 关闭时负载均衡行为不变 |
| `quality_scheduling_never_promotes_lower_priority_tier` | **优先级硬分层不被质量分打穿** |

*评分语义*
| 用例 | 覆盖点 |
| --- | --- |
| `quality_scheduling_reorders_within_the_same_priority_tier` | 同层内按质量重排 |
| `quality_baseline_ignores_lower_priority_pools` | 质量中位数只来自最优优先级层 |
| `quality_scheduling_ranks_error_rate_above_latency_signals` | **失败率信号权重高于延迟**（用户硬性要求） |
| `quality_scheduling_separates_ttft_and_total_latency_signals` | 首字与总耗时相互独立 |
| `quality_relative_penalty_only_punishes_worse_than_median` | 相对中位数归一化只罚差的一侧 |
| `quality_median_handles_even_odd_and_empty_inputs` | 中位数奇偶与空输入 |

*降级时保证可用*
| 用例 | 覆盖点 |
| --- | --- |
| `quality_scheduling_keeps_all_pools_available_when_everyone_is_degraded` | **全员劣化时候选池不清空** |
| `quality_scheduling_still_selects_the_only_pool_even_when_degraded` | 唯一池即使很差也必须被选中 |
| `quality_scheduling_never_starves_a_probationary_pool` | 避让池不被饿死 |
| `quality_scheduling_treats_insufficient_samples_as_neutral` | 样本不足按中性 |
| `quality_scheduling_handles_pools_without_any_quality_data` | 完全无数据的池 |
| `quality_scheduling_top_k_larger_than_candidate_count_is_safe` | Top-K 越界安全 |
| `quality_scheduling_top_k_one_always_picks_the_best_scoring_pool` | Top-K=1 退化为确定性最优 |
| `quality_scheduling_empty_candidates_returns_none` | 空候选集 |

*错误归因*
| 用例 | 覆盖点 |
| --- | --- |
| `quality_sampling_ignores_downstream_caused_payload_errors` | 下游超长请求不污染 |
| `quality_sampling_counts_genuine_upstream_400s` | 真实上游 400 计入 |
| `quality_sampling_ignores_model_routing_misses` | 模型路由缺失不污染 |
| `quality_sampling_counts_upstream_faults` | 429/5xx/超时/网络层全部计入 |

*劣化 / 避让 / 探测 / 恢复*
| 用例 | 覆盖点 |
| --- | --- |
| `degrade_requires_error_rate_at_or_above_threshold` | 阈值边界 |
| `degrade_never_fires_when_master_switch_is_off` | 开关关闭不降级 |
| `degrade_ignores_pools_with_insufficient_samples` | 样本不足不降级 |
| `degrade_does_not_extend_an_active_probation` | **避让期内不顺延**（否则变永久封禁） |
| `degrade_backoff_doubles_and_is_capped` | 指数退避翻倍且封顶 |
| `degrade_level_increments_from_existing_level` | 层级递增 |
| `degrade_level_resets_after_recovery_ramp` | 恢复爬坡完成后层级归零 |
| `degrade_ttl_covers_probation_window_plus_recovery_ramp` | TTL 必须覆盖避让 + 爬坡 |
| `probe_share_zero_gives_probationary_pool_no_traffic` | `probe_share=0` 退化为硬避让 |
| `probe_traffic_never_crosses_priority_tiers` | **探测流量不得跨优先级层** |
| `probe_traffic_reaches_a_pool_demoted_by_its_failure_streak` | 被连击降权的池仍能拿到探测流量 |
| `probe_traffic_still_respects_user_configured_priority_for_demoted_pools` | 用户配置的备用池不借探测越层 |
| `recovery_ramp_gradually_restores_traffic_after_probation_expires` | 恢复爬坡逐步放量 |

*可观测性与前后端契约*
| 用例 | 覆盖点 |
| --- | --- |
| `quality_view_reports_probation_countdown_and_recovery_progress` | 避让倒计时与恢复进度 |
| `quality_view_marks_cold_start_pools_as_not_scoring` | 冷启动池显式标注"未参与调度" |
| `quality_config_serializes_with_the_camel_case_keys_the_ui_sends` | **锁定前后端键契约**，同时断言主开关默认 `false` |

#### 配置校验（`src/admin/service_tests.rs`）

| 用例 | 覆盖点 |
| --- | --- |
| `external_pool_quality_defaults_pass_validation` | **默认配置合法、主开关为 false** |
| `external_pool_quality_ewma_alpha_validation_is_bounded` | EWMA alpha 边界 |
| `external_pool_quality_weights_reject_negative_and_non_finite` | 权重拒绝负数与非有限值 |
| `external_pool_probation_ceiling_cannot_be_below_single_probation` | 避让上限不得低于单次避让 |
| `external_pool_degrade_error_rate_threshold_is_a_ratio` | 失败率阈值必须是比率 |
| `external_pool_quality_probe_share_and_top_k_are_bounded` | probe share 与 Top-K 边界 |

### 5.2 第二层：真实调度测试（mock 上游账号 + 高并发）

> 用户硬性要求：**不能只搞 1 并发、几并发，那种测试没有意义**。
> 本组统一 `L2_WAVE_SIZE = 256` 并发，跑真实 PG + Redis + mock 上游服务器，
> 走**完整调度链路**（不是直接调纯函数）。

**mock 上游账号能力**（`PatternExternalMessagesFakeServer`）

| 行为变体 | 模拟的账号状态 |
| --- | --- |
| `AlwaysSuccess` | 完全正常的账号 |
| `AlwaysFail { status }` | 持续异常、**从不恢复**的账号 |
| `FailFirst { count }` | 异常一段时间后**恢复**的账号 |
| `Intermittent { fail_percent, seed }` | 间歇性抖动（确定性种子，可复现） |
| `SlowSuccess { delay }` | **首字慢 / 总耗时长**但成功的账号 |

**L2 用例与用户列举场景的对应**

| L2 用例 | 覆盖的用户场景 |
| --- | --- |
| `l2_quality_disabled_keeps_legacy_priority_routing_under_load` | **开关关闭 = 旧行为**（最先必须通过的闸门） |
| `l2_quality_shifts_traffic_to_the_faster_pool_within_one_priority_tier` | 多账号**全正常但快慢不同** → 流量转向快的 |
| `l2_quality_never_promotes_a_lower_priority_tier_under_load` | 多账号**优先级不同** → 高并发下仍不越层 |
| `l2_all_pools_degraded_still_serves_traffic_without_emptying_candidates` | **所有账号异常** → 仍可用，不清空候选 |
| `l2_single_remaining_pool_is_never_starved_even_when_failing` | **只剩一个且它也在失败** → 不饿死 |
| `l2_failing_pool_is_probationed_then_recovers_and_regains_traffic` | **部分账号异常 → 降级 → 探测 → 逐渐恢复**（三阶段全轨迹） |
| `l2_quality_composes_with_existing_transient_failure_penalty` | **新因子与既有瞬态连击降权混合**，互不破坏 |

**与"已有因子"的混合覆盖**：
优先级分层、瞬态失败连击（`transient_failure_streak`）、负载均衡、
熔断与恢复探测，均已在上表用例中与质量因子同场测试。

**测试基建**（为可靠性专门做的）
- `wait_for_quality_samples`：**轮询收敛**替代固定 sleep
  —— 采样是 detached `tokio::spawn`，wave 跑完样本可能还没落 Redis，
  固定 sleep 是既有测试 flaky 的根源。
- `deterministic_failure_percent(index, seed)`：失败注入可复现，不用 `fastrand`。
- 并列打散用未播种随机 → 断言一律用**统计容差**，不断言精确落点。

---

## 六、测试中捕获的真实缺陷（改造价值的体现）

### 6.1 优先级被质量分打穿（P0-b，既有测试捕获）

初版把优先级当作评分中的**一项权重**。默认权重下优先级 1/2/3 只差 1.0 分，
Top-K 加权随机让优先级 3 的池仍能拿到 ~1/6 的抽中概率
——**用户配置的优先级被质量分打穿，备用池会抢主池流量**。

修正：优先级改为**硬分层**，先取最优层，质量分只在层内重排。

### 6.2 避让退化成永久放逐（P1）

纯罚分方案下，`top_k=3` 且有 3 个健康池时，被避让的池排第 4，
拿到的流量**恰好是零**。而质量样本只来自真实流量
→ 它永远产生不了新样本自证恢复 → "临时降低优先级"变成**永久放逐**，
直接违背用户"不是真冷却"的要求。

修正：在 Top-K 之前加独立的探测分支。

### 6.3 探测流量被既有连击挡在层外（L2 捕获）

**这是 L2 存在的全部意义** —— 纯函数单测全绿，但真实链路上探测流量为 0。

被降级的池几乎总是**同时**背着既有的 `transient_failure_streak`，
连击抬高有效优先级把它挤出最优层；而探测分支写在分层过滤**之后**，
于是层内永远抽不到它。两套降权机制相互独立，
探测要救的恰恰是"两者都踩中"的池。

修正：探测候选改为在分层过滤**之前**采集，并区分两种"低优先级"——

| 情形 | 放行探测？ | 理由 |
| --- | --- | --- |
| 用户配置的低优先级备用池 | ❌ | 用户明确意图，主池可用时就该闲着 |
| 基础优先级相同、被连击降下去 | ✅ | 系统自己打的惩罚，正是探测要解救的对象 |

---

## 七、已知问题

### 7.1 历史本机 5 个红灯记录（当前已不再复现）

以下内容是早期基线的历史记录。质量调度提交基线的完整 Rust 二进制回归覆盖
`2055` 个非忽略测试：首轮为 `2054 passed / 1 flaky failure / 6 ignored`，唯一失败用例
隔离复跑为 `1 passed / 0 failed`；`kiro_loadtest` 为 `31/31`。随后在当前共享工作区
（含并发的 region rotation 改动）复跑为主二进制 `2074 passed / 0 failed / 6 ignored`
加 `kiro_loadtest 31/31`，合计 `2105` 个非忽略测试，全部通过。下面列出的 5 个用例也
已逐项复验通过，因此它们不再是当前阻塞。保留归因内容仅用于解释历史平台差异。

`no_response_headers_becomes_client_timeout_without_raw_body` /
`retry_send_timeout_uses_remaining_dispatch_deadline` /
`slow_upstream_status_keeps_status_and_error_body_fragment` /
`stream_keepalive_emits_ping_during_silent_gap_before_output` /
`legacy_zero_external_wait_reaches_a_bounded_final_error`

**归因证据**：在引入这些测试的提交（`2f38e53`）上直接复跑，以**完全相同的方式失败**
——本机从未通过过，不存在回归。CI 跑 `ubuntu-24.04` 且 `--test-threads=1` 是必跑步骤，
说明它们**在 Linux 上是绿的**。

根因是测试假服务器与客户端之间的 TCP 关闭语义（macOS 上 `ECONNRESET(os error 54)`
导致响应体读取失败 → 触发同池重试 → `attempts.len()` 从 1 变 2）。
属于**测试夹具的平台可移植性问题**，不影响 CI、不影响生产逻辑。

> 历史处理结论：这五个问题当时先不修复，作为平台相关测试夹具问题单独记录。

### 7.2 运行测试的注意事项

- 环境变量：`KIRO_RS_TEST_POSTGRES_URL` + `KIRO_RS_TEST_REDIS_URL`
  （**不是** `KIRO_RS_TEST_DATABASE_URL`）
- 本地容器：`kiro-rs-postgres-local`(25432) / `kiro-rs-redis-local`(26379)
- 必须 `--test-threads=1`，否则
  `redis_external_pool_snapshot_and_acquire_are_atomic_across_managers` 会因并发争用失败
- `cargo check --bins` **不编译测试代码**，判断是否破坏既有测试必须用
  `cargo test --bins --no-run`
- 判定绿灯必须读 `test result:` 行 —— 命令若以 `| tail` 结尾，
  exit code 来自 `tail` 而非 `cargo`，**恒为 0**
- 本机内存/swap 吃紧：用 `CARGO_BUILD_JOBS=2` 限制并行度，测试后清理残留进程

---

## 八、附：用户列举场景 → 覆盖位置速查

| 用户列举的场景 | 覆盖 |
| --- | --- |
| 多个账号全正常（优先级不一定相同） | L2 `never_promotes_a_lower_priority_tier` + 单测 `never_promotes_lower_priority_tier` |
| 部分账号异常 | L2 `failing_pool_is_probationed_then_recovers` 阶段一 |
| 部分账号首字高 | L2 `shifts_traffic_to_the_faster_pool`（`SlowSuccess` mock）+ 单测 `separates_ttft_and_total_latency_signals` |
| 部分账号耗时长 | 同上（非流式下首字与总耗时同点，两者归一化项在单测中分别覆盖） |
| 所有账号异常 | L2 `all_pools_degraded_still_serves_traffic` + 单测 `keeps_all_pools_available_when_everyone_is_degraded` |
| 异常后逐渐恢复 | L2 `failing_pool_is_probationed_then_recovers`（`FailFirst` mock）三阶段 |
| 异常后很久才恢复 | 指数退避 `degrade_backoff_doubles_and_is_capped` + `recovery_ramp_gradually_restores_traffic` |
| 异常后不恢复 | L2 `single_remaining_pool_is_never_starved`（`AlwaysFail` mock） |
| **与既有因子混合** | L2 `composes_with_existing_transient_failure_penalty` + 探测/连击四个边界单测 |
| **不破坏既有逻辑** | L2 `quality_disabled_keeps_legacy_priority_routing` + 单测 `disabled_matches_legacy_selection_exactly` |
