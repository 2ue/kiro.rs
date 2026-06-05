import { LogOut, Moon, Palette, RefreshCw, Sun } from 'lucide-react'
import { Button, Dropdown } from 'react-daisyui'
import type { ThemeMode } from '@/types/ui'

interface TopBarProps {
  title: string
  subtitle?: string
  theme: ThemeMode
  onThemeChange: (theme: ThemeMode) => void
  onLogout: () => void
  onRefresh?: () => void
  isRefreshing?: boolean
  actions?: React.ReactNode
  adminApiKeyLabel?: string
}

const themes: Array<{ key: ThemeMode; label: string; icon: React.ReactNode; colors: string }> = [
  { key: 'kiroLight', label: '浅色', icon: <Sun className="h-4 w-4" />, colors: 'from-blue-500 to-teal-500' },
  { key: 'kiroDark', label: '深色', icon: <Moon className="h-4 w-4" />, colors: 'from-blue-400 to-teal-400' },
  { key: 'kiroOcean', label: '海洋', icon: <Palette className="h-4 w-4" />, colors: 'from-cyan-500 to-blue-600' },
  { key: 'kiroForest', label: '森林', icon: <Palette className="h-4 w-4" />, colors: 'from-emerald-500 to-green-600' },
  { key: 'kiroPurple', label: '紫罗兰', icon: <Palette className="h-4 w-4" />, colors: 'from-violet-500 to-purple-600' },
  { key: 'kiroSunset', label: '日落', icon: <Palette className="h-4 w-4" />, colors: 'from-orange-500 to-rose-500' },
]

export function TopBar({
  title,
  subtitle,
  theme,
  onThemeChange,
  onLogout,
  onRefresh,
  isRefreshing,
  actions,
  adminApiKeyLabel,
}: TopBarProps) {
  const currentTheme = themes.find((t) => t.key === theme) || themes[0]

  return (
    <header className="top-bar sticky top-0 z-30 border-b border-base-300 bg-base-100/80 backdrop-blur-lg">
      <div className="flex h-14 items-center justify-between gap-4 px-4 lg:px-6">
        {/* Title */}
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-lg font-semibold tracking-tight">{title}</h1>
          {(subtitle || adminApiKeyLabel) && (
            <p className="truncate text-xs text-base-content/50">
              {subtitle}
              {adminApiKeyLabel && (
                <>
                  {subtitle ? <span className="mx-2 text-base-content/25">|</span> : null}
                  <span className="font-mono">adminApiKey</span>
                  <span className="mx-1">:</span>
                  <span className="font-mono">{adminApiKeyLabel}</span>
                </>
              )}
            </p>
          )}
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
                <div className={`h-4 w-4 rounded-full bg-gradient-to-br ${currentTheme.colors}`} />
                <span className="hidden sm:inline">{currentTheme.label}</span>
              </Button>
            </Dropdown.Toggle>
            <Dropdown.Menu className="mt-2 w-48 rounded-xl border border-base-300 bg-base-100 p-2 shadow-xl">
              {themes.map((t) => (
                <Dropdown.Item key={t.key} onClick={() => onThemeChange(t.key)}>
                  <button
                    type="button"
                    className={`flex w-full items-center gap-3 rounded-lg px-2 py-1.5 text-left text-sm transition ${
                      theme === t.key ? 'bg-primary/10 text-primary' : 'hover:bg-base-200'
                    }`}
                  >
                    <div className={`h-5 w-5 rounded-full bg-gradient-to-br ${t.colors}`} />
                    <span className="flex-1">{t.label}</span>
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
