import { ListFilter, Search, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { ColumnToggle } from '@/components/data-table/column-toggle'
import { SelectControl } from '@/components/patterns/fields'
import { SavedViews } from '@/components/patterns/saved-views'
import { cn } from '@/lib/utils'
import { useProxies } from '@/queries/shared'
import { ACCOUNT_COLUMN_LABELS } from './columns'
import { STATUS_SEGMENTS, type AccountsSearch } from './search'
import type { VisibilityState as VS } from '@tanstack/react-table'

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

/** 账号列表筛选工具栏：搜索、维度筛选、排序、视图切换 */
export function AccountsFilterBar({
  search,
  setSearch,
  qText,
  setQText,
  view,
  viewSearch,
  onApplyView,
  visibility,
  setVisibility,
}: {
  search: AccountsSearch
  setSearch: (patch: Partial<AccountsSearch>, resetPage?: boolean) => void
  qText: string
  setQText: (v: string) => void
  view: 'table' | 'cards'
  viewSearch: AccountsSearch
  onApplyView: (v: AccountsSearch) => void
  visibility: VS
  setVisibility: (v: VS) => void
}) {
  const proxies = useProxies()
  return (
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
          <InputGroupInput
            placeholder="邮箱、#ID、标签…"
            value={qText}
            onChange={(e) => setQText(e.target.value)}
            onBlur={() => qText !== (search.q ?? '') && setSearch({ q: qText.trim() || undefined })}
          />
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
          <Button
            variant="ghost"
            size="sm"
            className={cn(search.status && !STATUS_SEGMENTS.some((s) => s.key === search.status) && 'text-primary')}
          >
            <ListFilter /> 更多筛选
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent className="w-52">
          <DropdownMenuItem onSelect={() => setSearch({ status: 'proxy_blocked' })}>代理不可用</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setSearch({ status: 'custom_scheduling' })}>自定义了调度参数</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setSearch({ status: 'unknown_subscription' })}>订阅未知</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <SavedViews scope="accounts" current={viewSearch} isEmpty={Object.keys(viewSearch).length === 0} onApply={onApplyView} />
      <div className="ml-auto flex items-center gap-2">
        <SelectControl
          className="h-7 w-32"
          value={search.sort ?? 'default'}
          onChange={(v) => setSearch({ sort: v === 'default' ? undefined : v })}
          options={SORT_OPTIONS}
        />
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => setSearch({ order: (search.order ?? 'desc') === 'desc' ? 'asc' : 'desc' })}
          aria-label="切换排序方向"
        >
          <span className="text-xs">{(search.order ?? 'desc') === 'desc' ? '↓' : '↑'}</span>
        </Button>
        {view === 'table' && <ColumnToggle columns={ACCOUNT_COLUMN_LABELS} visibility={visibility} onChange={setVisibility} />}
        <ToggleGroup
          type="single"
          size="sm"
          variant="outline"
          value={view}
          onValueChange={(v) => v && setSearch({ view: v === 'table' ? undefined : (v as 'cards') }, false)}
        >
          <ToggleGroupItem value="table" aria-label="表格视图">
            表格
          </ToggleGroupItem>
          <ToggleGroupItem value="cards" aria-label="卡片视图">
            卡片
          </ToggleGroupItem>
        </ToggleGroup>
      </div>
    </div>
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
