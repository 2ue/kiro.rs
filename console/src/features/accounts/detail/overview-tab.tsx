import { CircleSlash, RotateCcw, ShieldCheck, Timer } from 'lucide-react'
import { CredentialStatusBadge } from '@/components/status/credential-status-badge'
import { QuotaBar } from '@/components/status/quota-bar'
import { Meter } from '@/components/status/meter'
import { Stat, StatGrid } from '@/components/patterns/stat'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { modelCooldowns } from '@/domain/credential-status'
import { authMethodLabel, PROXY_SOURCE_LABEL } from '@/domain/labels'
import { fmtCompact, fmtDateTime, fmtDuration, fmtInt, fmtMs, fmtPct, fmtRelative, fmtUsd } from '@/lib/format'
import { useAccountActions } from '../actions'
import { ErrorText } from '@/components/patterns/error-text'
import type { AccountRow } from '../queries'

export function OverviewTab({ row }: { row: AccountRow }) {
  const actions = useAccountActions()
  const info = row.accountInfo
  const q = row.quota
  const reason = row.status.disabledReason
  const partial = modelCooldowns(row)

  return (
    <div className="space-y-5">
      {reason && (
        <Alert variant={reason.terminal ? 'destructive' : 'default'}>
          <CircleSlash />
          <AlertTitle>已禁用：{reason.label}</AlertTitle>
          <AlertDescription>
            <p>{reason.hint}</p>
            <div className="mt-2 flex flex-wrap gap-2">
              {reason.action === 'enable' || reason.action === 'wait_reset' || reason.action === 'review' ? (
                <Button size="xs" variant="outline" onClick={() => actions.toggleDisabled([row.id], false)}>
                  重新启用
                </Button>
              ) : null}
              {reason.action === 'reset_and_check' && (
                <Button size="xs" variant="outline" onClick={() => actions.resetAndCheck([row.id])}>
                  <RotateCcw /> 重置失败计数并体检
                </Button>
              )}
              {reason.action === 'reauth' && (
                <Button size="xs" variant="outline" onClick={() => actions.refreshToken.mutate(row.id)}>
                  刷新 Token
                </Button>
              )}
              {reason.action === 'wait_reset' && q.nextResetAt && (
                <span className="text-xs text-muted-foreground">额度将于 {fmtDateTime(q.nextResetAt.getTime())} 重置</span>
              )}
            </div>
          </AlertDescription>
        </Alert>
      )}

      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-medium">调度状态</h3>
          <CredentialStatusBadge view={row.status} />
        </div>
        <p className="text-xs text-muted-foreground">{row.status.detail}</p>
        {partial.length > 0 && (
          <div className="rounded-lg border bg-warning-subtle/40 p-2.5 text-xs">
            <div className="mb-1 flex items-center gap-1.5 font-medium text-warning">
              <Timer className="size-3.5" /> 部分模型冷却中
            </div>
            <ul className="space-y-0.5">
              {partial.map((c) => (
                <li key={c.model} className="num flex justify-between gap-2">
                  <span className="truncate font-mono">{c.model}</span>
                  <span className="text-muted-foreground">
                    {fmtDuration(c.remainingSecs)}
                    {c.reason ? ` · ${c.reason}` : ''}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
        <div className="space-y-1.5">
          <div className="flex justify-between text-xs">
            <span className="text-muted-foreground">在途 / 并发上限</span>
            <span className="num">
              {row.inFlightRequests} / {row.maxConcurrentRequests > 0 ? row.maxConcurrentRequests : '不限'}
              {typeof row.maxConcurrentRequestsOverride === 'number' && <span className="text-muted-foreground">（账号覆盖）</span>}
            </span>
          </div>
          {row.maxConcurrentRequests > 0 && (
            <Meter
              value={row.inFlightRequests}
              max={row.maxConcurrentRequests}
              tone={row.inFlightRequests >= row.maxConcurrentRequests ? 'warning' : 'info'}
            />
          )}
        </div>
        <StatGrid cols={3}>
          <Stat label="调度评分（越低越优先）">{(row.schedulerScore ?? 0).toFixed(3)}</Stat>
          <Stat label="选择压力">{(row.schedulerSelectionPressure ?? 0).toFixed(2)}</Stat>
          <Stat label="选中次数 10s/60s/5m">
            {row.recentSchedulerSelectionCount10s ?? 0} / {row.recentSchedulerSelectionCount60s ?? 0} /{' '}
            {row.recentSchedulerSelectionCount5m ?? 0}
          </Stat>
          <Stat label="近期错误率">{fmtPct(row.recentErrorRate ?? 0)}</Stat>
          <Stat label="延迟 EWMA">{fmtMs(row.latencyEwmaMs)}</Stat>
          <Stat label="连续临时失败">{row.transientFailureStreak ?? 0}</Stat>
          <Stat label="成功 / 失败">
            {fmtInt(row.successCount)} / {fmtInt(row.failureCount)}
          </Stat>
          <Stat label="刷新失败">{row.refreshFailureCount}</Stat>
          <Stat label="RPM 上限">{row.rpm > 0 ? row.rpm : '不限'}</Stat>
        </StatGrid>
        {row.lastErrorKind && (
          <div className="rounded-lg border bg-danger-subtle/40 p-2.5 text-xs">
            <div className="mb-1 text-muted-foreground">最近错误 · {row.lastErrorKind}</div>
            <ErrorText error={row.lastErrorReason ?? row.lastErrorKind} />
            {row.lastErrorAtMs && <p className="mt-0.5 text-muted-foreground">{fmtRelative(row.lastErrorAtMs)}</p>}
          </div>
        )}
      </section>

      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-medium">额度</h3>
          <Button size="xs" variant="ghost" onClick={() => actions.refreshInfo.mutate([row.id])} disabled={actions.refreshInfo.isPending}>
            <RotateCcw /> 刷新
          </Button>
        </div>
        <QuotaBar quota={q} />
        {info ? (
          <StatGrid cols={3}>
            <Stat label="套餐">{info.subscriptionTitle ?? row.subscriptionTitle ?? '—'}</Stat>
            <Stat label="已用 / 总量">
              {fmtInt(info.currentUsage)} / {fmtInt(info.usageLimit)}
            </Stat>
            <Stat label="剩余">{fmtInt(info.remaining)}</Stat>
            <Stat label="积分（基础 + 赠送）">
              {fmtCompact(info.creditBase)} + {fmtCompact(info.creditBonus)}
            </Stat>
            <Stat label="积分剩余">{fmtCompact(info.creditRemaining)}</Stat>
            <Stat label="下次重置">{info.nextResetAt ? `${fmtDateTime(info.nextResetAt)}（${fmtRelative(info.nextResetAt)}）` : '—'}</Stat>
            <Stat label="超额">{q.overageEnabled ? '已开启' : (info.overageStatus ?? '未知')}</Stat>
            <Stat label="超额费用">{fmtUsd(info.currentOverages)}</Stat>
            <Stat label="快照时间">
              {fmtRelative(info.checkedAt)}
              {q.stale ? '（可能过期）' : ''}
            </Stat>
          </StatGrid>
        ) : (
          <p className="text-xs text-muted-foreground">尚未查询额度快照，点击"刷新"获取。</p>
        )}
      </section>

      <section className="space-y-3">
        <h3 className="text-sm font-medium">成本（累计）</h3>
        <StatGrid cols={3}>
          <Stat label="估算费用">{fmtUsd(row.estimatedCostUsd)}</Stat>
          <Stat label="原价">{fmtUsd(row.originalCostUsd)}</Stat>
          <Stat label="Kiro metering">{fmtCompact(row.kiroMeteringUsage)}</Stat>
          <Stat label="有价 / 无价请求">
            {fmtInt(row.pricedRequests)} / {fmtInt(row.unpricedRequests)}
          </Stat>
        </StatGrid>
      </section>

      <section className="space-y-3">
        <h3 className="flex items-center gap-1.5 text-sm font-medium">
          <ShieldCheck className="size-4" /> 身份与连接
        </h3>
        <StatGrid cols={2}>
          <Stat label="认证方式">
            {authMethodLabel(row.authMethod)}
            {row.provider ? ` · ${row.provider}` : ''}
          </Stat>
          <Stat label="端点">{row.endpoint}</Stat>
          <Stat label="认证 Region">{row.effectiveAuthRegion}</Stat>
          <Stat label="API Region">{row.effectiveApiRegion}</Stat>
          <Stat label="代理来源">
            {PROXY_SOURCE_LABEL[row.effectiveProxySource]}
            {row.proxyResourceName ? ` · ${row.proxyResourceName}` : ''}
          </Stat>
          <Stat label="Token 过期">{row.expiresAt ? fmtRelative(row.expiresAt) : '—'}</Stat>
          <Stat label="Profile ARN">{row.hasProfileArn ? '已配置' : '未配置'}</Stat>
          <Stat label="创建时间">{fmtDateTime(row.createdAt)}</Stat>
          {row.refreshTokenHash && (
            <Stat label="Refresh Token 指纹" mono>
              {row.refreshTokenHash.slice(0, 16)}
            </Stat>
          )}
          {row.maskedApiKey && (
            <Stat label="API Key" mono>
              {row.maskedApiKey}
            </Stat>
          )}
        </StatGrid>
      </section>
    </div>
  )
}
