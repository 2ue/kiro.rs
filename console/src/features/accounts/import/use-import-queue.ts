import { useCallback, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { credentialsApi } from '@/api/endpoints/credentials'
import type { CredentialStatusItem } from '@/api/types'
import { qk } from '@/queries/keys'
import { mergeDefaults, type ImportDefaults, type ImportOptions } from './defaults'
import { processQueue, summarize, TERMINAL, type QueueItem, type QueueSteps } from './queue'

async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value))
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('')
}

/** 拉取现有账号的凭据哈希，用于本地判重（与旧版 UI 一致：refreshToken / API Key 的 SHA-256） */
async function loadExistingHashes(): Promise<Set<string>> {
  const hashes = new Set<string>()
  for (let page = 1; ; page++) {
    const res = await credentialsApi.list({ page, limit: 500 })
    for (const c of res.items as Array<Pick<CredentialStatusItem, 'refreshTokenHash' | 'apiKeyHash'>>) {
      if (c.refreshTokenHash) hashes.add(c.refreshTokenHash)
      if (c.apiKeyHash) hashes.add(c.apiKeyHash)
    }
    if (page >= res.totalPages) break
  }
  return hashes
}

/**
 * 导入队列：逐个账号导入 → 可选验活 → 失败回滚，实时更新每一项状态。
 * 默认同时处理 2 个账号；可停止、继续、单个或批量重试失败项。
 */
export function useImportQueue(config: { defaults: ImportDefaults; options: ImportOptions; testModel: string }) {
  const queryClient = useQueryClient()
  const [items, setItemsState] = useState<QueueItem[]>([])
  // 与 state 同步的最新列表，供异步流程读取（避免在 setState 回调里做副作用）
  const itemsRef = useRef<QueueItem[]>([])
  const setItems = useCallback((next: QueueItem[] | ((prev: QueueItem[]) => QueueItem[])) => {
    itemsRef.current = typeof next === 'function' ? next(itemsRef.current) : next
    setItemsState(itemsRef.current)
  }, [])
  const [running, setRunning] = useState(false)
  /** 本轮（开始或重试）的起止时间，用于显示已用时间 */
  const [runWindow, setRunWindow] = useState<{ start: number; end?: number } | null>(null)
  const stopRef = useRef(false)
  const hashesRef = useRef<Set<string> | null>(null)
  const configRef = useRef(config)
  configRef.current = config

  const patch = useCallback(
    (index: number, p: Partial<QueueItem>) => setItems((list) => list.map((i) => (i.index === index ? { ...i, ...p } : i))),
    [setItems],
  )

  const run = useCallback(
    async (targets: number[], source: QueueItem[]) => {
      if (!targets.length) return
      stopRef.current = false
      setRunning(true)
      setRunWindow({ start: Date.now() })
      const { defaults, options, testModel } = configRef.current
      try {
        hashesRef.current ??= await loadExistingHashes().catch(() => new Set<string>())
        const hashes = hashesRef.current
        const steps: QueueSteps = {
          shouldSkip: (item) =>
            options.skipErrorAccounts && item.sourceStatus === 'error' && item.attempts === 0
              ? '来源文件中标记为 error，已跳过'
              : undefined,
          isDuplicate: async (item) => {
            if (item.attempts > 0) return false
            const secret = item.credential.refreshToken ?? item.credential.kiroApiKey
            return secret ? hashes.has(await sha256Hex(secret)) : false
          },
          add: (item) =>
            credentialsApi.add({
              ...mergeDefaults(item.credential, defaults, item.index),
              autoDiscoverSupportedModels: options.autoDiscoverSupportedModels,
            }),
          verify: options.skipVerify
            ? undefined
            : async (_item, id) => {
                if (options.verifyMode === 'subscription_only') {
                  const info = await credentialsApi.balance(id)
                  return `订阅：${info.subscriptionTitle || '未知'}，用量 ${info.currentUsage}/${info.usageLimit}`
                }
                const tested = await credentialsApi.test(id, { model: testModel, prompt: 'hi' })
                if (options.refreshInfoAfterModelTest) await credentialsApi.balance(id).catch(() => undefined)
                return `${tested.model}：${tested.response.slice(0, 60)}`
              },
          rollback: async (id) => {
            await credentialsApi.setDisabled(id, true)
            await credentialsApi.remove(id)
          },
        }
        await processQueue(source, targets, steps, { concurrency: 2, shouldStop: () => stopRef.current, onUpdate: patch })
      } finally {
        setRunning(false)
        setRunWindow((w) => (w ? { ...w, end: Date.now() } : w))
        queryClient.invalidateQueries({ queryKey: qk.credentials.all })
      }
    },
    [patch, queryClient],
  )

  const reportDone = useCallback((list: QueueItem[]) => {
    const s = summarize(list)
    const msg = `导入结束：成功 ${s.success}，失败 ${s.failed}，跳过 ${s.skipped}${s.cancelled ? `，未处理 ${s.cancelled}` : ''}`
    if (s.failed || s.cancelled) toast.warning(msg)
    else toast.success(msg)
  }, [])

  return {
    items,
    running,
    runWindow,
    summary: summarize(items),
    /** 用新名单开始导入 */
    start: async (initial: QueueItem[]) => {
      hashesRef.current = null
      setItems(initial)
      await run(
        initial.map((i) => i.index),
        initial,
      )
      reportDone(itemsRef.current)
    },
    /** 重试指定账号（单个或批量） */
    retry: async (indexes: number[]) => {
      const current = itemsRef.current
      const targets = indexes.filter((i) => {
        const it = current.find((x) => x.index === i)
        return it && (it.status === 'failed' || it.status === 'cancelled')
      })
      targets.forEach((i) => patch(i, { status: 'pending', error: undefined }))
      if (!targets.length) return
      await run(targets, itemsRef.current)
      reportDone(itemsRef.current)
    },
    stop: () => {
      stopRef.current = true
    },
    reset: () => {
      setRunWindow(null)
      setItems([])
      hashesRef.current = null
    },
    isTerminal: (item: QueueItem) => TERMINAL.has(item.status),
  }
}
