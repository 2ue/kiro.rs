import { useEffect, useMemo, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { ArrowUpRight, Archive, Pencil, RefreshCw, Search, Send, Trash2, Upload, X } from 'lucide-react'
import { toast } from 'sonner'
import { Badge, Button, Checkbox, Input, Spinner } from '@/components/ui'
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
} from '@/components/patterns'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { useCheckInventory, useDeleteInventory, useInventory } from '@/hooks/use-inventory'
import { formatFullDate, formatLastUsed } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type { InventoryItem, InventoryStatus } from '@/types/api'
import { pageMeta } from '@/types/ui'
import { QuotaBar } from '@/features/credentials/credential-card'
import { authLabel, credentialSubscriptionLabel, disabledReasonLabel, quotaBarModel } from '@/features/credentials/credential-utils'
import { InventoryEditModal, InventoryImportModal, InventoryPushModal } from './inventory-dialogs'

const PAGE_SIZE_OPTIONS = [10, 20, 30, 50, 100]

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
  const [queryText, setQueryText] = useState('')
  const query = useDebouncedValue(queryText.trim(), 300)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [importOpen, setImportOpen] = useState(false)
  const [pushOpen, setPushOpen] = useState(false)
  const [editing, setEditing] = useState<InventoryItem | null>(null)
  const listTopRef = useRef<HTMLDivElement | null>(null)
  const confirm = useConfirm()

  const listQuery = useMemo(() => ({
    page,
    limit: pageSize,
    status: statusFilter === '__all__' ? undefined : statusFilter,
    q: query || undefined,
  }), [page, pageSize, statusFilter, query])
  const inventory = useInventory(listQuery)
  const check = useCheckInventory()
  const remove = useDeleteInventory()

  useEffect(() => { setPage(1) }, [statusFilter, query, pageSize])
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
            <Button size="sm" onClick={() => setImportOpen(true)}><Upload />导入到库存</Button>
          </>
        }
      />

      <StatGrid>
        <StatCard title="在库" value={counts?.stored ?? 0} desc="自动续期，不参与调度" tone="success" />
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
            <div className="relative min-w-[220px] flex-1 sm:max-w-sm">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden="true" />
              <Input value={queryText} onChange={(e) => setQueryText(e.target.value)} placeholder="邮箱 / #ID / 来源 / 备注" className="h-8 pl-8 text-xs" aria-label="搜索库存" />
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
              title={statusFilter !== '__all__' || query ? '没有符合条件的账号' : '库存是空的'}
              description="库存用来保管暂不使用的账号：自动续期，不参与调度，需要时推送到账号池。"
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
    </PageContainer>
  )
}
