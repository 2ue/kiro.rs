import { useCallback, useRef, useState } from 'react'

export interface RunProgress {
  done: number
  total: number
  /** 当前已完成批次中成功/失败/跳过的累计数，用于实时展示 */
  ok: number
  failed: number
  skipped: number
  startedAt: number
}

export interface ChunkResult<R> {
  items: R[]
  ok: number
  failed: number
  skipped: number
}

/**
 * 分批执行：把一次长请求拆成多个小请求，按批回报进度，可中途停止。
 * 后端逐个处理账号（每个约 1~3 秒），一次性提交时前端只能干等；拆批后每批返回即可更新进度。
 *
 * - chunkSize：每批条数；越小进度越细，请求数越多
 * - concurrency：同时在途的批次数；保持较小，避免同时向上游发起过多 Token 刷新
 */
export interface RunOutcome<R> {
  items: R[]
  errors: Array<{ offset: number; count: number; error: unknown }>
  stopped: boolean
  progress: RunProgress
}

/** 纯执行逻辑：与 React 无关，便于测试 */
export async function runChunks<T, R>(
  inputs: T[],
  runChunk: (chunk: T[], offset: number) => Promise<ChunkResult<R>>,
  opts: { chunkSize: number; concurrency: number; shouldStop: () => boolean; onProgress: (p: RunProgress) => void },
): Promise<RunOutcome<R>> {
  const total = inputs.length
  const state: RunProgress = { done: 0, total, ok: 0, failed: 0, skipped: 0, startedAt: Date.now() }
  opts.onProgress({ ...state })
  const chunks: Array<{ offset: number; items: T[] }> = []
  for (let i = 0; i < total; i += opts.chunkSize) chunks.push({ offset: i, items: inputs.slice(i, i + opts.chunkSize) })
  const results: Array<R[] | undefined> = new Array(chunks.length)
  const errors: RunOutcome<R>['errors'] = []
  let cursor = 0
  const worker = async () => {
    while (!opts.shouldStop() && cursor < chunks.length) {
      const index = cursor++
      const chunk = chunks[index]!
      try {
        const res = await runChunk(chunk.items, chunk.offset)
        results[index] = res.items
        state.ok += res.ok
        state.failed += res.failed
        state.skipped += res.skipped
      } catch (error) {
        // 整批请求失败（网络、超时等）：记录下来，继续下一批
        errors.push({ offset: chunk.offset, count: chunk.items.length, error })
        state.failed += chunk.items.length
      }
      state.done += chunk.items.length
      opts.onProgress({ ...state })
    }
  }
  await Promise.all(Array.from({ length: Math.min(opts.concurrency, chunks.length) }, worker))
  return { items: results.flatMap((r) => r ?? []), errors, stopped: opts.shouldStop() && state.done < total, progress: { ...state } }
}

export function useChunkedRunner<T, R>(options: { chunkSize?: number; concurrency?: number } = {}) {
  const chunkSize = options.chunkSize ?? 3
  const concurrency = options.concurrency ?? 2
  const [progress, setProgress] = useState<RunProgress | null>(null)
  const [running, setRunning] = useState(false)
  const stopRef = useRef(false)

  const run = useCallback(
    async (inputs: T[], runChunk: (chunk: T[], offset: number) => Promise<ChunkResult<R>>) => {
      stopRef.current = false
      setRunning(true)
      try {
        return await runChunks(inputs, runChunk, { chunkSize, concurrency, shouldStop: () => stopRef.current, onProgress: setProgress })
      } finally {
        setRunning(false)
      }
    },
    [chunkSize, concurrency],
  )

  return {
    run,
    progress,
    running,
    /** 停止派发后续批次；已在途的批次会正常完成 */
    stop: () => {
      stopRef.current = true
    },
    reset: () => setProgress(null),
  }
}

/** 按已用时间和完成数估算剩余时间（秒） */
export function estimateRemainingSecs(p: RunProgress, now = Date.now()): number | null {
  if (p.done === 0 || p.done >= p.total) return null
  const elapsed = (now - p.startedAt) / 1000
  return Math.round((elapsed / p.done) * (p.total - p.done))
}
