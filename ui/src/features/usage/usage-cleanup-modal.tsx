import { useState } from 'react'
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
import type { UsageCleanupRequest } from '@/types/api'
import { Badge, Button, Checkbox, Input } from '@/components/ui'
import { Callout, ModalShell, useConfirm } from '@/components/patterns'

const CLEANUP_MAX_OLDER_THAN_DAYS = 3650
const CLEANUP_DEFAULT_OLDER_THAN_DAYS = 0
const CLEANUP_DEFAULT_MAX_ROWS = 10_000
const CLEANUP_DEFAULT_BATCH_SIZE = 5_000
const CLEANUP_MAX_ROWS = 50_000_000
const CLEANUP_MAX_PAUSE_MS = 10_000
const CLEANUP_DEFAULT_PAUSE_MS = 10

function boundedInteger(value: string, fallback: number, min: number, max: number): number {
  const parsed = Number(value)
  const normalized = Number.isFinite(parsed) ? Math.floor(parsed) : fallback
  return Math.max(min, Math.min(max, normalized))
}

function cleanupRangeLabel(days: number): string {
  return days === 0 ? '任务开始前的全部明细' : `${days} 天前的明细`
}

function statusLabel(status: string): string {
  switch (status) {
    case 'queued':
      return '排队中'
    case 'running':
      return '执行中'
    case 'paused':
      return '已暂停'
    case 'completed':
      return '已完成'
    case 'cancelled':
      return '已取消'
    case 'failed':
      return '失败'
    default:
      return status
  }
}

export function UsageCleanupModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [olderThanDays, setOlderThanDays] = useState(String(CLEANUP_DEFAULT_OLDER_THAN_DAYS))
  const [maxRows, setMaxRows] = useState(String(CLEANUP_DEFAULT_MAX_ROWS))
  const [pauseMs, setPauseMs] = useState(String(CLEANUP_DEFAULT_PAUSE_MS))
  const [includeSummary, setIncludeSummary] = useState(false)
  const [physicalDelete, setPhysicalDelete] = useState(false)
  const [previewResult, setPreviewResult] = useState<{
    mode: UsageCleanupRequest['mode']
    matchedRows: number
    cutoffAt: string
    oldestCreatedAt?: string
    newestCreatedAt?: string
    includeSummary: boolean
  } | null>(null)
  const [previewing, setPreviewing] = useState(false)

  const preview = usePreviewUsageCleanup()
  const startCleanup = useStartUsageCleanup()
  const cancelCleanup = useCancelUsageCleanup()
  const resumeCleanup = useResumeUsageCleanup()
  const cleanupStatus = useUsageCleanupStatus()
  const confirm = useConfirm()

  const status = cleanupStatus.data
  const isRunning = ['queued', 'running'].includes(status?.status || '')

  const buildRequest = (): UsageCleanupRequest => ({
    mode: physicalDelete && !includeSummary ? 'hard_delete' : 'soft_delete',
    includeSummary,
    olderThanDays: includeSummary
      ? 0
      : boundedInteger(olderThanDays, CLEANUP_DEFAULT_OLDER_THAN_DAYS, 0, CLEANUP_MAX_OLDER_THAN_DAYS),
    maxRows: includeSummary
      ? 0
      : boundedInteger(maxRows, CLEANUP_DEFAULT_MAX_ROWS, 0, CLEANUP_MAX_ROWS),
    batchSize: CLEANUP_DEFAULT_BATCH_SIZE,
    pauseMsBetweenBatches: boundedInteger(pauseMs, CLEANUP_DEFAULT_PAUSE_MS, 0, CLEANUP_MAX_PAUSE_MS),
  })

  const handlePreview = async () => {
    setPreviewing(true)
    try {
      const result = await preview.mutateAsync(buildRequest())
      setPreviewResult(result)
    } catch (error) {
      toast.error(`预览失败：${extractErrorMessage(error)}`)
    } finally {
      setPreviewing(false)
    }
  }

  const handleStart = async () => {
    const request = buildRequest()
    const previewText = previewResult
      && previewResult.mode === request.mode
      && previewResult.includeSummary === request.includeSummary
      ? `预计命中 ${formatNumber(previewResult.matchedRows)} 条，`
      : ''
    const summaryText = request.includeSummary
      ? '同时清空历史汇总，完成后无法恢复'
      : request.mode === 'hard_delete'
        ? '明细将直接从数据库删除，历史汇总会保留'
        : '历史汇总会保留'
    const ok = await confirm({
      title: request.includeSummary
        ? '确认清理全部历史数据'
        : request.mode === 'hard_delete'
          ? '确认物理删除用量明细'
          : '确认清理用量明细',
      message: `将${previewText}清理${cleanupRangeLabel(request.olderThanDays ?? 0)}；${summaryText}。`,
      confirmText: '开始清理',
      tone: request.includeSummary || request.mode === 'hard_delete' ? 'danger' : 'default',
    })
    if (!ok) return

    try {
      await startCleanup.mutateAsync(request)
      toast.success('清理任务已启动')
      setPreviewResult(null)
    } catch (error) {
      toast.error(`启动失败：${extractErrorMessage(error)}`)
    }
  }

  const handleCancel = async () => {
    try {
      await cancelCleanup.mutateAsync()
      toast.success('已请求停止清理')
    } catch (error) {
      toast.error(`取消失败：${extractErrorMessage(error)}`)
    }
  }

  const handleResume = async () => {
    if (!status?.jobId) return
    try {
      await resumeCleanup.mutateAsync(status.jobId)
      toast.success('清理任务已重新排队')
    } catch (error) {
      toast.error(`恢复失败：${extractErrorMessage(error)}`)
    }
  }

  const handleSummaryChange = (checked: boolean) => {
    setIncludeSummary(checked)
    if (checked) {
      setOlderThanDays('0')
      setPhysicalDelete(false)
    }
    setPreviewResult(null)
  }

  return (
    <ModalShell open={open} onClose={onClose} title="清理用量" width="max-w-lg">
      <div className="space-y-5 text-sm">
        <div className="space-y-1">
          <div className="font-medium">清理范围</div>
          <p className="text-xs text-muted-foreground">
            默认从任务开始时刻往前清理，0 表示当前之前的全部明细。
          </p>
          <div className="grid grid-cols-[1fr_auto] items-center gap-3">
            <Input
              type="number"
              min={0}
              max={CLEANUP_MAX_OLDER_THAN_DAYS}
              value={olderThanDays}
              disabled={includeSummary}
              onChange={(event) => {
                setOlderThanDays(event.target.value)
                setPreviewResult(null)
              }}
              aria-label="清理多少天前"
            />
            <span className="text-xs text-muted-foreground">天前</span>
          </div>
        </div>

        <div className="space-y-3">
          <div className="font-medium">清理选项</div>
          <div className="grid grid-cols-[1fr_auto] items-center gap-3">
            <label htmlFor="usage-cleanup-max-rows" className="text-xs text-muted-foreground">
              本次最多清理
            </label>
            <div className="flex items-center gap-2">
              <Input
                id="usage-cleanup-max-rows"
                type="number"
                min={0}
                max={CLEANUP_MAX_ROWS}
                className="h-8 w-32 text-xs"
                value={maxRows}
                disabled={includeSummary}
                onChange={(event) => {
                  setMaxRows(event.target.value)
                  setPreviewResult(null)
                }}
              />
              <span className="text-xs text-muted-foreground">条，0 = 全部</span>
            </div>
          </div>
          <div className="grid grid-cols-[1fr_auto] items-center gap-3">
            <label htmlFor="usage-cleanup-pause" className="text-xs text-muted-foreground">
              批次间隔
            </label>
            <div className="flex items-center gap-2">
              <Input
                id="usage-cleanup-pause"
                type="number"
                min={0}
                max={CLEANUP_MAX_PAUSE_MS}
                className="h-8 w-32 text-xs"
                value={pauseMs}
                onChange={(event) => {
                  setPauseMs(event.target.value)
                  setPreviewResult(null)
                }}
              />
              <span className="text-xs text-muted-foreground">ms</span>
            </div>
          </div>
          <label className="flex cursor-pointer items-start gap-2 rounded-lg border border-destructive/25 bg-destructive/5 p-3">
            <Checkbox
              checked={includeSummary}
              onCheckedChange={(value) => handleSummaryChange(Boolean(value))}
              className="mt-0.5"
            />
            <span className="space-y-0.5">
              <span className="block font-medium text-destructive">清理全部历史数据（包含汇总）</span>
              <span className="block text-xs text-muted-foreground">
                自动使用当前时刻作为范围，明细、趋势、排行和费用汇总都会清空。
              </span>
            </span>
          </label>
          <label className="flex cursor-pointer items-start gap-2 rounded-lg border border-destructive/25 bg-destructive/5 p-3">
            <Checkbox
              checked={physicalDelete}
              disabled={includeSummary}
              onCheckedChange={(value) => {
                setPhysicalDelete(Boolean(value))
                setPreviewResult(null)
              }}
              className="mt-0.5"
            />
            <span className="space-y-0.5">
              <span className="block font-medium text-destructive">物理删除明细</span>
              <span className="block text-xs text-muted-foreground">
                直接从数据库删除命中的活跃明细或旧软删除明细，无法恢复；历史汇总默认保留。
              </span>
            </span>
          </label>
        </div>

        {includeSummary && (
          <Callout tone="error">汇总也会被清空，清理完成后无法恢复。默认不勾选。</Callout>
        )}
        {physicalDelete && !includeSummary && (
          <Callout tone="error">
            当前为物理删除模式，明细删除后无法恢复；汇总不会同步扣减，只有勾选“包含汇总”时才会重建汇总。
          </Callout>
        )}

        {previewResult && (
          <div className="space-y-1 rounded-lg bg-muted/30 p-3 text-xs">
            <div className="font-medium">预览结果</div>
            <div>
              明细：<span className="font-semibold tabular-nums">{formatNumber(previewResult.matchedRows)}</span> 条
            </div>
            <div>方式：{previewResult.mode === 'hard_delete' ? '物理删除' : '软删除'}</div>
            <div>汇总：{previewResult.includeSummary ? '将清空' : '保留'}</div>
            <div>截止：{formatDate(previewResult.cutoffAt)}</div>
            {previewResult.oldestCreatedAt && <div>最早：{formatDate(previewResult.oldestCreatedAt)}</div>}
            {previewResult.newestCreatedAt && <div>最近：{formatDate(previewResult.newestCreatedAt)}</div>}
          </div>
        )}

        {status && status.status !== 'idle' && (
          <div className="space-y-3 rounded-lg border bg-muted/20 p-3">
            <div className="flex items-center justify-between">
              <span className="font-medium">最近任务</span>
              <Badge tone={
                ['queued', 'running'].includes(status.status)
                  ? 'warning'
                  : status.status === 'completed'
                    ? 'success'
                    : status.status === 'failed'
                      ? 'error'
                      : 'neutral'
              }>
                {statusLabel(status.status)}
              </Badge>
            </div>
            <div className="grid grid-cols-2 gap-2 text-xs text-muted-foreground">
              <span>已处理 {formatNumber(status.processedRows)} 条</span>
              <span>批次 {formatNumber(status.batches)}</span>
              {status.remainingRows !== undefined && <span>剩余 {formatNumber(status.remainingRows)} 条</span>}
              <span>{status.mode === 'hard_delete' ? '物理删除' : '软删除'}</span>
              <span>{status.includeSummary ? '包含汇总' : '保留汇总'}</span>
            </div>
            {status.stopReason && <div className="text-xs text-muted-foreground">{status.stopReason}</div>}
            {status.lastError && <div className="text-xs text-destructive">{status.lastError}</div>}
            {isRunning && (
              <Button variant="outline" size="sm" className="w-full" onClick={handleCancel} disabled={cancelCleanup.isPending}>
                取消任务
              </Button>
            )}
            {!isRunning && ['paused', 'failed', 'cancelled'].includes(status.status) && status.jobId && (
              <Button variant="outline" size="sm" className="w-full" onClick={handleResume} disabled={resumeCleanup.isPending}>
                {resumeCleanup.isPending ? '恢复中...' : '恢复任务'}
              </Button>
            )}
          </div>
        )}

        <div className="flex items-center justify-end gap-2 border-t pt-4">
          <Button variant="outline" size="sm" onClick={handlePreview} disabled={previewing || isRunning}>
            {previewing ? '预览中...' : '预览'}
          </Button>
          <Button size="sm" onClick={handleStart} disabled={startCleanup.isPending || isRunning}>
            {startCleanup.isPending ? '启动中...' : '开始清理'}
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}
