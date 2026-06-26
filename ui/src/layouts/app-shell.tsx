import * as React from 'react'
import { Outlet } from 'react-router-dom'
import { Sidebar } from './sidebar'
import { Topbar } from './topbar'
import { Dialog, DialogContent } from '@/components/ui'

/**
 * 应用外壳:左侧域导航(自适应宽度:域条 + 可选二级条) + 右侧主区域。
 * 移动端侧栏走 Dialog 抽屉。
 */
export function AppShell({ onLogout }: { onLogout: () => void }) {
  const [mobileOpen, setMobileOpen] = React.useState(false)

  return (
    <div className="flex min-h-screen bg-background">
      {/* 桌面侧栏 */}
      <aside className="sticky top-0 hidden h-screen shrink-0 lg:block">
        <Sidebar />
      </aside>

      {/* 移动抽屉 */}
      <Dialog open={mobileOpen} onOpenChange={setMobileOpen}>
        {mobileOpen && (
          <DialogContent
            width="w-auto max-w-[85vw]"
            hideClose
            className="left-0 top-0 h-dvh max-h-dvh translate-x-0 translate-y-0 rounded-none border-l-0 p-0 data-[state=closed]:slide-out-to-left data-[state=open]:slide-in-from-left"
          >
            <Sidebar onNavigate={() => setMobileOpen(false)} />
          </DialogContent>
        )}
      </Dialog>

      {/* 主区域 */}
      <div className="flex min-w-0 flex-1 flex-col">
        <Topbar onLogout={onLogout} onOpenMobileMenu={() => setMobileOpen(true)} />
        <main className="scrollbar-thin min-w-0 flex-1 px-4 py-5 lg:px-6 lg:py-6">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
