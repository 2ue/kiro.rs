export type TabKey = 'dashboard' | 'credentials' | 'validation' | 'proxies' | 'external' | 'usage' | 'pricing' | 'audit' | 'config'

export type ThemeMode = 'kiroOfficial' | 'kiroLavender' | 'kiroFocus'

export const DEFAULT_THEME: ThemeMode = 'kiroOfficial'

export const themeOptions: Array<{
  key: ThemeMode
  label: string
  description: string
  swatch: string
}> = [
  {
    key: 'kiroOfficial',
    label: '官方紫',
    description: 'Kiro 官方紫，默认品牌主题',
    swatch: '#9046FF',
  },
  {
    key: 'kiroLavender',
    label: '柔和紫',
    description: '低饱和紫调，适合长时间查看',
    swatch: '#7C3AED',
  },
  {
    key: 'kiroFocus',
    label: '深紫',
    description: '更强对比的紫色强调',
    swatch: '#6D28D9',
  },
]

export function isThemeMode(value: string | null | undefined): value is ThemeMode {
  return value === 'kiroOfficial' || value === 'kiroLavender' || value === 'kiroFocus'
}

export function getStoredTheme(): ThemeMode {
  const stored = localStorage.getItem('kiro-theme')
  return isThemeMode(stored) ? stored : DEFAULT_THEME
}

export const pageConfig: Record<TabKey, { title: string; subtitle: string }> = {
  dashboard: { title: '总览', subtitle: '查看请求聚合、趋势、错误率和 Top 维度统计' },
  credentials: { title: '凭据管理', subtitle: '管理 API 凭据、账号状态和调度配置' },
  validation: { title: '账号校验', subtitle: '批量校验账号订阅状态和额度' },
  proxies: { title: '代理资源', subtitle: '配置和管理代理服务器资源' },
  external: { title: '备用号池', subtitle: '配置外部备用池、直连策略和 fallback 调度' },
  usage: { title: '用量', subtitle: '查看聚合总览、请求列表和费用估算' },
  pricing: { title: '模型价格', subtitle: '配置模型定价和计费规则' },
  audit: { title: '审计日志', subtitle: '查看系统操作日志和变更记录' },
  config: { title: '运行配置', subtitle: '调整调度、缓存和兼容性参数' },
}
