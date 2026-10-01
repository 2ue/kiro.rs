import { z } from 'zod'

export const requestsSearchSchema = z.object({
  q: z.string().optional().catch(undefined),
  status: z.enum(['success', 'error', 'stream_error', 'upstream_timeout', 'client_dropped']).optional().catch(undefined),
  route: z.enum(['local_credential', 'external_pool']).optional().catch(undefined),
  model: z.string().optional().catch(undefined),
  endpoint: z.string().optional().catch(undefined),
  credentialId: z.number().int().optional().catch(undefined),
  poolId: z.number().int().optional().catch(undefined),
  keyId: z.string().optional().catch(undefined),
  conversationId: z.string().optional().catch(undefined),
  stream: z.boolean().optional().catch(undefined),
  minTtft: z.number().optional().catch(undefined),
  minCacheRead: z.number().optional().catch(undefined),
  since: z.string().optional().catch(undefined),
  until: z.string().optional().catch(undefined),
  page: z.number().int().min(1).optional().catch(undefined),
  /** 打开详情的请求 ID */
  id: z.string().optional().catch(undefined),
})

export type RequestsSearch = z.infer<typeof requestsSearchSchema>
