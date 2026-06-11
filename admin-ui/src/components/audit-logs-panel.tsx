import { useMemo, useState } from 'react'
import { CheckCircle2, Eye, FileClock, RefreshCw, XCircle } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { useAuditLogsPage } from '@/hooks/use-usage'
import type { AdminAuditLogRow } from '@/types/api'

function formatDate(value?: string): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function actionLabel(action: string): string {
  const labels: Record<string, string> = {
    add_credential: '新增凭据',
    delete_credential: '删除凭据',
    set_credential_disabled: '设置启用状态',
    set_credential_priority: '设置优先级',
    set_credential_concurrency: '设置账号并发',
    reset_credential: '重置凭据',
    force_refresh_token: '强制刷新 Token',
    set_credential_warmup: '设置预热次数',
    clear_credential_in_flight: '清理并发占用',
    set_load_balancing_mode: '切换负载模式',
    update_runtime_config: '更新运行配置',
    sync_model_pricing: '同步模型价格',
    export_credentials: '导出凭据',
    clear_usage_records: '清空 Usage 展示',
  }
  return labels[action] || action
}

function objectLabel(type: string): string {
  const labels: Record<string, string> = {
    credential: '凭据',
    runtime_config: '运行配置',
    model_pricing: '模型价格',
    usage_record: 'Usage',
  }
  return labels[type] || type
}

function detailText(value: unknown): string {
  if (value === null || value === undefined) {
    return '-'
  }
  if (typeof value === 'string') {
    return value
  }
  return JSON.stringify(value, null, 2)
}

export function AuditLogsPanel() {
  const [currentPage, setCurrentPage] = useState(1)
  const [selectedLog, setSelectedLog] = useState<AdminAuditLogRow | null>(null)
  const itemsPerPage = 20
  const query = useMemo(() => ({ page: currentPage, limit: itemsPerPage }), [currentPage])
  const logs = useAuditLogsPage(query)
  const records = logs.data?.records || []
  const hasNextPage = Boolean(logs.data?.hasNext)
  const recordsPage = logs.data?.page
  const pageTransitionPending = recordsPage !== undefined && (logs.isPlaceholderData || (logs.isFetching && recordsPage !== currentPage))

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader className="flex flex-row items-start justify-between gap-4">
          <div>
            <CardTitle className="flex items-center gap-2 text-base">
              <FileClock className="h-4 w-4" />
              审计日志
            </CardTitle>
            <CardDescription>
              记录后台关键写操作和导出动作，便于排查配置、凭据和统计数据的变化来源。
            </CardDescription>
          </div>
          <Button variant="outline" size="sm" onClick={() => logs.refetch()}>
            <RefreshCw className="mr-2 h-4 w-4" />
            刷新
          </Button>
        </CardHeader>
        <CardContent>
          {logs.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : logs.error ? (
            <div className="py-8 text-center text-destructive">审计日志加载失败</div>
          ) : records.length === 0 ? (
            <div className="py-8 text-center text-muted-foreground">暂无审计记录</div>
          ) : (
            <div className="overflow-x-auto rounded-md border">
              <table className="w-full min-w-[860px] text-sm">
                <thead className="bg-muted/60">
                  <tr className="text-left">
                    <th className="px-3 py-2 font-medium">时间</th>
                    <th className="px-3 py-2 font-medium">动作</th>
                    <th className="px-3 py-2 font-medium">对象</th>
                    <th className="px-3 py-2 font-medium">执行者</th>
                    <th className="px-3 py-2 font-medium">结果</th>
                    <th className="px-3 py-2 text-right font-medium">详情</th>
                  </tr>
                </thead>
                <tbody>
                  {records.map((record) => (
                    <tr key={record.id} className="border-t">
                      <td className="whitespace-nowrap px-3 py-2">{formatDate(record.createdAt)}</td>
                      <td className="px-3 py-2">
                        <div className="font-medium">{actionLabel(record.action)}</div>
                        <div className="text-xs text-muted-foreground">{record.action}</div>
                      </td>
                      <td className="px-3 py-2">
                        <div>{objectLabel(record.objectType)}</div>
                        <div className="text-xs text-muted-foreground">
                          {record.objectId ? `#${record.objectId}` : '-'}
                        </div>
                      </td>
                      <td className="px-3 py-2">{record.actor}</td>
                      <td className="px-3 py-2">
                        {record.success ? (
                          <Badge variant="success" className="gap-1">
                            <CheckCircle2 className="h-3 w-3" />
                            成功
                          </Badge>
                        ) : (
                          <Badge variant="destructive" className="gap-1">
                            <XCircle className="h-3 w-3" />
                            失败
                          </Badge>
                        )}
                      </td>
                      <td className="px-3 py-2 text-right">
                        <Button
                          variant="ghost"
                          size="icon"
                          className="h-7 w-7"
                          onClick={() => setSelectedLog(record)}
                          title="查看审计详情"
                        >
                          <Eye className="h-4 w-4" />
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {(currentPage > 1 || hasNextPage || pageTransitionPending) && (
            <div className="mt-4 flex items-center justify-center gap-4">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setCurrentPage((page) => Math.max(1, page - 1))}
                disabled={currentPage === 1 || pageTransitionPending}
              >
                上一页
              </Button>
              <span className="text-sm text-muted-foreground">
                第 {currentPage} 页，每页 {itemsPerPage} 条
              </span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setCurrentPage((page) => page + 1)}
                disabled={!hasNextPage || pageTransitionPending}
              >
                下一页
              </Button>
            </div>
          )}
        </CardContent>
      </Card>

      <Dialog open={Boolean(selectedLog)} onOpenChange={(open) => !open && setSelectedLog(null)}>
        <DialogContent className="max-w-3xl">
          <DialogHeader>
            <DialogTitle>审计详情</DialogTitle>
          </DialogHeader>
          {selectedLog && (
            <div className="space-y-4 text-sm">
              <div className="grid gap-3 md:grid-cols-2">
                <div>
                  <div className="text-xs text-muted-foreground">时间</div>
                  <div>{formatDate(selectedLog.createdAt)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">执行者</div>
                  <div>{selectedLog.actor}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">动作</div>
                  <div>{actionLabel(selectedLog.action)}</div>
                </div>
                <div>
                  <div className="text-xs text-muted-foreground">对象</div>
                  <div>
                    {objectLabel(selectedLog.objectType)}
                    {selectedLog.objectId ? ` #${selectedLog.objectId}` : ''}
                  </div>
                </div>
              </div>
              {selectedLog.errorMessage && (
                <div>
                  <div className="mb-2 text-sm font-medium">错误信息</div>
                  <pre className="max-h-[180px] overflow-auto rounded-md border bg-muted p-3 text-xs whitespace-pre-wrap break-words">
                    {selectedLog.errorMessage}
                  </pre>
                </div>
              )}
              <div>
                <div className="mb-2 text-sm font-medium">完整 Detail</div>
                <pre className="max-h-[360px] overflow-auto rounded-md border bg-muted p-3 text-xs whitespace-pre-wrap break-words">
                  {detailText(selectedLog.detail)}
                </pre>
              </div>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </div>
  )
}
