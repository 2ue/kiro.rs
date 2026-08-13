import { useState } from 'react'
import { toast } from 'sonner'
import { extractErrorMessage } from '@/lib/utils'
import { formatDate, formatNumber } from '@/lib/format'
import { usePreviewUsageCleanup, useStartUsageCleanup, useCancelUsageCleanup, useUsageCleanupStatus, useClearUsageRecords, useResumeUsageCleanup } from '@/hooks/use-usage'
import type { UsageCleanupRequest } from '@/types/api'
import { ModalShell, Callout, useConfirm } from '@/components/patterns'
import { Badge, Button, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui'

const CLEANUP_MAX_OLDER_THAN_DAYS = 3650
const CLEANUP_DEFAULT_BATCH_SIZE = 250
const CLEANUP_MAX_BATCH_SIZE = 5000
const CLEANUP_MAX_PAUSE_MS = 10000

function boundedInteger(value: string, fallback: number, min: number, max: number): number {
  const parsed = Number(value)
  const normalized = Number.isFinite(parsed) ? Math.floor(parsed) : fallback
  return Math.max(min, Math.min(max, normalized))
}

function cleanupRangeLabel(days: number): string {
  return days === 0 ? '执行时刻之前' : `${days} 天前`
}

export function UsageCleanupModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [olderThanDays, setOlderThanDays] = useState('7')
  const [mode, setMode] = useState<'soft_delete' | 'hard_delete'>('soft_delete')
  const [batchSize, setBatchSize] = useState(String(CLEANUP_DEFAULT_BATCH_SIZE))
  const [pauseMs, setPauseMs] = useState('100')
  const [previewResult, setPreviewResult] = useState<{ matchedRows: number; cutoffAt: string; oldestCreatedAt?: string; newestCreatedAt?: string } | null>(null)
  const [previewing, setPreviewing] = useState(false)

  const preview = usePreviewUsageCleanup()
  const startCleanup = useStartUsageCleanup()
  const cancelCleanup = useCancelUsageCleanup()
  const clearRecords = useClearUsageRecords()
  const resumeCleanup = useResumeUsageCleanup()
  const cleanupStatus = useUsageCleanupStatus()
  const confirm = useConfirm()

  const isRunning = ['queued', 'running'].includes(cleanupStatus.data?.status || '')
  const status = cleanupStatus.data

  const buildRequest = (): UsageCleanupRequest => ({
    mode,
    olderThanDays: boundedInteger(olderThanDays, 30, 0, CLEANUP_MAX_OLDER_THAN_DAYS),
    batchSize: boundedInteger(batchSize, CLEANUP_DEFAULT_BATCH_SIZE, 1, CLEANUP_MAX_BATCH_SIZE),
    pauseMsBetweenBatches: boundedInteger(pauseMs, 100, 0, CLEANUP_MAX_PAUSE_MS),
  })

  const handlePreview = async () => {
    setPreviewing(true)
    try {
      const result = await preview.mutateAsync(buildRequest())
      setPreviewResult(result)
    } catch (e) {
      toast.error(`预览失败: ${extractErrorMessage(e)}`)
    } finally {
      setPreviewing(false)
    }
  }

  const handleStart = async () => {
    const request = buildRequest()
    const previewText = previewResult
      ? `预计命中 ${formatNumber(previewResult.matchedRows)} 条，`
      : ''
    const ok = await confirm({
      title: '确认清理',
      message: `将${previewText}清理 ${cleanupRangeLabel(request.olderThanDays ?? 7)}的记录（${mode === 'hard_delete' ? '物理删除，不可恢复' : '软删除'}）。确认执行？`,
      confirmText: '执行清理',
      tone: 'danger',
    })
    if (!ok) return
    try {
      await startCleanup.mutateAsync(buildRequest())
      toast.success('清理任务已启动')
      setPreviewResult(null)
    } catch (e) {
      toast.error(`启动失败: ${extractErrorMessage(e)}`)
    }
  }

  const handleCancel = async () => {
    try {
      await cancelCleanup.mutateAsync()
      toast.success('已取消清理任务')
    } catch (e) {
      toast.error(`取消失败: ${extractErrorMessage(e)}`)
    }
  }

  const handleClearAll = async () => {
    if (isRunning) {
      toast.warning('清理任务正在排队或执行，不能重复提交')
      return
    }
    const ok = await confirm({
      title: '清空所有记录',
      message: '将提交后台任务，分批软删除全部用量明细，并同步扣除这些记录对应的累计统计、费用和 Dashboard 汇总。任务可取消并审计。确认继续？',
      confirmText: '提交清理任务',
      tone: 'danger',
    })
    if (!ok) return
    try {
      await clearRecords.mutateAsync()
      toast.success('全量明细清理任务已提交')
    } catch (e) {
      toast.error(`清空失败: ${extractErrorMessage(e)}`)
    }
  }

  const handleResume = async () => {
    if (!status?.jobId) return
    try {
      await resumeCleanup.mutateAsync(status.jobId)
      toast.success('清理任务已重新排队')
    } catch (e) {
      toast.error(`恢复失败: ${extractErrorMessage(e)}`)
    }
  }

  return (
    <ModalShell open={open} onClose={onClose} title="清理用量记录" width="max-w-lg">
      <div className="space-y-4 text-sm">
        {/* 危险区：清空全部 */}
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 space-y-2">
          <div className="text-xs font-semibold text-destructive">危险操作</div>
          <p className="text-xs text-muted-foreground">
            后台分批软删除全部历史明细，并同步扣除对应的累计统计、费用和 Dashboard 汇总。任务状态会持久化，可取消并审计。
          </p>
          <Button
            variant="outline"
            size="sm"
            className="text-destructive border-destructive/40 hover:bg-destructive/10 w-full"
            onClick={handleClearAll}
            disabled={clearRecords.isPending || isRunning}
          >
            {isRunning ? '清理任务执行中' : '清理全部历史明细'}
          </Button>
        </div>

        {/* 当前任务状态 */}
        {status && status.status !== 'idle' && (
          <div className="rounded-lg bg-muted/30 p-3 space-y-2">
            <div className="flex items-center justify-between">
              <span className="font-medium">清理任务状态</span>
              <Badge tone={
                ['queued', 'running'].includes(status.status) ? 'warning'
                : status.status === 'completed' ? 'success'
                : status.status === 'failed' ? 'error'
                : 'neutral'
              }>
                {status.status === 'queued' ? '排队中'
                  : status.status === 'running' ? '执行中'
                  : status.status === 'paused' ? '已暂停'
                  : status.status === 'completed' ? '已完成'
                  : status.status === 'failed' ? '失败'
                  : status.status === 'cancelled' ? '已取消'
                  : status.status}
              </Badge>
            </div>
            <div className="grid grid-cols-2 gap-2 text-xs text-muted-foreground">
              {status.jobId && <span className="col-span-2">任务 ID: {status.jobId}</span>}
              <span>已处理: {formatNumber(status.processedRows)} 条</span>
              <span>阶段: {status.phase}</span>
              {status.matchedRows && <span>匹配: {formatNumber(status.matchedRows)} 条</span>}
              {status.remainingRows !== undefined && <span>剩余: {formatNumber(status.remainingRows)} 条</span>}
              <span>累计批次: {status.batches}</span>
              <span>单次执行上限: {status.maxBatches}</span>
              {status.lastBatchRows > 0 && <span>最后一批: {formatNumber(status.lastBatchRows)} 条</span>}
              {status.stopReason && <span className="col-span-2">停止原因: {status.stopReason}</span>}
              {status.redisDeleteCommands > 0 && <span className="col-span-2">Redis: {formatNumber(status.redisDeletedKeys)} keys / {formatNumber(status.redisDeleteCommands)} commands / 最大批 {formatNumber(status.redisMaxCommandKeys)}</span>}
            </div>
            {status.lastError && (
              <div className="text-xs text-destructive">{status.lastError}</div>
            )}
            {isRunning && (
              <Button variant="outline" size="sm" className="text-destructive w-full" onClick={handleCancel} disabled={cancelCleanup.isPending}>
                取消清理任务
              </Button>
            )}
            {!isRunning && ['paused', 'failed', 'cancelled'].includes(status.status) && status.jobId && (
              <Button variant="outline" size="sm" className="w-full" onClick={handleResume} disabled={resumeCleanup.isPending}>
                {resumeCleanup.isPending ? '恢复中...' : '恢复此任务'}
              </Button>
            )}
          </div>
        )}

        {/* 配置区 */}
        <div className="space-y-3">
          <div className="flex items-center gap-3">
            <label className="text-xs text-muted-foreground w-20 shrink-0">删除模式</label>
            <Select value={mode} onValueChange={(v) => {
              setMode(v as 'soft_delete' | 'hard_delete')
              setPreviewResult(null)
            }}>
              <SelectTrigger size="sm" className="flex-1"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="soft_delete">软删除（标记删除）</SelectItem>
                <SelectItem value="hard_delete">物理删除（不可恢复）</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="flex items-center gap-3">
            <label className="text-xs text-muted-foreground w-20 shrink-0">保留天数</label>
            <Input
              type="number"
              min={0}
              max={CLEANUP_MAX_OLDER_THAN_DAYS}
              className="flex-1 h-8 text-xs"
              value={olderThanDays}
              onChange={(e) => {
                setOlderThanDays(e.target.value)
                setPreviewResult(null)
              }}
            />
            <span className="text-xs text-muted-foreground">天前的记录</span>
          </div>
          <p className="pl-[5.5rem] text-[0.68rem] text-muted-foreground/50">允许填 0，表示以任务开始时间作为截止点。</p>
          <div className="space-y-1">
            <div className="flex items-center gap-3">
              <label className="text-xs text-muted-foreground w-20 shrink-0">每批数量</label>
              <Input
                type="number"
                min={1}
                max={CLEANUP_MAX_BATCH_SIZE}
                className="flex-1 h-8 text-xs"
                value={batchSize}
                onChange={(e) => {
                  setBatchSize(e.target.value)
                  setPreviewResult(null)
                }}
                placeholder={String(CLEANUP_DEFAULT_BATCH_SIZE)}
              />
              <span className="text-xs text-muted-foreground/60">条</span>
            </div>
            <p className="pl-[5.5rem] text-[0.68rem] text-muted-foreground/50">每批短事务，默认 {CLEANUP_DEFAULT_BATCH_SIZE}，后端安全上限 {formatNumber(CLEANUP_MAX_BATCH_SIZE)}；单批过大时任务可能因锁争用暂停，可降低后恢复。</p>
          </div>
          <div className="space-y-1">
            <div className="flex items-center gap-3">
              <label className="text-xs text-muted-foreground w-20 shrink-0">批次间隔</label>
              <Input
                type="number"
                min={0}
                max={CLEANUP_MAX_PAUSE_MS}
                className="flex-1 h-8 text-xs"
                value={pauseMs}
                onChange={(e) => {
                  setPauseMs(e.target.value)
                  setPreviewResult(null)
                }}
                placeholder="100"
              />
              <span className="text-xs text-muted-foreground/60">ms</span>
            </div>
            <p className="pl-[5.5rem] text-[0.68rem] text-muted-foreground/50">每批之间的等待毫秒，后端安全上限 {CLEANUP_MAX_PAUSE_MS}ms，默认 100ms</p>
          </div>
          <div className="text-xs text-muted-foreground/60">每次执行受 maxBatches 保护，达到上限后进入暂停状态，可由管理员显式恢复下一轮。</div>
        </div>

        {mode === 'hard_delete' && (
          <Callout tone="error">物理删除无法恢复，请谨慎操作。</Callout>
        )}

        {/* 预览结果 */}
        {previewResult && (
          <div className="rounded-lg bg-muted/30 p-3 space-y-1 text-xs">
            <div className="font-medium">预览结果</div>
            <div>匹配记录: <span className="font-semibold tabular-nums">{formatNumber(previewResult.matchedRows)}</span> 条</div>
            <div>截止时间: <span className="tabular-nums">{formatDate(previewResult.cutoffAt)}</span></div>
            {previewResult.oldestCreatedAt && <div>最旧记录: {formatDate(previewResult.oldestCreatedAt)}</div>}
            {previewResult.newestCreatedAt && <div>最新记录: {formatDate(previewResult.newestCreatedAt)}</div>}
          </div>
        )}

        {/* 操作按钮 */}
        <div className="flex flex-wrap gap-2 justify-end pt-1">
          <Button variant="outline" size="sm" onClick={handlePreview} disabled={previewing || isRunning}>
            {previewing ? '预览中...' : '预览'}
          </Button>
          <Button size="sm" onClick={handleStart} disabled={startCleanup.isPending || isRunning}>
            {startCleanup.isPending ? '启动中...' : '执行清理'}
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}
