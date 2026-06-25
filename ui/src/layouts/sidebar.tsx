import { NavLink, useLocation } from 'react-router-dom'
import { cn } from '@/lib/utils'
import { navDomains, navPages, pagesOfDomain, type DomainKey } from '@/types/ui'
import { useSystemVersion } from '@/hooks/use-credentials'

interface SidebarProps {
  embedded?: boolean
  onNavigate?: () => void
}

/** 从当前路径解析所属域 */
function domainOfPathname(pathname: string): DomainKey {
  const seg = pathname.replace(/^\/+/, '').split('/')[0]
  const page = navPages.find((p) => p.path === seg)
  return page?.domain ?? 'overview'
}

export function Sidebar({ embedded, onNavigate }: SidebarProps) {
  const version = useSystemVersion()
  const location = useLocation()
  const activeDomain = domainOfPathname(location.pathname)
  const subPages = pagesOfDomain(activeDomain)
  const showSubNav = subPages.length > 1

  return (
    <div className="flex h-full bg-sidebar text-sidebar-foreground">
      {/* 一级:域导航条 */}
      <nav
        className={cn(
          'flex w-[3.75rem] shrink-0 flex-col items-center gap-1 border-r border-sidebar-border py-3',
          showSubNav ? '' : 'w-[4.25rem]'
        )}
      >
        <div className="mb-2 flex size-9 items-center justify-center rounded-lg bg-sidebar-accent/15 text-sidebar-accent">
          <span className="text-base font-bold">K</span>
        </div>
        {navDomains.map((domain) => {
          const Icon = domain.icon
          const active = domain.key === activeDomain
          return (
            <NavLink
              key={domain.key}
              to={`/${domain.path}`}
              onClick={onNavigate}
              title={domain.label}
              className={cn(
                'group flex w-full flex-col items-center gap-1 rounded-lg py-2 text-[0.62rem] font-medium transition-colors',
                active
                  ? 'text-sidebar-accent'
                  : 'text-sidebar-muted hover:text-sidebar-foreground'
              )}
            >
              <span
                className={cn(
                  'flex size-9 items-center justify-center rounded-lg transition-colors',
                  active ? 'bg-sidebar-accent/15' : 'group-hover:bg-sidebar-active'
                )}
              >
                <Icon className="size-[1.15rem]" />
              </span>
              {domain.label}
            </NavLink>
          )
        })}
      </nav>

      {/* 二级:当前域内页面 */}
      {showSubNav && (
        <div className="flex w-[11.25rem] shrink-0 flex-col">
          <div className="flex h-[--header-h] items-center px-4">
            <div className="text-sm font-bold tracking-tight text-sidebar-foreground/95">
              {navDomains.find((d) => d.key === activeDomain)?.label}
            </div>
          </div>
          <nav className="scrollbar-thin flex-1 overflow-y-auto px-2.5 pb-3">
            <ul className="space-y-0.5">
              {subPages.map((page) => {
                const Icon = page.icon
                return (
                  <li key={page.key}>
                    <NavLink
                      to={`/${page.path}`}
                      onClick={onNavigate}
                      className={({ isActive }) =>
                        cn(
                          'group flex items-center gap-2.5 rounded-md px-2.5 py-2 text-[0.8rem] font-medium transition-colors',
                          isActive
                            ? 'bg-sidebar-active text-sidebar-foreground'
                            : 'text-sidebar-muted hover:bg-sidebar-active/60 hover:text-sidebar-foreground'
                        )
                      }
                    >
                      <Icon className="size-4 shrink-0" />
                      <span className="truncate">{page.label}</span>
                    </NavLink>
                  </li>
                )
              })}
            </ul>
          </nav>
          <div className="px-3 pb-3">
            <div className="flex items-center justify-between rounded-md bg-sidebar-active/50 px-2.5 py-1.5">
              <span className="text-[0.62rem] font-semibold text-sidebar-muted">版本</span>
              <span className="font-mono text-[0.62rem] font-semibold text-sidebar-foreground/70">
                {version.data?.version ? `v${version.data.version}` : '—'}
              </span>
            </div>
          </div>
        </div>
      )}

      {/* 单页域(总览):域条右侧不展开二级,但仍显示版本 */}
      {!showSubNav && !embedded && (
        <div className="flex w-0 flex-col" aria-hidden />
      )}
    </div>
  )
}
