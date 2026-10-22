import * as React from 'react'
import { ChevronDown, X } from 'lucide-react'
import { Badge, Button, Checkbox, Input, Popover, PopoverContent, PopoverTrigger } from '@/components/ui'
import { cn } from '@/lib/utils'

/**
 * Multi-select of routes. Only configured routes can be picked; a selected route that is no
 * longer configured stays visible (marked) so it can be removed.
 */
export function RouteSelector({
  routes,
  value,
  disabled,
  onChange,
}: {
  routes: string[]
  value: string[]
  disabled?: boolean
  onChange: (next: string[]) => void
}) {
  const [open, setOpen] = React.useState(false)
  const [search, setSearch] = React.useState('')
  const known = new Set(routes)
  const selected = new Set(value)
  const keyword = search.trim().toLowerCase()
  const visible = routes.filter((route) => !keyword || route.toLowerCase().includes(keyword))

  const toggle = (route: string, checked: boolean) => {
    const next = new Set(selected)
    if (checked) next.add(route)
    else next.delete(route)
    // Keep the configured order, unknown routes last.
    onChange([...routes.filter((item) => next.has(item)), ...value.filter((item) => !known.has(item) && next.has(item))])
  }

  return (
    <div className={cn('flex min-h-9 flex-wrap items-center gap-1.5 rounded-md border bg-background px-2 py-1.5', disabled && 'opacity-60')}>
      {value.length === 0 && <span className="px-1 text-xs text-muted-foreground">{disabled ? '不限制时无需选择' : '未选路由'}</span>}
      {value.map((route) => (
        <Badge key={route} tone={known.has(route) ? 'primary' : 'error'} className="gap-1 font-mono">
          {route}
          {!known.has(route) && '（已不存在）'}
          {!disabled && (
            <button type="button" className="ml-0.5 rounded hover:bg-black/10" aria-label={`移除 ${route}`} onClick={() => toggle(route, false)}>
              <X className="size-3" />
            </button>
          )}
        </Badge>
      ))}
      <Popover open={open && !disabled} onOpenChange={(next) => { setOpen(next); if (!next) setSearch('') }}>
        <PopoverTrigger asChild>
          <Button type="button" size="xs" variant="ghost" className="ml-auto" disabled={disabled} aria-label="选择路由">
            选择路由<ChevronDown />
          </Button>
        </PopoverTrigger>
        <PopoverContent align="end" className="w-72 p-2">
          <Input className="mb-2" placeholder="搜索路由" value={search} onChange={(event) => setSearch(event.target.value)} aria-label="搜索路由" />
          <div className="max-h-64 space-y-0.5 overflow-y-auto">
            {visible.length === 0 ? (
              <p className="py-4 text-center text-xs text-muted-foreground">没有匹配的路由</p>
            ) : (
              visible.map((route) => (
                <label key={route} className="flex cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-sm hover:bg-muted">
                  <Checkbox checked={selected.has(route)} onCheckedChange={(checked) => toggle(route, Boolean(checked))} aria-label={route} />
                  <span className="font-mono">{route}</span>
                </label>
              ))
            )}
          </div>
          <p className="mt-2 border-t pt-2 text-xs text-muted-foreground">只能选择路由策略里已有的路由，新路由请先在路由策略页添加。</p>
        </PopoverContent>
      </Popover>
    </div>
  )
}
