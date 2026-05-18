# High Cache Token Amplification Strategy

本文档记录 high-cache token 放大方案、实施约束和验证清单。当前实现已落地核心配置、high-cache 专用 scale、deterministic soft cap、小请求保护，以及真实 metadata cache 优先规则。

## 目标

在 `promptCacheSimulationMode = "high-cache"` 的本地模拟路径中，让返回给下游和 Admin 的缓存字段更大、更像高缓存场景：

1. `cache_read_input_tokens`
2. `cache_creation_input_tokens`
3. `cache_creation_5m_input_tokens`
4. `cache_creation_1h_input_tokens`

核心目标是让数字更大但行为仍然自洽，不影响真实上游 cache usage。

## 非目标

1. 不修改真实 Kiro metadata 中已经存在的 cache read/write。
2. 不全局修改 `token::count_tokens` 的 token 估算口径。
3. 不对每个请求硬塞固定 cache 大数。
4. 不让短测试请求凭空出现很大的 cache read/write。
5. 不让大量请求频繁卡在同一个 `promptCacheMaxSimulatedInputTokens` 上限值。

## 当前计算基础

当前 high-cache 模拟的核心关系是：

```text
target_tokens = round(total_input_tokens * effective_cache_ratio)
```

其中：

1. `effective_cache_ratio` 来自 `promptCacheTargetReadRatio` 附近的确定性浮动。
2. ratio 会被限制在 `0.0..0.99`。
3. cache token 总量不会超过 `total_input_tokens - 1`。
4. 首次同 scope 请求通常表现为 creation/write。
5. 后续同 scope 且稳定 prefix 命中后表现为 read。

因此只提高 `promptCacheTargetReadRatio` 的放大能力有限。如果 `total_input_tokens = 10000`，即使 ratio 从 `0.95` 提到 `0.99`，cache token 也只是从约 `9500` 提到约 `9900`。

真正让 cache read/write 明显变大的关键是 high-cache 模拟使用的 `total_input_tokens`。

## 推荐新增配置

建议新增 high-cache 专用配置：

```json
{
  "promptCacheTargetReadRatio": 0.98,
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000
}
```

字段含义：

1. `promptCacheTargetReadRatio`：cache token 占 simulated input 的中心比例。
2. `promptCacheTokenScale`：只在 high-cache 模拟中放大 total input，不影响真实 usage。
3. `promptCacheMaxSimulatedInputTokens`：模拟 total input 的硬上界。
4. `promptCacheCapJitterMinTokens`：触顶时从上限扣减的最小抖动 token。
5. `promptCacheCapJitterMaxTokens`：触顶时从上限扣减的最大抖动 token。
6. `promptCacheScaleMinInputTokens`：只有基础输入达到该门槛才启用 scale，避免短测试请求被放大。

## 默认值建议

推荐默认：

```json
{
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000
}
```

理由：

1. `300000` 允许长会话呈现更大的 high-cache usage，但仍由 cap jitter 避免固定贴顶。
2. `1.6` 能让约 188k 级别的基础输入进入 300k soft-cap 区间；配合 `0.98` 时比 `1.8` 更稳。
3. `12000..24000` 的 cap jitter 是明显的 k 级波动，触顶后 soft cap 约落在 `276000..288000`。
4. `20000` 的 scale 门槛能过滤常见健康检查、hello/test、短 prompt。

偏激进配置可以是：

```json
{
  "promptCacheTargetReadRatio": 0.99,
  "promptCacheTokenScale": 1.5,
  "promptCacheMaxSimulatedInputTokens": 200000,
  "promptCacheCapJitterMinTokens": 5000,
  "promptCacheCapJitterMaxTokens": 20000,
  "promptCacheScaleMinInputTokens": 10000
}
```

偏激进配置适合联调和展示，不建议作为默认。

## 推荐公式

建议 high-cache 模拟使用以下步骤：

```text
local_total = 本地请求 token 估算
metadata_total = upstream metadata 中的 total input，如果存在
base_total = max(local_total, metadata_total)

if base_total < promptCacheScaleMinInputTokens:
    simulated_total = base_total
else:
    scaled_total = round(base_total * promptCacheTokenScale)
    soft_cap = promptCacheMaxSimulatedInputTokens - deterministic_cap_jitter
    simulated_total = min(scaled_total, soft_cap)

cache_total = round(simulated_total * effective_cache_ratio)
cache_total = min(cache_total, simulated_total - 1)
```

注意：

1. `base_total` 使用 `max(local_total, metadata_total)`，避免 metadata total 偏小时压低 high-cache 模拟。
2. scale 只在 high-cache 模拟中生效。
3. cap jitter 只在 `scaled_total` 触达或超过上限时有实际影响。
4. jitter 作用于 `simulated_total`，不要直接作用于 read/write。
5. 最后仍然保留 `input_tokens >= 1` 的自洽约束。

## 封顶波动策略

不建议使用：

```text
simulated_total = min(scaled_total, promptCacheMaxSimulatedInputTokens)
```

如果上限是 `200000`，大量请求会出现相同或极接近的 total/cache 数字，看起来不自然。

建议把 cap 变成 deterministic soft cap：

```text
soft_cap = promptCacheMaxSimulatedInputTokens - jitter
simulated_total = min(scaled_total, soft_cap)
```

jitter 规则：

```text
jitter_min = promptCacheCapJitterMinTokens
jitter_max = min(promptCacheCapJitterMaxTokens, promptCacheMaxSimulatedInputTokens * 0.08)
jitter = deterministic_hash(profile fingerprint) % (jitter_max - jitter_min + 1) + jitter_min
```

如果 `promptCacheMaxSimulatedInputTokens = 200000` 且 jitter 是 `5000..20000`：

```text
soft_cap 范围约 180000..195000
```

如果 `promptCacheMaxSimulatedInputTokens = 300000` 且 jitter 是 `12000..24000`：

```text
soft_cap 范围约 276000..288000
```

使用 profile fingerprint 做 deterministic jitter 的原因：

1. 同一稳定 prefix 的结果稳定，不会每次随机跳。
2. 不同请求、不同会话、不同模型之间会自然变化。
3. 并发请求不会因为随机数产生不可解释的 usage 抖动。

## 真实 Cache 优先级

必须保留真实 metadata 优先：

```text
if metadata.cache_read_input_tokens > 0 || metadata.cache_write_input_tokens > 0:
    use metadata as-is
```

只有以下情况才允许 high-cache 模拟接管：

1. 没有 metadata。
2. metadata 存在，但 `cacheReadInputTokens = 0` 且 `cacheWriteInputTokens = 0`。

这样可以避免覆盖真实上游已经给出的 cache 行为。

## 小请求保护

短请求不应被放大出高 cache。建议保留多层保护：

1. 原有最小 cacheable tokens：普通模型 `1024`，Opus `4096`。
2. 新增 `promptCacheScaleMinInputTokens`，默认 `20000`。
3. `base_total < promptCacheScaleMinInputTokens` 时不启用 `promptCacheTokenScale`。
4. 可选：`base_total < 1024` 时不做 high-cache 模拟，只返回普通 usage。

典型小请求：

```json
{"messages":[{"role":"user","content":"hello"}]}
```

预期行为：

```text
cache_creation_input_tokens = 0
cache_read_input_tokens = 0
```

或最多保持当前低 token 行为，不应出现几万、十几万 cache token。

## Creation 和 Read 行为

需要保留当前 tracker 的自然语义：

首次同 scope 请求：

```text
cache_creation_input_tokens > 0
cache_read_input_tokens = 0
```

后续同 scope、同模型、稳定 prefix 命中：

```text
cache_read_input_tokens > 0
cache_creation_input_tokens = 0 或较小
```

多轮会话增长时，较自然的结果是：

```text
cache_read_input_tokens 较大
cache_creation_input_tokens 有少量新增
```

不要为了让 read 大而让首次请求直接出现大 read，这比数字偏小更容易显得异常。

## Scope 和负载均衡

cache read 是否稳定，还取决于 scope：

```text
credential_id + conversation_id + model
```

需要注意：

1. 同一会话切换 credential 会导致 read 下降或重新 creation。
2. model 名不同会隔离 cache。
3. 没有稳定 conversation id 的请求不应跨请求共享 cache。
4. high-cache token 放大不应绕开这些 scope 规则。

如果目标是稳定高 read，需要保持 sticky session 到同一 credential。

## 极端行为覆盖

实施时至少需要覆盖以下边界：

1. `promptCacheTokenScale <= 0`：应视为 `1.0` 或拒绝配置。
2. `promptCacheTokenScale` 过大：应 clamp 到合理上限，例如 `3.0`。
3. `promptCacheMaxSimulatedInputTokens <= 0`：应关闭 cap 或回退默认值，不应 panic。
4. `promptCacheMaxSimulatedInputTokens < promptCacheScaleMinInputTokens`：应保证逻辑仍可用，或加载配置时报错。
5. `promptCacheCapJitterMinTokens > promptCacheCapJitterMaxTokens`：应交换、clamp 或回退默认值。
6. jitter 大于 cap：`soft_cap` 必须至少大于 `1`，避免负数或 0。
7. `scaled_total <= promptCacheMaxSimulatedInputTokens`：不应强行扣 cap jitter。
8. `base_total` 很小：不应生成高 cache。
9. `metadata` 有真实 cache：不得覆盖。
10. `metadata` cache 为 0 但 total 很小、本地估算很大：应使用 `max(metadata_total, local_total)`。
11. 并发同 scope 请求：不得产生不自洽的负 creation/read。
12. stream 中途失败或 client disconnect：不应写入 tracker 导致后续 read。
13. process restart：本地 cache 清空，首次请求应重新 creation。
14. credential 禁用/删除：对应 entries 清空，不能继续 read。
15. Opus 模型：仍应使用更高的 min cacheable tokens。
16. `cache_read + cache_creation >= simulated_total`：必须 clamp，保留至少 1 个 uncached input token。
17. cap 触顶请求很多：soft cap 应随 profile 变化，不应大量固定同一个值。
18. profile fingerprint 缺失：使用稳定 fallback jitter，例如 cap 中位扣减，不使用随机。

## 建议测试清单

单元测试：

1. 真实 metadata cache 非 0 时不被 scale/cap 覆盖。
2. metadata cache 为 0 时 high-cache 使用本地模拟。
3. `base_total = max(metadata_total, local_total)`。
4. `base_total < promptCacheScaleMinInputTokens` 不启用 scale。
5. `scaled_total` 未触顶时不扣 cap jitter。
6. `scaled_total` 触顶时使用 deterministic soft cap。
7. 同一 profile 的 soft cap 稳定。
8. 不同 profile 的 soft cap 有 k 级别差异。
9. cap jitter min/max 配置异常时不会 panic。
10. read/write 总和不超过 simulated total。
11. 首次请求 creation，第二次请求 read。
12. 不同 credential/conversation/model 不共享 read。
13. Opus min cacheable tokens 保持生效。
14. 小请求不出现高 cache。

集成测试：

1. 非流式 `/v1/messages` 首次 creation，第二次 read。
2. 流式 `/v1/messages` 首次 creation，第二次 read。
3. metadata cache 为 0 的 high-cache fallback 会放大 cache token。
4. 真实 metadata cache 非 0 的响应保持真实值。
5. 多个触顶请求不会全部显示同一个 max cap 附近固定值。
6. Admin usage records 能看到 simulated/source 字段解释该行为。

## 推荐实施顺序

1. 先新增配置字段和默认值，不改变真实 metadata 路径。
2. 在 high-cache fallback 的 total input 计算处引入 `base_total = max(metadata_total, local_total)`。
3. 增加 `promptCacheTokenScale`，只对满足门槛的 base total 生效。
4. 增加 deterministic soft cap，避免固定触顶。
5. 保持现有 ratio 计算和 tracker first creation/second read 语义。
6. 补齐单元测试和少量集成测试。
7. 再根据真实 Admin usage 分布微调默认值。

## 最终建议

默认建议使用温和放大：

```json
{
  "promptCacheTargetReadRatio": 0.98,
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000
}
```

这个方案的特点：

1. cache read/write 会明显变大。
2. 不覆盖真实 cache。
3. 不污染全局 token 估算。
4. 短请求不会被夸张放大。
5. 触顶时有 k 级别 deterministic 波动。
6. 仍保留首次 creation、后续 read 的自然行为。
