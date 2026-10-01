import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import type { RowSelectionState, VisibilityState } from '@tanstack/react-table'
import { Ban, Download, FileUp, MoreHorizontal, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { DataTable } from '@/components/data-table/data-table'
import { Pager } from '@/components/data-table/pager'
import { EmptyState, ErrorState } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { credentialsApi } from '@/api/endpoints/credentials'
import { usePersistedState } from '@/lib/use-persisted-state'
import { cn } from '@/lib/utils'
import { fmtInt } from '@/lib/format'
import { useCredentialSummary } from '@/queries/shared'
import { usePaletteActions } from '@/shell/command-palette'
import { useAccountActions } from './actions'
import { accountColumns } from './columns'
import { AccountSheet } from './detail/account-sheet'
import { BatchEditDialog } from './batch-edit-dialog'
import { ImportWizard } from './import-wizard'
import { AccountsBulkBar, DisabledTriage } from './accounts-toolbars'
import { AccountsFilterBar } from './accounts-filter-bar'
import { searchToQuery, useAccountsPage, type AccountRow } from './queries'
import { STATUS_SEGMENTS, type AccountsSearch } from './search'
import { AccountCards } from './account-cards'

const route = getRouteApi('/accounts')

export function AccountsPage() {
  const search = route.useSearch()
  const navigate = route.useNavigate()
  const setSearch = (patch: Partial<AccountsSearch>, resetPage = true) =>
    navigate({ search: (prev) => ({ ...prev, ...(resetPage ? { page: undefined } : {}), ...patch }), replace: true })

  const page = useAccountsPage(search)
  const summary = useCredentialSummary()
  const actions = useAccountActions()
  const [selection, setSelection] = useState<RowSelectionState>({})
  const [visibility, setVisibility] = usePersistedState<VisibilityState>('accounts.columns', {})
  const [batchEditOpen, setBatchEditOpen] = useState(false)
  const [qText, setQText] = useState(search.q ?? '')
  const columns = useMemo(() => accountColumns(), [])
  /** 跨页"全选筛选结果"：保存完整的 ID 列表，翻页或改筛选时失效 */
  const [allMatching, setAllMatching] = useState<number[] | null>(null)
  const pageIds = Object.keys(selection)
    .filter((k) => selection[k])
    .map(Number)
  const selectedIds = allMatching ?? pageIds
  const clearSelection = () => {
    setSelection({})
    setAllMatching(null)
  }
  const filterKey = JSON.stringify({ ...search, id: undefined, tab: undefined, import: undefined, view: undefined })
  const [lastFilterKey, setLastFilterKey] = useState(filterKey)
  if (filterKey !== lastFilterKey) {
    setLastFilterKey(filterKey)
    setSelection({})
    setAllMatching(null)
  }
  const selectAllMatching = useMutation({
    mutationFn: async () => {
      const query = searchToQuery(search)
      const ids: number[] = []
      for (let p = 1; ; p++) {
        const res = await credentialsApi.list({ ...query, page: p, limit: 500 })
        ids.push(...res.items.map((i) => i.id))
        if (p >= res.totalPages) break
      }
      const reason = search.status === 'disabled' ? search.reason : undefined
      if (!reason) return ids
      // 禁用原因是前端筛选，需要再取一次带原因的分页数据
      const keep = new Set<number>()
      for (let p = 1; ; p++) {
        const res = await credentialsApi.page({ ...query, page: p, limit: 500 })
        res.credentials.filter((c) => (c.disabledReason ?? 'Manual') === reason).forEach((c) => keep.add(c.id))
        if (p >= res.totalPages) break
      }
      return ids.filter((id) => keep.has(id))
    },
    onSuccess: setAllMatching,
    meta: { error: '获取全部筛选结果失败' },
  })
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

  const viewSearch = Object.fromEntries(
    Object.entries({ ...search, id: undefined, tab: undefined, import: undefined, page: undefined, view: undefined }).filter(
      ([, v]) => v !== undefined,
    ),
  ) as AccountsSearch

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
              <Button
                variant="unstyled"
                size="none"
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
                  <span className={cn('num rounded-full px-1.5 text-xs', active ? 'bg-accent text-accent-foreground' : 'bg-muted')}>
                    {count}
                  </span>
                )}
              </Button>
            )
          })}
        </div>

        {activeStatus === 'disabled' && (
          <DisabledTriage
            reason={search.reason}
            rows={page.rows}
            disabledTotal={summary.data?.disabled}
            onReason={(reason) => setSearch({ reason })}
            onDeletedAll={() => page.refetch()}
          />
        )}

        <AccountsFilterBar
          search={search}
          setSearch={setSearch}
          qText={qText}
          setQText={setQText}
          view={view}
          viewSearch={viewSearch}
          onApplyView={(v) => {
            setQText(v.q ?? '')
            navigate({ search: { ...v, view: search.view }, replace: true })
          }}
          visibility={visibility}
          setVisibility={setVisibility}
        />

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
            onRowSelectionChange={(u) => {
              setAllMatching(null)
              setSelection(u)
            }}
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

      <AccountsBulkBar
        selectedIds={selectedIds}
        clearSelection={clearSelection}
        onEdit={() => setBatchEditOpen(true)}
        extra={
          allMatching ? (
            <span className="text-xs whitespace-nowrap text-primary">（全部筛选结果）</span>
          ) : page.data && pageIds.length === page.rows.length && page.data.filteredTotal > page.rows.length ? (
            <Button size="xs" variant="link" onClick={() => selectAllMatching.mutate()} disabled={selectAllMatching.isPending}>
              选中全部 {page.data.filteredTotal} 条
            </Button>
          ) : null
        }
      />

      <BatchEditDialog ids={selectedIds} open={batchEditOpen} onOpenChange={setBatchEditOpen} onDone={clearSelection} />
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
