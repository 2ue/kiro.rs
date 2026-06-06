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
import { useState } from 'react'
import { Button, Tooltip } from 'react-daisyui'
import type { TabKey } from '@/types/ui'

interface SidebarProps {
  activeTab: TabKey
  onTabChange: (tab: TabKey) => void
  collapsed?: boolean
  embedded?: boolean
  onCollapsedChange?: (collapsed: boolean) => void
}

const navItems: Array<{ key: TabKey; label: string; icon: React.ReactNode; description: string }> = [
  { key: 'dashboard', label: '总览', icon: <LayoutDashboard className="h-5 w-5" />, description: '聚合趋势和 Top 统计' },
  { key: 'credentials', label: '凭据', icon: <Server className="h-5 w-5" />, description: '管理 API 凭据和账号' },
  { key: 'validation', label: '校验', icon: <FileCheck2 className="h-5 w-5" />, description: '账号订阅状态校验' },
  { key: 'proxies', label: '代理', icon: <Router className="h-5 w-5" />, description: '代理资源配置' },
  { key: 'usage', label: '用量', icon: <BarChart3 className="h-5 w-5" />, description: '总览和请求列表' },
  { key: 'pricing', label: '价格', icon: <DollarSign className="h-5 w-5" />, description: '模型价格配置' },
  { key: 'audit', label: '审计', icon: <FileClock className="h-5 w-5" />, description: '操作日志记录' },
  { key: 'config', label: '配置', icon: <Settings className="h-5 w-5" />, description: '运行时参数设置' },
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

  return (
    <aside
      className={`sidebar-shell flex flex-col border-r border-base-300 bg-base-100 transition-all duration-200 ${
        embedded ? 'h-full w-64' : `fixed left-0 top-0 z-40 h-screen ${collapsed ? 'w-16' : 'w-56'}`
      }`}
    >
      {/* Logo */}
      <div className="flex h-14 items-center justify-between border-b border-base-300 px-3">
        <div className="flex items-center gap-2 overflow-hidden">
          <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-gradient-to-br from-primary to-secondary text-primary-content shadow-md">
            <Server className="h-4 w-4" />
          </div>
          {!collapsed && (
            <span className="whitespace-nowrap text-sm font-bold tracking-tight">Kiro Admin</span>
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
      <nav className="flex-1 overflow-y-auto p-2">
        <ul className="space-y-1">
          {navItems.map((item) => {
            const isActive = activeTab === item.key
            const button = (
              <button
                type="button"
                onClick={() => onTabChange(item.key)}
                className={`nav-item group flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-all ${
                  isActive
                    ? 'active bg-primary/10 text-primary shadow-sm'
                    : 'text-base-content/70 hover:bg-base-200 hover:text-base-content'
                }`}
              >
                <span className={`shrink-0 ${isActive ? 'text-primary' : 'text-base-content/50 group-hover:text-base-content/70'}`}>
                  {item.icon}
                </span>
                {!collapsed && (
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-sm font-medium">{item.label}</span>
                    <span className="block truncate text-[0.68rem] text-base-content/50">{item.description}</span>
                  </span>
                )}
                {isActive && !collapsed && (
                  <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-primary" />
                )}
              </button>
            )

            return (
              <li key={item.key}>
                {collapsed ? (
                  <Tooltip message={item.label} position="right">
                    {button}
                  </Tooltip>
                ) : (
                  button
                )}
              </li>
            )
          })}
        </ul>
      </nav>

      {/* Footer */}
      <div className="border-t border-base-300 p-2">
        {!collapsed && (
          <div className="rounded-lg bg-gradient-to-r from-primary/5 to-secondary/5 p-3">
            <div className="text-[0.68rem] font-medium text-base-content/60">Kiro Admin Console</div>
            <div className="mt-0.5 text-[0.62rem] text-base-content/40">v2.0.0 · DaisyUI</div>
          </div>
        )}
      </div>
    </aside>
  )
}
