const nf = new Intl.NumberFormat('zh-CN')

const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v)

export function fmtInt(v: number | null | undefined): string {
  return isNum(v) ? nf.format(Math.round(v)) : '—'
}

/** 1.2k / 3.4M 形式；用于 token 等大数 */
export function fmtCompact(v: number | null | undefined): string {
  if (!isNum(v)) return '—'
  const abs = Math.abs(v)
  if (abs >= 1e9) return `${(v / 1e9).toFixed(abs >= 1e10 ? 0 : 1)}B`
  if (abs >= 1e6) return `${(v / 1e6).toFixed(abs >= 1e7 ? 0 : 1)}M`
  if (abs >= 1e3) return `${(v / 1e3).toFixed(abs >= 1e4 ? 0 : 1)}k`
  return nf.format(Math.round(v))
}

export function fmtUsd(v: number | null | undefined, digits?: number): string {
  if (!isNum(v)) return '—'
  const abs = Math.abs(v)
  const d = digits ?? (abs === 0 ? 2 : abs < 0.01 ? 4 : abs < 1 ? 3 : 2)
  return `${v < 0 ? '-' : ''}$${Math.abs(v).toLocaleString('en-US', { minimumFractionDigits: d, maximumFractionDigits: d })}`
}

/** ratio 为 0~1 */
export function fmtPct(ratio: number | null | undefined, digits = 1): string {
  if (!isNum(ratio)) return '—'
  return `${(ratio * 100).toFixed(digits)}%`
}

export function fmtMs(v: number | null | undefined): string {
  if (!isNum(v)) return '—'
  if (v >= 60_000) return `${(v / 60_000).toFixed(1)}m`
  if (v >= 1000) return `${(v / 1000).toFixed(v >= 10_000 ? 0 : 1)}s`
  return `${Math.round(v)}ms`
}

/** 秒数转可读时长：45s / 3m / 2h 5m / 3d 4h */
export function fmtDuration(secs: number | null | undefined): string {
  if (!isNum(secs) || secs < 0) return '—'
  const s = Math.round(secs)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m${s % 60 && m < 10 ? ` ${s % 60}s` : ''}`
  const h = Math.floor(m / 60)
  if (h < 24) return `${h}h${m % 60 ? ` ${m % 60}m` : ''}`
  const d = Math.floor(h / 24)
  return `${d}d${h % 24 ? ` ${h % 24}h` : ''}`
}

export function toDate(v: string | number | null | undefined): Date | null {
  if (v === null || v === undefined || v === '') return null
  const d = typeof v === 'number' ? new Date(v < 1e12 ? v * 1000 : v) : new Date(v)
  return Number.isNaN(d.getTime()) ? null : d
}

export function fmtDateTime(v: string | number | null | undefined, withSeconds = false): string {
  const d = toDate(v)
  if (!d) return '—'
  return d.toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    ...(withSeconds ? { second: '2-digit' } : {}),
  })
}

export function fmtFullDateTime(v: string | number | null | undefined): string {
  const d = toDate(v)
  if (!d) return '—'
  return d.toLocaleString('zh-CN', { hour12: false })
}

/** 相对时间："刚刚" / "3 分钟前" / "2 小时后" */
export function fmtRelative(v: string | number | null | undefined, now = Date.now()): string {
  const d = toDate(v)
  if (!d) return '—'
  const diff = (d.getTime() - now) / 1000
  const abs = Math.abs(diff)
  if (abs < 30) return '刚刚'
  const text = fmtDuration(abs).split(' ')[0]
  return diff < 0 ? `${text}前` : `${text}后`
}

export function fmtBytes(v: number | null | undefined): string {
  if (!isNum(v)) return '—'
  if (v >= 1024 * 1024) return `${(v / 1024 / 1024).toFixed(1)} MB`
  if (v >= 1024) return `${(v / 1024).toFixed(1)} KB`
  return `${v} B`
}

export function maskSecret(value: string | null | undefined, head = 6, tail = 4): string {
  if (!value) return '—'
  if (value.length <= head + tail) return '•'.repeat(Math.max(4, value.length))
  return `${value.slice(0, head)}••••${value.slice(-tail)}`
}

export function safeRatio(part: number, total: number): number {
  return total > 0 ? part / total : 0
}
