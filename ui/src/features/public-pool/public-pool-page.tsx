import { useEffect, useRef, useState, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { isAxiosError } from 'axios'
import { ChevronRight, Download, FileUp, Lock, Plus, RefreshCw, Server, Trash2, Upload, Wallet } from 'lucide-react'
import {
  Button,
  Checkbox,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
  Switch,
} from '@/components/ui'
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
  Toolbar,
} from '@/components/patterns'
import { getPublicPool } from '@/api/public-pool'
import { formatCredits, formatFullDate, formatNumber } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import { InertSidebar } from '@/layouts/sidebar'
import { InertTopbar } from '@/layouts/topbar'
import type { PublicPoolStatus } from '@/types/api'
import type { PageKey } from '@/types/ui'
import { CREDENTIAL_CARD_GRID } from '@/features/credentials/credential-card'
import { PublicPoolCard } from './public-pool-card'
import { PUBLIC_STATUS_TABS } from './public-pool-model'

const PAGE_SIZE_OPTIONS = [10, 20, 30, 50, 100]
const REFRESH_INTERVAL_MS = 10_000

/** Downstream agents should not learn that external pools exist. */
const HIDDEN_NAV_KEYS: ReadonlySet<PageKey> = new Set<PageKey>(['external', 'externalPoolRisk'])

const LOAD_BALANCING_LABELS: Record<string, string> = {
  priority: '优先级',
  balanced: '均衡负载',
  health_balanced: '健康均衡',
  weighted_least_inflight: '低负载优先',
}

/** A select that shows its value and never opens. */
function InertSelect({ value, label, className }: { value: string; label: string; className?: string }) {
  return (
    <Select value={value} open={false} onOpenChange={() => {}}>
      <SelectTrigger size="sm" className={className}><SelectValue /></SelectTrigger>
      <SelectContent>
        <SelectItem value={value}>{label}</SelectItem>
      </SelectContent>
    </Select>
  )
}

function FilterField({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="block min-w-0">
      <span className="mb-1 block h-4 whitespace-nowrap text-[0.68rem] font-medium leading-4 text-muted-foreground">{label}</span>
      {children}
    </label>
  )
}

/**
 * The account page for viewers without the admin key. It reads only `/api/public/pool`; the
 * sidebar, top bar and every action are drawn as on the real page but do nothing. Status tabs
 * and paging still work, since they only change what is viewed.
 */
export function PublicPoolPage() {
  return (
    <div className="flex min-h-screen bg-background">
      <aside className="sticky top-0 hidden h-screen shrink-0 lg:block">
        <InertSidebar activeKey="credentials" hiddenKeys={HIDDEN_NAV_KEYS} />
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        <InertTopbar page="credentials" />
        <main className="scrollbar-thin min-w-0 flex-1 px-4 py-4 lg:px-5 lg:py-5">
          <PublicPoolContent />
        </main>
      </div>
    </div>
  )
}

function PublicPoolContent() {
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(20)
  const [status, setStatus] = useState<PublicPoolStatus | '__all__'>('__all__')
  const cardScrollRef = useRef<HTMLDivElement | null>(null)
  const query = { page, limit: pageSize, status: status === '__all__' ? undefined : status }
  const pool = useQuery({
    queryKey: ['public-pool', query],
    queryFn: () => getPublicPool(query),
    refetchInterval: REFRESH_INTERVAL_MS,
    placeholderData: (previous) => previous,
    // A closed page answers 404; retrying will not open it.
    retry: (count, error) => !(isAxiosError(error) && error.response?.status === 404) && count < 1,
  })

  useEffect(() => {
    const list = cardScrollRef.current
    if (list && list.getBoundingClientRect().top < 0) list.scrollIntoView({ block: 'start' })
  }, [page, pageSize, status])
  useEffect(() => {
    if (pool.data && page > Math.max(pool.data.totalPages, 1)) setPage(Math.max(pool.data.totalPages, 1))
  }, [pool.data, page])

  if (pool.isLoading && !pool.data) return <LoadingState text="加载账号列表..." />
  if (pool.error && !pool.data) {
    if (isAxiosError(pool.error) && pool.error.response?.status === 404) {
      return (
        <EmptyState
          icon={<Lock className="h-12 w-12" />}
          title="号池页面未开放"
          description="管理员还没有开放这个页面，请联系管理员。"
        />
      )
    }
    return <ErrorState message={extractErrorMessage(pool.error)} />
  }
  if (!pool.data) return null

  const { overview, items, filteredTotal, totalPages } = pool.data
  const pending = pool.isPlaceholderData
  const requests = overview.realtime.requests
  const errorRate = requests > 0 ? overview.realtime.errorRequests / requests : 0
  const mode = overview.loadBalancingMode || 'priority'

  return (
    <PageContainer>
      <PageHeader
        actions={
          <div className="flex flex-wrap items-center gap-1.5">
            <InertSelect value={mode} label={LOAD_BALANCING_LABELS[mode] ?? mode} className="w-32" />
            <Button variant="outline" size="sm" aria-label="刷新">
              <RefreshCw className={`h-4 w-4 ${pool.isFetching ? 'animate-spin' : ''}`} />
            </Button>
            <Button variant="outline" size="sm" title="查询全部账号订阅和积分信息，刷新汇总">
              <Wallet className="h-4 w-4" /><span className="hidden sm:inline">查询全部积分</span>
            </Button>
            <Button variant="outline" size="sm"><FileUp className="h-4 w-4" /><span className="hidden sm:inline">KAM</span></Button>
            <Button variant="outline" size="sm"><Upload className="h-4 w-4" /><span className="hidden sm:inline">批量导入</span></Button>
            <Button variant="outline" size="sm"><Download className="h-4 w-4" /><span className="hidden sm:inline">导出</span></Button>
            <Button size="sm"><Plus className="h-4 w-4" />添加账号</Button>
          </div>
        }
      />

      <StatGrid>
        <StatCard
          title="账号"
          value={`${formatNumber(overview.enabled)} / ${formatNumber(overview.total)}`}
          valueTitle={`启用 ${formatNumber(overview.enabled)} / 共 ${formatNumber(overview.total)}`}
          desc={(
            <>
              <span className={overview.schedulable > 0 ? 'text-success' : 'text-muted-foreground'}>
                可调度 {formatNumber(overview.schedulable)}
              </span>
              <span className="text-muted-foreground"> · </span>
              <span className={overview.failing > 0 ? 'text-warning' : 'text-muted-foreground'}>
                异常 {formatNumber(overview.failing)}
              </span>
              <span className="text-muted-foreground"> · </span>
              <span className={overview.disabled > 0 ? 'text-destructive' : 'text-muted-foreground'}>
                禁用 {formatNumber(overview.disabled)}
              </span>
            </>
          )}
          icon={<Server className="h-5 w-5" />}
          tone="default"
        />
        <StatCard
          title="并发"
          value={`${formatNumber(overview.globalInFlightRequests)}${overview.globalMaxConcurrentRequests > 0 ? ` / ${formatNumber(overview.globalMaxConcurrentRequests)}` : ''}`}
          valueTitle={overview.globalMaxConcurrentRequests > 0
            ? `进行中 ${overview.globalInFlightRequests} / 上限 ${overview.globalMaxConcurrentRequests}`
            : `进行中 ${overview.globalInFlightRequests}，不限制`}
          desc={`占用账号 ${formatNumber(overview.inUse)} · 排队 ${formatNumber(overview.queuedRequests)}${overview.coolingDown > 0 ? ` · 冷却 ${formatNumber(overview.coolingDown)}` : ''}`}
          tone={overview.queuedRequests > 0 ? 'warning' : 'info'}
        />
        <StatCard
          title="RPM"
          value={formatNumber(overview.realtime.rpm)}
          valueTitle={`${overview.realtime.windowSeconds} 秒内 ${formatNumber(requests)} 次请求`}
          desc={`${formatNumber(overview.realtime.totalTpm)} TPM · 错误率 ${(errorRate * 100).toFixed(1)}%`}
          tone={errorRate >= 0.1 ? 'warning' : 'info'}
        />
        {/* Credits used only: what is left, and anything in dollars, is cost and stays private. */}
        <button
          type="button"
          className="relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl bg-card p-4 text-left shadow-sm transition-colors hover:shadow-md focus:outline-none focus:ring-2 focus:ring-primary/30"
          title="查看所有账号积分明细"
        >
          <span className="absolute left-0 top-4 h-8 w-1 rounded-r-full bg-success" />
          <div className="flex items-start justify-between gap-2 pl-2.5">
            <div className="min-w-0 flex-1">
              <div className="text-[0.72rem] font-semibold text-muted-foreground">已消耗积分</div>
              <div className="mt-1 break-words text-2xl font-semibold tracking-tight tabular-nums text-success">
                {formatCredits(overview.consumedCredits)}
              </div>
            </div>
            <ChevronRight className="mt-1 h-4 w-4 shrink-0 text-muted-foreground/60" />
          </div>
          <div className="mt-2 truncate pl-2.5 text-[0.72rem] text-muted-foreground">所有账号历史记录</div>
          <div className="mt-1 truncate pl-2.5 text-[0.72rem] text-muted-foreground">
            最近查询：{overview.lastCheckedAt ? formatFullDate(overview.lastCheckedAt) : '未查询'}
          </div>
        </button>
      </StatGrid>

      <SectionCard
        className="overflow-visible"
        title="账号列表"
        description={status === '__all__' ? `共 ${overview.total} 个账号` : `筛选后 ${filteredTotal} / 共 ${overview.total}`}
      >
        <div className="z-20 -mx-4 -mt-1 border-b bg-card px-4 pb-1 pt-1 lg:sticky lg:top-[var(--header-h)]">
          <div className="mb-2 inline-flex max-w-full flex-wrap gap-0.5 self-start rounded-lg bg-muted p-0.5" role="radiogroup" aria-label="按状态快速筛选">
            {PUBLIC_STATUS_TABS.map((tab) => {
              const selected = status === tab.value
              return (
                <button
                  key={tab.value}
                  type="button"
                  role="radio"
                  aria-checked={selected}
                  onClick={() => { setStatus(selected ? '__all__' : tab.value); setPage(1) }}
                  className={`flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40 ${selected
                    ? 'bg-card text-foreground shadow-sm'
                    : 'text-muted-foreground hover:text-foreground'
                  }`}
                >
                  {tab.value !== '__all__' && <span className={`size-1.5 rounded-full ${tab.dot}`} aria-hidden="true" />}
                  {tab.label}
                  <span className={`tabular-nums ${selected ? 'text-foreground' : 'text-muted-foreground/80'}`}>{tab.count(overview)}</span>
                </button>
              )
            })}
          </div>
          <Toolbar className="mb-2">
            <div className="grid w-full min-w-0 items-end gap-2 sm:grid-cols-2 xl:grid-cols-[minmax(220px,1.5fr)_140px_minmax(180px,1fr)_130px_140px] 2xl:grid-cols-[minmax(230px,1.25fr)_130px_minmax(170px,0.95fr)_120px_130px_minmax(130px,0.85fr)_140px_90px_auto]">
              <FilterField label="快速定位">
                <Input readOnly value="" placeholder="#ID / 邮箱 / Key / 错误" className="h-8 text-xs" />
              </FilterField>
              <FilterField label="状态"><InertSelect value="__all__" label="全部状态" /></FilterField>
              <FilterField label="可用模型"><InertSelect value="__all__" label="全部模型" /></FilterField>
              <FilterField label="订阅"><InertSelect value="__all__" label="全部订阅" /></FilterField>
              <FilterField label="区域"><InertSelect value="__all__" label="全部区域" /></FilterField>
              <FilterField label="代理"><InertSelect value="__all__" label="全部代理" /></FilterField>
              <FilterField label="排序"><InertSelect value="default" label="默认排序" /></FilterField>
              <FilterField label="方向"><InertSelect value="desc" label="降序" /></FilterField>
              {/* Holds the place of the account page's clear-filters button, so the columns line up. */}
              <div className="min-w-[4.5rem]" />
              <div className="flex h-8 items-center gap-2 self-end whitespace-nowrap">
                <Switch checked aria-label="显示邮箱后缀" title="关闭后邮箱后缀显示为 ***" />
                <span className="text-xs text-muted-foreground">显示邮箱后缀</span>
              </div>
            </div>
          </Toolbar>
          <div className="mb-2 flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Checkbox checked={false} aria-label="全选当前页" />
              <span className="text-xs text-muted-foreground">全选当前页</span>
              {pool.isFetching && !pool.isLoading && <Spinner size="sm" />}
            </div>
            {overview.disabled > 0 && (
              <Button variant="destructive" size="xs">
                <Trash2 className="h-3.5 w-3.5" />清除全部已禁用 ({overview.disabled})
              </Button>
            )}
          </div>
        </div>

        <div ref={cardScrollRef} className="scroll-mt-[calc(var(--header-h)+15rem)] pt-3" aria-label="账号卡片">
          {items.length === 0 ? (
            <EmptyState
              icon={<Server className="h-12 w-12" />}
              title="暂无账号"
              description={status === '__all__' ? '号池里还没有账号' : '没有匹配当前筛选条件的账号'}
            />
          ) : (
            <div className={CREDENTIAL_CARD_GRID}>
              {items.map((account) => <PublicPoolCard key={account.id} account={account} />)}
            </div>
          )}
        </div>

        {filteredTotal > 0 && (
          <div className="z-10 -mx-4 mt-3 border-t bg-card px-4 py-3 lg:sticky lg:bottom-0">
            <Pagination
              page={page}
              pageCount={totalPages}
              total={filteredTotal}
              pageSize={pageSize}
              pageSizeOptions={PAGE_SIZE_OPTIONS}
              onPageChange={setPage}
              onPageSizeChange={(next) => { setPageSize(next); setPage(1) }}
              pending={pending}
            />
          </div>
        )}
      </SectionCard>
    </PageContainer>
  )
}
