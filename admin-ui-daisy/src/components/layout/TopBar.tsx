import type { ReactNode } from 'react'
import { LogOut } from 'lucide-react'
import { Button } from 'react-daisyui'

interface TopBarProps {
  title: string
  subtitle?: string
  onLogout: () => void
  actions?: ReactNode
}

export function TopBar({
  title,
  subtitle,
  onLogout,
  actions,
}: TopBarProps) {
  return (
    <header className="top-bar">
      <div className="top-bar-inner flex items-center justify-between gap-4 px-4 py-3 lg:px-6">
        <div className="top-bar-title min-w-0 flex-1">
          <div className="min-w-0">
            <span className="top-bar-kicker">控制台</span>
            <h1 className="mt-1 truncate text-xl font-semibold tracking-tight text-base-content">{title}</h1>
            {subtitle && <p className="mt-0.5 truncate text-sm text-base-content/55">{subtitle}</p>}
          </div>
        </div>

        <div className="flex items-center gap-2">
          {actions}
          <Button type="button" color="ghost" size="sm" onClick={onLogout} className="gap-1.5 text-error hover:bg-error/10">
            <LogOut className="h-4 w-4" />
            <span className="hidden sm:inline">退出</span>
          </Button>
        </div>
      </div>
    </header>
  )
}
