import { NavLink } from 'react-router-dom'
import { cn } from '@/lib/utils'
import { navDomains, pagesOfDomain } from '@/types/ui'
import { useSystemVersion } from '@/hooks/use-credentials'

interface SidebarProps {
  onNavigate?: () => void
}

/**
 * 单层分组侧边栏:域作为分组标题,页面作为导航项直接平铺。
 * 干净、一目了然,无双层图标条。桌面常驻,移动端在抽屉中复用。
 */
export function Sidebar({ onNavigate }: SidebarProps) {
  const version = useSystemVersion()

  return (
    <div className="flex h-full w-[15rem] flex-col bg-sidebar text-sidebar-foreground">
      {/* 品牌 */}
      <div className="flex h-[var(--header-h)] shrink-0 items-center gap-2.5 px-4">
        <div className="flex size-8 items-center justify-center rounded-lg bg-sidebar-accent/15 text-sidebar-accent">
          <span className="text-[0.95rem] font-bold">K</span>
        </div>
        <div className="min-w-0">
          <div className="truncate text-[0.92rem] font-semibold tracking-tight text-sidebar-foreground">
            Kiro 控制台
          </div>
        </div>
      </div>

      {/* 分组导航 */}
      <nav className="scrollbar-thin flex-1 overflow-y-auto px-3 py-2">
        {navDomains.map((domain) => {
          const pages = pagesOfDomain(domain.key)
          return (
            <div key={domain.key} className="mb-4 last:mb-0">
              <div className="mb-1 px-2.5 text-[0.66rem] font-semibold uppercase tracking-wider text-sidebar-muted/80">
                {domain.label}
              </div>
              <ul className="space-y-0.5">
                {pages.map((page) => {
                  const Icon = page.icon
                  return (
                    <li key={page.key}>
                      <NavLink
                        to={`/${page.path}`}
                        onClick={onNavigate}
                        end={pages.length === 1}
                        className={({ isActive }) =>
                          cn(
                            'group relative flex items-center gap-2.5 rounded-lg px-2.5 py-[0.46rem] text-[0.84rem] font-medium transition-colors',
                            isActive
                              ? 'bg-sidebar-active text-sidebar-foreground'
                              : 'text-sidebar-muted hover:bg-sidebar-active/55 hover:text-sidebar-foreground'
                          )
                        }
                      >
                        {({ isActive }) => (
                          <>
                            <span
                              className={cn(
                                'absolute left-0 top-1/2 h-4 w-[3px] -translate-y-1/2 rounded-r-full bg-sidebar-accent transition-all',
                                isActive ? 'opacity-100' : 'opacity-0'
                              )}
                            />
                            <Icon
                              className={cn(
                                'size-[1.05rem] shrink-0 transition-colors',
                                isActive ? 'text-sidebar-accent' : 'text-sidebar-muted group-hover:text-sidebar-foreground'
                              )}
                            />
                            <span className="truncate">{page.label}</span>
                          </>
                        )}
                      </NavLink>
                    </li>
                  )
                })}
              </ul>
            </div>
          )
        })}
      </nav>

      {/* 版本 */}
      <div className="shrink-0 px-4 py-3">
        <div className="flex items-center justify-between border-t border-sidebar-border pt-3">
          <span className="text-[0.66rem] font-medium text-sidebar-muted">版本</span>
          <span className="font-mono text-[0.66rem] font-semibold text-sidebar-foreground/70">
            {version.data?.version ? `v${version.data.version}` : '—'}
          </span>
        </div>
      </div>
    </div>
  )
}
