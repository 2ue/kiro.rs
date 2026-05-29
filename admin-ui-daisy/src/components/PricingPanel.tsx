import { useEffect, useMemo, useState } from 'react'
import { AlertTriangle, Edit3, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { Alert as DaisyAlert, Button, Checkbox, Input, Loading, Modal, Table, Textarea, Toggle } from 'react-daisyui'
import { Badge, EmptyState, ErrorState, FieldLabel, LoadingState, ModalShell, SectionCard, StatCard } from '@/components/common'
import { formatCompact, formatDate, formatNumber, formatPricePerMillion } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useDeleteManualModel,
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
  useUpsertManualModel,
} from '@/hooks/use-usage'
import type { ModelCapabilityItem, ModelPricing, UpsertManualModelRequest } from '@/types/api'

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

const emptyManualForm: ManualModelForm = {
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
}

function dollarsPerMillion(value?: number): string {
  if (value === undefined || value === null || !Number.isFinite(value)) return ''
  return String(Number((value * 1_000_000).toFixed(6)))
}

function sourceTone(source?: string): 'neutral' | 'primary' | 'success' | 'warning' | 'info' {
  if (!source) return 'neutral'
  if (source === 'manual') return 'warning'
  if (source.includes('kiro')) return 'success'
  if (source === 'litellm') return 'info'
  if (source.includes('seed') || source === 'built-in') return 'primary'
  return 'neutral'
}

function sourceLabel(source?: string): string {
  if (!source) return '-'
  if (source === 'manual') return '手动'
  if (source.includes('kiro')) return '上游'
  if (source === 'litellm') return '价格源'
  if (source.includes('seed')) return 'Seed'
  if (source === 'built-in') return '内置'
  return source
}

function pricingByModel(pricing?: { models: { model: string; pricing: ModelPricing; source?: string }[] }) {
  const map = new Map<string, { pricing: ModelPricing; source?: string }>()
  for (const item of pricing?.models || []) {
    map.set(item.model, { pricing: item.pricing, source: item.source })
  }
  return map
}

function formFromModel(item: ModelCapabilityItem, price?: ModelPricing): ManualModelForm {
  return {
    model: item.model,
    displayName: item.displayName || item.model,
    description: item.description || '',
    maxInputTokens: item.maxInputTokens ? String(item.maxInputTokens) : '',
    maxOutputTokens: item.maxOutputTokens ? String(item.maxOutputTokens) : '',
    supportsPromptCaching: item.supportsPromptCaching ?? true,
    supportedInputTypes: {
      TEXT: item.supportedInputTypes?.includes('TEXT') ?? true,
      IMAGE: item.supportedInputTypes?.includes('IMAGE') ?? false,
    },
    includePricing: Boolean(price),
    inputCostPerMillion: dollarsPerMillion(price?.inputCostPerToken),
    outputCostPerMillion: dollarsPerMillion(price?.outputCostPerToken),
    cacheCreationInputCostPerMillion: dollarsPerMillion(price?.cacheCreationInputTokenCost),
    cacheReadInputCostPerMillion: dollarsPerMillion(price?.cacheReadInputTokenCost),
  }
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

function buildManualPayload(form: ManualModelForm): UpsertManualModelRequest {
  const supportedInputTypes = Object.entries(form.supportedInputTypes)
    .filter(([, enabled]) => enabled)
    .map(([value]) => value)
  const maxInputTokens = positiveInteger(form.maxInputTokens)
  const maxOutputTokens = positiveInteger(form.maxOutputTokens)
  if (Number.isNaN(maxInputTokens) || Number.isNaN(maxOutputTokens)) {
    throw new Error('输入上限和输出上限必须是大于 0 的整数，或留空')
  }
  const payload: UpsertManualModelRequest = {
    model: form.model.trim(),
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

function ManualModelModal({
  open,
  initial,
  onClose,
}: {
  open: boolean
  initial: ManualModelForm | null
  onClose: () => void
}) {
  const [form, setForm] = useState<ManualModelForm>(initial || emptyManualForm)
  const upsert = useUpsertManualModel()
  const editing = Boolean(initial?.model)

  useEffect(() => {
    if (open) setForm(initial || emptyManualForm)
  }, [initial, open])

  const update = <K extends keyof ManualModelForm>(key: K, value: ManualModelForm[K]) => {
    setForm((prev) => ({ ...prev, [key]: value }))
  }

  const submit = () => {
    let payload: UpsertManualModelRequest
    try {
      payload = buildManualPayload(form)
    } catch (error) {
      toast.error(extractErrorMessage(error))
      return
    }
    upsert.mutate(payload, {
      onSuccess: (response) => {
        toast.success(response.message)
        onClose()
      },
      onError: (error) => toast.error(`保存失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <ModalShell open={open} title={editing ? `编辑手动模型：${initial?.model}` : '手动添加模型'} width="max-w-4xl" onClose={onClose}>
      <div className="grid gap-3 md:grid-cols-2">
        <FieldLabel title="模型 ID" description="最终会按这个模型 ID 请求 Kiro 上游。">
          <Input bordered size="sm" value={form.model} disabled={editing || upsert.isPending} onChange={(event) => update('model', event.target.value)} placeholder="claude-opus-5-20270101" />
        </FieldLabel>
        <FieldLabel title="显示名">
          <Input bordered size="sm" value={form.displayName} disabled={upsert.isPending} onChange={(event) => update('displayName', event.target.value)} placeholder="Claude Opus 5" />
        </FieldLabel>
        <FieldLabel title="输入上限">
          <Input bordered size="sm" type="number" min={1} value={form.maxInputTokens} disabled={upsert.isPending} onChange={(event) => update('maxInputTokens', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="输出上限">
          <Input bordered size="sm" type="number" min={1} value={form.maxOutputTokens} disabled={upsert.isPending} onChange={(event) => update('maxOutputTokens', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="输入类型">
          <div className="flex h-9 items-center gap-4">
            {(['TEXT', 'IMAGE'] as const).map((type) => (
              <label key={type} className="flex items-center gap-2 text-sm">
                <Checkbox size="sm" checked={form.supportedInputTypes[type]} disabled={upsert.isPending} onChange={(event) => update('supportedInputTypes', { ...form.supportedInputTypes, [type]: event.target.checked })} />
                {type}
              </label>
            ))}
          </div>
        </FieldLabel>
        <FieldLabel title="缓存能力">
          <label className="flex h-9 items-center gap-2 text-sm">
            <Toggle size="sm" checked={form.supportsPromptCaching} disabled={upsert.isPending} onChange={(event) => update('supportsPromptCaching', event.target.checked)} />
            支持 prompt cache
          </label>
        </FieldLabel>
        <div className="md:col-span-2">
          <FieldLabel title="描述">
            <Textarea bordered className="min-h-20" value={form.description} disabled={upsert.isPending} onChange={(event) => update('description', event.target.value)} placeholder="可选" />
          </FieldLabel>
        </div>
      </div>

      <div className="mt-4 rounded-lg border border-base-300 p-3">
        <label className="flex items-center gap-2 text-sm font-medium">
          <Checkbox size="sm" checked={form.includePricing} disabled={upsert.isPending} onChange={(event) => update('includePricing', event.target.checked)} />
          同时配置计价
        </label>
        {form.includePricing && (
          <div className="mt-3 grid gap-3 md:grid-cols-4">
            <FieldLabel title="输入 $/M">
              <Input bordered size="sm" type="number" min={0} step="0.000001" value={form.inputCostPerMillion} disabled={upsert.isPending} onChange={(event) => update('inputCostPerMillion', event.target.value)} />
            </FieldLabel>
            <FieldLabel title="输出 $/M">
              <Input bordered size="sm" type="number" min={0} step="0.000001" value={form.outputCostPerMillion} disabled={upsert.isPending} onChange={(event) => update('outputCostPerMillion', event.target.value)} />
            </FieldLabel>
            <FieldLabel title="缓存写入 $/M" description="留空按输入价格 1.25 倍。">
              <Input bordered size="sm" type="number" min={0} step="0.000001" value={form.cacheCreationInputCostPerMillion} disabled={upsert.isPending} onChange={(event) => update('cacheCreationInputCostPerMillion', event.target.value)} />
            </FieldLabel>
            <FieldLabel title="缓存读取 $/M" description="留空按输入价格 0.1 倍。">
              <Input bordered size="sm" type="number" min={0} step="0.000001" value={form.cacheReadInputCostPerMillion} disabled={upsert.isPending} onChange={(event) => update('cacheReadInputCostPerMillion', event.target.value)} />
            </FieldLabel>
          </div>
        )}
      </div>

      <Modal.Actions>
        <Button type="button" color="ghost" size="sm" disabled={upsert.isPending} onClick={onClose}>取消</Button>
        <Button type="button" color="primary" size="sm" disabled={upsert.isPending} onClick={submit}>
          {upsert.isPending && <Loading size="xs" />}
          保存
        </Button>
      </Modal.Actions>
    </ModalShell>
  )
}

export function PricingPanel() {
  const [manualOpen, setManualOpen] = useState(false)
  const [editing, setEditing] = useState<ManualModelForm | null>(null)
  const pricing = useModelPricing()
  const syncPricing = useSyncModelPricing()
  const capabilities = useModelCapabilities()
  const syncCapabilities = useSyncModelCapabilities()
  const deleteManual = useDeleteManualModel()
  const priceMap = useMemo(() => pricingByModel(pricing.data), [pricing.data])

  const openAdd = () => {
    setEditing(null)
    setManualOpen(true)
  }

  const openEdit = (item: ModelCapabilityItem) => {
    setEditing(formFromModel(item, priceMap.get(item.model)?.pricing))
    setManualOpen(true)
  }

  const removeManual = (model: string) => {
    if (!window.confirm(`确认删除手动模型 ${model}？`)) return
    deleteManual.mutate(model, {
      onSuccess: (response) => toast.success(response.message),
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

  const syncPrice = () => {
    syncPricing.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) toast.warning(`同步失败，继续使用当前价格目录: ${status.lastError}`)
        else toast.success(`模型价格已同步：${status.modelCount} 个模型`)
      },
      onError: (error) => toast.error(`同步失败: ${extractErrorMessage(error)}`),
    })
  }

  const syncCapability = () => {
    syncCapabilities.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) toast.warning(`模型能力同步失败，继续使用当前目录: ${status.lastError}`)
        else toast.success(`模型能力已同步：${status.modelCount} 个模型`)
      },
      onError: (error) => toast.error(`同步失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <div className="space-y-4">
      <ManualModelModal open={manualOpen} initial={editing} onClose={() => setManualOpen(false)} />

      <div className="metric-grid">
        <StatCard title="模型能力" value={<Badge tone={capabilities.data?.available ? 'success' : 'error'}>{capabilities.data?.available ? '可用' : '不可用'}</Badge>} />
        <StatCard title="能力来源" value={capabilities.data?.source || '-'} />
        <StatCard title="模型能力数" value={formatNumber(capabilities.data?.modelCount || 0)} />
        <StatCard title="能力同步" value={formatDate(capabilities.data?.lastSyncedAt)} />
      </div>

      <SectionCard
        title="Kiro 模型能力目录"
        description="从 Kiro 上游同步可用模型、上下文窗口、输出上限和缓存能力；手动模型作为补充保留。"
        actions={
          <>
            <Button type="button" color="primary" size="sm" onClick={openAdd}>
              <Plus className="h-4 w-4" />
              手动添加模型
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={syncCapability} disabled={syncCapabilities.isPending}>
              {syncCapabilities.isPending ? <Loading size="xs" /> : <RefreshCw className="h-4 w-4" />}
              同步模型能力
            </Button>
          </>
        }
      >
        {capabilities.data?.lastError && <WarningAlert text={capabilities.data.lastError} />}
        {capabilities.isLoading ? (
          <LoadingState />
        ) : capabilities.error ? (
          <ErrorState text={extractErrorMessage(capabilities.error)} />
        ) : !capabilities.data?.models.length ? (
          <EmptyState text="暂无模型能力数据" />
        ) : (
          <div className="table-panel table-panel-tall">
            <Table zebra size="sm" className="data-table min-w-[1040px]">
              <Table.Head>
                <span>模型</span>
                <span>显示名</span>
                <span>来源</span>
                <span className="text-right">输入上限</span>
                <span className="text-right">输出上限</span>
                <span className="text-right">缓存</span>
                <span>输入类型</span>
                <span className="text-right">操作</span>
              </Table.Head>
              <Table.Body>
                {capabilities.data.models.map((item) => {
                  const isManual = item.source === 'manual'
                  return (
                    <Table.Row key={item.model} hover>
                      <span className="font-medium">{item.model}</span>
                      <span>{item.displayName}</span>
                      <span><Badge tone={sourceTone(item.source)}>{sourceLabel(item.source)}</Badge></span>
                      <span className="text-right font-mono">{formatCompact(item.maxInputTokens)}</span>
                      <span className="text-right font-mono">{formatCompact(item.maxOutputTokens)}</span>
                      <span className="text-right">{item.supportsPromptCaching === undefined ? '-' : item.supportsPromptCaching ? '支持' : '不支持'}</span>
                      <span className="text-base-content/60">{item.supportedInputTypes.length ? item.supportedInputTypes.join(', ') : '-'}</span>
                      <span className="flex justify-end gap-1">
                        {isManual ? (
                          <>
                            <Button type="button" color="ghost" size="xs" onClick={() => openEdit(item)} title="编辑">
                              <Edit3 className="h-3.5 w-3.5" />
                            </Button>
                            <Button type="button" color="ghost" size="xs" disabled={deleteManual.isPending} onClick={() => removeManual(item.model)} title="删除">
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </>
                        ) : '-'}
                      </span>
                    </Table.Row>
                  )
                })}
              </Table.Body>
            </Table>
          </div>
        )}
      </SectionCard>

      <div className="metric-grid">
        <StatCard title="价格状态" value={<Badge tone={pricing.data?.available ? 'success' : 'error'}>{pricing.data?.available ? '可用' : '不可用'}</Badge>} />
        <StatCard title="来源" value={pricing.data?.source || '-'} />
        <StatCard title="模型数" value={formatNumber(pricing.data?.modelCount || 0)} />
        <StatCard title="最后同步" value={formatDate(pricing.data?.lastSyncedAt)} />
      </div>

      <SectionCard
        title="关注模型价格"
        description={pricing.data?.sourceUrl || '加载中...'}
        actions={
          <Button type="button" variant="outline" size="sm" onClick={syncPrice} disabled={syncPricing.isPending}>
            {syncPricing.isPending ? <Loading size="xs" /> : <RefreshCw className="h-4 w-4" />}
            同步模型价格
          </Button>
        }
      >
        {pricing.data?.lastError && <WarningAlert text={pricing.data.lastError} />}
        {pricing.isLoading ? (
          <LoadingState />
        ) : pricing.error ? (
          <ErrorState text={extractErrorMessage(pricing.error)} />
        ) : !pricing.data?.models.length ? (
          <EmptyState text="暂无价格数据" />
        ) : (
          <div className="table-panel">
            <Table zebra size="sm" className="data-table min-w-[960px]">
              <Table.Head>
                <span>模型</span>
                <span>来源</span>
                <span className="text-right">输入</span>
                <span className="text-right">输出</span>
                <span className="text-right">缓存写入</span>
                <span className="text-right">缓存读取</span>
              </Table.Head>
              <Table.Body>
                {pricing.data.models.map((item) => (
                  <Table.Row key={item.model} hover>
                    <span className="font-medium">{item.model}</span>
                    <span><Badge tone={sourceTone(item.source)}>{sourceLabel(item.source)}</Badge></span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.inputCostPerToken)}</span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.outputCostPerToken)}</span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.cacheCreationInputTokenCost)}</span>
                    <span className="text-right font-mono">{formatPricePerMillion(item.pricing.cacheReadInputTokenCost)}</span>
                  </Table.Row>
                ))}
              </Table.Body>
            </Table>
          </div>
        )}
      </SectionCard>
    </div>
  )
}

function WarningAlert({ text }: { text: string }) {
  return (
    <DaisyAlert status="warning" className="mb-4 text-sm">
      <AlertTriangle className="h-4 w-4" />
      <span className="break-all">{text}</span>
    </DaisyAlert>
  )
}
