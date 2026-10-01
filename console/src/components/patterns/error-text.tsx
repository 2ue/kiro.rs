import { ChevronDown } from 'lucide-react'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { translateError } from '@/domain/upstream-error'
import { cn } from '@/lib/utils'
import { CopyButton } from './copy-button'

/** 错误展示：中文标题 + 建议 + 可展开原文 */
export function ErrorText({ error, className, compact }: { error?: string | null; className?: string; compact?: boolean }) {
  const t = translateError(error)
  if (!t) return null
  const same = t.title === t.raw
  if (compact) {
    return (
      <span className={cn('text-danger', className)} title={t.raw}>
        {t.title}
      </span>
    )
  }
  return (
    <div className={cn('text-xs', className)}>
      <div className="font-medium text-danger">{t.title}</div>
      {t.hint && <div className="mt-0.5 text-muted-foreground">建议：{t.hint}</div>}
      {!same && (
        <Collapsible className="mt-1">
          <CollapsibleTrigger className="group inline-flex items-center gap-0.5 text-muted-foreground hover:text-foreground">
            <ChevronDown className="size-3 transition-transform group-data-[state=open]:rotate-180" />
            原始错误
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="mt-1 flex items-start gap-1 rounded bg-muted/60 p-1.5">
              <code className="min-w-0 flex-1 font-mono text-2xs break-all whitespace-pre-wrap">{t.raw}</code>
              <CopyButton value={t.raw} label="复制原始错误" />
            </div>
          </CollapsibleContent>
        </Collapsible>
      )}
    </div>
  )
}
