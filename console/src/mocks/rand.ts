/** 可复现的伪随机数（mulberry32），保证每次打开 mock 数据一致 */
export function createRand(seed = 20261002) {
  let s = seed >>> 0
  const next = () => {
    s = (s + 0x6d2b79f5) >>> 0
    let t = s
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  return {
    next,
    int: (min: number, max: number) => Math.floor(next() * (max - min + 1)) + min,
    float: (min: number, max: number) => next() * (max - min) + min,
    pick: <T>(list: readonly T[]): T => list[Math.floor(next() * list.length)]!,
    chance: (p: number) => next() < p,
    /** 按权重选择 */
    weighted: <T>(entries: ReadonlyArray<readonly [T, number]>): T => {
      const total = entries.reduce((a, [, w]) => a + w, 0)
      let r = next() * total
      for (const [v, w] of entries) {
        r -= w
        if (r <= 0) return v
      }
      return entries[entries.length - 1]![0]
    },
    hex: (len: number) => Array.from({ length: len }, () => Math.floor(next() * 16).toString(16)).join(''),
  }
}

export type Rand = ReturnType<typeof createRand>
