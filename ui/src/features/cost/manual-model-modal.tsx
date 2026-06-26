import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { extractErrorMessage } from '@/lib/utils'
import { useUpsertManualModel } from '@/hooks/use-usage'
import type { ModelCapabilityItem, ModelPriceItem, UpsertManualModelRequest } from '@/types/api'
import { ModalShell, Field, FieldGrid } from '@/components/patterns'
import { Button, Checkbox, Input, Switch, Textarea } from '@/components/ui'

export interface ManualModelForm {
  model: string
  displayName: string
  description: string
  maxInputTokens: string
  maxOutputTokens: string
  supportsPromptCaching: boolean
  supportedInputTypes: { TEXT: boolean; IMAGE: boolean }
  inputCostPerMillion: string
  outputCostPerMillion: string
  cacheCreationInputCostPerMillion: string
  cacheReadInputCostPerMillion: string
  clearPricing: boolean
  includePricing: boolean
}

export function emptyForm(): ManualModelForm {
  return {
    model: '',
    displayName: '',
    description: '',
    maxInputTokens: '200000',
    maxOutputTokens: '64000',
    supportsPromptCaching: false,
    supportedInputTypes: { TEXT: true, IMAGE: false },
    inputCostPerMillion: '',
    outputCostPerMillion: '',
    cacheCreationInputCostPerMillion: '',
    cacheReadInputCostPerMillion: '',
    clearPricing: false,
    includePricing: false,
  }
}

export function formFromCapability(item: ModelCapabilityItem, priceItem?: ModelPriceItem): ManualModelForm {
  return {
    model: item.model,
    displayName: item.displayName ?? '',
    description: item.description ?? '',
    maxInputTokens: item.maxInputTokens != null ? String(item.maxInputTokens) : '',
    maxOutputTokens: item.maxOutputTokens != null ? String(item.maxOutputTokens) : '',
    supportsPromptCaching: item.supportsPromptCaching ?? false,
    supportedInputTypes: {
      TEXT: item.supportedInputTypes?.includes('TEXT') ?? true,
      IMAGE: item.supportedInputTypes?.includes('IMAGE') ?? false,
    },
    inputCostPerMillion: priceItem ? String(priceItem.pricing.inputCostPerToken * 1_000_000) : '',
    outputCostPerMillion: priceItem ? String(priceItem.pricing.outputCostPerToken * 1_000_000) : '',
    cacheCreationInputCostPerMillion: priceItem ? String(priceItem.pricing.cacheCreationInputTokenCost * 1_000_000) : '',
    cacheReadInputCostPerMillion: priceItem ? String(priceItem.pricing.cacheReadInputTokenCost * 1_000_000) : '',
    clearPricing: false,
    includePricing: Boolean(priceItem),
  }
}

function parsePositiveInteger(value: string): number | undefined | typeof Number.NaN {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const parsed = Number(trimmed)
  if (!Number.isFinite(parsed) || parsed <= 0 || !Number.isInteger(parsed)) return Number.NaN
  return parsed
}

export function ManualModelModal({
  open,
  initialForm,
  onClose,
}: {
  open: boolean
  initialForm?: ManualModelForm | null
  onClose: () => void
}) {
  const [form, setForm] = useState<ManualModelForm>(initialForm ?? emptyForm())
  const upsert = useUpsertManualModel()
  const isEdit = !!initialForm?.model

  useEffect(() => {
    if (open) setForm(initialForm ?? emptyForm())
  }, [open, initialForm])

  const set = <K extends keyof ManualModelForm>(k: K, v: ManualModelForm[K]) =>
    setForm((f) => ({ ...f, [k]: v }))

  const handleSubmit = async () => {
    if (!form.model.trim()) {
      toast.error('模型 ID 不能为空')
      return
    }

    const maxInput = parsePositiveInteger(form.maxInputTokens)
    const maxOutput = parsePositiveInteger(form.maxOutputTokens)
    if (Number.isNaN(maxInput)) {
      toast.error('输入上限必须是大于 0 的整数，或留空')
      return
    }
    if (Number.isNaN(maxOutput)) {
      toast.error('输出上限必须是大于 0 的整数，或留空')
      return
    }

    const supportedInputTypes = (Object.entries(form.supportedInputTypes) as [string, boolean][])
      .filter(([, enabled]) => enabled)
      .map(([type]) => type)

    const payload: UpsertManualModelRequest = {
      model: form.model.trim(),
      displayName: form.displayName.trim() || undefined,
      description: form.description.trim() || undefined,
      maxInputTokens: maxInput,
      maxOutputTokens: maxOutput,
      supportsPromptCaching: form.supportsPromptCaching,
      supportedInputTypes,
      clearPricing: isEdit ? form.clearPricing : !form.includePricing,
    }

    // 创建模式：includePricing=true 时才附价格
    // 编辑模式：clearPricing=false 且有输入时附价格
    const shouldAttachPricing = isEdit
      ? !form.clearPricing && (form.inputCostPerMillion || form.outputCostPerMillion)
      : form.includePricing && (form.inputCostPerMillion || form.outputCostPerMillion)

    if (shouldAttachPricing) {
      const input = Number(form.inputCostPerMillion)
      const output = Number(form.outputCostPerMillion)
      if (!Number.isFinite(input) || input < 0 || !Number.isFinite(output) || output < 0) {
        toast.error('价格必须是有效的非负数')
        return
      }
      payload.pricing = {
        inputCostPerMillion: input,
        outputCostPerMillion: output,
        cacheCreationInputCostPerMillion: form.cacheCreationInputCostPerMillion ? Number(form.cacheCreationInputCostPerMillion) : undefined,
        cacheReadInputCostPerMillion: form.cacheReadInputCostPerMillion ? Number(form.cacheReadInputCostPerMillion) : undefined,
      }
    }

    try {
      await upsert.mutateAsync(payload)
      toast.success(isEdit ? '模型已更新' : '模型已添加')
      onClose()
    } catch (e) {
      toast.error(`操作失败: ${extractErrorMessage(e)}`)
    }
  }

  return (
    <ModalShell open={open} onClose={onClose} title={isEdit ? '编辑手动模型' : '添加手动模型'} width="max-w-xl">
      <div className="space-y-4 text-sm">
        <FieldGrid>
          <Field label="模型 ID" required>
            <Input
              className="h-8 text-xs"
              value={form.model}
              disabled={isEdit}
              onChange={(e) => set('model', e.target.value)}
              placeholder="claude-3-5-sonnet-20241022"
            />
          </Field>
          <Field label="显示名称">
            <Input className="h-8 text-xs" value={form.displayName} onChange={(e) => set('displayName', e.target.value)} placeholder="可选" />
          </Field>
          <Field label="最大输入 Token" description="正整数，留空表示不限制">
            <Input
              type="number"
              min={1}
              step={1}
              className="h-8 text-xs"
              value={form.maxInputTokens}
              onChange={(e) => set('maxInputTokens', e.target.value)}
              placeholder="200000"
            />
          </Field>
          <Field label="最大输出 Token" description="正整数，留空表示不限制">
            <Input
              type="number"
              min={1}
              step={1}
              className="h-8 text-xs"
              value={form.maxOutputTokens}
              onChange={(e) => set('maxOutputTokens', e.target.value)}
              placeholder="64000"
            />
          </Field>
        </FieldGrid>

        <div className="col-span-2">
          <Field label="描述">
            <Textarea
              className="min-h-16 text-xs"
              value={form.description}
              onChange={(e) => set('description', e.target.value)}
              placeholder="可选，模型用途说明"
            />
          </Field>
        </div>

        <div className="flex flex-wrap items-center gap-6">
          <div className="flex items-center gap-2">
            <label className="text-xs text-muted-foreground">支持 Prompt 缓存</label>
            <Switch checked={form.supportsPromptCaching} onCheckedChange={(v) => set('supportsPromptCaching', v)} />
          </div>
          <div className="flex items-center gap-3">
            <span className="text-xs text-muted-foreground">输入类型</span>
            {(['TEXT', 'IMAGE'] as const).map((type) => (
              <label key={type} className="flex items-center gap-1.5 text-xs cursor-pointer select-none">
                <Checkbox
                  checked={form.supportedInputTypes[type]}
                  onCheckedChange={(checked) =>
                    set('supportedInputTypes', { ...form.supportedInputTypes, [type]: Boolean(checked) })
                  }
                />
                {type}
              </label>
            ))}
          </div>
        </div>

        <div className="rounded-lg border border-border p-3 space-y-3">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium">价格配置（每百万 Token，USD）</span>
            {isEdit ? (
              <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
                <Switch checked={form.clearPricing} onCheckedChange={(v) => set('clearPricing', v)} />
                清除价格
              </label>
            ) : (
              <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
                <Switch checked={form.includePricing} onCheckedChange={(v) => set('includePricing', v)} />
                同时配置计价
              </label>
            )}
          </div>
          {(isEdit ? !form.clearPricing : form.includePricing) && (
            <FieldGrid>
              <Field label="输入单价">
                <Input type="number" min={0} step="0.000001" className="h-8 text-xs" value={form.inputCostPerMillion} onChange={(e) => set('inputCostPerMillion', e.target.value)} placeholder="3.00" />
              </Field>
              <Field label="输出单价">
                <Input type="number" min={0} step="0.000001" className="h-8 text-xs" value={form.outputCostPerMillion} onChange={(e) => set('outputCostPerMillion', e.target.value)} placeholder="15.00" />
              </Field>
              <Field label="缓存写入单价" description="留空按输入 ×1.25">
                <Input type="number" min={0} step="0.000001" className="h-8 text-xs" value={form.cacheCreationInputCostPerMillion} onChange={(e) => set('cacheCreationInputCostPerMillion', e.target.value)} placeholder="3.75" />
              </Field>
              <Field label="缓存读取单价" description="留空按输入 ×0.1">
                <Input type="number" min={0} step="0.000001" className="h-8 text-xs" value={form.cacheReadInputCostPerMillion} onChange={(e) => set('cacheReadInputCostPerMillion', e.target.value)} placeholder="0.30" />
              </Field>
            </FieldGrid>
          )}
        </div>

        <div className="flex justify-end gap-2 pt-1 border-t border-border">
          <Button variant="outline" size="sm" onClick={onClose}>取消</Button>
          <Button size="sm" onClick={handleSubmit} disabled={upsert.isPending}>
            {upsert.isPending ? '保存中...' : isEdit ? '保存' : '添加'}
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}
