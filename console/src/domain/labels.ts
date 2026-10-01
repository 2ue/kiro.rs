import type { CredentialStatusItem, LoadBalancingMode, UsageRecord, UsageRecordStatus, UsageSource } from '@/api/types'

export function authMethodLabel(method: string | null | undefined): string {
  switch (method) {
    case 'api_key':
      return 'API Key'
    case 'idc':
      return 'IdC'
    case 'external_idp':
      return '外部 IdP'
    case 'social':
      return 'Social'
    default:
      return method || '未知'
  }
}

export type SubscriptionTier = 'power' | 'pro_max' | 'pro_plus' | 'pro' | 'free' | 'trial' | 'other' | 'unknown'

export function subscriptionTier(title: string | null | undefined): SubscriptionTier {
  if (!title) return 'unknown'
  const t = title.toLowerCase().replace(/[_\s-]+/g, ' ')
  if (t.includes('power')) return 'power'
  if (t.includes('pro max')) return 'pro_max'
  if (t.includes('pro plus') || t.includes('pro+')) return 'pro_plus'
  if (t.includes('pro')) return 'pro'
  if (t.includes('free')) return 'free'
  if (t.includes('trial') || t.includes('试用')) return 'trial'
  return 'other'
}

export const SUBSCRIPTION_LABEL: Record<SubscriptionTier, string> = {
  power: 'Power',
  pro_max: 'Pro Max',
  pro_plus: 'Pro+',
  pro: 'Pro',
  free: 'Free',
  trial: 'Trial',
  other: '其他',
  unknown: '未知',
}

export const PROXY_SOURCE_LABEL: Record<CredentialStatusItem['effectiveProxySource'], string> = {
  credential: '账号代理',
  resource: '代理资源',
  resource_disabled: '代理已禁用',
  resource_missing: '代理不存在',
  global: '全局代理',
  direct: '直连',
  none: '无代理',
}

export const LOAD_BALANCING_LABEL: Record<LoadBalancingMode, { label: string; desc: string }> = {
  priority: { label: '优先级', desc: '总是先用优先级最高的可用账号' },
  balanced: { label: '均衡负载', desc: '在可用账号之间平均分配请求' },
  health_balanced: { label: '健康均衡', desc: '综合错误率、延迟、负载等评分选择账号' },
  weighted_least_inflight: { label: '低负载优先', desc: '高并发时优先选择在途请求少的账号' },
}

export const RECORD_STATUS_LABEL: Record<UsageRecordStatus, string> = {
  success: '成功',
  error: '错误',
  stream_error: '流中断',
  upstream_timeout: '上游超时',
  client_dropped: '客户端断开',
}

export const USAGE_SOURCE_LABEL: Record<UsageSource, string> = {
  upstream_metadata: '上游上报',
  local_prompt_cache: '本地缓存模拟',
  context_estimate: '上下文估算',
  request_estimate: '请求估算',
  none: '无',
}

const ROUTE_SUBTYPE_LABEL: Record<NonNullable<UsageRecord['routeSubtype']>, string> = {
  local_success: '本地',
  local_error_no_fallback: '本地（未兜底）',
  local_rescue_after_external: '外部失败后本地救援',
  external_fallback_preflight: '外部池（预检兜底）',
  external_fallback_after_local_attempts: '外部池（本地失败后）',
  external_direct_policy: '外部池（直连策略）',
  external_route_policy: '外部池（路由规则）',
  external_error: '外部池（失败）',
}

export function routeLabel(r: Pick<UsageRecord, 'routeKind' | 'routeSubtype'>): string {
  if (r.routeSubtype) return ROUTE_SUBTYPE_LABEL[r.routeSubtype] ?? r.routeSubtype
  return r.routeKind === 'external_pool' ? '外部池' : '本地'
}

const ATTEMPT_ACTION_LABEL: Record<string, string> = {
  success: '成功',
  retry: '重试',
  transient_retry: '临时错误重试',
  fail: '失败',
  disable_and_retry: '禁用后换号',
  failure_count_and_retry: '计失败后换号',
  force_refresh_and_retry: '刷新 Token 后重试',
}

export function attemptActionLabel(action: string): string {
  return ATTEMPT_ACTION_LABEL[action] ?? action
}

export function isErrorStatus(status: string): boolean {
  return status !== 'success'
}
