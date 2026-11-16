import { navDomains,navPages,type PageKey } from '@/types/ui'

const segmentToPage = navPages.reduce<Record<string, PageKey>>((acc, page) => {
  acc[page.path] = page.key
  return acc
}, {})

/**
 * 从 pathname 解析当前页面 key。
 * 传入 react-router `useLocation().pathname`(basename /ui 已被剥离),如 `/accounts`。
 */
export function getPageFromPathname(pathname: string): PageKey {
  const path = pathname.replace(/^\/+|\/+$/g, '')
  const page = navPages.find(page => path === page.path || path.startsWith(`${page.path}/`))
  return page?.key ?? segmentToPage[path] ?? 'overview'
}

/** 当前页所属域的标签 */
export function domainLabelOf(pageKey: PageKey): string {
  const page = navPages.find((p) => p.key === pageKey)
  const domain = navDomains.find((d) => d.key === page?.domain)
  return domain?.label ?? ''
}
