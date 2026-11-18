import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { ArrowUpRight } from 'lucide-react'
import { toast } from 'sonner'
import { Badge, Button, Checkbox, Spinner } from '@/components/ui'
import { Callout, Field, ModalShell } from '@/components/patterns'
import { useMoveToInventory, useMoveToInventoryPreview } from '@/hooks/use-inventory'
import { extractErrorMessage } from '@/lib/utils'
import type { MovePreviewReason, MoveToInventoryRequest, MoveToInventoryResponse } from '@/types/api'
import { InventoryGroupPicker } from '@/features/inventory/inventory-group-picker'
import { disabledReasonLabel } from './credential-utils'

/** 后端用这个值表示没有记录禁用原因。 */
const NO_REASON = '__none__'

function reasonKey(reason: MovePreviewReason): string {
  return reason.reason || NO_REASON
}

function OptionCard({ checked, onChange, title, desc, disabled }: {
  checked: boolean
  onChange: (value: boolean) => void
  title: string
  desc: string
  disabled?: boolean
}) {
  return (
    <label className="flex items-start gap-2 rounded-lg border px-3 py-2.5">
      <Checkbox checked={checked} onCheckedChange={(value) => onChange(value === true)} disabled={disabled} className="mt-0.5" />
      <span>
        <span className="block text-[13px] font-medium">{title}</span>
        <span className="block text-xs text-muted-foreground">{desc}</span>
      </span>
    </label>
  )
}

/**
 * 手动把已禁用账号移入库存：账号离开账号池，由库存保管并自动续期，需要时再推回。
 * `ids` 不传时处理全部已禁用账号，传了只处理其中已禁用的。按禁用原因逐项勾选要移的账号。
 */
export function CredentialMoveToInventoryModal({ open, ids, onClose, onDone }: {
  open: boolean
  ids?: number[]
  onClose: () => void
  onDone?: () => void
}) {
  const [selectedReasons, setSelectedReasons] = useState<Set<string> | null>(null)
  const [excludeAbnormal, setExcludeAbnormal] = useState(true)
  const [keepProxy, setKeepProxy] = useState(false)
  const [check, setCheck] = useState(false)
  const [group, setGroup] = useState('')
  const [result, setResult] = useState<MoveToInventoryResponse | null>(null)
  const move = useMoveToInventory()

  useEffect(() => {
    if (!open) {
      setSelectedReasons(null); setExcludeAbnormal(true); setKeepProxy(false); setCheck(false); setGroup(''); setResult(null)
    }
  }, [open])

  const scopedIds = ids && ids.length > 0 ? ids : undefined
  // 预览按当前勾选计算，和实际移入用同一套判断（含账号异常、与库存或同批账号重复）。
  const previewReasons = useMemo(() => (selectedReasons ? Array.from(selectedReasons).sort() : undefined), [selectedReasons])
  const previewRequest = useMemo<MoveToInventoryRequest>(() => ({
    ids: scopedIds,
    reasons: previewReasons,
    excludeAbnormal,
    keepProxy: false,
    check: false,
  }), [scopedIds, previewReasons, excludeAbnormal])
  const preview = useMoveToInventoryPreview(previewRequest, open && !result)
  const data = preview.data
  const reasons = useMemo(() => data?.reasons ?? [], [data])

  // 首次拿到预览时按建议勾选：额度耗尽、手动禁用、无原因；其余由操作人决定。
  useEffect(() => {
    if (open && data && selectedReasons === null) {
      setSelectedReasons(new Set(data.reasons.filter((reason) => reason.suggested).map(reasonKey)))
    }
  }, [open, data, selectedReasons])

  const selected = selectedReasons ?? new Set<string>()
  // 首次预览还没按建议勾选时（全选口径），先不显示数量。
  const settled = selectedReasons !== null && !preview.isFetching
  const moving = selectedReasons === null ? 0 : (data?.moving ?? 0)
  const duplicates = selectedReasons === null ? 0 : (data?.duplicates ?? 0)
  const disabledTotal = data?.disabled ?? 0
  const toggleReason = (key: string, value: boolean) => setSelectedReasons((current) => {
    const next = new Set(current ?? [])
    if (value) next.add(key)
    else next.delete(key)
    return next
  })

  const submit = async () => {
    try {
      const response = await move.mutateAsync({
        ids: scopedIds,
        reasons: Array.from(selected),
        excludeAbnormal,
        keepProxy,
        group: group.trim() || undefined,
        check,
      })
      setResult(response)
      if (response.failed === 0 && response.skipped === 0) toast.success(`已移入库存 ${response.moved} 个账号`)
      else toast.warning(`移入 ${response.moved}，跳过 ${response.skipped}，失败 ${response.failed}`)
      onDone?.()
    } catch (e) {
      toast.error(`移入失败：${extractErrorMessage(e)}`)
    }
  }

  const problems = result?.items.filter((item) => !item.ok || item.message) ?? []

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={scopedIds ? '选中的禁用账号移入库存' : '禁用账号入库'}
      description="手动操作：确认后，勾选的账号离开账号池、不再参与调度，由库存保管并自动续期；原来的状态和调度参数一并记录，需要时在库存页推回账号池。"
      width="max-w-2xl"
      footer={result ? (
        <>
          <Button variant="outline" asChild>
            <Link to="/inventory">去库存查看<ArrowUpRight /></Link>
          </Button>
          <Button onClick={onClose}>完成</Button>
        </>
      ) : (
        <>
          <Button variant="outline" onClick={onClose} disabled={move.isPending}>取消</Button>
          <Button onClick={() => void submit()} disabled={move.isPending || !settled || moving === 0}>
            {move.isPending && <Spinner size="sm" />}
            {move.isPending ? '移入中…' : `移入 ${moving} 个账号`}
          </Button>
        </>
      )}
    >
      {result ? (
        <div className="space-y-3 text-sm">
          <div className="flex flex-wrap gap-2">
            <Badge tone="success">移入 {result.moved}</Badge>
            <Badge tone="warning">跳过 {result.skipped}</Badge>
            <Badge tone="error">失败 {result.failed}</Badge>
            {result.excluded > 0 && <Badge>未选 / 排除 {result.excluded}</Badge>}
          </div>
          {problems.length > 0 ? (
            <div className="max-h-[50vh] divide-y overflow-y-auto rounded-lg border">
              {problems.map((item) => (
                <div key={item.credentialId} className="flex items-start justify-between gap-3 px-3 py-2 text-xs">
                  <span className="min-w-0 truncate">
                    #{item.credentialId} {item.email || ''}{item.inventoryId ? ` → 库存 #${item.inventoryId}` : ''}
                  </span>
                  <span className={item.ok ? 'text-warning' : item.skipped ? 'text-muted-foreground' : 'text-destructive'}>
                    {item.message || (item.ok ? '已移入' : '失败')}
                  </span>
                </div>
              ))}
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">全部移入成功。</p>
          )}
        </div>
      ) : (
        <div className="space-y-4 text-sm">
          {preview.isLoading && !data ? (
            <div className="flex justify-center py-6"><Spinner /></div>
          ) : preview.error ? (
            <Callout tone="error">{extractErrorMessage(preview.error)}</Callout>
          ) : data && (
            <>
              <Callout tone={moving > 0 ? 'info' : 'warning'}>
                {disabledTotal === 0
                  ? '没有已禁用的账号。'
                  : `共 ${disabledTotal} 个已禁用账号，按当前勾选将移入 ${moving} 个，其余 ${disabledTotal - moving} 个留在账号池。`}
                {duplicates > 0 && ` 其中 ${duplicates} 个勾选了但与库存里已有的账号、或同一批里前面的账号是同一账号（同邮箱或同密钥），每个账号只移入一份，这些会跳过并留在账号池。`}
                {scopedIds && data.notDisabled > 0 && ` 选中项里有 ${data.notDisabled} 个未禁用，不会处理。`}
              </Callout>
              {reasons.length > 0 && (
                <Field label="按禁用原因选择要移入的账号" description="默认只勾选额度耗尽、手动禁用和未记录原因的账号；refreshToken 失效、封禁、连续失败等由你决定。">
                  <div className="divide-y rounded-lg border">
                    {reasons.map((reason) => {
                      const key = reasonKey(reason)
                      const checked = selected.has(key)
                      const count = reason.moving
                      return (
                        <label key={key} className="flex cursor-pointer items-center justify-between gap-3 px-3 py-2 text-xs">
                          <span className="flex items-center gap-2">
                            <Checkbox checked={checked} onCheckedChange={(value) => toggleReason(key, value === true)} disabled={move.isPending} />
                            <span className="font-medium">{reason.reason ? disabledReasonLabel(reason.reason) : '未记录原因'}</span>
                            {!reason.suggested && <Badge tone="warning">需确认</Badge>}
                          </span>
                          <span className="flex items-center gap-2 tabular-nums text-muted-foreground">
                            <span>{reason.count} 个</span>
                            {checked && excludeAbnormal && reason.abnormal > 0 && <span>（{reason.abnormal} 个账号异常不移入）</span>}
                            {checked && reason.duplicates > 0 && <span>（{reason.duplicates} 个重复跳过）</span>}
                            {checked ? <Badge tone="success">移入 {count}</Badge> : <Badge>不移入</Badge>}
                          </span>
                        </label>
                      )
                    })}
                  </div>
                </Field>
              )}
            </>
          )}

          <div className="grid gap-3 sm:grid-cols-2">
            <OptionCard
              checked={excludeAbnormal}
              onChange={setExcludeAbnormal}
              disabled={move.isPending}
              title="另外排除账号异常的账号"
              desc="即使勾选了上面的原因，账号页判定为“账号异常”的（refreshToken 失效、配置无效、风控、封禁、锁定或认证持续失败）也不移入。"
            />
            <OptionCard
              checked={keepProxy}
              onChange={setKeepProxy}
              disabled={move.isPending}
              title="保留代理"
              desc="默认不保留：去掉账号上的代理地址和代理资源绑定，推回账号池时再分配。"
            />
            <OptionCard
              checked={check}
              onChange={setCheck}
              disabled={move.isPending}
              title="入库后立即查询额度"
              desc="默认不查询，直接沿用账号池最近一次的额度和订阅快照；勾选后逐个刷新 token 并查询，会占用与账号池共用的刷新额度。"
            />
            <Field label="分组" description="可选；选已有分组或新建一个">
              <InventoryGroupPicker value={group} groups={data?.groups ?? []} onChange={setGroup} disabled={move.isPending} className="w-full" />
            </Field>
          </div>
          <p className="text-xs text-muted-foreground">
            每个账号移入时会记录原状态：禁用原因、健康状态、订阅和额度、优先级、RPM、并发、模型列表、失败次数和最后错误，库存卡片上可以看到；推回账号池时默认恢复原来的调度参数。曾从库存推出去的账号会回到原来那条库存记录。
          </p>
        </div>
      )}
    </ModalShell>
  )
}
