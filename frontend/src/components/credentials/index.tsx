import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import {
  CheckCircle2,
  Filter,
  Loader2,
  Plus,
  RefreshCw,
  Search,
  Trash2,
  Upload,
} from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Pagination } from '@/components/ui/pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  useCredentialsPage,
  useResetFailure,
} from '@/hooks/use-credentials'
import { usePricing } from '@/hooks/use-pricing'
import { useUsageStats } from '@/hooks/use-usage'
import { getCredentialBalance, forceRefreshToken } from '@/api/admin'
import { extractErrorMessage } from '@/lib/utils'
import { usePreferences } from '@/store/preferences'
import type { BalanceResponse } from '@/types/api'
import { CredentialCard } from './credential-card'
import { ImportCredentialsDialog } from './import-dialog'
import { AddCredentialDialog } from './add-credential-dialog'

const PAGE_SIZE_OPTIONS = [12, 20, 24, 48, 100]

interface AggregatedUsage {
  todayCost: number
  totalCost: number
  todayTokens: number
  totalTokens: number
}

export default function CredentialsPage() {
  const pageSize = usePreferences((s) => s.pageSize)
  const setPageSize = usePreferences((s) => s.setPageSize)
  const [page, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [statusFilter, setStatusFilter] = useState<'all' | 'enabled' | 'disabled'>('all')
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [importOpen, setImportOpen] = useState(false)
  const [addOpen, setAddOpen] = useState(false)
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalance, setLoadingBalance] = useState<Set<number>>(new Set())
  const [refreshing, setRefreshing] = useState(false)

  const { data, isLoading, error, refetch } = useCredentialsPage({
    page,
    limit: pageSize,
  })
  const pricingQuery = usePricing()
  // 取两份聚合:今日 (today 0 点 - now) 和 全部累计 (since 给个非常早的时间)
  const todayRange = useMemo(() => {
    const start = new Date()
    start.setHours(0, 0, 0, 0)
    return { since: start.toISOString(), until: new Date().toISOString(), bucket: 'hour' as const }
  }, [])
  const todayStats = useUsageStats(todayRange)
  const totalRange = useMemo(
    () => ({
      since: new Date('2000-01-01T00:00:00Z').toISOString(),
      until: new Date().toISOString(),
      bucket: 'day' as const,
    }),
    [],
  )
  const totalStats = useUsageStats(totalRange)
  const resetFailure = useResetFailure()

  const credentials = useMemo(() => data?.credentials ?? [], [data?.credentials])
  const totalPages = data?.totalPages ?? 0

  const filtered = useMemo(() => {
    const lowered = search.trim().toLowerCase()
    return credentials.filter((c) => {
      if (statusFilter === 'enabled' && c.disabled) return false
      if (statusFilter === 'disabled' && !c.disabled) return false
      if (!lowered) return true
      return (
        String(c.id).includes(lowered) ||
        c.email?.toLowerCase().includes(lowered) ||
        c.maskedApiKey?.toLowerCase().includes(lowered) ||
        c.authMethod?.toLowerCase().includes(lowered)
      )
    })
  }, [credentials, search, statusFilter])

  const usageAgg = useMemo(() => {
    const map = new Map<number, AggregatedUsage>()
    // total stats(所有历史)
    for (const c of totalStats.data?.byCredential ?? []) {
      map.set(c.credentialId, {
        todayCost: 0,
        totalCost: c.costUsd,
        todayTokens: 0,
        totalTokens: c.tokens + c.outputTokens,
      })
    }
    // 叠加 today stats 到 todayCost / todayTokens
    for (const c of todayStats.data?.byCredential ?? []) {
      const entry: AggregatedUsage = map.get(c.credentialId) ?? {
        todayCost: 0,
        totalCost: 0,
        todayTokens: 0,
        totalTokens: 0,
      }
      entry.todayCost = c.costUsd
      entry.todayTokens = c.tokens + c.outputTokens
      map.set(c.credentialId, entry)
    }
    return map
  }, [todayStats.data?.byCredential, totalStats.data?.byCredential])

  useEffect(() => {
    setSelectedIds(new Set())
  }, [page, pageSize])

  const toggleSelect = (id: number) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }
  const selectAll = () => {
    setSelectedIds(new Set(filtered.map((c) => c.id)))
  }
  const clearSelection = () => setSelectedIds(new Set())

  const queryBalances = async () => {
    const ids = filtered.filter((c) => !c.disabled).map((c) => c.id)
    if (ids.length === 0) {
      toast.info('当前页没有可查询的可用凭据')
      return
    }
    setRefreshing(true)
    let ok = 0
    let fail = 0
    for (const id of ids) {
      setLoadingBalance((prev) => new Set(prev).add(id))
      try {
        const b = await getCredentialBalance(id)
        ok++
        setBalanceMap((prev) => new Map(prev).set(id, b))
      } catch {
        fail++
      } finally {
        setLoadingBalance((prev) => {
          const next = new Set(prev)
          next.delete(id)
          return next
        })
      }
    }
    setRefreshing(false)
    if (fail === 0) toast.success(`已查询 ${ok} 个余额`)
    else toast.warning(`查询完成: 成功 ${ok},失败 ${fail}`)
  }

  const handleBatchRefresh = async () => {
    if (selectedIds.size === 0) return
    const targets = Array.from(selectedIds).filter((id) => {
      const c = credentials.find((x) => x.id === id)
      return c && !c.disabled && c.authMethod !== 'api_key'
    })
    if (targets.length === 0) {
      toast.error('选中项里没有可刷新的 OAuth 凭据')
      return
    }
    setRefreshing(true)
    let ok = 0
    let fail = 0
    for (const id of targets) {
      try {
        await forceRefreshToken(id)
        ok++
      } catch {
        fail++
      }
    }
    setRefreshing(false)
    refetch()
    toast.info(`刷新完成: 成功 ${ok},失败 ${fail}`)
    clearSelection()
  }

  const handleBatchReset = () => {
    if (selectedIds.size === 0) return
    const targets = Array.from(selectedIds).filter((id) => {
      const c = credentials.find((x) => x.id === id)
      return c && (c.failureCount > 0 || c.refreshFailureCount > 0)
    })
    if (targets.length === 0) {
      toast.error('选中项里没有需要重置的凭据')
      return
    }
    let ok = 0
    let fail = 0
    Promise.all(
      targets.map(
        (id) =>
          new Promise<void>((res) => {
            resetFailure.mutate(id, {
              onSuccess: () => {
                ok++
                res()
              },
              onError: () => {
                fail++
                res()
              },
            })
          }),
      ),
    ).then(() => {
      toast.info(`重置完成: 成功 ${ok},失败 ${fail}`)
      clearSelection()
    })
  }

  if (isLoading && !data) {
    return (
      <div className="grid h-64 place-items-center text-sm text-muted-foreground">
        <Loader2 className="h-5 w-5 animate-spin" />
      </div>
    )
  }

  if (error) {
    return (
      <Card>
        <CardContent className="space-y-3 py-8 text-center text-sm">
          <div className="text-destructive">{extractErrorMessage(error)}</div>
          <Button onClick={() => refetch()}>重试</Button>
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">账号管理</h1>
          <p className="text-sm text-muted-foreground">
            共 {data?.total ?? 0} 个,可用 {data?.available ?? 0} 个,
            当前活跃 #{data?.currentId ?? '-'}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative">
            <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="搜索 ID / 邮箱 / API Key"
              className="w-56 pl-8"
            />
          </div>
          <Select value={statusFilter} onValueChange={(v) => setStatusFilter(v as never)}>
            <SelectTrigger className="w-32">
              <Filter className="h-3.5 w-3.5" />
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">全部状态</SelectItem>
              <SelectItem value="enabled">已启用</SelectItem>
              <SelectItem value="disabled">已停用</SelectItem>
            </SelectContent>
          </Select>
          <Button
            variant="outline"
            onClick={queryBalances}
            disabled={refreshing || filtered.length === 0}
          >
            <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
            查询余额
          </Button>
          <Button variant="outline" onClick={() => setImportOpen(true)}>
            <Upload className="h-4 w-4" />
            批量导入
          </Button>
          <Button onClick={() => setAddOpen(true)}>
            <Plus className="h-4 w-4" />
            添加
          </Button>
        </div>
      </div>

      {selectedIds.size > 0 && (
        <Card>
          <CardContent className="flex flex-wrap items-center gap-2 py-3">
            <Badge variant="secondary">已选 {selectedIds.size} 项</Badge>
            <Button size="sm" variant="ghost" onClick={selectAll}>
              全选当前页
            </Button>
            <Button size="sm" variant="ghost" onClick={clearSelection}>
              取消
            </Button>
            <div className="ml-auto flex flex-wrap gap-2">
              <Button size="sm" variant="outline" onClick={handleBatchRefresh}>
                <RefreshCw className="h-4 w-4" />
                批量刷新令牌
              </Button>
              <Button size="sm" variant="outline" onClick={handleBatchReset}>
                <CheckCircle2 className="h-4 w-4" />
                批量重置失败
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="text-destructive"
                onClick={() => {
                  toast.warning('请在卡片中确认逐个删除,以避免误删可用账号')
                }}
                disabled={refreshing}
              >
                <Trash2 className="h-4 w-4" />
                批量删除
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {filtered.length === 0 ? (
        <Card>
          <CardContent className="py-12 text-center text-sm text-muted-foreground">
            {data?.total === 0 ? '尚无凭据,先添加一个吧' : '当前筛选下没有结果'}
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {filtered.map((c) => {
            const agg = usageAgg.get(c.id) ?? {
              todayCost: 0,
              totalCost: 0,
              todayTokens: 0,
              totalTokens: 0,
            }
            return (
              <CredentialCard
                key={c.id}
                credential={c}
                selected={selectedIds.has(c.id)}
                onToggleSelect={() => toggleSelect(c.id)}
                onViewBalance={(id) => {
                  setLoadingBalance((prev) => new Set(prev).add(id))
                  getCredentialBalance(id)
                    .then((b) => {
                      setBalanceMap((prev) => new Map(prev).set(id, b))
                      toast.success(`已查询凭据 #${id} 余额`)
                    })
                    .catch((err) =>
                      toast.error(extractErrorMessage(err)),
                    )
                    .finally(() =>
                      setLoadingBalance((prev) => {
                        const next = new Set(prev)
                        next.delete(id)
                        return next
                      }),
                    )
                }}
                balance={balanceMap.get(c.id) ?? null}
                loadingBalance={loadingBalance.has(c.id)}
                pricing={pricingQuery.data}
                todayCostUsd={agg.todayCost}
                totalCostUsd={agg.totalCost}
                todayTokens={agg.todayTokens}
                totalTokens={agg.totalTokens}
              />
            )
          })}
        </div>
      )}

      {totalPages > 0 && (
        <Pagination
          page={page}
          totalPages={totalPages}
          totalItems={data?.total ?? 0}
          pageSize={pageSize}
          pageSizeOptions={PAGE_SIZE_OPTIONS}
          onPageChange={setPage}
          onPageSizeChange={(size) => setPageSize(size)}
        />
      )}

      <ImportCredentialsDialog open={importOpen} onOpenChange={setImportOpen} />
      <AddCredentialDialog open={addOpen} onOpenChange={setAddOpen} />
    </div>
  )
}
