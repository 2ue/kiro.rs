import type { ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArrowRight, ChevronDown } from 'lucide-react'
import { usageApi } from '@/api/endpoints/usage'
import type { KiroCredentialAttempt, ExternalPoolAttempt, RawUpstreamError, UsageRecord } from '@/api/types'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { CopyButton } from '@/components/patterns/copy-button'
import { ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { Stat, StatGrid } from '@/components/patterns/stat'
import { ToneBadge } from '@/components/status/tone-badge'
import { attemptActionLabel, RECORD_STATUS_LABEL, routeLabel, USAGE_SOURCE_LABEL } from '@/domain/labels'
import { fmtCompact, fmtFullDateTime, fmtInt, fmtMs, fmtUsd } from '@/lib/format'
import { ErrorText } from '@/components/patterns/error-text'
import { translateError } from '@/domain/upstream-error'
import { recordTone } from './columns'

function Block({ title, children, defaultOpen = true }: { title: string; children: ReactNode; defaultOpen?: boolean }) {
  return (
    <Collapsible defaultOpen={defaultOpen} className="border-b py-4 last:border-b-0">
      <CollapsibleTrigger className="group flex w-full items-center justify-between text-sm font-medium">
        {title}
        <ChevronDown className="size-4 text-muted-foreground transition-transform group-data-[state=open]:rotate-180" />
      </CollapsibleTrigger>
      <CollapsibleContent className="pt-3">{children}</CollapsibleContent>
    </Collapsible>
  )
}

function RawError({ raw }: { raw: RawUpstreamError }) {
  return (
    <div className="mt-2 rounded-md bg-muted/60 p-2">
      <div className="mb-1 flex items-center justify-between text-2xs text-muted-foreground">
        <span>
          {raw.source} · HTTP {raw.statusCode ?? '—'} · {fmtInt(raw.bodyBytes)} B{raw.truncated ? '（已截断）' : ''}
        </span>
        <CopyButton value={raw.body} label="复制原文" />
      </div>
      <pre className="max-h-48 overflow-auto font-mono text-2xs break-all whitespace-pre-wrap">{raw.body}</pre>
    </div>
  )
}

function AttemptChain({ local, external }: { local?: KiroCredentialAttempt[]; external?: ExternalPoolAttempt[] }) {
  const items = [
    ...(local ?? []).map((a) => ({
      key: `l${a.attempt}`,
      kind: '本地',
      who: a.credentialLabel ?? `#${a.credentialId}`,
      link: a.credentialId,
      status: a.status,
      action: a.action,
      error: a.errorMessage ?? a.errorType,
      ms: a.durationMs,
      raw: a.rawUpstreamError,
      model: a.model,
    })),
    ...(external ?? []).map((a) => ({
      key: `e${a.attempt}`,
      kind: '外部池',
      who: a.poolName,
      link: undefined as number | undefined,
      status: a.status,
      action: a.action,
      error: a.errorMessage ?? a.errorType,
      ms: a.durationMs,
      raw: a.rawUpstreamError,
      model: a.outboundModel,
    })),
  ]
  if (!items.length) return <p className="text-xs text-muted-foreground">没有尝试记录</p>
  return (
    <ol className="relative space-y-3 border-l pl-4">
      {items.map((a) => {
        const ok = a.action === 'success'
        return (
          <li key={a.key} className="relative">
            <span
              className={`absolute top-1.5 -left-[21px] size-2.5 rounded-full border-2 border-background ${ok ? 'bg-success' : 'bg-danger'}`}
            />
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <span className="text-xs text-muted-foreground">{a.kind}</span>
              {a.link ? (
                <Link to="/accounts" search={{ id: a.link }} className="font-medium hover:underline">
                  {a.who}
                </Link>
              ) : (
                <span className="font-medium">{a.who}</span>
              )}
              <ToneBadge tone={ok ? 'success' : 'danger'}>{attemptActionLabel(a.action)}</ToneBadge>
              {a.status && <span className="num text-xs text-muted-foreground">HTTP {a.status}</span>}
              <span className="num ml-auto text-xs text-muted-foreground">{fmtMs(a.ms)}</span>
            </div>
            {a.model && <div className="font-mono text-xs text-muted-foreground">{a.model}</div>}
            {a.error && <ErrorText error={a.error} className="mt-0.5" />}
            {a.raw && <RawError raw={a.raw} />}
          </li>
        )
      })}
    </ol>
  )
}

function JsonBlock({ value }: { value: unknown }) {
  const text = JSON.stringify(value, null, 2)
  return (
    <div className="relative">
      <div className="absolute top-1 right-1">
        <CopyButton value={text} />
      </div>
      <pre className="max-h-72 overflow-auto rounded-md bg-muted/60 p-3 font-mono text-2xs">{text}</pre>
    </div>
  )
}

function RecordDetail({ r }: { r: UsageRecord }) {
  const billing = r.externalPoolBilling
  const trace = r.latencyTrace
  return (
    <div className="px-4">
      {(r.errorMessage || r.publicErrorMessage) && (
        <div className="mt-4 rounded-lg border border-danger/30 bg-danger-subtle/40 p-3 text-sm">
          <div className="font-medium text-danger">
            {r.errorType ?? '错误'}
            {r.errorStatusCode ? ` · HTTP ${r.errorStatusCode}` : ''}
            {r.errorSource ? ` · ${r.errorSource}` : ''}
          </div>
          {(() => {
            const t = translateError(r.errorMessage)
            return t && t.title !== t.raw ? (
              <>
                <p className="mt-1 text-sm">{t.title}</p>
                {t.hint && <p className="text-xs text-muted-foreground">建议：{t.hint}</p>}
                <p className="mt-1 font-mono text-2xs break-all text-muted-foreground">{r.errorMessage}</p>
              </>
            ) : (
              <p className="mt-1 text-xs break-all">{r.errorMessage}</p>
            )
          })()}
          {r.publicErrorMessage && r.publicErrorMessage !== r.errorMessage && (
            <p className="mt-1 text-xs text-muted-foreground">返回给客户端：{r.publicErrorMessage}</p>
          )}
          {r.errorDetail && <p className="mt-1 text-xs break-all text-muted-foreground">{r.errorDetail}</p>}
          {r.rawUpstreamError && <RawError raw={r.rawUpstreamError} />}
        </div>
      )}

      <Block title="路由与尝试链">
        <StatGrid cols={3} className="mb-4">
          <Stat label="路由">{routeLabel(r)}</Stat>
          <Stat label="兜底原因">{r.fallbackReason ?? r.directPolicyReason ?? '—'}</Stat>
          <Stat label="会话粘性">{r.stickyBound ? (r.fallbackFromSticky ? '已绑定（回退）' : '已绑定') : '未绑定'}</Stat>
        </StatGrid>
        <AttemptChain local={r.credentialAttempts} external={r.externalAttempts} />
      </Block>

      <Block title="Token 与缓存">
        <StatGrid cols={3}>
          <Stat label="输入（总）">{fmtInt(r.totalInputTokens)}</Stat>
          <Stat label="计费输入">{fmtInt(r.billableInputTokens)}</Stat>
          <Stat label="输出">{fmtInt(r.outputTokens)}</Stat>
          <Stat label="缓存读">{fmtInt(r.cacheReadInputTokens)}</Stat>
          <Stat label="缓存写（5m / 1h）">
            {fmtInt(r.cacheCreationInputTokens)}（{fmtCompact(r.cacheCreation5mInputTokens)} / {fmtCompact(r.cacheCreation1hInputTokens)}）
          </Stat>
          <Stat label="用量来源">{USAGE_SOURCE_LABEL[r.usageSource] ?? r.usageSource}</Stat>
          {r.requestedMaxTokens !== undefined && <Stat label="max_tokens">{fmtInt(r.requestedMaxTokens)}</Stat>}
          {r.simulated && <Stat label="缓存模拟">是</Stat>}
        </StatGrid>
      </Block>

      <Block title="费用口径">
        <p className="mb-3 text-xs text-muted-foreground">估算费用按模型价格目录计算，不等于上游实际扣费。</p>
        <StatGrid cols={3}>
          <Stat label="估算费用">{fmtUsd(r.estimatedCostUsd)}</Stat>
          <Stat label="原价">{fmtUsd(r.originalCostUsd)}</Stat>
          <Stat label="Kiro metering">{fmtCompact(r.kiroMeteringUsage)}</Stat>
          <Stat label="计价模型">{r.pricingModel ?? (r.pricingAvailable ? r.model : '无价格')}</Stat>
        </StatGrid>
        {billing && (
          <div className="mt-4 rounded-lg border p-3">
            <div className="mb-2 text-xs font-medium">外部池计费</div>
            <StatGrid cols={3}>
              <Stat label="上游原始成本">{fmtUsd(billing.rawCostUsd)}</Stat>
              {billing.shapedCostUsd !== undefined && <Stat label="整形后成本">{fmtUsd(billing.shapedCostUsd)}</Stat>}
              <Stat label="对外上报">{fmtUsd(billing.reportedCostUsd)}</Stat>
              <Stat label="计费">{fmtUsd(billing.billableCostUsd)}</Stat>
              {billing.profitUsd !== undefined && (
                <Stat label="利润">
                  <span className={billing.profitUsd < 0 ? 'text-danger' : 'text-success'}>{fmtUsd(billing.profitUsd)}</span>
                </Stat>
              )}
              <Stat label="成本底线补齐">{billing.costFloorApplied ? fmtUsd(billing.costFloorDeltaUsd) : '未触发'}</Stat>
              <Stat label="投影模式">{billing.usageProjectionMode}</Stat>
            </StatGrid>
          </div>
        )}
      </Block>

      <Block title="耗时">
        <StatGrid cols={3}>
          <Stat label="总耗时">{fmtMs(r.durationMs)}</Stat>
          <Stat label="首字延迟">{fmtMs(r.firstTokenLatencyMs)}</Stat>
          <Stat label="响应延迟">{fmtMs(r.responseLatencyMs)}</Stat>
          {trace?.upstreamHeaderMs !== undefined && <Stat label="上游响应头">{fmtMs(trace.upstreamHeaderMs)}</Stat>}
          {trace?.firstUpstreamChunkMs !== undefined && <Stat label="首个上游分片">{fmtMs(trace.firstUpstreamChunkMs)}</Stat>}
          {trace?.payloadGuardMs !== undefined && <Stat label="请求体处理">{fmtMs(trace.payloadGuardMs)}</Stat>}
          {trace?.terminalReason && <Stat label="结束原因">{trace.terminalReason}</Stat>}
          {trace?.streamRetryAttempts ? <Stat label="流式重试">{trace.streamRetryAttempts}</Stat> : null}
          {trace?.inferenceAttempts && (
            <Stat label="推理尝试">
              {trace.inferenceAttempts.consumed}/{trace.inferenceAttempts.maxAttempts}
            </Stat>
          )}
        </StatGrid>
      </Block>

      {(r.payloadGuardReport != null || r.payloadBreakdown != null || trace) && (
        <Block title="诊断数据" defaultOpen={false}>
          <div className="space-y-3">
            {r.payloadGuardReport != null && (
              <div>
                <div className="mb-1 text-xs text-muted-foreground">请求体保护报告</div>
                <JsonBlock value={r.payloadGuardReport} />
              </div>
            )}
            {r.payloadBreakdown != null && (
              <div>
                <div className="mb-1 text-xs text-muted-foreground">请求体构成</div>
                <JsonBlock value={r.payloadBreakdown} />
              </div>
            )}
            {trace && (
              <div>
                <div className="mb-1 text-xs text-muted-foreground">延迟追踪</div>
                <JsonBlock value={trace} />
              </div>
            )}
          </div>
        </Block>
      )}
    </div>
  )
}

export function RequestSheet({
  id,
  cached,
  onClose,
  onFilterConversation,
}: {
  id: string | undefined
  cached?: UsageRecord
  onClose: () => void
  onFilterConversation: (conversationId: string) => void
}) {
  const q = useQuery({
    queryKey: ['usage', 'record', id],
    enabled: !!id && !cached,
    queryFn: async () => {
      const res = await usageApi.records({ page: 1, limit: 1, requestId: id })
      const hit = res.records[0]
      if (!hit) throw new Error('请求记录不存在或已被清理')
      return hit
    },
  })
  const r = cached ?? q.data

  return (
    <Sheet open={!!id} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full gap-0 sm:max-w-2xl">
        <SheetHeader className="border-b pr-12">
          <div className="flex flex-wrap items-center gap-2">
            {r && (
              <ToneBadge tone={recordTone(r.status)} dot>
                {RECORD_STATUS_LABEL[r.status] ?? r.status}
              </ToneBadge>
            )}
            <SheetTitle className="truncate font-mono text-sm">{r?.model ?? '请求详情'}</SheetTitle>
          </div>
          <SheetDescription asChild>
            <div className="space-y-1 text-xs">
              <div className="flex items-center gap-1">
                <span className="font-mono">{id}</span>
                {id && <CopyButton value={id} label="复制请求 ID" />}
              </div>
              {r && (
                <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                  <span>{fmtFullDateTime(r.createdAt)}</span>
                  <span className="font-mono">{r.endpoint}</span>
                  {r.stream && <span>流式</span>}
                  {r.upstreamModel && r.upstreamModel !== r.model && (
                    <span className="inline-flex items-center gap-1 font-mono">
                      <ArrowRight className="size-3" /> {r.upstreamModel}
                    </span>
                  )}
                  {r.conversationId && (
                    <Button
                      variant="unstyled"
                      size="none"
                      className="text-primary hover:underline"
                      onClick={() => onFilterConversation(r.conversationId!)}
                    >
                      查看同会话请求
                    </Button>
                  )}
                </div>
              )}
            </div>
          </SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 overflow-y-auto pb-6">
          {r ? (
            <RecordDetail r={r} />
          ) : q.error ? (
            <ErrorState error={q.error} />
          ) : (
            <div className="p-4">
              <LoadingRows />
            </div>
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}
