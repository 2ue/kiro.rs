import { useEffect, useState } from 'react'

/**
 * 把高频变化的值延迟 `delay` 毫秒后输出,常用于搜索防抖。
 */
export function useDebouncedValue<T>(value: T, delay = 400): T {
  const [debounced, setDebounced] = useState(value)
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), delay)
    return () => clearTimeout(t)
  }, [value, delay])
  return debounced
}
