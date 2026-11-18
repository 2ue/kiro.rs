import { useEffect } from 'react'
import { useBlocker } from 'react-router-dom'

import { useConfirm } from '@/components/patterns'

/**
 * 有未保存修改时拦住离开：站内跳转弹确认，关闭/刷新页签走浏览器原生提示。
 *
 * 只在 `dirty` 为 true 时生效；保存成功后调用方把 dirty 置回 false，拦截随之解除。
 * 站内跳转用 react-router 的 blocker，所以只拦截 data router 内的导航；侧栏、返回按钮、
 * 浏览器前进后退都会经过它。
 */
export function useUnsavedChangesGuard(dirty: boolean, message = '当前页面有未保存的修改，离开后会丢失。') {
  const confirm = useConfirm()
  const blocker = useBlocker(({ currentLocation, nextLocation }) => dirty && currentLocation.pathname !== nextLocation.pathname)

  useEffect(() => {
    if (blocker.state !== 'blocked') return
    let cancelled = false
    void confirm({
      title: '放弃未保存的修改？',
      message,
      confirmText: '离开',
      cancelText: '留在本页',
      tone: 'danger',
    }).then((ok) => {
      if (cancelled) return
      if (ok) blocker.proceed()
      else blocker.reset()
    })
    return () => {
      cancelled = true
    }
  }, [blocker, confirm, message])

  useEffect(() => {
    if (!dirty) return
    const handler = (event: BeforeUnloadEvent) => {
      event.preventDefault()
      // 现代浏览器忽略自定义文案，但需要给 returnValue 赋值才会弹出原生提示。
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', handler)
    return () => window.removeEventListener('beforeunload', handler)
  }, [dirty])
}
