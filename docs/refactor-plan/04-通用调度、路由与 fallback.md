# 通用调度、路由与 fallback

## 1. Route policy 与 target group

请求先解析为不可变的 `RouteContext`：

```text
RouteContext = {
  ingressPath, protocol, model, stream,
  requestApiKeyId, conversationHint,
  cacheNamespace, requestFacts, attemptBudget
}
```

`RoutePolicy` 决定允许的 target group、优先顺序和边界：

```json
{
  "id": "route_cc_default",
  "match": {"pathPrefixes": ["/cc"], "models": ["*"]},
  "targets": [
    {"providerTypes": ["kiro"], "mode": "local_first"},
    {"providerTypes": ["claude_code_upstream"], "mode": "fallback"}
  ],
  "allowLocal": true,
  "allowExternal": true,
  "fallbackTriggers": ["local_capacity", "no_candidate", "redis_degraded", "transient_exhausted"],
  "rescue": {"toLocal": true, "on": ["rate_limit", "timeout", "capacity"], "maxWaitSecs": 5}
}
```

每个 target 和 channel/account 仍可有自己的 `allow_all`、`allow_list`、`deny_list`。匹配规则必须先规范化路径前缀，再按最长前缀/显式优先级解析；空 allow-list 的含义要固定为“无匹配”，不能在不同页面有不同解释。

## 2. 调度内核职责

通用 `Scheduler` 只做下列事情：

1. 根据 route policy 和 provider capabilities 过滤候选。
2. 读取并组合 priority、sticky match、capacity、cooldown、queue、in-flight lease 和 request attempt budget。
3. 申请跨实例 lease，维护 heartbeat、expiry、generation 和幂等 release。
4. 按配置选择 `priority`、`balanced` 或 `health_balanced`；把 provider 的额外指标作为 typed dimensions 交给 scorer。
5. 在每次 attempt 后依据 typed `AttemptOutcome` 更新 account/channel 状态，并决定 retry、fallback、rescue 或 terminal error。

它不负责 token refresh、HTTP request、Kiro quota 查询、Anthropic body 转换或外部 usage projection。

## 2.1 路由决策矩阵

实现时要把当前分散在 handler、Kiro token manager 和 external pool 中的判断收敛为可测试的纯决策函数。输入是 `RouteContext + LocalState + ExternalState + RoutePolicy`，输出是 action 和 reason：

| 本地状态 | 外部状态 | 配置 | 结果 |
| --- | --- | --- | --- |
| 可用且模型兼容 | 任意 | local-first | 选择 Kiro；不预占外部 lease |
| 无模型兼容账号 | 有可用外部账号 | `fallback_on_unsupported_model=true` | 选择外部，并记录 `local_unsupported_model` |
| 容量满/排队超时 | 有可用外部账号 | `fallback_on_local_capacity_exhausted=true` | 选择外部，并记录 `local_capacity` |
| Redis degraded | 有可用外部账号 | `fallback_on_scheduler_redis_degraded=true` | 选择外部的 degraded-safe lease；不能伪造本地 lease |
| 无本地账号/全部 disabled | 有可用外部账号 | `fallback_on_no_available_credentials=true` | 选择外部，并记录 `local_no_candidate` |
| 本地瞬态失败耗尽 | 有可用外部账号 | `fallback_on_local_transient_exhausted=true` | 选择外部，并记录 `local_transient_exhausted` |
| 外部直连失败 | 本地可用 | route origin=direct external | 返回外部错误；不得隐式回本地 |
| 外部 fallback 首输出前超时/限流/容量 | 本地可用 | rescue 对应开关=true | 最多一次 bounded local rescue |
| 外部 fallback 已发生 rescue | 任意 | 任意 | 禁止再次跨边回退，返回最终分类错误 |

这个矩阵要成为 table-driven tests 和 Admin “为何选择该渠道”的解释数据源。日志和 usage record 保存 `decisionPath`、`fallbackReason`、`attemptIndex`、`providerType`、`channelId`，不能只记录最终账号。

## 3. Provider 特殊调度指标

Provider 通过 `QuotaSnapshot` 提供零个或多个维度：

```text
QuotaDimension {
  name: "monthly_credits" | "request_tokens" | "region_slot" | ...
  remaining: optional number
  limit: optional number
  resetAt: optional timestamp
  hard: bool
}
```

通用 eligibility 使用 `hard` 维度判断是否不可派发；soft 维度进入评分但不能违反通用 priority 和 route constraints。Kiro 可提供多个额度 cohort；Claude Code upstream 可只提供并发、RPM 或供应商自定义 quota。这样新增 Cursor/OpenAI 时无需重写 scheduler。

## 4. fallback 状态机

一次请求只允许沿 route policy 声明的有向边移动：

```text
LOCAL_SELECTED
  ├─ local success → COMPLETE
  ├─ local capacity / no candidate / Redis degraded / transient exhausted
  │       └─ if enabled → EXTERNAL_FALLBACK
  ├─ local unsupported model
  │       └─ if enabled → EXTERNAL_FALLBACK
  └─ local error not allowed by policy → TERMINAL_LOCAL_ERROR

EXTERNAL_SELECTED
  ├─ external success → COMPLETE
  ├─ pre-output retry-safe error → same/cross external retry
  ├─ external failure → terminal external error
  └─ if request origin was LOCAL_FALLBACK and rescue enabled,
          eligible rate_limit/timeout/capacity → bounded LOCAL_RESCUE

LOCAL_RESCUE → one bounded local attempt → COMPLETE or terminal original-class error
```

约束：

- 外部直连失败不能隐式切回本地，除非 route 明确声明 rescue，且该请求最初确实是 local fallback。
- 已经发生 external fallback 或 local rescue 后不得再形成循环。
- 首语义输出后，除非协议明确可安全重放，否则不得切换 provider。
- 错误分类使用稳定枚举：`auth`、`quota`、`rate_limit`、`timeout`、`capacity`、`network`、`protocol`、`model_unavailable`、`disabled`、`internal`。
- retry 次数按 request 级 budget 统一计数；provider 内部重试、跨 channel retry 和 auxiliary discovery 要有独立 kind，不能互相无限消耗。

## 5. Kiro 和外部渠道的配置映射

当前 `ExternalPoolsConfig` 中的能力拆分为：

- 通用 `DispatchConfig`：全局并发、队列、capacity mode、最大等待、attempt budget、priority、sticky、cooldown、lease。
- 通用 `RoutePolicyConfig`：local/external route mode、rules、fallback trigger、rescue、direct maintenance。
- 通用 `RetryConfig`：状态码、network/protocol retry、same-target retry、pre-output retry。
- `QualitySchedulingConfig`：EWMA、sample TTL、min samples、权重、probation、probe share、recovery ramp。
- `ProviderConfig.claudeCode`：base URL、headers、TLS、model mapping、stream mode、usage projection、auto-disable。
- `ProviderConfig.kiro`：token refresh、region、endpoint、Kiro quota、Kiro-specific failure mapping。

迁移时每个旧字段必须有一张映射表，不能因为名称统一就删除行为。例如 `fallback_on_scheduler_redis_degraded` 要映射成 route policy 的 `fallbackTriggers: [redis_degraded]`，而不是笼统的 `fallbackEnabled=true`。

## 6. Redis key 与状态隔离

建议 key 形状：

```text
gateway:v2:{tenant}:scheduler:{providerType}:{channelId}:{accountId}:lease
gateway:v2:{tenant}:scheduler:{providerType}:{channelId}:{accountId}:cooldown
gateway:v2:{tenant}:scheduler:{providerType}:{channelId}:{accountId}:quality
gateway:v2:{tenant}:sticky:{routeId}:{sessionHash}
gateway:v2:{tenant}:coordination:{providerType}:epoch
```

Kiro refresh lock、Kiro sticky 和 Claude Code lease 必须分 namespace。迁移期旧 key 与新 key 双读/单写由 feature flag 控制，切换完成后再清理旧 key。
