import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { poolsApi } from '@/api/endpoints/pools'
import type { CreateExternalPoolRequest, ExternalPoolStatus, ExternalPoolsStatusResponse, UpdateExternalPoolRequest } from '@/api/types'
import type { Tone } from '@/components/status/tone'
import { useConfirm } from '@/components/patterns/confirm'
import { qk } from '@/queries/keys'

export interface PoolState {
  tone: Tone
  label: string
  reason?: string
}

/** 外部池的统一状态：说明当前为什么接或不接流量 */
export function poolState(s: ExternalPoolStatus): PoolState {
  const p = s.pool
  if (!p.enabled) return { tone: 'neutral', label: '已停用' }
  if (p.autoDisabled)
    return {
      tone: 'danger',
      label: '自动禁用',
      reason: [
        p.autoDisabledReason,
        p.autoDisabledUntil ? `至 ${new Date(p.autoDisabledUntil).toLocaleString('zh-CN', { hour12: false })}` : undefined,
      ]
        .filter(Boolean)
        .join(' · '),
    }
  if (s.cooldownRemainingSecs > 0) return { tone: 'warning', label: '冷却中', reason: s.cooldownReason }
  if (!s.dispatchable) return { tone: 'warning', label: '不可派发', reason: s.skippedReason }
  if (s.quality?.inProbation) return { tone: 'info', label: '降级观察', reason: `退避层级 ${s.quality.probationLevel}` }
  if (p.maxConcurrentRequests > 0 && s.inFlight >= p.maxConcurrentRequests) return { tone: 'warning', label: '并发满' }
  if (s.inFlight > 0) return { tone: 'info', label: '处理中' }
  return { tone: 'success', label: '可派发' }
}

export function usePoolActions() {
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const invalidate = () => queryClient.invalidateQueries({ queryKey: qk.pools.all })

  const create = useMutation({
    mutationFn: (req: CreateExternalPoolRequest) => poolsApi.create(req),
    onSettled: invalidate,
    meta: { success: '外部池已创建', error: '创建失败' },
  })
  const update = useMutation({
    mutationFn: ({ id, req }: { id: number; req: UpdateExternalPoolRequest }) => poolsApi.update(id, req),
    onSettled: invalidate,
    meta: { success: '外部池已保存', error: '保存失败' },
  })
  const patchEnabled = (id: number, enabled: boolean) =>
    queryClient.setQueryData<ExternalPoolsStatusResponse>(qk.pools.status, (old) =>
      old ? { pools: old.pools.map((p) => (p.pool.id === id ? { ...p, pool: { ...p.pool, enabled } } : p)) } : old,
    )
  const setEnabled = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) => poolsApi.setEnabled(id, enabled),
    onMutate: async ({ id, enabled }) => {
      await queryClient.cancelQueries({ queryKey: qk.pools.status })
      patchEnabled(id, enabled)
    },
    onError: (_e, { id, enabled }) => patchEnabled(id, !enabled),
    onSettled: invalidate,
    meta: { error: '操作失败' },
  })
  const clearCooldown = useMutation({
    mutationFn: (id: number) => poolsApi.clearCooldown(id),
    onSettled: invalidate,
    meta: { success: '已清除冷却', error: '操作失败' },
  })
  const clearAutoDisabled = useMutation({
    mutationFn: (id: number) => poolsApi.clearAutoDisabled(id),
    onSettled: invalidate,
    meta: { success: '已解除自动禁用', error: '操作失败' },
  })
  const remove = useMutation({
    mutationFn: (id: number) => poolsApi.remove(id),
    onSettled: invalidate,
    meta: { success: '外部池已删除', error: '删除失败' },
  })

  return {
    create,
    update,
    setEnabled,
    clearCooldown,
    clearAutoDisabled,
    remove,
    /** 启停为可恢复操作：乐观切换并提供撤销 */
    async toggleEnabled(id: number, name: string, enabled: boolean) {
      await setEnabled.mutateAsync({ id, enabled })
      toast.success(`${enabled ? '已启用' : '已停用'}外部池「${name}」`, {
        action: {
          label: '撤销',
          onClick: () => void setEnabled.mutateAsync({ id, enabled: !enabled }).then(() => toast.success('已撤销')),
        },
      })
    },
    async confirmRemove(id: number, name: string) {
      const ok = await confirm({
        title: `删除外部池「${name}」？`,
        description: '删除后不可恢复，历史请求记录保留。',
        confirmText: '删除',
        destructive: true,
        typeToConfirm: name,
      })
      if (!ok) return false
      await remove.mutateAsync(id)
      return true
    },
  }
}
