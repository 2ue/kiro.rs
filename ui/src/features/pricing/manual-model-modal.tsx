import * as React from 'react'
import { toast } from 'sonner'
import { extractErrorMessage } from '@/lib/utils'
import { useUpsertManualModel } from '@/hooks/use-usage'
import type { ModelCapabilityItem, ModelPricing, UpsertManualModelRequest } from '@/types/api'
import { ModalShell, Field, FieldGrid } from '@/components/patterns'
import { Button, Checkbox, Input, Textarea, Switch, Spinner, Separator } from '@/components/ui'

export type ManualModelForm = {
  model: string
  displayName: string
  description: string
  maxInputTokens: string
  maxOutputTokens: string
  supportsPromptCaching: boolean
  supportedInputTypes: { TEXT: boolean; IMAGE: boolean }
  includePricing: boolean
  inputCostPerMillion: string
  outputCostPerMillion: string
  cacheCreationInputCostPerMillion: string
  cacheReadInputCostPerMillion: string
}

export const emptyManualForm: ManualModelForm = {
  model: '',
  displayName: '',
  description: '',
  maxInputTokens: '200000',
  maxOutputTokens: '64000',
  supportsPromptCaching: true,
  supportedInputTypes: { TEXT: true, IMAGE: true },
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

export function formFromModel(item: ModelCapabilityItem, price?: ModelPricing): ManualModelForm {
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

export function ManualModelModal({
  open,
  initial,
  onClose,
}: {
  open: boolean
  initial: ManualModelForm | null
  onClose: () => void
}) {
  const [form, setForm] = React.useState<ManualModelForm>(initial || emptyManualForm)
  const upsert = useUpsertManualModel()
  const editing = Boolean(initial?.model)

  React.useEffect(() => {
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

  const busy = upsert.isPending

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={editing ? `编辑手动模型：${initial?.model}` : '手动添加模型'}
      width="max-w-3xl"
      footer={
        <>
          <Button variant="outline" size="sm" disabled={busy} onClick={onClose}>
            取消
          </Button>
          <Button size="sm" disabled={busy} onClick={submit}>
            {busy && <Spinner size="sm" />}
            保存
          </Button>
        </>
      }
    >
      <FieldGrid>
        <Field label="模型 ID" description="最终会按这个模型 ID 请求 Kiro 服务。">
          <Input
            value={form.model}
            disabled={editing || busy}
            onChange={(e) => update('model', e.target.value)}
            placeholder="claude-opus-5-20270101"
          />
        </Field>
        <Field label="显示名">
          <Input
            value={form.displayName}
            disabled={busy}
            onChange={(e) => update('displayName', e.target.value)}
            placeholder="Claude Opus 5"
          />
        </Field>
        <Field label="输入上限">
          <Input
            type="number"
            min={1}
            value={form.maxInputTokens}
            disabled={busy}
            onChange={(e) => update('maxInputTokens', e.target.value)}
          />
        </Field>
        <Field label="输出上限">
          <Input
            type="number"
            min={1}
            value={form.maxOutputTokens}
            disabled={busy}
            onChange={(e) => update('maxOutputTokens', e.target.value)}
          />
        </Field>
        <Field label="输入类型">
          <div className="flex h-9 items-center gap-4">
            {(['TEXT', 'IMAGE'] as const).map((type) => (
              <label key={type} className="flex items-center gap-2 text-sm">
                <Checkbox
                  checked={form.supportedInputTypes[type]}
                  disabled={busy}
                  onCheckedChange={(checked) =>
                    update('supportedInputTypes', {
                      ...form.supportedInputTypes,
                      [type]: checked === true,
                    })
                  }
                />
                {type}
              </label>
            ))}
          </div>
        </Field>
        <Field label="缓存能力">
          <label className="flex h-9 items-center gap-2 text-sm">
            <Switch
              checked={form.supportsPromptCaching}
              disabled={busy}
              onCheckedChange={(checked) => update('supportsPromptCaching', checked)}
            />
            支持 prompt cache
          </label>
        </Field>
      </FieldGrid>

      <Field label="描述" className="mt-4">
        <Textarea
          value={form.description}
          disabled={busy}
          onChange={(e) => update('description', e.target.value)}
          placeholder="可选"
        />
      </Field>

      <Separator className="my-4" />

      <label className="flex items-center gap-2 text-sm font-medium">
        <Checkbox
          checked={form.includePricing}
          disabled={busy}
          onCheckedChange={(checked) => update('includePricing', checked === true)}
        />
        同时配置计价
      </label>
      {form.includePricing && (
        <FieldGrid min="11rem" className="mt-3">
          <Field label="输入 $/M">
            <Input
              type="number"
              min={0}
              step="0.000001"
              value={form.inputCostPerMillion}
              disabled={busy}
              onChange={(e) => update('inputCostPerMillion', e.target.value)}
            />
          </Field>
          <Field label="输出 $/M">
            <Input
              type="number"
              min={0}
              step="0.000001"
              value={form.outputCostPerMillion}
              disabled={busy}
              onChange={(e) => update('outputCostPerMillion', e.target.value)}
            />
          </Field>
          <Field label="缓存写入 $/M" description="留空按输入价格 1.25 倍。">
            <Input
              type="number"
              min={0}
              step="0.000001"
              value={form.cacheCreationInputCostPerMillion}
              disabled={busy}
              onChange={(e) => update('cacheCreationInputCostPerMillion', e.target.value)}
            />
          </Field>
          <Field label="缓存读取 $/M" description="留空按输入价格 0.1 倍。">
            <Input
              type="number"
              min={0}
              step="0.000001"
              value={form.cacheReadInputCostPerMillion}
              disabled={busy}
              onChange={(e) => update('cacheReadInputCostPerMillion', e.target.value)}
            />
          </Field>
        </FieldGrid>
      )}
    </ModalShell>
  )
}
