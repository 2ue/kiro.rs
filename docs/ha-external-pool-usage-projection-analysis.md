# /ha 外部池高缓存输入偏大分析记录

记录时间：2026-06-14

## 背景

现网在 `/ha/v1/messages` 路径下出现过类似下面的 usage 形态：

```text
input_tokens: 33,329
output_tokens: 315
cache_read_input_tokens: 174.6K
```

直觉上 `/ha` 是高缓存路径，应该把可见输入压低，并把大部分上下文转成 `cache_read_input_tokens`。因此需要解释为什么仍然会看到“输入很大，同时又有高 cache read”的记录。

本次排查严格按只读方式进行：

1. 不重启现网服务。
2. 不修改配置文件。
3. 不写数据库。
4. 不调用任何写入型 Admin API。
5. 只读取进程、容器、运行时配置、日志片段和 usage 记录。

本文档不记录 SSH、Admin key、凭据 label、请求密钥等敏感信息。

## 结论

`/ha` 下“input 很大，同时 cache_read 也很高”的记录，主要不是普通本地凭据链路的 `/ha` 高缓存策略失效，而是出现在外部池 fallback 链路：

```text
routeKind = external_pool
routeSubtype = external_fallback_preflight 或 external_fallback_after_local_attempts
usageProjectionMode = current_path_policy
```

普通本地凭据成功链路的 `/ha` 记录仍符合预期：`compat_input_tokens` 会被压到 `/ha` 策略上限以内，同时 `cache_read_input_tokens` 很高。

真正的问题是：外部池 `current_path_policy` 的路径整形目前发生在中间阶段，后续还有 prompt-cache creation 频率控制、外部池 uplift、SSE usage merge 等步骤会继续改变 usage；最后只重新应用了 `finalCacheReadMaxTokens` 这类 cache read guard，没有重新保证 `/ha.input.maxTokens` 是最终约束。因此外部池记录可能最终表现为：

```text
/ha 路径 + 高 cache_read + input 仍然很大
```

## 现网只读观察

生产服务版本：

```text
kiro-rs 0.0.48
```

运行时配置中 `/ha` 的关键 usage 策略为：

```json
{
  "reportedUsage": {
    "pathOverrides": {
      "/ha": {
        "enabled": true,
        "input": {
          "mode": "sample-max",
          "maxTokens": 500,
          "moveDeltaToCacheRead": true
        },
        "cacheRead": {
          "mode": "preserve"
        },
        "cacheCreation": {
          "mode": "sample-target",
          "targetTokens": 150000,
          "normalMaxMultiplier": 1.5
        },
        "finalCacheReadMaxTokens": 500000,
        "finalCacheReadJitterMinTokens": 40342,
        "finalCacheReadJitterMaxTokens": 60256
      }
    }
  },
  "promptCacheTargetReadRatio": 0.99,
  "promptCacheTokenScale": 2.0,
  "promptCacheMaxSimulatedInputTokens": 300000
}
```

最近 24 小时 `/ha` 的本地凭据成功记录聚合显示：

```text
route = local_credential
usage_source = local_prompt_cache
compat_input_tokens <= 500
avg compat_input_tokens ~= 129
avg cache_read_input_tokens ~= 160K
```

这说明普通本地 `/ha` 高缓存上报策略是生效的。

再筛选 `/ha` 下同时满足下面条件的记录：

```text
cache_read_input_tokens > 0
compat_input_tokens > 1000
```

现网样本全部落在外部池链路：

```text
routeKind = external_pool
usage_source = local_prompt_cache
usageProjectionMode = current_path_policy
```

典型样本的字段形态如下：

```text
routeKind: external_pool
routeSubtype: external_fallback_preflight
usageProjectionMode: current_path_policy

rawUsage.inputTokens: 18192
rawUsage.cacheReadInputTokens: 52405

shapedUsage.inputTokens: 40883
shapedUsage.cacheReadInputTokens: 105074

reportedUsage.inputTokens: 40883
reportedUsage.cacheReadInputTokens: 141850
```

这类样本说明：大 input 并不是普通本地 `/ha` 成功路径产生的，而是在外部池 fallback 的 usage projection 过程中形成并最终落库/返回。

## 当前外部池 usage 处理顺序

外部池只有在 pool 的 `usageProjectionMode = current_path_policy` 时才会启用路径投影。

### 1. 读取外部池返回 usage

非流式响应在 `maybe_project_non_stream_usage` 中解析 body 的 `usage` 字段，并先把它记录为 `rawUsage`：

```rust
let raw_usage = cache_usage_from_value(usage);
usage_capture.raw = raw_usage;
usage_capture.reported = raw_usage;
```

流式响应在每个 SSE 事件里遇到 `usage` 时，也会先解析原始 usage，再进入投影流程。

### 2. 构建外部池投影上下文

`build_external_usage_projection_context` 只在 `CurrentPathPolicy` 下返回上下文：

```rust
if pool.usage_projection_mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
    return None;
}
```

它会从当前请求 payload 重新计算本地 raw input：

```rust
let raw_input_tokens = count_external_route_input_tokens(&route.payload);
```

然后用当前路径、当前模型、prompt-cache tracker 和 high-cache 参数生成模拟缓存 usage：

```rust
let profile = route.prompt_cache.build_high_cache_profile_for_model(...);
let prompt_usage = route.prompt_cache.compute(...);
let simulated_usage =
    CacheSimulation::from_prompt_cache_with_ratio_and_amplification(...);
```

### 3. 按当前路径选择 reportedUsage 策略

这里不是先套一个“通用策略”再套 `/ha`，而是直接用当前 endpoint 做路径匹配：

```rust
let reported_policy = ReportedCacheUsagePolicy::from_path_policy(
    route.reported_usage.policy_for_path(route.endpoint),
    ...
);
```

对 `/ha/v1/messages` 来说，正常应该命中 `/ha` override。

### 4. 先生成 computed usage

`project_usage_value` 内先把 prompt-cache 模拟结果转成 `computed` usage：

```rust
let computed = projection
    .simulated_usage
    .map(|simulation| {
        CacheSimulation::to_usage(simulation, projection.raw_input_tokens, output_tokens)
    })
    .unwrap_or_else(|| CacheUsage {
        total_input_tokens: projection.raw_input_tokens,
        input_tokens: projection.raw_input_tokens,
        output_tokens,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    });
```

这一步是外部池路径自己的“基础高缓存模拟/重算”。

### 5. 套当前路径 reportedUsage 策略

随后把 `computed` 套上路径策略：

```rust
let reported = projection
    .reported_policy
    .clone()
    .map(|policy| computed.with_reported_cache_usage_policy_and_raw(policy, raw_usage))
    .unwrap_or(computed);
```

对 `/ha` 来说，这一步会把 `input_tokens` 按 `sample-max` 压低，并且因为 `moveDeltaToCacheRead = true`，会把 input 的差值转入 `cache_read_input_tokens`。

到这里为止，路径整形是生效的。

### 6. 后续 prompt-cache creation 控制会继续改 usage

路径策略之后，会进入 creation 频率控制：

```rust
let shaped = projection.prompt_cache_creation_controller.preview_success(
    projection.scope.as_ref(),
    projection.prompt_cache_creation_control,
    reported,
);
```

关键点在 `PromptCacheCreationController::with_allowed_creation`：如果本次 `cache_creation_input_tokens` 被频率控制压低，被抑制的 creation 会加回 `input_tokens`：

```rust
let suppressed_creation = original_creation.saturating_sub(allowed_creation);
let input_tokens = usage.input_tokens.saturating_add(suppressed_creation);
```

这意味着：

1. `/ha` 刚刚把 `input_tokens` 压到了上限以内。
2. 后续 creation-control 可能又把大量被抑制的 creation 转回 input。
3. 因此最终 `input_tokens` 可能重新变大。

这是“已经按路径整形，但后续又不像 `/ha` 约束”的主要原因之一。

### 7. 外部池 uplift 继续应用

creation-control 后还会执行外部池自己的 uplift：

```rust
let projected = shaped
    .with_external_pool_usage_uplift(projection.uplift_percent)
    .with_external_pool_output_uplift(...);
```

当前 `with_external_pool_usage_uplift` 会放大：

```text
cache_read_input_tokens
cache_creation_input_tokens
```

但不会重新压低 `input_tokens`：

```rust
input_tokens: self.input_tokens,
```

因此，如果前一步已经让 input 变大，uplift 不会修正它。

### 8. 最后只执行 cache_read guard

最后只重新执行了：

```rust
policy.apply_final_cache_read_guard(projected)
```

这一步只限制最终 `cache_read_input_tokens`，不会再次执行：

```text
/ha.input.maxTokens
/ha.input.moveDeltaToCacheRead
/ha.cacheCreation.sample-target
```

因此 `/ha.input.maxTokens` 当前不是外部池 projection 链路的最终不变量。

### 9. billing 记录 raw/shaped/reported

外部池记录最终会把三套 usage 进入 `externalPoolBilling`：

```text
rawUsage      外部池原始返回 usage
shapedUsage   路径策略后、外部池最终补偿前的 usage
reportedUsage 最终返回给下游并用于记录的 usage
```

成本也按这三套 usage 分别估算：

```text
rawCostUsd
shapedCostUsd
reportedCostUsd / billableCostUsd
profitUsd = reportedCostUsd - rawCostUsd
```

## 回答当前问题

问题：

> 现在的外部池计算逻辑是先按照通用的进行计算？然后再使用外部池的配置计算？实际设置按路径整形知识第一步？后续可能没有按照这个来？

准确回答：

1. 外部池不是简单“先通用 default，再外部池配置”。
2. 在 `current_path_policy` 下，它会先基于当前请求 payload 和 prompt-cache/high-cache 逻辑重算一个 `computed` usage。
3. 然后按当前请求路径选择 `reportedUsage.policy_for_path(endpoint)`，所以 `/ha` 会命中 `/ha` override。
4. 也就是说，路径整形确实发生了，而且发生在外部池投影的较早阶段。
5. 但是路径整形不是最后一步。
6. 后续的 creation-control 可能把被抑制的 `cache_creation_input_tokens` 加回 `input_tokens`。
7. 外部池 uplift 又会继续改 cache read/cache creation/output。
8. 最后只重新做了 `cache_read` 上限保护，没有重新套完整 `/ha` input 上限。
9. 所以最终结果可能不再严格满足 `/ha.input.maxTokens`。

## 根因

根因不是 `/ha` 路径没有匹配，也不是本地普通高缓存策略完全失效，而是外部池 `current_path_policy` 的处理阶段划分不够严格：

```text
路径策略是中间整形步骤，不是最终上报约束。
```

当前外部池链路的实际顺序可以概括为：

```text
外部池 raw usage
  -> 本地 payload 重新计数
  -> prompt-cache/high-cache 模拟 computed usage
  -> 当前路径 reportedUsage 策略
  -> prompt-cache creation-control
  -> 外部池 cache/output uplift
  -> final cache_read guard
  -> 返回给下游并落库
```

其中 `prompt-cache creation-control` 可以增加 input，`final cache_read guard` 不会再压 input，因此出现 `/ha` 下大 input 和高 cache read 共存。

## 为什么本地凭据链路没有同样问题

普通本地凭据成功链路里，`/ha` usage 最终会通过 `reported_usage_for_downstream` 和 `ensure_reported_usage_for_record` 处理，现网观测结果也显示：

```text
routeKind = local_credential
usage_source = local_prompt_cache
compat_input_tokens <= 500
```

所以普通本地 `/ha` 路径目前符合配置预期。

大 input + high cache_read 的样本主要来自：

```text
routeKind = external_pool
usageProjectionMode = current_path_policy
```

## 修复方向

不要简单在最后再调用一次现有的 `with_reported_cache_usage_policy_and_raw`，因为它会重新按 raw input 计算 delta，并可能把 input delta 重复转入 cache read，造成双算。

更稳妥的修复方向是把外部池 usage projection 拆成更清晰的阶段。

### 方案 A：把路径策略变成最终约束

重构顺序：

```text
computed high-cache usage
  -> creation-control
  -> external-pool uplift
  -> final path reported usage policy
  -> final cache_read guard
```

这样 `/ha.input.maxTokens` 就是最后一道规则。

需要注意：最终 path policy 必须只执行一次完整 delta 迁移，避免重复把 input 差值加到 cache read。

### 方案 B：新增 finalize 函数，只处理最终不变量

新增一个专门用于外部池最终阶段的函数，例如：

```rust
finalize_external_path_reported_usage(projected, raw_usage, policy)
```

它需要明确做到：

1. 对 `/ha.input.sample-max` 做最终 cap。
2. 如果 `moveDeltaToCacheRead = true`，把最终 input delta 转入 cache read。
3. 应用 `finalCacheReadMaxTokens` 和 jitter。
4. 不重复执行已经完成过的 cache creation sample。
5. 不破坏 raw/shaped/reported billing 快照语义。

这个方案比“最后完整套一次旧 policy”更安全。

### 方案 C：creation-control 不再把 suppressed creation 加回 input

这是行为变更较大的方案，不建议直接采用。

当前 creation-control 把被抑制的 creation 加回 input，是为了避免“隐藏 creation 后总输入成本突然降低”。如果直接取消，可能影响本地链路和成本统计，不应作为第一选择。

## 建议补充测试

需要补以下测试覆盖外部池路径：

1. 非流式 `/ha` + `current_path_policy` + creation-control 抑制 creation 后，最终 `input_tokens <= /ha.maxTokens`。
2. 流式 SSE 多个 usage event 下，最终 merge 后仍不突破 `/ha.maxTokens`。
3. 外部池 uplift 后，最终仍执行 `/ha` input cap 与 cache read guard。
4. `externalPoolBilling.rawUsage/shapedUsage/reportedUsage` 的含义保持稳定。
5. 本地凭据 `/ha` 链路不受影响。
6. `/v1`、`/cc`、`/na` 路径维持各自原策略，不因为 `/ha` 修复被误伤。

## 运维建议

在修复上线前，使用记录页面应尽量展示以下字段，否则容易误判：

```text
routeKind
routeSubtype
usageSource
compatInputTokens
billableInputTokens
cacheCreationInputTokens
cacheReadInputTokens
externalPoolBilling.usageProjectionMode
externalPoolBilling.rawUsage
externalPoolBilling.shapedUsage
externalPoolBilling.reportedUsage
```

判断规则：

1. 如果 `routeKind = local_credential` 且 `/ha` input 仍然很大，再看本地 reportedUsage 是否失效。
2. 如果 `routeKind = external_pool` 且 `usageProjectionMode = current_path_policy`，优先按本文外部池投影链路分析。
3. 如果页面显示的是 `billableInputTokens`，要注意它等于 `inputTokens + cacheCreationInputTokens`，即使 `compatInputTokens` 很小，也可能因为 creation 很大而显示成大输入。

## 后续处理建议

1. 不建议直接在现网手工改配置或重启来规避。
2. 先在本地修复外部池 projection 的最终路径约束。
3. 增加外部池 `/ha` 专项单元测试。
4. 发版后再观察 `/ha` 下 `routeKind=external_pool` 的大 input 记录是否消失。
5. 同时在使用日志详情里保留 raw/shaped/reported 三套 usage，避免后续成本分析失去依据。

## 本地修复记录

本地采用“方案 B”的窄修复：不在最后完整重跑一次 `with_reported_cache_usage_policy_and_raw`，而是新增一个最终 input guard，只处理最终 reported usage 的 input 不变量。

修改点：

1. `ReportedCacheUsagePolicy::apply_final_input_guard`
   - 只在 `reportedUsage` 启用且 usage 有 prompt-cache 字段时生效。
   - 复用当前路径的 input sampling 规则。
   - 如果最终 `input_tokens` 仍大于路径期望，会再次采样到路径上限内。
   - 如果 `moveDeltaToCacheRead = true`，把被压掉的 input delta 加到 `cache_read_input_tokens`。
   - 不重新处理 output、cache creation，也不重新按 raw usage 做整套路径改写。

2. 外部池 `project_usage_value` 最终阶段调整为：

```text
computed high-cache usage
  -> 当前路径 reportedUsage 策略
  -> prompt-cache creation-control
  -> external-pool cache/output uplift
  -> final input guard
  -> final cache_read guard
  -> 返回给下游并落库
```

选择这个顺序的原因：

1. `creation-control` 把 suppressed creation 加回 input 后，final input guard 能把这部分重新移到 cache read。
2. external-pool uplift 仍然可以作用在 cache read / cache creation 上，尽量减少收益损失。
3. final cache read guard 仍作为最后一道上限，避免 cache read 异常超大。
4. `rawUsage` 和 `shapedUsage` 仍保留原始/中间口径，成本分析不丢信息。

新增回归测试：

```text
usage_projection_final_input_guard_reapplies_path_input_limit_after_uplift
```

该测试覆盖：

1. 外部池 `current_path_policy`。
2. 路径配置 `input.sample-max`。
3. 外部池 uplift 后，最终 `reported.input_tokens` 仍在路径上限内。
4. 压掉的 input 会体现在 cache read 中。
5. `total_input_tokens = input + cache_read + cache_creation` 仍保持一致。

本地验证结果：

```text
cargo test: 530 passed
pnpm check: passed
git diff --check: passed
```
