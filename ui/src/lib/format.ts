export function formatNumber(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '0'
  return new Intl.NumberFormat('zh-CN').format(value as number)
}

export function formatCompact(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const num = value as number
  const abs = Math.abs(num)
  // 小数据保持原始显示(千分位),不缩写
  if (abs < 1000) return new Intl.NumberFormat('zh-CN').format(num)
  const sign = num < 0 ? '-' : ''
  const trim = (n: number) => {
    // 最多两位小数,去掉尾随的 0(1.20→1.2, 1.00→1)
    const fixed = n.toFixed(2)
    return fixed.replace(/\.?0+$/, '')
  }
  if (abs >= 1_000_000_000) return `${sign}${trim(abs / 1_000_000_000)}B`
  if (abs >= 1_000_000) return `${sign}${trim(abs / 1_000_000)}M`
  return `${sign}${trim(abs / 1000)}K`
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
    minimumFractionDigits: 0,
    maximumFractionDigits: 2,
    useGrouping: true,
  }).format(value as number)
}

export function formatMeteringUsage(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const num = value as number
  return new Intl.NumberFormat('zh-CN', {
    maximumFractionDigits: num >= 1 ? 3 : 6,
  }).format(num)
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
