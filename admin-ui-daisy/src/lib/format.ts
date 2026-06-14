export function formatNumber(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '0'
  return new Intl.NumberFormat('zh-CN').format(value as number)
}

export function formatCompact(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const num = value as number
  if (Math.abs(num) >= 1_000_000) return `${(num / 1_000_000).toFixed(1)}M`
  if (Math.abs(num) >= 1000) return `${Math.round(num / 1000)}K`
  return String(num)
}

export function formatUsd(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const num = value as number
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: num >= 1 ? 2 : 6,
    maximumFractionDigits: num >= 1 ? 2 : 6,
  }).format(num)
}

export function formatQuota(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const num = value as number
  return new Intl.NumberFormat('zh-CN', {
    minimumFractionDigits: num >= 1 ? 2 : 6,
    maximumFractionDigits: num >= 1 ? 2 : 6,
  }).format(num)
}

export function formatCredits(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return new Intl.NumberFormat('zh-CN', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 20,
    useGrouping: true,
  }).format(value as number)
}

export function formatPricePerMillion(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return `$${(value * 1_000_000).toFixed(2)}/M`
}

export function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return `${(value * 100).toFixed(1)}%`
}

export function ratio(part: number, total: number): number {
  if (!Number.isFinite(part) || !Number.isFinite(total) || total <= 0) return Number.NaN
  return part / total
}

export function formatDate(value?: string | null): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

export function formatFullDate(value?: string | null): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

export function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未使用'
  const date = new Date(lastUsedAt)
  const diff = Date.now() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  return `${Math.floor(hours / 24)} 天前`
}

export function formatApproxElapsedMs(value?: number | null): string | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) return null
  const diff = Date.now() - value
  if (diff < 0) return '约刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `约${seconds}秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `约${minutes}分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `约${hours}小时前`
  return `约${Math.floor(hours / 24)}天前`
}
