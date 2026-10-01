import { useIsFetching, useQueryClient } from '@tanstack/react-query'
import { Pause, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { useUiPrefs, type RefreshSecs } from '@/stores/ui-prefs'

const OPTIONS: Array<{ value: RefreshSecs; label: string }> = [
  { value: 0, label: '暂停自动刷新' },
  { value: 10, label: '每 10 秒' },
  { value: 30, label: '每 30 秒' },
  { value: 60, label: '每 60 秒' },
]

export function RefreshControl() {
  const queryClient = useQueryClient()
  const fetching = useIsFetching()
  const secs = useUiPrefs((s) => s.refreshSecs)
  const setSecs = useUiPrefs((s) => s.setRefreshSecs)

  return (
    <div className="flex items-center rounded-lg border bg-background">
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            className="rounded-r-none"
            aria-label="立即刷新"
            onClick={() => queryClient.invalidateQueries({ type: 'active' })}
          >
            <RefreshCw className={cn(fetching > 0 && 'animate-spin')} />
          </Button>
        </TooltipTrigger>
        <TooltipContent>立即刷新当前页面数据</TooltipContent>
      </Tooltip>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="sm" className="num h-7 rounded-l-none border-l px-2 text-xs text-muted-foreground">
            {secs === 0 ? <Pause className="size-3.5" /> : `${secs}s`}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuLabel>自动刷新</DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuRadioGroup value={String(secs)} onValueChange={(v) => setSecs(Number(v) as RefreshSecs)}>
            {OPTIONS.map((o) => (
              <DropdownMenuRadioItem key={o.value} value={String(o.value)}>
                {o.label}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}
