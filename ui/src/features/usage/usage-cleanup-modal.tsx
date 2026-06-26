import { useState } from 'react'
import { toast } from 'sonner'
import { extractErrorMessage } from '@/lib/utils'
import { formatDate, formatNumber } from '@/lib/format'
import { usePreviewUsageCleanup, useStartUsageCleanup, useCancelUsageCleanup, useUsageCleanupStatus, useClearUsageRecords } from '@/hooks/use-usage'
import type { UsageCleanupRequest } from '@/types/api'
import { ModalShell, Callout, useConfirm } from '@/components/patterns'
import { Badge, Button, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui'

export function UsageCleanupModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [olderThanDays, setOlderThanDays] = useState('30')
  const [mode, setMode] = useState<'soft_delete' | 'hard_delete'>('soft_delete')
  const [batchSize, setBatchSize] = useState('1000')
  const [pauseMs, setPauseMs] = useState('100')
  const [previewResult, setPreviewResult] = useState<{ matchedRows: number; cutoffAt: string; oldestCreatedAt?: string; newestCreatedAt?: string } | null>(null)
  const [previewing, setPreviewing] = useState(false)

  const preview = usePreviewUsageCleanup()
  const startCleanup = useStartUsageCleanup()
  const cancelCleanup = useCancelUsageCleanup()
  const clearRecords = useClearUsageRecords()
  const cleanupStatus = useUsageCleanupStatus()
  const confirm = useConfirm()

  const isRunning = cleanupStatus.data?.status === 'running'
  const status = cleanupStatus.data

  const buildRequest = (): UsageCleanupRequest => ({
    mode,
    olderThanDays: Number(olderThanDays),
    batchSize: Number(batchSize) || 1000,
    pauseMsBetweenBatches: Number(pauseMs) || 100,
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
    if (!previewResult) return
    const ok = await confirm({
      title: '确认清理',
      message: `将删除 ${previewResult.matchedRows} 条 ${olderThanDays} 天前的记录（${mode === 'hard_delete' ? '物理删除，不可恢复' : '软删除'}）。确认执行？`,
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
    const ok = await confirm({
      title: '清空所有记录',
      message: '将清空所有用量展示记录（仅影响展示，不影响实际计费），此操作无法撤销，确认继续？',
      confirmText: '清空全部',
      tone: 'danger',
    })
    if (!ok) return
    try {
      await clearRecords.mutateAsync()
      toast.success('已清空用量展示记录')
      onClose()
    } catch (e) {
      toast.error(`清空失败: ${extractErrorMessage(e)}`)
    }
  }

  return (
    <ModalShell open={open} onClose={onClose} title="清理用量记录" width="max-w-lg">
      <div className="space-y-4 text-sm">
        {/* 当前任务状态 */}
        {status && status.status !== 'idle' && (
          <div className="rounded-lg border border-border bg-muted/40 p-3 space-y-2">
            <div className="flex items-center justify-between">
              <span className="font-medium">清理任务状态</span>
              <Badge tone={
                status.status === 'running' ? 'warning'
                : status.status === 'completed' ? 'success'
                : status.status === 'failed' ? 'error'
                : 'neutral'
              }>
                {status.status === 'running' ? '执行中'
                  : status.status === 'completed' ? '已完成'
                  : status.status === 'failed' ? '失败'
                  : status.status === 'cancelled' ? '已取消'
                  : status.status}
              </Badge>
            </div>
            <div className="grid grid-cols-2 gap-2 text-xs text-muted-foreground">
              {status.jobId && <span className="col-span-2">任务 ID: {status.jobId}</span>}
              <span>已处理: {formatNumber(status.processedRows)} 条</span>
              {status.matchedRows && <span>匹配: {formatNumber(status.matchedRows)} 条</span>}
              {status.remainingRows !== undefined && <span>剩余: {formatNumber(status.remainingRows)} 条</span>}
              <span>批次: {status.batches} / 上限 {status.maxBatches}</span>
              {status.stopReason && <span className="col-span-2">停止原因: {status.stopReason}</span>}
            </div>
            {status.lastError && (
              <div className="text-xs text-destructive">{status.lastError}</div>
            )}
            {isRunning && (
              <Button variant="outline" size="sm" className="text-destructive w-full" onClick={handleCancel} disabled={cancelCleanup.isPending}>
                取消清理任务
              </Button>
            )}
          </div>
        )}

        {/* 配置区 */}
        <div className="space-y-3">
          <div className="flex items-center gap-3">
            <label className="text-xs text-muted-foreground w-20 shrink-0">删除模式</label>
            <Select value={mode} onValueChange={(v) => setMode(v as 'soft_delete' | 'hard_delete')}>
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
              min={1}
              max={3650}
              className="flex-1 h-8 text-xs"
              value={olderThanDays}
              onChange={(e) => setOlderThanDays(e.target.value)}
            />
            <span className="text-xs text-muted-foreground">天前的记录</span>
          </div>
          <div className="flex items-center gap-3">
            <label className="text-xs text-muted-foreground w-20 shrink-0">每批数量</label>
            <Input
              type="number"
              min={1}
              className="flex-1 h-8 text-xs"
              value={batchSize}
              onChange={(e) => setBatchSize(e.target.value)}
              placeholder="1000"
            />
            <span className="text-xs text-muted-foreground/60">条</span>
          </div>
          <div className="flex items-center gap-3">
            <label className="text-xs text-muted-foreground w-20 shrink-0">批次间隔</label>
            <Input
              type="number"
              min={0}
              className="flex-1 h-8 text-xs"
              value={pauseMs}
              onChange={(e) => setPauseMs(e.target.value)}
              placeholder="100"
            />
            <span className="text-xs text-muted-foreground/60">ms</span>
          </div>
          <div className="text-xs text-muted-foreground/60">系统后端保留安全上限（maxBatches），超出后自动停止。</div>
        </div>

        {mode === 'hard_delete' && (
          <Callout tone="error">物理删除无法恢复，请谨慎操作。</Callout>
        )}

        {/* 预览结果 */}
        {previewResult && (
          <div className="rounded-lg border border-border bg-muted/40 p-3 space-y-1 text-xs">
            <div className="font-medium">预览结果</div>
            <div>匹配记录: <span className="font-semibold tabular-nums">{formatNumber(previewResult.matchedRows)}</span> 条</div>
            <div>截止时间: <span className="tabular-nums">{formatDate(previewResult.cutoffAt)}</span></div>
            {previewResult.oldestCreatedAt && <div>最旧记录: {formatDate(previewResult.oldestCreatedAt)}</div>}
            {previewResult.newestCreatedAt && <div>最新记录: {formatDate(previewResult.newestCreatedAt)}</div>}
          </div>
        )}

        {/* 操作按钮 */}
        <div className="flex flex-wrap gap-2 justify-end pt-1 border-t border-border">
          <Button variant="ghost" size="sm" className="text-destructive hover:bg-destructive/10" onClick={handleClearAll} disabled={clearRecords.isPending}>
            清空全部展示记录
          </Button>
          <div className="flex gap-2 ml-auto">
            <Button variant="outline" size="sm" onClick={handlePreview} disabled={previewing || isRunning}>
              {previewing ? '预览中...' : '预览'}
            </Button>
            <Button size="sm" onClick={handleStart} disabled={!previewResult || startCleanup.isPending || isRunning}>
              {startCleanup.isPending ? '启动中...' : '执行清理'}
            </Button>
          </div>
        </div>
      </div>
    </ModalShell>
  )
}
