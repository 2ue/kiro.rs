import {
EmptyState,
ErrorState,
Field,
FieldGrid,
LoadingState,
ModalShell,
PageContainer,
PageHeader,
SectionCard,
Toolbar,
ToolbarActions,
ToolbarSearch,
useConfirm,
} from '@/components/patterns'
import {
Badge,
Button,
Checkbox,
Input,
Spinner,
Switch,
Table,
TableBody,
TableCell,
TableHead,
TableHeader,
TableRow,
Textarea,
} from '@/components/ui'
import { useAdoptModelCatalog, useDiscoverModelCatalog, useSetModelListed } from '@/hooks/use-models'
import {
useDeleteManualModel,
useModelCapabilities,
useModelPricing,
useSyncModelPricing,
useUpsertManualModel,
} from '@/hooks/use-usage'
import { formatDate, formatNumber } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type { ModelCapabilityItem,ModelCatalogCandidate,ModelCatalogDiscoverResponse,ModelPriceItem,ModelPricing,UpsertManualModelRequest } from '@/types/api'
import {
CloudDownload,
Pencil,
Plus,
RefreshCw,
Trash2,
} from 'lucide-react'
import { useEffect,useMemo,useState } from 'react'
import { toast } from 'sonner'

// ─── 工具 ──────────────────────────────────────────────────────────────────────

function isManual(item: ModelCapabilityItem): boolean {
  return item.source === 'manual'
}

function formatTokens(v?: number): string {
  if (v == null) return '—'
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`
  if (v >= 1000) return `${(v / 1000).toFixed(0)}K`
  return String(v)
}

function formatUsdPerM(perToken?: number): string {
  if (perToken == null) return '—'
  return `$${(perToken * 1_000_000).toFixed(2)}`
}

function sourceLabel(source?: string): string {
  switch (source) {
    case 'manual': return '手动添加'
    case 'synced': return '同步加入'
    case 'anthropic-seed':
    case 'built-in': return '内置'
    case 'litellm': return '价格源'
    case undefined: return '同步'
    default: return source
  }
}

function sourceTone(source?: string): 'neutral' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' | 'info' {
  if (source === 'manual') return 'primary'
  if (source === 'synced') return 'info'
  if (source === 'built-in' || source === 'anthropic-seed') return 'secondary'
  if (source === 'litellm') return 'success'
  return 'neutral'
}

function addModelKeyAliases(model: string, target: Set<string>) {
  const normalized = model.trim().toLowerCase()
  if (!normalized) return
  target.add(normalized)
  const slashIndex = normalized.lastIndexOf('/')
  if (slashIndex >= 0 && slashIndex + 1 < normalized.length) {
    target.add(normalized.slice(slashIndex + 1))
  }
  for (const value of Array.from(target)) {
    const dotVersion = value.match(/^(claude-(?:opus|sonnet|haiku)-\d+)-(\d+)(-.+)?$/)
    if (dotVersion) {
      target.add(`${dotVersion[1]}.${dotVersion[2]}${dotVersion[3] ?? ''}`)
    }
    const dashVersion = value.match(/^(claude-(?:opus|sonnet|haiku)-\d+)\.(\d+)(-.+)?$/)
    if (dashVersion) {
      target.add(`${dashVersion[1]}-${dashVersion[2]}${dashVersion[3] ?? ''}`)
    }
  }
}

function modelKeyAliases(model: string): string[] {
  const aliases = new Set<string>()
  addModelKeyAliases(model, aliases)
  return Array.from(aliases)
}

function pricingIndex(items: ModelPriceItem[]): Map<string, ModelPriceItem> {
  const map = new Map<string, ModelPriceItem>()
  for (const item of items) {
    for (const alias of modelKeyAliases(item.model)) {
      if (!map.has(alias)) map.set(alias, item)
    }
  }
  return map
}

function findPricing(index: Map<string, ModelPriceItem>, model: string): ModelPriceItem | undefined {
  for (const alias of modelKeyAliases(model)) {
    const item = index.get(alias)
    if (item) return item
  }
  return undefined
}

function dollarsPerMillion(value?: number): string {
  if (value == null || !Number.isFinite(value)) return ''
  return String(Number((value * 1_000_000).toFixed(6)))
}

function optionalNumber(value: string): number | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const parsed = Number(trimmed)
  return Number.isFinite(parsed) ? parsed : Number.NaN
}

function positiveInteger(value: string): number | undefined {
  const parsed = optionalNumber(value)
  if (parsed === undefined) return undefined
  return Number.isFinite(parsed) && parsed > 0 ? Math.floor(parsed) : Number.NaN
}

type ManualModelForm = {
  model: string
  displayName: string
  description: string
  maxInputTokens: string
  maxOutputTokens: string
  supportsPromptCaching: boolean
  supportedInputTypes: {
    TEXT: boolean
    IMAGE: boolean
  }
  includePricing: boolean
  inputCostPerMillion: string
  outputCostPerMillion: string
  cacheCreationInputCostPerMillion: string
  cacheCreation1hInputCostPerMillion: string
  cacheReadInputCostPerMillion: string
}

const emptyManualForm = (): ManualModelForm => ({
  model: '',
  displayName: '',
  description: '',
  maxInputTokens: '200000',
  maxOutputTokens: '64000',
  supportsPromptCaching: true,
  supportedInputTypes: {
    TEXT: true,
    IMAGE: true,
  },
  includePricing: false,
  inputCostPerMillion: '',
  outputCostPerMillion: '',
  cacheCreationInputCostPerMillion: '',
  cacheCreation1hInputCostPerMillion: '',
  cacheReadInputCostPerMillion: '',
})

function formFromModel(item?: ModelCapabilityItem, price?: ModelPricing): ManualModelForm {
  if (!item) return emptyManualForm()
  return {
    model: item.model,
    displayName: item.displayName || item.model,
    description: item.description || '',
    maxInputTokens: item.maxInputTokens ? String(item.maxInputTokens) : '',
    maxOutputTokens: item.maxOutputTokens ? String(item.maxOutputTokens) : '',
    supportsPromptCaching: item.supportsPromptCaching ?? true,
    supportedInputTypes: {
      TEXT: item.supportedInputTypes?.some((type) => type.toUpperCase() === 'TEXT') ?? true,
      IMAGE: item.supportedInputTypes?.some((type) => type.toUpperCase() === 'IMAGE') ?? false,
    },
    includePricing: Boolean(price),
    inputCostPerMillion: dollarsPerMillion(price?.inputCostPerToken),
    outputCostPerMillion: dollarsPerMillion(price?.outputCostPerToken),
    cacheCreationInputCostPerMillion: dollarsPerMillion(price?.cacheCreationInputTokenCost),
    cacheCreation1hInputCostPerMillion: dollarsPerMillion(price?.cacheCreation1hInputTokenCost),
    cacheReadInputCostPerMillion: dollarsPerMillion(price?.cacheReadInputTokenCost),
  }
}

function buildManualPayload(form: ManualModelForm): UpsertManualModelRequest {
  const supportedInputTypes = Object.entries(form.supportedInputTypes)
    .filter(([, enabled]) => enabled)
    .map(([type]) => type)
  const maxInputTokens = positiveInteger(form.maxInputTokens)
  const maxOutputTokens = positiveInteger(form.maxOutputTokens)
  if (Number.isNaN(maxInputTokens) || Number.isNaN(maxOutputTokens)) {
    throw new Error('输入上限和输出上限必须是大于 0 的整数，或留空')
  }
  const payload: UpsertManualModelRequest = {
    model: form.model.trim().toLowerCase(),
    displayName: form.displayName.trim() || undefined,
    description: form.description.trim() || undefined,
    maxInputTokens,
    maxOutputTokens,
    supportsPromptCaching: form.supportsPromptCaching,
    supportedInputTypes,
    clearPricing: !form.includePricing,
  }
  if (form.includePricing) {
    const input = optionalNumber(form.inputCostPerMillion)
    const output = optionalNumber(form.outputCostPerMillion)
    const cacheCreation = optionalNumber(form.cacheCreationInputCostPerMillion)
    const cacheCreation1h = optionalNumber(form.cacheCreation1hInputCostPerMillion)
    const cacheRead = optionalNumber(form.cacheReadInputCostPerMillion)
    if (
      !Number.isFinite(input) ||
      !Number.isFinite(output) ||
      (input as number) < 0 ||
      (output as number) < 0 ||
      (cacheCreation !== undefined && (!Number.isFinite(cacheCreation) || cacheCreation < 0)) ||
      (cacheCreation1h !== undefined && (!Number.isFinite(cacheCreation1h) || cacheCreation1h < 0)) ||
      (cacheRead !== undefined && (!Number.isFinite(cacheRead) || cacheRead < 0))
    ) {
      throw new Error('价格必须是有效数字')
    }
    payload.pricing = {
      inputCostPerMillion: input as number,
      outputCostPerMillion: output as number,
      cacheCreationInputCostPerMillion: cacheCreation,
      cacheCreation1hInputCostPerMillion: cacheCreation1h,
      cacheReadInputCostPerMillion: cacheRead,
    }
  }
  return payload
}

// ─── SyncCapabilitiesDialog ──────────────────────────────────────────────────

// ─── ModelsPage ───────────────────────────────────────────────────────────────

export function ModelsPage() {
  const listedMutation = useSetModelListed()
  const capabilities = useModelCapabilities()
  const pricing = useModelPricing()
  const syncPrice = useSyncModelPricing()
  const deleteModel = useDeleteManualModel()
  const discover = useDiscoverModelCatalog()
  const confirm = useConfirm()

  const [editTarget, setEditTarget] = useState<ModelCapabilityItem | null>(null)
  const [addOpen, setAddOpen] = useState(false)
  const [discovered, setDiscovered] = useState<ModelCatalogDiscoverResponse | null>(null)
  const [search, setSearch] = useState('')
  const [onlyListed, setOnlyListed] = useState(false)
  const [priceSearch, setPriceSearch] = useState('')

  const models = capabilities.data?.models ?? []
  const priceIndex = useMemo(() => pricingIndex(pricing.data?.models ?? []), [pricing.data?.models])
  const keyword = search.trim().toLowerCase()
  const visible = models.filter((m) =>
    (!keyword || `${m.model} ${m.displayName}`.toLowerCase().includes(keyword)) && (!onlyListed || m.listed),
  )
  const listedCount = models.filter((m) => m.listed).length
  const priceKeyword = priceSearch.trim().toLowerCase()
  const visiblePrices = (pricing.data?.models ?? []).filter((item) => !priceKeyword || item.model.toLowerCase().includes(priceKeyword))

  const handleDelete = async (item: ModelCapabilityItem) => {
    const ok = await confirm({
      title: '删除模型',
      message: `确定删除「${item.model}」？此操作无法撤销。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!ok) return
    deleteModel.mutate(item.model, {
      onSuccess: () => toast.success(`已删除模型 ${item.model}`),
      onError: (e) => toast.error(`删除失败: ${extractErrorMessage(e)}`),
    })
  }

  const startDiscover = () =>
    discover.mutate(undefined, {
      onSuccess: (data) => setDiscovered(data),
      onError: (e) => toast.error(`拉取模型来源失败: ${extractErrorMessage(e)}`),
    })

  const capLastSync = capabilities.data?.lastSyncedAt ? formatDate(capabilities.data.lastSyncedAt) : '—'
  const priceLastSync = pricing.data?.lastSyncedAt ? formatDate(pricing.data.lastSyncedAt) : '—'

  return (
    <PageContainer>
      <PageHeader
        actions={
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              disabled={syncPrice.isPending}
              onClick={() =>
                syncPrice.mutate(undefined, {
                  onSuccess: () => toast.success('价格目录已同步'),
                  onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
                })
              }
            >
              {syncPrice.isPending ? <Spinner size="sm" /> : <RefreshCw />}
              同步价格
            </Button>
            <Button variant="outline" disabled={discover.isPending} onClick={startDiscover}>
              {discover.isPending ? <Spinner size="sm" /> : <CloudDownload />}
              从来源同步模型
            </Button>
            <Button onClick={() => setAddOpen(true)}>
              <Plus />
              手动添加
            </Button>
          </div>
        }
      />

      <div className="grid gap-3 sm:grid-cols-2">
        <SectionCard title="模型目录" description={`来源: ${sourceLabel(capabilities.data?.source)} · 最后同步: ${capLastSync}`}>
          <div className="space-y-2">
            <div className="flex items-center gap-2">
              <Badge tone={capabilities.data?.available ? 'success' : 'error'}>{capabilities.data?.available ? '可用' : '不可用'}</Badge>
              <span className="text-sm text-muted-foreground">共 {formatNumber(models.length)} 个模型，对外列出 {formatNumber(listedCount)} 个</span>
            </div>
            {capabilities.data?.lastError && (
              <div className="whitespace-pre-wrap break-words rounded-lg border border-destructive/30 bg-destructive/5 p-2 text-xs text-destructive">{capabilities.data.lastError}</div>
            )}
          </div>
        </SectionCard>
        <SectionCard title="价格目录" description={`来源: ${sourceLabel(pricing.data?.source)} · 最后同步: ${priceLastSync}`}>
          <div className="space-y-2">
            <div className="flex items-center gap-2">
              <Badge tone={pricing.data?.available ? 'success' : 'error'}>{pricing.data?.available ? '可用' : '不可用'}</Badge>
              <span className="text-sm text-muted-foreground">共 {formatNumber(pricing.data?.modelCount ?? 0)} 个模型有价格</span>
            </div>
            {pricing.data?.lastError && (
              <div className="whitespace-pre-wrap break-words rounded-lg border border-destructive/30 bg-destructive/5 p-2 text-xs text-destructive">{pricing.data.lastError}</div>
            )}
          </div>
        </SectionCard>
      </div>

      <SectionCard
        title="模型清单"
        description="对外 /v1/models 只返回“列出”打开的模型。内置与同步加入的模型不可删除；手动添加的可编辑或删除。价格按模型名匹配价格目录。"
        noPadding
      >
        <div className="border-b px-4 py-3">
          <Toolbar>
            <ToolbarSearch value={search} onChange={setSearch} placeholder="搜索模型 ID 或显示名" />
            <ToolbarActions>
              <label className="flex items-center gap-2 text-sm">
                <Switch checked={onlyListed} onCheckedChange={setOnlyListed} aria-label="只看已列出" />
                只看已列出
              </label>
              <span className="text-xs text-muted-foreground">{visible.length} / {models.length}</span>
            </ToolbarActions>
          </Toolbar>
        </div>
        {capabilities.isLoading ? (
          <LoadingState text="加载模型列表..." className="py-8" />
        ) : capabilities.error ? (
          <ErrorState message={extractErrorMessage(capabilities.error)} />
        ) : models.length === 0 ? (
          <EmptyState title="暂无模型" description="从来源同步，或手动添加" action={<Button size="sm" onClick={startDiscover}><CloudDownload />从来源同步模型</Button>} />
        ) : visible.length === 0 ? (
          <EmptyState title="没有匹配的模型" description="换个关键字或取消筛选。" />
        ) : (
          <div className="scrollbar-thin overflow-x-auto">
            <Table className="min-w-[960px]">
              <TableHeader>
                <TableRow>
                  <TableHead className="w-10 text-right">#</TableHead>
                  <TableHead>模型 ID</TableHead>
                  <TableHead>显示名称</TableHead>
                  <TableHead className="text-right">最大输入</TableHead>
                  <TableHead className="text-right">最大输出</TableHead>
                  <TableHead>缓存</TableHead>
                  <TableHead className="text-right">输入 $/M</TableHead>
                  <TableHead className="text-right">输出 $/M</TableHead>
                  <TableHead className="text-right">缓存读 $/M</TableHead>
                  <TableHead>列出</TableHead>
                  <TableHead>来源</TableHead>
                  <TableHead className="w-20" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {visible.map((item, index) => {
                  const price = findPricing(priceIndex, item.model)?.pricing
                  const manual = isManual(item)
                  return (
                    <TableRow key={item.model} className={item.listed ? undefined : 'opacity-60'}>
                      <TableCell className="text-right font-mono text-xs tabular-nums text-muted-foreground">{index + 1}</TableCell>
                      <TableCell className="font-mono text-xs max-w-[220px]"><div className="truncate" title={item.model}>{item.model}</div></TableCell>
                      <TableCell className="text-sm text-muted-foreground max-w-[160px]"><div className="truncate">{item.displayName || '—'}</div></TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">{formatTokens(item.maxInputTokens)}</TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">{formatTokens(item.maxOutputTokens)}</TableCell>
                      <TableCell>{item.supportsPromptCaching ? <Badge size="xs" tone="success">支持</Badge> : <span className="text-xs text-muted-foreground/50">—</span>}</TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(price?.inputCostPerToken)}</TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(price?.outputCostPerToken)}</TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(price?.cacheReadInputTokenCost)}</TableCell>
                      <TableCell>
                        <Switch
                          checked={item.listed}
                          disabled={listedMutation.isPending}
                          aria-label={`对外列出 ${item.model}`}
                          onCheckedChange={(listed) =>
                            listedMutation.mutate({ model: item.model, listed }, {
                              onSuccess: () => toast.success(listed ? `${item.model} 已对外列出` : `${item.model} 已隐藏`),
                              onError: (error) => toast.error(extractErrorMessage(error)),
                            })
                          }
                        />
                      </TableCell>
                      <TableCell><Badge size="xs" tone={sourceTone(manual ? 'manual' : item.source)}>{sourceLabel(manual ? 'manual' : item.source)}</Badge></TableCell>
                      <TableCell>
                        {manual && (
                          <div className="flex items-center gap-1">
                            <Button variant="ghost" size="icon-xs" title="编辑" aria-label={`编辑 ${item.model}`} onClick={() => setEditTarget(item)}><Pencil /></Button>
                            <Button variant="ghost" size="icon-xs" className="text-destructive hover:text-destructive" title="删除" aria-label={`删除 ${item.model}`} onClick={() => handleDelete(item)}><Trash2 /></Button>
                          </div>
                        )}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
        )}
      </SectionCard>

      <SectionCard
        title="价格目录"
        description={pricing.data?.sourceUrl || '同步后展示用于计费和成本估算的模型价格'}
        noPadding
      >
        {pricing.isLoading ? (
          <LoadingState text="加载价格目录..." className="py-8" />
        ) : pricing.error ? (
          <ErrorState message={extractErrorMessage(pricing.error)} />
        ) : !pricing.data?.models.length ? (
          <EmptyState title="暂无价格数据" description="点击「同步价格」获取模型价格目录" />
        ) : (
          <>
          <div className="px-5 pt-4">
            <Toolbar>
              <ToolbarSearch value={priceSearch} onChange={setPriceSearch} placeholder="搜索价格目录里的模型 ID" />
              <ToolbarActions>
                <span className="text-xs text-muted-foreground">{visiblePrices.length} / {pricing.data.models.length}</span>
              </ToolbarActions>
            </Toolbar>
          </div>
          {visiblePrices.length === 0 ? (
            <EmptyState title="没有匹配的模型" description="换个关键字。" />
          ) : (
          <div className="scrollbar-thin overflow-x-auto">
            <Table className="min-w-[860px]">
              <TableHeader>
                <TableRow>
                  <TableHead className="w-10 text-right">#</TableHead>
                  <TableHead>模型 ID</TableHead>
                  <TableHead>来源</TableHead>
                  <TableHead className="text-right">输入 $/M</TableHead>
                  <TableHead className="text-right">输出 $/M</TableHead>
                  <TableHead className="text-right">缓存写 5 分钟 $/M</TableHead>
                  <TableHead className="text-right">缓存写 1 小时 $/M</TableHead>
                  <TableHead className="text-right">缓存读 $/M</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {visiblePrices.map((item, index) => (
                  <TableRow key={item.model}>
                    <TableCell className="text-right font-mono text-xs tabular-nums text-muted-foreground">{index + 1}</TableCell>
                    <TableCell className="font-mono text-xs max-w-[260px]"><div className="truncate" title={item.model}>{item.model}</div></TableCell>
                    <TableCell><Badge size="xs" tone={sourceTone(item.source)}>{sourceLabel(item.source)}</Badge></TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.inputCostPerToken)}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.outputCostPerToken)}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.cacheCreationInputTokenCost)}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.cacheCreation1hInputTokenCost)}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.cacheReadInputTokenCost)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
          )}
          </>
        )}
      </SectionCard>

      <EditModelDialog open={addOpen} onClose={() => setAddOpen(false)} />
      {editTarget && (
        <EditModelDialog open initial={editTarget} initialPricing={findPricing(priceIndex, editTarget.model)?.pricing} onClose={() => setEditTarget(null)} />
      )}
      {discovered && <DiscoverDialog data={discovered} onClose={() => setDiscovered(null)} />}
    </PageContainer>
  )
}

// ─── 从来源同步：弹层确认 ────────────────────────────────────────────────────

function DiscoverDialog({ data, onClose }: { data: ModelCatalogDiscoverResponse; onClose: () => void }) {
  const adopt = useAdoptModelCatalog()
  const fresh = data.candidates.filter((c) => !c.inCatalog)
  const [selected, setSelected] = useState<Set<string>>(() => new Set(fresh.map((c) => c.model)))
  const [listed, setListed] = useState(true)
  const toggle = (model: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev)
      if (on) next.add(model)
      else next.delete(model)
      return next
    })
  const row = (c: ModelCatalogCandidate) => (
    <div key={c.model} className={`flex items-center gap-3 px-3 py-2 ${c.inCatalog ? 'opacity-60' : 'hover:bg-muted/30'}`}>
      <Checkbox checked={c.inCatalog || selected.has(c.model)} disabled={c.inCatalog} onCheckedChange={(v) => toggle(c.model, v === true)} aria-label={`选择 ${c.model}`} />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-sm">{c.model}</span>
          <span className="text-xs text-muted-foreground">{c.displayName}</span>
          {c.inCatalog && <Badge size="xs" tone="neutral">已在目录</Badge>}
          {!c.hasPricing && <Badge size="xs" tone="warning">无价格</Badge>}
        </div>
      </div>
      <span className="shrink-0 font-mono text-xs text-muted-foreground">{formatTokens(c.maxInputTokens)} / {formatTokens(c.maxOutputTokens)}</span>
    </div>
  )
  return (
    <ModalShell
      open
      onClose={onClose}
      title="从来源同步模型"
      description={`来源 ${data.source}，${formatDate(data.fetchedAt)} 拉取。勾选要加入目录的模型，确认后才会写入；已在目录里的不会被改动。`}
      width="max-w-3xl"
      footer={
        <>
          <label className="mr-auto flex items-center gap-2 text-sm">
            <Switch checked={listed} onCheckedChange={setListed} aria-label="加入后对外列出" />
            加入后对外列出
          </label>
          <Button variant="outline" onClick={onClose} disabled={adopt.isPending}>取消</Button>
          <Button
            disabled={adopt.isPending || selected.size === 0}
            onClick={() =>
              adopt.mutate({ models: Array.from(selected), listed }, {
                onSuccess: (res) => {
                  toast.success(`已加入 ${res.added.length} 个模型${res.skipped.length ? `，跳过 ${res.skipped.length} 个` : ''}`)
                  onClose()
                },
                onError: (e) => toast.error(`加入失败: ${extractErrorMessage(e)}`),
              })
            }
          >
            {adopt.isPending ? <Spinner size="sm" /> : <Plus />}
            加入所选（{selected.size}）
          </Button>
        </>
      }
    >
      {data.candidates.length === 0 ? (
        <EmptyState title="来源里没有可用的 Claude 模型" />
      ) : (
        <div className="space-y-4">
          {fresh.length > 0 && (
            <div>
              <div className="mb-2 flex items-center justify-between text-xs text-muted-foreground">
                <span>可加入（{fresh.length}）</span>
                <span className="flex gap-2">
                  <Button size="xs" variant="ghost" onClick={() => setSelected(new Set(fresh.map((c) => c.model)))}>全选</Button>
                  <Button size="xs" variant="ghost" onClick={() => setSelected(new Set())}>清空</Button>
                </span>
              </div>
              <div className="divide-y rounded-md border">{fresh.map(row)}</div>
            </div>
          )}
          {data.candidates.some((c) => c.inCatalog) && (
            <div>
              <div className="mb-2 text-xs text-muted-foreground">已在目录（{data.candidates.filter((c) => c.inCatalog).length}）</div>
              <div className="divide-y rounded-md border">{data.candidates.filter((c) => c.inCatalog).map(row)}</div>
            </div>
          )}
          <p className="text-xs text-muted-foreground">来源地址：<span className="break-all font-mono">{data.sourceUrl}</span></p>
        </div>
      )}
    </ModalShell>
  )
}

// ─── 能力编辑弹窗 ─────────────────────────────────────────────────────────────

interface EditModelDialogProps {
  open: boolean
  initial?: ModelCapabilityItem
  initialPricing?: ModelPricing
  onClose: () => void
}

function EditModelDialog({ open, initial, initialPricing, onClose }: EditModelDialogProps) {
  const upsert = useUpsertManualModel()
  const [form, setForm] = useState<ManualModelForm>(() => formFromModel(initial, initialPricing))

  useEffect(() => {
    if (open) setForm(formFromModel(initial, initialPricing))
  }, [initial, initialPricing, open])

  const update = <K extends keyof ManualModelForm>(key: K, value: ManualModelForm[K]) =>
    setForm((prev) => ({ ...prev, [key]: value }))

  const updateInputType = (type: keyof ManualModelForm['supportedInputTypes'], enabled: boolean) =>
    setForm((prev) => ({
      ...prev,
      supportedInputTypes: { ...prev.supportedInputTypes, [type]: enabled },
    }))

  const handleSave = () => {
    if (!form.model.trim()) return toast.error('模型名称不能为空')
    let payload: UpsertManualModelRequest
    try {
      payload = buildManualPayload(form)
    } catch (error) {
      toast.error(extractErrorMessage(error))
      return
    }
    upsert.mutate(payload, {
      onSuccess: () => {
        toast.success(`模型 ${payload.model} 已保存`)
        onClose()
      },
      onError: (e) => toast.error(`保存失败: ${extractErrorMessage(e)}`),
    })
  }

  const num = (key: 'maxInputTokens' | 'maxOutputTokens' | 'inputCostPerMillion' | 'outputCostPerMillion' | 'cacheCreationInputCostPerMillion' | 'cacheCreation1hInputCostPerMillion' | 'cacheReadInputCostPerMillion', label: string, unit: string, extra?: { placeholder?: string; description?: string; step?: string }) => (
    <Field label={label} htmlFor={`model-${key}`} description={extra?.description}>
      <div className="flex min-h-9 items-center gap-2">
        <Input id={`model-${key}`} type="number" inputMode="decimal" className="w-36 shrink-0 tabular-nums" min={0} step={extra?.step ?? '1'} value={form[key]} placeholder={extra?.placeholder} disabled={upsert.isPending} onChange={(e) => update(key, e.target.value)} />
        <span className="shrink-0 text-xs text-muted-foreground">{unit}</span>
      </div>
    </Field>
  )
  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={initial ? `编辑模型 · ${initial.model}` : '手动添加模型'}
      description={initial ? '只能修改能力参数与价格；模型 ID 不可改。' : '手动添加的模型可随时编辑或删除；价格可选。'}
      width="max-w-3xl"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={upsert.isPending}>取消</Button>
          <Button disabled={upsert.isPending || !form.model.trim()} onClick={handleSave}>
            {upsert.isPending ? <Spinner size="sm" /> : null}
            保存
          </Button>
        </>
      }
    >
      <div className="space-y-6">
        <FieldGrid min="16rem">
          <Field label="模型 ID" htmlFor="model-id" required description="小写，横杠分隔，例如 claude-opus-4-5">
            <Input id="model-id" value={form.model} placeholder="claude-opus-4-5" disabled={!!initial || upsert.isPending} onChange={(e) => update('model', e.target.value)} />
          </Field>
          <Field label="显示名称" htmlFor="model-display" description="留空按模型 ID 推导">
            <Input id="model-display" value={form.displayName} placeholder="Claude Opus 4.5" disabled={upsert.isPending} onChange={(e) => update('displayName', e.target.value)} />
          </Field>
          {num('maxInputTokens', '最大输入', 'Token', { placeholder: '200000' })}
          {num('maxOutputTokens', '最大输出', 'Token', { placeholder: '64000' })}
          <Field label="输入类型">
            <div className="flex min-h-9 items-center gap-4">
              {(['TEXT', 'IMAGE'] as const).map((type) => (
                <label key={type} className="flex items-center gap-2 text-sm">
                  <Checkbox checked={form.supportedInputTypes[type]} disabled={upsert.isPending} onCheckedChange={(checked) => updateInputType(type, checked === true)} />
                  {type === 'TEXT' ? '文本' : '图片'}
                </label>
              ))}
            </div>
          </Field>
          <Field label="Prompt Caching" htmlFor="model-caching">
            <div className="flex min-h-9 items-center gap-2">
              <Switch id="model-caching" checked={form.supportsPromptCaching} disabled={upsert.isPending} onCheckedChange={(checked) => update('supportsPromptCaching', checked)} />
              <span className="text-xs text-muted-foreground">{form.supportsPromptCaching ? '支持' : '不支持'}</span>
            </div>
          </Field>
        </FieldGrid>
        <Field label="描述" htmlFor="model-desc">
          <Textarea id="model-desc" rows={2} value={form.description} disabled={upsert.isPending} placeholder="用于说明模型能力或适用场景" onChange={(e) => update('description', e.target.value)} />
        </Field>

        <section className="space-y-4 rounded-lg border bg-muted/20 p-4">
          <Field label="同时维护价格" htmlFor="model-pricing-toggle" description="关闭后保存会清除该模型的手动价格；开启后按每百万 Token 填写，缓存价格留空时按后端默认规则推导。">
            <div className="flex min-h-9 items-center gap-2">
              <Switch id="model-pricing-toggle" checked={form.includePricing} disabled={upsert.isPending} onCheckedChange={(checked) => update('includePricing', checked)} />
              <span className="text-xs text-muted-foreground">{form.includePricing ? '维护' : '不维护'}</span>
            </div>
          </Field>
          {form.includePricing && (
            <FieldGrid min="14rem">
              {num('inputCostPerMillion', '输入', '$/M', { step: '0.000001' })}
              {num('outputCostPerMillion', '输出', '$/M', { step: '0.000001' })}
              {num('cacheCreationInputCostPerMillion', '缓存写 5 分钟', '$/M', { step: '0.000001', placeholder: '输入 × 1.25' })}
              {num('cacheCreation1hInputCostPerMillion', '缓存写 1 小时', '$/M', { step: '0.000001', placeholder: '输入 × 2' })}
              {num('cacheReadInputCostPerMillion', '缓存读', '$/M', { step: '0.000001' })}
            </FieldGrid>
          )}
        </section>
      </div>
    </ModalShell>
  )
}
