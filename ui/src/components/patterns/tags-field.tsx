import { useMemo, useState, type ClipboardEvent, type KeyboardEvent } from 'react'
import { Plus, X } from 'lucide-react'
import { Badge, Button, Input } from '@/components/ui'
import { MAX_TAG_LENGTH, MAX_TAGS, addTags, cleanTags, splitTagInput } from './tag-utils'

/**
 * 标签输入：已选标签以标签块显示，可逐个删除；回车或逗号添加，粘贴多个标签时按分隔符拆开；
 * 下方列出已有标签，点一下即可添加。交互与 main 的 CredentialTagsField 一致。
 * 已有标签原样显示和保留（不截断、不限数量），64 字符和 32 个的上限只拦新输入的标签。
 */
export function TagsField({
  id,
  value,
  options = [],
  onChange,
  disabled,
  placeholder = '输入标签后按回车或逗号添加',
  emptyText = '未选择标签',
}: {
  id?: string
  value: string[]
  options?: string[]
  onChange: (tags: string[]) => void
  disabled?: boolean
  placeholder?: string
  emptyText?: string
}) {
  const [draft, setDraft] = useState('')
  const selected = useMemo(() => cleanTags(value), [value])
  const full = selected.length >= MAX_TAGS
  const candidates = useMemo(() => {
    const selectedSet = new Set(selected)
    const query = draft.trim().toLocaleLowerCase()
    return cleanTags(options)
      .filter((tag) => !selectedSet.has(tag))
      .filter((tag) => !query || tag.toLocaleLowerCase().includes(query))
      .sort((a, b) => a.localeCompare(b))
  }, [draft, options, selected])

  const commit = (incoming: string[]) => {
    if (!incoming.length) return
    const next = addTags(selected, incoming)
    if (next.length !== selected.length) onChange(next)
  }
  const commitDraft = () => {
    commit(splitTagInput(draft))
    setDraft('')
  }
  const removeTag = (tag: string) => onChange(selected.filter((item) => item !== tag))

  const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter' || event.key === ',' || event.key === '，') {
      event.preventDefault()
      commitDraft()
    } else if (event.key === 'Backspace' && !draft && selected.length > 0) {
      removeTag(selected[selected.length - 1])
    }
  }
  const handlePaste = (event: ClipboardEvent<HTMLInputElement>) => {
    const text = event.clipboardData.getData('text')
    if (!/[,，;；\n]/.test(text)) return
    event.preventDefault()
    commit(splitTagInput(`${draft}${text}`))
    setDraft('')
  }

  return (
    <div className="space-y-2">
      <div className="flex min-h-5 flex-wrap gap-1.5">
        {selected.map((tag) => (
          <Badge key={tag} tone="secondary" className="gap-1">
            {tag}
            <Button
              type="button"
              variant="ghost"
              className="size-4 rounded-sm p-0 text-muted-foreground hover:bg-transparent hover:text-foreground [&_svg]:size-3"
              aria-label={`移除标签 ${tag}`}
              onClick={() => removeTag(tag)}
              disabled={disabled}
            >
              <X />
            </Button>
          </Badge>
        ))}
        {selected.length === 0 && <span className="text-xs text-muted-foreground">{emptyText}</span>}
      </div>
      <div className="flex gap-2">
        <Input
          id={id}
          value={draft}
          maxLength={MAX_TAG_LENGTH * 4}
          placeholder={full ? `最多 ${MAX_TAGS} 个标签` : placeholder}
          disabled={disabled || full}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          // 输完没按回车就去点保存时，不丢掉已经输入的标签。
          onBlur={() => { if (draft.trim()) commitDraft() }}
        />
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          aria-label="添加标签"
          title="添加标签"
          disabled={disabled || !draft.trim() || full}
          onClick={commitDraft}
        >
          <Plus />
        </Button>
      </div>
      {candidates.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {candidates.map((tag) => (
            <Button key={tag} type="button" variant="outline" size="xs" disabled={disabled || full} onClick={() => commit([tag])}>
              {tag}
            </Button>
          ))}
        </div>
      )}
    </div>
  )
}
