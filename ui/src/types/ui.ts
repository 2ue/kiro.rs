import type { LucideIcon } from 'lucide-react'
import {
BarChart3,
Boxes,
FileClock,
FileInput,
Gauge,
LayoutDashboard,
Boxes as ModelsIcon,
Route,
Server,
ShieldCheck,
SlidersHorizontal,
Zap,
} from 'lucide-react'

export const CONSOLE_BASE_PATH = '/ui'

/** 四个一级任务域 */
export type DomainKey = 'overview' | 'resources' | 'analytics' | 'settings'

/** 页面 key(二级,或单页域本身) */
export type PageKey =
  | 'overview'
  | 'accounts'
  | 'usage'
  | 'accountRisk'
  | 'audit'
  | 'runtime'
  | 'routes'
  | 'policyTemplates'
  | 'configTransfer'
  | 'routeScenarios'
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
  { key: 'resources', label: '资源', icon: Server, path: 'accounts' },
  { key: 'analytics', label: '分析', icon: BarChart3, path: 'usage' },
  { key: 'settings', label: '设置', icon: SlidersHorizontal, path: 'runtime' },
]

export const navPages: NavPage[] = [
  // 总览(单页域)
  { key: 'overview', path: 'overview', label: '总览', description: '实时健康与关键指标', icon: LayoutDashboard, domain: 'overview' },

  // 资源域
  { key: 'accounts', path: 'accounts', label: '账号', description: '账号池与计费', icon: Boxes, domain: 'resources' },

  // 分析域
  { key: 'usage', path: 'usage', label: '用量', description: '请求趋势与明细', icon: BarChart3, domain: 'analytics' },
  { key: 'accountRisk', path: 'account-risk', label: '账号风控', description: '缓存与成本风险检查', icon: Gauge, domain: 'analytics' },
  { key: 'audit', path: 'audit', label: '审计', description: '关键操作记录', icon: FileClock, domain: 'analytics' },

  // 设置域
  { key: 'runtime', path: 'runtime', label: '运行', description: '调度、限流、冷却、缓存、兼容', icon: SlidersHorizontal, domain: 'settings' },
  { key: 'routes', path: 'routes', label: '路由', description: '入口策略、功能与规则', icon: Route, domain: 'resources' },
  { key: 'policyTemplates', path: 'policy-templates', label: '策略模板', description: '全局缓存与注入默认值', icon: Zap, domain: 'settings' },
  { key: 'configTransfer', path: 'settings/transfer', label: '配置迁移', description: '导出、预览与导入配置', icon: FileInput, domain: 'settings' },
  { key: 'routeScenarios', path: 'route-scenarios', label: '路由场景', description: '保存多套路由规则并一键切换', icon: Route, domain: 'settings' },
  { key: 'models', path: 'models', label: '模型', description: '模型能力与价格目录', icon: ModelsIcon, domain: 'settings' },
  { key: 'security', path: 'security', label: '安全', description: 'Admin Key、客户端接入 Key', icon: ShieldCheck, domain: 'settings' },
]

export function pagesOfDomain(domain: DomainKey): NavPage[] {
  return navPages.filter((p) => p.domain === domain)
}

export const pageMeta: Record<PageKey, { title: string; subtitle: string }> = {
  overview: { title: '总览', subtitle: '实时健康状态与关键指标一览' },
  accounts: { title: '账号', subtitle: '管理账号池与计费拆分' },
  usage: { title: '用量', subtitle: '请求趋势、Top 维度与明细记录' },
  accountRisk: { title: '账号风控', subtitle: '检查账号缓存分布与成本风险' },
  audit: { title: '审计', subtitle: '关键操作与变更记录' },
  runtime: { title: '运行配置', subtitle: '调度、限流、冷却、缓存与兼容策略' },
  routes: {title:'路由',subtitle:'入口前缀、策略、功能与账号和模型规则'},
  policyTemplates: {title:'策略模板',subtitle:'全局缓存模板、usage 默认值与注入文案'},
  configTransfer: {title:'配置迁移',subtitle:'导出配置并预览全量覆盖差异'},
  routeScenarios: { title: '路由场景', subtitle: '保存路由定义与账号规则快照，一键切换' },
  models: { title: '模型', subtitle: '模型能力与价格目录,支持发现与手动维护' },
  security: { title: '安全', subtitle: 'Admin Key 与客户端接入 Key 管理' },
}
