import { useEffect, type PropsWithChildren } from 'react'
import { NavLink, useLocation } from 'react-router-dom'
import {
  Activity,
  CreditCard,
  Gauge,
  LogOut,
  Moon,
  Settings,
  Sun,
  Tags,
  Terminal,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'
import { useAuth } from '@/store/auth'
import { usePreferences } from '@/store/preferences'

interface NavItem {
  to: string
  label: string
  icon: typeof Gauge
  description: string
}

const NAV: NavItem[] = [
  { to: '/dashboard', label: '仪表盘', icon: Gauge, description: '总览今日与累计数据' },
  { to: '/credentials', label: '账号管理', icon: CreditCard, description: '凭据列表 / 导入 / 验活' },
  { to: '/usage', label: '用量分析', icon: Activity, description: '请求记录与成本' },
  { to: '/pricing', label: '模型计价', icon: Tags, description: '可用模型与单价同步' },
  { to: '/settings', label: '设置', icon: Settings, description: '运行时配置 / 安全' },
]

export function AppShell({ children }: PropsWithChildren) {
  const logout = useAuth((s) => s.logout)
  const theme = usePreferences((s) => s.theme)
  const setTheme = usePreferences((s) => s.setTheme)
  const location = useLocation()

  useEffect(() => {
    document.title = 'Kiro Console'
  }, [location.pathname])

  return (
    <TooltipProvider delayDuration={300}>
      <div className="flex h-full min-h-screen bg-muted/30">
        {/* 侧边栏 */}
        <aside className="hidden w-60 shrink-0 border-r bg-background md:flex md:flex-col">
          <div className="flex h-14 items-center gap-2 border-b px-4">
            <Terminal className="h-5 w-5" />
            <span className="text-sm font-semibold tracking-tight">Kiro Console</span>
          </div>
          <nav className="flex-1 space-y-0.5 p-2">
            {NAV.map((item) => {
              const Icon = item.icon
              return (
                <NavLink
                  key={item.to}
                  to={item.to}
                  className={({ isActive }) =>
                    cn(
                      'flex items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors',
                      isActive
                        ? 'bg-secondary text-secondary-foreground'
                        : 'text-muted-foreground hover:bg-accent hover:text-accent-foreground',
                    )
                  }
                >
                  <Icon className="h-4 w-4" />
                  {item.label}
                </NavLink>
              )
            })}
          </nav>
          <Separator />
          <div className="p-2 text-xs text-muted-foreground">
            版本 2026.3.1 · 新版前端预览
          </div>
        </aside>

        {/* 主区域 */}
        <div className="flex min-w-0 flex-1 flex-col">
          <header className="sticky top-0 z-30 flex h-14 items-center justify-between gap-2 border-b bg-background/80 px-4 backdrop-blur">
            <div className="flex items-center gap-2 md:hidden">
              <Terminal className="h-4 w-4" />
              <span className="text-sm font-semibold">Kiro Console</span>
            </div>
            <div className="ml-auto flex items-center gap-1">
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}
                    aria-label="切换主题"
                  >
                    {theme === 'dark' ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
                  </Button>
                </TooltipTrigger>
                <TooltipContent>切换主题</TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon"
                    onClick={logout}
                    aria-label="退出登录"
                  >
                    <LogOut className="h-4 w-4" />
                  </Button>
                </TooltipTrigger>
                <TooltipContent>退出登录</TooltipContent>
              </Tooltip>
            </div>
          </header>

          <main className="flex-1 overflow-auto px-4 py-6 md:px-8">
            <div className="mx-auto w-full max-w-7xl">{children}</div>
          </main>
        </div>
      </div>
    </TooltipProvider>
  )
}
