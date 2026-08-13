import { useMemo, useState } from 'react'
import { CheckCircle2, Eye, FileClock, Filter, RefreshCw, X, XCircle } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
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

const ACTION_LABELS: Record<string, string> = {
  add_credential: '新增账号',
  delete_credential: '删除账号',
  set_credential_disabled: '设置启用状态',
  set_credential_priority: '设置优先级',
  set_credential_concurrency: '设置账号并发',
  set_credential_rpm: '设置账号限速',
  set_credential_rate_limit_auto_disable: '设置429自动禁用',
  set_credential_regions: '设置账号区域',
  set_credential_proxy: '设置账号代理',
  reset_credential: '重置账号',
  update_credential_auth: '更新账号认证',
  force_refresh_token: '强制刷新 Token',
  set_credential_warmup: '设置预热次数',
  clear_credential_in_flight: '清理并发占用',
  auto_disable_credential: '自动禁用账号',
  auto_enable_credential: '自动恢复账号',
  batch_import_credentials: '批量导入账号',
  batch_update_credentials: '批量修改账号',
  delete_disabled_credentials: '删除已禁用账号',
  set_load_balancing_mode: '切换负载模式',
  update_runtime_config: '更新运行配置',
  create_request_api_key: '新增请求 Key',
  update_request_api_key: '修改请求 Key',
  delete_request_api_key: '删除请求 Key',
  update_admin_api_key: '修改登录 Key',
  sync_model_pricing: '同步模型价格',
  sync_model_capabilities: '同步模型能力',
  export_credentials: '导出账号',
  clear_usage_records: '清空用量展示',
  start_usage_cleanup: '启动清理任务',
  cancel_usage_cleanup: '取消清理任务',
  upsert_manual_model: '手动添加/更新模型',
  delete_manual_model: '删除手动模型',
  create_proxy_resource: '新增代理资源',
  update_proxy_resource: '更新代理资源',
  delete_proxy_resource: '删除代理资源',
  create_external_pool: '新增外部账号池',
  update_external_pool: '更新外部账号池',
  delete_external_pool: '删除外部账号池',
  set_external_pool_enabled: '设置账号池启用状态',
  clear_external_pool_auto_disabled: '清除账号池自动禁用',
}

const OBJECT_LABELS: Record<string, string> = {
  credential: '账号',
  runtime_config: '运行配置',
  model_pricing: '模型价格',
  model_capability: '模型能力',
  model_capabilities: '模型能力',
  usage_record: '用量记录',
  manual_model: '手动模型',
  request_api_key: '请求 Key',
  admin_api_key: '登录 Key',
  security_keys: '密钥配置',
  proxy_resource: '代理资源',
  external_pool: '外部账号池',
  load_balancing: '负载均衡',
}

const ACTION_CATEGORIES = [
  { value: '__all__', label: '全部动作' },
  { value: 'credential', label: '账号管理' },
  { value: 'config', label: '配置变更' },
  { value: 'security', label: '密钥管理' },
  { value: 'pricing', label: '模型价格' },
  { value: 'usage', label: '用量操作' },
  { value: 'proxy', label: '代理资源' },
  { value: 'external_pool', label: '外部账号池' },
]

const ACTION_TO_CATEGORY: Record<string, string> = {
  add_credential: 'credential',
  delete_credential: 'credential',
  set_credential_disabled: 'credential',
  set_credential_priority: 'credential',
  set_credential_concurrency: 'credential',
  set_credential_rpm: 'credential',
  set_credential_rate_limit_auto_disable: 'credential',
  set_credential_regions: 'credential',
  set_credential_proxy: 'credential',
  reset_credential: 'credential',
  update_credential_auth: 'credential',
  force_refresh_token: 'credential',
  set_credential_warmup: 'credential',
  clear_credential_in_flight: 'credential',
  auto_disable_credential: 'credential',
  auto_enable_credential: 'credential',
  batch_import_credentials: 'credential',
  batch_update_credentials: 'credential',
  delete_disabled_credentials: 'credential',
  export_credentials: 'credential',
  set_load_balancing_mode: 'config',
  update_runtime_config: 'config',
  create_request_api_key: 'security',
  update_request_api_key: 'security',
  delete_request_api_key: 'security',
  update_admin_api_key: 'security',
  sync_model_pricing: 'pricing',
  sync_model_capabilities: 'pricing',
  upsert_manual_model: 'pricing',
  delete_manual_model: 'pricing',
  clear_usage_records: 'usage',
  start_usage_cleanup: 'usage',
  cancel_usage_cleanup: 'usage',
  create_proxy_resource: 'proxy',
  update_proxy_resource: 'proxy',
  delete_proxy_resource: 'proxy',
  create_external_pool: 'external_pool',
  update_external_pool: 'external_pool',
  delete_external_pool: 'external_pool',
  set_external_pool_enabled: 'external_pool',
  clear_external_pool_auto_disabled: 'external_pool',
}

function actionLabel(action: string): string {
  return ACTION_LABELS[action] || action
}

function objectLabel(type: string): string {
  return OBJECT_LABELS[type] || type
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
  const [search, setSearch] = useState('')
  const [successFilter, setSuccessFilter] = useState<'__all__' | 'success' | 'failed'>('__all__')
  const [categoryFilter, setCategoryFilter] = useState('__all__')
  const [showFilters, setShowFilters] = useState(false)
  const itemsPerPage = 20
  const query = useMemo(() => ({ page: currentPage, limit: itemsPerPage }), [currentPage])
  const logs = useAuditLogsPage(query)
  const allRecords = logs.data?.records || []
  const hasNextPage = Boolean(logs.data?.hasNext)
  const recordsPage = logs.data?.page
  const pageTransitionPending = recordsPage !== undefined && (logs.isPlaceholderData || (logs.isFetching && recordsPage !== currentPage))
  const records = useMemo(() => {
    let filtered = allRecords
    const keyword = search.trim().toLowerCase()
    if (keyword) {
      filtered = filtered.filter((record) => {
        const haystack = [
          record.action,
          actionLabel(record.action),
          record.actor,
          record.objectType,
          objectLabel(record.objectType),
          record.objectId ? String(record.objectId) : '',
          record.errorMessage || '',
        ].join(' ').toLowerCase()
        return haystack.includes(keyword)
      })
    }
    if (successFilter !== '__all__') {
      const expected = successFilter === 'success'
      filtered = filtered.filter((record) => record.success === expected)
    }
    if (categoryFilter !== '__all__') {
      filtered = filtered.filter((record) => ACTION_TO_CATEGORY[record.action] === categoryFilter)
    }
    return filtered
  }, [allRecords, categoryFilter, search, successFilter])
  const hasFilters = Boolean(search.trim()) || successFilter !== '__all__' || categoryFilter !== '__all__'
  const filterCount = [Boolean(search.trim()), successFilter !== '__all__', categoryFilter !== '__all__'].filter(Boolean).length
  const clearFilters = () => {
    setSearch('')
    setSuccessFilter('__all__')
    setCategoryFilter('__all__')
    setCurrentPage(1)
  }

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
              记录后台关键写操作和导出动作，便于排查配置、账号和统计数据的变化来源。
            </CardDescription>
          </div>
          <Button variant="outline" size="sm" onClick={() => logs.refetch()}>
            <RefreshCw className="mr-2 h-4 w-4" />
            刷新
          </Button>
        </CardHeader>
        <CardContent>
          <div className="mb-4 space-y-3">
            <div className="flex flex-col gap-2 md:flex-row md:items-center md:justify-between">
              <Input
                className="md:max-w-sm"
                value={search}
                onChange={(event) => {
                  setSearch(event.target.value)
                  setCurrentPage(1)
                }}
                placeholder="搜索动作、执行者、对象..."
              />
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className={hasFilters ? 'border-primary text-primary' : ''}
                  onClick={() => setShowFilters((value) => !value)}
                >
                  <Filter className="mr-2 h-4 w-4" />
                  筛选
                  {filterCount > 0 && <Badge className="ml-2">{filterCount}</Badge>}
                </Button>
                {hasFilters && (
                  <Button type="button" variant="ghost" size="sm" onClick={clearFilters}>
                    <X className="mr-2 h-4 w-4" />
                    清除筛选
                  </Button>
                )}
              </div>
            </div>
            {showFilters && (
              <div className="grid gap-2 rounded-md border bg-muted/30 p-3 md:grid-cols-2">
                <select
                  className="h-10 rounded-md border border-input bg-background px-3 text-sm"
                  value={categoryFilter}
                  onChange={(event) => {
                    setCategoryFilter(event.target.value)
                    setCurrentPage(1)
                  }}
                >
                  {ACTION_CATEGORIES.map((category) => (
                    <option key={category.value} value={category.value}>
                      {category.label}
                    </option>
                  ))}
                </select>
                <select
                  className="h-10 rounded-md border border-input bg-background px-3 text-sm"
                  value={successFilter}
                  onChange={(event) => {
                    setSuccessFilter(event.target.value as typeof successFilter)
                    setCurrentPage(1)
                  }}
                >
                  <option value="__all__">全部结果</option>
                  <option value="success">成功</option>
                  <option value="failed">失败</option>
                </select>
              </div>
            )}
          </div>
          {logs.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : logs.error ? (
            <div className="py-8 text-center text-destructive">审计日志加载失败</div>
          ) : records.length === 0 ? (
            <div className="py-8 text-center text-muted-foreground">
              {hasFilters ? '没有匹配当前筛选条件的审计记录' : '暂无审计记录'}
            </div>
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
