import { Eye, EyeOff } from 'lucide-react'
import { Button, Input } from '@/components/ui'

// ============================================================================
// SecretInput — 密码/Token 输入框，带显隐切换按钮
// 从 credential-card.tsx 和 credential-dialogs.tsx 提取，两处共用
// ============================================================================

export function SecretInput({
  value, onChange, visible, onToggle, disabled, placeholder,
}: {
  value: string
  onChange: (v: string) => void
  visible: boolean
  onToggle: () => void
  disabled?: boolean
  placeholder?: string
}) {
  return (
    <div className="relative">
      <Input
        className="pr-10"
        type={visible ? 'text' : 'password'}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        className="absolute right-1 top-1"
        onClick={onToggle}
        disabled={disabled}
        title={visible ? '隐藏' : '显示'}
      >
        {visible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
      </Button>
    </div>
  )
}
