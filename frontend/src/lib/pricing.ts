import type { ModelPrice, UsageRecord } from '@/types/api'

/**
 * 把 model 名归一化,匹配 model_prices 表里的 key。
 *
 * 例如 `anthropic.claude-opus-4-7-v1:0` → `claude-opus-4-7`
 */
export function normalizeModelKey(model: string): string[] {
  const out = new Set<string>()
  out.add(model)
  out.add(model.toLowerCase())
  const noVer = model.split(':')[0]
  if (noVer) out.add(noVer)
  if (model.startsWith('anthropic.')) out.add(model.slice('anthropic.'.length).split(':')[0])
  if (model.startsWith('anthropic/')) out.add(model.slice('anthropic/'.length))
  for (const prefix of ['us.', 'eu.', 'apac.']) {
    if (model.startsWith(prefix)) out.add(model.slice(prefix.length))
  }
  return Array.from(out)
}

export function findPriceForModel(
  model: string,
  prices: ModelPrice[] | undefined,
): ModelPrice | null {
  if (!prices || prices.length === 0) return null
  const candidates = normalizeModelKey(model)
  for (const cand of candidates) {
    const hit = prices.find((p) => p.modelId === cand)
    if (hit) return hit
  }
  // 模糊匹配:子串
  const lower = model.toLowerCase()
  return (
    prices.find((p) => lower.includes(p.modelId.toLowerCase())) ??
    prices.find((p) => p.modelId.toLowerCase().includes(lower)) ??
    null
  )
}

/**
 * 在前端用 model_prices 估算单条 UsageRecord 的成本。
 * 4 个 token 维度全配齐才能给出确切值;缺失时按合理回退估算。
 */
export function estimateCostUsd(
  record: Pick<
    UsageRecord,
    | 'totalInputTokens'
    | 'compatInputTokens'
    | 'billableInputTokens'
    | 'outputTokens'
    | 'cacheReadInputTokens'
    | 'cacheCreationInputTokens'
    | 'model'
  >,
  prices: ModelPrice[] | undefined,
): number | null {
  const price = findPriceForModel(record.model, prices)
  if (!price || price.inputCostPerToken == null || price.outputCostPerToken == null) {
    return null
  }
  // 用 billable 优先(剔除被缓存覆盖的部分),否则用 compat
  const baseInput =
    record.billableInputTokens > 0
      ? record.billableInputTokens
      : record.compatInputTokens
  const inputCost = baseInput * (price.inputCostPerToken ?? 0)
  const outputCost = record.outputTokens * (price.outputCostPerToken ?? 0)
  const readCost =
    record.cacheReadInputTokens *
    (price.cacheReadInputTokenCost ?? (price.inputCostPerToken ?? 0) * 0.1)
  const writeCost =
    record.cacheCreationInputTokens *
    (price.cacheCreationInputTokenCost ?? (price.inputCostPerToken ?? 0) * 1.25)
  return inputCost + outputCost + readCost + writeCost
}

/**
 * 把 usage_source 转成"说人话"的标签。
 */
export function usageSourceLabel(source: UsageRecord['usageSource']): string {
  switch (source) {
    case 'upstream_metadata':
      return '上游真实'
    case 'local_prompt_cache':
      return '本地缓存推算'
    case 'context_estimate':
      return '上下文估算'
    case 'request_estimate':
      return '请求估算'
    default:
      return '无缓存数据'
  }
}

export function usageStatusLabel(status: UsageRecord['status']): string {
  switch (status) {
    case 'success':
      return '成功'
    case 'error':
      return '错误'
    case 'stream_error':
      return '流错误'
    case 'upstream_timeout':
      return '上游超时'
    case 'client_dropped':
      return '客户端断开'
    default:
      return status
  }
}
