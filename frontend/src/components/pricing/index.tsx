import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Loader2, RefreshCw, Search, ShieldAlert } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Pagination } from '@/components/ui/pagination'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { useAppConfig } from '@/hooks/use-app-config'
import { usePricing, useSyncPricing } from '@/hooks/use-pricing'
import { extractErrorMessage, formatDateTime, formatUsd } from '@/lib/utils'

const PAGE_SIZE_OPTIONS = [20, 25, 50, 100, 200]

function pricePerMillion(perToken: number | null | undefined): string {
  if (perToken == null) return '—'
  return formatUsd(perToken * 1_000_000, 2)
}

export default function PricingPage() {
  const pricing = usePricing()
  const sync = useSyncPricing()
  const config = useAppConfig()

  const [search, setSearch] = useState('')
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(50)
  const debouncedSearch = useDebouncedValue(search, 300)

  useEffect(() => {
    setPage(1)
  }, [debouncedSearch, pageSize])

  const filtered = useMemo(() => {
    const items = pricing.data ?? []
    const lower = debouncedSearch.trim().toLowerCase()
    if (!lower) return items
    return items.filter(
      (p) =>
        p.modelId.toLowerCase().includes(lower) ||
        (p.displayName ?? '').toLowerCase().includes(lower) ||
        p.provider.toLowerCase().includes(lower),
    )
  }, [pricing.data, debouncedSearch])

  const totalItems = filtered.length
  const totalPages = Math.max(1, Math.ceil(totalItems / pageSize))
  const pageItems = useMemo(
    () => filtered.slice((page - 1) * pageSize, page * pageSize),
    [filtered, page, pageSize],
  )

  const lastSync = useMemo(() => {
    const items = pricing.data ?? []
    const latest = items
      .map((p) => p.syncedAt)
      .filter(Boolean)
      .sort()
      .pop()
    return latest ?? null
  }, [pricing.data])

  const sourceUrl =
    (config.data?.find((c) => c.key === 'pricing_source_url')?.value as string) ??
    'https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json'

  const handleSync = (forceBuiltin = false) => {
    sync.mutate(forceBuiltin, {
      onSuccess: (s) =>
        toast.success(
          `同步完成 · 来源 ${s.source} · 写入 ${s.upserted} 条${s.usedFallback ? '(已回退到内置快照)' : ''}`,
        ),
      onError: (err) => toast.error(extractErrorMessage(err)),
    })
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">模型计价</h1>
          <p className="text-sm text-muted-foreground">
            来源 LiteLLM,启动时若数据库为空会自动同步一次。仅保留 Claude 模型。
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            onClick={() => handleSync(true)}
            disabled={sync.isPending}
          >
            <ShieldAlert className="h-4 w-4" />
            使用内置兜底
          </Button>
          <Button onClick={() => handleSync(false)} disabled={sync.isPending}>
            {sync.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <RefreshCw className="h-4 w-4" />
            )}
            立即同步
          </Button>
        </div>
      </div>

      <div className="grid gap-3 md:grid-cols-3">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              模型条数
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-semibold tabular-nums">
              {pricing.data?.length ?? 0}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              最后同步时间
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-base font-medium">
              {lastSync ? formatDateTime(lastSync) : '从未'}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              数据源
            </CardTitle>
          </CardHeader>
          <CardContent>
            <a
              href={sourceUrl}
              target="_blank"
              rel="noreferrer"
              className="break-all text-xs underline hover:opacity-80"
            >
              {sourceUrl}
            </a>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardContent className="space-y-3 py-3">
          <div className="relative max-w-md">
            <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="搜索 modelId / displayName / provider..."
              className="pl-8"
            />
          </div>

          {pricing.isLoading ? (
            <div className="py-8 text-center text-sm text-muted-foreground">
              加载中...
            </div>
          ) : pricing.error ? (
            <div className="py-8 text-center text-sm text-destructive">
              {extractErrorMessage(pricing.error)}
            </div>
          ) : totalItems === 0 ? (
            <div className="py-12 text-center text-sm text-muted-foreground">
              {(pricing.data?.length ?? 0) === 0
                ? '尚未同步任何模型,点击右上角"立即同步"'
                : '当前筛选下没有结果'}
            </div>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>模型 ID</TableHead>
                  <TableHead>提供商</TableHead>
                  <TableHead className="text-right">输入 / 1M</TableHead>
                  <TableHead className="text-right">输出 / 1M</TableHead>
                  <TableHead className="text-right">缓存读 / 1M</TableHead>
                  <TableHead className="text-right">缓存写 / 1M</TableHead>
                  <TableHead className="text-right">上下文</TableHead>
                  <TableHead>来源</TableHead>
                  <TableHead>同步时间</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {pageItems.map((p) => (
                  <TableRow key={p.modelId}>
                    <TableCell className="font-mono text-xs">{p.modelId}</TableCell>
                    <TableCell>
                      <Badge variant="outline">{p.provider}</Badge>
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {pricePerMillion(p.inputCostPerToken)}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {pricePerMillion(p.outputCostPerToken)}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {pricePerMillion(p.cacheReadInputTokenCost)}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {pricePerMillion(p.cacheCreationInputTokenCost)}
                    </TableCell>
                    <TableCell className="text-right tabular-nums">
                      {p.maxInputTokens?.toLocaleString('zh-CN') ?? '—'}
                    </TableCell>
                    <TableCell>
                      <Badge
                        variant={p.source === 'litellm' ? 'success' : 'warning'}
                      >
                        {p.source}
                      </Badge>
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground">
                      {formatDateTime(p.syncedAt)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}

          {totalItems > 0 && (
            <Pagination
              page={page}
              totalPages={totalPages}
              totalItems={totalItems}
              pageSize={pageSize}
              pageSizeOptions={PAGE_SIZE_OPTIONS}
              onPageChange={setPage}
              onPageSizeChange={setPageSize}
            />
          )}
        </CardContent>
      </Card>
    </div>
  )
}
