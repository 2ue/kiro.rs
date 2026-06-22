export type TabKey = 'dashboard' | 'credentials' | 'validation' | 'proxies' | 'external' | 'usage' | 'pricing' | 'audit' | 'config'

export type ThemeMode = 'blackGold'

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
    label: '浅底黑金',
    description: '温暖浅底、黑色结构、金色强调',
    swatches: ['#FFFDF8', '#1C1710', '#B4862C', '#D8B568'],
  },
]

export function isThemeMode(value: string | null | undefined): value is ThemeMode {
  return value === 'blackGold'
}

export function getStoredTheme(): ThemeMode {
  const stored = localStorage.getItem('kiro-theme')
  return isThemeMode(stored) ? stored : DEFAULT_THEME
}

export const pageConfig: Record<TabKey, { title: string; subtitle: string }> = {
  dashboard: { title: '总览', subtitle: '快速了解系统状态和关键变化' },
  credentials: { title: '凭据管理', subtitle: '维护账号资源，保持服务稳定' },
  validation: { title: '账号校验', subtitle: '检查账号可用性，减少异常影响' },
  proxies: { title: '代理资源', subtitle: '维护网络资源和连通状态' },
  external: { title: '备用号池', subtitle: '维护备用资源，提高服务可用性' },
  usage: { title: '用量', subtitle: '查看使用情况和成本变化' },
  pricing: { title: '模型价格', subtitle: '维护价格信息，辅助成本核算' },
  audit: { title: '审计日志', subtitle: '查看关键操作记录' },
  config: { title: '运行配置', subtitle: '调整基础设置，控制运行表现' },
}
