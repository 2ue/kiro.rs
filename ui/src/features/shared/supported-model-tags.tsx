import { Plus, RotateCcw, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Badge, Button, Input } from '@/components/ui'
import { useModelCapabilities } from '@/hooks/use-usage'
import type { ModelFilterMode } from '@/types/api'

export function parseSupportedModelItems(value: string): string[] {
  const seen = new Set<string>()
  const models: string[] = []
  for (const item of value.split(/[\s,，;；]+/)) {
    const model = item.trim()
    if (!model) continue
    const key = model.toLowerCase()
    if (seen.has(key)) continue
    seen.add(key)
    models.push(model)
  }
  return models
}

export function mergeSupportedModels(current: string[], incoming: string[]): string[] {
  const seen = new Set<string>()
  const next: string[] = []
  for (const model of [...current, ...incoming]) {
    const trimmed = model.trim()
    if (!trimmed) continue
    const key = trimmed.toLowerCase()
    if (seen.has(key)) continue
    seen.add(key)
    next.push(trimmed)
  }
  return next
}

export function SupportedModelTagsEditor({
  value,
  disabled = false,
  placeholder = 'claude-sonnet-5',
  onChange,
  mode = 'allowlist',
  onModeChange,
  options = [],
  inheritTier = false,
}: {
  value: string[]
  mode?: ModelFilterMode
  onModeChange?: (mode: ModelFilterMode) => void
  options?: string[]
  inheritTier?: boolean
  disabled?: boolean
  placeholder?: string
  onChange: (next: string[]) => void
}) {
  const catalog = useModelCapabilities()
  const [draft, setDraft] = useState('')
  const [cleared, setCleared] = useState<{ models: string[]; draft: string } | null>(null)
  const normalized = useMemo(() => mergeSupportedModels([], value), [value])
  const choices = useMemo(() => mergeSupportedModels(catalog.data?.models.map((item) => item.model) ?? [], [...options, ...normalized]).sort(), [catalog.data?.models, options, normalized])
  const emptyLabel = mode === 'denylist' ? '黑名单为空：不排除任何模型' : inheritTier ? '白名单为空：继承订阅等级模型配置' : '白名单为空：不限制模型'
  useEffect(() => {
    if (normalized.length) setCleared(null)
  }, [value])
  const addDraft = () => {
    const incoming = parseSupportedModelItems(draft)
    if (!incoming.length) return
    setCleared(null)
    onChange(mergeSupportedModels(normalized, incoming))
    setDraft('')
  }
  const removeAt = (index: number) => {
    setCleared(null)
    onChange(normalized.filter((_, itemIndex) => itemIndex !== index))
  }

  return (
    <div className="space-y-2">
      {onModeChange && (
        <div className="space-y-1">
          <label className="text-xs text-muted-foreground">模型筛选模式</label>
          <select aria-label="模型筛选模式" value={mode} disabled={disabled} onChange={(event) => onModeChange(event.target.value as ModelFilterMode)} className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
            <option value="allowlist">白名单：只允许列表内模型</option>
            <option value="denylist">黑名单：只禁止列表内模型</option>
          </select>
          <p className="text-xs text-muted-foreground">{mode === 'denylist' ? '未列出的模型允许调度；不会与订阅等级白名单叠加。' : '非空时只允许所选模型。'} 同一账号仅启用一种模式，切换后当前列表按新模式解释；保存后生效。</p>
        </div>
      )}
      <div className="flex gap-2">
        <select aria-label="选择模型" value="" disabled={disabled || !choices.length} onChange={(event) => { if (event.target.value) onChange(mergeSupportedModels(normalized, [event.target.value])) }} className="h-9 min-w-0 flex-1 rounded-md border border-input bg-background px-3 font-mono text-xs">
          <option value="">{catalog.isLoading ? '加载系统模型…' : '从系统模型清单选择'}</option>
          {choices.map((model) => <option key={model} value={model} disabled={normalized.some((item) => item.toLowerCase() === model.toLowerCase())}>{model}</option>)}
        </select>
        <Button type="button" size="sm" variant="outline" disabled={disabled || !choices.length} onClick={() => onChange(mergeSupportedModels(normalized, choices))}>一键填充</Button>
      </div>
      {catalog.isError && <p className="text-xs text-destructive">系统模型清单加载失败，仍可手动输入模型 ID。</p>}
      <div className="flex min-h-7 flex-wrap items-center justify-between gap-2">
        <span className="text-xs text-muted-foreground" aria-live="polite">{normalized.length ? `已选 ${normalized.length} 个模型` : emptyLabel}</span>
        <div className="flex items-center gap-1">
          {cleared && normalized.length === 0 && !draft && (
            <Button type="button" size="xs" variant="ghost" disabled={disabled} onClick={() => {
              onChange(cleared.models); setDraft(cleared.draft); setCleared(null)
            }}><RotateCcw className="h-3.5 w-3.5" />撤销清空</Button>
          )}
          <Button type="button" size="xs" variant="ghost" disabled={disabled || (!normalized.length && !draft)} onClick={() => {
            setCleared({ models: normalized, draft }); onChange([]); setDraft('')
          }}>清空列表</Button>
        </div>
      </div>
      <div className="flex gap-2">
        <Input
          value={draft}
          disabled={disabled}
          placeholder={placeholder}
          className="font-mono text-xs"
          onChange={(event) => { setCleared(null); setDraft(event.target.value) }}
          onKeyDown={(event) => {
            if (event.key === 'Enter') {
              event.preventDefault()
              addDraft()
            }
          }}
        />
        <Button type="button" size="sm" variant="outline" disabled={disabled || !draft.trim()} onClick={addDraft}>
          <Plus className="h-4 w-4" />添加
        </Button>
      </div>
      <div className="min-h-28 rounded-lg border border-input bg-card p-2">
        {normalized.length === 0 ? (
          <div className="flex h-20 items-center justify-center text-xs text-muted-foreground">{emptyLabel}</div>
        ) : (
          <div className="flex max-h-56 flex-wrap gap-2 overflow-y-auto pr-1">
            {normalized.map((model, index) => (
              <Badge key={`${model}-${index}`} tone="secondary" className="h-auto max-w-full gap-1 rounded-md py-1 font-mono text-[0.68rem]">
                <span className="truncate">{model}</span>
                <button
                  type="button"
                  className="rounded-sm text-muted-foreground hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
                  disabled={disabled}
                  onClick={() => removeAt(index)}
                  aria-label={`删除 ${model}`}
                >
                  <X className="h-3 w-3" />
                </button>
              </Badge>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
