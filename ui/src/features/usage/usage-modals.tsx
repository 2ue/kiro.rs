import * as React from 'react'
import { toast } from 'sonner'
import { formatDate, formatNumber, formatUsd } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useCancelUsageCleanup,
  usePreviewUsageCleanup,
  useRefreshUsageQueriesAfterCleanup,
  useStartUsageCleanup,
  useUsageCleanupStatus,
} from '@/hooks/use-usage'
import type { UsageCleanupMode, UsageCleanupRequest, UsageRecord } from '@/types/api'
import { ModalShell, Field, FieldGrid, useConfirm } from '@/components/patterns'
import {
  Badge,
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'
import {
  LatencyTracePanel,
  UsageDetailField as Detail,
  UsageMetric,
  attemptActionLabel,
  billingDeltaBadgeTone,
  billingDeltaTextClass,
  billingDeltaTone,
  formatAttemptChain,
  formatAttemptSummary,
  formatExternalAttemptChain,
  formatLatency,
  formatUsageSnapshot,
  routeLabel,
  statusLabel,
  upstreamModel,
} from './usage-helpers'

const USAGE_CLEANUP_DEFAULT_MAX_BATCHES = 10000

function cleanupModeLabel(mode?: UsageCleanupMode): string {
  return mode === 'hard_delete' ? '硬删除已软删记录' : '软删除可见明细'
}

function cleanupStatusLabel(status?: string): string {
  const labels: Record<string, string> = {
    idle: '空闲',
    running: '运行中',
    completed: '已完成',
    cancelled: '已取消',
    failed: '失败',
  }
  return labels[status || 'idle'] || status || '空闲'
}

function parseCleanupInteger(value: string, fallback: number, min: number): number {
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.max(min, Math.floor(parsed))
}

export function UsageCleanupModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [mode, setMode] = React.useState<UsageCleanupMode>('soft_delete')
  const [olderThanDays, setOlderThanDays] = React.useState('7')
  const [batchSize, setBatchSize] = React.useState('1000')
  const [pauseMs, setPauseMs] = React.useState('100')
  const cleanupStatus = useUsageCleanupStatus()
  const previewCleanup = usePreviewUsageCleanup()
  const startCleanup = useStartUsageCleanup()
  const cancelCleanup = useCancelUsageCleanup()
  const confirmDialog = useConfirm()
  useRefreshUsageQueriesAfterCleanup(cleanupStatus.data)

  const parsedOlderThanDays = parseCleanupInteger(olderThanDays, 7, 0)
  const parsedBatchSize = parseCleanupInteger(batchSize, 1000, 1)
  const parsedPauseMs = parseCleanupInteger(pauseMs, 100, 0)
  const cleanupRangeText = (cutoffLabel: string) =>
    parsedOlderThanDays === 0
      ? `${cutoffLabel}早于任务启动时刻（清理当时之前全部匹配记录）`
      : `${cutoffLabel}早于 ${parsedOlderThanDays} 天`
  const payload = (): UsageCleanupRequest => ({
    mode,
    olderThanDays: parsedOlderThanDays,
    batchSize: parsedBatchSize,
    pauseMsBetweenBatches: parsedPauseMs,
  })

  const running = cleanupStatus.data?.status === 'running'
  const preview = previewCleanup.data
  const estimatedBatches = preview ? Math.ceil(preview.matchedRows / Math.max(parsedBatchSize, 1)) : null

  const previewRows = () => {
    previewCleanup.mutate(payload(), {
      onError: (error) => toast.error(`预估失败: ${extractErrorMessage(error)}`),
    })
  }

  const start = async () => {
    const cutoffLabel = mode === 'hard_delete' ? '删除时间' : '创建时间'
    const confirmed = await confirmDialog({
      title: `开始${cleanupModeLabel(mode)}`,
      message: (
        <div className="space-y-2">
          <p>范围：{cleanupRangeText(cutoffLabel)}</p>
          <p>每批：{formatNumber(parsedBatchSize)} 条</p>
          <p>
            系统会持续分批执行，直到没有更多匹配记录或达到安全上限{' '}
            {formatNumber(USAGE_CLEANUP_DEFAULT_MAX_BATCHES)} 批。
          </p>
          <p>清理只影响使用记录明细列表，已累计的顶部统计和总览统计会保留。</p>
        </div>
      ),
      confirmText: '开始清理',
      tone: 'danger',
    })
    if (!confirmed) return
    startCleanup.mutate(payload(), {
      onSuccess: () => {
        toast.success('用量记录分批清理已启动')
        cleanupStatus.refetch()
      },
      onError: (error) => toast.error(`启动失败: ${extractErrorMessage(error)}`),
    })
  }

  const cancel = () => {
    cancelCleanup.mutate(undefined, {
      onSuccess: () => {
        toast.info('已请求取消清理任务')
        cleanupStatus.refetch()
      },
      onError: (error) => toast.error(`取消失败: ${extractErrorMessage(error)}`),
    })
  }

  const status = cleanupStatus.data

  return (
    <ModalShell open={open} onClose={onClose} title="分批清理用量记录" width="max-w-2xl">
      <div className="space-y-4 text-sm">
        <div className="rounded-lg border border-border bg-muted/40 p-3 text-muted-foreground">
          <span className="mr-2 inline-block h-3 w-1 rounded-full bg-warning align-[-1px]" />
          这是手动单次任务，不会定时执行。你只需要设置清理范围和每批数量，系统会自动分批清到没有更多匹配记录；后端保留{' '}
          {formatNumber(USAGE_CLEANUP_DEFAULT_MAX_BATCHES)} 批安全上限。清理只影响使用记录明细列表，已累计的顶部统计和总览统计会保留。
        </div>

        <FieldGrid min="13rem">
          <Field label="清理方式">
            <Select value={mode} onValueChange={(v) => setMode(v as UsageCleanupMode)}>
              <SelectTrigger size="sm">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="soft_delete">软删除可见明细</SelectItem>
                <SelectItem value="hard_delete">硬删除已软删记录</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field
            label={mode === 'hard_delete' ? '删除时间早于多少天' : '创建时间早于多少天'}
            description="填 0 表示以任务启动时刻为截止时间，清理当时之前全部匹配记录。"
          >
            <Input
              type="number"
              min={0}
              value={olderThanDays}
              inputMode="numeric"
              onChange={(e) => setOlderThanDays(e.target.value)}
            />
          </Field>
          <Field label="每批数量">
            <Input value={batchSize} inputMode="numeric" onChange={(e) => setBatchSize(e.target.value)} />
          </Field>
          <Field label="批次间隔毫秒">
            <Input value={pauseMs} inputMode="numeric" onChange={(e) => setPauseMs(e.target.value)} />
          </Field>
        </FieldGrid>

        {preview && (
          <div className="rounded-lg border border-border bg-muted/40 p-3">
            <div className="font-medium">
              预估：{cleanupModeLabel(preview.mode)}，匹配 {formatNumber(preview.matchedRows)} 条
            </div>
            <div className="mt-1 text-xs text-muted-foreground">
              截止时间 {formatDate(preview.cutoffAt)} · 预计 {formatNumber(estimatedBatches || 0)} 批 · 匹配记录创建时间{' '}
              {formatDate(preview.oldestCreatedAt)} 至 {formatDate(preview.newestCreatedAt)}
            </div>
          </div>
        )}

        <div className="rounded-lg border border-border bg-muted/40 p-3">
          <div className="font-medium">当前任务：{cleanupStatusLabel(status?.status)}</div>
          {status?.jobId && (
            <div className="mt-1 grid gap-1 text-xs text-muted-foreground sm:grid-cols-2">
              <span>任务 {status.jobId}</span>
              <span>模式 {cleanupModeLabel(status.mode)}</span>
              <span>已处理 {formatNumber(status.processedRows)} 条</span>
              <span>剩余约 {formatNumber(status.remainingRows || 0)} 条</span>
              <span>已执行 {formatNumber(status.batches)} 批</span>
              <span>内部安全上限 {formatNumber(status.maxBatches)} 批</span>
              <span>最后一批 {formatNumber(status.lastBatchRows)} 条</span>
              {status.stopReason && <span>停止原因 {status.stopReason}</span>}
              {status.lastError && <span className="text-destructive">错误 {status.lastError}</span>}
            </div>
          )}
        </div>

        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="outline" size="sm" onClick={previewRows} disabled={previewCleanup.isPending || running}>
            {previewCleanup.isPending ? '预估中...' : '预估'}
          </Button>
          <Button size="sm" onClick={start} disabled={startCleanup.isPending || running}>
            {startCleanup.isPending ? '启动中...' : '开始分批清理'}
          </Button>
          <Button variant="outline" size="sm" onClick={cancel} disabled={!running || cancelCleanup.isPending}>
            请求取消
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}

export function UsageBillingModal({
  record,
  onClose,
}: {
  record: UsageRecord | null
  onClose: () => void
}) {
  const billing = record?.externalPoolBilling
  const shapedCost = billing?.shapedCostUsd ?? billing?.reportedCostUsd ?? 0
  const upliftedCost = billing?.upliftedCostUsd ?? billing?.reportedCostUsd ?? billing?.billableCostUsd ?? 0
  const profit = billing ? (billing.profitUsd ?? upliftedCost - (billing.rawCostUsd || 0)) : 0
  const deltaTone = billingDeltaTone(profit)

  return (
    <ModalShell open={Boolean(record)} onClose={onClose} title="计费明细" width="max-w-3xl">
      {record && (
        <div className="space-y-4">
          <div className="grid gap-3 text-sm sm:grid-cols-2">
            <Detail label="请求 ID" value={record.id} mono />
            <Detail label="时间" value={formatDate(record.createdAt)} />
            <Detail label="请求模型" value={record.model || '-'} />
            <Detail label="实际模型" value={upstreamModel(record)} />
            <Detail label="计价模型" value={record.pricingAvailable ? record.pricingModel || 'priced' : 'unpriced'} />
            <Detail label="计费状态" value={record.pricingAvailable ? '已计价' : '未计价'} />
            <Detail label="估算费用" value={formatUsd(record.estimatedCostUsd || 0)} />
            <Detail
              label="耗时 / 首字"
              value={`${formatLatency(record.durationMs)} / ${formatLatency(record.firstTokenLatencyMs)}`}
            />
          </div>

          <div className="rounded-lg border border-border bg-muted/40 p-3 text-sm">
            <div className="mb-2 font-medium">本条返回给客户端的用量</div>
            <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-5">
              <UsageMetric label="输入" value={formatNumber(record.compatInputTokens)} />
              <UsageMetric label="缓存写入" value={formatNumber(record.cacheCreationInputTokens)} tone="info" />
              <UsageMetric label="缓存读取" value={formatNumber(record.cacheReadInputTokens)} tone="success" />
              <UsageMetric label="输出" value={formatNumber(record.outputTokens)} />
              <UsageMetric
                label="总输入"
                value={formatNumber(
                  record.compatInputTokens + record.cacheCreationInputTokens + record.cacheReadInputTokens
                )}
              />
            </div>
          </div>

          {billing && (
            <div className="rounded-lg border border-border bg-muted/40 p-3 text-sm">
              <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
                <div className="font-medium">外部账号计费拆分</div>
                <Badge tone={billingDeltaBadgeTone(deltaTone)}>
                  {deltaTone === 'loss'
                    ? `亏损 ${formatUsd(Math.abs(profit))}`
                    : deltaTone === 'profit'
                      ? `盈利 ${formatUsd(profit)}`
                      : '持平'}
                </Badge>
              </div>
              <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                <div>
                  <div className="text-xs text-muted-foreground">原始成本</div>
                  <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.rawUsage)}</div>
                  <div className="mt-1 font-medium">{formatUsd(billing.rawCostUsd || 0)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">展示计费</div>
                  <div className="break-all font-mono text-xs">
                    {formatUsageSnapshot(billing.shapedUsage || billing.reportedUsage)}
                  </div>
                  <div className="mt-1 font-medium">{formatUsd(shapedCost)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">补偿后计费</div>
                  <div className="break-all font-mono text-xs">{formatUsageSnapshot(billing.reportedUsage)}</div>
                  <div className="mt-1 font-medium">{formatUsd(upliftedCost)}</div>
                  <div className={`text-xs ${billingDeltaTextClass(deltaTone)}`}>
                    盈利 = 放大后 - 原始：{profit >= 0 ? '+' : ''}
                    {formatUsd(profit)}
                  </div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">计价模型 / 用量模式</div>
                  <div className="break-all">
                    {billing.pricingAvailable ? billing.pricingModel || 'priced' : 'unpriced'}
                  </div>
                  <div className="text-xs text-muted-foreground">
                    {billing.usageProjectionApplied ? '已按入口规则展示' : '保持原样'}
                  </div>
                </div>
              </div>
            </div>
          )}
        </div>
      )}
    </ModalShell>
  )
}

export function UsageDetailModal({
  record,
  onClose,
}: {
  record: UsageRecord | null
  onClose: () => void
}) {
  return (
    <ModalShell open={Boolean(record)} onClose={onClose} title="使用详情" width="max-w-5xl">
      {record && (
        <div className="space-y-4">
          <div className="grid gap-3 text-sm sm:grid-cols-2">
            <Detail label="请求 ID" value={record.id} mono />
            <Detail label="时间" value={formatDate(record.createdAt)} />
            <Detail label="请求模型" value={record.model || '-'} />
            <Detail label="实际模型" value={upstreamModel(record)} />
            <Detail label="解析来源" value={record.modelResolutionSource || '-'} />
            {record.modelResolutionNote && <Detail label="解析说明" value={record.modelResolutionNote} />}
            <Detail label="会话" value={record.conversationId || '-'} mono />
            <Detail label="账号" value={`#${record.credentialId ?? '-'} ${record.credentialLabel || ''}`} />
            <Detail
              label="路由"
              value={`${routeLabel(record)} · ${record.routeKind || '-'}${record.routeSubtype ? ` · ${record.routeSubtype}` : ''}`}
            />
            {record.routeKind === 'external_pool' && (
              <Detail label="外部账号" value={`#${record.externalPoolId ?? '-'} ${record.externalPoolName || ''}`} />
            )}
            {(record.fallbackReason || record.directPolicyReason) && (
              <Detail label="路由原因" value={record.fallbackReason || record.directPolicyReason || '-'} />
            )}
            <Detail label="状态" value={statusLabel(record.status)} />
            <Detail
              label="估算费用"
              value={`${formatUsd(record.estimatedCostUsd || 0)} ${record.pricingAvailable ? record.pricingModel || 'priced' : 'unpriced'}`}
            />
            <Detail label="首字 token" value={formatLatency(record.firstTokenLatencyMs)} />
          </div>

          <LatencyTracePanel record={record} />

          <div className="rounded-lg border border-border bg-muted/40 p-3 text-sm">
            <div className="mb-2 flex items-center gap-2">
              <div className="font-medium">用量口径</div>
              <span className="text-xs text-muted-foreground">
                主列表只展示返回给客户端的用量；实际输入和成本估算只在详情里查看。
              </span>
            </div>
            <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-6">
              <UsageMetric label="用户实际输入" value={formatNumber(record.totalInputTokens)} />
              <UsageMetric label="展示输入" value={formatNumber(record.compatInputTokens)} />
              <UsageMetric label="展示缓存写入" value={formatNumber(record.cacheCreationInputTokens)} tone="info" />
              <UsageMetric label="展示缓存读取" value={formatNumber(record.cacheReadInputTokens)} tone="success" />
              <UsageMetric label="展示输出" value={formatNumber(record.outputTokens)} />
              <UsageMetric label="成本估算输入" value={formatNumber(record.billableInputTokens)} />
            </div>
            <div className="mt-2 text-xs leading-5 text-muted-foreground">
              成本估算输入 = 展示输入 + 展示缓存写入，仅用于本系统费用估算和历史兼容，不是返回给客户端的独立字段。
            </div>
          </div>

          {(record.credentialAttempts || []).length > 0 && (
            <div>
              <div className="mb-2 flex flex-wrap items-center gap-2 text-sm">
                <span className="font-medium">调用链路</span>
                <Badge tone="secondary">{formatAttemptSummary(record)}</Badge>
              </div>
              <div className="mb-2 rounded-lg border border-border bg-muted px-3 py-2 font-mono text-xs">
                {formatAttemptChain(record)}
              </div>
              <Table className="min-w-[760px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>顺序</TableHead>
                    <TableHead>账号</TableHead>
                    <TableHead>状态</TableHead>
                    <TableHead>动作</TableHead>
                    <TableHead className="text-right">耗时</TableHead>
                    <TableHead>错误</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {(record.credentialAttempts || []).map((attempt) => (
                    <TableRow key={`${attempt.attempt}-${attempt.credentialId}-${attempt.durationMs}`}>
                      <TableCell>{attempt.attempt}</TableCell>
                      <TableCell>
                        <div className="font-medium">#{attempt.credentialId}</div>
                        {attempt.credentialLabel && (
                          <div className="max-w-[220px] truncate text-xs text-muted-foreground">
                            {attempt.credentialLabel}
                          </div>
                        )}
                        {attempt.model && (
                          <div className="max-w-[220px] truncate text-xs text-muted-foreground">
                            模型 {attempt.model}
                          </div>
                        )}
                      </TableCell>
                      <TableCell>{attempt.statusText || attempt.status || '-'}</TableCell>
                      <TableCell>{attemptActionLabel(attempt.action)}</TableCell>
                      <TableCell className="text-right">{formatLatency(attempt.durationMs)}</TableCell>
                      <TableCell>
                        <div className="max-w-[320px] truncate" title={attempt.errorMessage || attempt.errorType || ''}>
                          {attempt.errorMessage || attempt.errorType || '-'}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}

          {(record.externalAttempts || []).length > 0 && (
            <div>
              <div className="mb-2 text-sm font-medium">外部账号链路</div>
              <div className="mb-2 rounded-lg border border-border bg-muted px-3 py-2 font-mono text-xs">
                {formatExternalAttemptChain(record)}
              </div>
              <Table className="min-w-[760px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>顺序</TableHead>
                    <TableHead>外部账号</TableHead>
                    <TableHead>状态</TableHead>
                    <TableHead>动作</TableHead>
                    <TableHead className="text-right">耗时</TableHead>
                    <TableHead>错误</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {(record.externalAttempts || []).map((attempt) => (
                    <TableRow key={`${attempt.attempt}-${attempt.poolId}-${attempt.durationMs}`}>
                      <TableCell>{attempt.attempt}</TableCell>
                      <TableCell>
                        <div className="font-medium">#{attempt.poolId}</div>
                        <div className="max-w-[220px] truncate text-xs text-muted-foreground">
                          {attempt.poolName}
                        </div>
                      </TableCell>
                      <TableCell>{attempt.status || '-'}</TableCell>
                      <TableCell>{attemptActionLabel(attempt.action)}</TableCell>
                      <TableCell className="text-right">{formatLatency(attempt.durationMs)}</TableCell>
                      <TableCell>
                        <div className="max-w-[320px] truncate" title={attempt.errorMessage || attempt.errorType || ''}>
                          {attempt.errorMessage || attempt.errorType || '-'}
                        </div>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}

          <div>
            <div className="mb-2 text-sm font-medium">错误详情</div>
            <pre className="scrollbar-thin max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-muted p-3 text-xs">
              {record.errorDetail || record.errorMessage || '-'}
            </pre>
          </div>
        </div>
      )}
    </ModalShell>
  )
}
