export type TabKey = 'dashboard' | 'credentials' | 'validation' | 'proxies' | 'external' | 'usage' | 'pricing' | 'audit' | 'config'

export type ThemeMode = 'blackGold'

export type PageLayout = 'dashboard' | 'resource' | 'data' | 'settings'

export const DEFAULT_THEME: ThemeMode = 'blackGold'

export const CONSOLE_BASE_PATH = '/console'

export const tabPathSegments: Record<TabKey, string> = {
  dashboard: 'dashboard',
  credentials: 'credentials',
  validation: 'validation',
  proxies: 'proxies',
  external: 'external-pools',
  usage: 'usage',
  pricing: 'pricing',
  audit: 'audit',
  config: 'config',
}

const segmentToTab = Object.entries(tabPathSegments).reduce<Record<string, TabKey>>((acc, [tab, segment]) => {
  acc[segment] = tab as TabKey
  return acc
}, {
  external: 'external',
})

export function getConsoleTabPath(tab: TabKey): string {
  return `${CONSOLE_BASE_PATH}/${tabPathSegments[tab]}`
}

export function getTabFromPathname(pathname: string): TabKey {
  const normalized = pathname.replace(/\/+$/, '') || CONSOLE_BASE_PATH
  if (normalized === CONSOLE_BASE_PATH) return 'dashboard'

  const prefix = `${CONSOLE_BASE_PATH}/`
  if (!normalized.startsWith(prefix)) return 'dashboard'

  const [segment] = normalized.slice(prefix.length).split('/')
  return segmentToTab[segment] ?? 'dashboard'
}

export const themeOptions: Array<{
  key: ThemeMode
  label: string
  description: string
  swatches: string[]
}> = [
  {
    key: 'blackGold',
    label: '中性黑金',
    description: '浅灰底色、黑色结构、金色点缀',
    swatches: ['#F6F7F9', '#111827', '#B88A2E', '#FFFFFF'],
  },
]

export function isThemeMode(value: string | null | undefined): value is ThemeMode {
  return value === 'blackGold'
}

export function getStoredTheme(): ThemeMode {
  const stored = localStorage.getItem('kiro-theme')
  return isThemeMode(stored) ? stored : DEFAULT_THEME
}

export const pageConfig: Record<TabKey, { title: string; subtitle: string; layout: PageLayout }> = {
  dashboard: { title: '总览', subtitle: '快速了解系统状态和关键变化', layout: 'dashboard' },
  credentials: { title: '账号管理', subtitle: '维护本地账号资源，保持服务稳定', layout: 'resource' },
  validation: { title: '账号校验', subtitle: '检查账号可用性，减少异常影响', layout: 'resource' },
  proxies: { title: '代理资源', subtitle: '维护网络资源和连通状态', layout: 'resource' },
  external: { title: '外部账号', subtitle: '维护扩展账号资源，提高服务稳定性', layout: 'settings' },
  usage: { title: '用量', subtitle: '查看使用情况和成本变化', layout: 'data' },
  pricing: { title: '模型价格', subtitle: '维护价格信息，辅助成本核算', layout: 'data' },
  audit: { title: '审计日志', subtitle: '查看关键操作记录', layout: 'data' },
  config: { title: '运行配置', subtitle: '调整基础设置，控制运行表现', layout: 'settings' },
}
