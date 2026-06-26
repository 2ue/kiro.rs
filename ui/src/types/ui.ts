import type { LucideIcon } from 'lucide-react'
import {
  LayoutDashboard,
  Server,
  Boxes,
  Network,
  BarChart3,
  Wallet,
  FileClock,
  SlidersHorizontal,
  Boxes as ModelsIcon,
  ShieldCheck,
  FileCheck2,
} from 'lucide-react'

export const CONSOLE_BASE_PATH = '/ui'

/** 四个一级任务域 */
export type DomainKey = 'overview' | 'resources' | 'analytics' | 'settings'

/** 页面 key(二级,或单页域本身) */
export type PageKey =
  | 'overview'
  | 'credentials'
  | 'validation'
  | 'external'
  | 'proxies'
  | 'usage'
  | 'cost'
  | 'audit'
  | 'runtime'
  | 'models'
  | 'security'

export interface NavPage {
  key: PageKey
  /** 相对 basename 的路径(不含 /ui) */
  path: string
  label: string
  description: string
  icon: LucideIcon
  domain: DomainKey
}

export interface NavDomain {
  key: DomainKey
  label: string
  icon: LucideIcon
  /** 域入口路径(点击域名跳转的默认页) */
  path: string
}

export const navDomains: NavDomain[] = [
  { key: 'overview', label: '总览', icon: LayoutDashboard, path: 'overview' },
  { key: 'resources', label: '资源', icon: Server, path: 'credentials' },
  { key: 'analytics', label: '分析', icon: BarChart3, path: 'usage' },
  { key: 'settings', label: '设置', icon: SlidersHorizontal, path: 'runtime' },
]

export const navPages: NavPage[] = [
  // 总览(单页域)
  { key: 'overview', path: 'overview', label: '总览', description: '实时健康与关键指标', icon: LayoutDashboard, domain: 'overview' },

  // 资源域
  { key: 'credentials', path: 'credentials', label: '账号', description: '本地账号池:筛选、批量、导入、校验', icon: Server, domain: 'resources' },
  { key: 'validation', path: 'validation', label: '校验', description: '账号可用性与订阅校验', icon: FileCheck2, domain: 'resources' },
  { key: 'external', path: 'external-pools', label: '外部池', description: '外部账号池与计费', icon: Boxes, domain: 'resources' },
  { key: 'proxies', path: 'proxies', label: '代理', description: '网络代理资源', icon: Network, domain: 'resources' },

  // 分析域
  { key: 'usage', path: 'usage', label: '用量', description: '请求趋势与明细', icon: BarChart3, domain: 'analytics' },
  { key: 'cost', path: 'cost', label: '成本', description: '计费链路、盈亏与模型价格', icon: Wallet, domain: 'analytics' },
  { key: 'audit', path: 'audit', label: '审计', description: '关键操作记录', icon: FileClock, domain: 'analytics' },

  // 设置域
  { key: 'runtime', path: 'runtime', label: '运行', description: '调度、限流、冷却、缓存、兼容', icon: SlidersHorizontal, domain: 'settings' },
  { key: 'models', path: 'models', label: '模型', description: '模型能力与价格目录', icon: ModelsIcon, domain: 'settings' },
  { key: 'security', path: 'security', label: '安全', description: 'Admin Key、客户端接入 Key', icon: ShieldCheck, domain: 'settings' },
]

export function pagesOfDomain(domain: DomainKey): NavPage[] {
  return navPages.filter((p) => p.domain === domain)
}

export const pageMeta: Record<PageKey, { title: string; subtitle: string }> = {
  overview: { title: '总览', subtitle: '实时健康状态与关键指标一览' },
  credentials: { title: '账号', subtitle: '维护本地账号池,保持调度稳定' },
  validation: { title: '校验', subtitle: '账号可用性、订阅与用量校验' },
  external: { title: '外部池', subtitle: '管理外部账号池与计费拆分' },
  proxies: { title: '代理', subtitle: '维护网络代理资源与连通状态' },
  usage: { title: '用量', subtitle: '请求趋势、Top 维度与明细记录' },
  cost: { title: '成本', subtitle: '计费链路、外部池盈亏与模型价格' },
  audit: { title: '审计', subtitle: '关键操作与变更记录' },
  runtime: { title: '运行配置', subtitle: '调度、限流、冷却、缓存与兼容策略' },
  models: { title: '模型', subtitle: '模型能力与价格目录,支持同步与手动维护' },
  security: { title: '安全', subtitle: 'Admin Key 与客户端接入 Key 管理' },
}
