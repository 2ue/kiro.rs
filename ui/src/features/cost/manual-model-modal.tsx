import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { extractErrorMessage } from '@/lib/utils'
import { useUpsertManualModel } from '@/hooks/use-usage'
import type { ModelCapabilityItem, ModelPriceItem, UpsertManualModelRequest } from '@/types/api'
import { ModalShell, Field, FieldGrid } from '@/components/patterns'
import { Button, Input, Switch } from '@/components/ui'

export interface ManualModelForm {
  model: string
  displayName: string
  description: string
  maxInputTokens: string
  maxOutputTokens: string
  supportsPromptCaching: boolean
  inputCostPerMillion: string
  outputCostPerMillion: string
  cacheCreationInputCostPerMillion: string
  cacheReadInputCostPerMillion: string
  clearPricing: boolean
}

export function emptyForm(): ManualModelForm {
  return {
    model: '',
    displayName: '',
    description: '',
    maxInputTokens: '',
    maxOutputTokens: '',
    supportsPromptCaching: false,
    inputCostPerMillion: '',
    outputCostPerMillion: '',
    cacheCreationInputCostPerMillion: '',
    cacheReadInputCostPerMillion: '',
    clearPricing: false,
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
    inputCostPerMillion: priceItem ? String(priceItem.pricing.inputCostPerToken * 1_000_000) : '',
    outputCostPerMillion: priceItem ? String(priceItem.pricing.outputCostPerToken * 1_000_000) : '',
    cacheCreationInputCostPerMillion: priceItem ? String(priceItem.pricing.cacheCreationInputTokenCost * 1_000_000) : '',
    cacheReadInputCostPerMillion: priceItem ? String(priceItem.pricing.cacheReadInputTokenCost * 1_000_000) : '',
    clearPricing: false,
  }
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
    const payload: UpsertManualModelRequest = {
      model: form.model.trim(),
      displayName: form.displayName.trim() || undefined,
      description: form.description.trim() || undefined,
      maxInputTokens: form.maxInputTokens ? Number(form.maxInputTokens) : undefined,
      maxOutputTokens: form.maxOutputTokens ? Number(form.maxOutputTokens) : undefined,
      supportsPromptCaching: form.supportsPromptCaching,
      supportedInputTypes: ['text'],
      clearPricing: form.clearPricing,
    }
    if (!form.clearPricing && (form.inputCostPerMillion || form.outputCostPerMillion)) {
      payload.pricing = {
        inputCostPerMillion: Number(form.inputCostPerMillion) || 0,
        outputCostPerMillion: Number(form.outputCostPerMillion) || 0,
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
    <ModalShell open={open} onClose={onClose} title={isEdit ? '编辑手动模型' : '添加手动模型'} width="max-w-lg">
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
          <Field label="最大输入 Token">
            <Input type="number" className="h-8 text-xs" value={form.maxInputTokens} onChange={(e) => set('maxInputTokens', e.target.value)} />
          </Field>
          <Field label="最大输出 Token">
            <Input type="number" className="h-8 text-xs" value={form.maxOutputTokens} onChange={(e) => set('maxOutputTokens', e.target.value)} />
          </Field>
        </FieldGrid>

        <div className="flex items-center gap-3">
          <label className="text-xs text-muted-foreground w-28">支持 Prompt 缓存</label>
          <Switch checked={form.supportsPromptCaching} onCheckedChange={(v) => set('supportsPromptCaching', v)} />
        </div>

        <div className="rounded-lg border border-border p-3 space-y-3">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium">价格配置（每百万 Token，USD）</span>
            {isEdit && (
              <label className="flex items-center gap-2 text-xs text-muted-foreground cursor-pointer">
                <Switch checked={form.clearPricing} onCheckedChange={(v) => set('clearPricing', v)} />
                清除价格
              </label>
            )}
          </div>
          {!form.clearPricing && (
            <FieldGrid>
              <Field label="输入单价">
                <Input type="number" className="h-8 text-xs" value={form.inputCostPerMillion} onChange={(e) => set('inputCostPerMillion', e.target.value)} placeholder="3.00" />
              </Field>
              <Field label="输出单价">
                <Input type="number" className="h-8 text-xs" value={form.outputCostPerMillion} onChange={(e) => set('outputCostPerMillion', e.target.value)} placeholder="15.00" />
              </Field>
              <Field label="缓存写入单价">
                <Input type="number" className="h-8 text-xs" value={form.cacheCreationInputCostPerMillion} onChange={(e) => set('cacheCreationInputCostPerMillion', e.target.value)} placeholder="3.75" />
              </Field>
              <Field label="缓存读取单价">
                <Input type="number" className="h-8 text-xs" value={form.cacheReadInputCostPerMillion} onChange={(e) => set('cacheReadInputCostPerMillion', e.target.value)} placeholder="0.30" />
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
