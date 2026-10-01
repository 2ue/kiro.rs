import { Link, useRouterState } from '@tanstack/react-router'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarRail,
} from '@/components/ui/sidebar'
import { useCredentialSummary, usePoolsStatus, useSystemVersion } from '@/queries/shared'
import { NAV } from './nav'
import { SystemHealthButton } from './system-health'

function useNavBadges(): Record<string, { value: number; tone: 'danger' | 'warning' } | undefined> {
  const summary = useCredentialSummary()
  const pools = usePoolsStatus()
  const failing = summary.data ? summary.data.failing : 0
  const blockedPools = pools.data?.pools.filter((p) => p.pool.enabled && !p.dispatchable).length ?? 0
  return {
    '/accounts': failing > 0 ? { value: failing, tone: 'danger' } : undefined,
    '/pools': blockedPools > 0 ? { value: blockedPools, tone: 'warning' } : undefined,
  }
}

export function AppSidebar() {
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const version = useSystemVersion()
  const badges = useNavBadges()

  return (
    <Sidebar collapsible="icon" variant="sidebar">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" asChild>
              <Link to="/overview">
                <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                  <svg viewBox="0 0 32 32" className="size-5" aria-hidden>
                    <path d="M10 8v16M10 16l9-8M13.5 13l6.5 11" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" fill="none" />
                  </svg>
                </span>
                <span className="grid flex-1 text-left leading-tight">
                  <span className="truncate text-sm font-semibold">Kiro Console</span>
                  <span className="truncate text-xs text-muted-foreground">
                    {version.data?.version ? `v${version.data.version}` : '账号池网关'}
                  </span>
                </span>
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        {NAV.map((group) => (
          <SidebarGroup key={group.label}>
            <SidebarGroupLabel>{group.label}</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {group.items.map((item) => {
                  const active = pathname === item.to || pathname.startsWith(`${item.to}/`)
                  const badge = badges[item.to]
                  return (
                    <SidebarMenuItem key={item.to}>
                      <SidebarMenuButton asChild isActive={active} tooltip={item.label}>
                        <Link to={item.to}>
                          <item.icon />
                          <span>{item.label}</span>
                        </Link>
                      </SidebarMenuButton>
                      {badge && (
                        <SidebarMenuBadge className={badge.tone === 'danger' ? 'text-danger' : 'text-warning'}>
                          {badge.value}
                        </SidebarMenuBadge>
                      )}
                    </SidebarMenuItem>
                  )
                })}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        ))}
      </SidebarContent>
      <SidebarFooter>
        <SystemHealthButton />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  )
}
