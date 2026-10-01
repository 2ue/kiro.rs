import type { DisabledReasonCode } from '@/api/types'

export type SuggestedAction = 'delete' | 'reauth' | 'wait_reset' | 'reset_and_check' | 'enable' | 'review'

export interface DisabledReasonMeta {
  label: string
  /** 对管理员的一句话解释 */
  hint: string
  action: SuggestedAction
  actionLabel: string
  /** 是否属于不可恢复（需要删除或重新认证） */
  terminal: boolean
}

export const DISABLED_REASONS: Record<DisabledReasonCode, DisabledReasonMeta> = {
  Manual: {
    label: '手动禁用',
    hint: '由管理员手动停用',
    action: 'enable',
    actionLabel: '重新启用',
    terminal: false,
  },
  TooManyFailures: {
    label: '连续失败',
    hint: '请求连续失败达到阈值后自动停用',
    action: 'reset_and_check',
    actionLabel: '重置失败计数并体检',
    terminal: false,
  },
  TooManyRefreshFailures: {
    label: 'Token 刷新失败',
    hint: 'Token 刷新连续失败，可能是网络或认证信息问题',
    action: 'reauth',
    actionLabel: '刷新 Token / 更新认证',
    terminal: false,
  },
  QuotaExceeded: {
    label: '额度用尽',
    hint: '本周期额度已用完，重置后可恢复',
    action: 'wait_reset',
    actionLabel: '等待额度重置',
    terminal: false,
  },
  InvalidRefreshToken: {
    label: 'Refresh Token 失效',
    hint: '上游返回 invalid_grant，需重新登录获取凭据',
    action: 'delete',
    actionLabel: '删除或重新导入',
    terminal: true,
  },
  InvalidConfig: {
    label: '配置无效',
    hint: '凭据字段不完整，例如 API Key 账号缺少 kiroApiKey',
    action: 'reauth',
    actionLabel: '修正认证信息',
    terminal: false,
  },
  TemporarilySuspended: {
    label: '临时风控',
    hint: '上游临时暂停该账号',
    action: 'review',
    actionLabel: '稍后体检',
    terminal: false,
  },
  AccountSuspended: {
    label: '账号封禁',
    hint: '上游返回账号已暂停或封禁',
    action: 'delete',
    actionLabel: '删除',
    terminal: true,
  },
  AccountLocked: {
    label: '账号锁定',
    hint: '上游返回账号已锁定',
    action: 'delete',
    actionLabel: '删除',
    terminal: true,
  },
}

export const DISABLED_REASON_CODES = Object.keys(DISABLED_REASONS) as DisabledReasonCode[]

export function disabledReasonMeta(code: string | null | undefined): DisabledReasonMeta | undefined {
  if (!code) return undefined
  return DISABLED_REASONS[code as DisabledReasonCode] ?? {
    label: code,
    hint: '未识别的禁用原因',
    action: 'review',
    actionLabel: '查看详情',
    terminal: false,
  }
}
