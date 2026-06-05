# Kiro 凭据调度、并发限制与 429 优化分析

更新日期：2026-06-05

本文用于记录 `kiro.rs` 当前凭据调度系统在高频请求、账号 429、粘性会话、并发限制、冷却退避方面的背景、原因分析和无歧义落地方案。本文应当能够脱离当前对话独立阅读。

## 1. 背景

系统通过多个 Kiro 账号凭据向上游发起 Claude Code / Anthropic 兼容请求。生产使用中出现了以下问题：

1. 某些账号被连续调度，导致请求过于集中。
2. 同一账号请求太频繁时，上游返回 `429`、`high traffic`、`rate limit` 或其他瞬态错误。
3. 同一会话存在 sticky 粘性调用需求，但 sticky 不能变成“必须等待或必须反复调用同一个账号”。
4. 某些账号实际已经不可用，例如 `403 Forbidden`、账号被暂停、账号被锁定、无模型权限，但调度链路可能仍重复打到这些账号。
5. 用户期望调度更平均，但仍保留一定粘度，以兼顾缓存、会话连续性和请求成功率。

因此需要明确：

1. 当前系统的并发限制到底是什么。
2. 当前已有的 429 / 冷却 / 限速能力是什么。
3. 为什么账号仍可能触发 429。
4. 应当如何通过配置立即降低风险。
5. 后续代码应当如何改造，才能更主动地保护账号。

## 2. 关键术语

### 2.1 并发限制

并发限制限制的是某一瞬间正在进行中的请求数量，也就是 in-flight request 数量。

它不是“某个时间窗口内最多多少请求”，也不是“每分钟最多多少请求”。如果请求很快完成，即使并发限制为 1，一个账号仍可能在一分钟内被调度很多次。

当前相关配置：

```json
{
  "credentialMaxConcurrentRequests": 1,
  "dispatchGlobalMaxConcurrentRequests": 20,
  "dispatchMaxQueuedRequests": 100
}
```

含义：

1. `credentialMaxConcurrentRequests`：单个账号同时最多处理多少个请求。`0` 表示不限制。
2. `dispatchGlobalMaxConcurrentRequests`：整个服务所有账号合计最多同时处理多少个请求。`0` 表示不限制。
3. `dispatchMaxQueuedRequests`：没有账号可调度时，最多允许多少请求等待。`0` 表示不限制等待队列数量。

### 2.2 RPM 限速

RPM 限速限制的是单账号目标请求频率。当前字段是：

```json
{
  "credentialRpm": 6
}
```

如果 `credentialRpm = 6`，系统会换算成单账号约 10 秒调度一次。它比并发限制更适合避免“短请求高频打同一个账号”。

### 2.3 冷却

冷却是账号遇到上游瞬态错误之后的保护状态。例如账号返回 429 后，账号会进入 rate limit 冷却，在冷却结束前不会被正常调度。

当前相关配置：

```json
{
  "credentialRateLimitCooldownSecs": 90,
  "credentialCooldownBackoffMultiplier": 2.0,
  "credentialCooldownJitterPercent": 30,
  "credentialMaxCooldownSecs": 900,
  "credentialProbationSecs": 120
}
```

含义：

1. `credentialRateLimitCooldownSecs`：遇到 429 且上游没有明确 `Retry-After` 时的基础冷却秒数。
2. `credentialCooldownBackoffMultiplier`：连续瞬态错误时的冷却退避倍率。
3. `credentialCooldownJitterPercent`：冷却抖动比例，避免多个账号同时恢复、同时再次打爆。
4. `credentialMaxCooldownSecs`：冷却最大秒数。
5. `credentialProbationSecs`：冷却结束后的观察期。观察期内账号应降低调度权重。

### 2.4 Sticky 粘性

Sticky 粘性表示同一个会话优先使用上一次成功绑定的账号。它的目的不是强制所有请求都必须等待同一个账号，而是尽量保持会话和账号之间的稳定关系。

正确语义：

```text
优先使用 sticky 绑定账号。
如果绑定账号不可用、并发满、冷却、限速、模型不支持、账号被禁用，则临时 fallback 到其他账号。
fallback 成功不应破坏原 sticky 绑定，除非后续策略明确要求改绑。
```

## 3. 当前代码中的相关实现

### 3.1 配置字段

核心配置位于 `src/model/config.rs`：

1. `credentialRpm`
2. `credentialMaxConcurrentRequests`
3. `credentialRateLimitCooldownSecs`
4. `credentialServerErrorCooldownSecs`
5. `credentialNetworkErrorCooldownSecs`
6. `credentialStreamErrorCooldownSecs`
7. `credentialProtocolErrorCooldownSecs`
8. `credentialAuthErrorCooldownSecs`
9. `credentialCooldownBackoffMultiplier`
10. `credentialCooldownJitterPercent`
11. `credentialProbationSecs`
12. `credentialMaxCooldownSecs`
13. `credentialDispatchMaxWaitSecs`
14. `credentialRetryMaxAttempts`
15. `credentialInFlightLeaseMaxSecs`
16. `dispatchGlobalMaxConcurrentRequests`
17. `dispatchMaxQueuedRequests`
18. `loadBalancingMode`

### 3.2 调度入口

主要调度入口位于 `src/kiro/token_manager.rs`：

1. `acquire_context_for_session`：获取可调度凭据，支持模型过滤、sticky 会话和本次请求临时排除列表。
2. `credential_is_dispatchable`：判断凭据是否可调度。
3. `entry_has_concurrency_capacity`：判断凭据是否还有并发容量。
4. `entry_rate_limit_remaining`：判断凭据是否处于本地 RPM 限速状态。
5. `entry_cooldown_remaining`：判断凭据是否处于错误冷却状态。
6. `acquire_in_flight_slot`：实际占用账号并发槽。
7. `report_transient_failure_kind`：记录瞬态错误并计算冷却。

### 3.3 API / MCP 401 和 403 处理

当前已将普通 `401/403` 凭据失败处理收敛到统一 helper：

```text
handle_credential_auth_failure
```

统一语义：

1. 先记录 `Auth` 类型瞬态失败。
2. 如果是 bearer token 失效，每个凭据只强制刷新一次 token。
3. 强刷成功则继续重试，不禁用账号。
4. 强刷失败或普通权限错误则计入失败次数。
5. API 请求会解除当前会话对该坏账号的 sticky 绑定。
6. 如果还有其他支持当前模型且可调度的账号，本次请求链路会临时排除当前账号并换号。
7. MCP 请求也使用本次请求临时排除列表，不再只靠失败计数间接换号。

### 3.4 风控识别

当前已补充以下风控/锁定文本识别：

1. `TEMPORARILY_SUSPENDED`
2. `temporarily suspended`
3. `temporary suspended`
4. `temporarily is suspended`
5. `is temporarily suspended`
6. `ACCOUNT_SUSPENDED`
7. `PERMANENTLY_SUSPENDED`
8. `AccountSuspendedException`
9. `account locked`
10. `user locked`
11. `locked account`
12. `locked your account`

这些错误应走风控禁用分支，不应混入普通 `403` 三次失败重试。

### 3.5 429 处理

API 分支遇到以下状态会按瞬态错误处理：

```text
408
429
5xx
```

`429` 会被归类为：

```text
TransientFailureKind::RateLimit
```

之后进入 `report_transient_failure_kind`，按配置计算冷却时间。当前语义是：429 不会把账号永久禁用，也不会走普通失败计数，而是进入临时冷却。

## 4. 问题原因分析

### 4.1 只靠并发限制不能避免 429

并发限制只能限制同时进行中的请求数。它不能限制一个账号在一分钟内被调度多少次。

例如：

```json
{
  "credentialMaxConcurrentRequests": 1
}
```

如果每个请求 300ms 完成，那么理论上一分钟内同一账号仍可能被调度约 200 次。这种情况下，即使并发为 1，也可能触发上游 429。

结论：

```text
防 429 必须配置 RPM 或实现 token bucket。
仅配置并发不足以保护账号。
```

### 4.2 默认 priority 调度容易集中流量

默认 `loadBalancingMode` 是 `priority`。在 priority 模式下，如果多个账号优先级不同，高优先级账号会更容易被反复选中。

这适合“优先使用高质量账号”的场景，但不适合“尽量降低单账号 429 风险”的场景。

结论：

```text
高频生产流量建议使用 health_balanced 或 balanced。
```

### 4.3 Sticky 粘性会增加单账号压力

Sticky 可以提升会话连续性，但如果某个 Claude Code 会话连续发起多轮请求、工具调用、agent 调用，则 sticky 绑定账号会承受更高压力。

当前已修复：

```text
sticky 账号并发满时，可以临时 fallback 到其他账号。
```

仍需后续优化：

```text
sticky 账号接近 RPM、刚发生 429、处于 probation、近期错误率升高时，也应临时 fallback。
```

### 4.4 429 当前主要是被动保护

当前系统在账号返回 429 后，会将账号冷却。这能避免继续打爆同一个账号，但它是在上游已经报错后才发生。

结论：

```text
当前系统已有被动保护，但主动削峰不足。
```

主动削峰需要：

1. 单账号 RPM。
2. token bucket。
3. 模型级限流。
4. 429 后动态降速。
5. soft sticky fallback。

### 4.5 不同模型对账号压力不同

Opus、Sonnet、Haiku 的成本、耗时、上下文、上游敏感度不同。统一使用同一套并发/RPM 会导致策略粗糙。

建议后续增加模型级限制：

```json
{
  "modelLimits": {
    "claude-opus-*": {
      "globalConcurrent": 5,
      "credentialConcurrent": 1,
      "credentialRpm": 3
    },
    "claude-sonnet-*": {
      "globalConcurrent": 20,
      "credentialConcurrent": 1,
      "credentialRpm": 6
    },
    "claude-haiku-*": {
      "globalConcurrent": 30,
      "credentialConcurrent": 2,
      "credentialRpm": 12
    }
  }
}
```

## 5. 立即可用的配置方案

### 5.1 保守方案：优先保护账号

适用场景：

1. 账号容易 429。
2. 账号有被风控风险。
3. Claude Code 长会话、流式、多工具调用较多。
4. 目标是稳定而不是极限吞吐。

建议配置：

```json
{
  "loadBalancingMode": "health_balanced",
  "credentialMaxConcurrentRequests": 1,
  "credentialRpm": 6,
  "credentialRateLimitCooldownSecs": 90,
  "credentialServerErrorCooldownSecs": 15,
  "credentialNetworkErrorCooldownSecs": 10,
  "credentialStreamErrorCooldownSecs": 10,
  "credentialProtocolErrorCooldownSecs": 30,
  "credentialAuthErrorCooldownSecs": 30,
  "credentialCooldownBackoffMultiplier": 2.0,
  "credentialCooldownJitterPercent": 30,
  "credentialProbationSecs": 120,
  "credentialMaxCooldownSecs": 900,
  "credentialDispatchMaxWaitSecs": 60,
  "dispatchMaxQueuedRequests": 100
}
```

全局并发按账号数量设置：

```text
可用账号 <= 10：dispatchGlobalMaxConcurrentRequests = 可用账号数
可用账号 11-30：dispatchGlobalMaxConcurrentRequests = 10 到 20
可用账号 > 30：dispatchGlobalMaxConcurrentRequests = 20 到 30
```

### 5.2 平衡方案：保护账号同时保留吞吐

适用场景：

1. 账号数量较多。
2. 429 偶发但不是非常严重。
3. 需要兼顾稳定性和吞吐。

建议配置：

```json
{
  "loadBalancingMode": "health_balanced",
  "credentialMaxConcurrentRequests": 1,
  "credentialRpm": 10,
  "credentialRateLimitCooldownSecs": 60,
  "credentialServerErrorCooldownSecs": 10,
  "credentialNetworkErrorCooldownSecs": 5,
  "credentialStreamErrorCooldownSecs": 10,
  "credentialProtocolErrorCooldownSecs": 20,
  "credentialAuthErrorCooldownSecs": 20,
  "credentialCooldownBackoffMultiplier": 2.0,
  "credentialCooldownJitterPercent": 25,
  "credentialProbationSecs": 90,
  "credentialMaxCooldownSecs": 600,
  "credentialDispatchMaxWaitSecs": 60,
  "dispatchMaxQueuedRequests": 100
}
```

全局并发：

```text
dispatchGlobalMaxConcurrentRequests = min(可用账号数, 30)
```

### 5.3 激进方案：吞吐优先

适用场景：

1. 账号稳定。
2. 上游 429 少。
3. 请求多为短请求。
4. 能接受偶发 429。

建议配置：

```json
{
  "loadBalancingMode": "health_balanced",
  "credentialMaxConcurrentRequests": 2,
  "credentialRpm": 20,
  "credentialRateLimitCooldownSecs": 45,
  "credentialCooldownBackoffMultiplier": 2.0,
  "credentialCooldownJitterPercent": 20,
  "credentialProbationSecs": 60,
  "credentialMaxCooldownSecs": 600,
  "credentialDispatchMaxWaitSecs": 60,
  "dispatchMaxQueuedRequests": 200
}
```

全局并发：

```text
dispatchGlobalMaxConcurrentRequests = min(可用账号数 * 2, 50)
```

不建议在账号不稳定时使用该方案。

## 6. 后续代码优化方案

本节描述下一阶段明确的代码落地方案。除非用户明确要求实施，否则本文仅作为设计方案和验收标准。

### 6.1 P0：429 后当前请求链路立即换号

当前 429 会让账号进入冷却，但建议进一步做当前请求链路排除。

目标行为：

```text
当 API 或 MCP 调用返回 429：
1. 记录 RateLimit 瞬态失败。
2. 根据 Retry-After 或本地配置设置账号冷却。
3. 如果还有其他支持当前模型且可调度的账号，将当前账号加入本次请求 excluded_ids。
4. 当前请求立即 retry 其他账号。
5. 如果没有其他可调度账号，则进入等待或最终返回上游错误。
```

日志 action 建议：

```text
rate_limit_exclude_and_retry
rate_limit_retry
```

验收标准：

1. 账号 A 返回 429，账号 B 可用时，同一请求下一次尝试不能再次选中 A。
2. 账号 A 返回 429，账号 B 不支持当前模型时，不应错误切到 B。
3. 账号 A 返回 429，所有账号都冷却时，应进入调度等待或返回明确调度错误。
4. 429 不应计入普通 failure_count，不应导致账号永久禁用。

### 6.2 P1：账号级 token bucket

当前 `credentialRpm` 更接近最小间隔控制。建议升级为 token bucket。

新增配置建议：

```json
{
  "credentialRpm": 6,
  "credentialBurst": 2
}
```

语义：

```text
每个账号维护一个 token bucket。
bucket 容量 = credentialBurst。
每秒补充 credentialRpm / 60 个 token。
每个上游请求消耗 1 个 token。
token 不足时调度器优先选择其他账号。
所有账号 token 不足时进入调度等待。
```

验收标准：

1. `credentialRpm = 6`、`credentialBurst = 2` 时，单账号允许短时间 2 个突发请求。
2. 长期平均调度速率不应超过每分钟 6 个请求。
3. 多实例部署时，token bucket 状态必须通过 Redis 共享。
4. 本地单实例部署时，内存状态即可。

### 6.3 P1：429 自适应降速

429 后只冷却不够。账号恢复后如果立刻按原 RPM 调度，可能再次触发 429。

建议新增运行时状态：

```text
effective_rpm
rate_limit_strike_count
rate_limit_degraded_until
```

目标行为：

```text
正常 rpm = credentialRpm。
第一次 429：effective_rpm = credentialRpm * 0.5，持续 10 分钟。
第二次 429：effective_rpm = credentialRpm * 0.25，持续 30 分钟。
第三次及以上 429：effective_rpm = max(1, credentialRpm * 0.1)，持续 60 分钟。
连续成功 20 次后，effective_rpm 逐步恢复。
```

验收标准：

1. 连续 429 的账号调度频率必须持续下降。
2. 成功稳定后账号必须能自动恢复，不需要手动重启。
3. 降速状态要出现在 admin UI 和快照 API 中。

### 6.4 P1：Soft sticky

Sticky 应保留，但不能让一个账号无限承压。

建议新增配置：

```json
{
  "stickyMaxConsecutiveRequests": 3,
  "stickyMinIntervalSecs": 10,
  "stickyFallbackWhenRateLimited": true,
  "stickyFallbackWhenRecentErrorRateAbove": 0.2
}
```

目标行为：

```text
同一会话优先使用绑定账号。
如果绑定账号连续承接该会话请求达到 stickyMaxConsecutiveRequests，且有其他可用账号，则临时 fallback。
如果绑定账号距离上次调度不足 stickyMinIntervalSecs，且有其他可用账号，则临时 fallback。
如果绑定账号近期错误率超过阈值，则临时 fallback。
fallback 不必立即改绑，除非原绑定账号持续不可用。
```

验收标准：

1. 粘性不是绝对绑定。
2. 绑定账号并发满、限速、冷却、近期错误率高时必须 fallback。
3. fallback 后原绑定仍可在恢复后继续使用。

### 6.5 P2：模型级限速与并发

建议新增模型级配置：

```json
{
  "modelLimits": {
    "claude-opus-*": {
      "globalConcurrent": 5,
      "credentialConcurrent": 1,
      "credentialRpm": 3
    },
    "claude-sonnet-*": {
      "globalConcurrent": 20,
      "credentialConcurrent": 1,
      "credentialRpm": 6
    },
    "claude-haiku-*": {
      "globalConcurrent": 30,
      "credentialConcurrent": 2,
      "credentialRpm": 12
    }
  }
}
```

模型级限制优先级：

```text
凭据级配置 > 模型级配置 > 全局配置
```

验收标准：

1. Opus 请求不能把 Sonnet/Haiku 的可用容量全部占满。
2. 模型级全局并发达到上限时，同模型请求排队或返回明确错误。
3. 不同模型之间的限速状态相互隔离。

### 6.6 P2：代理/IP 维度限速

如果多个账号共用同一个代理或出口 IP，上游限流可能不是账号级，而是代理/IP 级。

建议新增维度：

```text
proxy_id / proxy_resource_id
proxy_rpm
proxy_global_concurrent
proxy_cooldown_until
```

验收标准：

1. 同一代理下多个账号不能无限叠加请求。
2. 代理维度出现 429 或网络错误时，应降低该代理下所有账号的调度权重。
3. 代理不可用时，不应误判所有账号坏。

## 7. 推荐落地顺序

### 7.1 立即执行

先通过配置降低账号风险：

```json
{
  "loadBalancingMode": "health_balanced",
  "credentialMaxConcurrentRequests": 1,
  "credentialRpm": 6,
  "credentialRateLimitCooldownSecs": 90,
  "credentialCooldownBackoffMultiplier": 2.0,
  "credentialCooldownJitterPercent": 30,
  "credentialProbationSecs": 120,
  "credentialMaxCooldownSecs": 900,
  "credentialDispatchMaxWaitSecs": 60,
  "dispatchMaxQueuedRequests": 100
}
```

同时按可用账号数量设置：

```text
dispatchGlobalMaxConcurrentRequests = min(可用账号数, 20)
```

### 7.2 下一次代码迭代优先级

优先做：

1. 429 后当前请求链路立即排除当前账号并换号。
2. MCP 分支同步该行为。
3. 日志链路明确显示 `rate_limit_exclude_and_retry`。
4. admin UI 显示 429 冷却、最近错误时间、有效并发、账号级并发覆盖。

然后做：

1. token bucket。
2. 429 自适应降速。
3. soft sticky。

最后做：

1. 模型级限流。
2. 代理/IP 维度限流。
3. 更精细的 429 body 分类。

## 8. 风险与取舍

### 8.1 降低 429 会降低峰值吞吐

`credentialRpm`、单账号并发、全局并发降低后，短时间峰值吞吐会下降。但这是必要取舍，因为账号被 429 或风控后整体可用性会更差。

### 8.2 排队会增加用户感知延迟

启用 `dispatchMaxQueuedRequests` 和 `credentialDispatchMaxWaitSecs` 后，请求在高峰时会等待。等待比直接打爆账号更可控，但需要在 UI 或错误响应中明确提示排队/限流状态。

### 8.3 Sticky 和平均调度存在冲突

Sticky 越强，账号越容易承压不均；平均调度越强，会话连续性越弱。推荐使用 soft sticky，而不是绝对 sticky。

### 8.4 429 不应直接禁用账号

429 通常是限流或高峰，不代表账号永久不可用。正确做法是冷却、降速、降低权重，而不是永久禁用。

## 9. 当前发版包含的相关能力

当前代码已经包含以下与调度稳定性相关的能力：

1. 普通 `401/403` 处理在 API 和 MCP 分支收敛。
2. 普通 `401/403` 可以在当前请求链路排除当前账号并换号。
3. API sticky 绑定账号发生普通凭据失败后会解除绑定。
4. MCP 请求也支持本次请求临时排除列表。
5. 账号风控/锁定文本识别更完整。
6. sticky 账号并发满时可以 fallback 到其他账号，并发释放后仍回到原绑定账号。
7. 支持凭据级最大并发覆盖值。
8. admin UI 与 API 已包含一批配置和凭据管理能力调整。

当前代码尚未包含以下能力：

1. 429 后当前请求链路立即排除账号并换号。
2. 真正 token bucket。
3. 429 自适应降速。
4. soft sticky 的连续请求阈值和最小间隔阈值。
5. 模型级限速。
6. 代理/IP 维度限速。

## 10. 最终结论

当前系统已有基础的调度、并发、限速、冷却和故障转移能力，但对账号频繁请求导致的 429 仍偏被动。

短期最有效方案是：

```text
health_balanced
+ 单账号并发 1
+ 单账号 RPM 6 到 10
+ 429 冷却 60 到 90 秒
+ 冷却退避和 jitter
+ 合理全局并发
+ 有界等待队列
```

中期最关键的代码改造是：

```text
429 后当前请求链路立即排除当前账号并换号
+ token bucket
+ 429 自适应降速
+ soft sticky
```

长期应进一步实现：

```text
模型级限流
+ 代理/IP 维度限流
+ 429 body 分类
+ admin UI 可观测性
```

最终目标是：

```text
请求分布更平均；
账号不被短时间连续打爆；
sticky 保留但不死粘；
429 后快速切走并保护账号；
冷却结束后渐进恢复；
高峰时排队削峰，而不是把压力直接打到上游。
```
