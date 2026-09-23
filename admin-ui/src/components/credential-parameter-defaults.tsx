import { useState } from 'react'
import { Eye, EyeOff } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import type { AddCredentialRequest, ProxyResource } from '@/types/api'

export type ProxyAssignmentMode = 'single' | 'round_robin' | 'none'

export interface CredentialParameterDefaults {
  disabled: string
  priority: string
  maxConcurrentRequests: string
  rpm: string
  region: string
  authRegion: string
  apiRegion: string
  machineId: string
  endpoint: string
  proxyMode: ProxyAssignmentMode
  proxyResourceId: string
  proxyResourceIds: string[]
  proxyUrl: string
  proxyUsername: string
  proxyPassword: string
  enableOverageAfterImport: boolean
}

export function initialParameterDefaults(): CredentialParameterDefaults {
  return {
    disabled: 'false',
    priority: '',
    maxConcurrentRequests: '',
    rpm: '',
    region: '',
    authRegion: '',
    apiRegion: '',
    machineId: '',
    endpoint: '',
    proxyMode: 'single',
    proxyResourceId: '',
    proxyResourceIds: [],
    proxyUrl: '',
    proxyUsername: '',
    proxyPassword: '',
    enableOverageAfterImport: false,
  }
}

export function optionalTrimmed(value: unknown) {
  const trimmed =
    typeof value === 'string'
      ? value.trim()
      : typeof value === 'number' && Number.isFinite(value)
        ? String(Math.trunc(value))
        : ''
  return trimmed ? trimmed : undefined
}

export function parseOptionalNonNegativeInteger(value: string, label: string): number | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const parsed = Number(trimmed)
  if (!Number.isInteger(parsed) || parsed < 0) throw new Error(`${label}必须是非负整数`)
  return parsed
}

export function mergeCredentialDefaults(
  credential: AddCredentialRequest,
  defaults: CredentialParameterDefaults,
  importIndex = 0,
): AddCredentialRequest {
  const roundRobinProxyResourceId = defaults.proxyResourceIds.length > 0
    ? parseOptionalNonNegativeInteger(
      defaults.proxyResourceIds[importIndex % defaults.proxyResourceIds.length],
      '代理资源 ID',
    )
    : undefined
  const defaultProxyResourceId = roundRobinProxyResourceId !== undefined
    ? roundRobinProxyResourceId
    : parseOptionalNonNegativeInteger(defaults.proxyResourceId, '代理资源 ID')
  const credentialHasDirectProxy = Boolean(
    optionalTrimmed(credential.proxyUrl) ||
    optionalTrimmed(credential.proxyUsername) ||
    optionalTrimmed(credential.proxyPassword)
  )
  const proxyResourceId =
    typeof credential.proxyResourceId !== 'undefined'
      ? credential.proxyResourceId
      : credentialHasDirectProxy
        ? undefined
        : defaultProxyResourceId
  const useProxyResource = typeof proxyResourceId === 'number'
  return {
    ...credential,
    disabled: typeof credential.disabled === 'undefined' || credential.disabled === null
      ? defaults.disabled === 'true'
      : credential.disabled,
    priority: credential.priority ?? parseOptionalNonNegativeInteger(defaults.priority, '默认优先级'),
    maxConcurrentRequests: typeof credential.maxConcurrentRequests === 'undefined'
      ? parseOptionalNonNegativeInteger(defaults.maxConcurrentRequests, '默认账号并发')
      : credential.maxConcurrentRequests,
    rpm: typeof credential.rpm === 'undefined'
      ? parseOptionalNonNegativeInteger(defaults.rpm, '默认账号 RPM')
      : credential.rpm,
    region: optionalTrimmed(credential.region) || optionalTrimmed(defaults.region),
    authRegion: optionalTrimmed(credential.authRegion) || optionalTrimmed(defaults.authRegion),
    apiRegion: optionalTrimmed(credential.apiRegion) || optionalTrimmed(defaults.apiRegion),
    machineId: optionalTrimmed(credential.machineId) || optionalTrimmed(defaults.machineId),
    endpoint: optionalTrimmed(credential.endpoint) || optionalTrimmed(defaults.endpoint),
    enableOverageAfterImport:
      typeof credential.enableOverageAfterImport === 'undefined' || credential.enableOverageAfterImport === null
        ? defaults.enableOverageAfterImport
        : credential.enableOverageAfterImport,
    proxyResourceId,
    proxyUrl: optionalTrimmed(credential.proxyUrl) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyUrl)),
    proxyUsername: optionalTrimmed(credential.proxyUsername) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyUsername)),
    proxyPassword: optionalTrimmed(credential.proxyPassword) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyPassword)),
  }
}

export function validateProxyAssignment(defaults: CredentialParameterDefaults): string | undefined {
  if (defaults.proxyMode === 'round_robin' && defaults.proxyResourceIds.length === 0) {
    return '已选择多个代理轮换，请至少选择一个代理资源'
  }
  return undefined
}

function FieldLabel({
  title,
  description,
  children,
}: {
  title: string
  description?: string
  children: React.ReactNode
}) {
  return (
    <label className="block space-y-1.5">
      <span className="text-sm font-medium">{title}</span>
      {children}
      {description && <span className="block text-xs leading-5 text-muted-foreground">{description}</span>}
    </label>
  )
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

export function CredentialParameterDefaultsPanel({
  defaults,
  onChange,
  proxyResources,
  disabled,
  title = '默认参数',
}: {
  defaults: CredentialParameterDefaults
  onChange: (defaults: CredentialParameterDefaults) => void
  proxyResources: ProxyResource[]
  disabled?: boolean
  title?: string
}) {
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)

  type StringDefaultKey = Exclude<keyof CredentialParameterDefaults, 'enableOverageAfterImport' | 'proxyMode' | 'proxyResourceIds'>

  const update = (key: StringDefaultKey, value: string) => {
    if (key === 'proxyResourceId' && value) {
      onChange({
        ...defaults,
        proxyMode: 'single',
        proxyResourceId: value,
        proxyResourceIds: [],
        proxyUrl: '',
        proxyUsername: '',
        proxyPassword: '',
      })
      return
    }
    if ((key === 'proxyUrl' || key === 'proxyUsername' || key === 'proxyPassword') && value.trim()) {
      onChange({
        ...defaults,
        proxyMode: 'single',
        [key]: value,
        proxyResourceId: '',
        proxyResourceIds: [],
      })
      return
    }
    if (key === 'region' && value.trim() && !defaults.authRegion.trim()) {
      onChange({ ...defaults, region: value, authRegion: value })
      return
    }
    onChange({ ...defaults, [key]: value })
  }

  const setProxyMode = (proxyMode: ProxyAssignmentMode) => {
    if (proxyMode === 'none') {
      onChange({
        ...defaults,
        proxyMode,
        proxyResourceId: '',
        proxyResourceIds: [],
        proxyUrl: '',
        proxyUsername: '',
        proxyPassword: '',
      })
      return
    }
    if (proxyMode === 'round_robin') {
      onChange({
        ...defaults,
        proxyMode,
        proxyResourceId: '',
        proxyUrl: '',
        proxyUsername: '',
        proxyPassword: '',
      })
      return
    }
    onChange({ ...defaults, proxyMode, proxyResourceIds: [] })
  }

  const toggleProxyResource = (id: number, checked: boolean | 'indeterminate') => {
    const idValue = String(id)
    const selected = new Set(defaults.proxyResourceIds)
    if (checked === true) selected.add(idValue)
    else selected.delete(idValue)
    onChange({
      ...defaults,
      proxyMode: 'round_robin',
      proxyResourceId: '',
      proxyResourceIds: Array.from(selected),
    })
  }

  const proxyLocked = Boolean(defaults.proxyResourceId)

  return (
    <div className="rounded-md border bg-muted/20 p-3">
      <div className="mb-3 flex items-start justify-between gap-3">
        <div>
          <div className="text-sm font-semibold">{title}</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">
            只填充每条凭据里缺失的字段；导入 JSON 中已有字段会保留。
          </div>
        </div>
        <Button type="button" variant="ghost" size="sm" disabled={disabled} onClick={() => onChange(initialParameterDefaults())}>
          清空
        </Button>
      </div>

      <div className="grid gap-3 md:grid-cols-2">
        <FieldLabel title="导入后状态" description="导入 JSON 中已有 disabled 字段时优先使用账号自身值">
          <select
            value={defaults.disabled}
            disabled={disabled}
            onChange={(event) => update('disabled', event.target.value)}
            className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <option value="false">启用</option>
            <option value="true">禁用</option>
          </select>
        </FieldLabel>
        <div className="flex items-center gap-2 rounded-md bg-background/60 px-3 py-2">
          <Checkbox
            id="default-enable-overage"
            checked={defaults.enableOverageAfterImport}
            disabled={disabled}
            onCheckedChange={(checked) => onChange({ ...defaults, enableOverageAfterImport: checked === true })}
          />
          <label htmlFor="default-enable-overage" className="cursor-pointer text-sm">
            导入后尝试开启超额
          </label>
        </div>
        <FieldLabel title="默认优先级" description="留空时使用凭据自身值或 0">
          <Input type="number" min="0" value={defaults.priority} disabled={disabled} onChange={(event) => update('priority', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="默认账号并发" description="留空继承全局，0 表示不限">
          <Input type="number" min="0" value={defaults.maxConcurrentRequests} disabled={disabled} onChange={(event) => update('maxConcurrentRequests', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="默认账号 RPM" description="留空继承全局，0 表示不限">
          <Input type="number" min="0" value={defaults.rpm} disabled={disabled} onChange={(event) => update('rpm', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="Region 兼容字段" description="未设置 Auth Region 时自动同步到 Auth Region">
          <Input className="font-mono" value={defaults.region} disabled={disabled} onChange={(event) => update('region', event.target.value)} placeholder="us-east-1" />
        </FieldLabel>
        <FieldLabel title="Auth Region" description="Token 刷新区域">
          <Input className="font-mono" value={defaults.authRegion} disabled={disabled} onChange={(event) => update('authRegion', event.target.value)} placeholder="us-east-1" />
        </FieldLabel>
        <FieldLabel title="API Region" description="API 请求区域">
          <Input className="font-mono" value={defaults.apiRegion} disabled={disabled} onChange={(event) => update('apiRegion', event.target.value)} placeholder="us-east-1" />
        </FieldLabel>
        <FieldLabel title="Machine ID" description="留空使用全局配置或自动派生">
          <Input value={defaults.machineId} disabled={disabled} onChange={(event) => update('machineId', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="端点" description="留空使用全局 defaultEndpoint">
          <Input value={defaults.endpoint} disabled={disabled} onChange={(event) => update('endpoint', event.target.value)} placeholder="ide / cli" />
        </FieldLabel>
        <FieldLabel title="代理分配方式" description="轮换按导入顺序循环分配；账号自身代理配置优先">
          <select
            value={defaults.proxyMode}
            disabled={disabled}
            onChange={(event) => setProxyMode(event.target.value as ProxyAssignmentMode)}
            className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <option value="single">统一代理</option>
            <option value="round_robin">多个代理轮换</option>
            <option value="none">不设置代理</option>
          </select>
        </FieldLabel>
        {defaults.proxyMode === 'round_robin' && (
          <div className="md:col-span-2 space-y-1.5">
            <div className="text-sm font-medium">轮换代理资源</div>
            {proxyResources.length > 0 ? (
              <div className="grid max-h-40 gap-2 overflow-y-auto rounded-md border bg-background/60 p-2 sm:grid-cols-2">
                {proxyResources.map((resource) => (
                  <label key={resource.id} className="flex min-w-0 items-center gap-2 rounded-md px-2 py-1.5 hover:bg-muted/60">
                    <Checkbox
                      checked={defaults.proxyResourceIds.includes(String(resource.id))}
                      onCheckedChange={(checked) => toggleProxyResource(resource.id, checked)}
                      disabled={disabled}
                    />
                    <span className="min-w-0 flex-1 truncate text-sm">{resource.name}</span>
                    {!resource.enabled && <span className="shrink-0 text-[0.7rem] text-muted-foreground">已禁用</span>}
                  </label>
                ))}
              </div>
            ) : (
              <div className="rounded-md border border-dashed px-3 py-2 text-xs text-muted-foreground">暂无可用代理资源</div>
            )}
            <p className="text-xs leading-5 text-muted-foreground">
              已选 {defaults.proxyResourceIds.length} 个，按选择顺序循环分配给账号 1、2、3…。
            </p>
          </div>
        )}
        {defaults.proxyMode === 'single' && (
          <>
            <FieldLabel title="代理资源" description="选择资源会清空直连代理；填写直连代理会取消资源">
              <select
                value={defaults.proxyResourceId}
                disabled={disabled}
                onChange={(event) => update('proxyResourceId', event.target.value)}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="">不绑定</option>
                {proxyResources.map((resource) => (
                  <option key={resource.id} value={resource.id}>
                    {resource.name}{resource.enabled ? '' : '（已禁用）'}
                  </option>
                ))}
              </select>
            </FieldLabel>
            <FieldLabel title="独立代理 URL" description={proxyLocked ? '已选择代理资源，输入前请先取消资源' : '可填 direct 或完整代理 URL'}>
              <Input
                value={defaults.proxyUrl}
                disabled={disabled || proxyLocked}
                onChange={(event) => update('proxyUrl', event.target.value)}
                placeholder="socks5h://127.0.0.1:1080"
              />
            </FieldLabel>
            <FieldLabel title="代理用户名">
              <SecretInput
                value={defaults.proxyUsername}
                onChange={(value) => update('proxyUsername', value)}
                visible={showProxyUsername}
                onToggle={() => setShowProxyUsername((value) => !value)}
                disabled={disabled || proxyLocked}
                placeholder="可选"
              />
            </FieldLabel>
            <FieldLabel title="代理密码">
              <SecretInput
                value={defaults.proxyPassword}
                onChange={(value) => update('proxyPassword', value)}
                visible={showProxyPassword}
                onToggle={() => setShowProxyPassword((value) => !value)}
                disabled={disabled || proxyLocked}
                placeholder="可选"
              />
            </FieldLabel>
          </>
        )}
      </div>
    </div>
  )
}
