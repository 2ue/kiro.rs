import { CheckCircle2, Eye, FileClock, XCircle } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Button, Table } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, LoadingState, ModalShell, SectionCard } from '@/components/common'
import { formatDate } from '@/lib/format'
import { useAuditLogsPage } from '@/hooks/use-usage'
import type { AdminAuditLogRow } from '@/types/api'

function actionLabel(action: string): string {
  const labels: Record<string, string> = {
    add_credential: '新增账号',
    delete_credential: '删除账号',
    set_credential_disabled: '设置启用状态',
    set_credential_priority: '设置优先级',
    set_credential_concurrency: '设置账号并发',
    reset_credential: '重置账号',
    force_refresh_token: '强制刷新 Token',
    set_credential_warmup: '设置预热次数',
    clear_credential_in_flight: '清理并发占用',
    set_load_balancing_mode: '切换负载模式',
    update_runtime_config: '更新运行配置',
    sync_model_pricing: '同步模型价格',
    export_credentials: '导出账号',
    clear_usage_records: '清空用量展示',
  }
  return labels[action] || action
}

function objectLabel(type: string): string {
  const labels: Record<string, string> = {
    credential: '账号',
    runtime_config: '运行配置',
    model_pricing: '模型价格',
    usage_record: '用量记录',
  }
  return labels[type] || type
}

function detailText(value: unknown): string {
  if (value === null || value === undefined) return '-'
  if (typeof value === 'string') return value
  return JSON.stringify(value, null, 2)
}

export function AuditPanel() {
  const [page, setPage] = useState(1)
  const [selectedLog, setSelectedLog] = useState<AdminAuditLogRow | null>(null)
  const limit = 20
  const query = useMemo(() => ({ page, limit }), [page])
  const logs = useAuditLogsPage(query)
  const records = logs.data?.records || []
  const hasNext = Boolean(logs.data?.hasNext)
  const recordsPage = logs.data?.page
  const pageTransitionPending = recordsPage !== undefined && (logs.isPlaceholderData || (logs.isFetching && recordsPage !== page))

  return (
    <div className="space-y-4">
      <SectionCard
        title={<span className="flex items-center gap-2"><FileClock className="h-4 w-4" /> 审计日志</span>}
        description="记录后台关键写操作和导出动作，便于排查配置、账号和统计数据的变化来源。"
      >
        {logs.isLoading ? (
          <LoadingState />
        ) : logs.error ? (
          <ErrorState text="审计日志加载失败" />
        ) : records.length === 0 ? (
          <EmptyState text="暂无审计记录" />
        ) : (
          <div className="table-panel">
            <Table zebra size="sm" className="data-table min-w-[860px]">
              <Table.Head>
                <span>时间</span>
                <span>动作</span>
                <span>对象</span>
                <span>执行者</span>
                <span>结果</span>
                <span className="text-right">详情</span>
              </Table.Head>
              <Table.Body>
                {records.map((record) => (
                  <Table.Row key={record.id} hover>
                    <span>{formatDate(record.createdAt)}</span>
                    <span>
                      <div className="font-medium">{actionLabel(record.action)}</div>
                      <div className="text-xs text-base-content/50">{record.action}</div>
                    </span>
                    <span>
                      <div>{objectLabel(record.objectType)}</div>
                      <div className="text-xs text-base-content/50">{record.objectId ? `#${record.objectId}` : '-'}</div>
                    </span>
                    <span>{record.actor}</span>
                    <span>
                      <Badge tone={record.success ? 'success' : 'error'}>
                        {record.success ? <CheckCircle2 className="h-3 w-3" /> : <XCircle className="h-3 w-3" />}
                        {record.success ? '成功' : '失败'}
                      </Badge>
                    </span>
                    <span className="text-right">
                      <Button type="button" color="ghost" size="xs" shape="square" onClick={() => setSelectedLog(record)} title="查看审计详情">
                        <Eye className="h-4 w-4" />
                      </Button>
                    </span>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          </div>
        )}
        {(page > 1 || hasNext || pageTransitionPending) && (
          <div className="mt-4 flex items-center justify-center gap-3">
            <Button type="button" variant="outline" size="sm" disabled={page === 1 || pageTransitionPending} onClick={() => setPage((value) => Math.max(1, value - 1))}>
              上一页
            </Button>
            <span className="text-sm text-base-content/60">第 {page} 页，每页 {limit} 条</span>
            <Button type="button" variant="outline" size="sm" disabled={!hasNext || pageTransitionPending} onClick={() => setPage((value) => value + 1)}>
              下一页
            </Button>
          </div>
        )}
      </SectionCard>
      <ModalShell open={Boolean(selectedLog)} title="审计详情" width="max-w-4xl" onClose={() => setSelectedLog(null)}>
        {selectedLog && (
          <div className="space-y-4 text-sm">
            <div className="grid gap-3 md:grid-cols-2">
              <Detail label="时间" value={formatDate(selectedLog.createdAt)} />
              <Detail label="执行者" value={selectedLog.actor} />
              <Detail label="动作" value={actionLabel(selectedLog.action)} />
              <Detail label="对象" value={`${objectLabel(selectedLog.objectType)}${selectedLog.objectId ? ` #${selectedLog.objectId}` : ''}`} />
            </div>
            {selectedLog.errorMessage && (
              <div>
                <div className="mb-2 font-medium">错误信息</div>
                <pre className="max-h-44 overflow-auto rounded-box border border-base-300 bg-base-200 p-3 text-xs whitespace-pre-wrap break-words">{selectedLog.errorMessage}</pre>
              </div>
            )}
            <div>
              <div className="mb-2 font-medium">完整 Detail</div>
              <pre className="max-h-96 overflow-auto rounded-box border border-base-300 bg-base-200 p-3 text-xs whitespace-pre-wrap break-words">{detailText(selectedLog.detail)}</pre>
            </div>
          </div>
        )}
      </ModalShell>
    </div>
  )
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-xs text-base-content/50">{label}</div>
      <div className="break-all">{value}</div>
    </div>
  )
}
