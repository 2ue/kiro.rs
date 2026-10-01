import type { CredentialSummaryResponse, ExternalPoolsStatusResponse, UsageDashboardTop, UsageRecorderStats } from '@/api/types'
import { writerHealth } from '@/shell/system-health'
import type { Tone } from '@/components/status/tone'

export interface AttentionItem {
  id: string
  tone: Tone
  title: string
  detail?: string
  to: string
  search?: Record<string, unknown>
}

/** 汇总"需要关注"的行动队列：每一项都能跳转到对应筛选视图 */
export function buildAttention(input: {
  summary?: CredentialSummaryResponse
  pools?: ExternalPoolsStatusResponse
  writer?: UsageRecorderStats
  top?: UsageDashboardTop
  errorRate?: number
}): AttentionItem[] {
  const items: AttentionItem[] = []
  const { summary, pools, writer, top } = input

  if (summary) {
    if (summary.total > 0 && summary.schedulable === 0) {
      items.push({
        id: 'no-schedulable',
        tone: 'danger',
        title: '没有可调度的本地账号',
        detail: '所有请求只能走外部池或失败',
        to: '/scheduler',
      })
    } else if (summary.total > 0 && summary.schedulable / Math.max(1, summary.available) < 0.3) {
      items.push({
        id: 'low-schedulable',
        tone: 'warning',
        title: `只有 ${summary.schedulable} 个账号可调度`,
        detail: `启用 ${summary.available} 个，冷却 ${summary.coolingDown} 个`,
        to: '/scheduler',
      })
    }
    if (summary.failing > 0)
      items.push({
        id: 'failing',
        tone: 'danger',
        title: `${summary.failing} 个账号近期异常`,
        detail: '查看错误原因并处置',
        to: '/accounts',
        search: { status: 'error' },
      })
    if (summary.disabled > 0)
      items.push({
        id: 'disabled',
        tone: 'neutral',
        title: `${summary.disabled} 个账号已禁用`,
        detail: '按禁用原因批量处置',
        to: '/accounts',
        search: { status: 'disabled' },
      })
    if (summary.queuedRequests > 0)
      items.push({
        id: 'queued',
        tone: 'warning',
        title: `${summary.queuedRequests} 个请求在排队`,
        detail: '本地容量不足，考虑扩容或调整并发',
        to: '/settings/capacity',
      })
    if (!summary.runtimeFresh)
      items.push({
        id: 'stale-runtime',
        tone: 'warning',
        title: '运行态快照可能滞后',
        detail: 'Redis 调度状态未及时同步',
        to: '/scheduler',
      })
  }

  for (const p of pools?.pools ?? []) {
    if (!p.pool.enabled) continue
    if (p.pool.autoDisabled)
      items.push({
        id: `pool-ad-${p.pool.id}`,
        tone: 'danger',
        title: `外部池「${p.pool.name}」被自动禁用`,
        detail: p.pool.autoDisabledReason,
        to: '/pools',
        search: { id: p.pool.id },
      })
    else if (!p.dispatchable)
      items.push({
        id: `pool-${p.pool.id}`,
        tone: 'warning',
        title: `外部池「${p.pool.name}」暂不接流量`,
        detail: p.skippedReason ?? p.cooldownReason ?? (p.cooldownRemainingSecs > 0 ? `冷却 ${p.cooldownRemainingSecs}s` : undefined),
        to: '/pools',
        search: { id: p.pool.id },
      })
  }

  const wh = writerHealth(writer)
  if (wh.tone === 'danger' || wh.tone === 'warning')
    items.push({
      id: 'writer',
      tone: wh.tone,
      title: `用量写入链路：${wh.label}`,
      detail: '部分请求记录可能延迟或丢失',
      to: '/overview',
      search: { health: true },
    })

  if ((input.errorRate ?? 0) >= 0.1 && top?.errors[0])
    items.push({
      id: 'top-error',
      tone: 'danger',
      title: `错误率 ${((input.errorRate ?? 0) * 100).toFixed(1)}%`,
      detail: `最多：${top.errors[0].label ?? top.errors[0].key}`,
      to: '/requests',
      search: { status: 'error' },
    })

  const order: Record<Tone, number> = { danger: 0, orange: 1, warning: 2, info: 3, primary: 4, success: 5, neutral: 6 }
  return items.sort((a, b) => order[a.tone] - order[b.tone])
}
