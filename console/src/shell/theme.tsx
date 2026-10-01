import { useEffect } from 'react'
import { useUiPrefs } from '@/stores/ui-prefs'

/** 同步主题和密度偏好到 <html>；"跟随系统"时监听系统变化 */
export function ThemeSync() {
  const theme = useUiPrefs((s) => s.theme)
  const density = useUiPrefs((s) => s.density)

  useEffect(() => {
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const apply = () => {
      const dark = theme === 'dark' || (theme === 'system' && media.matches)
      document.documentElement.classList.toggle('dark', dark)
    }
    apply()
    if (theme !== 'system') return
    media.addEventListener('change', apply)
    return () => media.removeEventListener('change', apply)
  }, [theme])

  useEffect(() => {
    document.documentElement.dataset.density = density
  }, [density])

  return null
}

export function useResolvedTheme(): 'light' | 'dark' {
  const theme = useUiPrefs((s) => s.theme)
  if (theme !== 'system') return theme
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
}
