import { useMemo, useState } from 'react'
import { useInfiniteQuery } from '@tanstack/react-query'
import { getRouteApi } from '@tanstack/react-router'
import type { VisibilityState } from '@tanstack/react-table'
import { Eraser, Loader2, Search, X } from 'lucide-react'
import { usageApi } from '@/api/endpoints/usage'
import type { UsageRecordsPageQuery } from '@/api/types'
import { Button } from '@/components/ui/button'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { Input } from '@/components/ui/input'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { Label } from '@/components/ui/label'
import { DataTable } from '@/components/data-table/data-table'
import { ColumnToggle } from '@/components/data-table/column-toggle'
import { EmptyState, ErrorState } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { SelectControl } from '@/components/patterns/fields'
import { usePersistedState } from '@/lib/use-persisted-state'
import { usePollInterval } from '@/stores/ui-prefs'
import { usePoolsList } from '@/queries/shared'
import { cn } from '@/lib/utils'
import { REQUEST_COLUMN_LABELS, requestColumns } from './columns'
import { RequestSheet } from './request-sheet'
import type { RequestsSearch } from './search'
import { CleanupDialog } from './cleanup-dialog'

const route = getRouteApi('/requests')
const PAGE_SIZE = 100

const TIME_PRESETS = [
  { value: 'all', label: '全部时间' },
  { value: '15m', label: '最近 15 分钟' },
  { value: '1h', label: '最近 1 小时' },
  { value: '24h', label: '最近 24 小时' },
  { value: '7d', label: '最近 7 天' },
] as const

function presetSince(preset: string): string | undefined {
  const ms = { '15m': 15 * 60e3, '1h': 3600e3, '24h': 86400e3, '7d': 7 * 86400e3 }[preset]
  return ms ? new Date(Date.now() - ms).toISOString() : undefined
}

function toQuery(s: RequestsSearch): Omit<UsageRecordsPageQuery, 'page' | 'limit'> {
  const q = s.q?.trim()
  return {
    q: q || undefined,
    status: s.status,
    routeKind: s.route,
    model: s.model,
    endpoint: s.endpoint,
    credentialId: s.credentialId,
    externalPoolId: s.poolId,
    requestApiKeyId: s.keyId,
    conversationId: s.conversationId,
    stream: s.stream,
    minFirstTokenLatencyMs: s.minTtft,
    minCacheRead: s.minCacheRead,
    since: s.since ? presetSince(s.since) ?? s.since : undefined,
    until: s.until,
  }
}

export function RequestsPage() {
  const search = route.useSearch()
  const navigate = route.useNavigate()
  const setSearch = (patch: Partial<RequestsSearch>) => navigate({ search: (prev) => ({ ...prev, ...patch }), replace: true })
  const [qText, setQText] = useState(search.q ?? '')
  const [visibility, setVisibility] = usePersistedState<VisibilityState>('requests.columns', { endpoint: false, key: false })
  const [cleanupOpen, setCleanupOpen] = useState(false)
  const pools = usePoolsList()
  const interval = usePollInterval('normal')
  const filters = toQuery(search)

  const q = useInfiniteQuery({
    queryKey: ['usage', 'records-infinite', filters],
    queryFn: ({ pageParam }) => usageApi.records({ ...filters, page: pageParam, limit: PAGE_SIZE }),
    initialPageParam: 1,
    getNextPageParam: (last) => (last.hasNext ? last.page + 1 : undefined),
    // 只在第一页时自动刷新，避免已加载的多页被反复重拉
    refetchInterval: (query) => ((query.state.data?.pages.length ?? 0) <= 1 ? interval : false),
  })
  const records = useMemo(() => q.data?.pages.flatMap((p) => p.records) ?? [], [q.data])
  const columns = useMemo(() => requestColumns(), [])
  const cached = search.id ? records.find((r) => r.id === search.id) : undefined

  const activeFilters: Array<{ key: keyof RequestsSearch; label: string }> = []
  if (search.credentialId) activeFilters.push({ key: 'credentialId', label: `账号 #${search.credentialId}` })
  if (search.poolId) activeFilters.push({ key: 'poolId', label: `外部池 ${pools.data?.pools.find((p) => p.id === search.poolId)?.name ?? `#${search.poolId}`}` })
  if (search.conversationId) activeFilters.push({ key: 'conversationId', label: `会话 ${search.conversationId.slice(0, 12)}…` })
  if (search.keyId) activeFilters.push({ key: 'keyId', label: `Key ${search.keyId}` })
  if (search.model) activeFilters.push({ key: 'model', label: `模型 ${search.model}` })
  if (search.endpoint) activeFilters.push({ key: 'endpoint', label: `端点 ${search.endpoint}` })
  if (search.minTtft) activeFilters.push({ key: 'minTtft', label: `首字 ≥ ${search.minTtft}ms` })
  if (search.minCacheRead) activeFilters.push({ key: 'minCacheRead', label: `缓存读 ≥ ${search.minCacheRead}` })

  return (
    <Page>
      <PageHeader
        title="请求"
        description="逐条请求明细：路由、尝试链、Token、费用与错误"
        actions={
          <Button variant="outline" size="sm" onClick={() => setCleanupOpen(true)}>
            <Eraser /> 清理历史
          </Button>
        }
      />

      <div className="flex flex-wrap items-center gap-2">
        <form
          className="w-full sm:w-80"
          onSubmit={(e) => {
            e.preventDefault()
            setSearch({ q: qText.trim() || undefined })
          }}
        >
          <InputGroup>
            <InputGroupAddon>
              <Search />
            </InputGroupAddon>
            <InputGroupInput placeholder="请求 ID、模型、错误信息…" value={qText} onChange={(e) => setQText(e.target.value)} />
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
        <SelectControl
          className="h-7 w-auto"
          value={search.since && TIME_PRESETS.some((p) => p.value === search.since) ? search.since : 'all'}
          onChange={(v) => setSearch({ since: v === 'all' ? undefined : v })}
          options={[...TIME_PRESETS]}
        />
        <SelectControl
          className="h-7 w-auto"
          value={search.status ?? 'all'}
          onChange={(v) => setSearch({ status: v === 'all' ? undefined : (v as RequestsSearch['status']) })}
          options={[
            { value: 'all', label: '状态：全部' },
            { value: 'success', label: '成功' },
            { value: 'error', label: '错误' },
            { value: 'stream_error', label: '流中断' },
            { value: 'upstream_timeout', label: '上游超时' },
            { value: 'client_dropped', label: '客户端断开' },
          ]}
        />
        <SelectControl
          className="h-7 w-auto"
          value={search.route ?? 'all'}
          onChange={(v) => setSearch({ route: v === 'all' ? undefined : (v as RequestsSearch['route']) })}
          options={[
            { value: 'all', label: '路由：全部' },
            { value: 'local_credential', label: '本地账号' },
            { value: 'external_pool', label: '外部池' },
          ]}
        />
        <SelectControl
          className="h-7 w-auto"
          value={search.stream === undefined ? 'all' : search.stream ? 'stream' : 'non'}
          onChange={(v) => setSearch({ stream: v === 'all' ? undefined : v === 'stream' })}
          options={[
            { value: 'all', label: '流式：全部' },
            { value: 'stream', label: '流式' },
            { value: 'non', label: '非流式' },
          ]}
        />
        <AdvancedFilters search={search} onApply={setSearch} />
        <div className="ml-auto">
          <ColumnToggle columns={REQUEST_COLUMN_LABELS} visibility={visibility} onChange={setVisibility} />
        </div>
      </div>

      {activeFilters.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5">
          {activeFilters.map((f) => (
            <span key={f.key} className="inline-flex items-center gap-1 rounded-md border bg-accent/40 py-0.5 pr-0.5 pl-2 text-xs">
              {f.label}
              <Button variant="ghost" size="icon-xs" aria-label={`移除筛选 ${f.label}`} onClick={() => setSearch({ [f.key]: undefined })}>
                <X />
              </Button>
            </span>
          ))}
          <Button
            variant="link"
            size="xs"
            onClick={() =>
              setSearch(Object.fromEntries(activeFilters.map((f) => [f.key, undefined])) as Partial<RequestsSearch>)
            }
          >
            清除全部
          </Button>
        </div>
      )}

      {q.error && !q.data ? (
        <ErrorState error={q.error} onRetry={() => q.refetch()} />
      ) : (
        <DataTable
          data={records}
          columns={columns}
          getRowId={(r) => r.id}
          onRowClick={(r) => setSearch({ id: r.id })}
          activeRowId={search.id}
          columnVisibility={visibility}
          onColumnVisibilityChange={setVisibility}
          virtual
          maxHeight="calc(100svh - 17rem)"
          loading={q.isFetching && !q.isFetchingNextPage && !q.isLoading}
          rowClassName={(r) => cn(r.status !== 'success' && 'bg-danger-subtle/20')}
          empty={q.isLoading ? <div className="p-6 text-center text-sm text-muted-foreground">加载中…</div> : <EmptyState title="没有匹配的请求" />}
        />
      )}
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span className="num">已加载 {records.length} 条</span>
        {q.hasNextPage && (
          <Button size="sm" variant="outline" onClick={() => q.fetchNextPage()} disabled={q.isFetchingNextPage}>
            {q.isFetchingNextPage && <Loader2 className="animate-spin" />}
            加载更多
          </Button>
        )}
      </div>

      <RequestSheet
        id={search.id}
        cached={cached}
        onClose={() => setSearch({ id: undefined })}
        onFilterConversation={(conversationId) => setSearch({ conversationId, id: undefined })}
      />
      <CleanupDialog open={cleanupOpen} onOpenChange={setCleanupOpen} />
    </Page>
  )
}

function AdvancedFilters({ search, onApply }: { search: RequestsSearch; onApply: (patch: Partial<RequestsSearch>) => void }) {
  const [draft, setDraft] = useState({
    model: search.model ?? '',
    endpoint: search.endpoint ?? '',
    credentialId: search.credentialId?.toString() ?? '',
    keyId: search.keyId ?? '',
    conversationId: search.conversationId ?? '',
    minTtft: search.minTtft?.toString() ?? '',
    minCacheRead: search.minCacheRead?.toString() ?? '',
  })
  const [open, setOpen] = useState(false)
  const num = (v: string) => (v.trim() && Number.isFinite(Number(v)) ? Number(v) : undefined)
  const field = (key: keyof typeof draft, label: string, placeholder?: string) => (
    <div className="space-y-1">
      <Label className="text-xs">{label}</Label>
      <Input value={draft[key]} placeholder={placeholder} onChange={(e) => setDraft((d) => ({ ...d, [key]: e.target.value }))} className="h-7" />
    </div>
  )
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button variant="ghost" size="sm">
          更多筛选
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-80" align="start">
        <div className="grid grid-cols-2 gap-3">
          {field('model', '模型', 'claude-sonnet-4.5')}
          {field('endpoint', '端点', '/v1/messages')}
          {field('credentialId', '账号 ID')}
          {field('keyId', '请求 Key ID')}
          <div className="col-span-2">{field('conversationId', '会话 ID')}</div>
          {field('minTtft', '首字延迟 ≥ (ms)')}
          {field('minCacheRead', '缓存读 ≥ (tokens)')}
        </div>
        <div className="mt-4 flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={() => setOpen(false)}>
            取消
          </Button>
          <Button
            size="sm"
            onClick={() => {
              onApply({
                model: draft.model.trim() || undefined,
                endpoint: draft.endpoint.trim() || undefined,
                credentialId: num(draft.credentialId),
                keyId: draft.keyId.trim() || undefined,
                conversationId: draft.conversationId.trim() || undefined,
                minTtft: num(draft.minTtft),
                minCacheRead: num(draft.minCacheRead),
              })
              setOpen(false)
            }}
          >
            应用
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  )
}
