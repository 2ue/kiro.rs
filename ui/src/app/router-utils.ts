import { navItems, CONSOLE_BASE_PATH, type TabKey } from '@/types/ui'

const segmentToTab = navItems.reduce<Record<string, TabKey>>((acc, item) => {
  acc[item.path] = item.key
  return acc
}, {})

/** 从 pathname 解析当前 tab */
export function getTabFromPathname(pathname: string): TabKey {
  const normalized = pathname.replace(/\/+$/, '') || CONSOLE_BASE_PATH
  const prefix = `${CONSOLE_BASE_PATH}/`
  if (!normalized.startsWith(prefix)) return 'dashboard'
  const segment = normalized.slice(prefix.length).split('/')[0]
  return segmentToTab[segment] ?? 'dashboard'
}
