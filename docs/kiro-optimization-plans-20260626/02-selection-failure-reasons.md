# 结构化调度失败原因实施方案

## 适用范围

本方案处理本地账号调度失败、外部账号转发失败、队列等待失败、模型不可用、路由不可用、限流、并发打满、冷却中等失败原因的结构化记录。

本方案不改变调度决策本身。它只让系统能够准确回答“为什么没有账号接这个请求”，并把原因写入内部日志、usage、管理端诊断。

## 来源项目与学习点

- `kirocc-prox`：调度层将 selector、scheduler、conductor 分开，适合在选择失败时保留结构化上下文。
- `dntproxy/internal/service/account-selector.go`：账号选择失败原因更明确，便于判断是模型不支持、额度、限流还是健康态问题。
- 当前项目 `src/external_pool.rs`：已经对外部账号错误做了归一化和内部诊断保存，可作为本地账号失败原因的记录风格参考。

## 当前项目现状

当前项目已经有：

- 本地账号容量判断。
- RPM 和并发限制。
- dispatch wait。
- account cooldown。
- route state。
- usage 记录和 error id。

当前不足：

- “没有可调度账号”可能由多个原因造成，日志和页面不容易区分。
- 热路径中部分失败只保留字符串，后续无法做统计。
- 对下游需要统一口径，但内部又需要保留完整排查信息。

## 目标

- 每次调度失败都必须产生机器可读的失败原因。
- 内部记录必须完整但不重复。
- 对下游仍只返回统一英文错误，不暴露内部模块概念。
- 管理端可以展示失败原因聚合，但不泄漏敏感信息。
- 记录逻辑不得阻塞请求热路径。

## 非目标

- 不改变账号选择顺序。
- 不改变重试策略。
- 不改变 HTTP 状态码策略，除非错误归一化文档另有定义。
- 不把上游原始错误直接返回给下游。
- 不在本方案里做 UI 大改。

## 涉及文件

- `src/kiro/token_manager.rs`，拆分后为 `src/kiro/token_manager/capacity.rs`
- `src/kiro/token_manager/strategy.rs`
- `src/kiro/token_manager/queue.rs`
- `src/kiro/token_manager/route_state.rs`
- `src/kiro/call_trace.rs`
- `src/anthropic/usage.rs`
- `src/anthropic/envelope.rs`
- `src/external_pool.rs`
- `src/model/config.rs`

## 新增数据结构

新增内部枚举：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionFailureStage {
    RouteValidation,
    ModelEligibility,
    AccountEligibility,
    RpmLimit,
    AccountConcurrency,
    GlobalConcurrency,
    DispatchQueue,
    DispatchWait,
    Cooldown,
    StickyBinding,
    UpstreamPreflight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountRejectReason {
    Disabled,
    MissingAuth,
    ModelNotSupported,
    RouteNotAllowed,
    RpmLimited,
    AccountConcurrencyFull,
    GlobalConcurrencyFull,
    CooldownActive,
    HealthBlocked,
    StickyTargetUnavailable,
    RefreshInProgress,
    RefreshFailed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionFailureSummary {
    pub request_id: String,
    pub route: String,
    pub model: String,
    pub stage: SelectionFailureStage,
    pub primary_reason: AccountRejectReason,
    pub rejected_account_count: usize,
    pub waitable_account_count: usize,
    pub retry_after_ms: Option<u64>,
    pub reason_counts: BTreeMap<AccountRejectReason, usize>,
    pub sampled_accounts: Vec<RejectedAccountSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedAccountSample {
    pub account_id: u32,
    pub reason: AccountRejectReason,
    pub rpm_used: Option<u32>,
    pub rpm_limit: Option<u32>,
    pub in_flight: Option<u32>,
    pub max_concurrent: Option<u32>,
    pub cooldown_remaining_ms: Option<u64>,
}
```

采样规则：

- `sampled_accounts` 最多保留 20 个账号。
- 优先采样 `primary_reason` 对应账号。
- 不记录 token、access key、refresh token、authorization header、完整请求体。
- `reason_counts` 是完整聚合，不受采样数量影响。

## 对外错误映射

对下游必须只使用统一英文消息。不得出现 pool、fallback、external、credential、backup、capacity snapshot 等内部词。

映射规则：

| 内部原因 | HTTP 状态 | 对外 message |
| --- | --- | --- |
| `ModelNotSupported` | 400 | `The requested model is not available for this endpoint.` |
| `RouteNotAllowed` | 404 | `The requested endpoint is not configured.` |
| `RpmLimited` | 429 | `No account is ready for this request right now. Please retry shortly.` |
| `AccountConcurrencyFull` | 429 | `No account is ready for this request right now. Please retry shortly.` |
| `GlobalConcurrencyFull` | 429 | `The request queue is full right now. Please retry shortly.` |
| `DispatchQueue` | 429 | `The request queue is full right now. Please retry shortly.` |
| `DispatchWait` | 429 | `No account became ready before the dispatch wait timeout.` |
| `CooldownActive` | 429 | `No account is ready for this request right now. Please retry shortly.` |
| `MissingAuth` / `RefreshFailed` | 502 | `The upstream account could not complete this request.` |
| 其他未知原因 | 500 | `The request could not be completed.` |

最终返回必须追加 error id：

```text
{message} If this continues, contact the administrator with error ID: {error_id}
```

## 内部记录规则

必须在 usage 或 trace 中记录：

- `request_id`
- `error_id`
- `failure_stage`
- `primary_reason`
- `reason_counts`
- `sampled_accounts`
- `route`
- `model`
- `dispatch_wait_ms`
- `queue_depth`
- `global_in_flight`
- `timestamp`

不得重复记录：

- `error_id` 只能在顶层字段出现一次。
- 原始错误 message 只能出现在 `raw_error.message` 或 `record_message` 一个位置，不能同时复制到多个 metadata 字段。
- status code 只能在 `error_status_code` 或 `raw_error.status_code` 一个位置作为权威字段。

## 配置与兼容策略

新增配置：

```rust
pub selection_failure_sample_limit: usize, // 默认 20
pub selection_failure_record_enabled: bool, // 默认 true
```

兼容要求：

- 缺失配置时使用默认值。
- 管理端老数据没有这些字段时必须能正常反序列化。
- 关闭 `selection_failure_record_enabled` 时，对外错误仍保持归一化，只是不写详细 sampled accounts。

## 实施步骤

1. 在 token manager 内部新增 failure 类型，不接入行为。
2. 在容量判断处为每个被拒账号生成 `AccountRejectReason`。
3. 在选择失败出口聚合 `SelectionFailureSummary`。
4. 将 summary 挂到 `KiroCredentialAttempt` 或新增 `KiroAccountAttempt` 兼容字段。
5. 将 summary 写入 `UsageRecord.error_metadata`。
6. 修改 `envelope` 调用点，只根据内部 reason 选择统一英文 message。
7. 管理端后续读取 `failure_stage`、`primary_reason` 和 `reason_counts` 展示。

## 测试方案

新增测试名称和断言：

- `selection_failure_records_rpm_limited_accounts`
  - 构造 3 个账号全部 RPM 满。
  - 断言 `primary_reason == RpmLimited`。
  - 断言对外 message 不包含内部词。
- `selection_failure_records_concurrency_full_accounts`
  - 构造单账号 in-flight 达上限。
  - 断言 `retry_after_ms` 可以为空，但 reason count 必须正确。
- `selection_failure_prefers_model_not_supported_over_generic_unavailable`
  - 构造所有账号都不支持模型。
  - 断言 HTTP 400，message 为模型不可用。
- `selection_failure_sample_limit_is_enforced`
  - 构造 100 个账号失败。
  - 断言 `sampled_accounts.len() == 20`。
- `selection_failure_does_not_record_secret_fields`
  - 构造带 token 的账号。
  - 序列化 summary 后断言不包含 token 子串。
- `selection_failure_error_id_is_not_duplicated`
  - 断言 error id 只出现在权威字段。

## 验收标准

- 调度失败时 usage 能看到结构化 reason。
- 对下游错误消息不出现内部概念。
- 高并发失败场景下 CPU 和内存开销可控。
- `sampled_accounts` 有上限。
- 关闭详细记录后请求行为不变。

## 风险与回滚

风险：

- 热路径为每个账号分配过多对象。
- 管理端搜索 metadata 变慢。

规避：

- 聚合时使用计数器，账号样本只保留前 20 个。
- 大对象只在最终失败出口创建。
- 成功调度不写 failure summary。

回滚：

- 保留配置 `selection_failure_record_enabled`。
- 如果发现写入开销过高，先关闭详细记录，不回滚对外错误归一化。

## 不得做的事项

- 不得把内部 reason 原样返回给下游。
- 不得记录完整请求体。
- 不得记录 authorization、token、cookie。
- 不得在每个账号失败时同步写数据库。
- 不得把此方案和调度算法优化合并实施。

## 后续可选扩展

后续可以基于 `SelectionFailureSummary` 做管理端失败趋势图，例如 RPM 触顶、并发触顶、冷却中、模型不支持的比例。

