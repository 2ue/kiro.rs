import { navItems, type TabKey } from '@/types/ui'

const segmentToTab = navItems.reduce<Record<string, TabKey>>((acc, item) => {
  acc[item.path] = item.key
  return acc
}, {})

/**
 * 从 pathname 解析当前 tab。
 *
 * 传入的是 react-router `useLocation().pathname`，basename(/ui) 已被剥离，
 * 因此这里拿到的是相对路径,如 `/dashboard`、`/external-pools`。
 */
export function getTabFromPathname(pathname: string): TabKey {
  const segment = pathname.replace(/^\/+/, '').split('/')[0]
  return segmentToTab[segment] ?? 'dashboard'
}
