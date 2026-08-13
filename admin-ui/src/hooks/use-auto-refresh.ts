import { useEffect, useMemo, useState } from 'react'

export interface AutoRefreshPreference {
  enabled: boolean
  intervalSeconds: number
}

export const AUTO_REFRESH_MIN_SECONDS = 5
export const AUTO_REFRESH_MAX_SECONDS = 3600
export const AUTO_REFRESH_DEFAULT_SECONDS = 10

function clampIntervalSeconds(value: unknown, fallback = AUTO_REFRESH_DEFAULT_SECONDS): number {
  const parsed = typeof value === 'number' ? value : Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.min(AUTO_REFRESH_MAX_SECONDS, Math.max(AUTO_REFRESH_MIN_SECONDS, Math.round(parsed)))
}

function readPreference(storageKey: string, defaultIntervalSeconds: number): AutoRefreshPreference {
  if (typeof window === 'undefined') {
    return { enabled: false, intervalSeconds: defaultIntervalSeconds }
  }
  try {
    const raw = window.localStorage.getItem(storageKey)
    if (!raw) return { enabled: false, intervalSeconds: defaultIntervalSeconds }
    const parsed = JSON.parse(raw) as Partial<AutoRefreshPreference>
    return {
      enabled: parsed.enabled === true,
      intervalSeconds: clampIntervalSeconds(parsed.intervalSeconds, defaultIntervalSeconds),
    }
  } catch {
    return { enabled: false, intervalSeconds: defaultIntervalSeconds }
  }
}

export function useAutoRefreshPreference(storageKey: string, defaultIntervalSeconds = AUTO_REFRESH_DEFAULT_SECONDS) {
  const normalizedDefault = clampIntervalSeconds(defaultIntervalSeconds)
  const [preference, setPreference] = useState<AutoRefreshPreference>(() => readPreference(storageKey, normalizedDefault))

  useEffect(() => {
    if (typeof window === 'undefined') return
    window.localStorage.setItem(storageKey, JSON.stringify(preference))
  }, [preference, storageKey])

  const refetchInterval = useMemo<number | false>(
    () => (preference.enabled ? preference.intervalSeconds * 1000 : false),
    [preference.enabled, preference.intervalSeconds]
  )

  return {
    enabled: preference.enabled,
    intervalSeconds: preference.intervalSeconds,
    refetchInterval,
    setEnabled: (enabled: boolean) => setPreference((current) => ({ ...current, enabled })),
    setIntervalSeconds: (intervalSeconds: number) => setPreference((current) => ({
      ...current,
      intervalSeconds: clampIntervalSeconds(intervalSeconds, normalizedDefault),
    })),
  }
}
