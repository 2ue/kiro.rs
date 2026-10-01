import { useState } from 'react'
import { createRootRoute, Link, Outlet } from '@tanstack/react-router'
import { SidebarInset, SidebarProvider } from '@/components/ui/sidebar'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/patterns/data-state'
import { AppSidebar } from '@/shell/app-sidebar'
import { useAuth } from '@/shell/auth'
import { CommandPaletteProvider } from '@/shell/command-palette'
import { LoginPage } from '@/shell/login-page'
import { Topbar } from '@/shell/topbar'

const SIDEBAR_KEY = 'kiro-console.sidebar'

function RootLayout() {
  const { authed } = useAuth()
  const [sidebarOpen, setSidebarOpen] = useState(() => localStorage.getItem(SIDEBAR_KEY) !== 'false')
  if (!authed) return <LoginPage />
  return (
    <CommandPaletteProvider>
      <SidebarProvider
        open={sidebarOpen}
        onOpenChange={(open) => {
          setSidebarOpen(open)
          localStorage.setItem(SIDEBAR_KEY, String(open))
        }}
      >
        <AppSidebar />
        <SidebarInset className="min-w-0">
          <Topbar />
          <main className="min-w-0 flex-1">
            <Outlet />
          </main>
        </SidebarInset>
      </SidebarProvider>
    </CommandPaletteProvider>
  )
}

function NotFound() {
  return (
    <EmptyState
      title="页面不存在"
      description="链接可能已失效或地址有误"
      action={
        <Button asChild variant="outline" size="sm">
          <Link to="/overview">返回总览</Link>
        </Button>
      }
    />
  )
}

export const Route = createRootRoute({
  component: RootLayout,
  notFoundComponent: NotFound,
})
