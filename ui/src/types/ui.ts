import type { LucideIcon } from 'lucide-react'
import {
  BarChart3,
  DollarSign,
  FileClock,
  LayoutDashboard,
  Network,
  Server,
  Settings,
  ShieldCheck,
  Boxes,
} from 'lucide-react'

export type TabKey =
  | 'dashboard'
  | 'credentials'
  | 'validation'
  | 'proxies'
  | 'external'
  | 'usage'
  | 'pricing'
  | 'audit'
  | 'config'

export interface NavItem {
  key: TabKey
  path: string
  label: string
  description: string
  icon: LucideIcon
  group: 'overview' | 'resources' | 'insights' | 'system'
}

export const CONSOLE_BASE_PATH = '/ui'

export const navItems: NavItem[] = [
  { key: 'dashboard', path: 'dashboard', label: '总览', description: '系统状态概览', icon: LayoutDashboard, group: 'overview' },
  { key: 'credentials', path: 'credentials', label: '账号', description: '本地账号资源', icon: Server, group: 'resources' },
  { key: 'validation', path: 'validation', label: '校验', description: '账号可用性检查', icon: ShieldCheck, group: 'resources' },
  { key: 'proxies', path: 'proxies', label: '代理', description: '网络代理资源', icon: Network, group: 'resources' },
  { key: 'external', path: 'external-pools', label: '外部账号', description: '扩展账号池', icon: Boxes, group: 'resources' },
  { key: 'usage', path: 'usage', label: '用量', description: '请求与成本', icon: BarChart3, group: 'insights' },
  { key: 'pricing', path: 'pricing', label: '价格', description: '模型计价', icon: DollarSign, group: 'insights' },
  { key: 'audit', path: 'audit', label: '审计', description: '操作记录', icon: FileClock, group: 'insights' },
  { key: 'config', path: 'config', label: '配置', description: '运行设置', icon: Settings, group: 'system' },
]

export const navGroups: Array<{ id: NavItem['group']; label: string }> = [
  { id: 'overview', label: '概览' },
  { id: 'resources', label: '资源管理' },
  { id: 'insights', label: '数据洞察' },
  { id: 'system', label: '系统' },
]

export const pageMeta: Record<TabKey, { title: string; subtitle: string }> = {
  dashboard: { title: '总览', subtitle: '快速了解系统状态和关键变化' },
  credentials: { title: '账号管理', subtitle: '维护本地账号资源，保持服务稳定' },
  validation: { title: '账号校验', subtitle: '检查账号可用性，减少异常影响' },
  proxies: { title: '代理资源', subtitle: '维护网络资源和连通状态' },
  external: { title: '外部账号', subtitle: '维护扩展账号资源，提高服务稳定性' },
  usage: { title: '用量统计', subtitle: '查看使用情况和成本变化' },
  pricing: { title: '模型价格', subtitle: '维护价格信息，辅助成本核算' },
  audit: { title: '审计日志', subtitle: '查看关键操作记录' },
  config: { title: '运行配置', subtitle: '调整基础设置，控制运行表现' },
}
