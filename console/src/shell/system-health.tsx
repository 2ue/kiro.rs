import { useState } from 'react'
import { HeartPulse } from 'lucide-react'
import { SidebarMenu, SidebarMenuButton, SidebarMenuItem } from '@/components/ui/sidebar'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet'
import { StatusDot } from '@/components/status/tone-badge'
import { Meter } from '@/components/status/meter'
import { Stat, StatGrid } from '@/components/patterns/stat'
import type { UsageRecorderStats } from '@/api/types'
import { fmtInt } from '@/lib/format'
import { useCredentialSummary, useSystemVersion, useWriterStats } from '@/queries/shared'
import type { Tone } from '@/components/status/tone'

export function writerHealth(stats: UsageRecorderStats | undefined): { tone: Tone; label: string } {
  if (!stats) return { tone: 'neutral', label: '未知' }
  if (stats.accepting === false) return { tone: 'danger', label: '已停止接收' }
  if (stats.droppedPersistRecords > 0 || stats.droppedRedisRecords > 0) return { tone: 'danger', label: '有记录丢弃' }
  if ((stats.backpressuredPersistRecords ?? 0) > 0 || (stats.backpressuredRedisRecords ?? 0) > 0)
    return { tone: 'warning', label: '写入背压' }
  return { tone: 'success', label: '正常' }
}

function QueueRow({ label, enabled, capacity, available }: { label: string; enabled: boolean; capacity: number; available: number }) {
  if (!enabled) {
    return (
      <div className="flex items-center justify-between text-sm">
        <span>{label}</span>
        <span className="text-xs text-muted-foreground">未启用</span>
      </div>
    )
  }
  const used = Math.max(0, capacity - available)
  const ratio = capacity > 0 ? used / capacity : 0
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between text-sm">
        <span>{label}</span>
        <span className="num text-xs text-muted-foreground">
          {fmtInt(used)} / {fmtInt(capacity)}
        </span>
      </div>
      <Meter value={used} max={capacity} tone={ratio > 0.8 ? 'danger' : ratio > 0.5 ? 'warning' : 'success'} label={`${label}占用`} />
    </div>
  )
}

export function SystemHealthButton() {
  const [open, setOpen] = useState(false)
  const writer = useWriterStats()
  const summary = useCredentialSummary()
  const version = useSystemVersion()
  const health = writerHealth(writer.data)
  const s = writer.data

  return (
    <>
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton onClick={() => setOpen(true)} tooltip={`系统健康：${health.label}`}>
            <HeartPulse />
            <span className="flex-1">系统健康</span>
            <StatusDot tone={health.tone} pulse={health.tone === 'danger'} />
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
      <Sheet open={open} onOpenChange={setOpen}>
        <SheetContent className="w-full sm:max-w-md">
          <SheetHeader>
            <SheetTitle>系统健康</SheetTitle>
            <SheetDescription>
              版本 {version.data?.version ?? '—'} · 用量写入链路{health.label}
            </SheetDescription>
          </SheetHeader>
          <div className="space-y-6 overflow-y-auto px-4 pb-6">
            <section className="space-y-3">
              <h3 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">调度容量</h3>
              {summary.data && (
                <>
                  <QueueRow
                    label="全局在途"
                    enabled
                    capacity={summary.data.globalMaxConcurrentRequests || summary.data.globalInFlightRequests || 1}
                    available={Math.max(0, (summary.data.globalMaxConcurrentRequests || 0) - summary.data.globalInFlightRequests)}
                  />
                  <StatGrid>
                    <Stat label="排队请求">{fmtInt(summary.data.queuedRequests)}</Stat>
                    <Stat label="排队上限">{summary.data.maxQueuedRequests > 0 ? fmtInt(summary.data.maxQueuedRequests) : '不限'}</Stat>
                    <Stat label="运行态快照">{summary.data.runtimeFresh ? '实时' : '可能滞后'}</Stat>
                  </StatGrid>
                </>
              )}
            </section>
            {s && (
              <section className="space-y-3">
                <h3 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">用量写入链路</h3>
                <QueueRow label="内存缓冲" enabled capacity={s.inMemoryLimit} available={s.inMemoryLimit - s.inMemoryRecords} />
                <QueueRow
                  label="Redis 队列"
                  enabled={s.redisQueueEnabled}
                  capacity={s.redisQueueCapacity}
                  available={s.redisQueueAvailable}
                />
                <QueueRow
                  label="PostgreSQL 写入队列"
                  enabled={s.writerQueueEnabled}
                  capacity={s.writerQueueCapacity}
                  available={s.writerQueueAvailable}
                />
                <StatGrid>
                  <Stat label="Redis 背压">{fmtInt(s.backpressuredRedisRecords ?? 0)}</Stat>
                  <Stat label="Redis 丢弃">{fmtInt(s.droppedRedisRecords)}</Stat>
                  <Stat label="持久化背压">{fmtInt(s.backpressuredPersistRecords ?? 0)}</Stat>
                  <Stat label="持久化丢弃">{fmtInt(s.droppedPersistRecords)}</Stat>
                  <Stat label="已写入 / 已接收">
                    {fmtInt(s.writerFinished ?? 0)} / {fmtInt(s.writerAccepted ?? 0)}
                  </Stat>
                  <Stat label="清理水位拒绝">{fmtInt(s.rejectedByCleanupWatermark ?? 0)}</Stat>
                </StatGrid>
              </section>
            )}
          </div>
        </SheetContent>
      </Sheet>
    </>
  )
}
