import * as React from 'react'
import { Outlet } from 'react-router-dom'
import { Sidebar } from './sidebar'
import { Topbar } from './topbar'
import { Dialog, DialogContent } from '@/components/ui'

/**
 * 应用外壳:CSS Grid 双栏布局
 *  - 桌面:[sidebar] [main],侧栏宽度由 --sidebar-w 变量驱动,收起只改一个变量
 *  - 移动:侧栏走 Dialog 抽屉
 */
export function AppShell({ onLogout }: { onLogout: () => void }) {
  const [collapsed, setCollapsed] = React.useState(false)
  const [mobileOpen, setMobileOpen] = React.useState(false)

  const sidebarWidth = collapsed ? 'var(--sidebar-w-collapsed)' : 'var(--sidebar-w)'

  return (
    <div
      className="min-h-screen bg-background lg:grid"
      style={{ gridTemplateColumns: `${sidebarWidth} 1fr` }}
    >
      {/* 顶部金色细条 */}
      <div className="fixed inset-x-0 top-0 z-50 h-0.5 bg-primary" />

      {/* 桌面侧栏 */}
      <aside className="sticky top-0 hidden h-screen lg:block">
        <Sidebar collapsed={collapsed} onToggleCollapse={() => setCollapsed((v) => !v)} />
      </aside>

      {/* 移动抽屉 */}
      <Dialog open={mobileOpen} onOpenChange={setMobileOpen}>
        {mobileOpen && (
          <DialogContent
            width="w-[17rem] max-w-[80vw]"
            hideClose
            className="left-0 top-0 h-dvh max-h-dvh translate-x-0 translate-y-0 rounded-none rounded-r-xl border-l-0 data-[state=closed]:slide-out-to-left data-[state=open]:slide-in-from-left"
          >
            <Sidebar embedded onNavigate={() => setMobileOpen(false)} />
          </DialogContent>
        )}
      </Dialog>

      {/* 主区域 */}
      <div className="flex min-w-0 flex-col">
        <Topbar onLogout={onLogout} onOpenMobileMenu={() => setMobileOpen(true)} />
        <main className="scrollbar-thin min-w-0 flex-1 px-4 py-5 lg:px-6 lg:py-6">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
