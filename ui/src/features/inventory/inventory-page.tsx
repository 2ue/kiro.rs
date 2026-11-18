import { useEffect, useMemo, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { ArrowUpRight, Archive, Download, FolderCog, FolderInput, Pencil, RefreshCw, Search, Send, Trash2, Upload, X } from 'lucide-react'
import { toast } from 'sonner'
import { Badge, Button, Checkbox, Input, Select, SelectContent, SelectItem, SelectSeparator, SelectTrigger, SelectValue, Spinner } from '@/components/ui'
import {
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
import { useCheckInventory, useDeleteInventory, useInventory, useInventoryGroups } from '@/hooks/use-inventory'
import { formatFullDate, formatLastUsed } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type { InventoryItem, InventoryOrigin, InventoryStatus } from '@/types/api'
import { pageMeta } from '@/types/ui'
import { QuotaBar } from '@/features/credentials/credential-card'
import { authLabel, credentialSubscriptionLabel, disabledReasonLabel, quotaBarModel } from '@/features/credentials/credential-utils'
import {
  InventoryEditModal,
  InventoryExportModal,
  InventoryGroupsModal,
  InventoryImportModal,
  InventoryPushModal,
  InventorySetGroupModal,
} from './inventory-dialogs'

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

function InventoryCard({ item, selected, onToggle, onEdit }: {
  item: InventoryItem
  selected: boolean
  onToggle: () => void
  onEdit: () => void
}) {
  const status = STATUS_META[item.status]
  const selectable = item.status === 'stored' || item.status === 'invalid'
  const quota = item.accountInfo ? quotaBarModel(item.accountInfo) : null
  const error = item.status === 'invalid' ? item.lastRefreshError : item.lastCheckError || item.lastRefreshError
  return (
    <div className={`relative overflow-hidden rounded-xl bg-card shadow-sm transition-shadow hover:shadow-md ${item.status === 'pushed' ? 'opacity-90' : ''}`}>
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
        {selectable && (
          <Button type="button" variant="ghost" size="icon-xs" onClick={onEdit} title="编辑区域、代理和备注" aria-label="编辑">
            <Pencil />
          </Button>
        )}
      </div>

      <div className="space-y-2 px-3 pb-3">
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
              <span className="tabular font-medium">
                {item.accountInfo ? `${item.accountInfo.currentUsage.toFixed(2)} / ${item.accountInfo.usageLimit}` : '未查询'}
              </span>
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
    </div>
  )
}

export function InventoryPage() {
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [statusFilter, setStatusFilter] = useState<'__all__' | InventoryStatus>('__all__')
  const [groupFilter, setGroupFilter] = useState<string>(ALL_GROUPS)
  const [originFilter, setOriginFilter] = useState<string>(ALL_ORIGINS)
  const [queryText, setQueryText] = useState('')
  const query = useDebouncedValue(queryText.trim(), 300)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [importOpen, setImportOpen] = useState(false)
  const [pushOpen, setPushOpen] = useState(false)
  const [editing, setEditing] = useState<InventoryItem | null>(null)
  const [setGroupOpen, setSetGroupOpen] = useState(false)
  const [groupsOpen, setGroupsOpen] = useState(false)
  const [exportOpen, setExportOpen] = useState(false)
  const groups = useInventoryGroups()
  const listTopRef = useRef<HTMLDivElement | null>(null)
  const confirm = useConfirm()

  const listQuery = useMemo(() => ({
    page,
    limit: pageSize,
    status: statusFilter === '__all__' ? undefined : statusFilter,
    q: query || undefined,
    group: groupFilter === ALL_GROUPS ? undefined : groupFilter,
    origin: originFilter === ALL_ORIGINS ? undefined : originFilter,
  }), [page, pageSize, statusFilter, query, groupFilter, originFilter])
  const inventory = useInventory(listQuery)
  const check = useCheckInventory()
  const remove = useDeleteInventory()

  useEffect(() => { setPage(1) }, [statusFilter, query, pageSize, groupFilter, originFilter])
  // A dissolved or renamed group is no longer a valid filter.
  useEffect(() => {
    if (groupFilter === ALL_GROUPS || groupFilter === UNGROUPED || !groups.data) return
    if (!groups.data.groups.some((group) => group.name === groupFilter)) setGroupFilter(ALL_GROUPS)
  }, [groups.data, groupFilter])
  useEffect(() => { setSelectedIds(new Set()) }, [listQuery])
  useEffect(() => {
    const top = listTopRef.current
    if (top && top.getBoundingClientRect().top < 0) top.scrollIntoView({ block: 'start' })
  }, [listQuery])

  const items = inventory.data?.items ?? []
  const counts = inventory.data?.counts
  const selectable = items.filter((item) => item.status === 'stored' || item.status === 'invalid')
  const selectedItems = selectable.filter((item) => selectedIds.has(item.id))
  const toggle = (id: number) => setSelectedIds((current) => {
    const next = new Set(current)
    if (next.has(id)) next.delete(id)
    else next.add(id)
    return next
  })

  const runCheck = async () => {
    try {
      const result = await check.mutateAsync(selectedItems.map((item) => item.id))
      if (result.failed === 0) toast.success(`已查询 ${result.succeeded} 个账号`)
      else toast.warning(`查询成功 ${result.succeeded}，失败 ${result.failed}`)
    } catch (e) {
      toast.error(`查询失败：${extractErrorMessage(e)}`)
    }
  }

  const runDelete = async () => {
    const ok = await confirm({
      title: '删除库存账号',
      message: `将从库存删除 ${selectedItems.length} 个账号，账号数据一并清除，无法恢复。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!ok) return
    try {
      const result = await remove.mutateAsync(selectedItems.map((item) => item.id))
      toast.success(`已删除 ${result.succeeded} 个账号`)
      setSelectedIds(new Set())
    } catch (e) {
      toast.error(`删除失败：${extractErrorMessage(e)}`)
    }
  }

  const groupItems = groups.data?.groups ?? []
  const namedGroupItems = groupItems.filter((group) => group.name)
  const ungroupedItem = groupItems.find((group) => !group.name)
  const exportFilter = {
    status: statusFilter === '__all__' ? undefined : statusFilter,
    group: groupFilter === ALL_GROUPS ? undefined : groupFilter,
    origin: originFilter === ALL_ORIGINS ? undefined : originFilter,
    q: query || undefined,
  }
  const exportFilterLabel = [
    statusFilter !== '__all__' ? STATUS_META[statusFilter].label : null,
    groupFilter === UNGROUPED ? '未分组' : groupFilter !== ALL_GROUPS ? `分组「${groupFilter}」` : null,
    originFilter === ORIGIN_POOL ? '账号池移入' : originFilter === ORIGIN_IMPORTED ? '导入' : originFilter !== ALL_ORIGINS ? `原状态「${disabledReasonLabel(originFilter)}」` : null,
    query ? `搜索“${query}”` : null,
  ].filter(Boolean).join(' · ')

  const renewal = inventory.data?.renewal
  const meta = pageMeta.inventory
  if (inventory.isLoading && !inventory.data) return <LoadingState text="加载库存..." />
  if (inventory.error) return <ErrorState message={extractErrorMessage(inventory.error)} />

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
            <Button variant="outline" size="sm" onClick={() => setExportOpen(true)} disabled={(counts?.total ?? 0) === 0}><Download /><span className="hidden sm:inline">导出</span></Button>
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
        <StatCard title="已入池" value={counts?.pushed ?? 0} desc="由账号池接管" tone="info" />
        <StatCard title="失效" value={counts?.invalid ?? 0} desc="refreshToken 被拒，已停止续期" tone="error" />
        <StatCard title="合计" value={counts?.total ?? 0} desc={counts?.pushing ? `推送中 ${counts.pushing}` : '库存全部记录'} />
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
            <Select value={originFilter} onValueChange={setOriginFilter}>
              <SelectTrigger size="sm" className="w-40" aria-label="按来源和原状态筛选"><SelectValue /></SelectTrigger>
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
              <SelectTrigger size="sm" className="w-44" aria-label="按分组筛选"><SelectValue /></SelectTrigger>
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
          <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <Checkbox
                checked={selectable.length > 0 && selectedItems.length === selectable.length}
                onCheckedChange={(value) => setSelectedIds(value === true ? new Set(selectable.map((item) => item.id)) : new Set())}
                disabled={selectable.length === 0}
                aria-label="全选当前页可操作的账号"
              />
              <span className="text-xs text-muted-foreground">{selectedItems.length > 0 ? `已选 ${selectedItems.length} 个` : '全选当前页'}</span>
              {inventory.isFetching && <Spinner size="sm" />}
            </div>
            {selectedItems.length > 0 && (
              <div className="flex flex-wrap items-center gap-1.5">
                <Button size="xs" onClick={() => setPushOpen(true)}><Send />推送到账号池</Button>
                <Button variant="outline" size="xs" onClick={() => setSetGroupOpen(true)}><FolderInput />设置分组</Button>
                <Button variant="outline" size="xs" onClick={() => void runCheck()} disabled={check.isPending}>
                  {check.isPending ? <Spinner size="sm" /> : <RefreshCw />}查询额度
                </Button>
                <Button variant="outline" size="xs" className="text-destructive hover:bg-destructive/10" onClick={() => void runDelete()} disabled={remove.isPending}>
                  <Trash2 />删除
                </Button>
                <Button variant="ghost" size="xs" onClick={() => setSelectedIds(new Set())}>取消</Button>
              </div>
            )}
          </div>
        </div>

        <div ref={listTopRef} className="scroll-mt-[calc(var(--header-h)+8rem)] pt-3">
          {items.length === 0 ? (
            <EmptyState
              icon={<Archive className="h-12 w-12" />}
              title={statusFilter !== '__all__' || query || groupFilter !== ALL_GROUPS || originFilter !== ALL_ORIGINS ? '没有符合条件的账号' : '库存是空的'}
              description="库存用来保管暂不使用的账号：自动续期，不参与调度，需要时推送到账号池。账号页的禁用账号也可以一键移入库存。"
              action={<Button size="sm" onClick={() => setImportOpen(true)}><Upload />导入到库存</Button>}
            />
          ) : (
            <div className="grid items-start gap-3 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4 3xl:grid-cols-5 4xl:grid-cols-6">
              {items.map((item) => (
                <InventoryCard key={item.id} item={item} selected={selectedIds.has(item.id)} onToggle={() => toggle(item.id)} onEdit={() => setEditing(item)} />
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
      <InventoryPushModal open={pushOpen} items={selectedItems} onClose={() => setPushOpen(false)} onDone={() => setSelectedIds(new Set())} />
      <InventoryEditModal item={editing} onClose={() => setEditing(null)} />
      <InventorySetGroupModal open={setGroupOpen} ids={selectedItems.map((item) => item.id)} onClose={() => setSetGroupOpen(false)} onDone={() => setSelectedIds(new Set())} />
      <InventoryGroupsModal open={groupsOpen} onClose={() => setGroupsOpen(false)} />
      <InventoryExportModal
        open={exportOpen}
        onClose={() => setExportOpen(false)}
        selectedIds={selectedItems.map((item) => item.id)}
        filter={exportFilter}
        filterLabel={exportFilterLabel}
        filteredCount={inventory.data?.total ?? 0}
      />
    </PageContainer>
  )
}
