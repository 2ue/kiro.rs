import { useRouterState } from '@tanstack/react-router'
import { LogOut, Monitor, Moon, Rows3, Rows4, Search, Sun } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Kbd } from '@/components/ui/kbd'
import { Separator } from '@/components/ui/separator'
import { SidebarTrigger } from '@/components/ui/sidebar'
import { useUiPrefs, type Density, type ThemePref } from '@/stores/ui-prefs'
import { useAuth } from './auth'
import { useCommandPalette } from './command-palette'
import { findNavItem, NAV } from './nav'
import { RefreshControl } from './refresh-control'
import { WindowPicker } from './window-picker'

/** 使用全局统计窗口的页面 */
const WINDOWED = new Set(['/overview', '/billing'])

export function Topbar() {
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const item = findNavItem(pathname)
  const group = NAV.find((g) => g.items.includes(item!))
  const palette = useCommandPalette()
  const { logout } = useAuth()
  const theme = useUiPrefs((s) => s.theme)
  const setTheme = useUiPrefs((s) => s.setTheme)
  const density = useUiPrefs((s) => s.density)
  const setDensity = useUiPrefs((s) => s.setDensity)
  const isMac = typeof navigator !== 'undefined' && /Mac/i.test(navigator.platform)

  return (
    <header className="sticky top-0 z-20 flex h-12 shrink-0 items-center gap-2 border-b bg-background/85 px-3 backdrop-blur supports-[backdrop-filter]:bg-background/70">
      <SidebarTrigger />
      <Separator orientation="vertical" className="mx-1 h-4 data-[orientation=vertical]:h-4" />
      <nav aria-label="当前位置" className="flex min-w-0 items-center gap-1.5 text-sm">
        {group && <span className="hidden text-muted-foreground sm:inline">{group.label}</span>}
        {group && <span className="hidden text-muted-foreground/50 sm:inline">/</span>}
        <span className="truncate font-medium">{item?.label ?? 'Kiro Console'}</span>
      </nav>
      <div className="ml-auto flex items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          className="hidden h-7 w-56 justify-start gap-2 text-muted-foreground md:flex"
          onClick={palette.open}
        >
          <Search className="size-3.5" />
          <span className="flex-1 text-left text-xs">搜索或执行操作…</span>
          <Kbd>{isMac ? '⌘' : 'Ctrl'} K</Kbd>
        </Button>
        <Button variant="ghost" size="icon-sm" className="md:hidden" aria-label="命令面板" onClick={palette.open}>
          <Search />
        </Button>
        {WINDOWED.has(item?.to ?? '') && <WindowPicker />}
        <RefreshControl />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon-sm" aria-label="外观与账户">
              {theme === 'dark' ? <Moon /> : theme === 'light' ? <Sun /> : <Monitor />}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-48">
            <DropdownMenuLabel>主题</DropdownMenuLabel>
            <DropdownMenuRadioGroup value={theme} onValueChange={(v) => setTheme(v as ThemePref)}>
              <DropdownMenuRadioItem value="light">
                <Sun /> 亮色
              </DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="dark">
                <Moon /> 暗色
              </DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="system">
                <Monitor /> 跟随系统
              </DropdownMenuRadioItem>
            </DropdownMenuRadioGroup>
            <DropdownMenuSeparator />
            <DropdownMenuLabel>列表密度</DropdownMenuLabel>
            <DropdownMenuRadioGroup value={density} onValueChange={(v) => setDensity(v as Density)}>
              <DropdownMenuRadioItem value="compact">
                <Rows4 /> 紧凑
              </DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="comfortable">
                <Rows3 /> 舒适
              </DropdownMenuRadioItem>
            </DropdownMenuRadioGroup>
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onSelect={logout}>
              <LogOut /> 退出登录
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </header>
  )
}
