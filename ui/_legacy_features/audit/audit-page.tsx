import * as React from 'react'
import { CheckCircle2, Eye, FileClock, XCircle } from 'lucide-react'
import { formatDate } from '@/lib/format'
import { useAuditLogsPage } from '@/hooks/use-usage'
import type { AdminAuditLogRow } from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  EmptyState,
  ErrorState,
  LoadingState,
  ModalShell,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'

const ACTION_LABELS: Record<string, string> = {
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

const OBJECT_LABELS: Record<string, string> = {
  credential: '账号',
  runtime_config: '运行配置',
  model_pricing: '模型价格',
  usage_record: '用量记录',
}

const actionLabel = (action: string) => ACTION_LABELS[action] ?? action
const objectLabel = (type: string) => OBJECT_LABELS[type] ?? type

function detailText(value: unknown): string {
  if (value === null || value === undefined) return '-'
  if (typeof value === 'string') return value
  return JSON.stringify(value, null, 2)
}

export function AuditPage() {
  const [page, setPage] = React.useState(1)
  const [selected, setSelected] = React.useState<AdminAuditLogRow | null>(null)
  const limit = 20
  const query = React.useMemo(() => ({ page, limit }), [page])
  const logs = useAuditLogsPage(query)
  const records = logs.data?.records ?? []
  const hasNext = Boolean(logs.data?.hasNext)
  const pending = logs.data?.page !== undefined && (logs.isPlaceholderData || logs.isFetching)

  return (
    <PageContainer>
      <PageHeader title={pageMeta.audit.title} subtitle={pageMeta.audit.subtitle} />

      <SectionCard
        icon={<FileClock />}
        title="审计日志"
        description="记录后台关键写操作和导出动作，便于排查配置、账号和统计数据的变化来源。"
        noPadding
      >
        {logs.isLoading ? (
          <LoadingState />
        ) : logs.error ? (
          <div className="p-5">
            <ErrorState message="审计日志加载失败" />
          </div>
        ) : records.length === 0 ? (
          <div className="p-5">
            <EmptyState icon={<FileClock />} title="暂无审计记录" />
          </div>
        ) : (
          <>
            <Table className="min-w-[860px]">
              <TableHeader>
                <TableRow>
                  <TableHead>时间</TableHead>
                  <TableHead>动作</TableHead>
                  <TableHead>对象</TableHead>
                  <TableHead>执行者</TableHead>
                  <TableHead>结果</TableHead>
                  <TableHead className="text-right">详情</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {records.map((record) => (
                  <TableRow key={record.id}>
                    <TableCell className="tabular-nums text-muted-foreground">
                      {formatDate(record.createdAt)}
                    </TableCell>
                    <TableCell>
                      <div className="font-medium text-foreground">{actionLabel(record.action)}</div>
                      <div className="text-xs text-muted-foreground">{record.action}</div>
                    </TableCell>
                    <TableCell>
                      <div>{objectLabel(record.objectType)}</div>
                      <div className="text-xs text-muted-foreground">
                        {record.objectId ? `#${record.objectId}` : '-'}
                      </div>
                    </TableCell>
                    <TableCell>{record.actor}</TableCell>
                    <TableCell>
                      <Badge tone={record.success ? 'success' : 'error'}>
                        {record.success ? (
                          <CheckCircle2 className="size-3" />
                        ) : (
                          <XCircle className="size-3" />
                        )}
                        {record.success ? '成功' : '失败'}
                      </Badge>
                    </TableCell>
                    <TableCell className="text-right">
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        onClick={() => setSelected(record)}
                        title="查看审计详情"
                      >
                        <Eye className="size-4" />
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            {(page > 1 || hasNext) && (
              <div className="flex items-center justify-center gap-3 border-t border-border px-5 py-3">
                <Button
                  variant="outline"
                  size="sm"
                  disabled={page === 1 || pending}
                  onClick={() => setPage((v) => Math.max(1, v - 1))}
                >
                  上一页
                </Button>
                <span className="text-sm text-muted-foreground">
                  第 {page} 页，每页 {limit} 条
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!hasNext || pending}
                  onClick={() => setPage((v) => v + 1)}
                >
                  下一页
                </Button>
              </div>
            )}
          </>
        )}
      </SectionCard>

      <ModalShell
        open={Boolean(selected)}
        onClose={() => setSelected(null)}
        title="审计详情"
        width="max-w-3xl"
      >
        {selected && (
          <div className="space-y-4 text-sm">
            <div className="grid gap-3 sm:grid-cols-2">
              <Detail label="时间" value={formatDate(selected.createdAt)} />
              <Detail label="执行者" value={selected.actor} />
              <Detail label="动作" value={actionLabel(selected.action)} />
              <Detail
                label="对象"
                value={`${objectLabel(selected.objectType)}${selected.objectId ? ` #${selected.objectId}` : ''}`}
              />
            </div>
            {selected.errorMessage && (
              <div>
                <div className="mb-2 font-medium text-destructive">错误信息</div>
                <pre className="scrollbar-thin max-h-44 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-muted p-3 text-xs">
                  {selected.errorMessage}
                </pre>
              </div>
            )}
            <div>
              <div className="mb-2 font-medium">完整 Detail</div>
              <pre className="scrollbar-thin max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-muted p-3 font-mono text-xs">
                {detailText(selected.detail)}
              </pre>
            </div>
          </div>
        )}
      </ModalShell>
    </PageContainer>
  )
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="break-all">{value}</div>
    </div>
  )
}
