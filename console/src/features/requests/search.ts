import { opt, searchSchema } from '@/lib/search-schema'

export const requestsSearchSchema = searchSchema({
  q: opt.str(),
  status: opt.enum(['success', 'error', 'stream_error', 'upstream_timeout', 'client_dropped']),
  route: opt.enum(['local_credential', 'external_pool']),
  model: opt.str(),
  endpoint: opt.str(),
  credentialId: opt.int(),
  poolId: opt.int(),
  keyId: opt.str(),
  conversationId: opt.str(),
  stream: opt.bool(),
  minTtft: opt.num(),
  minCacheRead: opt.num(),
  since: opt.str(),
  until: opt.str(),
  page: opt.int(1),
  /** 打开详情的请求 ID */
  id: opt.str(),
})

export type RequestsSearch = ReturnType<typeof requestsSearchSchema>
