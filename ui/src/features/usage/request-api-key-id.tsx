import { Copy } from 'lucide-react'
import { toast } from 'sonner'
import { Button, Tooltip } from '@/components/ui'
import { formatRequestApiKeyId, normalizeRequestApiKeyId } from '@/lib/request-api-key-id'
import { extractErrorMessage } from '@/lib/utils'

export function RequestApiKeyIdDisplay({ value }: { value?: string }) {
  const normalized = normalizeRequestApiKeyId(value)
  if (!normalized) return <span className="text-muted-foreground">-</span>

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(normalized)
      toast.success('请求渠道 ID 已复制')
    } catch (error) {
      toast.error(`复制失败: ${extractErrorMessage(error)}`)
    }
  }

  return (
    <span className="inline-flex min-w-0 items-center gap-1 font-mono text-xs">
      <Tooltip label={<span className="block max-w-[calc(100vw-3rem)] break-all font-mono sm:max-w-lg">{normalized}</span>}>
        <span className="truncate">{formatRequestApiKeyId(normalized)}</span>
      </Tooltip>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        className="shrink-0"
        title="复制完整请求渠道 ID"
        aria-label="复制完整请求渠道 ID"
        onClick={(event) => {
          event.stopPropagation()
          void copy()
        }}
      >
        <Copy className="h-3.5 w-3.5" />
      </Button>
    </span>
  )
}
