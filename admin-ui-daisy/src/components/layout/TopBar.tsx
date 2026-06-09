import { LogOut, Paintbrush, RefreshCw } from 'lucide-react'
import { Button, Dropdown } from 'react-daisyui'
import { themeOptions, type ThemeMode } from '@/types/ui'

interface TopBarProps {
  title: string
  subtitle?: string
  theme: ThemeMode
  onThemeChange: (theme: ThemeMode) => void
  onLogout: () => void
  onRefresh?: () => void
  isRefreshing?: boolean
  actions?: React.ReactNode
}

export function TopBar({
  title,
  subtitle,
  theme,
  onThemeChange,
  onLogout,
  onRefresh,
  isRefreshing,
  actions,
}: TopBarProps) {
  const currentTheme = themeOptions.find((t) => t.key === theme) || themeOptions[0]

  return (
    <header className="top-bar sticky top-0 z-30 border-b border-base-300 bg-base-100/80 backdrop-blur-lg">
      <div className="flex h-14 items-center justify-between gap-4 px-4 lg:px-6">
        {/* Title */}
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-lg font-semibold tracking-tight">{title}</h1>
          {subtitle && <p className="truncate text-xs text-base-content/50">{subtitle}</p>}
        </div>

        {/* Actions */}
        <div className="flex items-center gap-2">
          {actions}

          {onRefresh && (
            <Button
              type="button"
              color="ghost"
              size="sm"
              onClick={onRefresh}
              disabled={isRefreshing}
              className="gap-1.5"
            >
              <RefreshCw className={`h-4 w-4 ${isRefreshing ? 'animate-spin' : ''}`} />
              <span className="hidden sm:inline">刷新</span>
            </Button>
          )}

          {/* Theme Selector */}
          <Dropdown end>
            <Dropdown.Toggle button={false}>
              <Button type="button" color="ghost" size="sm" className="gap-1.5">
                <Paintbrush className="h-4 w-4 text-base-content/55" />
                <span className="hidden sm:inline">{currentTheme.label}</span>
              </Button>
            </Dropdown.Toggle>
            <Dropdown.Menu className="mt-2 w-56 rounded-box border border-base-300 bg-base-100 p-2 shadow-xl">
              {themeOptions.map((t) => (
                <Dropdown.Item key={t.key} onClick={() => onThemeChange(t.key)}>
                  <button
                    type="button"
                    className={`flex w-full items-center gap-3 rounded-lg px-2 py-1.5 text-left text-sm transition ${
                      theme === t.key ? 'bg-primary/10 text-primary' : 'hover:bg-base-200'
                    }`}
                  >
                    <span
                      className="h-4 w-4 shrink-0 rounded-full border border-base-content/10"
                      style={{ backgroundColor: t.swatch }}
                    />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate">{t.label}</span>
                      <span className="block truncate text-[0.62rem] text-base-content/45">{t.description}</span>
                    </span>
                    {theme === t.key && <span className="h-1.5 w-1.5 rounded-full bg-primary" />}
                  </button>
                </Dropdown.Item>
              ))}
            </Dropdown.Menu>
          </Dropdown>

          {/* Logout */}
          <Button type="button" color="ghost" size="sm" onClick={onLogout} className="gap-1.5 text-error hover:bg-error/10">
            <LogOut className="h-4 w-4" />
            <span className="hidden sm:inline">退出</span>
          </Button>
        </div>
      </div>
    </header>
  )
}
