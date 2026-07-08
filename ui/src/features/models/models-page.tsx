import { useEffect, useMemo, useState } from 'react'
import {
  Plus,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  useConfirm,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
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
import { extractErrorMessage } from '@/lib/utils'
import { formatNumber } from '@/lib/format'
import {
  useDeleteManualModel,
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
  useUpsertManualModel,
} from '@/hooks/use-usage'
import type { ModelCapabilityItem, ModelPriceItem, ModelPricing, UpsertManualModelRequest } from '@/types/api'

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
  if (!source) return '同步'
  if (source === 'manual') return '手动'
  if (source === 'built-in') return '内置'
  if (source === 'litellm') return '价格源'
  return source
}

function sourceTone(source?: string): 'neutral' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' | 'info' {
  if (source === 'manual') return 'primary'
  if (source === 'built-in') return 'secondary'
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
    const cacheRead = optionalNumber(form.cacheReadInputCostPerMillion)
    if (
      !Number.isFinite(input) ||
      !Number.isFinite(output) ||
      (input as number) < 0 ||
      (output as number) < 0 ||
      (cacheCreation !== undefined && (!Number.isFinite(cacheCreation) || cacheCreation < 0)) ||
      (cacheRead !== undefined && (!Number.isFinite(cacheRead) || cacheRead < 0))
    ) {
      throw new Error('价格必须是有效数字')
    }
    payload.pricing = {
      inputCostPerMillion: input as number,
      outputCostPerMillion: output as number,
      cacheCreationInputCostPerMillion: cacheCreation,
      cacheReadInputCostPerMillion: cacheRead,
    }
  }
  return payload
}

// ─── ModelsPage ───────────────────────────────────────────────────────────────

export function ModelsPage() {
  const capabilities = useModelCapabilities()
  const pricing = useModelPricing()
  const syncCap = useSyncModelCapabilities()
  const syncPrice = useSyncModelPricing()
  const deleteModel = useDeleteManualModel()
  const confirm = useConfirm()

  const [editTarget, setEditTarget] = useState<ModelCapabilityItem | null>(null)
  const [addOpen, setAddOpen] = useState(false)

  const models = capabilities.data?.models ?? []
  const priceIndex = useMemo(() => pricingIndex(pricing.data?.models ?? []), [pricing.data?.models])

  const handleDelete = async (item: ModelCapabilityItem) => {
    const ok = await confirm({
      title: '删除手动模型',
      message: `确定删除手动添加的模型 "${item.model}"？此操作无法撤销。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!ok) return
    deleteModel.mutate(item.model, {
      onSuccess: () => toast.success(`已删除模型 ${item.model}`),
      onError: (e) => toast.error(`删除失败: ${extractErrorMessage(e)}`),
    })
  }

  const capLastSync = capabilities.data?.lastSyncedAt
    ? new Date(capabilities.data.lastSyncedAt).toLocaleString('zh-CN')
    : '—'
  const priceLastSync = pricing.data?.lastSyncedAt
    ? new Date(pricing.data.lastSyncedAt).toLocaleString('zh-CN')
    : '—'

  return (
    <PageContainer>
      <PageHeader
        title="模型能力"
        subtitle="查看同步来的模型列表、手动维护能力参数；模型价格与盈亏分析请见「成本」页"
        actions={
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={syncCap.isPending}
              onClick={() =>
                syncCap.mutate(undefined, {
                  onSuccess: () => toast.success('能力目录已同步'),
                  onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
                })
              }
            >
              {syncCap.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-4 w-4" />}
              同步能力
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={syncPrice.isPending}
              onClick={() =>
                syncPrice.mutate(undefined, {
                  onSuccess: () => toast.success('价格目录已同步'),
                  onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
                })
              }
            >
              {syncPrice.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-4 w-4" />}
              同步价格
            </Button>
            <Button size="sm" onClick={() => setAddOpen(true)}>
              <Plus className="h-4 w-4" />
              手动添加
            </Button>
          </div>
        }
      />

      {/* 同步状态摘要 */}
      <div className="grid gap-3 sm:grid-cols-2">
        <SectionCard title="能力目录" description={`来源: ${capabilities.data?.source ?? '—'} · 最后同步: ${capLastSync}`}>
          <div className="flex items-center gap-2">
            <Badge tone={capabilities.data?.available ? 'success' : 'error'}>
              {capabilities.data?.available ? '可用' : '不可用'}
            </Badge>
            <span className="text-sm text-muted-foreground">
              共 {formatNumber(capabilities.data?.modelCount ?? 0)} 个模型
            </span>
            {capabilities.data?.lastError && (
              <span className="text-xs text-destructive truncate max-w-xs">{capabilities.data.lastError}</span>
            )}
          </div>
        </SectionCard>
        <SectionCard title="价格目录" description={`来源: ${pricing.data?.source ?? '—'} · 最后同步: ${priceLastSync}`}>
          <div className="flex items-center gap-2">
            <Badge tone={pricing.data?.available ? 'success' : 'error'}>
              {pricing.data?.available ? '可用' : '不可用'}
            </Badge>
            <span className="text-sm text-muted-foreground">
              共 {formatNumber(pricing.data?.modelCount ?? 0)} 个模型
            </span>
            {pricing.data?.lastError && (
              <span className="text-xs text-destructive truncate max-w-xs">{pricing.data.lastError}</span>
            )}
          </div>
        </SectionCard>
      </div>

      {/* 模型能力清单 */}
      <SectionCard
        title="模型价格目录"
        description={pricing.data?.sourceUrl || '同步后展示当前可用于计费和成本估算的模型价格'}
        noPadding
      >
        {pricing.isLoading ? (
          <LoadingState text="加载价格目录..." className="py-8" />
        ) : pricing.error ? (
          <ErrorState message={extractErrorMessage(pricing.error)} />
        ) : !pricing.data?.models.length ? (
          <EmptyState title="暂无价格数据" description="点击「同步价格」获取模型价格目录" />
        ) : (
          <div className="scrollbar-thin overflow-x-auto">
            <Table className="min-w-[840px]">
              <TableHeader>
                <TableRow>
                  <TableHead>模型 ID</TableHead>
                  <TableHead>来源</TableHead>
                  <TableHead className="text-right">输入价格/M</TableHead>
                  <TableHead className="text-right">输出价格/M</TableHead>
                  <TableHead className="text-right">缓存写入/M</TableHead>
                  <TableHead className="text-right">缓存读取/M</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {pricing.data.models.map((item) => (
                  <TableRow key={item.model}>
                    <TableCell className="font-mono text-xs max-w-[260px]">
                      <div className="truncate" title={item.model}>{item.model}</div>
                    </TableCell>
                    <TableCell>
                      <Badge tone={sourceTone(item.source)}>{sourceLabel(item.source)}</Badge>
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.inputCostPerToken)}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.outputCostPerToken)}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.cacheCreationInputTokenCost)}</TableCell>
                    <TableCell className="text-right font-mono text-xs tabular-nums">{formatUsdPerM(item.pricing.cacheReadInputTokenCost)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </SectionCard>

      <SectionCard
        title="模型清单"
        description="系统同步的模型不可删除，手动添加的可编辑或删除；价格列会按模型名兼容匹配当前价格目录"
        noPadding
      >
        {capabilities.isLoading ? (
          <LoadingState text="加载模型列表..." className="py-8" />
        ) : capabilities.error ? (
          <ErrorState message={extractErrorMessage(capabilities.error)} />
        ) : models.length === 0 ? (
          <EmptyState
            title="暂无模型"
            description="点击「同步能力」获取最新模型列表，或手动添加"
            action={
              <Button size="sm" onClick={() => setAddOpen(true)}>
                <Plus className="h-4 w-4" />手动添加
              </Button>
            }
          />
        ) : (
          <div className="scrollbar-thin overflow-x-auto">
            <Table className="min-w-[980px]">
              <TableHeader>
                <TableRow>
                  <TableHead>模型 ID</TableHead>
                  <TableHead>显示名称</TableHead>
                  <TableHead className="text-right">最大输入</TableHead>
                  <TableHead className="text-right">最大输出</TableHead>
                  <TableHead>缓存支持</TableHead>
                  <TableHead>输入类型</TableHead>
                  <TableHead className="text-right">输入价格/M</TableHead>
                  <TableHead className="text-right">输出价格/M</TableHead>
                  <TableHead className="text-right">缓存写入/M</TableHead>
                  <TableHead className="text-right">缓存读取/M</TableHead>
                  <TableHead>来源</TableHead>
                  <TableHead className="w-20" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {models.map((item) => {
                  const priceItem = findPricing(priceIndex, item.model)
                  const price = priceItem?.pricing
                  const manual = isManual(item)
                  return (
                    <TableRow key={item.model}>
                      <TableCell className="font-mono text-xs max-w-[180px]">
                        <div className="truncate" title={item.model}>{item.model}</div>
                      </TableCell>
                      <TableCell className="text-sm text-muted-foreground max-w-[140px]">
                        <div className="truncate">{item.displayName || '—'}</div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatTokens(item.maxInputTokens)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatTokens(item.maxOutputTokens)}
                      </TableCell>
                      <TableCell>
                        {item.supportsPromptCaching ? (
                          <Badge tone="success">支持</Badge>
                        ) : (
                          <span className="text-xs text-muted-foreground/50">—</span>
                        )}
                      </TableCell>
                      <TableCell className="max-w-[140px] text-xs text-muted-foreground">
                        <div className="truncate" title={item.supportedInputTypes?.join(', ') || ''}>
                          {item.supportedInputTypes?.length ? item.supportedInputTypes.join(', ') : '—'}
                        </div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatUsdPerM(price?.inputCostPerToken)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatUsdPerM(price?.outputCostPerToken)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatUsdPerM(price?.cacheCreationInputTokenCost)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatUsdPerM(price?.cacheReadInputTokenCost)}
                      </TableCell>
                      <TableCell>
                        <Badge tone={sourceTone(item.source)}>
                          {sourceLabel(manual ? 'manual' : item.source)}
                        </Badge>
                      </TableCell>
                      <TableCell>
                        {manual && (
                          <div className="flex items-center gap-1">
                            <Button
                              variant="ghost"
                              size="xs"
                              onClick={() => setEditTarget(item)}
                            >
                              编辑
                            </Button>
                            <Button
                              variant="ghost"
                              size="xs"
                              className="text-destructive hover:text-destructive"
                              onClick={() => handleDelete(item)}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
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

      {/* 弹窗 */}
      <EditModelDialog
        open={addOpen}
        onClose={() => setAddOpen(false)}
      />
      {editTarget && (
        <EditModelDialog
          open
          initial={editTarget}
          initialPricing={findPricing(priceIndex, editTarget.model)?.pricing}
          onClose={() => setEditTarget(null)}
        />
      )}
    </PageContainer>
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

  return (
    <Dialog open={open} onOpenChange={(v) => !v && onClose()}>
      <DialogContent width="max-w-4xl">
        <DialogHeader>
          <DialogTitle>{initial ? '编辑模型能力' : '手动添加模型'}</DialogTitle>
        </DialogHeader>
        <DialogBody className="space-y-4">
          <div className="grid gap-3 md:grid-cols-2">
            <div className="space-y-1.5">
              <Label>模型 ID</Label>
              <Input
                value={form.model}
                placeholder="claude-opus-4-5"
                disabled={!!initial || upsert.isPending}
                onChange={(e) => update('model', e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label>显示名称（可选）</Label>
              <Input
                value={form.displayName}
                placeholder="Claude Opus 4.5"
                disabled={upsert.isPending}
                onChange={(e) => update('displayName', e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label>最大输入 Token</Label>
              <Input
                type="number"
                min={1}
                value={form.maxInputTokens}
                placeholder="200000"
                disabled={upsert.isPending}
                onChange={(e) => update('maxInputTokens', e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label>最大输出 Token</Label>
              <Input
                type="number"
                min={1}
                value={form.maxOutputTokens}
                placeholder="64000"
                disabled={upsert.isPending}
                onChange={(e) => update('maxOutputTokens', e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label>输入类型</Label>
              <div className="flex h-9 items-center gap-4">
                {(['TEXT', 'IMAGE'] as const).map((type) => (
                  <label key={type} className="flex items-center gap-2 text-sm">
                    <Checkbox
                      checked={form.supportedInputTypes[type]}
                      disabled={upsert.isPending}
                      onCheckedChange={(checked) => updateInputType(type, checked === true)}
                    />
                    {type}
                  </label>
                ))}
              </div>
            </div>
            <div className="flex items-center gap-3">
              <Switch
                checked={form.supportsPromptCaching}
                disabled={upsert.isPending}
                onCheckedChange={(checked) => update('supportsPromptCaching', checked)}
              />
              <span className="text-sm">支持 Prompt Caching</span>
            </div>
            <div className="space-y-1.5 md:col-span-2">
              <Label>描述（可选）</Label>
              <Textarea
                rows={3}
                value={form.description}
                disabled={upsert.isPending}
                placeholder="用于说明模型能力或适用场景"
                onChange={(e) => update('description', e.target.value)}
              />
            </div>
          </div>

          <div className="rounded-lg bg-muted/30 p-3">
            <label className="flex items-center gap-2 text-sm font-medium">
              <Checkbox
                checked={form.includePricing}
                disabled={upsert.isPending}
                onCheckedChange={(checked) => update('includePricing', checked === true)}
              />
              同时维护价格
            </label>
            <p className="mt-1 text-xs text-muted-foreground">
              关闭后保存会清除该手动模型的价格；开启后按每百万 Token 填写，缓存价格留空时使用后端默认规则。
            </p>
            {form.includePricing && (
              <div className="mt-3 grid gap-3 md:grid-cols-4">
                <div className="space-y-1.5">
                  <Label>输入 $/M</Label>
                  <Input
                    type="number"
                    min={0}
                    step="0.000001"
                    value={form.inputCostPerMillion}
                    disabled={upsert.isPending}
                    onChange={(e) => update('inputCostPerMillion', e.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label>输出 $/M</Label>
                  <Input
                    type="number"
                    min={0}
                    step="0.000001"
                    value={form.outputCostPerMillion}
                    disabled={upsert.isPending}
                    onChange={(e) => update('outputCostPerMillion', e.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label>缓存写入 $/M</Label>
                  <Input
                    type="number"
                    min={0}
                    step="0.000001"
                    value={form.cacheCreationInputCostPerMillion}
                    disabled={upsert.isPending}
                    onChange={(e) => update('cacheCreationInputCostPerMillion', e.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label>缓存读取 $/M</Label>
                  <Input
                    type="number"
                    min={0}
                    step="0.000001"
                    value={form.cacheReadInputCostPerMillion}
                    disabled={upsert.isPending}
                    onChange={(e) => update('cacheReadInputCostPerMillion', e.target.value)}
                  />
                </div>
              </div>
            )}
          </div>
        </DialogBody>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>取消</Button>
          <Button size="sm" disabled={upsert.isPending} onClick={handleSave}>
            {upsert.isPending ? <Spinner size="sm" /> : null}
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
