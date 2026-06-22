import {
  BarChart3,
  ChevronLeft,
  ChevronRight,
  DollarSign,
  FileCheck2,
  FileClock,
  LayoutDashboard,
  Router,
  Server,
  Settings,
} from 'lucide-react'
import type { MouseEvent, ReactNode } from 'react'
import { useState } from 'react'
import { Button, Tooltip } from 'react-daisyui'
import type { TabKey } from '@/types/ui'
import { getConsoleTabPath } from '@/types/ui'

interface SidebarProps {
  activeTab: TabKey
  onTabChange: (tab: TabKey) => void
  collapsed?: boolean
  embedded?: boolean
  onCollapsedChange?: (collapsed: boolean) => void
}

const navItems: Array<{ key: TabKey; label: string; icon: ReactNode; description: string }> = [
  { key: 'dashboard', label: '总览', icon: <LayoutDashboard className="h-5 w-5" />, description: '查看整体状态' },
  { key: 'credentials', label: '凭据', icon: <Server className="h-5 w-5" />, description: '维护账号资源' },
  { key: 'validation', label: '校验', icon: <FileCheck2 className="h-5 w-5" />, description: '检查账号可用性' },
  { key: 'proxies', label: '代理', icon: <Router className="h-5 w-5" />, description: '维护网络资源' },
  { key: 'external', label: '备用池', icon: <Router className="h-5 w-5" />, description: '维护备用资源' },
  { key: 'usage', label: '用量', icon: <BarChart3 className="h-5 w-5" />, description: '查看使用情况' },
  { key: 'pricing', label: '价格', icon: <DollarSign className="h-5 w-5" />, description: '维护成本信息' },
  { key: 'audit', label: '审计', icon: <FileClock className="h-5 w-5" />, description: '查看操作记录' },
  { key: 'config', label: '配置', icon: <Settings className="h-5 w-5" />, description: '调整系统设置' },
]

export function Sidebar({
  activeTab,
  onTabChange,
  collapsed: controlledCollapsed,
  embedded,
  onCollapsedChange,
}: SidebarProps) {
  const [localCollapsed, setLocalCollapsed] = useState(false)
  const collapsed = embedded ? false : controlledCollapsed ?? localCollapsed

  const toggleCollapsed = () => {
    const next = !collapsed
    if (onCollapsedChange) onCollapsedChange(next)
    else setLocalCollapsed(next)
  }

  const handleNavClick = (event: MouseEvent<HTMLAnchorElement>, tab: TabKey) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.altKey || event.ctrlKey || event.shiftKey) {
      return
    }

    event.preventDefault()
    onTabChange(tab)
  }

  return (
    <aside
      className={`sidebar-shell flex flex-col border-r transition-all duration-200 ${
        embedded ? 'h-full w-64' : `fixed left-0 top-0 z-40 h-screen ${collapsed ? 'w-16' : 'w-56'}`
      }`}
    >
      {/* Logo */}
      <div className="flex h-16 items-center justify-between border-b border-base-300/70 px-3">
        <div className="flex items-center gap-2 overflow-hidden">
          <div className="brand-mark flex h-8 w-8 shrink-0 items-center justify-center rounded-lg">
            <Server className="h-4 w-4" />
          </div>
          {!collapsed && (
            <span className="min-w-0">
              <span className="block whitespace-nowrap text-sm font-bold tracking-tight">Kiro Admin</span>
              <span className="block truncate text-[0.66rem] font-semibold text-base-content/45">后台控制台</span>
            </span>
          )}
        </div>
        {!embedded && (
          <Button
            type="button"
            color="ghost"
            size="xs"
            shape="circle"
            className="shrink-0"
            onClick={toggleCollapsed}
            aria-label={collapsed ? '展开侧边栏' : '收起侧边栏'}
          >
            {collapsed ? <ChevronRight className="h-4 w-4" /> : <ChevronLeft className="h-4 w-4" />}
          </Button>
        )}
      </div>

      {/* Navigation */}
      <nav className="flex-1 overflow-y-auto px-2 py-3">
        <ul className="space-y-1.5">
          {navItems.map((item) => {
            const isActive = activeTab === item.key
            const link = (
              <a
                href={getConsoleTabPath(item.key)}
                onClick={(event) => handleNavClick(event, item.key)}
                aria-current={isActive ? 'page' : undefined}
                className={`nav-item group flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left no-underline transition-all ${
                  isActive
                    ? 'active text-primary shadow-sm'
                    : 'text-base-content/70 hover:bg-primary/10 hover:text-base-content'
                }`}
              >
                <span className={`shrink-0 ${isActive ? 'text-primary' : 'text-base-content/50 group-hover:text-base-content/70'}`}>
                  {item.icon}
                </span>
                {!collapsed && (
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[0.92rem] font-semibold">{item.label}</span>
                    <span className="block truncate text-[0.68rem] text-base-content/50">{item.description}</span>
                  </span>
                )}
                {isActive && !collapsed && (
                  <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-primary" />
                )}
              </a>
            )

            return (
              <li key={item.key}>
                {collapsed ? (
                  <Tooltip message={item.label} position="right">
                    {link}
                  </Tooltip>
                ) : (
                  link
                )}
              </li>
            )
          })}
        </ul>
      </nav>

      {/* Footer */}
      <div className="border-t border-base-300/70 p-2">
        {!collapsed && (
          <div className="sidebar-footer-card rounded-lg p-3">
            <div className="flex items-center justify-between gap-2">
              <div className="text-[0.7rem] font-semibold text-base-content/65">当前入口</div>
              <span className="rounded bg-primary/10 px-1.5 py-0.5 text-[0.58rem] font-bold text-primary">/console</span>
            </div>
            <div className="mt-2 h-1 rounded-full bg-base-300">
              <div className="h-full w-2/3 rounded-full bg-primary" />
            </div>
          </div>
        )}
      </div>
    </aside>
  )
}
