import { useCallback, useRef, useState } from 'react'
import { checkInventory, testInventory } from '@/api/inventory'
import { extractErrorMessage } from '@/lib/utils'

export type InventoryBatchKind = 'check' | 'test'

export interface InventoryBatchState {
  kind: InventoryBatchKind
  total: number
  done: number
  succeeded: number
  failed: number
  /** 正在等待刷新限流（毫秒）；0 表示没在等。 */
  waitMs: number
  /** 连续多次被限流后暂停，等用户继续。 */
  paused: boolean
  running: boolean
  cancelled: boolean
  failures: { id: number; message: string }[]
}

/** 批量查询每次请求的账号数，低于刷新突发上限。 */
const CHECK_CHUNK = 5
/** 测试逐个执行，间隔与账号页验活一致。 */
const TEST_GAP_MS = 2000
const FIRST_BACKOFF_MS = 2000
const MAX_BACKOFF_MS = 30_000
/** 连续这么多次整块被限流就暂停。 */
const MAX_DEFERRED_IN_A_ROW = 5

/**
 * 分块执行批量查询额度 / 测试，显示进度、可取消。
 *
 * 查询按账号池共用刷新速率的一半控制节奏，给账号池留出余量；被限流或正在续期的账号
 * （deferred）放回队首，等待时间从 2 秒起翻倍、最长 30 秒，不计失败；连续 5 次被限流就
 * 暂停，等用户点继续。取消只停止发出新请求，不中断进行中的那一个。
 */
export function useInventoryBatch({ refreshRpm, testModel, testPrompt, onFinished }: {
  refreshRpm: number
  testModel: string
  testPrompt: string
  onFinished?: () => void
}) {
  const [state, setState] = useState<InventoryBatchState | null>(null)
  const cancelRef = useRef(false)
  const resumeRef = useRef<(() => void) | null>(null)

  const wait = async (ms: number) => {
    const until = Date.now() + ms
    while (!cancelRef.current && Date.now() < until) {
      await new Promise((resolve) => setTimeout(resolve, Math.min(250, until - Date.now())))
    }
  }

  const run = useCallback(async (kind: InventoryBatchKind, ids: number[]) => {
    if (ids.length === 0) return
    cancelRef.current = false
    const queue = [...ids]
    let deferredInARow = 0
    let backoff = FIRST_BACKOFF_MS
    const perAccountMs = 60_000 / Math.max(1, refreshRpm / 2)
    setState({ kind, total: ids.length, done: 0, succeeded: 0, failed: 0, waitMs: 0, paused: false, running: true, cancelled: false, failures: [] })
    const record = (id: number, ok: boolean, message?: string) => setState((current) => current && ({
      ...current,
      done: current.done + 1,
      succeeded: current.succeeded + (ok ? 1 : 0),
      failed: current.failed + (ok ? 0 : 1),
      failures: ok ? current.failures : [...current.failures, { id, message: message || '失败' }],
    }))

    while (queue.length > 0 && !cancelRef.current) {
      const chunk = queue.splice(0, kind === 'check' ? CHECK_CHUNK : 1)
      const started = Date.now()
      const deferred: number[] = []
      try {
        if (kind === 'check') {
          const result = await checkInventory(chunk)
          for (const item of result.items) {
            if (item.deferred) deferred.push(item.id)
            else record(item.id, item.ok, item.message)
          }
        } else {
          const result = await testInventory(chunk[0], { model: testModel, prompt: testPrompt })
          if (result.outcome === 'deferred') deferred.push(chunk[0])
          else record(chunk[0], result.outcome === 'success', result.message)
        }
      } catch (e) {
        for (const id of chunk) record(id, false, extractErrorMessage(e))
      }

      if (deferred.length > 0) {
        queue.unshift(...deferred)
        deferredInARow += 1
        if (deferredInARow >= MAX_DEFERRED_IN_A_ROW) {
          setState((current) => current && { ...current, paused: true, waitMs: 0 })
          await new Promise<void>((resolve) => { resumeRef.current = resolve })
          resumeRef.current = null
          setState((current) => current && { ...current, paused: false })
          deferredInARow = 0
          backoff = FIRST_BACKOFF_MS
          continue
        }
        setState((current) => current && { ...current, waitMs: backoff })
        await wait(backoff)
        setState((current) => current && { ...current, waitMs: 0 })
        backoff = Math.min(backoff * 2, MAX_BACKOFF_MS)
        continue
      }
      deferredInARow = 0
      backoff = FIRST_BACKOFF_MS
      const budget = kind === 'check' ? perAccountMs * chunk.length : TEST_GAP_MS
      const spent = Date.now() - started
      if (queue.length > 0 && spent < budget) await wait(budget - spent)
    }
    setState((current) => current && { ...current, running: false, paused: false, waitMs: 0, cancelled: cancelRef.current })
    onFinished?.()
  }, [refreshRpm, testModel, testPrompt, onFinished])

  const cancel = useCallback(() => {
    cancelRef.current = true
    resumeRef.current?.()
  }, [])
  const resume = useCallback(() => { resumeRef.current?.() }, [])
  const dismiss = useCallback(() => setState(null), [])

  return { state, run, cancel, resume, dismiss }
}
