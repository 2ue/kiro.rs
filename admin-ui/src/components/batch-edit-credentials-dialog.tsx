import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Eye, EyeOff, Loader2 } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { useBatchUpdateCredentials, useProxyResources } from '@/hooks/use-credentials'
import {
  optionalTrimmed,
  parseOptionalNonNegativeInteger,
} from '@/components/credential-parameter-defaults'
import { extractErrorMessage } from '@/lib/utils'
import type { BatchUpdateCredentialsRequest } from '@/types/api'

interface BatchEditCredentialsDialogProps {
  open: boolean
  ids: number[]
  onOpenChange: (open: boolean) => void
  onDone: () => void
}

function optionalRegionUpdate(enabled: boolean, value: string): string | null | undefined {
  if (!enabled) return undefined
  const trimmed = value.trim()
  return trimmed ? trimmed : null
}

function SecretInput({
  value,
  onChange,
  visible,
  onToggle,
  disabled,
  placeholder,
}: {
  value: string
  onChange: (value: string) => void
  visible: boolean
  onToggle: () => void
  disabled?: boolean
  placeholder?: string
}) {
  return (
    <div className="relative">
      <Input
        className="pr-10"
        type={visible ? 'text' : 'password'}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="absolute right-1 top-1 h-8 w-8"
        onClick={onToggle}
        disabled={disabled}
        title={visible ? '隐藏' : '显示'}
      >
        {visible ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
      </Button>
    </div>
  )
}

function ToggleRow({
  checked,
  disabled,
  label,
  onCheckedChange,
}: {
  checked: boolean
  disabled?: boolean
  label: string
  onCheckedChange: (checked: boolean) => void
}) {
  return (
    <label className="flex w-fit cursor-pointer items-center gap-2 text-sm font-medium">
      <Checkbox
        checked={checked}
        disabled={disabled}
        onCheckedChange={(value) => onCheckedChange(value === true)}
      />
      {label}
    </label>
  )
}

export function BatchEditCredentialsDialog({
  open,
  ids,
  onOpenChange,
  onDone,
}: BatchEditCredentialsDialogProps) {
  const [updateRegions, setUpdateRegions] = useState(false)
  const [updateRegion, setUpdateRegion] = useState(false)
  const [updateAuthRegion, setUpdateAuthRegion] = useState(false)
  const [updateApiRegion, setUpdateApiRegion] = useState(false)
  const [regionValue, setRegionValue] = useState('')
  const [authRegionValue, setAuthRegionValue] = useState('')
  const [apiRegionValue, setApiRegionValue] = useState('')
  const [updateConcurrency, setUpdateConcurrency] = useState(false)
  const [concurrencyValue, setConcurrencyValue] = useState('')
  const [updateRpm, setUpdateRpm] = useState(false)
  const [rpmValue, setRpmValue] = useState('')
  const [updateRateLimitAutoDisable, setUpdateRateLimitAutoDisable] = useState(false)
  const [rateLimitAutoDisableEnabled, setRateLimitAutoDisableEnabled] = useState(true)
  const [updateProxy, setUpdateProxy] = useState(false)
  const [proxyResourceId, setProxyResourceId] = useState('')
  const [proxyUrl, setProxyUrl] = useState('')
  const [proxyUsername, setProxyUsername] = useState('')
  const [proxyPassword, setProxyPassword] = useState('')
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const [updatePriority, setUpdatePriority] = useState(false)
  const [priorityValue, setPriorityValue] = useState('')

  const batchUpdate = useBatchUpdateCredentials()
  const proxyResources = useProxyResources()
  const proxyResourceOptions = (proxyResources.data?.resources || []).filter(resource => resource.enabled)
  const proxyLocked = Boolean(proxyResourceId)

  const setRegionsEnabled = (enabled: boolean) => {
    setUpdateRegions(enabled)
    setUpdateAuthRegion(enabled)
    setUpdateApiRegion(enabled)
    if (!enabled) {
      setUpdateRegion(false)
      setRegionValue('')
      setAuthRegionValue('')
      setApiRegionValue('')
    }
  }

  const setProxyResourceDraft = (value: string) => {
    setProxyResourceId(value)
    if (value) {
      setProxyUrl('')
      setProxyUsername('')
      setProxyPassword('')
    }
  }

  const setDirectProxyDraft = (setter: (value: string) => void, value: string) => {
    setter(value)
    if (value.trim()) {
      setProxyResourceId('')
    }
  }

  useEffect(() => {
    if (open) return
    setUpdateRegions(false)
    setUpdateRegion(false)
    setUpdateAuthRegion(false)
    setUpdateApiRegion(false)
    setRegionValue('')
    setAuthRegionValue('')
    setApiRegionValue('')
    setUpdateConcurrency(false)
    setConcurrencyValue('')
    setUpdateRpm(false)
    setRpmValue('')
    setUpdateRateLimitAutoDisable(false)
    setRateLimitAutoDisableEnabled(true)
    setUpdateProxy(false)
    setProxyResourceId('')
    setProxyUrl('')
    setProxyUsername('')
    setProxyPassword('')
    setShowProxyUsername(false)
    setShowProxyPassword(false)
    setUpdatePriority(false)
    setPriorityValue('')
  }, [open])

  const submit = () => {
    if (ids.length === 0) {
      toast.error('请先选择要修改的账号')
      return
    }
    if (!updatePriority && !updateRegions && !updateConcurrency && !updateRpm && !updateRateLimitAutoDisable && !updateProxy) {
      toast.error('请选择至少一组要修改的参数')
      return
    }

    const request: BatchUpdateCredentialsRequest = { ids }
    if (updatePriority) {
      try {
        request.priority = {
          priority: priorityValue.trim()
            ? (parseOptionalNonNegativeInteger(priorityValue, '账号优先级') ?? 0)
            : 0,
        }
      } catch (error) {
        toast.error(extractErrorMessage(error))
        return
      }
    }

    if (updateRegions) {
      const regions = {
        region: optionalRegionUpdate(updateRegion, regionValue),
        authRegion: optionalRegionUpdate(updateAuthRegion, authRegionValue),
        apiRegion: optionalRegionUpdate(updateApiRegion, apiRegionValue),
      }
      if (
        typeof regions.region === 'undefined' &&
        typeof regions.authRegion === 'undefined' &&
        typeof regions.apiRegion === 'undefined'
      ) {
        toast.error('请选择至少一个 Region 字段')
        return
      }
      request.regions = regions
    }

    if (updateConcurrency) {
      try {
        request.concurrency = {
          maxConcurrentRequests: concurrencyValue.trim()
            ? parseOptionalNonNegativeInteger(concurrencyValue, '账号并发覆盖')
            : null,
        }
      } catch (error) {
        toast.error(extractErrorMessage(error))
        return
      }
    }

    if (updateRpm) {
      try {
        request.rpm = {
          rpm: rpmValue.trim()
            ? parseOptionalNonNegativeInteger(rpmValue, '账号 RPM 覆盖')
            : null,
        }
      } catch (error) {
        toast.error(extractErrorMessage(error))
        return
      }
    }

    if (updateRateLimitAutoDisable) {
      request.rateLimitAutoDisable = { enabled: rateLimitAutoDisableEnabled }
    }

    if (updateProxy) {
      const resourceId = proxyResourceId ? Number(proxyResourceId) : null
      request.proxy = {
        proxyResourceId: resourceId,
        proxyUrl: resourceId ? undefined : optionalTrimmed(proxyUrl),
        proxyUsername: resourceId ? undefined : optionalTrimmed(proxyUsername),
        proxyPassword: resourceId ? undefined : optionalTrimmed(proxyPassword),
      }
    }

    batchUpdate.mutate(request, {
      onSuccess: (response) => {
        if (response.failed === 0) {
          toast.success(`成功修改 ${response.success}/${response.total} 个账号`)
        } else {
          toast.warning(`批量修改完成：成功 ${response.success} 个，失败 ${response.failed} 个`)
        }
        onDone()
        onOpenChange(false)
      },
      onError: (error) => toast.error(`批量修改失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => { if (!batchUpdate.isPending) onOpenChange(nextOpen) }}>
      <DialogContent className="max-h-[85vh] max-w-3xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>批量修改 {ids.length} 个账号</DialogTitle>
          <DialogDescription>
            只会修改已勾选的参数组；Region 字段填空并勾选保存时会清空对应账号覆盖。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className={`rounded-md border p-3 ${updatePriority ? 'border-primary bg-primary/5' : 'bg-muted/20'}`}>
            <ToggleRow
              checked={updatePriority}
              disabled={batchUpdate.isPending}
              label="修改账号优先级"
              onCheckedChange={setUpdatePriority}
            />
            <label className="mt-3 block space-y-2">
              <span className="text-sm font-medium">账号优先级</span>
              <Input
                type="number"
                min="0"
                value={priorityValue}
                placeholder="留空保存为 0"
                disabled={!updatePriority || batchUpdate.isPending}
                onChange={(event) => setPriorityValue(event.target.value)}
              />
              <span className="block text-xs leading-5 text-muted-foreground">
                数值越大优先级越高；保存为 0 表示回到默认优先级。
              </span>
            </label>
          </div>

          <div className={`rounded-md border p-3 ${updateRegions ? 'border-primary bg-primary/5' : 'bg-muted/20'}`}>
            <ToggleRow
              checked={updateRegions}
              disabled={batchUpdate.isPending}
              label="修改 Region"
              onCheckedChange={setRegionsEnabled}
            />
            <div className="mt-3 grid gap-3 md:grid-cols-3">
              <label className="space-y-2">
                <ToggleRow
                  checked={updateRegion}
                  disabled={!updateRegions || batchUpdate.isPending}
                  label="Region 兼容字段"
                  onCheckedChange={setUpdateRegion}
                />
                <Input
                  className="font-mono"
                  value={regionValue}
                  placeholder="us-east-1"
                  disabled={!updateRegions || !updateRegion || batchUpdate.isPending}
                  onChange={(event) => setRegionValue(event.target.value)}
                />
              </label>
              <label className="space-y-2">
                <ToggleRow
                  checked={updateAuthRegion}
                  disabled={!updateRegions || batchUpdate.isPending}
                  label="Auth Region"
                  onCheckedChange={setUpdateAuthRegion}
                />
                <Input
                  className="font-mono"
                  value={authRegionValue}
                  placeholder="us-east-1"
                  disabled={!updateRegions || !updateAuthRegion || batchUpdate.isPending}
                  onChange={(event) => setAuthRegionValue(event.target.value)}
                />
              </label>
              <label className="space-y-2">
                <ToggleRow
                  checked={updateApiRegion}
                  disabled={!updateRegions || batchUpdate.isPending}
                  label="API Region"
                  onCheckedChange={setUpdateApiRegion}
                />
                <Input
                  className="font-mono"
                  value={apiRegionValue}
                  placeholder="us-east-1"
                  disabled={!updateRegions || !updateApiRegion || batchUpdate.isPending}
                  onChange={(event) => setApiRegionValue(event.target.value)}
                />
              </label>
            </div>
          </div>

          <div className={`rounded-md border p-3 ${updateConcurrency ? 'border-primary bg-primary/5' : 'bg-muted/20'}`}>
            <ToggleRow
              checked={updateConcurrency}
              disabled={batchUpdate.isPending}
              label="修改账号并发覆盖"
              onCheckedChange={setUpdateConcurrency}
            />
            <label className="mt-3 block space-y-2">
              <span className="text-sm font-medium">账号级最大并发</span>
              <Input
                type="number"
                min="0"
                value={concurrencyValue}
                placeholder="留空改为继承全局，0 表示不限"
                disabled={!updateConcurrency || batchUpdate.isPending}
                onChange={(event) => setConcurrencyValue(event.target.value)}
              />
            </label>
          </div>

          <div className={`rounded-md border p-3 ${updateRpm ? 'border-primary bg-primary/5' : 'bg-muted/20'}`}>
            <ToggleRow
              checked={updateRpm}
              disabled={batchUpdate.isPending}
              label="修改账号 RPM 覆盖"
              onCheckedChange={setUpdateRpm}
            />
            <label className="mt-3 block space-y-2">
              <span className="text-sm font-medium">账号级 RPM</span>
              <Input
                type="number"
                min="0"
                value={rpmValue}
                placeholder="留空改为继承全局，0 表示不限"
                disabled={!updateRpm || batchUpdate.isPending}
                onChange={(event) => setRpmValue(event.target.value)}
              />
            </label>
          </div>

          <div className={`rounded-md border p-3 ${updateRateLimitAutoDisable ? 'border-primary bg-primary/5' : 'bg-muted/20'}`}>
            <ToggleRow
              checked={updateRateLimitAutoDisable}
              disabled={batchUpdate.isPending}
              label="修改 429 自动禁用"
              onCheckedChange={setUpdateRateLimitAutoDisable}
            />
            <div className="mt-3">
              <ToggleRow
                checked={rateLimitAutoDisableEnabled}
                disabled={!updateRateLimitAutoDisable || batchUpdate.isPending}
                label="429 临时风控自动禁用账号"
                onCheckedChange={setRateLimitAutoDisableEnabled}
              />
            </div>
          </div>

          <div className={`rounded-md border p-3 ${updateProxy ? 'border-primary bg-primary/5' : 'bg-muted/20'}`}>
            <ToggleRow
              checked={updateProxy}
              disabled={batchUpdate.isPending}
              label="修改代理"
              onCheckedChange={setUpdateProxy}
            />
            <div className="mt-3 grid gap-3 md:grid-cols-2">
              <label className="block space-y-2">
                <span className="text-sm font-medium">代理资源</span>
                <select
                  value={proxyResourceId}
                  disabled={!updateProxy || batchUpdate.isPending}
                  onChange={(event) => setProxyResourceDraft(event.target.value)}
                  className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <option value="">不绑定</option>
                  {proxyResourceOptions.map((resource) => (
                    <option key={resource.id} value={resource.id}>
                      {resource.name}
                    </option>
                  ))}
                </select>
                <span className="block text-xs leading-5 text-muted-foreground">
                  选择资源会清空账号直连代理；不选且 URL 为空会清空账号级代理。
                </span>
              </label>
              <label className="block space-y-2">
                <span className="text-sm font-medium">独立代理 URL</span>
                <Input
                  value={proxyUrl}
                  disabled={!updateProxy || proxyLocked || batchUpdate.isPending}
                  placeholder="socks5h://127.0.0.1:1080"
                  onChange={(event) => setDirectProxyDraft(setProxyUrl, event.target.value)}
                />
                <span className="block text-xs leading-5 text-muted-foreground">
                  {proxyLocked ? '已选择代理资源，保存时会清空直连代理。' : '可填 direct 或完整代理 URL。'}
                </span>
              </label>
              <label className="block space-y-2">
                <span className="text-sm font-medium">代理用户名</span>
                <SecretInput
                  value={proxyUsername}
                  onChange={(value) => setDirectProxyDraft(setProxyUsername, value)}
                  visible={showProxyUsername}
                  onToggle={() => setShowProxyUsername((value) => !value)}
                  disabled={!updateProxy || proxyLocked || batchUpdate.isPending}
                  placeholder="可选"
                />
              </label>
              <label className="block space-y-2">
                <span className="text-sm font-medium">代理密码</span>
                <SecretInput
                  value={proxyPassword}
                  onChange={(value) => setDirectProxyDraft(setProxyPassword, value)}
                  visible={showProxyPassword}
                  onToggle={() => setShowProxyPassword((value) => !value)}
                  disabled={!updateProxy || proxyLocked || batchUpdate.isPending}
                  placeholder="可选"
                />
              </label>
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={batchUpdate.isPending}>
            取消
          </Button>
          <Button onClick={submit} disabled={batchUpdate.isPending || ids.length === 0}>
            {batchUpdate.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
            保存批量修改
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
