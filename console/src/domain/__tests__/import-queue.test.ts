import { describe, expect, it } from 'vitest'
import { initialDefaults, mergeDefaults, validateDefaults } from '@/features/accounts/import/defaults'
import { processQueue, type QueueItem, type QueueSteps } from '@/features/accounts/import/queue'

const make = (n: number, extra: Partial<QueueItem> = {}): QueueItem[] =>
  Array.from({ length: n }, (_, i) => ({
    index: i,
    label: `a${i}`,
    credential: { refreshToken: `rt${i}` },
    status: 'pending' as const,
    attempts: 0,
    ...extra,
  }))

async function run(items: QueueItem[], steps: Partial<QueueSteps>, opts: { concurrency?: number; stopAfter?: number } = {}) {
  const state = new Map(items.map((i) => [i.index, { ...i }]))
  const history: Array<[number, string]> = []
  let finished = 0
  await processQueue(
    items,
    items.map((i) => i.index),
    { isDuplicate: async () => false, add: async (it) => ({ credentialId: 100 + it.index }), ...steps },
    {
      concurrency: opts.concurrency ?? 1,
      shouldStop: () => opts.stopAfter !== undefined && finished >= opts.stopAfter,
      onUpdate: (index, patch) => {
        const cur = { ...state.get(index)!, ...patch }
        state.set(index, cur)
        if (patch.status) history.push([index, patch.status])
        if (patch.status && ['success', 'failed', 'skipped'].includes(patch.status)) finished++
      },
    },
  )
  return { state: [...state.values()], history }
}

describe('processQueue', () => {
  it('reports each item through importing → verifying → success', async () => {
    const { state, history } = await run(make(2), { verify: async () => '订阅：KIRO PRO' })
    expect(history.filter(([i]) => i === 0).map(([, s]) => s)).toEqual(['importing', 'verifying', 'success'])
    expect(state[0]).toMatchObject({ status: 'success', credentialId: 100, detail: '订阅：KIRO PRO', attempts: 1 })
  })

  it('rolls back and marks failed when verification fails', async () => {
    const rolled: number[] = []
    const { state } = await run(make(1), {
      verify: async () => {
        throw new Error('403 forbidden')
      },
      rollback: async (id) => void rolled.push(id),
    })
    expect(rolled).toEqual([100])
    expect(state[0]).toMatchObject({ status: 'failed', failedAt: 'verify', credentialId: undefined })
    expect(state[0]!.error).toContain('验活失败')
  })

  it('reports rollback failure in the error', async () => {
    const { state } = await run(make(1), {
      verify: async () => {
        throw new Error('x')
      },
      rollback: async () => {
        throw new Error('db down')
      },
    })
    expect(state[0]!.error).toContain('请手动删除 #100')
  })

  it('treats local and backend duplicates as skipped', async () => {
    const { state } = await run(make(2), {
      isDuplicate: async (it) => it.index === 0,
      add: async () => {
        throw new Error('凭据已存在（refreshToken 重复）')
      },
    })
    expect(state.map((s) => s.status)).toEqual(['skipped', 'skipped'])
  })

  it('skips by source status', async () => {
    const { state } = await run(make(1, { sourceStatus: 'error' }), {
      shouldSkip: (it) => (it.sourceStatus === 'error' ? 'error 账号' : undefined),
    })
    expect(state[0]).toMatchObject({ status: 'skipped', error: 'error 账号' })
  })

  it('stop marks remaining as cancelled', async () => {
    const { state } = await run(make(5), {}, { stopAfter: 2 })
    expect(state.map((s) => s.status)).toEqual(['success', 'success', 'cancelled', 'cancelled', 'cancelled'])
  })

  it('respects concurrency', async () => {
    let inFlight = 0
    let peak = 0
    await run(
      make(8),
      {
        add: async (it) => {
          peak = Math.max(peak, ++inFlight)
          await new Promise((r) => setTimeout(r, 3))
          inFlight--
          return { credentialId: it.index }
        },
      },
      { concurrency: 2 },
    )
    expect(peak).toBe(2)
  })

  it('retry only processes the given targets and increments attempts', async () => {
    const items = make(3).map((i, k) =>
      k === 1 ? { ...i, status: 'failed' as const, attempts: 1 } : { ...i, status: 'success' as const, attempts: 1 },
    )
    const calls: number[] = []
    await processQueue(
      items,
      [1],
      { isDuplicate: async () => false, add: async (it) => (calls.push(it.index), { credentialId: 9 }) },
      { concurrency: 2, shouldStop: () => false, onUpdate: () => {} },
    )
    expect(calls).toEqual([1])
  })
})

describe('import defaults', () => {
  it('fills only missing fields; account values win', () => {
    const d = { ...initialDefaults(), priority: '5', tags: ['t'], disabled: true }
    expect(mergeDefaults({ refreshToken: 'x' }, d, 0)).toMatchObject({ priority: 5, tags: ['t'], disabled: true })
    expect(mergeDefaults({ refreshToken: 'x', priority: 1, tags: ['own'], disabled: false }, d, 0)).toMatchObject({
      priority: 1,
      tags: ['own'],
      disabled: false,
    })
  })

  it('round-robin proxies by global index; account proxy wins', () => {
    const d = { ...initialDefaults(), proxyMode: 'round_robin' as const, proxyResourceIds: ['1', '2', '3'] }
    expect([0, 1, 2, 3].map((i) => mergeDefaults({ refreshToken: 'x' }, d, i).proxyResourceId)).toEqual([1, 2, 3, 1])
    expect(mergeDefaults({ refreshToken: 'x', proxyUrl: 'http://own' }, d, 0).proxyResourceId).toBeUndefined()
  })

  it('validates', () => {
    expect(validateDefaults({ ...initialDefaults(), proxyMode: 'round_robin' })).toContain('至少选择一个')
    expect(validateDefaults({ ...initialDefaults(), priority: '-1' })).toContain('非负整数')
    expect(validateDefaults(initialDefaults())).toBeUndefined()
  })
})
