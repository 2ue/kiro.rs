# P2 - 外部池流式成功请求大量 0 计费

## 现象

生产 114 中，Usage 明细存在大量成功请求：

- `status=success`
- `routeKind=external_pool`
- `estimatedCostUsd=0`
- `pricingAvailable=false`
- `pricingModel` 为空
- `totalInputTokens` 有值，但 `outputTokens=0`
- `externalPoolBilling` 为空

最初怀疑是 `claude-opus-4-8` 这类模型没有匹配到 `opus-4.8` 价格。

## 生产证据

24h 内 0 计费成功请求大头集中在外部池：

- `claude-opus-4-8`
- `claude-sonnet-4-6`
- `claude-opus-4-6`
- `claude-sonnet-5`
- `claude-haiku-4-5-20251001`

进一步复核：

- PgSQL `model_pricing` 表有 `claude-opus-4-8`、`claude-sonnet-4-6`、`claude-opus-4-6`、`claude-sonnet-5` 等价格。
- Admin `/model-pricing` 返回的内存 catalog 也有这些模型价格。
- 当前分支已有 `claude-opus-4-8` 与 `claude-opus-4.8` 互配测试。

因此主因不是模型价格表缺失，也不是模型 alias 映射失败。

按 114 迁移重启时间之后重新统计：

- 非流式外部池成功请求已经有 billing/计费；
- 0 计费主要剩流式外部池成功请求；
- 这些流式 0 计费记录没有 `externalPoolBilling`。

## 根因

外部池非流式成功缺 upstream usage 时，代码已有估算兜底：

- 按请求 input token 和响应文本估算 output token；
- 注入/记录 estimated usage；
- 生成 `ExternalPoolBilling`。

外部池流式成功路径不同：

- 只有捕获到上游 SSE usage event 时才生成 billing；
- 如果上游流式响应没有 usage event，完成时 `billing=None`；
- `record_external` 只能把请求 input 估算写入 `totalInputTokens`，但无法写 `externalPoolBilling`、`pricingModel`、`estimatedCostUsd`；
- 最终表现为成功请求 0 计费。

## 复现方法

构造一个外部池流式成功响应：

```text
event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello world"}}
```

不发送任何 `usage` event，然后完成流。

旧逻辑结果：

- UsageRecord 成功；
- `externalPoolBilling=None`；
- `estimatedCostUsd=0`；
- `pricingAvailable=false`。

## 修复方案

### 1. 流式输出 token 轻量估算

`ExternalStreamUsageGuard` 在下游 SSE chunk 上累计输出 token：

- `content_block_delta.text_delta.text`
- `content_block_delta.thinking_delta.thinking`
- `content_block_delta.input_json_delta.partial_json`
- `content_block_start` 的 text/thinking/redacted/tool_use/server_tool_use
- OpenAI-compatible `choices[].delta.content`

只解析已经准备发给下游的 bounded SSE chunk，不读取请求正文，不保存原文。

### 2. 流式 missing usage billing fallback

流结束时：

1. 先尝试沿用 captured upstream usage；
2. 如果没有 captured usage，则用：
   - request input token；
   - 累计 output token；
   - 当前 external pool usage projection policy；
   - 当前共享 `PricingCatalog`
3. 构造 `ExternalPoolBilling`：
   - `usageEstimated=true`
   - `usageEstimateReason=missing_stream_usage`
   - `usageCandidatePath=$stream.estimated`
   - `pricingAvailable=true`（当价格目录可匹配）
   - `billableCostUsd>0`

模型调用、模型映射、外部池请求 body/response 转发不变；只补 UsageRecord 和计费记录。

## 验证

新增单测：

- `stream_output_token_estimator_counts_text_thinking_and_tool_events`
- `stream_missing_usage_builds_estimated_billable_external_pool_billing`

关联计价 alias 单测：

- `estimate_matches_dashed_request_to_dotted_price_model`
- `estimate_matches_dotted_request_to_dashed_price_model`

发布后生产验证：

```sql
SELECT
  stream,
  data->>'externalPoolId' AS pool_id,
  count(*) AS requests,
  count(*) FILTER (WHERE data->'externalPoolBilling' IS NOT NULL) AS billing_present,
  count(*) FILTER (WHERE estimated_cost_usd > 0) AS priced
FROM usage_records
WHERE created_at >= now() - interval '30 minutes'
  AND status = 'success'
  AND data->>'routeKind' = 'external_pool'
GROUP BY stream, data->>'externalPoolId';
```

预期：

- 新增流式成功请求中 `billing_present` 接近成功数；
- 对无上游 usage 的流式请求，`externalPoolBilling.usageEstimateReason=missing_stream_usage`；
- `estimated_cost_usd > 0`。

