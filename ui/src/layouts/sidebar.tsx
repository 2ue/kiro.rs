import { NavLink } from 'react-router-dom'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import { cn } from '@/lib/utils'
import { navItems, navGroups, CONSOLE_BASE_PATH } from '@/types/ui'
import { useSystemVersion } from '@/hooks/use-credentials'
import { Tooltip, TooltipProvider } from '@/components/ui'

interface SidebarProps {
  collapsed?: boolean
  embedded?: boolean
  onToggleCollapse?: () => void
  onNavigate?: () => void
}

export function Sidebar({ collapsed = false, embedded, onToggleCollapse, onNavigate }: SidebarProps) {
  const version = useSystemVersion()
  const showLabels = embedded || !collapsed

  return (
    <TooltipProvider delayDuration={150}>
      <div className="flex h-full flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
        {/* 品牌 */}
        <div
          className={cn(
            'flex h-[--header-h] shrink-0 items-center gap-2.5 border-b border-sidebar-border px-3',
            !showLabels && 'justify-center px-0'
          )}
        >
          <div className="relative flex size-9 shrink-0 items-center justify-center overflow-hidden rounded-lg bg-secondary text-primary">
            <span className="text-base font-bold">K</span>
            <span className="absolute inset-x-1.5 bottom-1 h-0.5 rounded-full bg-primary" />
          </div>
          {showLabels && (
            <div className="min-w-0">
              <div className="truncate text-sm font-bold tracking-tight">Kiro Console</div>
              <div className="truncate text-[0.66rem] font-semibold text-muted-foreground">
                管理控制台
              </div>
            </div>
          )}
        </div>

        {/* 导航 */}
        <nav className="scrollbar-thin flex-1 overflow-y-auto px-2 py-3">
          {navGroups.map((group) => {
            const items = navItems.filter((item) => item.group === group.id)
            if (!items.length) return null
            return (
              <div key={group.id} className="mb-3 last:mb-0">
                {showLabels && (
                  <div className="mb-1 px-2 text-[0.62rem] font-bold uppercase tracking-wider text-muted-foreground/70">
                    {group.label}
                  </div>
                )}
                <ul className="space-y-0.5">
                  {items.map((item) => {
                    const Icon = item.icon
                    const link = (
                      <NavLink
                        to={`${CONSOLE_BASE_PATH}/${item.path}`}
                        onClick={onNavigate}
                        className={({ isActive }) =>
                          cn(
                            'group relative flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm font-medium transition-colors',
                            !showLabels && 'justify-center px-0',
                            isActive
                              ? 'bg-primary/10 text-primary'
                              : 'text-muted-foreground hover:bg-muted hover:text-foreground'
                          )
                        }
                      >
                        {({ isActive }) => (
                          <>
                            <span
                              className={cn(
                                'absolute left-0 top-1/2 h-0 w-0.5 -translate-y-1/2 rounded-r-full bg-primary transition-all',
                                isActive && 'h-6'
                              )}
                            />
                            <Icon className="size-[1.15rem] shrink-0" />
                            {showLabels && (
                              <span className="min-w-0 flex-1">
                                <span className="block truncate">{item.label}</span>
                              </span>
                            )}
                          </>
                        )}
                      </NavLink>
                    )
                    return (
                      <li key={item.key}>
                        {showLabels ? (
                          link
                        ) : (
                          <Tooltip label={item.label} side="right">
                            {link}
                          </Tooltip>
                        )}
                      </li>
                    )
                  })}
                </ul>
              </div>
            )
          })}
        </nav>

        {/* 底部 */}
        <div className="shrink-0 border-t border-sidebar-border p-2">
          {showLabels ? (
            <div className="flex items-center justify-between rounded-lg bg-muted px-3 py-2">
              <span className="text-[0.66rem] font-semibold text-muted-foreground">版本</span>
              <span className="rounded-full border border-border px-2 py-0.5 font-mono text-[0.62rem] font-semibold text-foreground/70">
                {version.data?.version ? `v${version.data.version}` : '—'}
              </span>
            </div>
          ) : null}
          {!embedded && onToggleCollapse && (
            <button
              type="button"
              onClick={onToggleCollapse}
              className={cn(
                'mt-1.5 flex w-full items-center justify-center gap-2 rounded-lg px-3 py-2 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground'
              )}
              aria-label={collapsed ? '展开侧边栏' : '收起侧边栏'}
            >
              {collapsed ? <ChevronRight className="size-4" /> : <ChevronLeft className="size-4" />}
              {showLabels && <span>收起</span>}
            </button>
          )}
        </div>
      </div>
    </TooltipProvider>
  )
}
