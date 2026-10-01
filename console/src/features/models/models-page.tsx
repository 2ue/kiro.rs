import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import type { ColumnDef } from '@tanstack/react-table'
import { MoreHorizontal, Pencil, Plus, RefreshCw, Search, Trash2 } from 'lucide-react'
import { systemApi } from '@/api/endpoints/system'
import type { ModelCapabilityItem, ModelPricing, UpsertManualModelRequest } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { InputGroup, InputGroupAddon, InputGroupInput } from '@/components/ui/input-group'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { DataTable } from '@/components/data-table/data-table'
import { NumberInput } from '@/components/patterns/fields'
import { EmptyState } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { useConfirm } from '@/components/patterns/confirm'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtCompact, fmtRelative } from '@/lib/format'
import { qk } from '@/queries/keys'
import { useModelCapabilities, useModelPricing } from '@/queries/shared'

interface ModelRow {
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
  const remove = useMutation({ mutationFn: (model: string) => systemApi.deleteManualModel(model), onSettled: invalidate, meta: { success: '已删除手工模型', error: '删除失败' } })

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
    return [...map.values()].filter((r) => !term || `${r.model} ${r.cap?.displayName ?? ''}`.toLowerCase().includes(term)).sort((a, b) => a.model.localeCompare(b.model))
  }, [caps.data, pricing.data, q])

  const columns = useMemo<ColumnDef<ModelRow, unknown>[]>(
    () => [
      {
        id: 'model',
        header: '模型',
        size: 240,
        meta: { grow: true },
        cell: ({ row: { original: r } }) => (
          <div className="min-w-0 leading-tight">
            <div className="truncate font-mono text-xs">{r.model}</div>
            {r.cap?.displayName && r.cap.displayName !== r.model && <div className="truncate text-xs text-muted-foreground">{r.cap.displayName}</div>}
          </div>
        ),
      },
      {
        id: 'ctx',
        header: '上下文 / 输出',
        size: 120,
        meta: { align: 'right' },
        cell: ({ row: { original: r } }) => (r.cap ? `${fmtCompact(r.cap.maxInputTokens)} / ${fmtCompact(r.cap.maxOutputTokens)}` : '—'),
      },
      {
        id: 'caps',
        header: '能力',
        size: 160,
        cell: ({ row: { original: r } }) => (
          <span className="flex flex-wrap gap-1">
            {r.cap?.supportsPromptCaching && <ToneBadge tone="info">缓存</ToneBadge>}
            {r.cap?.supportedInputTypes.filter((t) => t.toLowerCase() !== 'text').map((t) => (
              <ToneBadge key={t}>{t}</ToneBadge>
            ))}
          </span>
        ),
      },
      { id: 'in', header: '输入/M', size: 80, meta: { align: 'right' }, cell: ({ row: { original: r } }) => perM(r.price?.inputCostPerToken) },
      { id: 'out', header: '输出/M', size: 80, meta: { align: 'right' }, cell: ({ row: { original: r } }) => perM(r.price?.outputCostPerToken) },
      { id: 'cw', header: '缓存写/M', size: 84, meta: { align: 'right' }, cell: ({ row: { original: r } }) => perM(r.price?.cacheCreationInputTokenCost) },
      { id: 'cr', header: '缓存读/M', size: 84, meta: { align: 'right' }, cell: ({ row: { original: r } }) => perM(r.price?.cacheReadInputTokenCost) },
      {
        id: 'src',
        header: '来源',
        size: 90,
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
                      if (await confirm({ title: `删除手工模型 ${r.model}？`, description: '同步来源中的同名模型会重新生效。', destructive: true, confirmText: '删除' })) remove.mutate(r.model)
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
            能力目录 {caps.data?.modelCount ?? '—'} 个（{caps.data?.lastSyncedAt ? `${fmtRelative(caps.data.lastSyncedAt)}同步` : '未同步'}） · 价格目录 {pricing.data?.modelCount ?? '—'} 个（
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
        <p className="text-xs text-danger">最近同步错误：{caps.data?.lastError ?? pricing.data?.lastError}</p>
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
        empty={caps.isLoading ? <div className="p-6 text-center text-sm text-muted-foreground">加载中…</div> : <EmptyState title="没有模型" />}
      />
      <ModelDialog row={editing === 'new' ? undefined : editing ?? undefined} open={editing !== null} onOpenChange={(o) => !o && setEditing(null)} onSaved={invalidate} />
    </Page>
  )
}

const INPUT_TYPES = ['text', 'image', 'document']

function ModelDialog({ row, open, onOpenChange, onSaved }: { row?: ModelRow; open: boolean; onOpenChange: (v: boolean) => void; onSaved: () => void }) {
  const [form, setForm] = useState<UpsertManualModelRequest & { withPricing: boolean }>({ model: '', supportedInputTypes: ['text'], withPricing: false })
  const [lastOpen, setLastOpen] = useState(false)
  if (open !== lastOpen) {
    setLastOpen(open)
    if (open)
      setForm({
        model: row?.model ?? '',
        displayName: row?.cap?.displayName,
        description: row?.cap?.description,
        maxInputTokens: row?.cap?.maxInputTokens,
        maxOutputTokens: row?.cap?.maxOutputTokens,
        supportsPromptCaching: row?.cap?.supportsPromptCaching ?? true,
        supportedInputTypes: row?.cap?.supportedInputTypes ?? ['text'],
        withPricing: !!row?.price,
        pricing: row?.price
          ? {
              inputCostPerMillion: row.price.inputCostPerToken * 1e6,
              outputCostPerMillion: row.price.outputCostPerToken * 1e6,
              cacheCreationInputCostPerMillion: row.price.cacheCreationInputTokenCost * 1e6,
              cacheReadInputCostPerMillion: row.price.cacheReadInputTokenCost * 1e6,
            }
          : { inputCostPerMillion: 0, outputCostPerMillion: 0 },
      })
  }
  const save = useMutation({
    mutationFn: () => {
      const { withPricing, ...req } = form
      return systemApi.upsertManualModel({ ...req, model: req.model.trim(), pricing: withPricing ? req.pricing : undefined, clearPricing: !withPricing && !!row?.price ? true : undefined })
    },
    onSuccess: () => {
      onSaved()
      onOpenChange(false)
    },
    meta: { success: '模型已保存', error: '保存失败' },
  })
  const p = form.pricing ?? { inputCostPerMillion: 0, outputCostPerMillion: 0 }
  const setP = (k: keyof typeof p, v: number | null) => setForm((f) => ({ ...f, pricing: { ...p, [k]: v ?? 0 } }))

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{row ? '编辑手工模型' : '添加手工模型'}</DialogTitle>
          <DialogDescription>手工配置优先于同步来源</DialogDescription>
        </DialogHeader>
        <div className="grid gap-3">
          <div className="grid grid-cols-2 gap-3">
            <div className="col-span-2 space-y-1.5">
              <Label htmlFor="m-id">模型 ID</Label>
              <Input id="m-id" value={form.model} disabled={!!row} onChange={(e) => setForm((f) => ({ ...f, model: e.target.value }))} className="font-mono text-xs" />
            </div>
            <div className="col-span-2 space-y-1.5">
              <Label htmlFor="m-name">显示名称</Label>
              <Input id="m-name" value={form.displayName ?? ''} onChange={(e) => setForm((f) => ({ ...f, displayName: e.target.value || undefined }))} />
            </div>
            <div className="space-y-1.5">
              <Label>最大输入</Label>
              <NumberInput value={form.maxInputTokens ?? null} allowEmpty min={0} suffix="tokens" onChange={(v) => setForm((f) => ({ ...f, maxInputTokens: v ?? undefined }))} />
            </div>
            <div className="space-y-1.5">
              <Label>最大输出</Label>
              <NumberInput value={form.maxOutputTokens ?? null} allowEmpty min={0} suffix="tokens" onChange={(v) => setForm((f) => ({ ...f, maxOutputTokens: v ?? undefined }))} />
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-4 text-sm">
            {INPUT_TYPES.map((t) => (
              <label key={t} className="flex items-center gap-1.5">
                <Checkbox
                  checked={form.supportedInputTypes.includes(t)}
                  onCheckedChange={(v) =>
                    setForm((f) => ({ ...f, supportedInputTypes: v ? [...new Set([...f.supportedInputTypes, t])] : f.supportedInputTypes.filter((x) => x !== t) }))
                  }
                />
                {t}
              </label>
            ))}
            <label className="ml-auto flex items-center gap-2">
              支持缓存
              <Switch checked={!!form.supportsPromptCaching} onCheckedChange={(v) => setForm((f) => ({ ...f, supportsPromptCaching: v }))} />
            </label>
          </div>
          <label className="flex items-center justify-between border-t pt-3 text-sm font-medium">
            设置价格（美元 / 百万 tokens）
            <Switch checked={form.withPricing} onCheckedChange={(v) => setForm((f) => ({ ...f, withPricing: v }))} />
          </label>
          {form.withPricing && (
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label>输入</Label>
                <NumberInput value={p.inputCostPerMillion} min={0} step={0.01} suffix="$/M" onChange={(v) => setP('inputCostPerMillion', v)} />
              </div>
              <div className="space-y-1.5">
                <Label>输出</Label>
                <NumberInput value={p.outputCostPerMillion} min={0} step={0.01} suffix="$/M" onChange={(v) => setP('outputCostPerMillion', v)} />
              </div>
              <div className="space-y-1.5">
                <Label>缓存写</Label>
                <NumberInput value={p.cacheCreationInputCostPerMillion ?? null} allowEmpty min={0} step={0.01} suffix="$/M" onChange={(v) => setP('cacheCreationInputCostPerMillion', v)} />
              </div>
              <div className="space-y-1.5">
                <Label>缓存读</Label>
                <NumberInput value={p.cacheReadInputCostPerMillion ?? null} allowEmpty min={0} step={0.01} suffix="$/M" onChange={(v) => setP('cacheReadInputCostPerMillion', v)} />
              </div>
            </div>
          )}
        </div>
        <DialogFooter>
          <Button onClick={() => save.mutate()} disabled={!form.model.trim() || form.supportedInputTypes.length === 0 || save.isPending}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
