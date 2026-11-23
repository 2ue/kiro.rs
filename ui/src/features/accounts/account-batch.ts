// 批量操作的纯逻辑：限并发执行器、批量修改的逐账号补丁计算、overrides 合并。
// 后端没有批量接口，批量操作都在前端逐个调用单账号接口。
import type {
  Account,
  AccountOverrides,
  AutoDisablePolicy,
  RouteRuleMode,
  StreamRetryMode,
  UpdateAccountRequest,
  UsageProjectionMode,
} from '@/types/api'
import { MAX_TAGS, addTags, cleanTags, removeTags, tagsDroppedByLimit } from '../../components/patterns/tag-utils.ts'

/** 批量操作同时进行的请求数。 */
export const BATCH_CONCURRENCY = 4

/**
 * 起 `concurrency` 个 worker 从同一个队列取任务；`shouldStop` 返回 true 后不再取新任务，
 * 已经开始的任务照常完成。worker 抛出的错误由调用方自己处理，这里不吞也不中断别的任务。
 */
export async function runWithConcurrency<T>(
  items: readonly T[],
  worker: (item: T, index: number) => Promise<void>,
  options: { concurrency?: number; shouldStop?: () => boolean } = {},
): Promise<{ stopped: boolean; started: number }> {
  const concurrency = Math.max(1, Math.floor(options.concurrency ?? BATCH_CONCURRENCY))
  let next = 0
  let started = 0
  let stopped = false
  const run = async () => {
    while (next < items.length) {
      if (options.shouldStop?.()) {
        stopped = true
        return
      }
      const index = next++
      started += 1
      await worker(items[index], index)
    }
  }
  await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, run))
  return { stopped: stopped && started < items.length, started }
}

export type BatchRowStatus = 'pending' | 'running' | 'success' | 'failed' | 'skipped'

export interface BatchRow {
  id: number
  label: string
  status: BatchRowStatus
  message?: string
  /** 点了停止后还没开始的行：标为“跳过：已停止”，可以用“继续剩余”只跑这些。 */
  notStarted?: boolean
}

/** 停止时还没开始的行的说明。 */
export const BATCH_NOT_STARTED_MESSAGE = '已停止'

/** 一轮执行结束后，把这轮里没开始的行标成“跳过：已停止”；已经跑过的行不动。 */
export function markNotStartedRows(rows: readonly BatchRow[], indices: readonly number[], started: ReadonlySet<number>): BatchRow[] {
  const skipped = new Set(indices.filter((index) => !started.has(index)))
  if (skipped.size === 0) return [...rows]
  return rows.map((row, index) => (skipped.has(index) ? { ...row, status: 'skipped', message: BATCH_NOT_STARTED_MESSAGE, notStarted: true } : row))
}

/** “继续剩余”要跑的行：上次停止时没开始的。 */
export function notStartedIndices(rows: readonly BatchRow[]): number[] {
  return rows.flatMap((row, index) => (row.notStarted ? [index] : []))
}

/** 把要重跑的行重置为等待；用于“重试失败”和“继续剩余”。 */
export function resetRows(rows: readonly BatchRow[], indices: readonly number[]): BatchRow[] {
  const reset = new Set(indices)
  return rows.map((row, index) => (reset.has(index) ? { id: row.id, label: row.label, status: 'pending' } : row))
}

export interface BatchTaskResult {
  status: 'success' | 'failed' | 'skipped'
  message?: string
}

export function summarizeBatchRows(rows: readonly Pick<BatchRow, 'status'>[]) {
  const count = (status: BatchRowStatus) => rows.filter((row) => row.status === status).length
  const success = count('success')
  const failed = count('failed')
  const skipped = count('skipped')
  return {
    total: rows.length,
    pending: count('pending'),
    running: count('running'),
    success,
    failed,
    skipped,
    finished: success + failed + skipped,
  }
}

// ---------------------------------------------------------------------------
// 批量修改
// ---------------------------------------------------------------------------

export type OverrideKey = keyof AccountOverrides

export const OVERRIDE_KEYS: OverrideKey[] = [
  'requestTimeoutSecs',
  'streamRequestTimeoutSecs',
  'streamIdleTimeoutSecs',
  'streamFirstOutputTimeoutSecs',
  'usageProjectionUpliftPercent',
  'usageProjectionCostFloorEnabled',
  'usageProjectionCostFloorMarginPercent',
  'usageProjectionOutputUpliftMinTokens',
  'usageProjectionOutputUpliftPercent',
]

export type SupportedModelsEditMode = 'replace' | 'append' | 'remove' | 'clear'

/** 批量修改草稿：只有出现的字段才会修改；overrides 里值为 null 表示恢复继承全局。 */
export interface BatchEditDraft {
  priority?: number
  maxConcurrentRequests?: number
  costMultiplier?: number
  enabled?: boolean
  usageProjectionMode?: UsageProjectionMode
  autoDisablePolicy?: AutoDisablePolicy
  preOutputStreamRetryMode?: StreamRetryMode
  tags?: { add: string[]; remove: string[] }
  route?: { routeMode: RouteRuleMode; routeRules: string[] }
  supportedModels?: { mode: SupportedModelsEditMode; models: string[]; modelFilterMode?: 'allowlist' | 'denylist' }
  proxy?: { mode: 'set' | 'clear'; url: string }
  overrides?: Partial<Record<OverrideKey, number | boolean | null>>
}

/**
 * overrides 在后端是整体替换：先拿账号现有的，再把这次改的键合进去；null 表示删掉这个键（恢复全局）。
 * 只保留已知键且值非空，避免把未知字段发给 deny_unknown_fields 的后端。
 */
export function mergeOverrides(
  current: AccountOverrides | null | undefined,
  changes: Partial<Record<OverrideKey, number | boolean | null>>,
): AccountOverrides {
  const merged: Record<string, number | boolean> = {}
  for (const key of OVERRIDE_KEYS) {
    const value = key in changes ? changes[key] : current?.[key]
    if (value !== null && value !== undefined) merged[key] = value
  }
  return merged as AccountOverrides
}

function overridesEqual(left: AccountOverrides | null | undefined, right: AccountOverrides): boolean {
  return OVERRIDE_KEYS.every((key) => (left?.[key] ?? null) === (right[key] ?? null))
}

const modelKey = (model: string) => model.trim().toLowerCase()

function dedupeModels(models: readonly string[]): string[] {
  const seen = new Set<string>()
  const result: string[] = []
  for (const model of models) {
    const trimmed = model.trim()
    if (!trimmed || seen.has(modelKey(trimmed))) continue
    seen.add(modelKey(trimmed))
    result.push(trimmed)
  }
  return result
}

/** 按操作方式算出新的支持模型列表；比较忽略大小写，与后端去重口径一致。 */
export function applySupportedModelsEdit(current: readonly string[], edit: { mode: SupportedModelsEditMode; models: string[] }): string[] {
  const incoming = dedupeModels(edit.models)
  if (edit.mode === 'clear') return []
  if (edit.mode === 'replace') return incoming
  if (edit.mode === 'append') return dedupeModels([...current, ...incoming])
  const drop = new Set(incoming.map(modelKey))
  return dedupeModels(current).filter((model) => !drop.has(modelKey(model)))
}

const sameList = (left: readonly string[], right: readonly string[]) =>
  left.length === right.length && left.every((item, index) => item === right[index])

export interface BatchUpdatePlan {
  /** PUT /accounts/{id} 的请求体，只含这次真正变化的字段。 */
  update: UpdateAccountRequest
  /** 启停走 POST /accounts/{id}/enabled（会记账号异动），单独返回。 */
  enabled?: boolean
  /** 变化说明，用于结果表。 */
  changes: string[]
  /** 没能照做的部分（比如标签已达上限），和 changes 一起显示在结果表里。 */
  warnings: string[]
}

const listText = (items: readonly string[]) => (items.length ? items.join('、') : '无')

/** 对单个账号算出这次批量修改真正要改的字段；没有任何变化时 changes 为空，调用方跳过这个账号。 */
export function buildBatchUpdate(account: Account, draft: BatchEditDraft): BatchUpdatePlan {
  const update: UpdateAccountRequest = {}
  const changes: string[] = []
  const warnings: string[] = []
  let enabled: boolean | undefined

  if (draft.priority !== undefined && draft.priority !== account.priority) {
    update.priority = draft.priority
    changes.push(`优先级 ${account.priority} → ${draft.priority}`)
  }
  if (draft.maxConcurrentRequests !== undefined && draft.maxConcurrentRequests !== account.maxConcurrentRequests) {
    update.maxConcurrentRequests = draft.maxConcurrentRequests
    changes.push(`并发 ${account.maxConcurrentRequests} → ${draft.maxConcurrentRequests}`)
  }
  if (draft.costMultiplier !== undefined && draft.costMultiplier !== account.costMultiplier) {
    update.costMultiplier = draft.costMultiplier
    changes.push(`成本倍率 ×${account.costMultiplier} → ×${draft.costMultiplier}`)
  }
  if (draft.usageProjectionMode !== undefined && draft.usageProjectionMode !== account.usageProjectionMode) {
    update.usageProjectionMode = draft.usageProjectionMode
    changes.push(draft.usageProjectionMode === 'current_path_policy' ? 'usage 改为按路径整理' : 'usage 改为透传上游')
  }
  if (draft.autoDisablePolicy !== undefined && draft.autoDisablePolicy !== account.autoDisablePolicy) {
    update.autoDisablePolicy = draft.autoDisablePolicy
    changes.push('自动禁用策略')
  }
  if (draft.preOutputStreamRetryMode !== undefined && draft.preOutputStreamRetryMode !== (account.preOutputStreamRetryMode || 'inherit')) {
    update.preOutputStreamRetryMode = draft.preOutputStreamRetryMode
    changes.push('首输出前流式恢复')
  }
  if (draft.tags) {
    // 已有标签只去空白去重，不截断；先移除再追加，移除腾出的名额可以给新标签用。
    const current = cleanTags(account.tags ?? [])
    const removed = removeTags(current, draft.tags.remove)
    const removing = new Set(cleanTags(draft.tags.remove))
    const adding = cleanTags(draft.tags.add).filter((tag) => !removing.has(tag))
    const next = addTags(removed, adding)
    const dropped = tagsDroppedByLimit(removed, adding)
    if (dropped.length > 0) warnings.push(`标签已达 ${MAX_TAGS} 个上限，未加入：${dropped.join('、')}`)
    if (!sameList(current, next)) {
      update.tags = next
      changes.push(`标签：${listText(next)}`)
    }
  }
  if (draft.route) {
    const routeRules = draft.route.routeMode === 'allow_all' ? [] : [...draft.route.routeRules]
    const currentMode = account.routeMode || 'allow_all'
    if (draft.route.routeMode !== currentMode || !sameList(account.routeRules ?? [], routeRules)) {
      update.routeMode = draft.route.routeMode
      update.routeRules = routeRules
      changes.push('路由限制')
    }
  }
  if (draft.supportedModels) {
    const current = account.supportedModels ?? []
    const mode = draft.supportedModels.modelFilterMode ?? account.modelFilterMode ?? 'allowlist'
    const currentMode = account.modelFilterMode ?? 'allowlist'
    if (mode !== currentMode && !['replace', 'clear'].includes(draft.supportedModels.mode)) {
      throw new Error('切换白名单/黑名单时请使用替换或清空，避免反转原列表含义')
    }
    if (mode !== currentMode) {
      update.modelFilterMode = mode
      changes.push(mode === 'denylist' ? '切换黑名单' : '切换白名单')
    }
    const next = applySupportedModelsEdit(current, draft.supportedModels)
    if (!sameList(current, next)) {
      update.supportedModels = next
      changes.push(next.length ? `支持模型 ${next.length} 个` : '支持模型改为不限制')
    }
  }
  if (draft.proxy) {
    const current = account.proxyUrl?.trim() || null
    if (draft.proxy.mode === 'clear') {
      if (current) {
        update.proxyUrl = null
        changes.push('清除代理')
      }
    } else {
      const url = draft.proxy.url.trim()
      if (url && url !== current) {
        update.proxyUrl = url
        changes.push('设置代理')
      }
    }
  }
  if (draft.overrides && Object.keys(draft.overrides).length > 0) {
    const next = mergeOverrides(account.overrides, draft.overrides)
    if (!overridesEqual(account.overrides, next)) {
      update.overrides = next
      changes.push('覆盖全局参数')
    }
  }
  if (draft.enabled !== undefined && draft.enabled !== account.enabled) {
    enabled = draft.enabled
    changes.push(draft.enabled ? '启用' : '停用')
  }
  return { update, enabled, changes, warnings }
}

/** 后端读接口把代理地址里的用户名、密码打码成这个占位。 */
export const MASKED_CREDENTIAL = '***'

/** 批量修改代理时不允许 *** 占位：单个账号编辑时 *** 表示沿用该账号已保存的凭据，批量时每个账号的原凭据各不相同。 */
export const BATCH_PROXY_MASK_ERROR =
  '批量修改的代理地址不能带 ***：*** 只是单个账号已保存凭据的打码，批量时每个账号原来的用户名和密码各不相同，无法逐个沿用。请填写完整的用户名和密码（或不带凭据的地址）。'

const isNonNegativeInteger = (value: number | undefined) => value === undefined || (Number.isInteger(value) && value >= 0)

/** 提交前的校验；返回第一条错误，没有错误返回 null。 */
export function validateBatchEditDraft(draft: BatchEditDraft): string | null {
  if (draft.priority !== undefined && !Number.isInteger(draft.priority)) return '优先级必须是整数'
  if (draft.maxConcurrentRequests !== undefined && !(Number.isInteger(draft.maxConcurrentRequests) && draft.maxConcurrentRequests >= 1 && draft.maxConcurrentRequests <= 10000)) {
    return '单账号最大并发必须是 1 到 10000 的整数'
  }
  if (draft.costMultiplier !== undefined && !(Number.isFinite(draft.costMultiplier) && draft.costMultiplier > 0)) return '成本倍率必须大于 0'
  if (draft.tags && draft.tags.add.length === 0 && draft.tags.remove.length === 0) return '标签：至少填一个要添加或移除的标签'
  if (draft.route?.routeMode === 'allow_list' && draft.route.routeRules.length === 0) {
    return '只允许模式至少需要一条路由，否则这些账号不会承接任何请求'
  }
  if (draft.supportedModels && draft.supportedModels.mode !== 'clear' && draft.supportedModels.models.length === 0) {
    return draft.supportedModels.mode === 'replace' ? '支持模型：替换为空列表请选“清空（不限制）”' : '支持模型：至少填一个模型'
  }
  if (draft.proxy?.mode === 'set') {
    const url = draft.proxy.url.trim()
    if (!url) return '代理：请填写代理地址，或改选“清空”'
    if (!/^(https?|socks5h?):\/\/[^\s]+$/i.test(url)) return '代理地址必须以 http://、https://、socks5:// 或 socks5h:// 开头'
    if (url.includes(MASKED_CREDENTIAL)) return BATCH_PROXY_MASK_ERROR
  }
  for (const [key, value] of Object.entries(draft.overrides ?? {})) {
    if (typeof value === 'number' && !isNonNegativeInteger(value)) return `覆盖参数 ${key} 必须是非负整数`
  }
  return null
}

/** 批量修改一共勾了几项（用于“将修改 N 项”）。 */
export function countBatchEditFields(draft: BatchEditDraft): number {
  let count = 0
  for (const [key, value] of Object.entries(draft)) {
    if (value === undefined) continue
    count += key === 'overrides' ? Object.keys(value as object).length : 1
  }
  return count
}
