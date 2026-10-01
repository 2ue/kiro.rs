import type { CredentialStatusItem } from '@/api/types'
import { disabledReasonMeta, type DisabledReasonMeta } from '@/domain/disabled-reason'

/** 全局唯一的账号状态集合；颜色/图标映射见 components/status/status-badge.tsx */
export type PrimaryStatus =
  | 'disabled'
  | 'failing'
  | 'rateLimited'
  | 'cooling'
  | 'saturated'
  | 'probation'
  | 'warmup'
  | 'busy'
  | 'healthy'

export interface CredentialStatusView {
  primary: PrimaryStatus
  /** 主状态之外仍然成立的状态（用于 hover 说明） */
  secondary: PrimaryStatus[]
  /** 主状态的人话说明 */
  detail?: string
  /** 冷却/限流/观察期剩余秒数（主状态相关） */
  remainingSecs?: number
  disabledReason?: DisabledReasonMeta & { code: string }
  /** 当前是否会被调度选中：禁用、冷却、限流、并发满均为 false */
  schedulable: boolean
}

type StatusInput = Pick<
  CredentialStatusItem,
  | 'disabled'
  | 'disabledReason'
  | 'cooledDown'
  | 'cooldownRemainingSecs'
  | 'cooldownReason'
  | 'cooldowns'
  | 'rateLimited'
  | 'rateLimitRemainingSecs'
  | 'inFlightRequests'
  | 'maxConcurrentRequests'
  | 'inProbation'
  | 'probationRemainingSecs'
  | 'warmupRemaining'
  | 'failureCount'
  | 'refreshFailureCount'
  | 'lastErrorKind'
  | 'lastErrorReason'
  | 'recentErrorRate'
>

/** 近期错误率超过该值视为"异常"（仍可调度，但需要关注） */
const FAILING_ERROR_RATE = 0.5

export const STATUS_PRIORITY: PrimaryStatus[] = [
  'disabled',
  'rateLimited',
  'cooling',
  'saturated',
  'failing',
  'probation',
  'warmup',
  'busy',
  'healthy',
]

/**
 * 从后端原始字段推导唯一主状态。
 * 优先级：禁用 > 限流 > 冷却 > 并发满 > 异常 > 观察期 > 预热 > 在用 > 可调度。
 * 前四种会让账号暂时不可被调度。
 */
export function deriveCredentialStatus(c: StatusInput): CredentialStatusView {
  const states = new Set<PrimaryStatus>()
  if (c.disabled) states.add('disabled')
  if (c.rateLimited) states.add('rateLimited')
  const globalCooldown = c.cooledDown || (c.cooldowns ?? []).some((item) => item.global && item.remainingSecs > 0)
  if (globalCooldown) states.add('cooling')
  if (c.maxConcurrentRequests > 0 && c.inFlightRequests >= c.maxConcurrentRequests) states.add('saturated')
  const failing =
    (c.recentErrorRate ?? 0) >= FAILING_ERROR_RATE || c.failureCount > 0 || c.refreshFailureCount > 0
  if (failing) states.add('failing')
  if (c.inProbation) states.add('probation')
  if (c.warmupRemaining > 0) states.add('warmup')
  if (c.inFlightRequests > 0) states.add('busy')

  const primary = STATUS_PRIORITY.find((s) => states.has(s)) ?? 'healthy'
  const secondary = STATUS_PRIORITY.filter((s) => s !== primary && states.has(s))
  const schedulable = !c.disabled && !c.rateLimited && !globalCooldown && !states.has('saturated')

  const view: CredentialStatusView = { primary, secondary, schedulable }
  switch (primary) {
    case 'disabled': {
      const meta = disabledReasonMeta(c.disabledReason ?? 'Manual')
      if (meta) view.disabledReason = { ...meta, code: c.disabledReason ?? 'Manual' }
      view.detail = meta?.hint
      break
    }
    case 'rateLimited':
      view.remainingSecs = c.rateLimitRemainingSecs
      view.detail = '收到上游限流响应，等待恢复'
      break
    case 'cooling':
      view.remainingSecs = c.cooldownRemainingSecs
      view.detail = c.cooldownReason ? `冷却原因：${c.cooldownReason}` : '出错后暂停调度'
      break
    case 'saturated':
      view.detail = `并发已满 ${c.inFlightRequests}/${c.maxConcurrentRequests}`
      break
    case 'failing':
      view.detail = c.lastErrorReason || c.lastErrorKind || '近期出现错误'
      break
    case 'probation':
      view.remainingSecs = c.probationRemainingSecs
      view.detail = '刚从错误中恢复，降低调度权重观察中'
      break
    case 'warmup':
      view.detail = `新账号预热中，剩余 ${c.warmupRemaining} 次请求`
      break
    case 'busy':
      view.detail = `正在处理 ${c.inFlightRequests} 个请求`
      break
    default:
      view.detail = '可正常调度'
  }
  return view
}

/** 只有模型级冷却（非全局）时返回这些模型，用于展示"部分模型冷却" */
export function modelCooldowns(c: Pick<CredentialStatusItem, 'cooldowns'>) {
  return (c.cooldowns ?? []).filter((item) => !item.global && item.remainingSecs > 0)
}
