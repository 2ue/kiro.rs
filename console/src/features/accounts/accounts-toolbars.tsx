import { Ban, CirclePlay, Download, Pencil, RefreshCw, Stethoscope, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { BulkBar } from '@/components/data-table/selection'
import { useConfirm } from '@/components/patterns/confirm'
import { credentialsApi } from '@/api/endpoints/credentials'
import { DISABLED_REASONS, DISABLED_REASON_CODES } from '@/domain/disabled-reason'
import { useAccountActions } from './actions'
import { type AccountRow } from './queries'

/** 已禁用分段下的禁用原因筛选与按原因处置 */
export function DisabledTriage({
  reason,
  rows,
  disabledTotal,
  onReason,
  onDeletedAll,
}: {
  reason?: string
  rows: AccountRow[]
  disabledTotal?: number
  onReason: (reason: string | undefined) => void
  onDeletedAll: () => void
}) {
  const actions = useAccountActions()
  const confirm = useConfirm()
  const deleteByReason = async (reason?: string) => {
    if (!reason) {
      const ok = await confirm({
        title: `删除全部 ${disabledTotal ?? ''} 个已禁用账号？`,
        description: '会删除所有处于禁用状态的账号（包括手动禁用）。操作不可撤销，建议先导出备份。',
        confirmText: '全部删除',
        destructive: true,
        typeToConfirm: '删除',
      })
      if (!ok) return
      const res = await credentialsApi.removeDisabled()
      toast.success(`已删除 ${res.success} 个账号${res.failed ? `，${res.failed} 个失败` : ''}`)
      onDeletedAll()
      return
    }
    // 按原因删除：只作用于当前页筛选结果
    const ids = rows.map((r) => r.id)
    await actions.confirmRemove(
      ids,
      `原因为「${DISABLED_REASONS[reason as keyof typeof DISABLED_REASONS]?.label ?? reason}」的 ${ids.length} 个账号`,
    )
  }
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <span className="mr-1 text-xs text-muted-foreground">禁用原因</span>
      <Button size="xs" variant={!reason ? 'secondary' : 'ghost'} onClick={() => onReason(undefined)}>
        全部
      </Button>
      {DISABLED_REASON_CODES.map((code) => (
        <Button key={code} size="xs" variant={reason === code ? 'secondary' : 'ghost'} onClick={() => onReason(code)}>
          {DISABLED_REASONS[code].label}
        </Button>
      ))}
      <div className="ml-auto">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="xs" variant="outline">
              按原因处置
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-64">
            <DropdownMenuLabel>当前筛选（{rows.length} 个）</DropdownMenuLabel>
            {reason && DISABLED_REASONS[reason as keyof typeof DISABLED_REASONS] ? (
              <>
                <DropdownMenuItem
                  onSelect={() =>
                    actions.toggleDisabled(
                      rows.map((r) => r.id),
                      false,
                    )
                  }
                >
                  <CirclePlay /> 全部重新启用
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => actions.resetAndCheck(rows.map((r) => r.id))}>
                  <Stethoscope /> 重置失败计数并体检
                </DropdownMenuItem>
                <DropdownMenuItem variant="destructive" onSelect={() => deleteByReason(reason)}>
                  <Trash2 /> 删除当前页这些账号
                </DropdownMenuItem>
              </>
            ) : (
              <DropdownMenuItem variant="destructive" onSelect={() => deleteByReason()}>
                <Trash2 /> 删除全部已禁用账号
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <p className="px-2 py-1.5 text-xs text-muted-foreground">
              {reason ? DISABLED_REASONS[reason as keyof typeof DISABLED_REASONS]?.hint : '先选择一个禁用原因以执行对应处置'}
            </p>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  )
}

/** 选中账号后的批量操作栏 */
export function AccountsBulkBar({
  selectedIds,
  clearSelection,
  onEdit,
  extra,
}: {
  selectedIds: number[]
  clearSelection: () => void
  onEdit: () => void
  extra?: React.ReactNode
}) {
  const actions = useAccountActions()
  return (
    <BulkBar count={selectedIds.length} onClear={clearSelection} extra={extra}>
      <Button size="sm" variant="ghost" onClick={() => actions.toggleDisabled(selectedIds, false).then(clearSelection)}>
        <CirclePlay /> 启用
      </Button>
      <Button size="sm" variant="ghost" onClick={() => actions.toggleDisabled(selectedIds, true).then(clearSelection)}>
        <Ban /> 禁用
      </Button>
      <Button
        size="sm"
        variant="ghost"
        onClick={async () => {
          const res = await actions.validate.mutateAsync(selectedIds)
          toast.success(`体检完成：成功 ${res.success}，失败 ${res.failed}，升级 ${res.upgraded}，降级 ${res.downgraded}`)
        }}
        disabled={actions.validate.isPending}
      >
        <Stethoscope /> 体检
      </Button>
      <Button size="sm" variant="ghost" onClick={() => actions.refreshInfo.mutate(selectedIds)} disabled={actions.refreshInfo.isPending}>
        <RefreshCw /> 刷新额度
      </Button>
      <Button size="sm" variant="ghost" onClick={() => onEdit()}>
        <Pencil /> 修改
      </Button>
      <Button size="sm" variant="ghost" onClick={() => actions.exportCredentials.mutate({ format: 'backup-json', ids: selectedIds })}>
        <Download /> 导出
      </Button>
      <Button
        size="sm"
        variant="ghost"
        className="text-danger"
        onClick={async () => {
          if (await actions.confirmRemove(selectedIds)) clearSelection()
        }}
      >
        <Trash2 /> 删除
      </Button>
    </BulkBar>
  )
}
