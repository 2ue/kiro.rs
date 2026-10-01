import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { credentialsApi } from '@/api/endpoints/credentials'
import type { BatchUpdateCredentialsRequest, CredentialExportFormat, CredentialStatusItem } from '@/api/types'
import { useConfirm } from '@/components/patterns/confirm'
import { qk } from '@/queries/keys'
import { toAccountRow } from './queries'

/**
 * 账号相关操作的唯一入口。页面工具栏、批量栏、详情面板、命令面板都调用这里，
 * 保证同一操作在不同位置行为一致。
 */
export function useAccountActions() {
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const invalidate = () => queryClient.invalidateQueries({ queryKey: qk.credentials.all })

  /**
   * 乐观更新：先改本地缓存中的 disabled 字段，请求失败的账号回滚。
   * 列表、详情、调度器共用 ['credentials'] 下的缓存，统一处理。
   */
  const patchCached = (ids: Set<number>, disabled: boolean) => {
    queryClient.setQueriesData<unknown>({ queryKey: qk.credentials.all }, (old: unknown) => {
      if (!old || typeof old !== 'object') return old
      const o = old as { credentials?: CredentialStatusItem[]; id?: number; disabled?: boolean }
      if (Array.isArray(o.credentials)) {
        return {
          ...o,
          credentials: o.credentials.map((c) => (ids.has(c.id) ? { ...c, disabled, disabledReason: disabled ? 'Manual' : undefined } : c)),
        }
      }
      if (typeof o.id === 'number' && ids.has(o.id) && 'status' in o) {
        return toAccountRow({ ...(o as unknown as CredentialStatusItem), disabled, disabledReason: disabled ? 'Manual' : undefined })
      }
      return old
    })
  }

  const setDisabled = useMutation({
    mutationFn: ({ ids, disabled }: { ids: number[]; disabled: boolean }) =>
      settleAll(ids, (id) => credentialsApi.setDisabled(id, disabled)),
    onMutate: async ({ ids, disabled }) => {
      await queryClient.cancelQueries({ queryKey: qk.credentials.all })
      patchCached(new Set(ids), disabled)
    },
    onSuccess: (res, { disabled }) => {
      // 部分失败：把失败的账号改回去
      if (res.failed) patchCached(new Set(res.errors.map((e) => e.id)), !disabled)
    },
    onError: (_e, { ids, disabled }) => patchCached(new Set(ids), !disabled),
    onSettled: invalidate,
    meta: { error: '操作失败' },
  })

  const refreshInfo = useMutation({
    mutationFn: (ids: number[]) => credentialsApi.refreshInfo(ids, true),
    onSuccess: (res) => {
      if (res.failed > 0) toast.warning(`额度刷新完成：成功 ${res.success}，失败 ${res.failed}`)
      else toast.success(`已刷新 ${res.success} 个账号的额度`)
    },
    onSettled: invalidate,
    meta: { error: '刷新额度失败' },
  })

  const validate = useMutation({
    mutationFn: (ids: number[]) => credentialsApi.validateExisting({ scope: 'selected', ids, force: true }),
    onSettled: invalidate,
    meta: { error: '体检失败' },
  })

  const batchUpdate = useMutation({
    mutationFn: (req: BatchUpdateCredentialsRequest) => credentialsApi.batchUpdate(req),
    onSuccess: (res) => {
      if (res.failed > 0) toast.warning(`已更新 ${res.success} 个，${res.failed} 个失败`)
      else toast.success(`已更新 ${res.success} 个账号`)
    },
    onSettled: invalidate,
    meta: { error: '批量修改失败' },
  })

  const remove = useMutation({
    mutationFn: (ids: number[]) => settleAll(ids, (id) => credentialsApi.remove(id)),
    onSettled: invalidate,
    meta: { error: '删除失败' },
  })

  const resetFailures = useMutation({
    mutationFn: (ids: number[]) => settleAll(ids, (id) => credentialsApi.resetFailures(id)),
    onSettled: invalidate,
    meta: { error: '重置失败' },
  })

  const refreshToken = useMutation({
    mutationFn: (id: number) => credentialsApi.refreshToken(id),
    onSettled: invalidate,
    meta: { success: 'Token 已刷新', error: '刷新 Token 失败' },
  })

  const exportCredentials = useMutation({
    mutationFn: async ({ format, ids }: { format: CredentialExportFormat; ids?: number[] }) => {
      const blob = await credentialsApi.export(format, ids)
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `kiro-credentials-${new Date().toISOString().replace(/[:.]/g, '-')}.${format === 'jsonl' ? 'jsonl' : 'json'}`
      a.click()
      URL.revokeObjectURL(url)
    },
    meta: { success: '导出已开始下载', error: '导出失败' },
  })

  return {
    setDisabled,
    refreshInfo,
    validate,
    batchUpdate,
    remove,
    resetFailures,
    refreshToken,
    exportCredentials,

    /**
     * 启用/禁用：可恢复操作，乐观更新并在提示中提供"撤销"，不再弹确认框。
     * 批量禁用超过 20 个时仍需确认，避免误操作大面积影响调度。
     */
    async toggleDisabled(ids: number[], disabled: boolean, { undoable = true }: { undoable?: boolean } = {}) {
      if (ids.length === 0) return
      if (disabled && ids.length > 20) {
        const ok = await confirm({
          title: `禁用 ${ids.length} 个账号？`,
          description: '禁用后这些账号不再参与调度，进行中的请求不受影响。可以随时重新启用。',
          confirmText: '禁用',
          destructive: true,
        })
        if (!ok) return
      }
      const res = await setDisabled.mutateAsync({ ids, disabled })
      const okIds = ids.filter((id) => !res.errors.some((e) => e.id === id))
      const verb = disabled ? '已禁用' : '已启用'
      if (res.failed === 0 && undoable && okIds.length) {
        toast.success(`${verb} ${okIds.length} 个账号`, {
          action: {
            label: '撤销',
            onClick: () => {
              void setDisabled.mutateAsync({ ids: okIds, disabled: !disabled }).then((r) => {
                if (r.failed) reportSettled(r, '撤销')
                else toast.success('已撤销')
              })
            },
          },
        })
      } else {
        reportSettled(res, verb)
      }
    },

    async confirmRemove(ids: number[], labels?: string) {
      if (ids.length === 0) return false
      const ok = await confirm({
        title: ids.length === 1 ? '删除该账号？' : `删除 ${ids.length} 个账号？`,
        description: `${labels ? `${labels}。` : ''}删除后凭据和运行态不可恢复，历史请求记录保留。建议先导出备份。`,
        confirmText: '删除',
        destructive: true,
        typeToConfirm: ids.length > 1 ? String(ids.length) : undefined,
      })
      if (!ok) return false
      const res = await remove.mutateAsync(ids)
      reportSettled(res, '已删除')
      return res.failed === 0
    },

    async resetAndCheck(ids: number[]) {
      const res = await resetFailures.mutateAsync(ids)
      reportSettled(res, '已重置失败计数')
      const report = await validate.mutateAsync(ids)
      toast.success(`体检完成：成功 ${report.success}，失败 ${report.failed}`)
      return report
    },
  }
}

interface SettledResult {
  success: number
  failed: number
  errors: Array<{ id: number; message: string }>
}

/** 逐个调用单账号接口，并发上限 8，避免"全选筛选结果"时瞬间打出上千个请求 */
async function settleAll(ids: number[], fn: (id: number) => Promise<unknown>): Promise<SettledResult> {
  const errors: SettledResult['errors'] = []
  let cursor = 0
  const worker = async () => {
    while (cursor < ids.length) {
      const id = ids[cursor++]!
      try {
        await fn(id)
      } catch (e) {
        errors.push({ id, message: (e as Error)?.message ?? '失败' })
      }
    }
  }
  await Promise.all(Array.from({ length: Math.min(8, ids.length) }, worker))
  return { success: ids.length - errors.length, failed: errors.length, errors }
}

function reportSettled(res: SettledResult, verb: string) {
  if (res.failed === 0) toast.success(`${verb} ${res.success} 个账号`)
  else
    toast.warning(`${verb} ${res.success} 个，${res.failed} 个失败`, {
      description: res.errors
        .slice(0, 3)
        .map((e) => `#${e.id}: ${e.message}`)
        .join('\n'),
    })
}
