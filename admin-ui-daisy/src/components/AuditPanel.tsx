import { CheckCircle2, Eye, FileClock, Filter, RefreshCw, X, XCircle } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Button, Input, Table } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, LoadingState, ModalShell, SectionCard, Select } from '@/components/common'
import { formatDate } from '@/lib/format'
import { useAuditLogsPage } from '@/hooks/use-usage'
import type { AdminAuditLogRow } from '@/types/api'

const ACTION_LABELS: Record<string, string> = {
  add_credential: '新增账号',
  delete_credential: '删除账号',
  set_credential_disabled: '设置启用状态',
  set_credential_priority: '设置优先级',
  set_credential_concurrency: '设置账号并发',
  set_credential_rpm: '设置账号限速',
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
  if (value === null || value === undefined) return '-'
  if (typeof value === 'string') return value
  return JSON.stringify(value, null, 2)
}

export function AuditPanel() {
  const [page, setPage] = useState(1)
  const [selectedLog, setSelectedLog] = useState<AdminAuditLogRow | null>(null)
  const [search, setSearch] = useState('')
  const [successFilter, setSuccessFilter] = useState<'__all__' | 'success' | 'failed'>('__all__')
  const [categoryFilter, setCategoryFilter] = useState('__all__')
  const [showFilters, setShowFilters] = useState(false)
  const limit = 20
  const query = useMemo(() => ({ page, limit }), [page])
  const logs = useAuditLogsPage(query)
  const allRecords = logs.data?.records || []
  const hasNext = Boolean(logs.data?.hasNext)
  const recordsPage = logs.data?.page
  const pageTransitionPending = recordsPage !== undefined && (logs.isPlaceholderData || (logs.isFetching && recordsPage !== page))
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
  const pending = logs.isPlaceholderData || logs.isFetching
  const hasFilters = Boolean(search.trim()) || successFilter !== '__all__' || categoryFilter !== '__all__'
  const filterCount = [Boolean(search.trim()), successFilter !== '__all__', categoryFilter !== '__all__'].filter(Boolean).length
  const clearFilters = () => {
    setSearch('')
    setSuccessFilter('__all__')
    setCategoryFilter('__all__')
    setPage(1)
  }

  return (
    <div className="space-y-4">
      <SectionCard
        title={<span className="flex items-center gap-2"><FileClock className="h-4 w-4" /> 审计日志</span>}
        description="记录后台关键写操作和导出动作，便于排查配置、账号和统计数据的变化来源。"
      >
        <div className="mb-4 space-y-3">
          <div className="flex flex-col gap-2 lg:flex-row lg:items-center lg:justify-between">
            <Input
              size="sm"
              bordered
              className="w-full lg:max-w-sm"
              value={search}
              onChange={(event) => {
                setSearch(event.target.value)
                setPage(1)
              }}
              placeholder="搜索动作、执行者、对象..."
            />
            <div className="flex flex-wrap items-center gap-2">
              {pending && <RefreshCw className="h-4 w-4 animate-spin text-base-content/45" />}
              <Button type="button" variant="outline" size="sm" onClick={() => logs.refetch()}>
                <RefreshCw className="h-4 w-4" />
                刷新
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className={hasFilters ? 'border-primary text-primary' : ''}
                onClick={() => setShowFilters((value) => !value)}
              >
                <Filter className="h-4 w-4" />
                筛选
                {filterCount > 0 && <Badge tone="primary" size="xs">{filterCount}</Badge>}
              </Button>
              {hasFilters && (
                <Button type="button" color="ghost" size="sm" onClick={clearFilters}>
                  <X className="h-4 w-4" />
                  清除筛选
                </Button>
              )}
            </div>
          </div>
          {showFilters && (
            <div className="grid gap-2 rounded-box border border-base-300 bg-base-200/50 p-3 md:grid-cols-2">
              <Select bordered size="sm" value={categoryFilter} onChange={(event) => { setCategoryFilter(event.target.value); setPage(1) }}>
                {ACTION_CATEGORIES.map((category) => (
                  <Select.Option key={category.value} value={category.value}>{category.label}</Select.Option>
                ))}
              </Select>
              <Select bordered size="sm" value={successFilter} onChange={(event) => { setSuccessFilter(event.target.value as typeof successFilter); setPage(1) }}>
                <Select.Option value="__all__">全部结果</Select.Option>
                <Select.Option value="success">成功</Select.Option>
                <Select.Option value="failed">失败</Select.Option>
              </Select>
            </div>
          )}
        </div>
        {logs.isLoading ? (
          <LoadingState />
        ) : logs.error ? (
          <ErrorState text="审计日志加载失败" />
        ) : records.length === 0 ? (
          <EmptyState text={hasFilters ? '没有匹配当前筛选条件的审计记录' : '暂无审计记录'} />
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
