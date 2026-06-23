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
import { ExternalPoolsPanel } from '@/components/ExternalPoolsPanel'
import { PricingPanel } from '@/components/PricingPanel'
import { ProxyPanel } from '@/components/ProxyPanel'
import { UsagePanel } from '@/components/UsagePanel'
import { UsageDashboardPanel } from '@/components/UsageDashboardPanel'
import { storage } from '@/lib/storage'
import type { TabKey } from '@/types/ui'
import { DEFAULT_THEME, getConsoleTabPath, getTabFromPathname, pageConfig } from '@/types/ui'

export function Dashboard({ onLogout }: { onLogout: () => void }) {
  const [activeTab, setActiveTab] = useState<TabKey>(() => getTabFromPathname(window.location.pathname))
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false)
  const queryClient = useQueryClient()

  useEffect(() => {
    document.documentElement.dataset.theme = DEFAULT_THEME
    localStorage.setItem('kiro-theme', DEFAULT_THEME)
  }, [])

  useEffect(() => {
    const handlePopState = () => {
      setActiveTab(getTabFromPathname(window.location.pathname))
      setMobileMenuOpen(false)
    }

    window.addEventListener('popstate', handlePopState)
    return () => window.removeEventListener('popstate', handlePopState)
  }, [])

  const handleLogout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  const handleTabChange = (tab: TabKey) => {
    const nextPath = getConsoleTabPath(tab)
    if (window.location.pathname !== nextPath) {
      window.history.pushState({ tab }, '', nextPath)
    }
    setActiveTab(tab)
    setMobileMenuOpen(false)
  }

  const currentPage = pageConfig[activeTab]

  return (
    <div className="app-shell min-h-screen text-base-content">
      <div className="command-strip fixed left-0 right-0 top-0 z-50" />
      {/* Desktop Sidebar */}
      <div className="sidebar-layer hidden lg:block">
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
          <div className="h-full w-64 bg-base-100/95 backdrop-blur-xl">
            <Sidebar activeTab={activeTab} embedded onTabChange={handleTabChange} />
          </div>
        }
        className="drawer-layer lg:hidden"
      >
        <div />
      </Drawer>

      {/* Main Content */}
      <div className={`app-content transition-[padding-left] duration-200 ${sidebarCollapsed ? 'lg:pl-16' : 'lg:pl-56'}`}>
        {/* Mobile Header */}
        <div className="top-bar sticky top-0 z-30 flex h-14 items-center gap-3 border-b px-4 backdrop-blur-lg lg:hidden">
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
            onLogout={handleLogout}
          />
        </div>

        {/* Page Content */}
        <main className={`page-content page-content--${currentPage.layout} relative mx-auto p-4 pb-20 lg:p-6`}>
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
      <div className="mobile-action-bar fixed bottom-0 left-0 right-0 z-30 flex items-center justify-end border-t px-4 py-2 lg:hidden">
        <Button type="button" color="ghost" size="sm" className="text-error" onClick={handleLogout}>
          退出
        </Button>
      </div>
    </div>
  )
}
