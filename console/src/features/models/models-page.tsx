import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import type { ColumnDef } from '@tanstack/react-table'
import { MoreHorizontal, Pencil, Plus, RefreshCw, Search, Trash2 } from 'lucide-react'
import { systemApi } from '@/api/endpoints/system'
import type { ModelCapabilityItem, ModelPricing } from '@/api/types'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { DataTable } from '@/components/data-table/data-table'
import { EmptyState } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { useConfirm } from '@/components/patterns/confirm'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtCompact, fmtRelative } from '@/lib/format'
import { qk } from '@/queries/keys'
import { ModelDialog } from './model-dialog'
import { ErrorText } from '@/components/patterns/error-text'
import { useModelCapabilities, useModelPricing } from '@/queries/shared'

export interface ModelRow {
  model: string
  cap?: ModelCapabilityItem
  price?: ModelPricing
  priceSource?: string
}

const perM = (v?: number) => (typeof v === 'number' ? `$${(v * 1e6).toFixed(v * 1e6 >= 10 ? 1 : 2)}` : '—')

/** 模型能力与价格合并为一张表；同步按钮是唯一入口 */
export function ModelsPage() {
  const caps = useModelCapabilities()
  const pricing = useModelPricing()
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const [q, setQ] = useState('')
  const [editing, setEditing] = useState<ModelRow | 'new' | null>(null)
  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: qk.system.capabilities })
    queryClient.invalidateQueries({ queryKey: qk.system.pricing })
  }
  const syncAll = useMutation({
    mutationFn: async () => {
      await Promise.all([systemApi.syncModelCapabilities(), systemApi.syncModelPricing()])
    },
    onSettled: invalidate,
    meta: { success: '模型能力与价格已同步', error: '同步失败' },
  })
  const remove = useMutation({
    mutationFn: (model: string) => systemApi.deleteManualModel(model),
    onSettled: invalidate,
    meta: { success: '已删除手工模型', error: '删除失败' },
  })

  const rows = useMemo<ModelRow[]>(() => {
    const map = new Map<string, ModelRow>()
    for (const c of caps.data?.models ?? []) map.set(c.model, { model: c.model, cap: c })
    for (const p of pricing.data?.models ?? []) {
      const row = map.get(p.model) ?? { model: p.model }
      row.price = p.pricing
      row.priceSource = p.source
      map.set(p.model, row)
    }
    const term = q.trim().toLowerCase()
    return [...map.values()]
      .filter((r) => !term || `${r.model} ${r.cap?.displayName ?? ''}`.toLowerCase().includes(term))
      .sort((a, b) => a.model.localeCompare(b.model))
  }, [caps.data, pricing.data, q])

  const columns = useMemo<ColumnDef<ModelRow, unknown>[]>(
    () => [
      {
        id: 'model',
        header: '模型',
        size: 240,
        meta: { mobile: 'title', grow: true },
        cell: ({ row: { original: r } }) => (
          <div className="min-w-0 leading-tight">
            <div className="truncate font-mono text-xs">{r.model}</div>
            {r.cap?.displayName && r.cap.displayName !== r.model && (
              <div className="truncate text-xs text-muted-foreground">{r.cap.displayName}</div>
            )}
          </div>
        ),
      },
      {
        id: 'ctx',
        header: '上下文 / 输出',
        size: 120,
        meta: { mobile: true, align: 'right' },
        cell: ({ row: { original: r } }) => (r.cap ? `${fmtCompact(r.cap.maxInputTokens)} / ${fmtCompact(r.cap.maxOutputTokens)}` : '—'),
      },
      {
        id: 'caps',
        header: '能力',
        size: 160,
        cell: ({ row: { original: r } }) => (
          <span className="flex flex-wrap gap-1">
            {r.cap?.supportsPromptCaching && <ToneBadge tone="info">缓存</ToneBadge>}
            {r.cap?.supportedInputTypes
              .filter((t) => t.toLowerCase() !== 'text')
              .map((t) => (
                <ToneBadge key={t}>{t}</ToneBadge>
              ))}
          </span>
        ),
      },
      {
        id: 'in',
        header: '输入/M',
        size: 80,
        meta: { mobile: true, align: 'right' },
        cell: ({ row: { original: r } }) => perM(r.price?.inputCostPerToken),
      },
      {
        id: 'out',
        header: '输出/M',
        size: 80,
        meta: { mobile: true, align: 'right' },
        cell: ({ row: { original: r } }) => perM(r.price?.outputCostPerToken),
      },
      {
        id: 'cw',
        header: '缓存写/M',
        size: 84,
        meta: { align: 'right' },
        cell: ({ row: { original: r } }) => perM(r.price?.cacheCreationInputTokenCost),
      },
      {
        id: 'cr',
        header: '缓存读/M',
        size: 84,
        meta: { mobile: true, align: 'right' },
        cell: ({ row: { original: r } }) => perM(r.price?.cacheReadInputTokenCost),
      },
      {
        id: 'src',
        header: '来源',
        size: 90,
        meta: { mobile: 'badge' },
        cell: ({ row: { original: r } }) => {
          const manual = r.cap?.source === 'manual' || r.priceSource === 'manual'
          return <ToneBadge tone={manual ? 'primary' : 'neutral'}>{manual ? '手工' : (r.cap?.source ?? r.priceSource ?? '—')}</ToneBadge>
        },
      },
      {
        id: 'act',
        header: '',
        size: 48,
        cell: ({ row: { original: r } }) => (
          <span onClick={(e) => e.stopPropagation()}>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon-xs" aria-label="更多操作">
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => setEditing(r)}>
                  <Pencil /> 编辑为手工模型
                </DropdownMenuItem>
                {(r.cap?.source === 'manual' || r.priceSource === 'manual') && (
                  <DropdownMenuItem
                    variant="destructive"
                    onSelect={async () => {
                      if (
                        await confirm({
                          title: `删除手工模型 ${r.model}？`,
                          description: '同步来源中的同名模型会重新生效。',
                          destructive: true,
                          confirmText: '删除',
                        })
                      )
                        remove.mutate(r.model)
                    }}
                  >
                    <Trash2 /> 删除手工配置
                  </DropdownMenuItem>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          </span>
        ),
      },
    ],
    [], // eslint-disable-line react-hooks/exhaustive-deps
  )

  return (
    <Page>
      <PageHeader
        title="模型"
        description={
          <span>
            能力目录 {caps.data?.modelCount ?? '—'} 个（{caps.data?.lastSyncedAt ? `${fmtRelative(caps.data.lastSyncedAt)}同步` : '未同步'}
            ） · 价格目录 {pricing.data?.modelCount ?? '—'} 个（
            {pricing.data?.lastSyncedAt ? `${fmtRelative(pricing.data.lastSyncedAt)}同步` : '未同步'}）
          </span>
        }
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => syncAll.mutate()} disabled={syncAll.isPending}>
              <RefreshCw className={syncAll.isPending ? 'animate-spin' : undefined} /> 同步能力与价格
            </Button>
            <Button size="sm" onClick={() => setEditing('new')}>
              <Plus /> 手工添加
            </Button>
          </>
        }
      />
      {(caps.data?.lastError || pricing.data?.lastError) && (
        <div className="rounded-lg border border-danger/30 bg-danger-subtle/30 p-3">
          <div className="mb-1 text-xs text-muted-foreground">最近同步错误</div>
          <ErrorText error={caps.data?.lastError ?? pricing.data?.lastError} />
        </div>
      )}
      <div className="w-full sm:w-72">
        <InputGroup>
          <InputGroupAddon>
            <Search />
          </InputGroupAddon>
          <InputGroupInput placeholder="搜索模型" value={q} onChange={(e) => setQ(e.target.value)} />
        </InputGroup>
      </div>
      <DataTable
        data={rows}
        columns={columns}
        getRowId={(r) => r.model}
        onRowClick={(r) => setEditing(r)}
        virtual={rows.length > 60}
        empty={
          caps.isLoading ? <div className="p-6 text-center text-sm text-muted-foreground">加载中…</div> : <EmptyState title="没有模型" />
        }
      />
      <ModelDialog
        row={editing === 'new' ? undefined : (editing ?? undefined)}
        open={editing !== null}
        onOpenChange={(o) => !o && setEditing(null)}
        onSaved={invalidate}
      />
    </Page>
  )
}
