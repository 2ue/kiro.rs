import { useLocation } from 'react-router-dom'
import { LogOut, Menu } from 'lucide-react'
import { Button } from '@/components/ui'
import { getTabFromPathname } from '@/app/router-utils'
import { pageMeta } from '@/types/ui'

export function Topbar({
  onLogout,
  onOpenMobileMenu,
}: {
  onLogout: () => void
  onOpenMobileMenu: () => void
}) {
  const location = useLocation()
  const tab = getTabFromPathname(location.pathname)
  const meta = pageMeta[tab]

  return (
    <header className="sticky top-0 z-30 flex h-[--header-h] shrink-0 items-center gap-3 border-b border-border bg-card/90 px-4 backdrop-blur-md lg:px-6">
      {/* 移动端菜单按钮 */}
      <Button
        variant="ghost"
        size="icon-sm"
        className="lg:hidden"
        onClick={onOpenMobileMenu}
        aria-label="打开菜单"
      >
        <Menu className="size-5" />
      </Button>

      <div className="min-w-0 flex-1">
        <h2 className="truncate text-sm font-semibold tracking-tight text-foreground">
          {meta.title}
        </h2>
        <p className="hidden truncate text-xs text-muted-foreground sm:block">{meta.subtitle}</p>
      </div>

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
