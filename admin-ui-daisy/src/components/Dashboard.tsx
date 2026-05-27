import { BarChart3, ChevronDown, Command, DollarSign, FileClock, LogOut, Moon, Palette, Router, Server, Settings, Sun } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Button, Card } from 'react-daisyui'
import { AuditPanel } from '@/components/AuditPanel'
import { ConfigPanel } from '@/components/ConfigPanel'
import { CredentialsPanel } from '@/components/CredentialsPanel'
import { PricingPanel } from '@/components/PricingPanel'
import { ProxyPanel } from '@/components/ProxyPanel'
import { UsagePanel } from '@/components/UsagePanel'
import { storage } from '@/lib/storage'

type TabKey = 'credentials' | 'proxies' | 'usage' | 'pricing' | 'audit' | 'config'

const tabs: Array<{ key: TabKey; label: string; icon: React.ReactNode }> = [
  { key: 'credentials', label: '凭据', icon: <Server className="h-4 w-4" /> },
  { key: 'proxies', label: '代理', icon: <Router className="h-4 w-4" /> },
  { key: 'usage', label: '使用记录', icon: <BarChart3 className="h-4 w-4" /> },
  { key: 'pricing', label: '模型价格', icon: <DollarSign className="h-4 w-4" /> },
  { key: 'audit', label: '审计日志', icon: <FileClock className="h-4 w-4" /> },
  { key: 'config', label: '运行配置', icon: <Settings className="h-4 w-4" /> },
]

const pageTitle: Record<TabKey, string> = {
  credentials: '凭据控制台',
  proxies: '代理 / 家宽',
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

  const selectTab = (tab: TabKey) => {
    setActiveTab(tab)
    setMobileMenuOpen(false)
  }

  const active = tabs.find((tab) => tab.key === activeTab) || tabs[0]

  return (
    <div className="min-h-screen bg-base-200">
      <header className="top-shell sticky top-0 z-40 border-b border-base-300">
        <div className="mx-auto flex max-w-[var(--page-max)] items-center gap-3 px-4 py-2 lg:px-6">
          <div className="flex min-w-0 items-center gap-2">
            <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary text-primary-content shadow-sm">
              <Command className="h-5 w-5" />
            </div>
            <div className="hidden text-sm font-semibold tracking-tight sm:block">Kiro Admin</div>
          </div>

          <nav className="hidden flex-1 items-center justify-center gap-1 lg:flex">
            {tabs.map((tab) => (
              <Button
                key={tab.key}
                type="button"
                size="sm"
                color={activeTab === tab.key ? 'primary' : 'ghost'}
                className="gap-2"
                onClick={() => selectTab(tab.key)}
              >
                {tab.icon}
                {tab.label}
              </Button>
            ))}
          </nav>

          <div className="relative flex flex-1 justify-end lg:hidden">
            <Button
              type="button"
              color="primary"
              size="sm"
              className="gap-2"
              aria-expanded={mobileMenuOpen}
              onClick={() => setMobileMenuOpen((value) => !value)}
            >
              {active.label}
              <ChevronDown className={`h-4 w-4 transition ${mobileMenuOpen ? 'rotate-180' : ''}`} />
            </Button>
            {mobileMenuOpen && (
              <Card className="absolute right-0 top-11 z-50 w-64 border border-base-300 bg-base-100 shadow-xl">
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
          </div>

          <div className="hidden items-center gap-1.5 sm:flex">
            <Button
              type="button"
              size="sm"
              onClick={() => setDark((value) => !value)}
              title="切换主题"
              className="theme-toggle-btn gap-2"
            >
              {dark ? <Moon className="h-4 w-4" /> : <Sun className="h-4 w-4" />}
              {dark ? '深色' : '浅色'}
            </Button>
            <Button type="button" color="ghost" size="sm" onClick={logout} title="退出登录">
              <LogOut className="h-4 w-4" />
              退出
            </Button>
          </div>
        </div>
      </header>

      <main className="mx-auto max-w-[var(--page-max)] px-4 py-4 lg:px-6 lg:py-5">
        <div className="page-heading mb-4 flex flex-col gap-3 px-1 py-2.5 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <div className="mb-1 flex items-center gap-2 text-xs font-semibold text-primary">
              {active.icon}
              {active.label}
            </div>
            <h1 className="text-lg font-semibold tracking-tight md:text-xl">{pageTitle[activeTab]}</h1>
          </div>
          <div className="flex gap-2 sm:hidden">
            <Button type="button" size="sm" onClick={() => setDark((value) => !value)} className="theme-toggle-btn" title="切换主题">
              <Palette className="h-4 w-4" />
            </Button>
            <Button type="button" color="ghost" size="sm" shape="square" onClick={logout} title="退出登录">
              <LogOut className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {activeTab === 'credentials' && <CredentialsPanel />}
        {activeTab === 'proxies' && <ProxyPanel />}
        {activeTab === 'usage' && <UsagePanel />}
        {activeTab === 'pricing' && <PricingPanel />}
        {activeTab === 'audit' && <AuditPanel />}
        {activeTab === 'config' && <ConfigPanel />}
      </main>
    </div>
  )
}
