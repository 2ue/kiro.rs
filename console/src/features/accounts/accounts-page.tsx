import { useMemo, useState } from 'react'
import { getRouteApi } from '@tanstack/react-router'
import type { RowSelectionState, VisibilityState } from '@tanstack/react-table'
import {
  Ban,
  CirclePlay,
  Download,
  FileUp,
  ListFilter,
  MoreHorizontal,
  Pencil,
  RefreshCw,
  Search,
  Stethoscope,
  Trash2,
  X,
} from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { DataTable } from '@/components/data-table/data-table'
import { Pager } from '@/components/data-table/pager'
import { BulkBar } from '@/components/data-table/selection'
import { ColumnToggle } from '@/components/data-table/column-toggle'
import { EmptyState, ErrorState } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { SelectControl } from '@/components/patterns/fields'
import { useConfirm } from '@/components/patterns/confirm'
import { credentialsApi } from '@/api/endpoints/credentials'
import { DISABLED_REASONS, DISABLED_REASON_CODES } from '@/domain/disabled-reason'
import { usePersistedState } from '@/lib/use-persisted-state'
import { cn } from '@/lib/utils'
import { fmtInt } from '@/lib/format'
import { useCredentialSummary, useProxies } from '@/queries/shared'
import { usePaletteActions } from '@/shell/command-palette'
import { useAccountActions } from './actions'
import { ACCOUNT_COLUMN_LABELS, accountColumns } from './columns'
import { AccountSheet } from './detail/account-sheet'
import { BatchEditDialog } from './batch-edit-dialog'
import { ImportWizard } from './import-wizard'
import { useAccountsPage, type AccountRow } from './queries'
import { STATUS_SEGMENTS, type AccountsSearch } from './search'
import { AccountCards } from './account-cards'

const route = getRouteApi('/accounts')

const SORT_OPTIONS: Array<{ value: NonNullable<AccountsSearch['sort']>; label: string }> = [
  { value: 'default', label: '默认排序' },
  { value: 'priority', label: '优先级' },
  { value: 'usage_percentage', label: '额度使用率' },
  { value: 'remaining_quota', label: '剩余额度' },
  { value: 'in_flight_requests', label: '在途请求' },
  { value: 'scheduler_score', label: '调度评分' },
  { value: 'failure_count', label: '失败次数' },
  { value: 'estimated_cost', label: '估算费用' },
  { value: 'last_used_at', label: '最近使用' },
  { value: 'created_at', label: '创建时间' },
  { value: 'id', label: 'ID' },
]

export function AccountsPage() {
  const search = route.useSearch()
  const navigate = route.useNavigate()
  const setSearch = (patch: Partial<AccountsSearch>, resetPage = true) =>
    navigate({ search: (prev) => ({ ...prev, ...(resetPage ? { page: undefined } : {}), ...patch }), replace: true })

  const page = useAccountsPage(search)
  const summary = useCredentialSummary()
  const proxies = useProxies()
  const actions = useAccountActions()
  const confirm = useConfirm()
  const [selection, setSelection] = useState<RowSelectionState>({})
  const [visibility, setVisibility] = usePersistedState<VisibilityState>('accounts.columns', {})
  const [batchEditOpen, setBatchEditOpen] = useState(false)
  const [qText, setQText] = useState(search.q ?? '')
  const columns = useMemo(() => accountColumns(), [])
  const selectedIds = Object.keys(selection).filter((k) => selection[k]).map(Number)
  const view = search.view ?? 'table'
  const activeStatus = search.status ?? 'all'

  usePaletteActions(
    [
      { id: 'accounts.import', group: 'Kiro 账号', label: '导入账号', icon: <FileUp />, run: () => setSearch({ import: true }, false) },
      {
        id: 'accounts.refresh-all',
        group: 'Kiro 账号',
        label: '刷新当前页全部账号额度',
        icon: <RefreshCw />,
        run: () => actions.refreshInfo.mutate(page.rows.map((r) => r.id)),
      },
      { id: 'accounts.disabled', group: 'Kiro 账号', label: '查看已禁用账号', icon: <Ban />, run: () => setSearch({ status: 'disabled' }) },
    ],
    [page.rows],
  )

  const segmentCount = (key: string): number | undefined => {
    const s = summary.data
    if (!s) return undefined
    switch (key) {
      case 'all':
        return s.total
      case 'enabled':
        return s.available
      case 'cooldown':
        return s.coolingDown
      case 'error':
        return s.failing
      case 'disabled':
        return s.disabled
      default:
        return undefined
    }
  }

  const deleteByReason = async (reason?: string) => {
    if (!reason) {
      const ok = await confirm({
        title: `删除全部 ${summary.data?.disabled ?? ''} 个已禁用账号？`,
        description: '会删除所有处于禁用状态的账号（包括手动禁用）。操作不可撤销，建议先导出备份。',
        confirmText: '全部删除',
        destructive: true,
        typeToConfirm: '删除',
      })
      if (!ok) return
      const res = await credentialsApi.removeDisabled()
      toast.success(`已删除 ${res.success} 个账号${res.failed ? `，${res.failed} 个失败` : ''}`)
      page.refetch()
      return
    }
    // 按原因删除：只作用于当前页筛选结果
    const ids = page.rows.map((r) => r.id)
    await actions.confirmRemove(ids, `原因为「${DISABLED_REASONS[reason as keyof typeof DISABLED_REASONS]?.label ?? reason}」的 ${ids.length} 个账号`)
  }

  const openRow = (r: AccountRow) => navigate({ search: (prev) => ({ ...prev, id: r.id, tab: undefined }) })

  return (
    <Page>
      <PageHeader
        title="Kiro 账号"
        description={
          summary.data ? (
            <span className="num">
              共 {fmtInt(summary.data.total)} 个 · 可调度 {fmtInt(summary.data.schedulable)} · 在用 {fmtInt(summary.data.inUse)} · 冷却{' '}
              {fmtInt(summary.data.coolingDown)} · 异常 {fmtInt(summary.data.failing)}
            </span>
          ) : (
            '账号状态、额度、导入与批量处置'
          )
        }
        actions={
          <>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="sm">
                  <MoreHorizontal /> 更多
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-52">
                <DropdownMenuItem onSelect={() => actions.refreshInfo.mutate(page.rows.map((r) => r.id))}>
                  <RefreshCw /> 刷新当前页额度
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => actions.exportCredentials.mutate({ format: 'backup-json' })}>
                  <Download /> 导出全部（备份 JSON）
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => actions.exportCredentials.mutate({ format: 'jsonl' })}>
                  <Download /> 导出全部（JSONL）
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
            <Button size="sm" onClick={() => setSearch({ import: true }, false)}>
              <FileUp /> 导入账号
            </Button>
          </>
        }
      />

      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-1 border-b">
          {STATUS_SEGMENTS.map((seg) => {
            const count = segmentCount(seg.key)
            const active = activeStatus === seg.key
            return (
              <button
                key={seg.key}
                type="button"
                onClick={() => setSearch({ status: seg.key === 'all' ? undefined : seg.key, reason: undefined })}
                className={cn(
                  '-mb-px flex items-center gap-1.5 border-b-2 px-3 py-2 text-sm transition-colors',
                  active ? 'border-primary font-medium text-foreground' : 'border-transparent text-muted-foreground hover:text-foreground',
                )}
              >
                {seg.label}
                {typeof count === 'number' && (
                  <span className={cn('num rounded-full px-1.5 text-xs', active ? 'bg-accent text-accent-foreground' : 'bg-muted')}>{count}</span>
                )}
              </button>
            )
          })}
        </div>

        {activeStatus === 'disabled' && (
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="mr-1 text-xs text-muted-foreground">禁用原因</span>
            <Button size="xs" variant={!search.reason ? 'secondary' : 'ghost'} onClick={() => setSearch({ reason: undefined })}>
              全部
            </Button>
            {DISABLED_REASON_CODES.map((code) => (
              <Button key={code} size="xs" variant={search.reason === code ? 'secondary' : 'ghost'} onClick={() => setSearch({ reason: code })}>
                {DISABLED_REASONS[code].label}
              </Button>
            ))}
            <div className="ml-auto">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button size="xs" variant="outline">
                    按原因处置
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-64">
                  <DropdownMenuLabel>当前筛选（{page.rows.length} 个）</DropdownMenuLabel>
                  {search.reason && DISABLED_REASONS[search.reason as keyof typeof DISABLED_REASONS] ? (
                    <>
                      <DropdownMenuItem onSelect={() => actions.toggleDisabled(page.rows.map((r) => r.id), false)}>
                        <CirclePlay /> 全部重新启用
                      </DropdownMenuItem>
                      <DropdownMenuItem onSelect={() => actions.resetAndCheck(page.rows.map((r) => r.id))}>
                        <Stethoscope /> 重置失败计数并体检
                      </DropdownMenuItem>
                      <DropdownMenuItem variant="destructive" onSelect={() => deleteByReason(search.reason)}>
                        <Trash2 /> 删除当前页这些账号
                      </DropdownMenuItem>
                    </>
                  ) : (
                    <DropdownMenuItem variant="destructive" onSelect={() => deleteByReason()}>
                      <Trash2 /> 删除全部已禁用账号
                    </DropdownMenuItem>
                  )}
                  <DropdownMenuSeparator />
                  <p className="px-2 py-1.5 text-xs text-muted-foreground">
                    {search.reason ? DISABLED_REASONS[search.reason as keyof typeof DISABLED_REASONS]?.hint : '先选择一个禁用原因以执行对应处置'}
                  </p>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
        )}

        <div className="flex flex-wrap items-center gap-2">
          <form
            className="w-full sm:w-72"
            onSubmit={(e) => {
              e.preventDefault()
              setSearch({ q: qText.trim() || undefined })
            }}
          >
            <InputGroup>
              <InputGroupAddon>
                <Search />
              </InputGroupAddon>
              <InputGroupInput placeholder="邮箱、#ID、标签…" value={qText} onChange={(e) => setQText(e.target.value)} onBlur={() => qText !== (search.q ?? '') && setSearch({ q: qText.trim() || undefined })} />
              {qText && (
                <InputGroupAddon align="inline-end">
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    aria-label="清除搜索"
                    onClick={() => {
                      setQText('')
                      setSearch({ q: undefined })
                    }}
                  >
                    <X />
                  </Button>
                </InputGroupAddon>
              )}
            </InputGroup>
          </form>
          <FilterSelect
            value={search.auth}
            placeholder="认证方式"
            onChange={(v) => setSearch({ auth: v })}
            options={[
              { value: 'social', label: 'Social' },
              { value: 'idc', label: 'IdC' },
              { value: 'external_idp', label: '外部 IdP' },
              { value: 'api_key', label: 'API Key' },
            ]}
          />
          <FilterSelect
            value={search.sub}
            placeholder="订阅"
            onChange={(v) => setSearch({ sub: v })}
            options={[
              { value: 'power', label: 'Power' },
              { value: 'pro_max', label: 'Pro Max' },
              { value: 'pro_plus', label: 'Pro+' },
              { value: 'pro', label: 'Pro' },
              { value: 'free', label: 'Free' },
              { value: 'unknown', label: '未知' },
            ]}
          />
          <FilterSelect
            value={search.proxy !== undefined ? String(search.proxy) : undefined}
            placeholder="代理资源"
            onChange={(v) => setSearch({ proxy: v ? Number(v) : undefined })}
            options={(proxies.data?.resources ?? []).map((p) => ({ value: String(p.id), label: p.name }))}
          />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="sm" className={cn(search.status && !STATUS_SEGMENTS.some((s) => s.key === search.status) && 'text-primary')}>
                <ListFilter /> 更多筛选
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent className="w-52">
              <DropdownMenuItem onSelect={() => setSearch({ status: 'proxy_blocked' })}>代理不可用</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setSearch({ status: 'custom_scheduling' })}>自定义了调度参数</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setSearch({ status: 'unknown_subscription' })}>订阅未知</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <div className="ml-auto flex items-center gap-2">
            <SelectControl
              className="h-7 w-32"
              value={search.sort ?? 'default'}
              onChange={(v) => setSearch({ sort: v === 'default' ? undefined : v })}
              options={SORT_OPTIONS}
            />
            <Button variant="ghost" size="icon-sm" onClick={() => setSearch({ order: (search.order ?? 'desc') === 'desc' ? 'asc' : 'desc' })} aria-label="切换排序方向">
              <span className="text-xs">{(search.order ?? 'desc') === 'desc' ? '↓' : '↑'}</span>
            </Button>
            {view === 'table' && <ColumnToggle columns={ACCOUNT_COLUMN_LABELS} visibility={visibility} onChange={setVisibility} />}
            <ToggleGroup type="single" size="sm" variant="outline" value={view} onValueChange={(v) => v && setSearch({ view: v === 'table' ? undefined : (v as 'cards') }, false)}>
              <ToggleGroupItem value="table" aria-label="表格视图">
                表格
              </ToggleGroupItem>
              <ToggleGroupItem value="cards" aria-label="卡片视图">
                卡片
              </ToggleGroupItem>
            </ToggleGroup>
          </div>
        </div>

        {page.error && !page.data ? (
          <ErrorState error={page.error} onRetry={() => page.refetch()} />
        ) : view === 'cards' ? (
          <AccountCards rows={page.rows} loading={page.isLoading} onOpen={openRow} activeId={search.id} />
        ) : (
          <DataTable
            data={page.rows}
            columns={columns}
            getRowId={(r) => String(r.id)}
            onRowClick={openRow}
            activeRowId={search.id !== undefined ? String(search.id) : undefined}
            rowSelection={selection}
            onRowSelectionChange={setSelection}
            columnVisibility={visibility}
            onColumnVisibilityChange={setVisibility}
            loading={page.isFetching && !page.isLoading}
            rowClassName={(r) => (r.disabled ? 'text-muted-foreground' : undefined)}
            empty={
              page.isLoading ? (
                <div className="p-6 text-center text-sm text-muted-foreground">加载中…</div>
              ) : (
                <EmptyState
                  title="没有匹配的账号"
                  description={search.q || search.status ? '调整筛选条件试试' : '还没有导入任何账号'}
                  action={
                    !search.q && !search.status ? (
                      <Button size="sm" onClick={() => setSearch({ import: true }, false)}>
                        <FileUp /> 导入账号
                      </Button>
                    ) : undefined
                  }
                />
              )
            }
          />
        )}

        {page.data && (
          <Pager
            page={page.data.page}
            totalPages={page.data.totalPages}
            total={page.data.filteredTotal}
            pageSize={page.data.limit}
            pageSizes={[20, 50, 100, 200]}
            onPageChange={(p) => setSearch({ page: p }, false)}
            onPageSizeChange={(size) => setSearch({ size })}
          />
        )}
      </div>

      <BulkBar count={selectedIds.length} onClear={() => setSelection({})}>
        <Button size="sm" variant="ghost" onClick={() => actions.toggleDisabled(selectedIds, false).then(() => setSelection({}))}>
          <CirclePlay /> 启用
        </Button>
        <Button size="sm" variant="ghost" onClick={() => actions.toggleDisabled(selectedIds, true).then(() => setSelection({}))}>
          <Ban /> 禁用
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={async () => {
            const res = await actions.validate.mutateAsync(selectedIds)
            toast.success(`体检完成：成功 ${res.success}，失败 ${res.failed}，升级 ${res.upgraded}，降级 ${res.downgraded}`)
          }}
          disabled={actions.validate.isPending}
        >
          <Stethoscope /> 体检
        </Button>
        <Button size="sm" variant="ghost" onClick={() => actions.refreshInfo.mutate(selectedIds)} disabled={actions.refreshInfo.isPending}>
          <RefreshCw /> 刷新额度
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setBatchEditOpen(true)}>
          <Pencil /> 修改
        </Button>
        <Button size="sm" variant="ghost" onClick={() => actions.exportCredentials.mutate({ format: 'backup-json', ids: selectedIds })}>
          <Download /> 导出
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="text-danger"
          onClick={async () => {
            if (await actions.confirmRemove(selectedIds)) setSelection({})
          }}
        >
          <Trash2 /> 删除
        </Button>
      </BulkBar>

      <BatchEditDialog ids={selectedIds} open={batchEditOpen} onOpenChange={setBatchEditOpen} onDone={() => setSelection({})} />
      <ImportWizard open={!!search.import} onOpenChange={(open) => !open && setSearch({ import: undefined }, false)} />
      <AccountSheet
        id={search.id}
        tab={search.tab}
        onTabChange={(tab) => navigate({ search: (prev) => ({ ...prev, tab }), replace: true })}
        onClose={() => navigate({ search: (prev) => ({ ...prev, id: undefined, tab: undefined }), replace: true })}
      />
    </Page>
  )
}

function FilterSelect({
  value,
  placeholder,
  options,
  onChange,
}: {
  value: string | undefined
  placeholder: string
  options: Array<{ value: string; label: string }>
  onChange: (v: string | undefined) => void
}) {
  return (
    <SelectControl
      className={cn('h-7 w-auto min-w-24', value && 'border-primary/40 text-primary')}
      value={value ?? '__all'}
      onChange={(v) => onChange(v === '__all' ? undefined : v)}
      options={[{ value: '__all', label: `${placeholder}：全部` }, ...options]}
    />
  )
}
