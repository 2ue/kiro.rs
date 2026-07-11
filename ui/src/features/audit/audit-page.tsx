import { useMemo, useState } from 'react'
import { CheckCircle2, Eye, FileClock, Filter, RefreshCw, X, XCircle } from 'lucide-react'
import { formatDate } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import { useAuditLogsPage } from '@/hooks/use-usage'
import type { AdminAuditLogRow } from '@/types/api'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  EmptyState,
  ErrorState,
  LoadingState,
  ModalShell,
  Toolbar,
  ToolbarSearch,
  ToolbarActions,
} from '@/components/patterns'
import {
  Badge,
  Button,
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

// ─── 标签映射 ─────────────────────────────────────────────────────────────────

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
  { value: 'pricing', label: '价格同步' },
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

function actionLabel(action: string): string { return ACTION_LABELS[action] ?? action }
function objectLabel(type: string): string { return OBJECT_LABELS[type] ?? type }
function detailText(value: unknown): string {
  if (value === null || value === undefined) return '-'
  if (typeof value === 'string') return value
  return JSON.stringify(value, null, 2)
}

// ─── 详情弹窗 ─────────────────────────────────────────────────────────────────

function AuditDetailModal({ record, open, onClose }: { record: AdminAuditLogRow | null; open: boolean; onClose: () => void }) {
  if (!record) return null
  return (
    <ModalShell open={open} onClose={onClose} title="审计详情" width="max-w-2xl">
      <div className="space-y-4 text-sm">
        <div className="grid gap-3 sm:grid-cols-2">
          {[
            { label: '时间', value: formatDate(record.createdAt) },
            { label: '执行者', value: record.actor },
            { label: '动作', value: actionLabel(record.action) },
            { label: '对象', value: `${objectLabel(record.objectType)}${record.objectId ? ` #${record.objectId}` : ''}` },
          ].map(({ label, value }) => (
            <div key={label}>
              <div className="text-xs text-muted-foreground">{label}</div>
              <div className="break-all">{value}</div>
            </div>
          ))}
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs text-muted-foreground">结果</span>
          <Badge tone={record.success ? 'success' : 'error'}>
            {record.success ? <CheckCircle2 className="size-3" /> : <XCircle className="size-3" />}
            {record.success ? '成功' : '失败'}
          </Badge>
        </div>
        {record.errorMessage && (
          <div>
            <div className="mb-1 text-xs font-semibold text-destructive">错误信息</div>
            <pre className="scrollbar-thin max-h-32 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-muted p-3 text-xs">
              {record.errorMessage}
            </pre>
          </div>
        )}
        <div>
          <div className="mb-1 text-xs font-semibold text-muted-foreground">详情</div>
          <pre className="scrollbar-thin max-h-80 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-muted p-3 font-mono text-xs">
            {detailText(record.detail)}
          </pre>
        </div>
      </div>
    </ModalShell>
  )
}

// ─── 主页 ──────────────────────────────────────────────────────────────────────

export function AuditPage() {
  const [page, setPage] = useState(1)
  const [selected, setSelected] = useState<AdminAuditLogRow | null>(null)
  const [q, setQ] = useState('')
  const [successFilter, setSuccessFilter] = useState<'__all__' | 'success' | 'failed'>('__all__')
  const [categoryFilter, setCategoryFilter] = useState('__all__')
  const [showFilters, setShowFilters] = useState(false)

  const limit = 20
  const query = useMemo(() => ({ page, limit }), [page])
  const logs = useAuditLogsPage(query)
  const allRecords = logs.data?.records ?? []
  const hasNext = Boolean(logs.data?.hasNext)
  const pending = logs.isPlaceholderData || logs.isFetching

  // Client-side filtering (server doesn't support filters yet)
  const records = useMemo(() => {
    let filtered = allRecords
    if (q.trim()) {
      const lower = q.toLowerCase()
      filtered = filtered.filter(
        (r) =>
          r.action.includes(lower) ||
          actionLabel(r.action).toLowerCase().includes(lower) ||
          r.actor.toLowerCase().includes(lower) ||
          r.objectType.includes(lower) ||
          objectLabel(r.objectType).toLowerCase().includes(lower) ||
          (r.objectId && String(r.objectId).includes(lower)) ||
          (r.errorMessage && r.errorMessage.toLowerCase().includes(lower))
      )
    }
    if (successFilter !== '__all__') {
      const wantSuccess = successFilter === 'success'
      filtered = filtered.filter((r) => r.success === wantSuccess)
    }
    if (categoryFilter !== '__all__') {
      filtered = filtered.filter((r) => ACTION_TO_CATEGORY[r.action] === categoryFilter)
    }
    return filtered
  }, [allRecords, q, successFilter, categoryFilter])

  const hasFilters = successFilter !== '__all__' || categoryFilter !== '__all__' || !!q.trim()
  const filterCount = [successFilter !== '__all__', categoryFilter !== '__all__', !!q.trim()].filter(Boolean).length

  const clearFilters = () => { setQ(''); setSuccessFilter('__all__'); setCategoryFilter('__all__') }

  return (
    <PageContainer>
      <PageHeader title="审计" subtitle="后台关键操作记录，便于排查配置与数据变化来源" />

      <SectionCard
        icon={<FileClock />}
        title="操作日志"
        description="记录账号、配置、价格、用量的关键写操作"
        noPadding
      >
        <div className="px-4 pt-4 pb-2">
          <Toolbar>
            <ToolbarSearch value={q} onChange={(v) => { setQ(v); setPage(1) }} placeholder="搜索动作、执行者、对象..." />
            <ToolbarActions>
              <Button variant="outline" size="sm" onClick={() => logs.refetch()} disabled={pending}>
                <RefreshCw className={`h-3.5 w-3.5 ${pending ? 'animate-spin' : ''}`} />
                刷新
              </Button>
              <Button
                variant="outline"
                size="sm"
                className={hasFilters ? 'border-primary text-primary' : ''}
                onClick={() => setShowFilters((v) => !v)}
              >
                <Filter className="h-3.5 w-3.5" />
                筛选
                {filterCount > 0 && <Badge tone="primary">{filterCount}</Badge>}
              </Button>
              {pending && <RefreshCw className="size-3.5 animate-spin text-muted-foreground/60" />}
            </ToolbarActions>
          </Toolbar>

          {showFilters && (
            <div className="mt-2 rounded-lg bg-muted/30 p-3">
              <div className="grid gap-2 sm:grid-cols-3">
                <Select value={categoryFilter} onValueChange={(v) => { setCategoryFilter(v); setPage(1) }}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {ACTION_CATEGORIES.map((c) => (
                      <SelectItem key={c.value} value={c.value}>{c.label}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Select value={successFilter} onValueChange={(v) => { setSuccessFilter(v as typeof successFilter); setPage(1) }}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__all__">全部结果</SelectItem>
                    <SelectItem value="success">成功</SelectItem>
                    <SelectItem value="failed">失败</SelectItem>
                  </SelectContent>
                </Select>
                {hasFilters && (
                  <Button variant="ghost" size="sm" onClick={clearFilters}>
                    <X className="h-3.5 w-3.5" />清除筛选
                  </Button>
                )}
              </div>
            </div>
          )}
        </div>

        {logs.isLoading ? (
          <LoadingState text="加载日志..." className="py-8" />
        ) : logs.error ? (
          <div className="px-4 pb-4">
            <ErrorState title="审计日志加载失败" message={extractErrorMessage(logs.error)} />
          </div>
        ) : records.length === 0 ? (
          <div className="px-4 pb-4">
            <EmptyState
              icon={<FileClock />}
              title="暂无审计记录"
              description={hasFilters ? '没有匹配当前筛选条件的记录' : '后台关键操作记录会在这里展示'}
              action={hasFilters ? <Button variant="outline" size="sm" onClick={clearFilters}>清除筛选</Button> : undefined}
            />
          </div>
        ) : (
          <>
            <div className="scrollbar-thin overflow-x-auto">
              <Table className="min-w-[720px]">
                <TableHeader>
                  <TableRow>
                    <TableHead>时间</TableHead>
                    <TableHead>动作</TableHead>
                    <TableHead>对象</TableHead>
                    <TableHead>执行者</TableHead>
                    <TableHead>结果</TableHead>
                    <TableHead className="text-right w-16">详情</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {records.map((record) => (
                    <TableRow key={record.id} className="cursor-pointer hover:bg-muted/40" onClick={() => setSelected(record)}>
                      <TableCell className="tabular-nums text-muted-foreground text-xs whitespace-nowrap">
                        {formatDate(record.createdAt)}
                      </TableCell>
                      <TableCell>
                        <div className="text-xs font-medium text-foreground">{actionLabel(record.action)}</div>
                        <div className="text-[0.62rem] text-muted-foreground/60 font-mono">{record.action}</div>
                      </TableCell>
                      <TableCell>
                        <div className="text-xs">{objectLabel(record.objectType)}</div>
                        <div className="text-[0.62rem] text-muted-foreground/60">
                          {record.objectId ? `#${record.objectId}` : '—'}
                        </div>
                      </TableCell>
                      <TableCell className="text-xs">{record.actor}</TableCell>
                      <TableCell>
                        <Badge tone={record.success ? 'success' : 'error'}>
                          {record.success
                            ? <CheckCircle2 className="size-3" />
                            : <XCircle className="size-3" />
                          }
                          {record.success ? '成功' : '失败'}
                        </Badge>
                      </TableCell>
                      <TableCell className="text-right">
                        <Button
                          variant="ghost"
                          size="icon-xs"
                          onClick={(e) => { e.stopPropagation(); setSelected(record) }}
                          title="查看详情"
                        >
                          <Eye className="size-3.5" />
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>

            <div className="px-4 py-3">
              <div className="flex items-center justify-center gap-3">
                <Button variant="outline" size="sm" disabled={page === 1 || pending} onClick={() => setPage((v) => Math.max(1, v - 1))}>
                  上一页
                </Button>
                <span className="text-xs text-muted-foreground">第 {page} 页，每页 {limit} 条</span>
                <Button variant="outline" size="sm" disabled={!hasNext || pending} onClick={() => setPage((v) => v + 1)}>
                  下一页
                </Button>
              </div>
            </div>
          </>
        )}
      </SectionCard>

      <AuditDetailModal record={selected} open={Boolean(selected)} onClose={() => setSelected(null)} />
    </PageContainer>
  )
}
