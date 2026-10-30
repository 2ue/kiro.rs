import { Activity, ChevronRight, Coins, Ellipsis, FlaskConical, Wallet, Zap } from 'lucide-react'
import { Badge, Button, Checkbox, Switch, Tooltip } from '@/components/ui'
import { MiddleTruncate } from '@/components/patterns'
import { formatCompact, formatDateTime, formatFullDate, formatMeteringUsage, formatNumber, formatPercent } from '@/lib/format'
import { cn } from '@/lib/utils'
import type { PublicPoolAccount } from '@/types/api'
import {
  authLabel,
  endpointLabel,
  loadTone,
  quotaBarModel,
  quotaReading,
  subscriptionBadgeMeta,
  successRate,
} from '@/features/credentials/credential-utils'
import {
  AUTH_CLASS,
  Meter,
  QuotaBar,
  STATE_STYLE,
  StatusBadge,
  SUBSCRIPTION_CLASS,
  useCountdown,
} from '@/features/credentials/credential-card'
import { publicAccountLabel, publicAttentionFlags, publicStatusMeta } from './public-pool-model'

/**
 * The account card as the public read-only page shows it: the same layout as the account page,
 * with every control drawn but wired to nothing, and no cost figures.
 */
export function PublicPoolCard({ account }: { account: PublicPoolAccount }) {
  const label = publicAccountLabel(account)
  const fast = account.fast ?? 0
  const fastTier = fast > 0 ? String(Math.min(fast, 3)) : undefined
  const status = publicStatusMeta(account)
  const stateStyle = STATE_STYLE[status.kind]
  const remaining = useCountdown(status.remainingSecs)
  const attentionFlags = publicAttentionFlags(account)
  const dim = account.disabled ? 'opacity-60' : undefined

  const subMeta = subscriptionBadgeMeta({ subscriptionTitle: account.subscriptionTitle, accountInfo: undefined })
  const epLabel = endpointLabel(account.endpoint)
  const quotaBar = quotaBarModel(account.quota)
  const quota = quotaReading(account.quota, quotaBar)
  const quotaToneClass = quotaBar.inOverage
    ? 'text-violet-600 dark:text-violet-400'
    : quotaBar.baseTone === 'error'
      ? 'text-destructive'
      : quotaBar.baseTone === 'warning'
        ? 'text-warning'
        : 'text-foreground'

  const concurrencyLimit = account.maxConcurrentRequests
  const concurrencyText = `${account.inFlightRequests}/${concurrencyLimit > 0 ? concurrencyLimit : '∞'}`
  const recentSelection60s = account.recentSchedulerSelectionCount60s
  const rpmLimit = account.rpm
  const rpmText = `${formatCompact(recentSelection60s)}/${rpmLimit > 0 ? formatCompact(rpmLimit) : '∞'}`
  const success = successRate(account.successCount, account.schedulerSelectionCount)

  return (
    <article
      data-fast-tier={fastTier}
      aria-label={`账号 ${label}`}
      className={cn(
        'group/card relative flex h-full flex-col overflow-hidden rounded-xl bg-card shadow-sm transition-shadow hover:shadow-md',
        fast > 0 ? 'fast-card' : 'border border-border/70',
      )}>
      {stateStyle.edgeClass && <span aria-hidden="true" className={cn('pointer-events-none absolute inset-x-0 top-0 h-1', stateStyle.edgeClass)} />}
      <div className="flex items-center gap-2 px-4 pt-4">
        <Checkbox checked={false} aria-label={`选择 ${label}`} />
        <StatusBadge status={status} remaining={remaining} />
        {fast > 0 && (
          <button type="button" className="fast-badge" title={`Fast ${fast}：优先打满这个账号`} aria-label={`Fast ${fast}`}>
            <Zap className="size-3" aria-hidden="true" fill="currentColor" />FAST {fast}
          </button>
        )}
        <span className="tabular ml-auto font-mono text-xs text-muted-foreground">#{account.id}</span>
        <Switch checked={!account.disabled} aria-label={account.disabled ? '启用账号' : '禁用账号'} title={account.disabled ? '启用账号' : '禁用账号'} />
      </div>

      <div className={cn('px-4 pt-2', dim)}>
        <button type="button" title={label}
          className="-mx-1.5 block w-[calc(100%+0.75rem)] rounded-md px-1.5 py-0.5 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
          <MiddleTruncate text={label} className="text-[0.95rem] font-semibold leading-6 text-foreground" />
        </button>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          <span className={cn('inline-flex h-5 items-center rounded-full px-2 text-[0.68rem] font-semibold', SUBSCRIPTION_CLASS[subMeta.key])} title={subMeta.title}>
            {subMeta.label}
          </span>
          <span className={cn('inline-flex h-5 items-center rounded-full px-2 text-[0.68rem] font-semibold', AUTH_CLASS[account.authMethod ?? ''] ?? 'bg-muted text-muted-foreground')}>
            {authLabel(account.authMethod)}
          </span>
          {epLabel && (
            <span className="inline-flex h-5 items-center rounded-full border border-border px-2 text-[0.68rem] font-medium text-muted-foreground" title={account.endpoint}>
              {epLabel}
            </span>
          )}
        </div>
      </div>

      <div className={cn('px-4 pt-3', dim)}>
        <div className="flex flex-wrap items-baseline gap-x-1.5 text-xs leading-5">
          <span className="mr-auto shrink-0 text-muted-foreground">额度</span>
          {account.quota && account.quota.usageLimit > 0 && quota ? (
            <span className="tabular ml-auto flex flex-wrap items-baseline justify-end gap-x-1.5 text-right"
              title={`查询时间：${formatFullDate(account.quota.checkedAt)}`}>
              <span className={cn('whitespace-nowrap font-semibold', quotaToneClass)}>{quota.percent}</span>
              <span className="whitespace-nowrap font-medium text-foreground">{quota.amount}</span>
            </span>
          ) : (
            <span className="font-medium text-muted-foreground">未查询</span>
          )}
        </div>
        <QuotaBar model={quotaBar} />
      </div>

      {/* Credits used are fine to show; the dollar figures next to them on the account page are not. */}
      <dl className={cn('grid grid-cols-3 gap-3 px-4 pt-3', dim)}>
        <div className="min-w-0">
          <dt className="text-[0.7rem] leading-4 text-muted-foreground">使用积分</dt>
          <dd className="tabular mt-0.5 truncate text-xs font-semibold leading-4 text-foreground" title={formatMeteringUsage(account.kiroMeteringUsage)}>
            {formatMeteringUsage(account.kiroMeteringUsage)}
          </dd>
        </div>
      </dl>

      <div className={cn('grid grid-cols-[1fr_1fr_0.7fr_1.6fr] items-start gap-x-2 px-4 pt-3.5', dim)}>
        <Meter label="并发" value={concurrencyText}
          percent={concurrencyLimit > 0 ? (account.inFlightRequests / concurrencyLimit) * 100 : null}
          tone={loadTone(account.inFlightRequests, concurrencyLimit)} />
        <Meter label="RPM" value={rpmText}
          percent={rpmLimit > 0 ? (recentSelection60s / rpmLimit) * 100 : null}
          tone={loadTone(recentSelection60s, rpmLimit)} />
        <Meter label="优先级" value={account.priority} percent={null} />
        <Meter label="成功/调度"
          wrap
          percent={null}
          value={<>{formatCompact(account.successCount)}/<wbr />{formatCompact(success.dispatched)}</>}
          tone={success.tone === 'success' ? undefined : success.tone}
          title={`成功 ${formatNumber(account.successCount)} / 调度 ${formatNumber(success.dispatched)}${success.percent !== undefined ? `，成功率 ${formatPercent(success.percent / 100)}` : ''}`} />
      </div>

      {(attentionFlags.length > 0 || account.quota?.overageEnabled) && (
        <div className="flex flex-wrap items-center gap-1.5 px-4 pt-3">
          {attentionFlags.map((flag) => (
            <button key={flag.key} type="button" title={flag.title}>
              <Badge tone={flag.tone} className="cursor-pointer">{flag.label}</Badge>
            </button>
          ))}
          {account.quota?.overageEnabled && (
            <Tooltip label="超额调用已开启">
              <span tabIndex={0} className="inline-flex h-5 items-center gap-1 rounded-full bg-violet-500/10 px-2 text-[0.68rem] font-medium text-violet-700 dark:text-violet-300">
                <Coins className="size-3" aria-hidden="true" />超额
              </span>
            </Tooltip>
          )}
        </div>
      )}

      <div className="tabular mt-auto grid grid-cols-2 gap-x-2 px-4 pb-3 pt-2.5 text-[0.7rem] leading-4 text-muted-foreground">
        <span className="min-w-0" title={account.createdAt ? `创建时间：${formatFullDate(account.createdAt)}` : undefined}>
          <span className="block">创建</span>
          <span className="block truncate text-foreground/80">{formatDateTime(account.createdAt)}</span>
        </span>
        <span className="min-w-0 text-right"
          title={account.lastUsedAt ? `最近一次成功请求：${formatFullDate(account.lastUsedAt)}` : '还没有成功请求'}>
          <span className="block">最近成功</span>
          <span className="block truncate text-foreground/80">{account.lastUsedAt ? formatDateTime(account.lastUsedAt) : '未成功过'}</span>
        </span>
      </div>

      <div className="flex items-center gap-0.5 border-t border-border/60 px-2 py-1.5">
        <Button variant="ghost" size="xs"><FlaskConical />测试</Button>
        <Button variant="ghost" size="xs"><Wallet />额度</Button>
        <Button variant="ghost" size="xs" title="查看账号调度与错误诊断"><Activity />诊断</Button>
        <Button variant="ghost" size="icon-xs" title="更多操作" aria-label="更多操作"><Ellipsis /></Button>
        <Button variant="ghost" size="xs" className="ml-auto text-muted-foreground" title="查看全部信息">
          详情<ChevronRight />
        </Button>
      </div>
    </article>
  )
}
