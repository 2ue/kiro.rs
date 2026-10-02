import { describe, expect, it } from 'vitest'
import { estimateRemainingSecs, runChunks, type RunProgress } from '@/features/accounts/import-runner'

const opts = (over: Partial<Parameters<typeof runChunks>[2]> = {}) => {
  const seen: RunProgress[] = []
  return { seen, o: { chunkSize: 3, concurrency: 2, shouldStop: () => false, onProgress: (p: RunProgress) => seen.push(p), ...over } }
}

describe('runChunks', () => {
  it('splits into chunks, keeps order, reports progress per chunk', async () => {
    const { seen, o } = opts()
    const offsets: number[] = []
    const res = await runChunks(
      [1, 2, 3, 4, 5, 6, 7],
      async (chunk, offset) => {
        offsets.push(offset)
        await new Promise((r) => setTimeout(r, chunk[0]! % 2 ? 5 : 1))
        return { items: chunk.map((x) => x * 10), ok: chunk.length, failed: 0, skipped: 0 }
      },
      o,
    )
    expect(offsets.sort((a, b) => a - b)).toEqual([0, 3, 6])
    expect(res.items).toEqual([10, 20, 30, 40, 50, 60, 70])
    expect(seen.map((p) => p.done)).toEqual([0, ...seen.slice(1).map((p) => p.done)])
    expect(seen.at(-1)).toMatchObject({ done: 7, total: 7, ok: 7 })
  })

  it('never exceeds concurrency', async () => {
    let inFlight = 0
    let peak = 0
    await runChunks(
      Array.from({ length: 20 }, (_, i) => i),
      async (chunk) => {
        peak = Math.max(peak, ++inFlight)
        await new Promise((r) => setTimeout(r, 2))
        inFlight--
        return { items: chunk, ok: chunk.length, failed: 0, skipped: 0 }
      },
      opts({ chunkSize: 2, concurrency: 3 }).o,
    )
    expect(peak).toBe(3)
  })

  it('a failed chunk counts as failed and others continue', async () => {
    const res = await runChunks(
      [1, 2, 3, 4, 5, 6],
      async (chunk, offset) => {
        if (offset === 3) throw new Error('timeout')
        return { items: chunk, ok: chunk.length, failed: 0, skipped: 0 }
      },
      opts({ concurrency: 1 }).o,
    )
    expect(res.errors).toEqual([{ offset: 3, count: 3, error: expect.any(Error) }])
    expect(res.progress).toMatchObject({ done: 6, ok: 3, failed: 3 })
  })

  it('stop halts dispatch after the in-flight chunk', async () => {
    let stop = false
    let calls = 0
    const res = await runChunks(
      [1, 2, 3, 4, 5, 6, 7, 8, 9],
      async (chunk) => {
        calls++
        stop = true
        return { items: chunk, ok: chunk.length, failed: 0, skipped: 0 }
      },
      opts({ concurrency: 1, shouldStop: () => stop }).o,
    )
    expect(calls).toBe(1)
    expect(res.stopped).toBe(true)
    expect(res.progress.done).toBe(3)
  })

  it('estimates remaining time', () => {
    expect(estimateRemainingSecs({ done: 5, total: 20, ok: 5, failed: 0, skipped: 0, startedAt: 0 }, 10_000)).toBe(30)
    expect(estimateRemainingSecs({ done: 0, total: 20, ok: 0, failed: 0, skipped: 0, startedAt: 0 }, 10_000)).toBeNull()
  })
})
