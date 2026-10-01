import { useState } from 'react'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import { systemApi } from '@/api/endpoints/system'
import type { AdminAuditLogRow } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet'
import { CopyButton } from '@/components/patterns/copy-button'
import { EmptyState, ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { Stat, StatGrid } from '@/components/patterns/stat'
import { StatusDot, ToneBadge } from '@/components/status/tone-badge'
import { fmtFullDateTime } from '@/lib/format'
import { qk } from '@/queries/keys'
import { usePollInterval } from '@/stores/ui-prefs'

const route = getRouteApi('/audit')
const LIMIT = 50

const OBJECT_LABEL: Record<string, string> = {
  credential: '账号',
  external_pool: '外部池',
  proxy_resource: '代理',
  runtime_config: '运行配置',
  request_api_key: '请求 Key',
  admin_api_key: 'Admin Key',
  model: '模型',
  usage_records: '请求记录',
}

/** 按日期分组的审计时间线；后端只支持分页，因此不提供会误导的"当前页筛选" */
export function AuditPage() {
  const { page = 1 } = route.useSearch()
  const navigate = route.useNavigate()
  const [selected, setSelected] = useState<AdminAuditLogRow | null>(null)
  const interval = usePollInterval('normal')
  const q = useQuery({
    queryKey: qk.system.audit(page, LIMIT),
    queryFn: () => systemApi.auditLogs(page, LIMIT),
    placeholderData: keepPreviousData,
    refetchInterval: page === 1 ? interval : false,
  })
  const records = q.data?.records ?? []
  const groups = records.reduce<Array<{ day: string; items: AdminAuditLogRow[] }>>((acc, r) => {
    const day = new Date(r.createdAt).toLocaleDateString('zh-CN', { year: 'numeric', month: 'long', day: 'numeric', weekday: 'short' })
    const last = acc[acc.length - 1]
    if (last?.day === day) last.items.push(r)
    else acc.push({ day, items: [r] })
    return acc
  }, [])

  return (
    <Page className="max-w-4xl">
      <PageHeader title="审计日志" description="管理后台的所有写操作记录，按时间倒序" />
      {q.isLoading ? (
        <LoadingRows rows={10} />
      ) : q.error && !q.data ? (
        <ErrorState error={q.error} onRetry={() => q.refetch()} />
      ) : records.length === 0 ? (
        <EmptyState title="暂无审计记录" />
      ) : (
        <div className={q.isFetching ? 'opacity-70 transition-opacity' : undefined}>
          {groups.map((g) => (
            <section key={g.day} className="mb-6">
              <h2 className="sticky top-12 z-10 bg-background/90 py-2 text-xs font-medium text-muted-foreground backdrop-blur">{g.day}</h2>
              <ol className="relative ml-2 space-y-1 border-l pl-5">
                {g.items.map((r) => (
                  <li key={r.id}>
                    <Button
                      variant="unstyled"
                      size="none"
                      onClick={() => setSelected(r)}
                      className="relative flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm hover:bg-muted/60"
                    >
                      <StatusDot tone={r.success ? 'success' : 'danger'} className="absolute top-1/2 -left-[25px] -translate-y-1/2" />
                      <span className="num w-16 shrink-0 text-xs text-muted-foreground">
                        {new Date(r.createdAt).toLocaleTimeString('zh-CN', { hour12: false })}
                      </span>
                      <span className="min-w-0 flex-1 truncate">
                        <span className="font-mono text-xs">{r.action}</span>
                        <span className="ml-2 text-muted-foreground">
                          {OBJECT_LABEL[r.objectType] ?? r.objectType}
                          {r.objectId ? ` #${r.objectId}` : ''}
                        </span>
                      </span>
                      {!r.success && <ToneBadge tone="danger">失败</ToneBadge>}
                      <span className="shrink-0 text-xs text-muted-foreground">{r.actor}</span>
                    </Button>
                  </li>
                ))}
              </ol>
            </section>
          ))}
        </div>
      )}
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span className="num">第 {page} 页</span>
        <div className="flex gap-1">
          <Button
            size="sm"
            variant="outline"
            disabled={page <= 1}
            onClick={() => navigate({ search: { page: page - 1 > 1 ? page - 1 : undefined } })}
          >
            <ChevronLeft /> 上一页
          </Button>
          <Button size="sm" variant="outline" disabled={!q.data?.hasNext} onClick={() => navigate({ search: { page: page + 1 } })}>
            下一页 <ChevronRight />
          </Button>
        </div>
      </div>

      <Sheet open={!!selected} onOpenChange={(o) => !o && setSelected(null)}>
        <SheetContent className="w-full sm:max-w-lg">
          {selected && (
            <>
              <SheetHeader>
                <SheetTitle className="font-mono text-sm">{selected.action}</SheetTitle>
                <SheetDescription>{fmtFullDateTime(selected.createdAt)}</SheetDescription>
              </SheetHeader>
              <div className="space-y-4 overflow-y-auto px-4 pb-6">
                <StatGrid>
                  <Stat label="操作者">{selected.actor}</Stat>
                  <Stat label="结果">{selected.success ? '成功' : '失败'}</Stat>
                  <Stat label="对象类型">{OBJECT_LABEL[selected.objectType] ?? selected.objectType}</Stat>
                  <Stat label="对象 ID">{selected.objectId ?? '—'}</Stat>
                </StatGrid>
                {selected.errorMessage && (
                  <p className="rounded-lg border border-danger/30 bg-danger-subtle/40 p-3 text-xs text-danger">{selected.errorMessage}</p>
                )}
                <div className="relative">
                  <div className="mb-1 text-xs text-muted-foreground">详情</div>
                  <div className="absolute top-6 right-1">
                    <CopyButton value={JSON.stringify(selected.detail, null, 2)} />
                  </div>
                  <pre className="max-h-[60svh] overflow-auto rounded-md bg-muted/60 p-3 font-mono text-2xs">
                    {JSON.stringify(selected.detail, null, 2)}
                  </pre>
                </div>
              </div>
            </>
          )}
        </SheetContent>
      </Sheet>
    </Page>
  )
}
