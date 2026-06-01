import { useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Menu } from 'lucide-react'
import { Button, Drawer } from 'react-daisyui'
import { Sidebar } from '@/components/layout/Sidebar'
import { TopBar } from '@/components/layout/TopBar'
import { AuditPanel } from '@/components/AuditPanel'
import { AccountValidationPanel } from '@/components/AccountValidationPanel'
import { ConfigPanel } from '@/components/ConfigPanel'
import { CredentialsPanel } from '@/components/CredentialsPanel'
import { PricingPanel } from '@/components/PricingPanel'
import { ProxyPanel } from '@/components/ProxyPanel'
import { UsagePanel } from '@/components/UsagePanel'
import { storage } from '@/lib/storage'
import type { TabKey, ThemeMode } from '@/types/ui'
import { pageConfig } from '@/types/ui'

const DEFAULT_THEME: ThemeMode = 'kiroLight'

function getStoredTheme(): ThemeMode {
  const stored = localStorage.getItem('kiro-theme')
  if (stored && ['kiroLight', 'kiroDark', 'kiroOcean', 'kiroForest', 'kiroPurple', 'kiroSunset'].includes(stored)) {
    return stored as ThemeMode
  }
  // Check system preference
  if (window.matchMedia('(prefers-color-scheme: dark)').matches) {
    return 'kiroDark'
  }
  return DEFAULT_THEME
}

export function Dashboard({ onLogout }: { onLogout: () => void }) {
  const [activeTab, setActiveTab] = useState<TabKey>('credentials')
  const [theme, setTheme] = useState<ThemeMode>(getStoredTheme)
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)
  const [isRefreshing, setIsRefreshing] = useState(false)
  const queryClient = useQueryClient()

  // Apply theme
  useEffect(() => {
    document.documentElement.dataset.theme = theme
    localStorage.setItem('kiro-theme', theme)
  }, [theme])

  const handleLogout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  const handleRefresh = async () => {
    setIsRefreshing(true)
    await queryClient.invalidateQueries()
    setTimeout(() => setIsRefreshing(false), 500)
  }

  const handleTabChange = (tab: TabKey) => {
    setActiveTab(tab)
    setMobileMenuOpen(false)
  }

  const currentPage = pageConfig[activeTab]

  return (
    <div className="min-h-screen bg-base-200">
      {/* Desktop Sidebar */}
      <div className="hidden lg:block">
        <Sidebar activeTab={activeTab} onTabChange={handleTabChange} />
      </div>

      {/* Mobile Drawer */}
      <Drawer
        open={mobileMenuOpen}
        onClickOverlay={() => setMobileMenuOpen(false)}
        side={
          <div className="h-full w-64 bg-base-100">
            <Sidebar activeTab={activeTab} onTabChange={handleTabChange} />
          </div>
        }
        className="lg:hidden"
      >
        <div />
      </Drawer>

      {/* Main Content */}
      <div className="lg:pl-56">
        {/* Mobile Header */}
        <div className="sticky top-0 z-30 flex h-14 items-center gap-3 border-b border-base-300 bg-base-100/80 px-4 backdrop-blur-lg lg:hidden">
          <Button
            type="button"
            color="ghost"
            size="sm"
            shape="square"
            onClick={() => setMobileMenuOpen(true)}
          >
            <Menu className="h-5 w-5" />
          </Button>
          <div className="min-w-0 flex-1">
            <h1 className="truncate text-sm font-semibold">{currentPage.title}</h1>
          </div>
        </div>

        {/* Desktop Top Bar */}
        <div className="hidden lg:block">
          <TopBar
            title={currentPage.title}
            subtitle={currentPage.subtitle}
            theme={theme}
            onThemeChange={setTheme}
            onLogout={handleLogout}
            onRefresh={handleRefresh}
            isRefreshing={isRefreshing}
          />
        </div>

        {/* Page Content */}
        <main className="mx-auto max-w-[var(--page-max)] p-4 lg:p-6">
          {activeTab === 'credentials' && <CredentialsPanel />}
          {activeTab === 'validation' && <AccountValidationPanel />}
          {activeTab === 'proxies' && <ProxyPanel />}
          {activeTab === 'usage' && <UsagePanel />}
          {activeTab === 'pricing' && <PricingPanel />}
          {activeTab === 'audit' && <AuditPanel />}
          {activeTab === 'config' && <ConfigPanel />}
        </main>
      </div>

      {/* Mobile Bottom Actions */}
      <div className="fixed bottom-0 left-0 right-0 z-30 flex items-center justify-between border-t border-base-300 bg-base-100/95 px-4 py-2 backdrop-blur-lg lg:hidden">
        <div className="flex items-center gap-2">
          <Button
            type="button"
            color="ghost"
            size="sm"
            onClick={() => setTheme(theme === 'kiroDark' ? 'kiroLight' : 'kiroDark')}
          >
            {theme === 'kiroDark' ? '浅色' : '深色'}
          </Button>
        </div>
        <Button type="button" color="ghost" size="sm" className="text-error" onClick={handleLogout}>
          退出
        </Button>
      </div>
    </div>
  )
}
