import { useEffect, useMemo, useState } from 'react'
import { RefreshCw, AlertTriangle, Plus, Edit3, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import {
  useDeleteManualModel,
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
  useUpsertManualModel,
} from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
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

function formatDate(value?: string): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function formatPrice(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return `$${(value * 1_000_000).toFixed(2)}/M`
}

function formatTokens(value?: number): string {
  if (!value || !Number.isFinite(value)) return '-'
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`
  if (value >= 1000) return `${Math.round(value / 1000)}K`
  return String(value)
}

function dollarsPerMillion(value?: number): string {
  if (value === undefined || value === null || !Number.isFinite(value)) return ''
  return String(Number((value * 1_000_000).toFixed(6)))
}

function sourceVariant(source?: string): 'outline' | 'secondary' | 'success' | 'warning' {
  if (source === 'manual') return 'warning'
  if (source?.includes('kiro')) return 'success'
  if (source === 'litellm' || source?.includes('seed') || source === 'built-in') return 'secondary'
  return 'outline'
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

function priceMapFrom(data?: { models: { model: string; pricing: ModelPricing; source?: string }[] }) {
  const map = new Map<string, { pricing: ModelPricing; source?: string }>()
  for (const item of data?.models || []) {
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
    clearPricing: !form.includePricing,
    supportedInputTypes: Object.entries(form.supportedInputTypes)
      .filter(([, enabled]) => enabled)
      .map(([type]) => type),
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

function ManualModelDialog({
  open,
  initial,
  onOpenChange,
}: {
  open: boolean
  initial: ManualModelForm | null
  onOpenChange: (open: boolean) => void
}) {
  const [form, setForm] = useState<ManualModelForm>(initial || emptyManualForm)
  const upsert = useUpsertManualModel()
  const editing = Boolean(initial?.model)

  useEffect(() => {
    if (open) setForm(initial || emptyManualForm)
  }, [initial, open])

  const set = <K extends keyof ManualModelForm>(key: K, value: ManualModelForm[K]) => {
    setForm((prev) => ({ ...prev, [key]: value }))
  }

  const submit = (event: React.FormEvent) => {
    event.preventDefault()
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
        onOpenChange(false)
      },
      onError: (error) => toast.error(`保存失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => !upsert.isPending && onOpenChange(nextOpen)}>
      <DialogContent className="max-h-[88vh] max-w-4xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{editing ? `编辑手动模型：${initial?.model}` : '手动添加模型'}</DialogTitle>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          <div className="grid gap-4 md:grid-cols-2">
            <Field title="模型 ID">
              <Input value={form.model} disabled={editing || upsert.isPending} onChange={(event) => set('model', event.target.value)} placeholder="claude-opus-5-20270101" />
            </Field>
            <Field title="显示名">
              <Input value={form.displayName} disabled={upsert.isPending} onChange={(event) => set('displayName', event.target.value)} placeholder="Claude Opus 5" />
            </Field>
            <Field title="输入上限">
              <Input type="number" min={1} value={form.maxInputTokens} disabled={upsert.isPending} onChange={(event) => set('maxInputTokens', event.target.value)} />
            </Field>
            <Field title="输出上限">
              <Input type="number" min={1} value={form.maxOutputTokens} disabled={upsert.isPending} onChange={(event) => set('maxOutputTokens', event.target.value)} />
            </Field>
            <Field title="输入类型">
              <div className="flex h-10 items-center gap-4">
                {(['TEXT', 'IMAGE'] as const).map((type) => (
                  <label key={type} className="flex items-center gap-2 text-sm">
                    <Checkbox checked={form.supportedInputTypes[type]} disabled={upsert.isPending} onCheckedChange={(checked) => set('supportedInputTypes', { ...form.supportedInputTypes, [type]: checked === true })} />
                    {type}
                  </label>
                ))}
              </div>
            </Field>
            <Field title="缓存能力">
              <div className="flex h-10 items-center gap-2">
                <Switch checked={form.supportsPromptCaching} disabled={upsert.isPending} onCheckedChange={(checked) => set('supportsPromptCaching', checked)} />
                <span className="text-sm">支持 prompt cache</span>
              </div>
            </Field>
            <div className="space-y-2 md:col-span-2">
              <label className="text-sm font-medium">描述</label>
              <textarea
                value={form.description}
                disabled={upsert.isPending}
                onChange={(event) => set('description', event.target.value)}
                className="min-h-24 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                placeholder="可选"
              />
            </div>
          </div>

          <div className="rounded-md border bg-muted/20 p-4">
            <label className="flex items-center gap-2 text-sm font-medium">
              <Checkbox checked={form.includePricing} disabled={upsert.isPending} onCheckedChange={(checked) => set('includePricing', checked === true)} />
              同时配置计价
            </label>
            {form.includePricing && (
              <div className="mt-4 grid gap-4 md:grid-cols-4">
                <Field title="输入 $/M">
                  <Input type="number" min={0} step="0.000001" value={form.inputCostPerMillion} disabled={upsert.isPending} onChange={(event) => set('inputCostPerMillion', event.target.value)} />
                </Field>
                <Field title="输出 $/M">
                  <Input type="number" min={0} step="0.000001" value={form.outputCostPerMillion} disabled={upsert.isPending} onChange={(event) => set('outputCostPerMillion', event.target.value)} />
                </Field>
                <Field title="缓存写入 $/M">
                  <Input type="number" min={0} step="0.000001" value={form.cacheCreationInputCostPerMillion} disabled={upsert.isPending} onChange={(event) => set('cacheCreationInputCostPerMillion', event.target.value)} />
                </Field>
                <Field title="缓存读取 $/M">
                  <Input type="number" min={0} step="0.000001" value={form.cacheReadInputCostPerMillion} disabled={upsert.isPending} onChange={(event) => set('cacheReadInputCostPerMillion', event.target.value)} />
                </Field>
              </div>
            )}
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" disabled={upsert.isPending} onClick={() => onOpenChange(false)}>取消</Button>
            <Button type="submit" disabled={upsert.isPending}>保存</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function Field({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="space-y-2">
      <label className="text-sm font-medium">{title}</label>
      {children}
    </div>
  )
}

export function ModelPricingPanel() {
  const [manualOpen, setManualOpen] = useState(false)
  const [editing, setEditing] = useState<ManualModelForm | null>(null)
  const pricing = useModelPricing()
  const syncPricing = useSyncModelPricing()
  const capabilities = useModelCapabilities()
  const syncCapabilities = useSyncModelCapabilities()
  const deleteManual = useDeleteManualModel()
  const data = pricing.data
  const capabilityData = capabilities.data
  const priceMap = useMemo(() => priceMapFrom(data), [data])

  const handleSync = () => {
    syncPricing.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) toast.warning(`同步失败，继续使用当前价格目录: ${status.lastError}`)
        else toast.success(`模型价格已同步：${status.modelCount} 个模型`)
      },
      onError: (error) => toast.error(`同步失败: ${extractErrorMessage(error)}`),
    })
  }

  const handleSyncCapabilities = () => {
    syncCapabilities.mutate(undefined, {
      onSuccess: (status) => {
        if (status.lastError) toast.warning(`模型能力同步失败，继续使用当前目录: ${status.lastError}`)
        else toast.success(`模型能力已同步：${status.modelCount} 个模型`)
      },
      onError: (error) => toast.error(`同步失败: ${extractErrorMessage(error)}`),
    })
  }

  const openAdd = () => {
    setEditing(null)
    setManualOpen(true)
  }

  const openEdit = (item: ModelCapabilityItem) => {
    setEditing(formFromModel(item, priceMap.get(item.model)?.pricing))
    setManualOpen(true)
  }

  const removeManual = (model: string) => {
    if (!confirm(`确认删除手动模型 ${model}？`)) return
    deleteManual.mutate(model, {
      onSuccess: (response) => toast.success(response.message),
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <div className="space-y-4">
      <ManualModelDialog open={manualOpen} initial={editing} onOpenChange={setManualOpen} />

      <div className="grid gap-4 md:grid-cols-4">
        <StatCard title="模型能力" value={<Badge variant={capabilityData?.available ? 'success' : 'destructive'}>{capabilityData?.available ? '可用' : '不可用'}</Badge>} />
        <StatCard title="能力来源" value={capabilityData?.source || '-'} />
        <StatCard title="模型能力数" value={capabilityData?.modelCount || 0} />
        <StatCard title="能力同步" value={formatDate(capabilityData?.lastSyncedAt)} small />
      </div>

      <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 md:flex-row md:items-center md:justify-between">
        <div className="min-w-0">
          <div className="font-medium">Kiro 模型能力目录</div>
          <div className="text-sm text-muted-foreground">
            从 Kiro 上游同步可用模型、上下文窗口、输出上限和缓存能力；手动模型作为补充保留。
          </div>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" onClick={openAdd}>
            <Plus className="h-4 w-4" />
            手动添加模型
          </Button>
          <Button variant="outline" size="sm" onClick={handleSyncCapabilities} disabled={syncCapabilities.isPending}>
            <RefreshCw className={`h-4 w-4 ${syncCapabilities.isPending ? 'animate-spin' : ''}`} />
            同步模型能力
          </Button>
        </div>
      </div>

      {capabilityData?.lastError && <WarningBox text={capabilityData.lastError} />}

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">模型能力</CardTitle>
        </CardHeader>
        <CardContent>
          {capabilities.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : capabilities.error ? (
            <div className="py-8 text-center text-destructive">{extractErrorMessage(capabilities.error)}</div>
          ) : !capabilityData?.models.length ? (
            <div className="py-8 text-center text-muted-foreground">暂无模型能力数据</div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[1040px] text-sm">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">模型</th>
                    <th className="px-3 py-2 font-medium">显示名</th>
                    <th className="px-3 py-2 font-medium">来源</th>
                    <th className="px-3 py-2 font-medium text-right">输入上限</th>
                    <th className="px-3 py-2 font-medium text-right">输出上限</th>
                    <th className="px-3 py-2 font-medium text-right">缓存</th>
                    <th className="px-3 py-2 font-medium">输入类型</th>
                    <th className="px-3 py-2 font-medium text-right">操作</th>
                  </tr>
                </thead>
                <tbody>
                  {capabilityData.models.map((item) => {
                    const isManual = item.source === 'manual'
                    return (
                      <tr key={item.model} className="border-b last:border-0">
                        <td className="px-3 py-2 font-medium">{item.model}</td>
                        <td className="px-3 py-2">{item.displayName}</td>
                        <td className="px-3 py-2"><Badge variant={sourceVariant(item.source)}>{sourceLabel(item.source)}</Badge></td>
                        <td className="px-3 py-2 text-right font-mono">{formatTokens(item.maxInputTokens)}</td>
                        <td className="px-3 py-2 text-right font-mono">{formatTokens(item.maxOutputTokens)}</td>
                        <td className="px-3 py-2 text-right">
                          {item.supportsPromptCaching === undefined ? '-' : item.supportsPromptCaching ? '支持' : '不支持'}
                        </td>
                        <td className="px-3 py-2 text-muted-foreground">
                          {item.supportedInputTypes.length ? item.supportedInputTypes.join(', ') : '-'}
                        </td>
                        <td className="px-3 py-2 text-right">
                          {isManual ? (
                            <div className="flex justify-end gap-1">
                              <Button type="button" variant="ghost" size="icon" className="h-8 w-8" onClick={() => openEdit(item)} title="编辑">
                                <Edit3 className="h-4 w-4" />
                              </Button>
                              <Button type="button" variant="ghost" size="icon" className="h-8 w-8" disabled={deleteManual.isPending} onClick={() => removeManual(item.model)} title="删除">
                                <Trash2 className="h-4 w-4" />
                              </Button>
                            </div>
                          ) : '-'}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>

      <div className="grid gap-4 md:grid-cols-4">
        <StatCard title="状态" value={<Badge variant={data?.available ? 'success' : 'destructive'}>{data?.available ? '可用' : '不可用'}</Badge>} />
        <StatCard title="来源" value={data?.source || '-'} />
        <StatCard title="模型数" value={data?.modelCount || 0} />
        <StatCard title="最后同步" value={formatDate(data?.lastSyncedAt)} small />
      </div>

      <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 md:flex-row md:items-center md:justify-between">
        <div className="min-w-0">
          <div className="font-medium">模型价格目录</div>
          <div className="truncate text-sm text-muted-foreground" title={data?.sourceUrl}>
            {data?.sourceUrl || '加载中...'}
          </div>
        </div>
        <Button variant="outline" size="sm" onClick={handleSync} disabled={syncPricing.isPending}>
          <RefreshCw className={`h-4 w-4 ${syncPricing.isPending ? 'animate-spin' : ''}`} />
          同步模型价格
        </Button>
      </div>

      {data?.lastError && <WarningBox text={data.lastError} />}

      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="text-base">关注模型价格</CardTitle>
        </CardHeader>
        <CardContent>
          {pricing.isLoading ? (
            <div className="py-8 text-center text-muted-foreground">加载中...</div>
          ) : pricing.error ? (
            <div className="py-8 text-center text-destructive">{extractErrorMessage(pricing.error)}</div>
          ) : !data?.models.length ? (
            <div className="py-8 text-center text-muted-foreground">暂无价格数据</div>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full min-w-[960px] text-sm">
                <thead>
                  <tr className="border-b text-left text-muted-foreground">
                    <th className="px-3 py-2 font-medium">模型</th>
                    <th className="px-3 py-2 font-medium">来源</th>
                    <th className="px-3 py-2 font-medium text-right">输入</th>
                    <th className="px-3 py-2 font-medium text-right">输出</th>
                    <th className="px-3 py-2 font-medium text-right">缓存写入</th>
                    <th className="px-3 py-2 font-medium text-right">缓存读取</th>
                  </tr>
                </thead>
                <tbody>
                  {data.models.map((item) => (
                    <tr key={item.model} className="border-b last:border-0">
                      <td className="px-3 py-2 font-medium">{item.model}</td>
                      <td className="px-3 py-2"><Badge variant={sourceVariant(item.source)}>{sourceLabel(item.source)}</Badge></td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.inputCostPerToken)}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.outputCostPerToken)}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.cacheCreationInputTokenCost)}</td>
                      <td className="px-3 py-2 text-right font-mono">{formatPrice(item.pricing.cacheReadInputTokenCost)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

function StatCard({ title, value, small }: { title: string; value: React.ReactNode; small?: boolean }) {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm font-medium text-muted-foreground">{title}</CardTitle>
      </CardHeader>
      <CardContent>
        <div className={small ? 'text-sm font-medium' : 'text-xl font-bold'}>{value}</div>
      </CardContent>
    </Card>
  )
}

function WarningBox({ text }: { text: string }) {
  return (
    <div className="flex items-start gap-2 rounded-lg border border-amber-300 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="break-all">{text}</div>
    </div>
  )
}
