import type { BadgeProps } from '@/components/ui'
import type { UsageRecord, UsageSource } from '@/types/api'
import { formatNumber } from '@/lib/format'

export type BillingDeltaTone = 'loss' | 'profit' | 'even'

export function billingDeltaTone(delta: number): BillingDeltaTone {
  if (delta < 0) return 'loss'
  if (delta > 0) return 'profit'
  return 'even'
}

export function billingDeltaTextClass(tone: BillingDeltaTone): string {
  if (tone === 'loss') return 'text-destructive'
  if (tone === 'profit') return 'text-warning'
  return 'text-muted-foreground'
}

export function billingDeltaBadgeTone(tone: BillingDeltaTone): NonNullable<BadgeProps['tone']> {
  if (tone === 'loss') return 'error'
  if (tone === 'profit') return 'warning'
  return 'success'
}

export function formatLatency(value?: number): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return '-'
  if (value < 1000) return `${formatNumber(Math.round(value))}ms`
  return `${(value / 1000).toFixed(2)}s`
}

export function sourceLabel(source: UsageSource): string {
  const labels: Record<UsageSource, string> = {
    upstream_metadata: '服务返回用量',
    local_prompt_cache: '本地缓存估算',
    context_estimate: '上下文估算',
    request_estimate: '请求估算',
    none: '无缓存',
  }
  return labels[source] || source
}

export function statusLabel(status: string): string {
  const labels: Record<string, string> = {
    success: '成功',
    error: '错误',
    stream_error: '流错误',
    upstream_timeout: '服务超时',
    client_dropped: '客户端断开',
  }
  return labels[status] || status
}

export function statusTone(status: string): NonNullable<BadgeProps['tone']> {
  if (status === 'success') return 'success'
  if (status === 'client_dropped') return 'warning'
  return 'error'
}

export function routeLabel(record: UsageRecord): string {
  const labels: Record<string, string> = {
    local_success: '本地成功',
    local_error_no_fallback: '本地错误',
    local_rescue_after_external: '外部账号后回本地',
    external_fallback_preflight: '预检 fallback',
    external_fallback_after_local_attempts: '失败后 fallback',
    external_direct_policy: '外部直连',
    external_route_policy: '路由策略',
    external_error: '外部错误',
  }
  return labels[record.routeSubtype || ''] || (record.routeKind === 'external_pool' ? '外部账号' : '本地')
}

export function routeTone(record: UsageRecord): NonNullable<BadgeProps['tone']> {
  if (record.routeSubtype === 'external_direct_policy') return 'warning'
  if (record.routeSubtype === 'external_route_policy') return 'warning'
  if (record.routeSubtype === 'local_rescue_after_external') return 'info'
  if (record.routeKind === 'external_pool') return record.status === 'success' ? 'info' : 'error'
  return record.status === 'success' ? 'success' : 'neutral'
}

export function attemptActionLabel(action: string): string {
  const labels: Record<string, string> = {
    success: '成功',
    retry: '重试',
    transient_retry: '重试',
    fail: '失败',
    disable_and_retry: '禁用后重试',
    failure_count_and_retry: '计失败后重试',
    force_refresh_and_retry: '刷新后重试',
  }
  return labels[action] || action || '-'
}

export function attemptOutcomeLabel(
  record: NonNullable<UsageRecord['credentialAttempts']>[number]
): string {
  if (typeof record.status === 'number') return String(record.status)
  if (record.errorType) return record.errorType
  return attemptActionLabel(record.action)
}

export function formatAttemptChain(record: UsageRecord): string {
  return (record.credentialAttempts || [])
    .map((attempt) => `#${attempt.credentialId}(${attemptOutcomeLabel(attempt)})`)
    .join(' > ')
}

export function formatAttemptSummary(record: UsageRecord): string {
  const attempts = record.credentialAttempts || []
  if (attempts.length === 0) return '无本地尝试'
  const uniqueCredentialIds = new Set(attempts.map((attempt) => attempt.credentialId))
  if (attempts.length === 1) {
    return record.fallbackFromSticky ? '本地尝试 1 次 · sticky换号' : '本地尝试 1 次'
  }
  if (uniqueCredentialIds.size <= 1)
    return `本地尝试 ${attempts.length} 次 · 同账号重试 ${attempts.length - 1} 次`
  return `本地尝试 ${attempts.length} 次 · 切换 ${uniqueCredentialIds.size} 个账号`
}

export function formatExternalAttemptChain(record: UsageRecord): string {
  return (record.externalAttempts || [])
    .map(
      (attempt) =>
        `外部账号 #${attempt.poolId}(${attempt.status ?? attempt.errorType ?? attempt.action})`
    )
    .join(' > ')
}

export function upstreamModelLabel(record: UsageRecord): string {
  const model = record.routeKind === 'external_pool'
    ? record.externalOutboundModel || record.upstreamModel || record.model || '-'
    : record.upstreamModel || record.model || '-'
  if (record.routeKind === 'external_pool' && record.externalOutboundModel) return model
  const source = record.modelResolutionSource ? `（${record.modelResolutionSource}）` : ''
  return `${model}${source}`
}

export function resolvedModelLabel(record: UsageRecord): string {
  const model = record.upstreamModel || record.model || '-'
  const source = record.modelResolutionSource ? `（${record.modelResolutionSource}）` : ''
  return `${model}${source}`
}
