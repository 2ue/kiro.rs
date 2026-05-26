import { BarChart3, ChevronDown, Command, DollarSign, FileClock, LogOut, Palette, RefreshCw, Server, Settings } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Button, Card, Navbar } from 'react-daisyui'
import { AuditPanel } from '@/components/AuditPanel'
import { ConfigPanel } from '@/components/ConfigPanel'
import { CredentialsPanel } from '@/components/CredentialsPanel'
import { PricingPanel } from '@/components/PricingPanel'
import { UsagePanel } from '@/components/UsagePanel'
import { storage } from '@/lib/storage'

type TabKey = 'credentials' | 'usage' | 'pricing' | 'audit' | 'config'

const tabs: Array<{ key: TabKey; label: string; desc: string; icon: React.ReactNode }> = [
  { key: 'credentials', label: '凭据', desc: '账号池与验活', icon: <Server className="h-4 w-4" /> },
  { key: 'usage', label: '使用记录', desc: '链路、费用、缓存', icon: <BarChart3 className="h-4 w-4" /> },
  { key: 'pricing', label: '模型价格', desc: '同步计价目录', icon: <DollarSign className="h-4 w-4" /> },
  { key: 'audit', label: '审计日志', desc: '后台操作追踪', icon: <FileClock className="h-4 w-4" /> },
  { key: 'config', label: '运行配置', desc: '热加载策略', icon: <Settings className="h-4 w-4" /> },
]

const pageCopy: Record<TabKey, { title: string; description: string }> = {
  credentials: {
    title: '凭据控制台',
    description: '管理账号池、并发占用、余额、导入导出和模型验活。',
  },
  usage: {
    title: '使用记录',
    description: '按请求查看上报 token、缓存读写、调用链路、费用和错误详情。',
  },
  pricing: {
    title: '模型价格与能力',
    description: '同步 Kiro 关注模型的价格和能力信息，仅用于统计与展示。',
  },
  audit: {
    title: '审计日志',
    description: '查看后台关键写操作、导出动作和配置变更记录。',
  },
  config: {
    title: '运行时配置',
    description: '调整调度、缓存模拟、路径上报和兼容诊断策略，新请求热加载生效。',
  },
}

export function Dashboard({ onLogout }: { onLogout: () => void }) {
  const [activeTab, setActiveTab] = useState<TabKey>('credentials')
  const [dark, setDark] = useState(() => document.documentElement.dataset.theme === 'kiroDark')
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)
  const queryClient = useQueryClient()

  useEffect(() => {
    document.documentElement.dataset.theme = dark ? 'kiroDark' : 'kiroLight'
  }, [dark])

  const logout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  const refreshAll = () => {
    queryClient.invalidateQueries()
    toast.success('已刷新页面数据')
  }

  const selectTab = (tab: TabKey) => {
    setActiveTab(tab)
    setMobileMenuOpen(false)
  }

  return (
    <div className="app-shell bg-base-200">
      <aside className="sidebar-surface sticky top-0 hidden h-screen border-r p-4 lg:block" style={{ borderColor: 'var(--shell-sidebar-border)' }}>
        <div className="flex h-full flex-col">
          <div className="flex items-center gap-3 px-2 py-2">
            <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-primary text-primary-content shadow-md shadow-black/20">
              <Command className="h-5 w-5" />
            </div>
            <div>
              <div className="text-sm font-semibold tracking-tight text-white">Kiro Admin</div>
              <div className="text-xs" style={{ color: 'var(--shell-sidebar-muted)' }}>Operations Console</div>
            </div>
          </div>

          <nav className="mt-6 space-y-1">
            {tabs.map((tab) => (
              <button
                type="button"
                key={tab.key}
                className={`sidebar-nav-item flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition ${
                  activeTab === tab.key
                    ? 'is-active text-white'
                    : 'text-slate-300 hover:bg-white/8 hover:text-white'
                }`}
                onClick={() => selectTab(tab.key)}
              >
                <span className={`flex h-8 w-8 items-center justify-center rounded-md ${activeTab === tab.key ? 'bg-primary/18 text-primary-content' : 'bg-white/[0.06]'}`}>
                  {tab.icon}
                </span>
                <span className="min-w-0">
                  <span className="block text-sm font-medium">{tab.label}</span>
                  <span className={`block truncate text-xs ${activeTab === tab.key ? 'text-slate-200/80' : 'text-slate-400'}`}>{tab.desc}</span>
                </span>
              </button>
            ))}
          </nav>

          <Card className="mt-auto border border-white/10 bg-white/[0.04] shadow-none">
            <Card.Body className="space-y-3 p-3">
            <div className="text-xs leading-5 text-slate-400">
              当前页面独立运行在 9026，通过 Vite proxy 调用后台 `/api/admin`。
            </div>
            <Button type="button" color="primary" size="sm" className="w-full" onClick={refreshAll}>
              <RefreshCw className="h-4 w-4" />
              刷新全部数据
            </Button>
            </Card.Body>
          </Card>
        </div>
      </aside>

      <div className="workspace-surface min-w-0">
        <Navbar className="glass-nav sticky top-0 z-30 border-b border-base-300 lg:hidden">
          <Navbar.Start>
            <div className="flex items-center gap-2 px-2 font-semibold">
              <Server className="h-5 w-5" />
              Kiro Admin
            </div>
          </Navbar.Start>
          <Navbar.End className="relative">
            <Button
              type="button"
              color="primary"
              size="sm"
              className="gap-2"
              aria-expanded={mobileMenuOpen}
              onClick={() => setMobileMenuOpen((value) => !value)}
            >
              {tabs.find((tab) => tab.key === activeTab)?.label}
              <ChevronDown className={`h-4 w-4 transition ${mobileMenuOpen ? 'rotate-180' : ''}`} />
            </Button>
            {mobileMenuOpen && (
              <Card className="absolute right-0 top-12 z-50 w-64 border border-base-300 bg-base-100 shadow-xl">
                <Card.Body className="p-2">
                  <div className="space-y-1">
                    {tabs.map((tab) => (
                      <button
                        type="button"
                        key={tab.key}
                        onClick={() => selectTab(tab.key)}
                        className={`flex w-full items-center gap-2 rounded-box px-3 py-2 text-left text-sm ${
                          activeTab === tab.key ? 'bg-primary text-primary-content' : 'hover:bg-base-200'
                        }`}
                      >
                        {tab.icon}
                        {tab.label}
                      </button>
                    ))}
                  </div>
                </Card.Body>
              </Card>
            )}
          </Navbar.End>
        </Navbar>

        <main className="mx-auto max-w-[var(--page-max)] px-4 py-5 lg:px-8 lg:py-7">
          <Card className="page-hero mb-5 rounded-box">
            <Card.Body className="flex flex-col gap-4 p-5 md:flex-row md:items-center md:justify-between">
              <div>
              <div className="mb-2 flex items-center gap-2 text-xs font-semibold text-primary">
                {tabs.find((tab) => tab.key === activeTab)?.icon}
                {tabs.find((tab) => tab.key === activeTab)?.label}
              </div>
              <h1 className="text-2xl font-semibold tracking-tight md:text-[1.8rem]">{pageCopy[activeTab].title}</h1>
              <p className="mt-2 max-w-3xl text-sm leading-6 text-base-content/60">{pageCopy[activeTab].description}</p>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                size="sm"
                onClick={() => setDark((value) => !value)}
                title="切换主题"
                className="theme-toggle-btn gap-2"
              >
                <Palette className="h-4 w-4" />
                {dark ? '深色主题' : '浅色主题'}
              </Button>
              <Button type="button" variant="outline" size="sm" onClick={refreshAll} title="刷新">
                <RefreshCw className="h-4 w-4" />
                刷新
              </Button>
              <Button type="button" color="ghost" size="sm" shape="square" onClick={logout} title="退出登录">
              <LogOut className="h-4 w-4" />
              </Button>
            </div>
            </Card.Body>
          </Card>

          {activeTab === 'credentials' && <CredentialsPanel />}
          {activeTab === 'usage' && <UsagePanel />}
          {activeTab === 'pricing' && <PricingPanel />}
          {activeTab === 'audit' && <AuditPanel />}
          {activeTab === 'config' && <ConfigPanel />}
        </main>
      </div>
    </div>
  )
}
