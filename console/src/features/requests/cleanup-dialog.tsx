import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { usageApi } from '@/api/endpoints/usage'
import type { UsageCleanupMode } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { NumberInput, SelectControl } from '@/components/patterns/fields'
import { Meter } from '@/components/status/meter'
import { useConfirm } from '@/components/patterns/confirm'
import { fmtDateTime, fmtInt } from '@/lib/format'
import { qk } from '@/queries/keys'

const ACTIVE = new Set(['queued', 'running'])

/** 历史请求清理：预览 → 确认 → 后台分批执行，可暂停恢复 */
export function CleanupDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (v: boolean) => void }) {
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const [days, setDays] = useState(30)
  const [mode, setMode] = useState<UsageCleanupMode>('soft_delete')

  const status = useQuery({
    queryKey: qk.usage.cleanup,
    queryFn: usageApi.cleanupStatus,
    enabled: open,
    refetchInterval: (q) => (q.state.data && ACTIVE.has(q.state.data.status) ? 2000 : false),
  })
  const preview = useMutation({ mutationFn: () => usageApi.cleanupPreview({ mode, olderThanDays: days }), meta: { error: '预览失败' } })
  const start = useMutation({
    mutationFn: () => usageApi.cleanupStart({ mode, olderThanDays: days }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: qk.usage.cleanup }),
    meta: { success: '清理任务已开始', error: '启动失败' },
  })
  const cancel = useMutation({
    mutationFn: usageApi.cleanupCancel,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: qk.usage.cleanup }),
    meta: { error: '取消失败' },
  })
  const resume = useMutation({
    mutationFn: (jobId: string) => usageApi.cleanupResume(jobId),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: qk.usage.cleanup }),
    meta: { error: '恢复失败' },
  })

  const s = status.data
  const running = s && ACTIVE.has(s.status)

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>清理历史请求记录</DialogTitle>
          <DialogDescription>只删除请求明细，不影响账号、配置与已聚合的统计。</DialogDescription>
        </DialogHeader>

        {s && s.status !== 'idle' && (
          <div className="space-y-2 rounded-lg border p-3 text-sm">
            <div className="flex justify-between">
              <span>当前任务：{s.status}</span>
              <span className="num text-muted-foreground">
                {fmtInt(s.processedRows)} / {fmtInt(s.matchedRows ?? 0)}
              </span>
            </div>
            {s.matchedRows ? <Meter value={s.processedRows} max={s.matchedRows} /> : null}
            {s.lastError && <p className="text-xs text-danger">{s.lastError}</p>}
            <div className="flex gap-2">
              {running && (
                <Button size="xs" variant="outline" onClick={() => cancel.mutate()}>
                  取消
                </Button>
              )}
              {s.status === 'paused' && s.jobId && (
                <Button size="xs" variant="outline" onClick={() => resume.mutate(s.jobId!)}>
                  继续
                </Button>
              )}
            </div>
          </div>
        )}

        <div className="grid grid-cols-2 gap-3">
          <div className="space-y-1.5">
            <Label>保留最近</Label>
            <NumberInput value={days} min={1} suffix="天" onChange={(v) => setDays(v ?? 30)} />
          </div>
          <div className="space-y-1.5">
            <Label>删除方式</Label>
            <SelectControl
              value={mode}
              onChange={setMode}
              options={[
                { value: 'soft_delete', label: '软删除（可恢复）' },
                { value: 'hard_delete', label: '物理删除' },
              ]}
            />
          </div>
        </div>
        {preview.data && (
          <p className="text-sm">
            将删除 <span className="num font-semibold">{fmtInt(preview.data.matchedRows)}</span> 条早于 {fmtDateTime(preview.data.cutoffAt)} 的记录
          </p>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => preview.mutate()} disabled={preview.isPending || !!running}>
            {preview.isPending && <Loader2 className="animate-spin" />}预览
          </Button>
          <Button
            variant="destructive"
            disabled={!preview.data || !!running || start.isPending}
            onClick={async () => {
              const ok = await confirm({
                title: `删除 ${fmtInt(preview.data?.matchedRows ?? 0)} 条请求记录？`,
                description: mode === 'hard_delete' ? '物理删除不可恢复。' : '软删除后记录不再显示。',
                destructive: true,
                confirmText: '开始清理',
              })
              if (ok) start.mutate()
            }}
          >
            开始清理
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
