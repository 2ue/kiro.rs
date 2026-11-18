import {
Callout,
EmptyState,
ErrorState,
LoadingState,
ModalShell,
PageContainer,
PageHeader,
PageSizeSelect,
SectionCard,
StatCard,
StatGrid,
Toolbar,
ToolbarActions,
ToolbarSearch,
useConfirm,
} from '@/components/patterns'
import {
Badge,
Button,
Input,
Select,
SelectContent,
SelectItem,
SelectTrigger,
SelectValue,
Switch,
Table,
TableBody,
TableCell,
TableHead,
TableHeader,
TableRow,
Tabs,
TabsList,
TabsTrigger,
} from '@/components/ui'
import { useRuntimeConfig,useUpdateRuntimeConfig } from '@/hooks/use-runtime-config'
import { useAccountEventSummary,useAuditLogsPage } from '@/hooks/use-usage'
import { formatDate } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type { AdminAuditLogPageQuery,AdminAuditLogRow } from '@/types/api'
import { Activity,CheckCircle2,Eye,FileClock,Filter,RefreshCw,Settings2,X,XCircle } from 'lucide-react'
import { useMemo,useState } from 'react'
import { toast } from 'sonner'
import {
ACCOUNT_EVENT_ORDER,
TIME_RANGES,
accountEventMeta,
accountEventQuery,
accountEventView,
summaryTiles,
type AccountFilterState,
} from './account-events'
import { AuditChangeSummary,AuditDetailSummary } from './audit-detail'

// ─── 标签映射 ─────────────────────────────────────────────────────────────────

const ACTION_LABELS: Record<string, string> = {
  account_account_event: '账号异动',
  create_account: '新增账号',
  copy_account: '复制账号',
  update_account: '更新账号',
  delete_account: '删除账号',
  set_account_enabled: '设置账号启用状态',
  clear_account_auto_disabled: '清除账号自动禁用',
  clear_account_cooldown: '清除账号冷却',
  set_account_supported_models: '设置账号支持的模型',
  sync_account_supported_models: '同步账号支持的模型',
  update_runtime_config: '更新运行配置',
  update_routes: '更新路由',
  create_route_scenario: '新增路由场景',
  update_route_scenario: '更新路由场景',
  delete_route_scenario: '删除路由场景',
  apply_route_scenario: '应用路由场景',
  import_config: '导入配置',
  create_request_api_key: '新增请求 Key',
  update_request_api_key: '修改请求 Key',
  delete_request_api_key: '删除请求 Key',
  update_admin_api_key: '修改登录 Key',
  sync_model_pricing: '同步模型价格',
  upsert_manual_model: '手动添加/更新模型',
  delete_manual_model: '删除手动模型',
  set_model_listed: '设置模型是否列出',
  clear_usage_records: '清空用量展示',
  start_usage_cleanup: '启动清理任务',
  cancel_usage_cleanup: '取消清理任务',
  resume_usage_cleanup: '恢复清理任务',
  finish_usage_cleanup: '清理任务结束',
}

const OBJECT_LABELS: Record<string, string> = {
  runtime_config: '运行配置',
  routes: '路由',
  route_scenario: '路由场景',
  model_pricing: '模型价格',
  model_capability: '模型能力',
  model_capabilities: '模型能力',
  model_id: '模型',
  usage_record: '用量记录',
  manual_model: '手动模型',
  request_api_key: '请求 Key',
  admin_api_key: '登录 Key',
  security_keys: '密钥配置',
  account: '账号',
  config: '配置包',
}

const ACTION_CATEGORIES = [
  { value: '__all__', label: '全部动作' },
  { value: 'config', label: '配置变更' },
  { value: 'security', label: '密钥管理' },
  { value: 'pricing', label: '模型目录' },
  { value: 'usage', label: '用量操作' },
  { value: 'account', label: '账号' },
]

const ACTION_TO_CATEGORY: Record<string, string> = {
  update_runtime_config: 'config',
  update_routes: 'config',
  create_route_scenario: 'config',
  update_route_scenario: 'config',
  delete_route_scenario: 'config',
  apply_route_scenario: 'config',
  import_config: 'config',
  create_request_api_key: 'security',
  update_request_api_key: 'security',
  delete_request_api_key: 'security',
  update_admin_api_key: 'security',
  sync_model_pricing: 'pricing',
  upsert_manual_model: 'pricing',
  delete_manual_model: 'pricing',
  set_model_listed: 'pricing',
  clear_usage_records: 'usage',
  start_usage_cleanup: 'usage',
  cancel_usage_cleanup: 'usage',
  resume_usage_cleanup: 'usage',
  finish_usage_cleanup: 'usage',
  create_account: 'account',
  copy_account: 'account',
  update_account: 'account',
  delete_account: 'account',
  set_account_enabled: 'account',
  clear_account_auto_disabled: 'account',
  clear_account_cooldown: 'account',
  set_account_supported_models: 'account',
  sync_account_supported_models: 'account',
  account_account_event: 'account',
}

function actionLabel(action: string): string { return ACTION_LABELS[action] ?? action }
function objectLabel(type: string): string { return OBJECT_LABELS[type] ?? type }
const DEFAULT_PAGE_LIMIT = 20

// ─── 详情弹窗 ─────────────────────────────────────────────────────────────────

function AuditDetailModal({ record, open, onClose }: { record: AdminAuditLogRow | null; open: boolean; onClose: () => void }) {
  if (!record) return null
  const event = accountEventView(record)
  return (
    <ModalShell open={open} onClose={onClose} title={event ? '账号异动详情' : '审计详情'} width="max-w-2xl">
      <div className="space-y-4 text-sm">
        <div className="grid gap-3 sm:grid-cols-2">
          {[
            { label: '时间', value: formatDate(record.createdAt) },
            { label: '执行者', value: event?.actor ?? record.actor },
            { label: event ? '变动' : '动作', value: event ? event.label : actionLabel(record.action) },
            {
              label: event ? '账号' : '对象',
              value: event
                ? `账号 #${event.accountId} · ${event.accountLabel}`
                : `${objectLabel(record.objectType)}${record.objectId ? ` #${record.objectId}` : ''}`,
            },
            ...(event?.reason ? [{ label: '原因', value: event.reason }] : []),
          ].map(({ label, value }) => (
            <div key={label}>
              <div className="text-xs text-muted-foreground">{label}</div>
              <div className="break-all">{value}</div>
            </div>
          ))}
        </div>
        {!event && (
          <div className="flex items-center gap-2">
            <span className="text-xs text-muted-foreground">结果</span>
            <Badge tone={record.success ? 'success' : 'error'}>
              {record.success ? <CheckCircle2 className="size-3" /> : <XCircle className="size-3" />}
              {record.success ? '成功' : '失败'}
            </Badge>
          </div>
        )}
        {record.errorMessage && (
          <div>
            <div className="mb-1 text-xs font-semibold text-destructive">错误信息</div>
            <pre className="scrollbar-thin max-h-32 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-muted p-3 text-xs">
              {record.errorMessage}
            </pre>
          </div>
        )}
        <div>
          <div className="mb-1 text-xs font-semibold text-muted-foreground">变化摘要</div>
          <AuditDetailSummary record={record} />
        </div>
      </div>
    </ModalShell>
  )
}

// ─── 保留时长设置 ─────────────────────────────────────────────────────────────

function RetentionDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const runtimeConfig = useRuntimeConfig()
  const update = useUpdateRuntimeConfig()
  const confirm = useConfirm()
  const currentEnabled = runtimeConfig.data?.auditLogRetentionEnabled ?? false
  const currentDays = runtimeConfig.data?.auditLogRetentionDays ?? 15
  const [draftEnabled, setDraftEnabled] = useState<boolean | null>(null)
  const [draftDays, setDraftDays] = useState<string | null>(null)
  const enabled = draftEnabled ?? currentEnabled
  const value = draftDays ?? String(currentDays)
  const days = Number(value)
  const valid = Number.isInteger(days) && days >= 1 && days <= 3650

  const close = () => { setDraftEnabled(null); setDraftDays(null); onClose() }
  const save = async () => {
    if (!runtimeConfig.data || !valid) return
    // Turning cleanup on, or shortening an active retention, deletes history; ask first.
    const deletes = enabled && (!currentEnabled || days < currentDays)
    if (deletes) {
      const ok = await confirm({
        title: '开启自动清理？',
        message: `保存后，后台会在 1 小时内开始删除 ${days} 天以前的全部审计日志（含账号异动和后台操作记录），删除后无法恢复。需要留存的话请先备份。`,
        confirmText: '确认开启',
        tone: 'danger',
      })
      if (!ok) return
    }
    update.mutate({ ...runtimeConfig.data, auditLogRetentionEnabled: enabled, auditLogRetentionDays: days }, {
      onSuccess: () => { toast.success(enabled ? `已开启自动清理，保留 ${days} 天` : '已关闭自动清理，不会删除任何日志'); close() },
      onError: (error) => toast.error(`保存失败：${extractErrorMessage(error)}`),
    })
  }

  return (
    <ModalShell
      open={open}
      onClose={close}
      title="日志自动清理"
      description="默认关闭，不删除任何日志。开启后，后台每小时清理一次超过保留天数的审计日志（含账号异动），分小批执行，失败只记日志，不影响调度。"
      footer={
        <>
          <Button variant="outline" onClick={close} disabled={update.isPending}>取消</Button>
          <Button onClick={() => void save()} disabled={!valid || !runtimeConfig.data || update.isPending}>{update.isPending ? '保存中...' : '保存'}</Button>
        </>
      }
    >
      {runtimeConfig.isLoading ? (
        <LoadingState text="读取配置..." />
      ) : runtimeConfig.error ? (
        <ErrorState title="读取配置失败" message={extractErrorMessage(runtimeConfig.error)} />
      ) : (
        <div className="space-y-4">
          <label className="flex items-center justify-between gap-3 rounded-lg border p-3">
            <span>
              <span className="block text-sm font-medium">自动清理过期日志</span>
              <span className="block text-xs text-muted-foreground">{enabled ? '已开启：超过保留天数的日志会被删除' : '已关闭：所有日志都会保留'}</span>
            </span>
            <Switch checked={enabled} onCheckedChange={setDraftEnabled} aria-label="自动清理过期日志" />
          </label>
          <div className="space-y-2">
            <label className="text-sm font-medium" htmlFor="audit-retention-days">保留天数</label>
            <div className="flex items-center gap-2">
              <Input
                id="audit-retention-days"
                className="w-32"
                type="number"
                min={1}
                max={3650}
                value={value}
                disabled={!enabled}
                onChange={(event) => setDraftDays(event.target.value)}
              />
              <span className="text-sm text-muted-foreground">天（1 ~ 3650，默认 15）</span>
            </div>
            {!valid && <p className="text-xs text-destructive">请输入 1 到 3650 之间的整数</p>}
          </div>
          {enabled && !currentEnabled && (
            <Callout tone="warning">开启后会删除 {valid ? days : '-'} 天以前的全部审计日志，删除后无法恢复。</Callout>
          )}
        </div>
      )}
    </ModalShell>
  )
}

// ─── 账号异动 ─────────────────────────────────────────────────────────────────

const DEFAULT_ACCOUNT_FILTER: AccountFilterState = { range: '24', kind: '__all__', source: '__all__', account: '' }

function AccountEventsView({ onSelect }: { onSelect: (record: AdminAuditLogRow) => void }) {
  const [filter, setFilter] = useState<AccountFilterState>(DEFAULT_ACCOUNT_FILTER)
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_LIMIT)
  // Recomputed only when the filter or page changes, so the time window doesn't move on every render.
  const query = useMemo(() => accountEventQuery(filter, page, pageSize), [filter, page, pageSize])
  const logs = useAuditLogsPage(query)
  const summaryHours = filter.range === 'all' ? 720 : Number(filter.range)
  const summary = useAccountEventSummary(summaryHours, filter.source === '__all__' ? undefined : filter.source)
  const records = logs.data?.records ?? []
  const pending = logs.isPlaceholderData || logs.isFetching
  const tiles = summaryTiles(summary.data?.items ?? [])
  const changed = filter.kind !== '__all__' || filter.source !== '__all__' || !!filter.account.trim() || filter.range !== DEFAULT_ACCOUNT_FILTER.range
  const rangeLabel = filter.range === 'all' ? '最近 30 天' : TIME_RANGES.find((item) => item.value === filter.range)?.label ?? ''

  const update = (patch: Partial<AccountFilterState>) => { setFilter((prev) => ({ ...prev, ...patch })); setPage(1) }

  return (
    <div className="space-y-4">
      <div>
        <div className="mb-2 text-xs text-muted-foreground">{rangeLabel}的异动统计，点击可筛选</div>
        {summary.isLoading ? (
          <LoadingState text="统计中..." className="py-4" />
        ) : tiles.length === 0 ? (
          <div className="rounded-lg bg-muted/30 px-4 py-3 text-sm text-muted-foreground">这段时间没有账号异动</div>
        ) : (
          <StatGrid min="9.5rem">
            {tiles.map((tile) => (
              <button
                key={tile.kind}
                type="button"
                className={`rounded-xl text-left outline-none ring-primary focus-visible:ring-2 ${filter.kind === tile.kind ? 'ring-2' : ''}`}
                onClick={() => update({ kind: filter.kind === tile.kind ? '__all__' : tile.kind })}
                aria-pressed={filter.kind === tile.kind}
                aria-label={`筛选${tile.label}`}
              >
                <StatCard
                  title={tile.label}
                  value={tile.events}
                  desc={`涉及 ${tile.accounts} 个账号`}
                  tone={tile.tone === 'neutral' ? 'default' : tile.tone}
                />
              </button>
            ))}
          </StatGrid>
        )}
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Select value={filter.range} onValueChange={(range) => update({ range })}>
          <SelectTrigger size="sm" className="w-36" aria-label="时间范围"><SelectValue /></SelectTrigger>
          <SelectContent>
            {TIME_RANGES.map((item) => <SelectItem key={item.value} value={item.value}>{item.label}</SelectItem>)}
          </SelectContent>
        </Select>
        <Select value={filter.kind} onValueChange={(kind) => update({ kind })}>
          <SelectTrigger size="sm" className="w-36" aria-label="变动类型"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">全部变动</SelectItem>
            {ACCOUNT_EVENT_ORDER.map((kind) => <SelectItem key={kind} value={kind}>{accountEventMeta(kind).label}</SelectItem>)}
          </SelectContent>
        </Select>
        <div className="min-w-48 flex-1">
          <ToolbarSearch value={filter.account} onChange={(account) => update({ account })} placeholder="账号邮箱、名称或 #ID" />
        </div>
        {changed && (
          <Button variant="ghost" size="sm" onClick={() => { setFilter(DEFAULT_ACCOUNT_FILTER); setPage(1) }}>
            <X className="h-3.5 w-3.5" />重置
          </Button>
        )}
        <Button variant="outline" size="sm" onClick={() => { logs.refetch(); summary.refetch() }} disabled={pending}>
          <RefreshCw className={`h-3.5 w-3.5 ${pending ? 'animate-spin' : ''}`} />刷新
        </Button>
      </div>

      {logs.isLoading ? (
        <LoadingState text="加载账号异动..." className="py-8" />
      ) : logs.error ? (
        <ErrorState title="账号异动加载失败" message={extractErrorMessage(logs.error)} />
      ) : records.length === 0 ? (
        <EmptyState
          icon={<Activity />}
          title="没有账号异动"
          description={changed ? '没有匹配当前筛选条件的记录' : '额度耗尽、自动禁用、风控和恢复会记录在这里'}
          action={changed ? <Button variant="outline" size="sm" onClick={() => { setFilter(DEFAULT_ACCOUNT_FILTER); setPage(1) }}>重置筛选</Button> : undefined}
        />
      ) : (
        <>
          <div className="scrollbar-thin overflow-x-auto rounded-lg border">
            <Table className="min-w-[760px]">
              <TableHeader>
                <TableRow>
                  <TableHead>时间</TableHead>
                  <TableHead>账号</TableHead>
                  <TableHead>变动</TableHead>
                  <TableHead>原因</TableHead>
                  <TableHead>触发</TableHead>
                  <TableHead className="w-12 text-right">详情</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {records.map((record) => {
                  const event = accountEventView(record)
                  if (!event) return null
                  return (
                    <TableRow key={record.id} className="cursor-pointer hover:bg-muted/40" onClick={() => onSelect(record)}>
                      <TableCell className="whitespace-nowrap text-xs tabular-nums text-muted-foreground">{formatDate(record.createdAt)}</TableCell>
                      <TableCell className="max-w-64">
                        <button
                          type="button"
                          className="block max-w-full truncate text-left text-xs font-medium hover:text-primary hover:underline"
                          title={`只看这个账号：${event.accountLabel}`}
                          onClick={(e) => { e.stopPropagation(); update({ account: `#${event.accountId}`, source: event.source }) }}
                        >
                          {event.accountLabel}
                        </button>
                        <div className="text-[0.62rem] text-muted-foreground/70">账号 #{event.accountId}</div>
                      </TableCell>
                      <TableCell><Badge tone={event.tone}>{event.label}</Badge></TableCell>
                      <TableCell className="max-w-72 truncate text-xs" title={event.reason}>{event.reason || '—'}</TableCell>
                      <TableCell className="whitespace-nowrap text-xs text-muted-foreground">{event.actor}</TableCell>
                      <TableCell className="text-right">
                        <Button variant="ghost" size="icon-xs" onClick={(e) => { e.stopPropagation(); onSelect(record) }} title="查看详情" aria-label="查看详情">
                          <Eye className="size-3.5" />
                        </Button>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
          <PageNav
            page={page}
            pageSize={pageSize}
            hasNext={Boolean(logs.data?.hasNext)}
            pending={pending}
            onChange={setPage}
            onPageSizeChange={(next) => { setPageSize(next); setPage(1) }}
          />
        </>
      )}
    </div>
  )
}

// ─── 全部操作 ─────────────────────────────────────────────────────────────────

function AllLogsView({ onSelect }: { onSelect: (record: AdminAuditLogRow) => void }) {
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_LIMIT)
  const [q, setQ] = useState('')
  const [successFilter, setSuccessFilter] = useState<'__all__' | 'success' | 'failed'>('__all__')
  const [categoryFilter, setCategoryFilter] = useState('__all__')
  const [showFilters, setShowFilters] = useState(false)

  const query = useMemo<AdminAuditLogPageQuery>(() => ({
    page,
    limit: pageSize,
    success: successFilter === '__all__' ? undefined : successFilter === 'success',
  }), [page, pageSize, successFilter, categoryFilter])
  const logs = useAuditLogsPage(query)
  const pending = logs.isPlaceholderData || logs.isFetching

  // Free-text and categories without their own object type still filter the loaded page.
  const records = useMemo(() => {
    let filtered = logs.data?.records ?? []
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
    if (categoryFilter !== '__all__' && categoryFilter !== 'account') {
      filtered = filtered.filter((r) => ACTION_TO_CATEGORY[r.action] === categoryFilter)
    }
    return filtered
  }, [logs.data?.records, q, categoryFilter])

  const hasFilters = successFilter !== '__all__' || categoryFilter !== '__all__' || !!q.trim()
  const filterCount = [successFilter !== '__all__', categoryFilter !== '__all__', !!q.trim()].filter(Boolean).length
  const clearFilters = () => { setQ(''); setSuccessFilter('__all__'); setCategoryFilter('__all__'); setPage(1) }

  return (
    <div className="space-y-2">
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
        </ToolbarActions>
      </Toolbar>

      {showFilters && (
        <div className="rounded-lg bg-muted/30 p-3">
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

      {logs.isLoading ? (
        <LoadingState text="加载日志..." className="py-8" />
      ) : logs.error ? (
        <ErrorState title="审计日志加载失败" message={extractErrorMessage(logs.error)} />
      ) : records.length === 0 ? (
        <EmptyState
          icon={<FileClock />}
          title="暂无审计记录"
          description={hasFilters ? '没有匹配当前筛选条件的记录' : '后台关键操作记录会在这里展示'}
          action={hasFilters ? <Button variant="outline" size="sm" onClick={clearFilters}>清除筛选</Button> : undefined}
        />
      ) : (
        <>
          <div className="scrollbar-thin overflow-x-auto rounded-lg border">
            <Table className="min-w-[720px]">
              <TableHeader>
                <TableRow>
                  <TableHead>时间</TableHead>
                  <TableHead>动作</TableHead>
                  <TableHead>对象</TableHead>
                  <TableHead>执行者</TableHead>
                  <TableHead>变化摘要</TableHead>
                  <TableHead>结果</TableHead>
                  <TableHead className="w-16 text-right">查看</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {records.map((record) => {
                  const event = accountEventView(record)
                  return (
                    <TableRow key={record.id} className="cursor-pointer hover:bg-muted/40" onClick={() => onSelect(record)}>
                      <TableCell className="whitespace-nowrap text-xs tabular-nums text-muted-foreground">
                        {formatDate(record.createdAt)}
                      </TableCell>
                      <TableCell>
                        <div className="flex items-center gap-1.5 text-xs font-medium text-foreground">
                          {actionLabel(record.action)}
                          {event && <Badge tone={event.tone}>{event.label}</Badge>}
                        </div>
                        <div className="font-mono text-[0.62rem] text-muted-foreground/60">{record.action}</div>
                      </TableCell>
                      <TableCell>
                        <div className="text-xs">{event ? event.accountLabel : objectLabel(record.objectType)}</div>
                        <div className="text-[0.62rem] text-muted-foreground/60">
                          {event ? `${objectLabel(record.objectType)} #${record.objectId}` : record.objectId ? `#${record.objectId}` : '—'}
                        </div>
                      </TableCell>
                      <TableCell className="text-xs">{record.actor}</TableCell>
                      <TableCell className="max-w-[18rem]"><AuditChangeSummary record={record} /></TableCell>
                      <TableCell>
                        <Badge tone={record.success ? 'success' : 'error'}>
                          {record.success ? <CheckCircle2 className="size-3" /> : <XCircle className="size-3" />}
                          {record.success ? '成功' : '失败'}
                        </Badge>
                      </TableCell>
                      <TableCell className="text-right">
                        <Button
                          variant="ghost"
                          size="icon-xs"
                          onClick={(e) => { e.stopPropagation(); onSelect(record) }}
                          title="查看详情"
                          aria-label="查看详情"
                        >
                          <Eye className="size-3.5" />
                        </Button>
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
          <PageNav
            page={page}
            pageSize={pageSize}
            hasNext={Boolean(logs.data?.hasNext)}
            pending={pending}
            onChange={setPage}
            onPageSizeChange={(next) => { setPageSize(next); setPage(1) }}
          />
        </>
      )}
    </div>
  )
}

function PageNav({ page, pageSize, hasNext, pending, onChange, onPageSizeChange }: {
  page: number
  pageSize: number
  hasNext: boolean
  pending: boolean
  onChange: (page: number) => void
  onPageSizeChange: (pageSize: number) => void
}) {
  return (
    <div className="flex flex-wrap items-center justify-center gap-3 py-1">
      <Button variant="outline" size="sm" disabled={page === 1 || pending} onClick={() => onChange(Math.max(1, page - 1))}>
        上一页
      </Button>
      <span className="text-xs text-muted-foreground">第 {page} 页</span>
      <Button variant="outline" size="sm" disabled={!hasNext || pending} onClick={() => onChange(page + 1)}>
        下一页
      </Button>
      <PageSizeSelect value={pageSize} onChange={onPageSizeChange} disabled={pending} />
    </div>
  )
}

// ─── 主页 ──────────────────────────────────────────────────────────────────────

export function AuditPage() {
  const [view, setView] = useState<'accounts' | 'all'>('accounts')
  const [selected, setSelected] = useState<AdminAuditLogRow | null>(null)
  const [retentionOpen, setRetentionOpen] = useState(false)
  const runtimeConfig = useRuntimeConfig()
  const retentionDays = runtimeConfig.data?.auditLogRetentionDays
  const retentionEnabled = runtimeConfig.data?.auditLogRetentionEnabled ?? false

  return (
    <PageContainer>
      <PageHeader
        title="审计"
        subtitle="账号异动和后台关键操作记录，便于排查账号状态与配置变化来源"
        actions={
          <Button variant="outline" size="sm" onClick={() => setRetentionOpen(true)}>
            <Settings2 className="h-3.5 w-3.5" />
            {retentionEnabled ? `自动清理：保留 ${retentionDays ?? '-'} 天` : '自动清理：已关闭'}
          </Button>
        }
      />

      <SectionCard>
        <Tabs value={view} onValueChange={(value) => setView(value as typeof view)}>
          <TabsList className="mb-4">
            <TabsTrigger value="accounts"><Activity className="size-3.5" />账号异动</TabsTrigger>
            <TabsTrigger value="all"><FileClock className="size-3.5" />全部操作</TabsTrigger>
          </TabsList>
        </Tabs>
        {view === 'accounts' ? <AccountEventsView onSelect={setSelected} /> : <AllLogsView onSelect={setSelected} />}
      </SectionCard>

      <AuditDetailModal record={selected} open={Boolean(selected)} onClose={() => setSelected(null)} />
      <RetentionDialog open={retentionOpen} onClose={() => setRetentionOpen(false)} />
    </PageContainer>
  )
}
