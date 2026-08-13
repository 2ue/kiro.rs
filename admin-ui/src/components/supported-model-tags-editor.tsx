import { useMemo, useState } from 'react'
import { Plus, X } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

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
  const seen = new Set(current.map((model) => model.trim().toLowerCase()).filter(Boolean))
  const next = current.map((model) => model.trim()).filter(Boolean)
  for (const model of incoming) {
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
  placeholder = 'claude-sonnet-4.5',
  onChange,
}: {
  value: string[]
  disabled?: boolean
  placeholder?: string
  onChange: (next: string[]) => void
}) {
  const [draft, setDraft] = useState('')
  const normalized = useMemo(() => mergeSupportedModels([], value), [value])

  const addDraft = () => {
    const incoming = parseSupportedModelItems(draft)
    if (!incoming.length) return
    onChange(mergeSupportedModels(normalized, incoming))
    setDraft('')
  }

  const removeAt = (index: number) => {
    onChange(normalized.filter((_, itemIndex) => itemIndex !== index))
  }

  return (
    <div className="space-y-2">
      <div className="flex gap-2">
        <Input
          value={draft}
          disabled={disabled}
          placeholder={placeholder}
          className="font-mono text-xs"
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') {
              event.preventDefault()
              addDraft()
            }
          }}
        />
        <Button type="button" size="sm" variant="outline" disabled={disabled || !draft.trim()} onClick={addDraft}>
          <Plus className="h-4 w-4" />
          添加
        </Button>
      </div>
      <div className="min-h-28 rounded-md border bg-background p-2">
        {normalized.length === 0 ? (
          <div className="flex h-20 items-center justify-center text-xs text-muted-foreground">空列表表示不限制模型</div>
        ) : (
          <div className="flex max-h-56 flex-wrap gap-2 overflow-y-auto pr-1">
            {normalized.map((model, index) => (
              <Badge key={`${model}-${index}`} variant="secondary" className="h-auto max-w-full gap-1 rounded-md py-1 font-mono text-[0.68rem]">
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
