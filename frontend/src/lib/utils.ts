import { type ClassValue, clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

export function extractErrorMessage(error: unknown): string {
  if (!error) return '未知错误'
  if (typeof error === 'string') return error
  if (error instanceof Error) return error.message
  // axios error
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const axiosErr = error as any
  if (axiosErr?.response?.data?.error?.message) {
    return String(axiosErr.response.data.error.message)
  }
  if (axiosErr?.response?.data?.message) {
    return String(axiosErr.response.data.message)
  }
  if (axiosErr?.message) return String(axiosErr.message)
  try {
    return JSON.stringify(error)
  } catch {
    return '未知错误'
  }
}

export function formatNumber(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return new Intl.NumberFormat('zh-CN').format(value)
}

export function formatPercent(value: number, fractionDigits = 1): string {
  if (!Number.isFinite(value)) return '-'
  return `${(value * 100).toFixed(fractionDigits)}%`
}

export function formatUsd(value: number, fractionDigits = 6): string {
  if (!Number.isFinite(value)) return '-'
  return `$${value.toFixed(fractionDigits)}`
}

export function formatDateTime(value: string | number | Date): string {
  const d = value instanceof Date ? value : new Date(value)
  if (Number.isNaN(d.getTime())) return '-'
  return d.toLocaleString('zh-CN', {
    hour12: false,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

export function formatRelative(value: string | null | undefined): string {
  if (!value) return '从未'
  const d = new Date(value)
  const diff = Date.now() - d.getTime()
  if (Number.isNaN(diff)) return '-'
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  const days = Math.floor(hours / 24)
  return `${days} 天前`
}

export async function sha256Hex(input: string): Promise<string> {
  const buffer = new TextEncoder().encode(input)
  const digest = await crypto.subtle.digest('SHA-256', buffer)
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('')
}
