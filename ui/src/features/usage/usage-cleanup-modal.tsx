import { useEffect, useState } from 'react'
import { Check, ChevronDown, Eye, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { extractErrorMessage } from '@/lib/utils'
import { formatDate, formatNumber } from '@/lib/format'
import {
  useCancelUsageCleanup,
  usePreviewUsageCleanup,
  useResumeUsageCleanup,
  useStartUsageCleanup,
  useUsageCleanupStatus,
} from '@/hooks/use-usage'
import type { UsageCleanupPreviewResponse, UsageCleanupRequest, UsageCleanupStatusResponse } from '@/types/api'
import { Badge, Button, Input, Progress } from '@/components/ui'
import { Callout, ModalShell, useConfirm } from '@/components/patterns'

const CLEANUP_MAX_OLDER_THAN_DAYS = 3650
const CLEANUP_DEFAULT_BATCH_SIZE = 10_000
const CLEANUP_MAX_BATCH_SIZE = 10_000
const CLEANUP_MAX_PAUSE_MS = 10_000
const CLEANUP_DEFAULT_PAUSE_MS = 10

type CleanupKind = 'soft' | 'hard' | 'all'
type CleanupRange = 'all' | 'older'

const KIND_OPTIONS: Array<{ value: CleanupKind; title: string; desc: string; danger?: boolean }> = [
  { value: 'soft', title: '隐藏明细', desc: '明细标记为已删除，不再显示；总览和趋势的汇总保留。' },
  { value: 'hard', title: '删除明细', desc: '明细从数据库删除，释放空间，无法恢复；汇总保留。', danger: true },
  { value: 'all', title: '清空全部', desc: '明细、趋势、排行和费用汇总全部清空，无法恢复。', danger: true },
]

const STOP_REASON_LABEL: Record<string, string> = {
  no_more_rows: '已清理完',
  cancel_requested: '已取消',
  cancel_requested_during_redis_cleanup: '清理缓存时取消',
  max_batches_reached: '达到单次批次上限，可继续',
  max_rows_reached: '达到单次行数上限，可继续',
  postgres_lock_contention: '数据库繁忙，可稍后继续',
  postgres_batch_failed: '数据库清理失败',
  postgres_batch_contract_invalid: '数据库清理失败',
  postgres_completion_probe_failed: '检查剩余数据失败',
  postgres_summary_cleanup_failed: '清理汇总失败',
  redis_admin_cache_cleanup_failed: '清理缓存失败',
  redis_snapshot_cleanup_failed: '清理快照失败',
  redis_admin_cache_pass_limit_reached: '缓存清理达到上限，可继续',
  redis_snapshot_pass_limit_reached: '快照清理达到上限，可继续',
}

const STATUS_LABEL: Record<string, string> = {
  queued: '排队中',
  running: '清理中',
  paused: '已暂停',
  completed: '已完成',
  cancelled: '已取消',
  failed: '失败',
}

function boundedInteger(value: string, fallback: number, min: number, max: number): number {
  const parsed = Number(value)
  const normalized = Number.isFinite(parsed) ? Math.floor(parsed) : fallback
  return Math.max(min, Math.min(max, normalized))
}

function statusTone(status: string): 'warning' | 'success' | 'error' | 'neutral' {
  if (status === 'queued' || status === 'running') return 'warning'
  if (status === 'completed') return 'success'
  if (status === 'failed') return 'error'
  return 'neutral'
}

function KindPicker({ value, onChange, disabled }: { value: CleanupKind; onChange: (next: CleanupKind) => void; disabled: boolean }) {
  return (
    <div className="grid gap-2 sm:grid-cols-3" role="radiogroup" aria-label="清理方式">
      {KIND_OPTIONS.map((option) => {
        const selected = option.value === value
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            disabled={disabled}
            onClick={() => onChange(option.value)}
            className={`min-w-0 rounded-lg border px-3 py-2.5 text-left transition-colors disabled:cursor-not-allowed disabled:opacity-60 ${selected
              ? option.danger
                ? 'border-destructive/60 bg-destructive/[0.04] shadow-[0_0_0_1px_hsl(var(--destructive)/0.16)]'
                : 'border-primary bg-primary/[0.04] shadow-[0_0_0_1px_hsl(var(--primary)/0.16)]'
              : 'border-border/80 hover:border-foreground/30 hover:bg-muted/25'
            }`}
          >
            <span className="flex items-center gap-2">
              <span className={`flex size-4 shrink-0 items-center justify-center rounded-full border ${selected
                ? option.danger ? 'border-destructive bg-destructive text-white' : 'border-primary bg-primary text-primary-foreground'
                : 'border-muted-foreground/45 text-transparent'}`}
              >
                {selected && <Check className="size-3" aria-hidden="true" />}
              </span>
              <span className={`text-[13px] font-semibold ${option.danger && selected ? 'text-destructive' : 'text-foreground'}`}>{option.title}</span>
            </span>
            <span className="mt-1 block text-[11px] leading-4 text-muted-foreground">{option.desc}</span>
          </button>
        )
      })}
    </div>
  )
}

function TaskStatus({ status, onCancel, onResume, cancelling, resuming }: {
  status: UsageCleanupStatusResponse
  onCancel: () => void
  onResume: () => void
  cancelling: boolean
  resuming: boolean
}) {
  const running = status.status === 'queued' || status.status === 'running'
  const resumable = ['paused', 'failed', 'cancelled'].includes(status.status) && Boolean(status.jobId)
  const total = status.matchedRows ?? (status.remainingRows !== undefined ? status.processedRows + status.remainingRows : undefined)
  const percent = total && total > 0 ? Math.min(100, (status.processedRows / total) * 100) : status.status === 'completed' ? 100 : undefined
  const reason = status.stopReason ? STOP_REASON_LABEL[status.stopReason] ?? status.stopReason : undefined
  return (
    <section className="space-y-2.5 rounded-lg border bg-muted/20 p-3" aria-label="最近一次清理任务">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[13px] font-medium">最近一次清理</span>
        <Badge tone={statusTone(status.status)}>{STATUS_LABEL[status.status] ?? status.status}</Badge>
      </div>
      {percent !== undefined && <Progress value={percent} tone={status.status === 'failed' ? 'error' : running ? 'primary' : 'success'} />}
      <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <span>已清理 <span className="font-medium tabular-nums text-foreground">{formatNumber(status.processedRows)}</span> 条{total !== undefined && ` / ${formatNumber(total)}`}</span>
        <span>{status.includeSummary ? '清空全部' : status.mode === 'hard_delete' ? '删除明细' : '隐藏明细'}</span>
        {status.cutoffAt && <span>截止 {formatDate(status.cutoffAt)}</span>}
        {reason && <span>{reason}</span>}
      </div>
      {status.lastError && <p className="text-xs text-destructive">{status.lastError}</p>}
      {running && (
        <Button variant="outline" size="sm" className="w-full" onClick={onCancel} disabled={cancelling}>停止清理</Button>
      )}
      {resumable && (
        <Button variant="outline" size="sm" className="w-full" onClick={onResume} disabled={resuming}>
          {resuming ? '继续中…' : '继续清理剩余数据'}
        </Button>
      )}
    </section>
  )
}

export function UsageCleanupModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [kind, setKind] = useState<CleanupKind>('soft')
  const [range, setRange] = useState<CleanupRange>('all')
  const [olderThanDays, setOlderThanDays] = useState('30')
  const [showAdvanced, setShowAdvanced] = useState(false)
  const [batchSize, setBatchSize] = useState(String(CLEANUP_DEFAULT_BATCH_SIZE))
  const [pauseMs, setPauseMs] = useState(String(CLEANUP_DEFAULT_PAUSE_MS))
  const [previewResult, setPreviewResult] = useState<UsageCleanupPreviewResponse | null>(null)

  const preview = usePreviewUsageCleanup()
  const startCleanup = useStartUsageCleanup()
  const cancelCleanup = useCancelUsageCleanup()
  const resumeCleanup = useResumeUsageCleanup()
  const cleanupStatus = useUsageCleanupStatus()
  const confirm = useConfirm()

  const status = cleanupStatus.data
  const isRunning = ['queued', 'running'].includes(status?.status || '')
  const days = kind === 'all' || range === 'all' ? 0 : boundedInteger(olderThanDays, 30, 1, CLEANUP_MAX_OLDER_THAN_DAYS)

  // Any change to what would be cleaned makes the shown count stale.
  useEffect(() => { setPreviewResult(null) }, [kind, range, olderThanDays])

  const buildRequest = (): UsageCleanupRequest => ({
    mode: kind === 'hard' ? 'hard_delete' : 'soft_delete',
    includeSummary: kind === 'all',
    olderThanDays: days,
    batchSize: boundedInteger(batchSize, CLEANUP_DEFAULT_BATCH_SIZE, 1, CLEANUP_MAX_BATCH_SIZE),
    pauseMsBetweenBatches: boundedInteger(pauseMs, CLEANUP_DEFAULT_PAUSE_MS, 0, CLEANUP_MAX_PAUSE_MS),
  })

  const rangeText = days === 0 ? '现在之前的全部用量明细' : `${days} 天前的用量明细`

  const handlePreview = async () => {
    try {
      setPreviewResult(await preview.mutateAsync(buildRequest()))
    } catch (error) {
      toast.error(`统计失败：${extractErrorMessage(error)}`)
    }
  }

  const handleStart = async () => {
    const request = buildRequest()
    const count = previewResult ? `${formatNumber(previewResult.matchedRows)} 条` : ''
    const consequence = kind === 'all'
      ? '明细和全部汇总都会清空，总览、趋势、排行会从零开始，无法恢复。'
      : kind === 'hard'
        ? '明细会从数据库删除，无法恢复；总览和趋势的汇总保留。'
        : '明细不再显示；总览和趋势的汇总保留。'
    const ok = await confirm({
      title: KIND_OPTIONS.find((option) => option.value === kind)!.title,
      message: `将清理${rangeText}${count ? `，共 ${count}` : ''}。${consequence}`,
      confirmText: '开始清理',
      tone: kind === 'soft' ? 'default' : 'danger',
    })
    if (!ok) return
    try {
      await startCleanup.mutateAsync(request)
      toast.success('清理已开始')
      setPreviewResult(null)
    } catch (error) {
      toast.error(`启动失败：${extractErrorMessage(error)}`)
    }
  }

  const handleCancel = async () => {
    try {
      await cancelCleanup.mutateAsync()
      toast.success('已请求停止')
    } catch (error) {
      toast.error(`停止失败：${extractErrorMessage(error)}`)
    }
  }

  const handleResume = async () => {
    if (!status?.jobId) return
    try {
      await resumeCleanup.mutateAsync(status.jobId)
      toast.success('已继续清理')
    } catch (error) {
      toast.error(`继续失败：${extractErrorMessage(error)}`)
    }
  }

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title="清理用量记录"
      description="按批次在后台执行，不影响正在处理的请求。"
      width="max-w-2xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>关闭</Button>
          <Button variant="outline" onClick={() => void handlePreview()} disabled={preview.isPending || isRunning}>
            <Eye />{preview.isPending ? '统计中…' : '统计数量'}
          </Button>
          <Button
            variant={kind === 'soft' ? 'default' : 'destructive'}
            onClick={() => void handleStart()}
            disabled={startCleanup.isPending || isRunning}
          >
            <Trash2 />{startCleanup.isPending ? '启动中…' : '开始清理'}
          </Button>
        </>
      }
    >
      <div className="space-y-5 text-sm">
        <section className="space-y-2">
          <h3 className="text-[13px] font-medium">清理方式</h3>
          <KindPicker value={kind} onChange={setKind} disabled={isRunning} />
        </section>

        <section className="space-y-2">
          <h3 className="text-[13px] font-medium">清理范围</h3>
          {kind === 'all' ? (
            <p className="text-xs text-muted-foreground">清空全部时固定清理现在之前的所有数据。</p>
          ) : (
            <div className="flex flex-wrap items-center gap-2" role="radiogroup" aria-label="清理范围">
              {([['all', '全部明细'], ['older', '只清理较早的']] as const).map(([value, label]) => (
                <Button
                  key={value}
                  type="button"
                  role="radio"
                  aria-checked={range === value}
                  variant={range === value ? 'secondary' : 'outline'}
                  size="sm"
                  disabled={isRunning}
                  onClick={() => setRange(value)}
                >
                  {label}
                </Button>
              ))}
              {range === 'older' && (
                <label className="flex items-center gap-2 text-xs text-muted-foreground">
                  <Input
                    type="number"
                    min={1}
                    max={CLEANUP_MAX_OLDER_THAN_DAYS}
                    className="h-8 w-20 text-xs"
                    value={olderThanDays}
                    disabled={isRunning}
                    onChange={(event) => setOlderThanDays(event.target.value)}
                    aria-label="清理多少天前的明细"
                  />
                  天前的明细
                </label>
              )}
            </div>
          )}
        </section>

        <div className="rounded-lg border bg-card px-3.5 py-3" aria-live="polite">
          {previewResult ? (
            <div className="space-y-1">
              <p className="text-[13px]">
                将清理 <span className="text-base font-semibold tabular-nums">{formatNumber(previewResult.matchedRows)}</span> 条明细
              </p>
              <p className="text-xs text-muted-foreground">
                {previewResult.oldestCreatedAt && previewResult.newestCreatedAt
                  ? `${formatDate(previewResult.oldestCreatedAt)} 至 ${formatDate(previewResult.newestCreatedAt)}`
                  : '没有符合条件的明细'}
              </p>
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">
              将清理{rangeText}。点“统计数量”可以先看会清理多少条，清理会一直执行到全部清完。
            </p>
          )}
        </div>

        {kind !== 'soft' && (
          <Callout tone="error">
            {kind === 'all' ? '明细和汇总都会被清空，总览、趋势和费用统计会从零开始，无法恢复。' : '明细会从数据库直接删除，无法恢复；汇总保留，总览数字不变。'}
          </Callout>
        )}

        <section>
          <button
            type="button"
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
            aria-expanded={showAdvanced}
            onClick={() => setShowAdvanced((value) => !value)}
          >
            <ChevronDown className={`size-3.5 transition-transform ${showAdvanced ? 'rotate-180' : ''}`} aria-hidden="true" />
            执行速度
          </button>
          {showAdvanced && (
            <div className="mt-2 grid gap-3 sm:grid-cols-2">
              <label className="space-y-1">
                <span className="block text-xs text-muted-foreground">每批清理（条）</span>
                <Input type="number" min={1} max={CLEANUP_MAX_BATCH_SIZE} className="h-8 text-xs" value={batchSize} onChange={(event) => setBatchSize(event.target.value)} />
              </label>
              <label className="space-y-1">
                <span className="block text-xs text-muted-foreground">批次间隔（毫秒）</span>
                <Input type="number" min={0} max={CLEANUP_MAX_PAUSE_MS} className="h-8 text-xs" value={pauseMs} onChange={(event) => setPauseMs(event.target.value)} />
              </label>
              <p className="text-[11px] leading-4 text-muted-foreground sm:col-span-2">数据库压力大时，调小每批数量或加大间隔。默认值适合大多数情况。</p>
            </div>
          )}
        </section>

        {status && status.status !== 'idle' && (
          <TaskStatus
            status={status}
            onCancel={() => void handleCancel()}
            onResume={() => void handleResume()}
            cancelling={cancelCleanup.isPending}
            resuming={resumeCleanup.isPending}
          />
        )}
      </div>
    </ModalShell>
  )
}
