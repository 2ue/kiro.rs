import { useEffect, useState } from 'react'

/** localStorage 持久化的组件状态（列显隐等 UI 偏好） */
export function usePersistedState<T>(key: string, initial: T) {
  const [value, setValue] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(`kiro-console.${key}`)
      return raw ? (JSON.parse(raw) as T) : initial
    } catch {
      return initial
    }
  })
  useEffect(() => {
    localStorage.setItem(`kiro-console.${key}`, JSON.stringify(value))
  }, [key, value])
  return [value, setValue] as const
}
