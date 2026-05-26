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

const tabs: Array<{ key: TabKey; label: string; icon: React.ReactNode }> = [
  { key: 'credentials', label: '凭据', icon: <Server className="h-4 w-4" /> },
  { key: 'usage', label: '使用记录', icon: <BarChart3 className="h-4 w-4" /> },
  { key: 'pricing', label: '模型价格', icon: <DollarSign className="h-4 w-4" /> },
  { key: 'audit', label: '审计日志', icon: <FileClock className="h-4 w-4" /> },
  { key: 'config', label: '运行配置', icon: <Settings className="h-4 w-4" /> },
]

const pageTitle: Record<TabKey, string> = {
  credentials: '凭据控制台',
  usage: '使用记录',
  pricing: '模型价格与能力',
  audit: '审计日志',
  config: '运行时配置',
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
      <aside className="sidebar-surface sticky top-0 hidden h-screen border-r p-3 lg:block" style={{ borderColor: 'var(--shell-sidebar-border)' }}>
        <div className="flex h-full flex-col">
          <div className="flex items-center gap-2 px-2 py-2">
            <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary text-primary-content shadow-md shadow-black/20">
              <Command className="h-5 w-5" />
            </div>
            <div>
              <div className="text-sm font-semibold tracking-tight text-white">Kiro Admin</div>
            </div>
          </div>

          <nav className="mt-5 space-y-1">
            {tabs.map((tab) => (
              <button
                type="button"
                key={tab.key}
                className={`sidebar-nav-item flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-left transition ${
                  activeTab === tab.key
                    ? 'is-active text-white'
                    : 'text-slate-300 hover:bg-white/8 hover:text-white'
                }`}
                onClick={() => selectTab(tab.key)}
              >
                <span className={`flex h-7 w-7 items-center justify-center rounded-md ${activeTab === tab.key ? 'bg-primary/18 text-primary-content' : 'bg-white/[0.06]'}`}>
                  {tab.icon}
                </span>
                <span className="truncate text-sm font-medium">{tab.label}</span>
              </button>
            ))}
          </nav>

          <Card className="mt-auto border border-white/10 bg-white/[0.04] shadow-none">
            <Card.Body className="p-2">
            <Button type="button" color="primary" size="sm" className="w-full" onClick={refreshAll}>
              <RefreshCw className="h-4 w-4" />
              刷新
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

        <main className="mx-auto max-w-[var(--page-max)] px-4 py-4 lg:px-6 lg:py-5">
          <Card className="page-hero mb-5 rounded-box">
            <Card.Body className="flex flex-col gap-3 p-4 md:flex-row md:items-center md:justify-between">
              <div className="min-w-0">
              <div className="mb-1 flex items-center gap-2 text-xs font-semibold text-primary">
                {tabs.find((tab) => tab.key === activeTab)?.icon}
                {tabs.find((tab) => tab.key === activeTab)?.label}
              </div>
              <h1 className="text-xl font-semibold tracking-tight md:text-2xl">{pageTitle[activeTab]}</h1>
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
