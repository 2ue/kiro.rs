import {
  Activity,
  Boxes,
  ChartPie,
  Cpu,
  FileClock,
  Gauge,
  KeyRound,
  LayoutDashboard,
  Network,
  Settings2,
  Users,
  type LucideIcon,
} from 'lucide-react'

export interface NavItem {
  to: string
  label: string
  icon: LucideIcon
  description: string
  keywords?: string
}

export interface NavGroup {
  label: string
  items: NavItem[]
}

export const NAV: NavGroup[] = [
  {
    label: '监控',
    items: [
      {
        to: '/overview',
        label: '总览',
        icon: LayoutDashboard,
        description: '容量水位、健康度与需要关注的问题',
        keywords: 'dashboard home',
      },
      { to: '/requests', label: '请求', icon: Activity, description: '逐条请求明细、尝试链与错误详情', keywords: 'usage logs records' },
    ],
  },
  {
    label: '资源',
    items: [
      { to: '/accounts', label: 'Kiro 账号', icon: Users, description: '账号状态、额度、导入与批量处置', keywords: 'credentials accounts' },
      { to: '/pools', label: '外部池', icon: Boxes, description: '外部上游与兜底路由策略', keywords: 'external pools upstream' },
      { to: '/proxies', label: '代理', icon: Network, description: '出站代理资源与绑定', keywords: 'proxy' },
    ],
  },
  {
    label: '分析',
    items: [
      {
        to: '/billing',
        label: '成本与额度',
        icon: ChartPie,
        description: '费用拆分、额度生命周期、外部池盈亏',
        keywords: 'cost billing quota',
      },
      { to: '/scheduler', label: '调度器', icon: Gauge, description: '账号评分、选中分布与不可调度原因', keywords: 'scheduler' },
    ],
  },
  {
    label: '系统',
    items: [
      {
        to: '/settings',
        label: '运行配置',
        icon: Settings2,
        description: '调度、重试、流式、请求体与缓存参数',
        keywords: 'runtime config settings',
      },
      { to: '/models', label: '模型', icon: Cpu, description: '模型能力与价格目录', keywords: 'models pricing' },
      { to: '/access', label: '访问控制', icon: KeyRound, description: 'Admin Key 与请求 API Key', keywords: 'security keys' },
      { to: '/audit', label: '审计日志', icon: FileClock, description: '管理操作记录', keywords: 'audit' },
    ],
  },
]

export const NAV_ITEMS = NAV.flatMap((group) => group.items)

export function findNavItem(pathname: string): NavItem | undefined {
  return NAV_ITEMS.find((item) => pathname === item.to || pathname.startsWith(`${item.to}/`))
}
