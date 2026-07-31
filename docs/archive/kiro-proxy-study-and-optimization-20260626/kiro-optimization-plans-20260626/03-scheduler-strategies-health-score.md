# 调度策略、健康分与优先级解释实施方案

## 适用范围

本方案处理账号选择策略、健康分解释、优先级语义、管理端调度说明和高并发参数建议。

本方案允许新增策略，但不得改变现有策略默认行为。现有配置下，账号选择结果必须保持兼容。

## 来源项目与学习点

- `kirocc-prox/internal/pool/selector_strategies.go`：策略接口清晰，便于解释和替换。
- `kiroxy/internal/pool/health.go`：健康分独立计算，适合管理端展示。
- `dntproxy/internal/service/account-selector.go`：账号选择可结合错误状态、健康状态和限流状态。
- 当前项目 `scheduler_score_with_config`：已经具备权重配置基础，但解释性不够。

## 当前项目现状

当前项目已有：

- `priority`
- `balanced`
- `health_balanced`
- `scheduler_*_weight`
- `scheduler_top_k`
- 单账号 RPM。
- 单账号并发。
- 全局并发。
- 冷却。
- sticky。

当前优先级语义必须明确：

- 当前系统中，数值更小的 `priority` 表示优先级更高。
- `priority = 0` 比 `priority = 10` 更优先。
- 如果要设置“平均优先级”，应使用相同数值，例如全部设为 `100`。
- 如果只想轻微倾斜，不应把差距拉得过大；建议差距使用 `1` 到 `10`。
- 如果某些账号必须明显优先，才使用 `50` 以上差距。

## 目标

- 保留现有优先级语义。
- 增加调度分数 breakdown，让管理端解释“为什么选中这个账号”。
- 增加一个更适合高并发的可选策略。
- 明确哪些参数会导致接口变慢。
- 明确大并发下推荐配置。

## 非目标

- 不删除现有策略。
- 不把随机选择作为默认策略。
- 不让健康分覆盖硬限制。
- 不在账号不可用时强行参与排序。
- 不改变 `/dfcache/*` 路由行为。

## 涉及文件

- `src/kiro/token_manager.rs`，拆分后为 `src/kiro/token_manager/strategy.rs`
- `src/kiro/token_manager/capacity.rs`
- `src/model/config.rs`
- `src/anthropic/usage.rs`
- 管理端调度状态页面对应 UI 文件

## 新增策略

新增策略名：

```text
weighted_least_inflight
```

策略行为：

1. 先执行硬过滤。
2. 硬过滤包括 disabled、模型不支持、RPM 满、账号并发满、全局并发满、冷却中、缺少认证。
3. 通过硬过滤的账号进入打分。
4. 分数越低越优先。
5. 若多个账号分数接近，使用 `scheduler_top_k` 在前 K 个账号中做轻量随机，避免同一账号被瞬间打满。

建议公式：

```text
score =
  priority_weight * priority_component
  + inflight_weight * inflight_component
  + rpm_weight * rpm_component
  + latency_weight * latency_component
  + recent_error_weight * recent_error_component
  - health_weight * health_component
  + sticky_penalty_component
```

组件定义：

- `priority_component = normalized(priority)`，数值越小越好。
- `inflight_component = in_flight / max_concurrent`。
- `rpm_component = rpm_used / rpm_limit`。
- `latency_component = clamp(ttfb_p95_ms / 30000, 0, 1)`。
- `recent_error_component = recent_error_rate_5m`。
- `health_component = health_score / 100`。
- `sticky_penalty_component = 0` 表示 sticky 命中，`0.05` 表示非 sticky 候选，具体数值可配置。

## 新增数据结构

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerScoreBreakdown {
    pub account_id: u32,
    pub strategy: String,
    pub final_score: f64,
    pub priority: i32,
    pub priority_component: f64,
    pub inflight_component: f64,
    pub rpm_component: f64,
    pub latency_component: f64,
    pub recent_error_component: f64,
    pub health_component: f64,
    pub sticky_penalty_component: f64,
    pub selected: bool,
}
```

记录规则：

- 成功请求只记录选中账号的 breakdown。
- 调度失败时可记录前 20 个候选的 breakdown。
- breakdown 不得包含 token 或请求内容。
- breakdown 默认只写 usage metadata，不返回给下游。

## 配置与兼容策略

新增配置：

```rust
pub scheduler_strategy: SchedulerStrategy,
pub scheduler_score_breakdown_enabled: bool, // 默认 false
pub scheduler_weighted_least_inflight_enabled: bool, // 默认 false
pub scheduler_top_k: usize, // 沿用现有字段，默认保持当前值
```

如果已有 `scheduler_strategy` 字段，则只新增枚举值，不改变默认值。

新增枚举值：

```rust
#[serde(rename_all = "snake_case")]
pub enum SchedulerStrategy {
    Priority,
    Balanced,
    HealthBalanced,
    WeightedLeastInflight,
}
```

兼容要求：

- 老配置缺失字段时仍使用当前默认策略。
- 配置了未知策略时启动必须失败，并给管理员清晰错误，不得静默回退。
- `scheduler_score_breakdown_enabled=false` 时不得产生额外 metadata。

## 哪些参数会导致接口变慢

必须在管理端和文档中明确：

- `credential_dispatch_max_wait_secs` 越大，请求在无账号可用时等待越久；这会提高成功率，但会增加下游等待时间。
- `dispatch_max_queued_requests` 越大，突发流量越不容易立即失败，但内存占用和排队延迟会增加。
- `kiro_upstream_response_timeout_secs` 越大，上游慢请求占用账号时间越久。
- `credential_in_flight_lease_max_secs` 过大时，异常请求释放延迟更久；过小时，长流式请求可能被误认为 lease 过期。
- 单账号 `credential_max_concurrent_requests` 过高会提高吞吐，但可能触发上游限流或首字变慢。
- `scheduler_top_k` 过大时选择更分散，但会削弱优先级控制。

## 大并发推荐配置原则

推荐原则必须写成可操作规则：

- 有多个正常账号时，`credential_max_concurrent_requests` 不应设置过高，优先让负载分散。
- 如果上游首字经常超过 10 秒，应先降低单账号并发，再观察 TTFB。
- `credential_dispatch_max_wait_secs` 建议从 `3` 到 `10` 秒试，不建议直接设置几十秒。
- `dispatch_max_queued_requests` 必须结合机器内存设置，建议先按 `账号数 * 单账号并发 * 2` 估算。
- `scheduler_top_k` 建议为 `2` 到 `5`，账号少于 3 个时不建议大于账号数。
- 优先级只用于业务倾斜，不应替代限流和并发配置。

## 实施步骤

1. 在策略模块新增 `SchedulerScoreBreakdown`。
2. 将现有策略打分函数改为同时可返回 breakdown。
3. 默认关闭 breakdown 记录，确保热路径无额外序列化。
4. 新增 `WeightedLeastInflight` 策略，但默认不启用。
5. 为所有策略写相同输入下的选择结果测试。
6. 在管理端只展示管理员可理解字段：priority、load、RPM usage、recent errors、health。
7. 在压测工具中加入策略对比场景。

## 测试方案

新增测试：

- `priority_lower_number_wins`
  - 断言 `priority=0` 先于 `priority=10`。
- `weighted_least_inflight_prefers_lower_load_when_priority_equal`
  - 优先级相同时，in-flight 比例低者胜出。
- `weighted_least_inflight_does_not_select_rpm_limited_account`
  - RPM 满的账号不得进入候选。
- `scheduler_top_k_keeps_selection_within_top_k`
  - 随机结果只能来自前 K 个。
- `scheduler_breakdown_is_disabled_by_default`
  - 默认不产生 metadata。
- `scheduler_breakdown_contains_no_secret`
  - 序列化结果不包含 token。

压测：

- 账号数 5，单账号并发 2，总并发 20，持续 5 分钟。
- 对比 `health_balanced` 和 `weighted_least_inflight`。
- 记录 P50/P95 TTFB、429 比例、平均 in-flight 分布。

## 验收标准

- 默认策略下线上行为不变。
- 新策略启用后账号负载分布更均匀。
- 管理端能解释选中原因。
- 大并发下不出现单账号持续被打满而其他账号空闲的情况。
- 未启用 breakdown 时，usage metadata 大小无明显增加。

## 风险与回滚

风险：

- 新策略可能改变用户预期的优先级倾斜。
- breakdown 记录过多可能增加写入压力。

规避：

- 默认不启用新策略。
- breakdown 默认关闭。
- 只记录选中账号或采样候选。

回滚：

- 将 `scheduler_strategy` 改回现有策略。
- 关闭 `scheduler_score_breakdown_enabled`。

## 不得做的事项

- 不得把优先级语义改成数值越大越高。
- 不得让健康分绕过 RPM、并发、冷却等硬限制。
- 不得在对下游错误中暴露调度分数。
- 不得为了均衡负载忽略 sticky 的缓存收益。

## 后续可选扩展

后续可以按路由配置策略，例如普通 `/v1` 使用 `weighted_least_inflight`，高缓存路由优先 sticky，但必须在单独方案中实现。

