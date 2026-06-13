import { useState } from 'react'
import { Eye, EyeOff } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import type { AddCredentialRequest, ProxyResource } from '@/types/api'

export interface CredentialParameterDefaults {
  priority: string
  maxConcurrentRequests: string
  region: string
  authRegion: string
  apiRegion: string
  machineId: string
  endpoint: string
  proxyResourceId: string
  proxyUrl: string
  proxyUsername: string
  proxyPassword: string
}

export function initialParameterDefaults(): CredentialParameterDefaults {
  return {
    priority: '',
    maxConcurrentRequests: '',
    region: '',
    authRegion: '',
    apiRegion: '',
    machineId: '',
    endpoint: '',
    proxyResourceId: '',
    proxyUrl: '',
    proxyUsername: '',
    proxyPassword: '',
  }
}

export function optionalTrimmed(value?: string | null) {
  const trimmed = value?.trim()
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
  defaults: CredentialParameterDefaults
): AddCredentialRequest {
  const defaultProxyResourceId = parseOptionalNonNegativeInteger(defaults.proxyResourceId, '代理资源 ID')
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
    priority: credential.priority ?? parseOptionalNonNegativeInteger(defaults.priority, '默认优先级'),
    maxConcurrentRequests: typeof credential.maxConcurrentRequests === 'undefined'
      ? parseOptionalNonNegativeInteger(defaults.maxConcurrentRequests, '默认账号并发')
      : credential.maxConcurrentRequests,
    region: optionalTrimmed(credential.region) || optionalTrimmed(defaults.region),
    authRegion: optionalTrimmed(credential.authRegion) || optionalTrimmed(defaults.authRegion),
    apiRegion: optionalTrimmed(credential.apiRegion) || optionalTrimmed(defaults.apiRegion),
    machineId: optionalTrimmed(credential.machineId) || optionalTrimmed(defaults.machineId),
    endpoint: optionalTrimmed(credential.endpoint) || optionalTrimmed(defaults.endpoint),
    proxyResourceId,
    proxyUrl: optionalTrimmed(credential.proxyUrl) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyUrl)),
    proxyUsername: optionalTrimmed(credential.proxyUsername) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyUsername)),
    proxyPassword: optionalTrimmed(credential.proxyPassword) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyPassword)),
  }
}

function clearDirectProxyDraft<T extends { proxyUrl: string; proxyUsername: string; proxyPassword: string }>(values: T): T {
  return { ...values, proxyUrl: '', proxyUsername: '', proxyPassword: '' }
}

function clearProxyResourceDraft<T extends { proxyResourceId: string }>(values: T): T {
  return { ...values, proxyResourceId: '' }
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

  const update = (key: keyof CredentialParameterDefaults, value: string) => {
    if (key === 'proxyResourceId' && value) {
      onChange(clearDirectProxyDraft({ ...defaults, proxyResourceId: value }))
      return
    }
    if ((key === 'proxyUrl' || key === 'proxyUsername' || key === 'proxyPassword') && value.trim()) {
      onChange(clearProxyResourceDraft({ ...defaults, [key]: value }))
      return
    }
    if (key === 'region' && value.trim() && !defaults.authRegion.trim()) {
      onChange({ ...defaults, region: value, authRegion: value })
      return
    }
    onChange({ ...defaults, [key]: value })
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
        <FieldLabel title="默认优先级" description="留空时使用凭据自身值或 0">
          <Input type="number" min="0" value={defaults.priority} disabled={disabled} onChange={(event) => update('priority', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="默认账号并发" description="留空继承全局，0 表示不限">
          <Input type="number" min="0" value={defaults.maxConcurrentRequests} disabled={disabled} onChange={(event) => update('maxConcurrentRequests', event.target.value)} />
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
                {resource.name}
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
      </div>
    </div>
  )
}
