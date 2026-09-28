import { useMemo, useState } from 'react'
import { Plus, X } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

const MAX_TAGS = 32
const MAX_TAG_LENGTH = 64

function normalizeTag(value: string): string {
  return value.trim().slice(0, MAX_TAG_LENGTH)
}

export function normalizeCredentialTags(tags: string[]): string[] {
  const seen = new Set<string>()
  return tags
    .map(normalizeTag)
    .filter((tag) => tag && !seen.has(tag) && seen.add(tag))
    .slice(0, MAX_TAGS)
}

export function CredentialTagsField({
  value,
  options,
  onChange,
  disabled,
}: {
  value: string[]
  options: string[]
  onChange: (tags: string[]) => void
  disabled?: boolean
}) {
  const [draft, setDraft] = useState('')
  const selected = useMemo(() => normalizeCredentialTags(value), [value])
  const candidates = useMemo(() => {
    const selectedSet = new Set(selected)
    const query = draft.trim().toLocaleLowerCase()
    return Array.from(new Set(options.map(normalizeTag).filter(Boolean)))
      .filter((tag) => !selectedSet.has(tag))
      .filter((tag) => !query || tag.toLocaleLowerCase().includes(query))
      .sort((a, b) => a.localeCompare(b))
  }, [draft, options, selected])

  const addTag = (raw: string) => {
    const tag = normalizeTag(raw)
    if (!tag) return
    const next = normalizeCredentialTags([...selected, tag])
    if (next.length === selected.length) return
    onChange(next)
    setDraft('')
  }

  const removeTag = (tag: string) => {
    onChange(selected.filter((item) => item !== tag))
  }

  const handleKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter' || event.key === ',') {
      event.preventDefault()
      addTag(draft)
    }
  }

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-1.5">
        {selected.map((tag) => (
          <Badge key={tag} variant="secondary" className="gap-1">
            {tag}
            <button
              type="button"
              className="rounded-sm text-muted-foreground hover:text-foreground"
              aria-label={`移除标签 ${tag}`}
              onClick={() => removeTag(tag)}
              disabled={disabled}
            >
              <X className="h-3 w-3" />
            </button>
          </Badge>
        ))}
        {selected.length === 0 && <span className="text-xs text-muted-foreground">未选择标签</span>}
      </div>
      <div className="flex gap-2">
        <Input
          value={draft}
          maxLength={MAX_TAG_LENGTH}
          placeholder="输入标签后按 Enter"
          disabled={disabled || selected.length >= MAX_TAGS}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={handleKeyDown}
        />
        <Button
          type="button"
          variant="outline"
          size="icon"
          aria-label="添加标签"
          title="添加标签"
          disabled={disabled || !draft.trim() || selected.length >= MAX_TAGS}
          onClick={() => addTag(draft)}
        >
          <Plus className="h-4 w-4" />
        </Button>
      </div>
      {candidates.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {candidates.map((tag) => (
            <Button
              key={tag}
              type="button"
              variant="outline"
              size="sm"
              disabled={disabled || selected.length >= MAX_TAGS}
              onClick={() => addTag(tag)}
            >
              {tag}
            </Button>
          ))}
        </div>
      )}
    </div>
  )
}
