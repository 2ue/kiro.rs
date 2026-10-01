import { useState } from 'react'
import { Check, Copy } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'

export function CopyButton({ value, label = '复制', size = 'icon-xs' }: { value: string; label?: string; size?: 'icon-xs' | 'icon-sm' }) {
  const [copied, setCopied] = useState(false)
  return (
    <Button
      variant="ghost"
      size={size}
      aria-label={label}
      onClick={async (e) => {
        e.stopPropagation()
        try {
          await navigator.clipboard.writeText(value)
          setCopied(true)
          setTimeout(() => setCopied(false), 1200)
        } catch {
          toast.error('复制失败，请手动选择复制')
        }
      }}
    >
      {copied ? <Check /> : <Copy />}
    </Button>
  )
}
