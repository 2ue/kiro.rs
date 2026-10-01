import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useBlocker } from '@tanstack/react-router'
import { toast } from 'sonner'
import { systemApi } from '@/api/endpoints/system'
import type { RuntimeConfig } from '@/api/types'
import { diff, getAt, merge3, setAt, type FieldChange } from '@/domain/config-diff'
import { qk } from '@/queries/keys'
import { useRuntimeConfig } from '@/queries/shared'

/** 只读运行态字段，不参与编辑与提交 */
const READONLY_KEYS: Array<keyof RuntimeConfig> = ['auxiliaryUpstreamRuntime', 'tokenRefreshAdmissionRuntime', 'proxyUrl', 'proxyUsername', 'proxyPassword']

function editable(c: RuntimeConfig): RuntimeConfig {
  const copy = { ...c }
  for (const k of READONLY_KEYS) delete copy[k]
  return copy
}

/**
 * 运行配置草稿：
 * - 编辑期间远端更新不会覆盖草稿，而是记录为"远端已更新"
 * - 保存前与最新远端做三方合并，只叠加用户真正修改的字段，避免多个页面互相覆盖
 * - 存在未保存修改时拦截路由跳转与页面关闭
 */
export function useConfigDraft() {
  const queryClient = useQueryClient()
  const query = useRuntimeConfig()
  const [base, setBase] = useState<RuntimeConfig | null>(null)
  const [draft, setDraft] = useState<RuntimeConfig | null>(null)
  const [remote, setRemote] = useState<RuntimeConfig | null>(null)
  const draftRef = useRef<{ base: RuntimeConfig | null; draft: RuntimeConfig | null }>({ base: null, draft: null })
  draftRef.current = { base, draft }

  // 远端数据更新：没有本地修改时直接同步；有修改时只记录最新远端，保存时再合并
  useEffect(() => {
    if (!query.data) return
    const next = editable(query.data)
    setRemote(next)
    const { base: b, draft: d } = draftRef.current
    if (!b || !d || diff(b, d).length === 0) {
      setBase(next)
      setDraft(next)
    }
  }, [query.data])

  const changes: FieldChange[] = useMemo(() => (base && draft ? diff(base, draft) : []), [base, draft])
  const remoteChanged = useMemo(() => !!base && !!remote && changes.length > 0 && diff(base, remote).length > 0, [base, remote, changes.length])
  const dirty = changes.length > 0

  const get = useCallback(<V = unknown>(path: string) => (draft ? (getAt(draft, path) as V) : undefined), [draft])
  const baseValue = useCallback(<V = unknown>(path: string) => (base ? (getAt(base, path) as V) : undefined), [base])
  const set = useCallback((path: string, value: unknown) => setDraft((d) => (d ? setAt(d, path, value) : d)), [])
  const reset = useCallback((path?: string) => {
    if (!base) return
    if (!path) setDraft(base)
    else setDraft((d) => (d ? setAt(d, path, getAt(base, path)) : d))
  }, [base])

  const save = useMutation({
    mutationFn: async () => {
      if (!base || !draft) throw new Error('配置尚未加载')
      // 保存前拉取最新远端，基于它合并
      const latest = editable(await systemApi.runtimeConfig())
      const { merged, conflicts } = merge3(base, latest, draft)
      const saved = await systemApi.updateRuntimeConfig(merged)
      return { saved, conflicts }
    },
    onSuccess: ({ saved, conflicts }) => {
      const next = editable(saved)
      queryClient.setQueryData(qk.system.runtimeConfig, saved)
      setBase(next)
      setDraft(next)
      if (conflicts.length) toast.warning(`已保存。${conflicts.length} 个字段在你编辑期间被其他人修改，已以你的修改为准`)
      else toast.success('配置已保存，新请求立即生效')
    },
    meta: { error: '保存失败' },
  })

  useBlocker({
    shouldBlockFn: () => {
      if (!dirty) return false
      return !window.confirm('有未保存的配置修改，确定离开吗？')
    },
    enableBeforeUnload: () => dirty,
  })

  return {
    query,
    ready: !!draft,
    draft,
    base,
    changes,
    dirty,
    remoteChanged,
    get,
    baseValue,
    set,
    reset,
    save,
    /** 放弃本地修改并加载最新远端 */
    reload: () => {
      if (remote) {
        setBase(remote)
        setDraft(remote)
      }
    },
  }
}

export type ConfigDraft = ReturnType<typeof useConfigDraft>
