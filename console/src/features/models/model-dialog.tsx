import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { systemApi } from '@/api/endpoints/system'
import type { UpsertManualModelRequest } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { NumberInput } from '@/components/patterns/fields'
import type { ModelRow } from './models-page'

const INPUT_TYPES = ['text', 'image', 'document']

export function ModelDialog({
  row,
  open,
  onOpenChange,
  onSaved,
}: {
  row?: ModelRow
  open: boolean
  onOpenChange: (v: boolean) => void
  onSaved: () => void
}) {
  const [form, setForm] = useState<UpsertManualModelRequest & { withPricing: boolean }>({
    model: '',
    supportedInputTypes: ['text'],
    withPricing: false,
  })
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
      return systemApi.upsertManualModel({
        ...req,
        model: req.model.trim(),
        pricing: withPricing ? req.pricing : undefined,
        clearPricing: !withPricing && !!row?.price ? true : undefined,
      })
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
              <Input
                id="m-id"
                value={form.model}
                disabled={!!row}
                onChange={(e) => setForm((f) => ({ ...f, model: e.target.value }))}
                className="font-mono text-xs"
              />
            </div>
            <div className="col-span-2 space-y-1.5">
              <Label htmlFor="m-name">显示名称</Label>
              <Input
                id="m-name"
                value={form.displayName ?? ''}
                onChange={(e) => setForm((f) => ({ ...f, displayName: e.target.value || undefined }))}
              />
            </div>
            <div className="space-y-1.5">
              <Label>最大输入</Label>
              <NumberInput
                value={form.maxInputTokens ?? null}
                allowEmpty
                min={0}
                suffix="tokens"
                onChange={(v) => setForm((f) => ({ ...f, maxInputTokens: v ?? undefined }))}
              />
            </div>
            <div className="space-y-1.5">
              <Label>最大输出</Label>
              <NumberInput
                value={form.maxOutputTokens ?? null}
                allowEmpty
                min={0}
                suffix="tokens"
                onChange={(v) => setForm((f) => ({ ...f, maxOutputTokens: v ?? undefined }))}
              />
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-4 text-sm">
            {INPUT_TYPES.map((t) => (
              <label key={t} className="flex items-center gap-1.5">
                <Checkbox
                  checked={form.supportedInputTypes.includes(t)}
                  onCheckedChange={(v) =>
                    setForm((f) => ({
                      ...f,
                      supportedInputTypes: v ? [...new Set([...f.supportedInputTypes, t])] : f.supportedInputTypes.filter((x) => x !== t),
                    }))
                  }
                />
                {t}
              </label>
            ))}
            <label className="ml-auto flex items-center gap-2">
              支持缓存
              <Switch
                checked={!!form.supportsPromptCaching}
                onCheckedChange={(v) => setForm((f) => ({ ...f, supportsPromptCaching: v }))}
              />
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
                <NumberInput
                  value={p.inputCostPerMillion}
                  min={0}
                  step={0.01}
                  suffix="$/M"
                  onChange={(v) => setP('inputCostPerMillion', v)}
                />
              </div>
              <div className="space-y-1.5">
                <Label>输出</Label>
                <NumberInput
                  value={p.outputCostPerMillion}
                  min={0}
                  step={0.01}
                  suffix="$/M"
                  onChange={(v) => setP('outputCostPerMillion', v)}
                />
              </div>
              <div className="space-y-1.5">
                <Label>缓存写</Label>
                <NumberInput
                  value={p.cacheCreationInputCostPerMillion ?? null}
                  allowEmpty
                  min={0}
                  step={0.01}
                  suffix="$/M"
                  onChange={(v) => setP('cacheCreationInputCostPerMillion', v)}
                />
              </div>
              <div className="space-y-1.5">
                <Label>缓存读</Label>
                <NumberInput
                  value={p.cacheReadInputCostPerMillion ?? null}
                  allowEmpty
                  min={0}
                  step={0.01}
                  suffix="$/M"
                  onChange={(v) => setP('cacheReadInputCostPerMillion', v)}
                />
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
