import { opt, searchSchema } from '@/lib/search-schema'

export const STATUS_SEGMENTS = [
  { key: 'all', label: '全部' },
  { key: 'enabled', label: '启用' },
  { key: 'cooldown', label: '冷却' },
  { key: 'rate_limited', label: '限流' },
  { key: 'error', label: '异常' },
  { key: 'disabled', label: '已禁用' },
] as const

export type StatusSegment = (typeof STATUS_SEGMENTS)[number]['key']

export const accountsSearchSchema = searchSchema({
  q: opt.str(),
  status: opt.enum([
    'all',
    'enabled',
    'cooldown',
    'rate_limited',
    'error',
    'disabled',
    'proxy_blocked',
    'custom_scheduling',
    'unknown_subscription',
  ]),
  /** 已禁用分段下的二级原因筛选（前端过滤，后端暂不支持） */
  reason: opt.str(),
  auth: opt.str(),
  sub: opt.str(),
  region: opt.str(),
  proxy: opt.int(),
  model: opt.str(),
  sort: opt.enum([
    'default',
    'id',
    'created_at',
    'priority',
    'last_used_at',
    'failure_count',
    'estimated_cost',
    'usage_percentage',
    'remaining_quota',
    'in_flight_requests',
    'scheduler_score',
  ]),
  order: opt.enum(['asc', 'desc']),
  page: opt.int(1),
  size: opt.int(),
  view: opt.enum(['table', 'cards']),
  /** 当前打开详情的账号 ID */
  id: opt.int(),
  tab: opt.str(),
  /** 打开导入向导 */
  import: opt.bool(),
})

export type AccountsSearch = ReturnType<typeof accountsSearchSchema>
