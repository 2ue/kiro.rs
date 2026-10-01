import { z } from 'zod'

export const STATUS_SEGMENTS = [
  { key: 'all', label: '全部' },
  { key: 'enabled', label: '启用' },
  { key: 'cooldown', label: '冷却' },
  { key: 'rate_limited', label: '限流' },
  { key: 'error', label: '异常' },
  { key: 'disabled', label: '已禁用' },
] as const

export type StatusSegment = (typeof STATUS_SEGMENTS)[number]['key']

export const accountsSearchSchema = z.object({
  q: z.string().optional().catch(undefined),
  status: z.enum(['all', 'enabled', 'cooldown', 'rate_limited', 'error', 'disabled', 'proxy_blocked', 'custom_scheduling', 'unknown_subscription']).optional().catch(undefined),
  /** 已禁用分段下的二级原因筛选（前端过滤，后端暂不支持） */
  reason: z.string().optional().catch(undefined),
  auth: z.string().optional().catch(undefined),
  sub: z.string().optional().catch(undefined),
  region: z.string().optional().catch(undefined),
  proxy: z.number().int().optional().catch(undefined),
  model: z.string().optional().catch(undefined),
  sort: z
    .enum([
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
    ])
    .optional()
    .catch(undefined),
  order: z.enum(['asc', 'desc']).optional().catch(undefined),
  page: z.number().int().min(1).optional().catch(undefined),
  size: z.number().int().optional().catch(undefined),
  view: z.enum(['table', 'cards']).optional().catch(undefined),
  /** 当前打开详情的账号 ID */
  id: z.number().int().optional().catch(undefined),
  tab: z.string().optional().catch(undefined),
  /** 打开导入向导 */
  import: z.boolean().optional().catch(undefined),
})

export type AccountsSearch = z.infer<typeof accountsSearchSchema>
