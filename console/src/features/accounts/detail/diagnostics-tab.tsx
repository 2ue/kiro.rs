import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { ExternalLink } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { EmptyState, ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { ToneBadge } from '@/components/status/tone-badge'
import { RECORD_STATUS_LABEL } from '@/domain/labels'
import { fmtCompact, fmtDateTime, fmtMs, fmtUsd } from '@/lib/format'
import { useDiagnostics } from '../queries'

export function DiagnosticsTab({ id }: { id: number }) {
  const [page, setPage] = useState(1)
  const q = useDiagnostics(id, page)
  const navigate = useNavigate()

  if (q.isLoading) return <LoadingRows />
  if (q.error) return <ErrorState error={q.error} onRetry={() => q.refetch()} />
  const records = q.data?.records ?? []

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium">最近请求</h3>
        <Button size="xs" variant="ghost" onClick={() => navigate({ to: '/requests', search: { credentialId: id } })}>
          在请求页查看全部 <ExternalLink />
        </Button>
      </div>
      {records.length === 0 ? (
        <EmptyState title="暂无请求记录" />
      ) : (
        <ul className="divide-y rounded-lg border">
          {records.map((r) => (
            <li key={r.id}>
              <button
                type="button"
                onClick={() => navigate({ to: '/requests', search: { id: r.id } })}
                className="flex w-full items-center gap-3 px-3 py-2 text-left text-xs hover:bg-muted/50"
              >
                <ToneBadge tone={r.status === 'success' ? 'success' : r.status === 'client_dropped' ? 'warning' : 'danger'}>
                  {RECORD_STATUS_LABEL[r.status] ?? r.status}
                </ToneBadge>
                <span className="min-w-0 flex-1 truncate font-mono">{r.model}</span>
                <span className="num text-muted-foreground">{fmtCompact(r.totalInputTokens)}→{fmtCompact(r.outputTokens)}</span>
                <span className="num w-14 text-right text-muted-foreground">{fmtMs(r.durationMs)}</span>
                <span className="num w-16 text-right">{fmtUsd(r.estimatedCostUsd)}</span>
                <span className="num w-24 text-right text-muted-foreground">{fmtDateTime(r.createdAt, true)}</span>
              </button>
              {r.errorMessage && <p className="truncate px-3 pb-2 text-xs text-danger">{r.errorMessage}</p>}
            </li>
          ))}
        </ul>
      )}
      <div className="flex justify-end gap-2">
        <Button size="xs" variant="outline" disabled={page <= 1} onClick={() => setPage((p) => p - 1)}>
          上一页
        </Button>
        <Button size="xs" variant="outline" disabled={!q.data?.hasNext} onClick={() => setPage((p) => p + 1)}>
          下一页
        </Button>
      </div>
    </div>
  )
}
