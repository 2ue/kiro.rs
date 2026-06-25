import { useLocation } from 'react-router-dom'
import { LogOut, Menu, Moon, Sun } from 'lucide-react'
import { Button } from '@/components/ui'
import { getPageFromPathname, domainLabelOf } from '@/app/router-utils'
import { pageMeta } from '@/types/ui'
import { useTheme } from '@/app/theme'

export function Topbar({
  onLogout,
  onOpenMobileMenu,
}: {
  onLogout: () => void
  onOpenMobileMenu: () => void
}) {
  const location = useLocation()
  const page = getPageFromPathname(location.pathname)
  const meta = pageMeta[page]
  const domainLabel = domainLabelOf(page)
  const { theme, toggleTheme } = useTheme()

  return (
    <header className="sticky top-0 z-30 flex h-[var(--header-h)] shrink-0 items-center gap-4 border-b border-border bg-card/85 px-4 backdrop-blur-md lg:px-6">
      <Button
        variant="ghost"
        size="icon-sm"
        className="lg:hidden"
        onClick={onOpenMobileMenu}
        aria-label="打开菜单"
      >
        <Menu className="size-5" />
      </Button>

      <div className="flex min-w-0 flex-1 items-baseline gap-2.5">
        <h1 className="truncate text-[0.98rem] font-semibold tracking-tight text-foreground">
          {meta.title}
        </h1>
        <span className="hidden truncate text-xs text-muted-foreground md:inline">
          {meta.subtitle}
        </span>
      </div>

      <span className="hidden rounded-md bg-muted px-2 py-0.5 text-[0.7rem] font-medium text-muted-foreground sm:inline">
        {domainLabel}
      </span>

      <Button
        variant="ghost"
        size="icon-sm"
        onClick={toggleTheme}
        aria-label={theme === 'dark' ? '切换到亮色' : '切换到暗色'}
        title={theme === 'dark' ? '切换到亮色' : '切换到暗色'}
      >
        {theme === 'dark' ? <Sun className="size-4" /> : <Moon className="size-4" />}
      </Button>

      <Button
        variant="ghost"
        size="sm"
        onClick={onLogout}
        className="text-destructive hover:bg-destructive/10 hover:text-destructive"
      >
        <LogOut className="size-4" />
        <span className="hidden sm:inline">退出</span>
      </Button>
    </header>
  )
}
