import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import {
  ArrowDownWideNarrow,
  ArrowUpNarrowWide,
  ArrowUpRight,
  Archive,
  Download,
  Ellipsis,
  FlaskConical,
  FolderCog,
  FolderInput,
  Pencil,
  RefreshCw,
  Router,
  Search,
  Send,
  Trash2,
  Upload,
  Wallet,
  X,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  Badge,
  Button,
  Checkbox,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Progress,
  Select,
  SelectContent,
  SelectItem,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
  Spinner,
} from '@/components/ui'
import {
  Callout,
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
  Pagination,
  SectionCard,
  StatCard,
  StatGrid,
  useConfirm,
  PAGE_SIZE_OPTIONS,
} from '@/components/patterns'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { checkInventory, getInventoryIds, testInventory } from '@/api/inventory'
import { useProxyResources } from '@/hooks/use-credentials'
import { useDeleteInventory, useInventory, useInventoryGroups } from '@/hooks/use-inventory'
import { useModelCapabilities } from '@/hooks/use-usage'
import { formatCredits, formatFullDate, formatLastUsed } from '@/lib/format'
import { buildTestModelOptions, defaultTestModelForOptions, DEFAULT_TEST_PROMPT } from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import type { InventoryFilter, InventoryIdsResponse, InventoryItem, InventoryOrigin, InventoryStatus } from '@/types/api'
import { pageMeta } from '@/types/ui'
import { QuotaBar } from '@/features/credentials/credential-card'
import {
  authLabel,
  CREDENTIAL_SUBSCRIPTION_OPTIONS,
  credentialSubscriptionLabel,
  disabledReasonLabel,
  quotaBarModel,
} from '@/features/credentials/credential-utils'
import { useInventoryBatch, type InventoryBatchKind, type InventoryBatchState } from './inventory-batch'
import {
  InventoryEditModal,
  InventoryExportModal,
  InventoryGroupsModal,
  InventoryImportModal,
  InventoryProxyModal,
  InventoryPushModal,
  InventorySetGroupModal,
} from './inventory-dialogs'

/** 订阅、额度、代理、认证等筛选的"全部"。 */
const ALL = '__all__'

const QUOTA_LABEL: Record<string, string> = {
  remaining: '有剩余额度',
  exhausted: '额度已用尽',
  unchecked: '未查询额度',
}

/** 分组筛选：全部、某个分组、未分组（后端用 `__none__`）。 */
const ALL_GROUPS = '__all__'
const UNGROUPED = '__none__'

/** 来源筛选：账号池移入（可再按原禁用原因细分）或导入。 */
const ALL_ORIGINS = '__all__'
const ORIGIN_POOL = '__pool__'
const ORIGIN_IMPORTED = '__imported__'
const ORIGIN_REASONS = ['QuotaExceeded', 'Manual', 'TooManyFailures', 'TooManyRefreshFailures', 'InvalidRefreshToken', 'AccountSuspended', 'AccountLocked', 'TemporarilySuspended', 'InvalidConfig']

const HEALTH_LABEL: Record<string, string> = {
  account_error: '账号异常',
  quota_exhausted: '额度耗尽',
  unknown: '状态未知',
  normal: '正常',
}

/** 卡片上的原状态：来源、禁用原因、订阅，以及原来的调度参数。 */
function originLines(origin: InventoryOrigin): { summary: string; settings: string; detail: string } {
  const summary = [
    `来自账号池 #${origin.credentialId}`,
    origin.disabledReason ? disabledReasonLabel(origin.disabledReason) : null,
    origin.healthStatus && origin.healthStatus !== 'normal' && !(origin.healthStatus === 'quota_exhausted' && origin.disabledReason === 'QuotaExceeded')
      ? HEALTH_LABEL[origin.healthStatus] ?? origin.healthStatus
      : null,
    origin.subscriptionTitle ? credentialSubscriptionLabel(origin.subscriptionTitle) : null,
  ].filter(Boolean).join(' · ')
  const settings = [
    origin.priority ? `优先级 ${origin.priority}` : null,
    origin.rpm ? `RPM ${origin.rpm}` : null,
    origin.maxConcurrentRequests ? `并发 ${origin.maxConcurrentRequests}` : null,
    origin.supportedModels?.length ? `模型 ${origin.supportedModels.length} 个` : null,
    origin.tags?.length ? `标签 ${origin.tags.join('、')}` : null,
  ].filter(Boolean).join(' · ')
  const detail = [
    summary,
    origin.email ? `邮箱：${origin.email}` : null,
    origin.usageLimit ? `额度：${(origin.currentUsage ?? 0).toFixed(2)} / ${origin.usageLimit}` : null,
    settings ? `原调度参数：${settings}` : null,
    origin.supportedModels?.length ? `模型：${origin.supportedModels.join('、')}` : null,
    origin.hadProxy ? `原代理：${origin.proxyResourceName || (origin.proxyResourceId ? `#${origin.proxyResourceId}` : '直连代理')}` : null,
    `失败 ${origin.failureCount ?? 0} 次 · 刷新失败 ${origin.refreshFailureCount ?? 0} 次 · 成功 ${origin.successCount ?? 0} 次`,
    origin.lastErrorReason ? `最后错误：${origin.lastErrorKind ? `[${origin.lastErrorKind}] ` : ''}${origin.lastErrorReason}` : null,
    origin.lastUsedAt ? `最后使用：${formatFullDate(origin.lastUsedAt)}` : null,
    origin.createdAt ? `加入账号池：${formatFullDate(origin.createdAt)}` : null,
    origin.movedAt ? `移入库存：${formatFullDate(origin.movedAt)}` : null,
  ].filter(Boolean).join('\n')
  return { summary, settings, detail }
}


const STATUS_META: Record<InventoryStatus, { label: string; tone: 'success' | 'error' | 'warning' | 'info'; dot: string }> = {
  stored: { label: '在库', tone: 'success', dot: 'bg-success' },
  invalid: { label: '失效', tone: 'error', dot: 'bg-destructive' },
  pushing: { label: '推送中', tone: 'warning', dot: 'bg-warning' },
  pushed: { label: '已入池', tone: 'info', dot: 'bg-info' },
}

type CardBusy = 'check' | 'test'

function InventoryCard({ item, selected, busy, onToggle, onEdit, onCheck, onTest, onPush, onSetGroup, onSetProxy, onExport, onDelete }: {
  item: InventoryItem
  selected: boolean
  busy?: CardBusy
  onToggle: () => void
  onEdit: () => void
  onCheck: () => void
  onTest: () => void
  onPush: () => void
  onSetGroup: () => void
  onSetProxy: () => void
  onExport: () => void
  onDelete: () => void
}) {
  const status = STATUS_META[item.status]
  const selectable = item.status === 'stored' || item.status === 'invalid'
  const quota = item.accountInfo ? quotaBarModel(item.accountInfo) : null
  const error = item.status === 'invalid' ? item.lastRefreshError : item.lastCheckError || item.lastRefreshError
  return (
    <div className={`relative flex flex-col overflow-hidden rounded-xl bg-card shadow-sm transition-shadow hover:shadow-md ${item.status === 'pushed' ? 'opacity-90' : ''}`}>
      <div className="flex items-start gap-2.5 px-3 pb-2 pt-3">
        <Checkbox checked={selected} onCheckedChange={onToggle} disabled={!selectable} className="mt-0.5 shrink-0" aria-label={`选择库存账号 #${item.id}`} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="max-w-[220px] truncate text-sm font-semibold" title={item.email || undefined}>{item.email || `库存账号 #${item.id}`}</span>
            <span className="font-mono text-xs text-muted-foreground tabular">#{item.id}</span>
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-1">
            <Badge tone={status.tone}>{status.label}</Badge>
            <Badge>{authLabel(item.authMethod)}</Badge>
            {item.accountInfo?.subscriptionTitle && <Badge tone="primary">{credentialSubscriptionLabel(item.accountInfo.subscriptionTitle)}</Badge>}
            {item.authRegion && <Badge tone="secondary">{item.authRegion}</Badge>}
            {item.group && <Badge tone="primary" title="分组">{item.group}</Badge>}
            {item.hasProxy ? (
              <Badge tone={item.proxyUsable === false ? 'error' : 'info'} title={item.proxyUrl}>
                {item.proxyResourceName || (item.proxyResourceId ? `代理 #${item.proxyResourceId}` : '直连代理')}
                {item.proxyUsable === false && '（不可用）'}
              </Badge>
            ) : item.status !== 'pushed' && <Badge tone="warning">未绑定代理</Badge>}
          </div>
        </div>
      </div>

      <div className="flex-1 space-y-2 px-3 pb-3">
        {item.status === 'pushed' && item.pool ? (
          <div className="rounded-lg bg-muted/40 px-3 py-2 text-xs">
            <div className="flex items-center justify-between gap-2">
              <span className="font-medium">账号池 #{item.pool.credentialId}</span>
              {item.pool.exists ? (
                <Badge tone={item.pool.disabled ? 'warning' : 'success'}>{item.pool.disabled ? '已禁用' : '调度中'}</Badge>
              ) : (
                <Badge tone="neutral">已下线</Badge>
              )}
            </div>
            <div className="mt-1 text-muted-foreground">
              {item.pool.exists
                ? `${item.pool.subscriptionTitle ? credentialSubscriptionLabel(item.pool.subscriptionTitle) + ' · ' : ''}${item.pool.disabledReason ? `${disabledReasonLabel(item.pool.disabledReason)} · ` : ''}由账号池续期`
                : '账号池中已删除'}
            </div>
            {item.pool.exists && (
              <Link to={`/credentials?q=%23${item.pool.credentialId}`} className="mt-1 inline-flex items-center gap-0.5 text-primary hover:underline">
                去账号页查看<ArrowUpRight className="size-3" aria-hidden="true" />
              </Link>
            )}
          </div>
        ) : (
          <>
            <div className="flex items-center justify-between text-xs">
              <span className="text-muted-foreground">额度</span>
              {item.accountInfo ? (
                <span className="tabular font-medium">{`${formatCredits(item.accountInfo.currentUsage)} / ${formatCredits(item.accountInfo.usageLimit)}`}</span>
              ) : (
                <button type="button" className="text-primary hover:underline disabled:opacity-60" onClick={onCheck} disabled={Boolean(busy) || !selectable}>
                  {busy === 'check' ? '查询中…' : '未查询，点击查询'}
                </button>
              )}
            </div>
            {quota && <QuotaBar model={quota} />}
            <div className="grid grid-cols-2 gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
              <span className="truncate">续期 {item.lastRefreshAt ? formatLastUsed(item.lastRefreshAt) : '未续期'}</span>
              <span className="truncate">查询 {item.lastCheckAt ? formatLastUsed(item.lastCheckAt) : '未查询'}</span>
              <span className="truncate" title={item.expiresAt}>Token 到期 {item.expiresAt ? formatFullDate(item.expiresAt) : '-'}</span>
              <span className="truncate">入库 {formatLastUsed(item.createdAt)}</span>
            </div>
          </>
        )}
        {error && item.status !== 'pushed' && <p className="line-clamp-2 text-[11px] leading-4 text-destructive" title={error}>{error}</p>}
        {item.status === 'stored' && item.nextRenewAt && item.lastRefreshError && (
          <p className="text-[11px] text-muted-foreground">续期失败 {item.refreshFailureCount} 次，{formatFullDate(item.nextRenewAt)} 再试</p>
        )}
        {item.origin && (() => {
          const origin = originLines(item.origin)
          return (
            <div className="rounded-md bg-muted/40 px-2 py-1.5 text-[11px] leading-4 text-muted-foreground" title={origin.detail}>
              <p className="truncate font-medium text-foreground/80">{origin.summary}</p>
              {origin.settings && <p className="truncate">原参数 {origin.settings}</p>}
            </div>
          )
        })()}
        {(item.source || item.note) && (
          <p className="truncate text-[11px] text-muted-foreground" title={[item.source, item.note].filter(Boolean).join(' · ')}>
            {[item.source, item.note].filter(Boolean).join(' · ')}
          </p>
        )}
      </div>

      <div className="flex items-center gap-0.5 border-t px-2 py-1.5">
        {selectable && (
          <>
            <Button variant="ghost" size="xs" onClick={onTest} disabled={Boolean(busy)} title="用这个账号真实调一次模型（会消耗少量额度）">
              {busy === 'test' ? <Spinner size="sm" /> : <FlaskConical />}测试
            </Button>
            <Button variant="ghost" size="xs" onClick={onCheck} disabled={Boolean(busy)} title="刷新订阅和额度；Token 过期会先续期">
              {busy === 'check' ? <Spinner size="sm" /> : <Wallet />}额度
            </Button>
            <Button variant="ghost" size="xs" onClick={onEdit} title="编辑区域、代理、备注和分组">
              <Pencil />编辑
            </Button>
          </>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon-xs" className="ml-auto" title="更多操作" aria-label="更多操作">
              <Ellipsis />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-44">
            {selectable && (
              <>
                <DropdownMenuItem onSelect={onPush}><Send /> 推送到账号池</DropdownMenuItem>
                <DropdownMenuItem onSelect={onSetGroup}><FolderInput /> 设置分组</DropdownMenuItem>
                <DropdownMenuItem onSelect={onSetProxy}><Router /> 设置代理</DropdownMenuItem>
                <DropdownMenuItem onSelect={onExport}><Download /> 导出此账号</DropdownMenuItem>
                <DropdownMenuSeparator />
              </>
            )}
            <DropdownMenuItem className="text-destructive focus:text-destructive" disabled={item.status === 'pushing'} onSelect={onDelete}>
              <Trash2 /> {item.status === 'pushed' ? '删除记录' : '删除'}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  )
}

/** 一组库存账号：ids 是要处理的账号，items 是其中已加载的（跨页全选时可能只是一部分）。 */
interface InventoryTarget {
  ids: number[]
  items: InventoryItem[]
}

const SUBSCRIPTION_LABEL: Record<string, string> = Object.fromEntries([
  ...CREDENTIAL_SUBSCRIPTION_OPTIONS.map((option) => [option.value, option.label]),
  ['unknown', '未知'],
])

const SORT_OPTIONS = [
  ['__default__', '最新入库'],
  ['remaining', '剩余额度'],
  ['checked', '最近查询'],
  ['created', '入库时间'],
  ['expires', 'Token 到期'],
] as const

function BatchProgressPanel({ state, onCancel, onResume, onDismiss }: {
  state: InventoryBatchState
  onCancel: () => void
  onResume: () => void
  onDismiss: () => void
}) {
  const label = state.kind === 'check' ? '查询额度' : '验活'
  const percent = state.total ? Math.round((state.done / state.total) * 100) : 0
  const tone = !state.running && state.failed > 0 ? 'warning' : !state.running ? 'success' : 'info'
  return (
    <Callout tone={tone} className="mt-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="font-medium">
          {label} {state.done}/{state.total}
          <span className="ml-2 font-normal text-muted-foreground">
            成功 {state.succeeded} · 失败 {state.failed}
            {state.waitMs > 0 && ` · 刷新限流中，${Math.round(state.waitMs / 1000)} 秒后重试`}
            {state.paused && ' · 连续被限流，已暂停'}
            {!state.running && (state.cancelled ? ' · 已取消' : ' · 已完成')}
          </span>
        </span>
        <span className="flex gap-1.5">
          {state.paused && <Button size="xs" onClick={onResume}>继续</Button>}
          {state.running
            ? <Button variant="outline" size="xs" onClick={onCancel}>取消</Button>
            : <Button variant="outline" size="xs" onClick={onDismiss}>关闭</Button>}
        </span>
      </div>
      <Progress value={percent} className="mt-2 h-1.5" tone={tone === 'warning' ? 'warning' : 'primary'} />
      {state.failures.length > 0 && (
        <details className="mt-2 text-xs">
          <summary className="cursor-pointer text-muted-foreground">失败 {state.failures.length} 个</summary>
          <ul className="mt-1 max-h-40 space-y-0.5 overflow-y-auto">
            {state.failures.map((failure) => (
              <li key={failure.id} className="truncate" title={failure.message}>#{failure.id} {failure.message}</li>
            ))}
          </ul>
        </details>
      )}
    </Callout>
  )
}

export function InventoryPage() {
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [statusFilter, setStatusFilter] = useState<'__all__' | InventoryStatus>('__all__')
  const [groupFilter, setGroupFilter] = useState<string>(ALL_GROUPS)
  const [originFilter, setOriginFilter] = useState<string>(ALL_ORIGINS)
  const [subscriptionFilter, setSubscriptionFilter] = useState<string>(ALL)
  const [quotaFilter, setQuotaFilter] = useState<string>(ALL)
  const [proxyFilter, setProxyFilter] = useState<string>(ALL)
  const [authFilter, setAuthFilter] = useState<string>(ALL)
  const [sort, setSort] = useState<string>('__default__')
  const [order, setOrder] = useState<'asc' | 'desc'>('desc')
  const [queryText, setQueryText] = useState('')
  const query = useDebouncedValue(queryText.trim(), 300)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [allFiltered, setAllFiltered] = useState<InventoryIdsResponse | null>(null)
  const [selectingAll, setSelectingAll] = useState(false)
  const [busy, setBusy] = useState<Map<number, CardBusy>>(new Map())
  const [importOpen, setImportOpen] = useState(false)
  const [pushTarget, setPushTarget] = useState<InventoryTarget | null>(null)
  const [groupTarget, setGroupTarget] = useState<number[] | null>(null)
  const [proxyTarget, setProxyTarget] = useState<number[] | null>(null)
  const [editing, setEditing] = useState<InventoryItem | null>(null)
  const [groupsOpen, setGroupsOpen] = useState(false)
  const [exportIds, setExportIds] = useState<number[] | null>(null)
  const groups = useInventoryGroups()
  const proxyResources = useProxyResources()
  const modelCapabilities = useModelCapabilities()
  const queryClient = useQueryClient()
  const listTopRef = useRef<HTMLDivElement | null>(null)
  const confirm = useConfirm()

  const filter = useMemo<InventoryFilter>(() => ({
    status: statusFilter === '__all__' ? undefined : statusFilter,
    q: query || undefined,
    group: groupFilter === ALL_GROUPS ? undefined : groupFilter,
    origin: originFilter === ALL_ORIGINS ? undefined : originFilter,
    subscription: subscriptionFilter === ALL ? undefined : subscriptionFilter,
    quota: quotaFilter === ALL ? undefined : quotaFilter,
    proxy: proxyFilter === ALL ? undefined : proxyFilter,
    auth: authFilter === ALL ? undefined : authFilter,
  }), [statusFilter, query, groupFilter, originFilter, subscriptionFilter, quotaFilter, proxyFilter, authFilter])
  const sortQuery = useMemo(() => (sort === '__default__' ? {} : { sort, order }), [sort, order])
  const listQuery = useMemo(() => ({ page, limit: pageSize, ...filter, ...sortQuery }), [page, pageSize, filter, sortQuery])
  const inventory = useInventory(listQuery)
  const remove = useDeleteInventory()
  const testModelOptions = useMemo(() => buildTestModelOptions(modelCapabilities.data?.models), [modelCapabilities.data?.models])
  const testModel = defaultTestModelForOptions(testModelOptions)
  const renewal = inventory.data?.renewal
  const refreshInventory = useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: ['inventory'] })
  }, [queryClient])
  const batch = useInventoryBatch({
    refreshRpm: renewal?.tokenRefreshMaxRpm ?? 60,
    testModel,
    testPrompt: DEFAULT_TEST_PROMPT,
    onFinished: refreshInventory,
  })

  useEffect(() => { setPage(1) }, [filter, sortQuery, pageSize])
  // A dissolved or renamed group is no longer a valid filter.
  useEffect(() => {
    if (groupFilter === ALL_GROUPS || groupFilter === UNGROUPED || !groups.data) return
    if (!groups.data.groups.some((group) => group.name === groupFilter)) setGroupFilter(ALL_GROUPS)
  }, [groups.data, groupFilter])
  useEffect(() => { setSelectedIds(new Set()); setAllFiltered(null) }, [listQuery])
  useEffect(() => {
    const top = listTopRef.current
    if (top && top.getBoundingClientRect().top < 0) top.scrollIntoView({ block: 'start' })
  }, [listQuery])

  const items = inventory.data?.items ?? []
  const counts = inventory.data?.counts
  const summary = inventory.data?.summary
  const selectable = items.filter((item) => item.status === 'stored' || item.status === 'invalid')
  const selectedItems = selectable.filter((item) => selectedIds.has(item.id))
  const target: InventoryTarget = allFiltered
    ? { ids: allFiltered.ids, items: items.filter((item) => allFiltered.ids.includes(item.id)) }
    : { ids: selectedItems.map((item) => item.id), items: selectedItems }
  const pageAllSelected = selectable.length > 0 && selectedItems.length === selectable.length
  const toggle = (id: number) => {
    setAllFiltered(null)
    setSelectedIds((current) => {
      const next = new Set(current)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }
  const clearSelection = () => { setSelectedIds(new Set()); setAllFiltered(null) }
  const setCardBusy = (id: number, value: CardBusy | null) => setBusy((current) => {
    const next = new Map(current)
    if (value) next.set(id, value)
    else next.delete(id)
    return next
  })

  const selectAllFiltered = async () => {
    setSelectingAll(true)
    try {
      setAllFiltered(await getInventoryIds({ ...filter, ...sortQuery }))
    } catch (e) {
      toast.error(`读取筛选结果失败：${extractErrorMessage(e)}`)
    } finally {
      setSelectingAll(false)
    }
  }

  const checkOne = async (item: InventoryItem) => {
    setCardBusy(item.id, 'check')
    try {
      const result = (await checkInventory([item.id])).items[0]
      if (result?.ok) toast.success(`#${item.id} 已刷新订阅和额度`)
      else if (result?.deferred) toast.warning(`#${item.id} ${result.message ?? '稍后再试'}`)
      else toast.error(`#${item.id} 查询失败：${result?.message ?? '未知错误'}`)
    } catch (e) {
      toast.error(`#${item.id} 查询失败：${extractErrorMessage(e)}`)
    } finally {
      setCardBusy(item.id, null)
      refreshInventory()
    }
  }

  const testOne = async (item: InventoryItem) => {
    setCardBusy(item.id, 'test')
    try {
      const result = await testInventory(item.id, { model: testModel, prompt: DEFAULT_TEST_PROMPT })
      const refreshed = result.refreshed ? '（已续期 Token）' : ''
      if (result.outcome === 'success') {
        toast.success(`#${item.id} 测试成功${refreshed} · ${result.model} · ${result.durationMs} ms`, {
          description: result.response ? result.response.slice(0, 120) : undefined,
        })
      } else if (result.outcome === 'deferred') {
        toast.warning(`#${item.id} ${result.message ?? '稍后再试'}`)
      } else if (result.outcome === 'invalid') {
        toast.error(`#${item.id} refreshToken 已失效，账号已标为失效`, { description: result.message })
      } else {
        toast.error(`#${item.id} 测试失败（${result.stage === 'refresh' ? '续期 Token' : '调用模型'}）`, { description: result.message })
      }
    } catch (e) {
      toast.error(`#${item.id} 测试失败：${extractErrorMessage(e)}`)
    } finally {
      setCardBusy(item.id, null)
      refreshInventory()
    }
  }

  const runDelete = async (ids: number[], label: string) => {
    const ok = await confirm({
      title: '删除库存账号',
      message: `将从库存删除${label}，账号数据一并清除，无法恢复。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!ok) return
    try {
      const result = await remove.mutateAsync(ids)
      toast.success(`已删除 ${result.succeeded} 个账号`)
      clearSelection()
    } catch (e) {
      toast.error(`删除失败：${extractErrorMessage(e)}`)
    }
  }

  const runBatch = (kind: InventoryBatchKind) => {
    if (batch.state?.running) return toast.warning('已有批量任务在进行')
    void batch.run(kind, target.ids)
  }

  const groupItems = groups.data?.groups ?? []
  const namedGroupItems = groupItems.filter((group) => group.name)
  const ungroupedItem = groupItems.find((group) => !group.name)
  const proxyOptions = (proxyResources.data?.resources ?? []).filter((resource) => resource.enabled)
  const exportFilterLabel = [
    statusFilter !== '__all__' ? STATUS_META[statusFilter].label : null,
    groupFilter === UNGROUPED ? '未分组' : groupFilter !== ALL_GROUPS ? `分组「${groupFilter}」` : null,
    originFilter === ORIGIN_POOL ? '账号池移入' : originFilter === ORIGIN_IMPORTED ? '导入' : originFilter !== ALL_ORIGINS ? `原状态「${disabledReasonLabel(originFilter)}」` : null,
    subscriptionFilter !== ALL ? `订阅 ${SUBSCRIPTION_LABEL[subscriptionFilter] ?? subscriptionFilter}` : null,
    quotaFilter !== ALL ? QUOTA_LABEL[quotaFilter] : null,
    proxyFilter === 'with' ? '有代理' : proxyFilter === 'without' ? '无代理' : proxyFilter !== ALL ? `代理 ${proxyOptions.find((p) => String(p.id) === proxyFilter)?.name ?? `#${proxyFilter}`}` : null,
    authFilter !== ALL ? authLabel(authFilter) : null,
    query ? `搜索“${query}”` : null,
  ].filter(Boolean).join(' · ')
  const filtered = exportFilterLabel !== ''

  const meta = pageMeta.inventory
  if (inventory.isLoading && !inventory.data) return <LoadingState text="加载库存..." />
  if (inventory.error) return <ErrorState message={extractErrorMessage(inventory.error)} />

  const tierLine = (summary?.bySubscription ?? [])
    .map((tier) => `${SUBSCRIPTION_LABEL[tier.key] ?? tier.key} ${tier.accounts}`)
    .join(' · ')

  return (
    <PageContainer>
      <PageHeader
        title={meta.title}
        subtitle={meta.subtitle}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => void inventory.refetch()} disabled={inventory.isFetching} aria-label="刷新">
              <RefreshCw className={inventory.isFetching ? 'animate-spin' : ''} />
            </Button>
            <Button variant="outline" size="sm" onClick={() => setGroupsOpen(true)}><FolderCog /><span className="hidden sm:inline">管理分组</span></Button>
            <Button variant="outline" size="sm" onClick={() => setExportIds(target.ids.length > 0 && !allFiltered ? target.ids : [])} disabled={(counts?.total ?? 0) === 0}><Download /><span className="hidden sm:inline">导出</span></Button>
            <Button size="sm" onClick={() => setImportOpen(true)}><Upload />导入到库存</Button>
          </>
        }
      />

      <StatGrid>
        <StatCard
          title="在库"
          value={counts?.stored ?? 0}
          desc={renewal
            ? renewal.enabled
              ? `每 ${Math.round(renewal.minIntervalSecs / 3600)} 小时续期一次，每轮最多 ${renewal.maxPerPass} 个；与账号池共用刷新限流`
              : '后台刷新已关闭，库存账号不会自动续期'
            : '自动续期，不参与调度'}
          tone={renewal && !renewal.enabled ? 'warning' : 'success'}
        />
        <StatCard
          title="剩余额度"
          value={formatCredits(summary?.remaining ?? 0)}
          desc={summary
            ? `${filtered ? '当前筛选 · ' : ''}${summary.accounts} 个账号，未查询 ${summary.unchecked}，已用尽 ${summary.exhausted}${tierLine ? ` · ${tierLine}` : ''}（按各账号最近一次查询）`
            : '按各账号最近一次查询'}
          tone="info"
        />
        <StatCard title="已入池" value={counts?.pushed ?? 0} desc="由账号池接管" tone="info" />
        <StatCard title="失效" value={counts?.invalid ?? 0} desc="refreshToken 被拒，已停止续期" tone="error" />
      </StatGrid>

      <SectionCard
        className="overflow-visible"
        title="库存账号"
        description={`共 ${inventory.data?.total ?? 0} 个`}
      >
        <div className="z-20 -mx-4 -mt-1 border-b bg-card px-4 pb-3 pt-1 lg:sticky lg:top-[var(--header-h)]">
          <div className="flex flex-wrap items-center gap-2">
            <div className="inline-flex max-w-full flex-wrap gap-0.5 rounded-lg bg-muted p-0.5" role="radiogroup" aria-label="按状态筛选">
              {([['__all__', '全部', counts?.total ?? 0, ''] as const, ...(['stored', 'pushed', 'invalid', 'pushing'] as const).map((key) => [key, STATUS_META[key].label, counts?.[key] ?? 0, STATUS_META[key].dot] as const)]).map(([value, label, count, dot]) => {
                const active = statusFilter === value
                return (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={active}
                    onClick={() => setStatusFilter(active ? '__all__' : value)}
                    className={`flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40 ${active ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'}`}
                  >
                    {dot && <span className={`size-1.5 rounded-full ${dot}`} aria-hidden="true" />}
                    {label}
                    <span className="tabular-nums">{count}</span>
                  </button>
                )
              })}
            </div>
            <div className="relative min-w-[220px] flex-1 sm:max-w-sm">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden="true" />
              <Input value={queryText} onChange={(e) => setQueryText(e.target.value)} placeholder="邮箱 / #ID / 来源 / 备注 / 分组" className="h-8 pl-8 text-xs" aria-label="搜索库存" />
              {queryText && (
                <button type="button" className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground" onClick={() => setQueryText('')} aria-label="清除搜索">
                  <X className="size-3.5" />
                </button>
              )}
            </div>
          </div>
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <Select value={originFilter} onValueChange={setOriginFilter}>
              <SelectTrigger size="sm" className="w-36" aria-label="按来源和原状态筛选"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL_ORIGINS}>全部来源</SelectItem>
                <SelectItem value={ORIGIN_IMPORTED}>导入</SelectItem>
                <SelectItem value={ORIGIN_POOL}>账号池移入</SelectItem>
                <SelectSeparator />
                {ORIGIN_REASONS.map((reason) => (
                  <SelectItem key={reason} value={reason}>移入·{disabledReasonLabel(reason)}</SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={groupFilter} onValueChange={setGroupFilter}>
              <SelectTrigger size="sm" className="w-36" aria-label="按分组筛选"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL_GROUPS}>全部分组</SelectItem>
                {namedGroupItems.length > 0 && <SelectSeparator />}
                {namedGroupItems.map((group) => (
                  <SelectItem key={group.name!} value={group.name!}>{group.name}（{group.total}）</SelectItem>
                ))}
                {ungroupedItem && <SelectSeparator />}
                {ungroupedItem && <SelectItem value={UNGROUPED}>未分组（{ungroupedItem.total}）</SelectItem>}
              </SelectContent>
            </Select>
            <Select value={subscriptionFilter} onValueChange={setSubscriptionFilter}>
              <SelectTrigger size="sm" className="w-28" aria-label="按订阅筛选"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>全部订阅</SelectItem>
                {CREDENTIAL_SUBSCRIPTION_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
                <SelectItem value="unknown">未知订阅</SelectItem>
              </SelectContent>
            </Select>
            <Select value={quotaFilter} onValueChange={setQuotaFilter}>
              <SelectTrigger size="sm" className="w-28" aria-label="按额度筛选"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>全部额度</SelectItem>
                {Object.entries(QUOTA_LABEL).map(([value, label]) => <SelectItem key={value} value={value}>{label}</SelectItem>)}
              </SelectContent>
            </Select>
            <Select value={proxyFilter} onValueChange={setProxyFilter}>
              <SelectTrigger size="sm" className="w-32" aria-label="按代理筛选"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>全部代理</SelectItem>
                <SelectItem value="with">有代理</SelectItem>
                <SelectItem value="without">无代理</SelectItem>
                {proxyOptions.length > 0 && <SelectSeparator />}
                {proxyOptions.map((resource) => <SelectItem key={resource.id} value={String(resource.id)}>{resource.name}</SelectItem>)}
              </SelectContent>
            </Select>
            <Select value={authFilter} onValueChange={setAuthFilter}>
              <SelectTrigger size="sm" className="w-28" aria-label="按认证方式筛选"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>全部认证</SelectItem>
                {(summary?.authKinds ?? []).map((kind) => <SelectItem key={kind} value={kind}>{authLabel(kind)}</SelectItem>)}
              </SelectContent>
            </Select>
            <div className="flex items-center gap-1">
              <Select value={sort} onValueChange={setSort}>
                <SelectTrigger size="sm" className="w-28" aria-label="排序"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {SORT_OPTIONS.map(([value, label]) => <SelectItem key={value} value={value}>{label}</SelectItem>)}
                </SelectContent>
              </Select>
              {sort !== '__default__' && (
                <Button variant="outline" size="icon-xs" onClick={() => setOrder(order === 'desc' ? 'asc' : 'desc')} title={order === 'desc' ? '从大到小 / 从新到旧' : '从小到大 / 从旧到新'} aria-label="切换排序方向">
                  {order === 'desc' ? <ArrowDownWideNarrow /> : <ArrowUpNarrowWide />}
                </Button>
              )}
            </div>
          </div>
          <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <Checkbox
                checked={Boolean(allFiltered) || pageAllSelected}
                onCheckedChange={(value) => { setAllFiltered(null); setSelectedIds(value === true ? new Set(selectable.map((item) => item.id)) : new Set()) }}
                disabled={selectable.length === 0}
                aria-label="全选当前页可操作的账号"
              />
              <span className="text-xs text-muted-foreground">
                {allFiltered
                  ? `已选筛选结果全部 ${allFiltered.ids.length} 个${allFiltered.truncated ? `（共 ${allFiltered.total} 个，一次最多 ${allFiltered.ids.length} 个）` : ''}`
                  : selectedItems.length > 0 ? `已选 ${selectedItems.length} 个` : '全选当前页'}
              </span>
              {!allFiltered && pageAllSelected && (inventory.data?.total ?? 0) > selectable.length && (
                <Button variant="link" size="xs" className="h-auto px-0" onClick={() => void selectAllFiltered()} disabled={selectingAll}>
                  {selectingAll ? '读取中…' : '选择全部筛选结果'}
                </Button>
              )}
              {inventory.isFetching && <Spinner size="sm" />}
            </div>
            {target.ids.length > 0 && (
              <div className="flex flex-wrap items-center gap-1.5">
                <Button size="xs" onClick={() => setPushTarget(target)}><Send />推送到账号池</Button>
                <Button variant="outline" size="xs" onClick={() => runBatch('check')} disabled={batch.state?.running}><Wallet />查询额度</Button>
                <Button variant="outline" size="xs" onClick={() => runBatch('test')} disabled={batch.state?.running} title="逐个真实调用模型，会消耗少量额度"><FlaskConical />验活</Button>
                <Button variant="outline" size="xs" onClick={() => setGroupTarget(target.ids)}><FolderInput />设置分组</Button>
                <Button variant="outline" size="xs" onClick={() => setProxyTarget(target.ids)}><Router />设置代理</Button>
                <Button variant="outline" size="xs" className="text-destructive hover:bg-destructive/10" onClick={() => void runDelete(target.ids, ` ${target.ids.length} 个账号`)} disabled={remove.isPending}>
                  <Trash2 />删除
                </Button>
                <Button variant="ghost" size="xs" onClick={clearSelection}>取消</Button>
              </div>
            )}
          </div>
          {batch.state && <BatchProgressPanel state={batch.state} onCancel={batch.cancel} onResume={batch.resume} onDismiss={batch.dismiss} />}
        </div>

        <div ref={listTopRef} className="scroll-mt-[calc(var(--header-h)+8rem)] pt-3">
          {items.length === 0 ? (
            <EmptyState
              icon={<Archive className="h-12 w-12" />}
              title={filtered ? '没有符合条件的账号' : '库存是空的'}
              description="库存用来保管暂不使用的账号：自动续期，不参与调度，需要时推送到账号池。账号页的禁用账号也可以一键移入库存。"
              action={<Button size="sm" onClick={() => setImportOpen(true)}><Upload />导入到库存</Button>}
            />
          ) : (
            <div className="grid items-stretch gap-3 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4 3xl:grid-cols-5 4xl:grid-cols-6">
              {items.map((item) => (
                <InventoryCard
                  key={item.id}
                  item={item}
                  selected={Boolean(allFiltered?.ids.includes(item.id)) || selectedIds.has(item.id)}
                  busy={busy.get(item.id)}
                  onToggle={() => toggle(item.id)}
                  onEdit={() => setEditing(item)}
                  onCheck={() => void checkOne(item)}
                  onTest={() => void testOne(item)}
                  onPush={() => setPushTarget({ ids: [item.id], items: [item] })}
                  onSetGroup={() => setGroupTarget([item.id])}
                  onSetProxy={() => setProxyTarget([item.id])}
                  onExport={() => setExportIds([item.id])}
                  onDelete={() => void runDelete([item.id], `账号 #${item.id}${item.email ? `（${item.email}）` : ''}`)}
                />
              ))}
            </div>
          )}
        </div>

        {(inventory.data?.total ?? 0) > 0 && (
          <div className="z-10 -mx-4 mt-3 border-t bg-card px-4 py-3 lg:sticky lg:bottom-0">
            <Pagination
              page={page}
              pageCount={inventory.data?.totalPages ?? 1}
              total={inventory.data?.total ?? 0}
              pageSize={pageSize}
              pageSizeOptions={PAGE_SIZE_OPTIONS}
              onPageChange={setPage}
              onPageSizeChange={setPageSize}
              pending={inventory.isFetching && inventory.isPlaceholderData}
            />
          </div>
        )}
      </SectionCard>

      <InventoryImportModal open={importOpen} onClose={() => setImportOpen(false)} />
      <InventoryPushModal
        open={Boolean(pushTarget)}
        ids={pushTarget?.ids ?? []}
        items={pushTarget?.items ?? []}
        onClose={() => setPushTarget(null)}
        onDone={clearSelection}
      />
      <InventoryEditModal item={editing} onClose={() => setEditing(null)} />
      <InventorySetGroupModal open={Boolean(groupTarget)} ids={groupTarget ?? []} onClose={() => setGroupTarget(null)} onDone={clearSelection} />
      <InventoryProxyModal open={Boolean(proxyTarget)} ids={proxyTarget ?? []} onClose={() => setProxyTarget(null)} onDone={clearSelection} />
      <InventoryGroupsModal open={groupsOpen} onClose={() => setGroupsOpen(false)} />
      <InventoryExportModal
        open={Boolean(exportIds)}
        onClose={() => setExportIds(null)}
        selectedIds={exportIds ?? []}
        filter={filter}
        filterLabel={exportFilterLabel}
        filteredCount={inventory.data?.total ?? 0}
      />
    </PageContainer>
  )
}
