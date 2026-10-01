import { useState } from 'react'
import type { BatchUpdateCredentialsRequest } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { NumberInput, SelectControl } from '@/components/patterns/fields'
import { Switch } from '@/components/ui/switch'
import { useProxies } from '@/queries/shared'
import { useAccountActions } from './actions'

type FieldKey = 'priority' | 'concurrency' | 'rpm' | 'rateLimitAutoDisable' | 'proxy'

/** 批量修改：勾选要修改的字段，未勾选的字段保持原值 */
export function BatchEditDialog({
  ids,
  open,
  onOpenChange,
  onDone,
}: {
  ids: number[]
  open: boolean
  onOpenChange: (v: boolean) => void
  onDone: () => void
}) {
  const actions = useAccountActions()
  const proxies = useProxies()
  const [enabled, setEnabled] = useState<Record<FieldKey, boolean>>({
    priority: false,
    concurrency: false,
    rpm: false,
    rateLimitAutoDisable: false,
    proxy: false,
  })
  const [priority, setPriority] = useState(0)
  const [concurrency, setConcurrency] = useState<number | null>(null)
  const [rpm, setRpm] = useState<number | null>(null)
  const [autoDisable, setAutoDisable] = useState(false)
  const [proxyId, setProxyId] = useState<string>('none')

  const toggle = (key: FieldKey) => (v: boolean | 'indeterminate') => setEnabled((e) => ({ ...e, [key]: v === true }))

  const submit = async () => {
    const req: BatchUpdateCredentialsRequest = { ids }
    if (enabled.priority) req.priority = { priority }
    if (enabled.concurrency) req.concurrency = { maxConcurrentRequests: concurrency }
    if (enabled.rpm) req.rpm = { rpm }
    if (enabled.rateLimitAutoDisable) req.rateLimitAutoDisable = { enabled: autoDisable }
    if (enabled.proxy) req.proxy = proxyId === 'none' ? { proxyResourceId: null, proxyUrl: '' } : { proxyResourceId: Number(proxyId) }
    await actions.batchUpdate.mutateAsync(req)
    onOpenChange(false)
    onDone()
  }

  const any = Object.values(enabled).some(Boolean)
  const rowProps = (k: FieldKey) => ({ checked: enabled[k], onToggle: toggle(k) })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>批量修改 {ids.length} 个账号</DialogTitle>
          <DialogDescription>只会修改勾选的字段</DialogDescription>
        </DialogHeader>
        <div className="divide-y">
          <Row {...rowProps('priority')} label="优先级">
            <NumberInput value={priority} min={0} onChange={(v) => setPriority(v ?? 0)} />
          </Row>
          <Row {...rowProps('concurrency')} label="最大并发">
            <NumberInput value={concurrency} min={0} allowEmpty placeholder="继承全局" onChange={setConcurrency} />
          </Row>
          <Row {...rowProps('rpm')} label="RPM 上限">
            <NumberInput value={rpm} min={0} allowEmpty placeholder="继承全局" onChange={setRpm} />
          </Row>
          <Row {...rowProps('rateLimitAutoDisable')} label="限流自动禁用">
            <Switch checked={autoDisable} onCheckedChange={setAutoDisable} />
          </Row>
          <Row {...rowProps('proxy')} label="代理资源">
            <SelectControl
              value={proxyId}
              onChange={setProxyId}
              options={[
                { value: 'none', label: '解除绑定（继承全局）' },
                ...(proxies.data?.resources ?? []).map((p) => ({ value: String(p.id), label: p.name })),
              ]}
            />
          </Row>
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button onClick={submit} disabled={!any || actions.batchUpdate.isPending}>
            应用
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function Row({
  checked,
  onToggle,
  label,
  children,
}: {
  checked: boolean
  onToggle: (v: boolean | 'indeterminate') => void
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="grid grid-cols-[auto_8rem_1fr] items-center gap-3 py-2">
      <Checkbox checked={checked} onCheckedChange={onToggle} aria-label={`修改${label}`} />
      <span className="text-sm">{label}</span>
      <div className={checked ? '' : 'pointer-events-none opacity-50'}>{children}</div>
    </div>
  )
}
