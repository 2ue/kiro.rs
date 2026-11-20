import type { BadgeProps } from '@/components/ui'
import type { UsageRecord, UsageSource } from '@/types/api'
import { formatNumber } from '@/lib/format'
import { routePrefixForEndpoint, UNKNOWN_USAGE_ROUTE } from '@/features/routes/route-utils'

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

/** 调度去向：转发到账号，或本地处理（本地缓存命中等）。 */
export function routeLabel(record: UsageRecord): string {
  return record.routeKind === 'account' ? '账号' : '本地'
}

/** 请求所属的路由前缀，从入口路径推出，与总览、用量筛选同一口径；推不出时写“未识别入口”。 */
export function usageRoutePrefixLabel(record: Pick<UsageRecord, 'endpoint'>): string {
  const route = routePrefixForEndpoint(record.endpoint)
  return route === UNKNOWN_USAGE_ROUTE ? '未识别入口' : route
}

export function routeTone(record: UsageRecord): NonNullable<BadgeProps['tone']> {
  if (record.routeKind === 'account') return record.status === 'success' ? 'info' : 'error'
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

export function formatExternalAttemptChain(record: UsageRecord): string {
  return (record.externalAttempts || [])
    .map(
      (attempt) =>
        `账号 #${attempt.poolId}(${attempt.status ?? attempt.errorType ?? attempt.action})`
    )
    .join(' > ')
}

export function upstreamModelLabel(record: UsageRecord): string {
  const model = record.routeKind === 'account'
    ? record.externalOutboundModel || record.upstreamModel || record.model || '-'
    : record.upstreamModel || record.model || '-'
  if (record.routeKind === 'account' && record.externalOutboundModel) return model
  const source = record.modelResolutionSource ? `（${record.modelResolutionSource}）` : ''
  return `${model}${source}`
}

/** 上游实际返回的模型与发给上游的模型不一致时返回上游返回值，否则返回 null。 */
export function upstreamResponseModelMismatch(record: UsageRecord): string | null {
  const returned = record.upstreamResponseModel?.trim()
  if (!returned) return null
  const sent = record.externalOutboundModel || record.upstreamModel || record.model
  return returned.toLowerCase() === (sent || '').trim().toLowerCase() ? null : returned
}

export function resolvedModelLabel(record: UsageRecord): string {
  const model = record.upstreamModel || record.model || '-'
  const source = record.modelResolutionSource ? `（${record.modelResolutionSource}）` : ''
  return `${model}${source}`
}

/** 错误发生阶段的中文说明；未知值原样返回，方便发现漏项。 */
export function errorSourceLabel(source?: string): string {
  const labels: Record<string, string> = {
    downstream_client: '下游客户端断开',
    external_account: '账号上游响应',
    external_account_stream: '账号上游流式',
    external_stream: '账号上游流式',
    external_dispatch: '账号调度',
    admission: '请求准入',
    body_extractor: '请求体读取',
    payload_guard: '请求体保护',
    gateway: '网关',
  }
  return source ? labels[source] ?? source : '-'
}

/** 流式结束原因的中文说明。 */
export function terminalReasonLabel(reason?: string): string {
  const labels: Record<string, string> = {
    completed: '正常完成',
    upstream_status_error: '上游状态错误',
    upstream_json_exception: '上游 JSON 异常',
    upstream_idle_timeout: '上游空闲超时',
    first_output_timeout: '首输出超时',
    upstream_max_duration: '超过最长时长',
    malformed_sse: 'SSE 格式错误',
    protocol_contamination: '协议污染',
    client_dropped: '客户端断开',
    internal_error: '内部错误',
  }
  return reason ? labels[reason] ?? reason : '-'
}

/** 模型解析来源的中文说明，键与后端 `ModelResolutionSource` 的取值一一对应；未知值原样返回。 */
export function modelResolutionSourceLabel(source?: string): string {
  const labels: Record<string, string> = {
    exact_upstream: '精确匹配',
    manual: '手动模型',
    alias: '别名',
    family_normalized: '同家族归一',
    pass_through: '直接透传',
    unsupported: '不支持的模型',
  }
  return source ? labels[source] ?? source : '-'
}
