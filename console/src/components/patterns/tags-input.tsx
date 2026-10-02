import { useState } from 'react'
import { Plus, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'

const MAX_TAGS = 32
const MAX_TAG_LENGTH = 64

/** 标签输入：回车或逗号添加，可从已有标签中快速选择 */
export function TagsInput({
  value,
  options = [],
  onChange,
  disabled,
  placeholder = '输入标签后回车',
}: {
  value: string[]
  options?: string[]
  onChange: (tags: string[]) => void
  disabled?: boolean
  placeholder?: string
}) {
  const [text, setText] = useState('')
  const add = (raw: string) => {
    const next = raw
      .split(/[,，\n]/)
      .map((t) => t.trim().slice(0, MAX_TAG_LENGTH))
      .filter(Boolean)
    if (!next.length) return
    onChange([...new Set([...value, ...next])].slice(0, MAX_TAGS))
    setText('')
  }
  const suggestions = options.filter((o) => !value.includes(o)).slice(0, 12)

  return (
    <div className="space-y-2">
      {value.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {value.map((tag) => (
            <span key={tag} className="inline-flex items-center gap-0.5 rounded-md border bg-muted/50 py-0.5 pr-0.5 pl-2 text-xs">
              {tag}
              <Button
                variant="ghost"
                size="icon-xs"
                disabled={disabled}
                aria-label={`移除标签 ${tag}`}
                onClick={() => onChange(value.filter((t) => t !== tag))}
              >
                <X />
              </Button>
            </span>
          ))}
        </div>
      )}
      <Input
        value={text}
        disabled={disabled}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ',') {
            e.preventDefault()
            add(text)
          }
        }}
        onBlur={() => add(text)}
      />
      {suggestions.length > 0 && (
        <div className="flex flex-wrap items-center gap-1">
          <span className="text-xs text-muted-foreground">已有标签：</span>
          {suggestions.map((s) => (
            <Button key={s} size="xs" variant="ghost" disabled={disabled} onClick={() => add(s)}>
              <Plus /> {s}
            </Button>
          ))}
        </div>
      )}
    </div>
  )
}
