import type { AddCredentialRequest } from '@/api/types'

export type ItemStatus = 'pending' | 'importing' | 'verifying' | 'success' | 'failed' | 'skipped' | 'cancelled'

export interface QueueItem {
  /** 在整份名单中的位置，用于代理轮换与展示序号 */
  index: number
  label: string
  credential: AddCredentialRequest
  sourceStatus?: string
  status: ItemStatus
  credentialId?: number
  email?: string
  /** 验活结果摘要，例如"订阅: KIRO PRO，用量 120/1000" */
  detail?: string
  warning?: string
  error?: string
  /** 失败发生的阶段，便于判断重试是否会重复创建 */
  failedAt?: 'import' | 'verify'
  startedAt?: number
  finishedAt?: number
  attempts: number
}

/** 单个账号的处理步骤，由调用方注入具体实现（便于测试） */
export interface QueueSteps {
  /** 是否在本地判定为已存在（按 refreshToken / API Key 哈希） */
  isDuplicate: (item: QueueItem) => Promise<boolean>
  add: (item: QueueItem) => Promise<{ credentialId: number; email?: string; warning?: string }>
  /** 返回验活结果摘要；抛错表示验活失败 */
  verify?: (item: QueueItem, credentialId: number) => Promise<string>
  /** 验活失败时回滚刚创建的账号 */
  rollback?: (credentialId: number) => Promise<void>
  shouldSkip?: (item: QueueItem) => string | undefined
}

export const TERMINAL: ReadonlySet<ItemStatus> = new Set(['success', 'failed', 'skipped', 'cancelled'])

export function summarize(items: QueueItem[]) {
  const count = (s: ItemStatus) => items.filter((i) => i.status === s).length
  const active = items.filter((i) => i.status === 'importing' || i.status === 'verifying')
  return {
    total: items.length,
    done: items.filter((i) => TERMINAL.has(i.status)).length,
    success: count('success'),
    failed: count('failed'),
    skipped: count('skipped'),
    cancelled: count('cancelled'),
    pending: count('pending'),
    active,
  }
}

/**
 * 逐个处理队列中的账号，每处理完一步就通过 onUpdate 回报单项状态。
 * concurrency 控制同时处理的账号数；shouldStop 返回 true 后不再开始新账号，
 * 剩余未开始的标记为 cancelled，可随后"继续"或"重试"。
 */
export async function processQueue(
  items: QueueItem[],
  targets: number[],
  steps: QueueSteps,
  opts: { concurrency: number; shouldStop: () => boolean; onUpdate: (index: number, patch: Partial<QueueItem>) => void },
): Promise<void> {
  const byIndex = new Map(items.map((i) => [i.index, i]))
  const queue = [...targets]
  const update = (index: number, patch: Partial<QueueItem>) => {
    const current = byIndex.get(index)
    if (current) byIndex.set(index, { ...current, ...patch })
    opts.onUpdate(index, patch)
  }

  const handle = async (index: number) => {
    const item = byIndex.get(index)!
    const attempts = item.attempts + 1
    update(index, {
      status: 'importing',
      error: undefined,
      detail: undefined,
      warning: undefined,
      failedAt: undefined,
      startedAt: Date.now(),
      finishedAt: undefined,
      attempts,
    })
    const skipReason = steps.shouldSkip?.(item)
    if (skipReason) {
      update(index, { status: 'skipped', error: skipReason, finishedAt: Date.now() })
      return
    }
    try {
      if (await steps.isDuplicate(item)) {
        update(index, { status: 'skipped', error: '已存在相同账号', finishedAt: Date.now() })
        return
      }
      const res = await steps.add(item)
      update(index, { credentialId: res.credentialId, email: res.email ?? item.email, warning: res.warning })
      if (!steps.verify) {
        update(index, { status: 'success', finishedAt: Date.now() })
        return
      }
      update(index, { status: 'verifying' })
      try {
        const detail = await steps.verify(byIndex.get(index)!, res.credentialId)
        update(index, { status: 'success', detail, finishedAt: Date.now() })
      } catch (e) {
        let rollbackNote = ''
        try {
          await steps.rollback?.(res.credentialId)
        } catch (re) {
          rollbackNote = `；回滚失败，请手动删除 #${res.credentialId}：${(re as Error).message}`
        }
        update(index, {
          status: 'failed',
          failedAt: 'verify',
          credentialId: undefined,
          error: `验活失败：${(e as Error).message}${rollbackNote}`,
          finishedAt: Date.now(),
        })
      }
    } catch (e) {
      const message = (e as Error).message ?? '导入失败'
      // 后端判定重复：视为跳过而不是失败
      if (/凭据已存在|重复/.test(message)) update(index, { status: 'skipped', error: '已存在相同账号', finishedAt: Date.now() })
      else update(index, { status: 'failed', failedAt: 'import', error: message, finishedAt: Date.now() })
    }
  }

  const worker = async () => {
    while (queue.length) {
      if (opts.shouldStop()) break
      const index = queue.shift()!
      await handle(index)
    }
  }
  await Promise.all(Array.from({ length: Math.max(1, Math.min(opts.concurrency, queue.length)) }, worker))
  // 停止后剩余未开始的账号标记为已取消
  for (const index of queue) update(index, { status: 'cancelled' })
}
