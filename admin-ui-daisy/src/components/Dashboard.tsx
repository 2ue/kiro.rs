import { useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Menu } from 'lucide-react'
import { Button, Drawer, Select } from 'react-daisyui'
import { Sidebar } from '@/components/layout/Sidebar'
import { TopBar } from '@/components/layout/TopBar'
import { AuditPanel } from '@/components/AuditPanel'
import { AccountValidationPanel } from '@/components/AccountValidationPanel'
import { ConfigPanel } from '@/components/ConfigPanel'
import { CredentialsPanel } from '@/components/CredentialsPanel'
import { ExternalPoolsPanel } from '@/components/ExternalPoolsPanel'
import { PricingPanel } from '@/components/PricingPanel'
import { ProxyPanel } from '@/components/ProxyPanel'
import { UsagePanel } from '@/components/UsagePanel'
import { UsageDashboardPanel } from '@/components/UsageDashboardPanel'
import { storage } from '@/lib/storage'
import type { TabKey, ThemeMode } from '@/types/ui'
import { getStoredTheme, pageConfig, themeOptions } from '@/types/ui'

export function Dashboard({ onLogout }: { onLogout: () => void }) {
  const [activeTab, setActiveTab] = useState<TabKey>('dashboard')
  const [theme, setTheme] = useState<ThemeMode>(getStoredTheme)
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false)
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

  const handleTabChange = (tab: TabKey) => {
    setActiveTab(tab)
    setMobileMenuOpen(false)
  }

  const currentPage = pageConfig[activeTab]

  return (
    <div className="min-h-screen bg-base-200">
      {/* Desktop Sidebar */}
      <div className="hidden lg:block">
        <Sidebar
          activeTab={activeTab}
          collapsed={sidebarCollapsed}
          onCollapsedChange={setSidebarCollapsed}
          onTabChange={handleTabChange}
        />
      </div>

      {/* Mobile Drawer */}
      <Drawer
        open={mobileMenuOpen}
        onClickOverlay={() => setMobileMenuOpen(false)}
        side={
          <div className="h-full w-64 bg-base-100">
            <Sidebar activeTab={activeTab} embedded onTabChange={handleTabChange} />
          </div>
        }
        className="lg:hidden"
      >
        <div />
      </Drawer>

      {/* Main Content */}
      <div className={`transition-[padding-left] duration-200 ${sidebarCollapsed ? 'lg:pl-16' : 'lg:pl-56'}`}>
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
            {currentPage.subtitle && <p className="truncate text-[0.68rem] text-base-content/45">{currentPage.subtitle}</p>}
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
          />
        </div>

        {/* Page Content */}
        <main className="mx-auto max-w-[var(--page-max)] p-4 pb-20 lg:p-6">
          {activeTab === 'dashboard' && <UsageDashboardPanel />}
          {activeTab === 'credentials' && <CredentialsPanel />}
          {activeTab === 'validation' && <AccountValidationPanel />}
          {activeTab === 'proxies' && <ProxyPanel />}
          {activeTab === 'external' && <ExternalPoolsPanel />}
          {activeTab === 'usage' && <UsagePanel />}
          {activeTab === 'pricing' && <PricingPanel />}
          {activeTab === 'audit' && <AuditPanel />}
          {activeTab === 'config' && <ConfigPanel />}
        </main>
      </div>

      {/* Mobile Bottom Actions */}
      <div className="fixed bottom-0 left-0 right-0 z-30 flex items-center justify-between border-t border-base-300 bg-base-100/95 px-4 py-2 backdrop-blur-lg lg:hidden">
        <Select
          size="sm"
          value={theme}
          className="w-28"
          onChange={(event) => setTheme(event.target.value as ThemeMode)}
        >
          {themeOptions.map((option) => (
            <option key={option.key} value={option.key}>
              {option.label}
            </option>
          ))}
        </Select>
        <Button type="button" color="ghost" size="sm" className="text-error" onClick={handleLogout}>
          退出
        </Button>
      </div>
    </div>
  )
}
