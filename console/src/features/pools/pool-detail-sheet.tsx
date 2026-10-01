import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { FlaskConical, Loader2, Pencil, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'
import { poolsApi } from '@/api/endpoints/pools'
import type { ExternalPoolStatus, UsageExternalPoolBillingByPool } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet'
import { SelectControl } from '@/components/patterns/fields'
import { Stat, StatGrid } from '@/components/patterns/stat'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtDateTime, fmtDuration, fmtInt, fmtMs, fmtPct, fmtUsd } from '@/lib/format'
import { qk } from '@/queries/keys'
import { poolState } from './queries'

const STANDARD_TEST_MODELS = ['claude-sonnet-4-5', 'claude-sonnet-4-6', 'claude-haiku-4-5', 'claude-opus-4-6', 'claude-opus-4-5']

/** 外部池测试使用标准 Claude Code 模型 ID（横杠，不带 -thinking / [1m]） */
function toStandardModelId(model: string): string | null {
  let v = model.trim().toLowerCase()
  if (!v) return null
  v = v.replace(/\[1m\]$/, '').replace(/-thinking$/, '')
  const m = v.match(/^(?:claude-)?(opus|sonnet|haiku|fable)-(\d+)(?:[.-](\d{1,2}))?(?:-(\d{8}))?$/)
  if (!m) return null
  const [, family, major, minor, date] = m
  return `claude-${family}-${major}${minor ? `-${minor}` : ''}${date ? `-${date}` : ''}`
}

export function PoolDetailSheet({
  status,
  billing,
  onClose,
  onEdit,
}: {
  status?: ExternalPoolStatus
  billing?: UsageExternalPoolBillingByPool
  onClose: () => void
  onEdit: () => void
}) {
  const queryClient = useQueryClient()
  const p = status?.pool
  const options = [...new Set([...(p?.supportedModels ?? []).map(toStandardModelId).filter((m): m is string => !!m), ...STANDARD_TEST_MODELS])]
  const [model, setModel] = useState(options[0] ?? 'claude-sonnet-4-5')
  const [prompt, setPrompt] = useState('hi')
  const test = useMutation({ mutationFn: () => poolsApi.test(p!.id, { model, prompt }), meta: { error: '测试失败' } })
  const sync = useMutation({
    mutationFn: () => poolsApi.syncSupportedModels(p!.id),
    onSuccess: (r) => toast.success(`已同步 ${r.count} 个模型`),
    onSettled: () => queryClient.invalidateQueries({ queryKey: qk.pools.all }),
    meta: { error: '同步失败' },
  })
  const st = status ? poolState(status) : undefined
  const q = status?.quality

  return (
    <Sheet open={!!status} onOpenChange={(o) => !o && onClose()}>
      <SheetContent className="w-full gap-0 sm:max-w-xl">
        {p && st && (
          <>
            <SheetHeader className="border-b pr-12">
              <div className="flex items-center gap-2">
                <SheetTitle>{p.name}</SheetTitle>
                <ToneBadge tone={st.tone} dot>
                  {st.label}
                </ToneBadge>
              </div>
              <SheetDescription className="font-mono text-xs">{p.baseUrl}</SheetDescription>
              <div className="flex gap-2 pt-2">
                <Button size="sm" variant="outline" onClick={onEdit}>
                  <Pencil /> 编辑
                </Button>
                <Button size="sm" variant="outline" asChild>
                  <Link to="/requests" search={{ poolId: p.id }}>
                    查看请求
                  </Link>
                </Button>
              </div>
            </SheetHeader>
            <div className="min-h-0 flex-1 space-y-6 overflow-y-auto p-4">
              {st.reason && <p className="rounded-lg border bg-muted/40 p-3 text-xs">{st.reason}</p>}
              {p.autoDisabled && p.autoDisabledLastError && (
                <div className="rounded-lg border border-danger/30 bg-danger-subtle/40 p-3 text-xs">
                  <div className="font-medium text-danger">触发自动禁用的错误</div>
                  <p className="mt-1 break-all">{p.autoDisabledLastError}</p>
                  {p.autoDisabledAt && <p className="mt-1 text-muted-foreground">{fmtDateTime(p.autoDisabledAt)}</p>}
                </div>
              )}
              <section className="space-y-3">
                <h3 className="text-sm font-medium">运行状态</h3>
                <StatGrid cols={3}>
                  <Stat label="在途 / 并发">
                    {status.inFlight} / {p.maxConcurrentRequests}
                  </Stat>
                  <Stat label="优先级">{p.priority}</Stat>
                  <Stat label="冷却剩余">{status.cooldownRemainingSecs > 0 ? fmtDuration(status.cooldownRemainingSecs) : '—'}</Stat>
                  <Stat label="连续临时失败">{status.transientFailureStreak}</Stat>
                  <Stat label="近期错误率">{q ? fmtPct(q.recentErrorRate) : '—'}</Stat>
                  <Stat label="首字 EWMA">{fmtMs(q?.ttftEwmaMs)}</Stat>
                  <Stat label="耗时 EWMA">{fmtMs(q?.latencyEwmaMs)}</Stat>
                  <Stat label="质量样本">
                    {q?.sampleCount ?? 0}
                    {q && !q.scoringActive ? '（未生效）' : ''}
                  </Stat>
                  <Stat label="降级">{q?.inProbation ? `层级 ${q.probationLevel} · 剩余 ${fmtDuration(q.probationRemainingSecs)}` : '无'}</Stat>
                </StatGrid>
              </section>
              {billing && (
                <section className="space-y-3">
                  <h3 className="text-sm font-medium">计费（当前统计窗口）</h3>
                  <StatGrid cols={3}>
                    <Stat label="请求 / 有价">
                      {fmtInt(billing.requests)} / {fmtInt(billing.pricedRequests)}
                    </Stat>
                    <Stat label="上游原始成本">{fmtUsd(billing.rawCostUsd)}</Stat>
                    <Stat label="对外上报">{fmtUsd(billing.reportedCostUsd)}</Stat>
                    <Stat label="计费">{fmtUsd(billing.billableCostUsd)}</Stat>
                    <Stat label="利润">
                      <span className={(billing.profitUsd ?? 0) < 0 ? 'text-danger' : 'text-success'}>{fmtUsd(billing.profitUsd)}</span>
                    </Stat>
                    <Stat label="成本底线补齐">
                      {fmtInt(billing.costFloorAppliedRequests)} 次 · {fmtUsd(billing.costFloorDeltaUsd)}
                    </Stat>
                  </StatGrid>
                </section>
              )}
              <section className="space-y-3">
                <div className="flex items-center justify-between">
                  <h3 className="text-sm font-medium">支持的模型</h3>
                  <Button size="xs" variant="ghost" onClick={() => sync.mutate()} disabled={sync.isPending}>
                    <RefreshCw className={sync.isPending ? 'animate-spin' : undefined} /> 从上游同步
                  </Button>
                </div>
                {p.supportedModels.length ? (
                  <div className="flex flex-wrap gap-1.5">
                    {p.supportedModels.map((m) => (
                      <span key={m} className="rounded-md border bg-muted/50 px-2 py-0.5 font-mono text-xs">
                        {m}
                      </span>
                    ))}
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground">不限制模型</p>
                )}
              </section>
              <section className="space-y-3">
                <h3 className="text-sm font-medium">连通性测试</h3>
                <div className="grid gap-2 sm:grid-cols-[1fr_1fr_auto]">
                  <SelectControl value={model} onChange={setModel} options={options.map((m) => ({ value: m, label: m }))} />
                  <Input value={prompt} onChange={(e) => setPrompt(e.target.value)} aria-label="测试提示词" />
                  <Button size="sm" onClick={() => test.mutate()} disabled={test.isPending}>
                    {test.isPending ? <Loader2 className="animate-spin" /> : <FlaskConical />} 测试
                  </Button>
                </div>
                {test.data && (
                  <div className={`rounded-lg border p-3 text-xs ${test.data.ok ? 'bg-success-subtle/40' : 'bg-danger-subtle/40'}`}>
                    <div className={test.data.ok ? 'font-medium text-success' : 'font-medium text-danger'}>
                      {test.data.ok ? '成功' : '失败'}
                      {test.data.status ? ` · HTTP ${test.data.status}` : ''} · {test.data.message}
                    </div>
                    {test.data.response && <p className="mt-1 line-clamp-4 whitespace-pre-wrap text-muted-foreground">{test.data.response}</p>}
                  </div>
                )}
              </section>
              {p.notes && (
                <section>
                  <h3 className="mb-1 text-sm font-medium">备注</h3>
                  <p className="text-xs whitespace-pre-wrap text-muted-foreground">{p.notes}</p>
                </section>
              )}
            </div>
          </>
        )}
      </SheetContent>
    </Sheet>
  )
}
