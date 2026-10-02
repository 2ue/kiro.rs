import { useEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { CircleAlert, CircleCheck, CircleDashed, CircleMinus, CircleSlash, Copy, Loader2, RotateCcw } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { translateError } from '@/domain/upstream-error'
import { fmtDuration } from '@/lib/format'
import { cn } from '@/lib/utils'
import type { ItemStatus, QueueItem } from './queue'

type Filter = 'all' | 'active' | 'failed' | 'success' | 'skipped' | 'pending'

const STATUS_META: Record<ItemStatus, { label: string; icon: React.ReactNode; className: string }> = {
  pending: { label: '等待中', icon: <CircleDashed className="size-4" />, className: 'text-muted-foreground' },
  importing: { label: '导入中', icon: <Loader2 className="size-4 animate-spin" />, className: 'text-primary' },
  verifying: { label: '验活中', icon: <Loader2 className="size-4 animate-spin" />, className: 'text-info' },
  success: { label: '成功', icon: <CircleCheck className="size-4" />, className: 'text-success' },
  failed: { label: '失败', icon: <CircleAlert className="size-4" />, className: 'text-danger' },
  skipped: { label: '跳过', icon: <CircleMinus className="size-4" />, className: 'text-warning' },
  cancelled: { label: '未处理', icon: <CircleSlash className="size-4" />, className: 'text-muted-foreground' },
}

function ItemRow({ item, onRetry, retryDisabled }: { item: QueueItem; onRetry: () => void; retryDisabled: boolean }) {
  const meta = STATUS_META[item.status]
  const err = item.error ? translateError(item.error) : null
  const elapsed = item.startedAt && item.finishedAt ? Math.max(0, (item.finishedAt - item.startedAt) / 1000) : null
  const active = item.status === 'importing' || item.status === 'verifying'
  return (
    <div
      className={cn(
        'flex items-start gap-3 border-b px-3 py-2 text-sm last:border-b-0',
        active && 'bg-accent/40',
        item.status === 'failed' && 'bg-danger-subtle/30',
      )}
    >
      <span className={cn('mt-0.5 shrink-0', meta.className)} aria-label={meta.label}>
        {meta.icon}
      </span>
      <span className="num mt-0.5 w-8 shrink-0 text-xs text-muted-foreground">{item.index + 1}</span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate font-medium">{item.email || item.label}</span>
          {item.credentialId && <span className="num shrink-0 text-xs text-muted-foreground">#{item.credentialId}</span>}
          {item.attempts > 1 && (
            <span className="shrink-0 rounded bg-muted px-1 text-2xs text-muted-foreground">第 {item.attempts} 次</span>
          )}
        </div>
        {active && (
          <div className={cn('text-xs', meta.className)}>{item.status === 'importing' ? '正在添加账号并刷新 Token…' : '正在验活…'}</div>
        )}
        {item.status === 'success' && item.detail && <div className="truncate text-xs text-muted-foreground">{item.detail}</div>}
        {item.warning && <div className="text-xs text-warning">{item.warning}</div>}
        {err && (
          <Tooltip>
            <TooltipTrigger asChild>
              <div className={cn('truncate text-xs', item.status === 'failed' ? 'text-danger' : 'text-muted-foreground')}>
                {err.title}
                {err.hint && <span className="text-muted-foreground"> · {err.hint}</span>}
              </div>
            </TooltipTrigger>
            <TooltipContent className="max-w-md text-xs break-all">{err.raw}</TooltipContent>
          </Tooltip>
        )}
      </div>
      <div className="flex shrink-0 items-center gap-1">
        {elapsed !== null && <span className="num text-xs text-muted-foreground">{elapsed.toFixed(1)}s</span>}
        {(item.status === 'failed' || item.status === 'cancelled') && (
          <Button size="xs" variant="outline" onClick={onRetry} disabled={retryDisabled}>
            <RotateCcw /> 重试
          </Button>
        )}
      </div>
    </div>
  )
}

/**
 * 导入进度：分段进度条 + 当前处理中的账号 + 可筛选的逐项列表。
 * 每一项都能看到状态、耗时、失败原因，失败项可单独或批量重试。
 */
export function ImportProgress({
  items,
  running,
  runWindow,
  onRetry,
  onStop,
}: {
  items: QueueItem[]
  running: boolean
  runWindow: { start: number; end?: number } | null
  onRetry: (indexes: number[]) => void
  onStop: () => void
}) {
  const [filter, setFilter] = useState<Filter>('all')
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!running) return
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [running])

  const counts = useMemo(() => {
    const c = { success: 0, failed: 0, skipped: 0, pending: 0, active: 0, cancelled: 0 }
    for (const i of items) {
      if (i.status === 'importing' || i.status === 'verifying') c.active++
      else if (i.status === 'cancelled') c.cancelled++
      else c[i.status]++
    }
    return c
  }, [items])
  const total = items.length
  const done = counts.success + counts.failed + counts.skipped + counts.cancelled
  const elapsed = runWindow ? ((running ? now : (runWindow.end ?? now)) - runWindow.start) / 1000 : 0
  // 按本轮已完成账号的平均耗时估算剩余时间
  const finishedThisRun = runWindow ? items.filter((i) => i.finishedAt && i.finishedAt >= runWindow.start).length : 0
  const remaining = running && finishedThisRun > 0 ? (elapsed / finishedThisRun) * (counts.pending + counts.active) : null
  const retryable = items.filter((i) => i.status === 'failed' || i.status === 'cancelled').map((i) => i.index)

  const visible = useMemo(() => {
    switch (filter) {
      case 'active':
        return items.filter((i) => i.status === 'importing' || i.status === 'verifying')
      case 'failed':
        return items.filter((i) => i.status === 'failed' || i.status === 'cancelled')
      case 'pending':
        return items.filter((i) => i.status === 'pending')
      case 'all':
        return items
      default:
        return items.filter((i) => i.status === filter)
    }
  }, [items, filter])

  const scrollRef = useRef<HTMLDivElement>(null)
  const virtualizer = useVirtualizer({
    count: visible.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 52,
    overscan: 8,
  })

  // 运行中自动滚动到正在处理的账号
  const firstActive = filter === 'all' ? visible.findIndex((i) => i.status === 'importing' || i.status === 'verifying') : -1
  useEffect(() => {
    if (running && firstActive >= 0) virtualizer.scrollToIndex(firstActive, { align: 'center' })
  }, [running, firstActive, virtualizer])

  const seg = (n: number) => `${total ? (n / total) * 100 : 0}%`
  const tabs: Array<{ key: Filter; label: string; count: number }> = [
    { key: 'all', label: '全部', count: total },
    { key: 'active', label: '处理中', count: counts.active },
    { key: 'failed', label: '失败', count: counts.failed + counts.cancelled },
    { key: 'success', label: '成功', count: counts.success },
    { key: 'skipped', label: '跳过', count: counts.skipped },
    { key: 'pending', label: '等待', count: counts.pending },
  ]

  const copyFailed = async () => {
    const lines = items.filter((i) => i.status === 'failed').map((i) => `${i.index + 1}\t${i.email || i.label}\t${i.error ?? ''}`)
    await navigator.clipboard.writeText(lines.join('\n'))
    toast.success(`已复制 ${lines.length} 条失败记录`)
  }

  return (
    <div className="space-y-3">
      <div className="space-y-2 rounded-lg border p-3" role="status" aria-live="polite">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2 text-sm font-medium">
            {running && <Loader2 className="size-4 animate-spin text-primary" />}
            {running ? '正在导入' : done === total ? '导入完成' : '已停止'}
            <span className="num text-muted-foreground">
              {done} / {total}（{total ? Math.round((done / total) * 100) : 0}%）
            </span>
          </div>
          <div className="num flex items-center gap-3 text-xs text-muted-foreground">
            <span>已用 {fmtDuration(Math.round(elapsed))}</span>
            {remaining !== null && <span>预计还需 {fmtDuration(Math.round(remaining))}</span>}
          </div>
        </div>
        <div className="flex h-2 overflow-hidden rounded-full bg-muted" aria-hidden>
          <div className="h-full bg-success transition-[width]" style={{ width: seg(counts.success) }} />
          <div className="h-full bg-danger transition-[width]" style={{ width: seg(counts.failed) }} />
          <div className="h-full bg-warning transition-[width]" style={{ width: seg(counts.skipped) }} />
          <div className="h-full animate-pulse bg-primary/60 transition-[width]" style={{ width: seg(counts.active) }} />
        </div>
        <div className="num flex flex-wrap gap-x-4 gap-y-1 text-xs">
          <span className="text-success">成功 {counts.success}</span>
          <span className="text-danger">失败 {counts.failed}</span>
          <span className="text-warning">跳过 {counts.skipped}</span>
          {counts.active > 0 && <span className="text-primary">处理中 {counts.active}</span>}
          {counts.pending > 0 && <span className="text-muted-foreground">等待 {counts.pending}</span>}
          {counts.cancelled > 0 && <span className="text-muted-foreground">未处理 {counts.cancelled}</span>}
        </div>
      </div>

      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap gap-1" role="tablist" aria-label="按状态筛选">
          {tabs.map((t) => (
            <Button
              key={t.key}
              size="xs"
              role="tab"
              aria-selected={filter === t.key}
              variant={filter === t.key ? 'secondary' : 'ghost'}
              onClick={() => setFilter(t.key)}
              disabled={t.key !== 'all' && t.count === 0}
            >
              {t.label}
              <span className="num opacity-70">{t.count}</span>
            </Button>
          ))}
        </div>
        <div className="flex gap-1">
          {counts.failed > 0 && (
            <Button size="xs" variant="ghost" onClick={copyFailed}>
              <Copy /> 复制失败明细
            </Button>
          )}
          {running ? (
            <Button size="xs" variant="outline" onClick={onStop}>
              停止
            </Button>
          ) : (
            retryable.length > 0 && (
              <Button size="xs" onClick={() => onRetry(retryable)}>
                <RotateCcw />{' '}
                {counts.cancelled && !counts.failed ? `继续导入 ${retryable.length} 个` : `重试全部失败 ${retryable.length} 个`}
              </Button>
            )
          )}
        </div>
      </div>

      <div ref={scrollRef} className="h-[min(22rem,45svh)] overflow-y-auto rounded-lg border">
        {visible.length === 0 ? (
          <p className="p-6 text-center text-sm text-muted-foreground">没有{tabs.find((t) => t.key === filter)?.label}的账号</p>
        ) : (
          <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
            {virtualizer.getVirtualItems().map((v) => {
              const item = visible[v.index]!
              return (
                <div
                  key={item.index}
                  ref={virtualizer.measureElement}
                  data-index={v.index}
                  style={{ position: 'absolute', top: 0, left: 0, width: '100%', transform: `translateY(${v.start}px)` }}
                >
                  <ItemRow item={item} retryDisabled={running} onRetry={() => onRetry([item.index])} />
                </div>
              )
            })}
          </div>
        )}
      </div>
      {running && (
        <p className="text-xs text-muted-foreground">
          每个账号需要访问上游，请保持页面打开。点击"停止"后，正在处理的账号会完成，剩余账号可稍后继续。
        </p>
      )}
    </div>
  )
}
