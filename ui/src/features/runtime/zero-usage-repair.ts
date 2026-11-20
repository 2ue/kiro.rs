// 路由缓存策略里“零用量估算”的默认值、选项与校验，与后端 ZeroUsageRepairPolicy 一致。
import type { ZeroUsageFallback, ZeroUsageRepairPolicyPatch, ZeroUsageSidePolicyPatch } from '../../types/runtime.ts'

export interface ZeroUsageSidePolicy {
  enabled: boolean
  fallback: ZeroUsageFallback
  minTokens: number
  maxTokens: number
  ratioPercent: number
}

export const ZERO_USAGE_SIDE_DEFAULTS: ZeroUsageSidePolicy = {
  enabled: true,
  fallback: 'keep',
  minTokens: 1,
  maxTokens: 20,
  ratioPercent: 0.5,
}

export const ZERO_USAGE_MAX_TOKENS_LIMIT = 1_000_000
export const ZERO_USAGE_RATIO_PERCENT_LIMIT = 10_000

export const ZERO_USAGE_FALLBACK_OPTIONS: Array<{ value: ZeroUsageFallback; label: string; desc: string }> = [
  { value: 'keep', label: '保持估算', desc: '估算结果为 0 时保持 0，与未开启随机时一致。' },
  { value: 'range', label: '按范围随机', desc: '在下限到上限之间随机取一个整数。' },
  { value: 'ratio', label: '按另一侧比例', desc: '输出按输入、输入按输出的百分比生成，结果收在下限到上限之间。' },
]

/** 未配置的字段取默认值；null 视为未配置。 */
export function resolveZeroUsageSide(patch: ZeroUsageSidePolicyPatch | null | undefined): ZeroUsageSidePolicy {
  return {
    enabled: patch?.enabled ?? ZERO_USAGE_SIDE_DEFAULTS.enabled,
    fallback: patch?.fallback ?? ZERO_USAGE_SIDE_DEFAULTS.fallback,
    minTokens: patch?.minTokens ?? ZERO_USAGE_SIDE_DEFAULTS.minTokens,
    maxTokens: patch?.maxTokens ?? ZERO_USAGE_SIDE_DEFAULTS.maxTokens,
    ratioPercent: patch?.ratioPercent ?? ZERO_USAGE_SIDE_DEFAULTS.ratioPercent,
  }
}

export function zeroUsageSideError(side: ZeroUsageSidePolicy, label: string): string | null {
  if (!Number.isInteger(side.minTokens) || side.minTokens < 1) return `${label}范围下限必须是至少为 1 的整数`
  if (!Number.isInteger(side.maxTokens) || side.maxTokens < side.minTokens || side.maxTokens > ZERO_USAGE_MAX_TOKENS_LIMIT) {
    return `${label}范围上限必须是不小于下限、不超过 ${ZERO_USAGE_MAX_TOKENS_LIMIT.toLocaleString()} 的整数`
  }
  if (!Number.isFinite(side.ratioPercent) || side.ratioPercent <= 0 || side.ratioPercent > ZERO_USAGE_RATIO_PERCENT_LIMIT) {
    return `${label}比例必须大于 0 且不超过 ${ZERO_USAGE_RATIO_PERCENT_LIMIT}%`
  }
  return null
}

export function zeroUsageRepairError(policy: ZeroUsageRepairPolicyPatch | null | undefined): string | null {
  return zeroUsageSideError(resolveZeroUsageSide(policy?.input), '输入') ?? zeroUsageSideError(resolveZeroUsageSide(policy?.output), '输出')
}
