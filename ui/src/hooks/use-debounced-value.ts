import { useEffect, useState } from 'react'

/**
 * 返回 value 的防抖副本：value 停止变化 delay 毫秒后才更新返回值。
 * 用于把高频输入(搜索/筛选框)与下游网络查询解耦，避免每次按键都触发请求。
 */
export function useDebouncedValue<T>(value: T, delay = 300): T {
  const [debounced, setDebounced] = useState(value)
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delay)
    return () => clearTimeout(timer)
  }, [value, delay])
  return debounced
}
