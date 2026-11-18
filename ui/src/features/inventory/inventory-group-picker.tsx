import { useState } from 'react'
import { Button, Input, Select, SelectContent, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from '@/components/ui'

const NONE = '__none__'
const NEW = '__new__'
/** 后端的分组名上限（字符）。 */
export const INVENTORY_GROUP_NAME_MAX = 64

/**
 * 选择已有分组、输入新分组或不分组。value 为空字符串表示不分组；
 * 不在已有列表里的值按新分组显示在输入框里。
 */
export function InventoryGroupPicker({
  value,
  groups,
  onChange,
  disabled,
  noneLabel = '不分组',
  className = 'w-56',
}: {
  value: string
  groups: string[]
  onChange: (value: string) => void
  disabled?: boolean
  noneLabel?: string
  className?: string
}) {
  const [creating, setCreating] = useState(false)
  const showInput = creating || (value !== '' && !groups.includes(value))

  if (showInput) {
    return (
      <div className={`flex items-center gap-1.5 ${className}`}>
        <Input
          autoFocus={creating}
          value={value}
          maxLength={INVENTORY_GROUP_NAME_MAX}
          placeholder="输入新分组名"
          className="h-8 text-xs"
          disabled={disabled}
          onChange={(e) => onChange(e.target.value)}
          aria-label="新分组名"
        />
        {groups.length > 0 && (
          <Button type="button" variant="ghost" size="xs" disabled={disabled} onClick={() => { setCreating(false); onChange('') }}>
            选已有
          </Button>
        )}
      </div>
    )
  }

  return (
    <Select
      value={value || NONE}
      disabled={disabled}
      onValueChange={(next) => {
        if (next === NEW) {
          setCreating(true)
          onChange('')
        } else {
          onChange(next === NONE ? '' : next)
        }
      }}
    >
      <SelectTrigger size="sm" className={className} aria-label="分组"><SelectValue /></SelectTrigger>
      <SelectContent>
        <SelectItem value={NONE}>{noneLabel}</SelectItem>
        {groups.map((group) => <SelectItem key={group} value={group}>{group}</SelectItem>)}
        <SelectSeparator />
        <SelectItem value={NEW}>新建分组…</SelectItem>
      </SelectContent>
    </Select>
  )
}
