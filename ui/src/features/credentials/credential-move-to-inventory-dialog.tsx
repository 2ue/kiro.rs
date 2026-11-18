import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { ArrowUpRight } from 'lucide-react'
import { toast } from 'sonner'
import { Badge, Button, Checkbox, Spinner } from '@/components/ui'
import { Callout, Field, ModalShell } from '@/components/patterns'
import { useMoveToInventory, useMoveToInventoryPreview } from '@/hooks/use-inventory'
import { extractErrorMessage } from '@/lib/utils'
import type { MoveToInventoryRequest, MoveToInventoryResponse } from '@/types/api'
import { InventoryGroupPicker } from '@/features/inventory/inventory-group-picker'
import { disabledReasonLabel } from './credential-utils'

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
 * 把已禁用账号移入库存：账号离开账号池，由库存保管和续期，需要时再推回。
 * `ids` 不传时处理全部已禁用账号，传了只处理其中已禁用的。
 */
export function CredentialMoveToInventoryModal({ open, ids, onClose, onDone }: {
  open: boolean
  ids?: number[]
  onClose: () => void
  onDone?: () => void
}) {
  const [excludeAbnormal, setExcludeAbnormal] = useState(true)
  const [keepProxy, setKeepProxy] = useState(false)
  const [check, setCheck] = useState(false)
  const [group, setGroup] = useState('')
  const [result, setResult] = useState<MoveToInventoryResponse | null>(null)
  const move = useMoveToInventory()

  useEffect(() => {
    if (!open) {
      setExcludeAbnormal(true); setKeepProxy(false); setCheck(false); setGroup(''); setResult(null)
    }
  }, [open])

  const request = useMemo<MoveToInventoryRequest>(() => ({
    ids: ids && ids.length > 0 ? ids : undefined,
    excludeAbnormal,
    keepProxy,
    group: group.trim() || undefined,
    check,
  }), [ids, excludeAbnormal, keepProxy, group, check])
  const preview = useMoveToInventoryPreview(request, open && !result)
  const data = preview.data
  const scoped = Boolean(request.ids)

  const submit = async () => {
    try {
      const response = await move.mutateAsync(request)
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
      title={scoped ? '选中的禁用账号移入库存' : '禁用账号入库'}
      description="移入后账号离开账号池、不再参与调度，由库存保管并自动续期；需要时在库存页推回账号池。"
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
          <Button onClick={() => void submit()} disabled={move.isPending || !data || data.moving === 0}>
            {move.isPending && <Spinner size="sm" />}
            {move.isPending ? '移入中…' : data ? `移入 ${data.moving} 个账号` : '移入'}
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
            {result.excluded > 0 && <Badge>排除异常 {result.excluded}</Badge>}
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
              <Callout tone={data.moving > 0 ? 'info' : 'warning'}>
                {data.disabled === 0
                  ? '没有已禁用的账号。'
                  : `共 ${data.disabled} 个已禁用账号，将移入 ${data.moving} 个${data.excluded ? `，排除异常 ${data.excluded} 个` : ''}。`}
                {scoped && data.notDisabled > 0 && ` 选中项里有 ${data.notDisabled} 个未禁用，不会处理。`}
              </Callout>
              {data.reasons.length > 0 && (
                <div className="divide-y rounded-lg border">
                  {data.reasons.map((reason) => (
                    <div key={reason.reason ?? '__none__'} className="flex items-center justify-between gap-3 px-3 py-2 text-xs">
                      <span className="font-medium">{reason.reason ? disabledReasonLabel(reason.reason) : '未记录原因'}</span>
                      <span className="flex items-center gap-2 tabular-nums">
                        <span className="text-muted-foreground">{reason.count} 个</span>
                        {reason.moving === reason.count ? (
                          <Badge tone="success">移入</Badge>
                        ) : reason.moving === 0 ? (
                          <Badge>排除</Badge>
                        ) : (
                          <Badge tone="warning">移入 {reason.moving} · 排除 {reason.count - reason.moving}</Badge>
                        )}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}

          <div className="grid gap-3 sm:grid-cols-2">
            <OptionCard
              checked={excludeAbnormal}
              onChange={setExcludeAbnormal}
              disabled={move.isPending}
              title="排除异常账号"
              desc="和账号页“账号异常”的判定一致：refreshToken 失效、配置无效、风控、封禁、锁定或认证持续失败。额度耗尽和手动禁用不算异常。"
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
              desc="默认不查询，直接沿用账号池最近一次的额度和订阅快照；勾选后逐个刷新 token 并查询，账号多时较慢。"
            />
            <Field label="分组" description="可选；选已有分组或新建一个">
              <InventoryGroupPicker value={group} groups={data?.groups ?? []} onChange={setGroup} disabled={move.isPending} className="w-full" />
            </Field>
          </div>
          <p className="text-xs text-muted-foreground">
            优先级、并发、RPM、模型列表等调度参数不带入库存，推回账号池时在推送弹窗里重新设置。曾从库存推出去的账号会回到原来那条库存记录。
          </p>
        </div>
      )}
    </ModalShell>
  )
}
