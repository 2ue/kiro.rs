import { useEffect, useState } from 'react'
import { Loader2, Square } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Meter } from '@/components/status/meter'
import { fmtDuration } from '@/lib/format'
import { cn } from '@/lib/utils'
import { estimateRemainingSecs, type RunProgress } from './import-runner'

/** 分批执行的实时进度：完成数、成功/失败/跳过、预计剩余时间，可停止 */
export function ImportProgress({
  label,
  progress,
  running,
  onStop,
  className,
}: {
  label: string
  progress: RunProgress
  running: boolean
  onStop: () => void
  className?: string
}) {
  // 每秒刷新一次已用时间与预计剩余
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!running) return
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [running])

  const pct = progress.total ? Math.round((progress.done / progress.total) * 100) : 0
  const remaining = estimateRemainingSecs(progress, now)
  const elapsed = Math.round((now - progress.startedAt) / 1000)

  return (
    <div className={cn('space-y-2 rounded-lg border bg-muted/30 p-3', className)} role="status" aria-live="polite">
      <div className="flex items-center justify-between gap-2 text-sm">
        <span className="flex items-center gap-2 font-medium">
          {running && <Loader2 className="size-4 animate-spin text-primary" />}
          {running ? `正在${label}` : `${label}已停止`}
          <span className="num text-muted-foreground">
            {progress.done} / {progress.total}（{pct}%）
          </span>
        </span>
        {running && (
          <Button size="xs" variant="outline" onClick={onStop}>
            <Square /> 停止
          </Button>
        )}
      </div>
      <Meter value={progress.done} max={progress.total} tone="primary" className="h-2" label={`${label}进度`} />
      <div className="num flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <span>
          成功 <span className="text-success">{progress.ok}</span>
        </span>
        {progress.skipped > 0 && <span>跳过 {progress.skipped}</span>}
        <span>
          失败 <span className={progress.failed ? 'text-danger' : undefined}>{progress.failed}</span>
        </span>
        <span>已用 {fmtDuration(elapsed)}</span>
        {running && remaining !== null && <span>预计还需 {fmtDuration(remaining)}</span>}
      </div>
      {running && <p className="text-xs text-muted-foreground">每个账号需要访问上游校验，请保持页面打开。停止后已完成的部分会保留。</p>}
    </div>
  )
}
