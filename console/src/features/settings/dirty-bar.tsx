import { useState } from 'react'
import { CircleAlert, Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { FIELD_BY_PATH } from './registry'
import type { ConfigDraft } from './use-config-draft'
import { validateConfig } from './validate'

function show(v: unknown): string {
  if (v === undefined) return '—'
  if (typeof v === 'boolean') return v ? '开' : '关'
  if (Array.isArray(v)) return v.length ? v.join(', ') : '（空）'
  if (typeof v === 'object' && v !== null) return JSON.stringify(v)
  const s = String(v)
  return s.length > 80 ? `${s.slice(0, 80)}…` : s
}

/** 粘性保存条：显示修改数量、查看差异、放弃、保存 */
export function DirtyBar({ cfg }: { cfg: ConfigDraft }) {
  const [open, setOpen] = useState(false)
  if (!cfg.dirty) return null

  const trySave = () => {
    const errors = validateConfig(cfg.draft ?? {})
    if (errors.length) {
      toast.error('配置校验未通过', { description: errors.join('\n') })
      return
    }
    cfg.save.mutate(undefined, { onSuccess: () => setOpen(false) })
  }

  return (
    <>
      <div className="sticky bottom-4 z-20 mx-auto flex w-full max-w-3xl items-center gap-3 rounded-xl border bg-popover px-4 py-2.5 shadow-lg animate-in fade-in-0 slide-in-from-bottom-2">
        <span className="size-2 rounded-full bg-primary" />
        <span className="text-sm">
          <span className="num font-medium">{cfg.changes.length}</span> 项修改未保存
        </span>
        {cfg.remoteChanged && (
          <span className="flex items-center gap-1 text-xs text-warning">
            <CircleAlert className="size-3.5" /> 远端配置已更新，保存时会自动合并
          </span>
        )}
        <div className="ml-auto flex gap-2">
          <Button variant="ghost" size="sm" onClick={() => setOpen(true)}>
            查看差异
          </Button>
          <Button variant="ghost" size="sm" onClick={() => cfg.reset()}>
            放弃
          </Button>
          <Button size="sm" onClick={trySave} disabled={cfg.save.isPending}>
            {cfg.save.isPending && <Loader2 className="animate-spin" />}保存
          </Button>
        </div>
      </div>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>待保存的修改</DialogTitle>
            <DialogDescription>保存后新请求立即生效</DialogDescription>
          </DialogHeader>
          <div className="max-h-[60svh] divide-y overflow-y-auto rounded-lg border">
            {cfg.changes.map((c) => {
              const meta = FIELD_BY_PATH.get(c.path)
              return (
                <div key={c.path} className="grid grid-cols-[1fr_auto] gap-2 px-3 py-2 text-sm">
                  <div className="min-w-0">
                    <div className="font-medium">{meta?.label ?? c.path}</div>
                    <div className="truncate font-mono text-xs text-muted-foreground">
                      {meta ? `${meta.sectionTitle} · ${meta.group}` : c.path}
                    </div>
                  </div>
                  <div className="num flex items-center gap-2 text-xs">
                    <span className="rounded bg-danger-subtle px-1.5 py-0.5 text-danger line-through">{show(c.before)}</span>
                    <span>→</span>
                    <span className="rounded bg-success-subtle px-1.5 py-0.5 text-success">{show(c.after)}</span>
                  </div>
                </div>
              )
            })}
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setOpen(false)}>
              继续编辑
            </Button>
            <Button onClick={trySave} disabled={cfg.save.isPending}>
              保存 {cfg.changes.length} 项修改
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
