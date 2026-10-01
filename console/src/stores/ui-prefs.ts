import { create } from 'zustand'
import { persist } from 'zustand/middleware'

export type ThemePref = 'light' | 'dark' | 'system'
export type Density = 'compact' | 'comfortable'
/** 0 表示暂停 */
export type RefreshSecs = 0 | 10 | 30 | 60
export type WindowKey = 'today' | 'last24h' | 'yesterday' | 'last7d' | 'last30d' | 'thisMonth'

export const WINDOW_OPTIONS: Array<{ key: WindowKey; label: string; short: string }> = [
  { key: 'today', label: '今天', short: '今天' },
  { key: 'last24h', label: '最近 24 小时', short: '24h' },
  { key: 'yesterday', label: '昨天', short: '昨天' },
  { key: 'last7d', label: '最近 7 天', short: '7d' },
  { key: 'last30d', label: '最近 30 天', short: '30d' },
  { key: 'thisMonth', label: '本月', short: '本月' },
]

interface UiPrefs {
  theme: ThemePref
  density: Density
  refreshSecs: RefreshSecs
  windowKey: WindowKey
  setTheme: (theme: ThemePref) => void
  setDensity: (density: Density) => void
  setRefreshSecs: (secs: RefreshSecs) => void
  setWindowKey: (key: WindowKey) => void
}

export const useUiPrefs = create<UiPrefs>()(
  persist(
    (set) => ({
      theme: 'system',
      density: 'compact',
      refreshSecs: 30,
      windowKey: 'today',
      setTheme: (theme) => set({ theme }),
      setDensity: (density) => set({ density }),
      setRefreshSecs: (refreshSecs) => set({ refreshSecs }),
      setWindowKey: (windowKey) => set({ windowKey }),
    }),
    { name: 'kiro-console.prefs' },
  ),
)

/**
 * 刷新档位：
 * - fast：运行态（在途、冷却），跟随全局刷新频率
 * - normal：聚合统计，至少 30s
 * - slow：额度快照等昂贵数据，至少 120s
 * 全局暂停时全部返回 false。
 */
export type PollTier = 'fast' | 'normal' | 'slow'

export function usePollInterval(tier: PollTier): number | false {
  const secs = useUiPrefs((s) => s.refreshSecs)
  if (secs === 0) return false
  const floor = tier === 'fast' ? secs : tier === 'normal' ? Math.max(secs, 30) : Math.max(secs, 120)
  return floor * 1000
}
