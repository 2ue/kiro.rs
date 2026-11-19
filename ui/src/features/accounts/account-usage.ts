// 账号卡片上的 usage 摘要：按“账号 overrides 优先、其次全局”算出生效值，被单账号覆盖的项单独标出。
// 只做纯计算，渲染（悬停说明）在卡片组件里。
import type { Account, AccountOverrides, AccountsConfig } from '@/types/api'

export type SettingSource = 'global' | 'account'

export interface EffectiveSetting<T> {
  value: T
  source: SettingSource
  /** 全局值，用于说明“覆盖前是多少”。 */
  global: T
}

export interface EffectiveUsageCompensation {
  cacheUpliftPercent: EffectiveSetting<number>
  costFloorEnabled: EffectiveSetting<boolean>
  costFloorMarginPercent: EffectiveSetting<number>
  outputUpliftMinTokens: EffectiveSetting<number>
  outputUpliftPercent: EffectiveSetting<number>
}

export type UsageCompensationConfig = Pick<
  AccountsConfig,
  | 'usageProjectionUpliftPercent'
  | 'usageProjectionCostFloorEnabled'
  | 'usageProjectionCostFloorMarginPercent'
  | 'usageProjectionOutputUpliftMinTokens'
  | 'usageProjectionOutputUpliftPercent'
  | 'streamResponseMode'
>

export const GLOBAL_USAGE_LOCATION = '运行配置 → 账号池 → 用量补偿'
export const ACCOUNT_OVERRIDE_LOCATION = '编辑账号 → 覆盖全局参数'

/** 运行配置页按 `?tab=` 直达账号池分组（页签名或分组名，见 runtime-tabs）。 */
export const RUNTIME_LINKS = {
  capacity: '/runtime?tab=capacity',
  retry: '/runtime?tab=retry',
  cooldown: '/runtime?tab=cooldown',
  disable: '/runtime?tab=disable',
  timeouts: '/runtime?tab=timeouts',
  uplift: '/runtime?tab=uplift',
} as const

export interface SettingLocation {
  label: string
  to: string
}

function pick<T>(override: T | null | undefined, global: T): EffectiveSetting<T> {
  if (override === null || override === undefined) return { value: global, source: 'global', global }
  return { value: override, source: 'account', global }
}

/** 与后端 AccountOverrides::merge 一致：单账号写了值（非 null）就用它，否则用全局。 */
export function effectiveUsageCompensation(
  overrides: AccountOverrides | null | undefined,
  config: UsageCompensationConfig,
): EffectiveUsageCompensation {
  const o = overrides ?? {}
  return {
    cacheUpliftPercent: pick(o.usageProjectionUpliftPercent, config.usageProjectionUpliftPercent),
    costFloorEnabled: pick(o.usageProjectionCostFloorEnabled, config.usageProjectionCostFloorEnabled),
    costFloorMarginPercent: pick(o.usageProjectionCostFloorMarginPercent, config.usageProjectionCostFloorMarginPercent),
    outputUpliftMinTokens: pick(o.usageProjectionOutputUpliftMinTokens, config.usageProjectionOutputUpliftMinTokens),
    outputUpliftPercent: pick(o.usageProjectionOutputUpliftPercent, config.usageProjectionOutputUpliftPercent),
  }
}

export interface UsageSummaryPart {
  key: 'usage' | 'stream' | 'cache' | 'costFloor' | 'output'
  text: string
  /** 这一项的生效值来自单账号覆盖。 */
  overridden: boolean
  /** 含义与当前值来源。 */
  detail: string
  /** 全局设置位置，悬停说明里渲染成可点的链接。 */
  global?: SettingLocation
  /** 单账号设置位置。 */
  account: string
  /** 完整的纯文本说明：含义、来源、在哪里改。 */
  hint: string
}

const USAGE_GLOBAL: SettingLocation = { label: GLOBAL_USAGE_LOCATION, to: RUNTIME_LINKS.uplift }

function sourceText(overridden: boolean, globalText: string): string {
  return overridden ? `当前值来自单账号覆盖（全局为 ${globalText}），在单账号里留空即恢复全局。` : '当前沿用全局值。'
}

function part(fields: Omit<UsageSummaryPart, 'hint'>): UsageSummaryPart {
  const where = [fields.global ? `全局在“${fields.global.label}”` : null, `单账号在“${fields.account}”`].filter(Boolean).join('；')
  return { ...fields, hint: `${fields.detail}${where}。` }
}

const outputText = (minTokens: number, percent: number) =>
  minTokens > 0 && percent > 0 ? `输出 ≥ ${minTokens} 后 +${percent}%` : '关闭'

/**
 * 卡片用量摘要的各段。只有下游 usage 为“按路径整理”时补偿才生效，这时才列出补偿项；
 * 生效值为关闭的项默认不显示，但被单账号覆盖成关闭时要显示出来，否则看不出账号和全局不一样。
 */
export function accountUsageSummaryParts(
  account: Pick<Account, 'usageProjectionMode' | 'streamResponseMode' | 'overrides'>,
  config: UsageCompensationConfig,
): UsageSummaryPart[] {
  const shaping = account.usageProjectionMode === 'current_path_policy'
  const parts: UsageSummaryPart[] = [
    part({
      key: 'usage',
      text: shaping ? 'Usage：按路径整理' : 'Usage：透传上游',
      overridden: false,
      detail: shaping
        ? '返回给下游的 usage 按当前入口路径的缓存策略整理，后面的补偿项只对这种账号生效。'
        : '返回给下游的 usage 保持上游原始值，不做补偿；用量补偿只对“按路径整理”的账号生效。',
      global: shaping ? USAGE_GLOBAL : undefined,
      account: '编辑账号 → 调度设置 → 下游 usage',
    }),
  ]
  const streamMode = account.streamResponseMode || config.streamResponseMode
  const streamOverridden = Boolean(account.streamResponseMode)
  parts.push(part({
    key: 'stream',
    text: streamMode === 'event_passthrough' ? '流式：事件透传' : '流式：继承默认',
    overridden: streamOverridden,
    detail: streamOverridden ? '这个账号单独指定了 SSE 转发方式。' : '沿用全局 SSE 转发方式。',
    global: { label: '运行配置 → 账号池 → 超时与流式', to: RUNTIME_LINKS.timeouts },
    account: '编辑账号 → 上游 Header 兼容 → SSE 转发',
  }))
  if (!shaping) return parts

  const effective = effectiveUsageCompensation(account.overrides, config)

  const cache = effective.cacheUpliftPercent
  const cacheOverridden = cache.source === 'account'
  if (cache.value > 0 || cacheOverridden) {
    parts.push(part({
      key: 'cache',
      text: cache.value > 0 ? `缓存 +${cache.value}%` : '缓存补偿：关',
      overridden: cacheOverridden,
      detail: `缓存读写补偿：有上游缓存证据时按百分比补缓存字段，不超过当前路径上限；0 表示关闭。${sourceText(cacheOverridden, cache.global > 0 ? `+${cache.global}%` : '关闭')}`,
      global: USAGE_GLOBAL,
      account: ACCOUNT_OVERRIDE_LOCATION,
    }))
  }

  const floor = effective.costFloorEnabled
  const margin = effective.costFloorMarginPercent
  const floorOverridden = floor.source === 'account' || margin.source === 'account'
  if (floor.value || floor.source === 'account') {
    const globalText = floor.global ? `开启 +${margin.global}%` : '关闭'
    parts.push(part({
      key: 'costFloor',
      text: floor.value ? `成本底线 +${margin.value}%` : '成本底线：关',
      overridden: floorOverridden,
      detail: `成本底线：下游 usage 为“按路径整理”时，返回给调用方的 usage 折算成本不低于上游原始成本，再多补余量百分比（当前 ${margin.value}%）；有缓存证据时先补缓存字段，不超过路由上下限；补过的请求在统计里记为“兜底”。${sourceText(floorOverridden, globalText)}`,
      global: USAGE_GLOBAL,
      account: ACCOUNT_OVERRIDE_LOCATION,
    }))
  }

  const minTokens = effective.outputUpliftMinTokens
  const outputPercent = effective.outputUpliftPercent
  const outputOverridden = minTokens.source === 'account' || outputPercent.source === 'account'
  const outputActive = minTokens.value > 0 && outputPercent.value > 0
  if (outputActive || outputOverridden) {
    parts.push(part({
      key: 'output',
      text: outputActive ? outputText(minTokens.value, outputPercent.value) : '输出补偿：关',
      overridden: outputOverridden,
      detail: `输出补偿：输出 token 达到阈值后按百分比放大；阈值或百分比为 0 时关闭。${sourceText(outputOverridden, outputText(minTokens.global, outputPercent.global))}`,
      global: USAGE_GLOBAL,
      account: ACCOUNT_OVERRIDE_LOCATION,
    }))
  }
  return parts
}

/** 纯文本形式（用于导出、测试和无法显示悬停说明的地方）。 */
export function accountUsageSummaryText(parts: UsageSummaryPart[]): string {
  return parts.map((part) => (part.overridden ? `${part.text}（单账号覆盖）` : part.text)).join(' · ')
}
