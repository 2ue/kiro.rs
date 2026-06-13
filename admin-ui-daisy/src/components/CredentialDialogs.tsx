import { AlertCircle, CheckCircle2, Download, Eye, EyeOff, FileUp, Loader2, Play, RotateCw, XCircle } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Alert, Button, Card, Checkbox, Form, Input, Loading, Modal, Progress, Select, Textarea } from 'react-daisyui'
import { addCredential, deleteCredential, exportCredentials, getCredentialBalance, setCredentialDisabled, testCredential } from '@/api/credentials'
import { Badge, FieldLabel, ModalShell } from '@/components/common'
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import { parseKamFiles, parseKamJson, type KamAccount } from '@/lib/kam-import'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage, sha256Hex } from '@/lib/utils'
import { useAddCredential, useBatchUpdateCredentials, useDeleteCredential, useProxyResources, useTestCredential } from '@/hooks/use-credentials'
import type {
  AddCredentialRequest,
  BatchUpdateCredentialsRequest,
  CredentialExportFormat,
  CredentialStatusItem,
  ProxyResource,
  TestCredentialResponse,
} from '@/types/api'

type AuthMethod = 'social' | 'idc' | 'api_key'

function SecretInput({
  value,
  onChange,
  visible,
  onToggle,
  placeholder,
  disabled,
}: {
  value: string
  onChange: (value: string) => void
  visible: boolean
  onToggle: () => void
  placeholder?: string
  disabled?: boolean
}) {
  return (
    <div className="relative">
      <Input
        bordered
        size="sm"
        className="pr-10"
        type={visible ? 'text' : 'password'}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
      <Button
        type="button"
        color="ghost"
        size="xs"
        className="absolute right-1 top-1 h-7 min-h-0 px-2"
        onClick={onToggle}
        disabled={disabled}
        title={visible ? '隐藏' : '显示'}
      >
        {visible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
      </Button>
    </div>
  )
}

function initialCredentialForm(): Required<Pick<AddCredentialRequest, 'email' | 'refreshToken' | 'kiroApiKey' | 'profileArn' | 'region' | 'authRegion' | 'apiRegion' | 'clientId' | 'clientSecret' | 'machineId' | 'proxyUrl' | 'proxyUsername' | 'proxyPassword' | 'endpoint'>> & { authMethod: AuthMethod; priority: string; maxConcurrentRequests: string; proxyResourceId: string } {
  return {
    authMethod: 'social',
    refreshToken: '',
    kiroApiKey: '',
    profileArn: '',
    region: '',
    authRegion: '',
    apiRegion: '',
    clientId: '',
    clientSecret: '',
    email: '',
    priority: '0',
    maxConcurrentRequests: '',
    machineId: '',
    proxyUrl: '',
    proxyUsername: '',
    proxyPassword: '',
    proxyResourceId: '',
    endpoint: '',
  }
}

function formFromCredential(credential: AddCredentialRequest) {
  return {
    ...initialCredentialForm(),
    authMethod: credential.authMethod || (credential.kiroApiKey ? 'api_key' : credential.clientId && credential.clientSecret ? 'idc' : 'social'),
    refreshToken: credential.refreshToken || '',
    kiroApiKey: credential.kiroApiKey || '',
    profileArn: credential.profileArn || '',
    region: credential.region || '',
    authRegion: credential.authRegion || '',
    apiRegion: credential.apiRegion || '',
    clientId: credential.clientId || '',
    clientSecret: credential.clientSecret || '',
    email: credential.email || '',
    priority: String(credential.priority ?? 0),
    maxConcurrentRequests: typeof credential.maxConcurrentRequests === 'number' ? String(credential.maxConcurrentRequests) : '',
    machineId: credential.machineId || '',
    proxyUrl: credential.proxyUrl || '',
    proxyUsername: credential.proxyUsername || '',
    proxyPassword: credential.proxyPassword || '',
    proxyResourceId: credential.proxyResourceId ? String(credential.proxyResourceId) : '',
    endpoint: credential.endpoint || '',
  }
}

interface CredentialParameterDefaults {
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

type ImportVerificationMode = 'model_and_subscription' | 'subscription_only'

function initialParameterDefaults(): CredentialParameterDefaults {
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

function optionalTrimmed(value?: string | null) {
  const trimmed = value?.trim()
  return trimmed ? trimmed : undefined
}

function parseOptionalNonNegativeInteger(value: string, label: string): number | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const parsed = Number(trimmed)
  if (!Number.isInteger(parsed) || parsed < 0) throw new Error(`${label}必须是非负整数`)
  return parsed
}

async function verifyImportedCredential(
  credentialId: number,
  mode: ImportVerificationMode
): Promise<{ model: string; response: string }> {
  if (mode === 'subscription_only') {
    const info = await getCredentialBalance(credentialId)
    return {
      model: '订阅查询',
      response: `订阅: ${info.subscriptionTitle || '未知'}，用量 ${info.currentUsage}/${info.usageLimit}`,
    }
  }

  const tested = await testCredential(credentialId, { model: DEFAULT_TEST_MODEL, prompt: DEFAULT_TEST_PROMPT })
  try {
    await getCredentialBalance(credentialId)
  } catch (error) {
    toast.warning(`凭据 #${credentialId} 验活成功，但查询信息失败: ${extractErrorMessage(error)}`)
  }
  return {
    model: testModelLabel(tested.model),
    response: tested.response,
  }
}

function ImportVerificationModeSelect({
  value,
  onChange,
  disabled,
}: {
  value: ImportVerificationMode
  onChange: (value: ImportVerificationMode) => void
  disabled?: boolean
}) {
  return (
    <div className="rounded-lg border border-base-300 bg-base-200/40 p-3">
      <FieldLabel title="验活方式" description="只查询订阅时不会发送模型测试请求；订阅查询失败的凭据仍会按验活失败回滚。">
        <Select bordered size="sm" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value as ImportVerificationMode)}>
          <Select.Option value="model_and_subscription">测试模型 + 查询订阅</Select.Option>
          <Select.Option value="subscription_only">只查询订阅（不请求模型）</Select.Option>
        </Select>
      </FieldLabel>
    </div>
  )
}

function clearDirectProxyDraft<T extends { proxyUrl: string; proxyUsername: string; proxyPassword: string }>(values: T): T {
  return { ...values, proxyUrl: '', proxyUsername: '', proxyPassword: '' }
}

function clearProxyResourceDraft<T extends { proxyResourceId: string }>(values: T): T {
  return { ...values, proxyResourceId: '' }
}

function mergeCredentialDefaults(credential: AddCredentialRequest, defaults: CredentialParameterDefaults): AddCredentialRequest {
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

function CredentialParameterDefaultsPanel({
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
    <div className="rounded-lg border border-base-300 bg-base-200/40 p-3">
      <div className="mb-3 flex items-start justify-between gap-3">
        <div>
          <div className="text-sm font-semibold">{title}</div>
          <div className="mt-1 text-xs text-base-content/55">只填充每条凭据里缺失的字段；导入 JSON 中已有的字段会保留。</div>
        </div>
        <Button type="button" color="ghost" size="xs" disabled={disabled} onClick={() => onChange(initialParameterDefaults())}>
          清空
        </Button>
      </div>
      <div className="form-grid">
        <FieldLabel title="默认优先级" description="留空时使用凭据自身值或 0">
          <Input bordered size="sm" type="number" min={0} value={defaults.priority} disabled={disabled} onChange={(event) => update('priority', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="默认账号并发" description="留空继承全局，0 表示不限">
          <Input bordered size="sm" type="number" min={0} value={defaults.maxConcurrentRequests} disabled={disabled} onChange={(event) => update('maxConcurrentRequests', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="Region 兼容字段" description="未设置 Auth Region 时作为 token 刷新回退">
          <Input bordered size="sm" className="font-mono" value={defaults.region} disabled={disabled} onChange={(event) => update('region', event.target.value)} placeholder="us-east-1" />
        </FieldLabel>
        <FieldLabel title="Auth Region" description="token 刷新区域">
          <Input bordered size="sm" className="font-mono" value={defaults.authRegion} disabled={disabled} onChange={(event) => update('authRegion', event.target.value)} placeholder="us-east-1" />
        </FieldLabel>
        <FieldLabel title="API Region" description="API 请求区域">
          <Input bordered size="sm" className="font-mono" value={defaults.apiRegion} disabled={disabled} onChange={(event) => update('apiRegion', event.target.value)} placeholder="us-east-1" />
        </FieldLabel>
        <FieldLabel title="Machine ID" description="留空使用全局配置或自动派生">
          <Input bordered size="sm" value={defaults.machineId} disabled={disabled} onChange={(event) => update('machineId', event.target.value)} />
        </FieldLabel>
        <FieldLabel title="端点" description="留空使用全局 defaultEndpoint">
          <Input bordered size="sm" value={defaults.endpoint} disabled={disabled} onChange={(event) => update('endpoint', event.target.value)} placeholder="ide / cli" />
        </FieldLabel>
        <FieldLabel title="代理资源" description="选择代理资源会清空直连代理；填写直连代理会自动取消资源">
          <Select bordered size="sm" value={defaults.proxyResourceId} disabled={disabled} onChange={(event) => update('proxyResourceId', event.target.value)}>
            <Select.Option value="">不绑定</Select.Option>
            {proxyResources.map((resource) => (
              <Select.Option key={resource.id} value={String(resource.id)}>
                {resource.name}
              </Select.Option>
            ))}
          </Select>
        </FieldLabel>
        <FieldLabel title="独立代理 URL" description={proxyLocked ? '已选择代理资源，输入前请先取消资源' : '可填 direct 或完整代理 URL；填写后会取消代理资源'}>
          <Input bordered size="sm" value={defaults.proxyUrl} disabled={disabled || proxyLocked} onChange={(event) => update('proxyUrl', event.target.value)} placeholder="socks5h://127.0.0.1:1080" />
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

export function AddCredentialModal({
  open,
  onClose,
}: {
  open: boolean
  onClose: () => void
}) {
  const [form, setForm] = useState(initialCredentialForm)
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const add = useAddCredential()
  const proxyResources = useProxyResources()
  const proxyResourceOptions = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const isApiKey = form.authMethod === 'api_key'

  useEffect(() => {
    if (!open) {
      setForm(initialCredentialForm())
      setShowProxyUsername(false)
      setShowProxyPassword(false)
    }
  }, [open])

  const update = (key: keyof typeof form, value: string) => setForm((prev) => {
    if (key === 'authMethod') {
      const authMethod = value as AuthMethod
      return {
        ...prev,
        authMethod,
        refreshToken: authMethod === 'api_key' ? '' : prev.refreshToken,
        kiroApiKey: authMethod === 'api_key' ? prev.kiroApiKey : '',
        clientId: authMethod === 'idc' ? prev.clientId : '',
        clientSecret: authMethod === 'idc' ? prev.clientSecret : '',
      }
    }
    if (key === 'region' && value.trim() && !prev.authRegion.trim()) {
      return { ...prev, region: value, authRegion: value }
    }
    if (key === 'proxyResourceId' && value) {
      return clearDirectProxyDraft({ ...prev, proxyResourceId: value })
    }
    if ((key === 'proxyUrl' || key === 'proxyUsername' || key === 'proxyPassword') && value.trim()) {
      return clearProxyResourceDraft({ ...prev, [key]: value })
    }
    return { ...prev, [key]: value }
  })

  const handleFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const result = await parseCredentialImportFiles(files)
    if (!result.credentials[0]) {
      toast.error(result.errors[0] || '文件中没有有效凭据')
      return
    }
    setForm(formFromCredential(result.credentials[0]))
    toast.success(`已填充第一条凭据${result.credentials.length > 1 ? `，另有 ${result.credentials.length - 1} 条可批量导入` : ''}`)
    if (result.errors.length) toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
  }

  const submit = (event: React.FormEvent) => {
    event.preventDefault()
    if (isApiKey && !form.kiroApiKey.trim()) return toast.error('请输入 Kiro API Key')
    if (!isApiKey && !form.refreshToken.trim()) return toast.error('请输入 Refresh Token')
    if (form.authMethod === 'idc' && (!form.clientId.trim() || !form.clientSecret.trim())) {
      return toast.error('IdC/Builder-ID/IAM 认证需要填写 Client ID 和 Client Secret')
    }
    const priority = Number(form.priority)
    if (!Number.isInteger(priority) || priority < 0) return toast.error('优先级必须是非负整数')
    let maxConcurrentRequests: number | undefined
    try {
      maxConcurrentRequests = parseOptionalNonNegativeInteger(form.maxConcurrentRequests, '账号并发覆盖')
    } catch (error) {
      return toast.error(extractErrorMessage(error))
    }

    add.mutate(
      {
        authMethod: form.authMethod,
        refreshToken: isApiKey ? undefined : form.refreshToken.trim(),
        kiroApiKey: isApiKey ? form.kiroApiKey.trim() : undefined,
        profileArn: form.profileArn.trim() || undefined,
        region: form.region.trim() || undefined,
        authRegion: form.authRegion.trim() || undefined,
        apiRegion: form.apiRegion.trim() || undefined,
        clientId: isApiKey ? undefined : form.clientId.trim() || undefined,
        clientSecret: isApiKey ? undefined : form.clientSecret.trim() || undefined,
        email: form.email.trim() || undefined,
        priority,
        maxConcurrentRequests,
        machineId: form.machineId.trim() || undefined,
        proxyResourceId: form.proxyResourceId ? Number(form.proxyResourceId) : undefined,
        proxyUrl: form.proxyUrl.trim() || undefined,
        proxyUsername: form.proxyUsername.trim() || undefined,
        proxyPassword: form.proxyPassword.trim() || undefined,
        endpoint: form.endpoint.trim() || undefined,
      },
      {
        onSuccess: (data) => {
          toast.success(data.message)
          onClose()
        },
        onError: (error) => toast.error(`添加失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  return (
    <ModalShell open={open} title="添加凭据" width="max-w-3xl" onClose={onClose}>
      <form className="space-y-4" onSubmit={submit}>
        <div className="flex justify-end">
          <Button tag="label" variant="outline" size="sm">
            <FileUp className="h-4 w-4" />
            从文件填充
            <input type="file" accept=".json,.jsonl,.txt,application/json" className="hidden" onChange={handleFile} />
          </Button>
        </div>
        <div className="form-grid">
          <FieldLabel title="认证方式">
            <Select bordered size="sm" value={form.authMethod} onChange={(event) => update('authMethod', event.target.value)}>
              <Select.Option value="social">Social</Select.Option>
              <Select.Option value="idc">IdC/Builder-ID/IAM</Select.Option>
              <Select.Option value="api_key">API Key</Select.Option>
            </Select>
          </FieldLabel>
          <FieldLabel title="账号邮箱" description="可选，用于管理页识别账号">
            <Input bordered size="sm" value={form.email} onChange={(event) => update('email', event.target.value)} />
          </FieldLabel>
          {isApiKey ? (
            <FieldLabel title="Kiro API Key">
              <Input bordered size="sm" type="password" value={form.kiroApiKey} onChange={(event) => update('kiroApiKey', event.target.value)} placeholder="ksk_xxxxxxxx" />
            </FieldLabel>
          ) : (
            <FieldLabel title="Refresh Token">
              <Input bordered size="sm" type="password" value={form.refreshToken} onChange={(event) => update('refreshToken', event.target.value)} />
            </FieldLabel>
          )}
          {form.authMethod === 'idc' && (
            <>
              <FieldLabel title="Client ID">
                <Input bordered size="sm" value={form.clientId} onChange={(event) => update('clientId', event.target.value)} />
              </FieldLabel>
              <FieldLabel title="Client Secret">
                <Input bordered size="sm" type="password" value={form.clientSecret} onChange={(event) => update('clientSecret', event.target.value)} />
              </FieldLabel>
            </>
          )}
          <FieldLabel title="Region 兼容字段" description="留空使用全局配置；未设置 Auth Region 时作为 token 刷新回退">
            <Input bordered size="sm" className="font-mono" value={form.region} onChange={(event) => update('region', event.target.value)} placeholder="us-east-1" />
          </FieldLabel>
          <FieldLabel title="Auth Region" description="留空使用全局配置">
            <Input bordered size="sm" className="font-mono" value={form.authRegion} onChange={(event) => update('authRegion', event.target.value)} placeholder="us-east-1" />
          </FieldLabel>
          <FieldLabel title="API Region" description="留空使用全局配置">
            <Input bordered size="sm" className="font-mono" value={form.apiRegion} onChange={(event) => update('apiRegion', event.target.value)} placeholder="us-east-1" />
          </FieldLabel>
          <FieldLabel title="优先级" description="数字越小优先级越高">
            <Input bordered size="sm" type="number" min={0} value={form.priority} onChange={(event) => update('priority', event.target.value)} />
          </FieldLabel>
          <FieldLabel title="账号并发覆盖" description="留空继承全局，0 表示该账号不限并发">
            <Input bordered size="sm" type="number" min={0} value={form.maxConcurrentRequests} onChange={(event) => update('maxConcurrentRequests', event.target.value)} />
          </FieldLabel>
          <FieldLabel title="Machine ID" description="留空使用配置中字段或自动派生">
            <Input bordered size="sm" value={form.machineId} onChange={(event) => update('machineId', event.target.value)} />
          </FieldLabel>
          <FieldLabel title="端点" description="留空使用全局 defaultEndpoint">
            <Input bordered size="sm" value={form.endpoint} onChange={(event) => update('endpoint', event.target.value)} placeholder="ide / cli" />
          </FieldLabel>
          <FieldLabel title="代理资源" description="选择代理资源会清空直连代理；填写直连代理会自动取消资源">
            <Select bordered size="sm" value={form.proxyResourceId} onChange={(event) => update('proxyResourceId', event.target.value)}>
              <Select.Option value="">不绑定</Select.Option>
              {proxyResourceOptions.map((resource) => (
                <Select.Option key={resource.id} value={String(resource.id)}>
                  {resource.name}
                </Select.Option>
              ))}
            </Select>
          </FieldLabel>
          <FieldLabel title="独立代理 URL" description={form.proxyResourceId ? '已选择代理资源，输入前请先取消资源' : '可填 direct 或完整代理 URL；填写后会取消代理资源'}>
            <Input bordered size="sm" value={form.proxyUrl} onChange={(event) => update('proxyUrl', event.target.value)} disabled={Boolean(form.proxyResourceId)} placeholder="socks5h://127.0.0.1:1080" />
          </FieldLabel>
          <FieldLabel title="代理用户名">
            <SecretInput
              value={form.proxyUsername}
              onChange={(value) => update('proxyUsername', value)}
              visible={showProxyUsername}
              onToggle={() => setShowProxyUsername((value) => !value)}
              disabled={Boolean(form.proxyResourceId)}
            />
          </FieldLabel>
          <FieldLabel title="代理密码">
            <SecretInput
              value={form.proxyPassword}
              onChange={(value) => update('proxyPassword', value)}
              visible={showProxyPassword}
              onToggle={() => setShowProxyPassword((value) => !value)}
              disabled={Boolean(form.proxyResourceId)}
            />
          </FieldLabel>
        </div>
        <Modal.Actions>
          <Button type="button" color="ghost" size="sm" onClick={onClose} disabled={add.isPending}>
            取消
          </Button>
          <Button type="submit" color="primary" size="sm" disabled={add.isPending}>
            {add.isPending && <Loading size="sm" />}
            添加
          </Button>
        </Modal.Actions>
      </form>
    </ModalShell>
  )
}

function credentialName(credential: CredentialStatusItem) {
  return credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
}

export function CredentialTestModal({
  credential,
  open,
  onClose,
}: {
  credential: CredentialStatusItem | null
  open: boolean
  onClose: () => void
}) {
  const [model, setModel] = useState(DEFAULT_TEST_MODEL)
  const [prompt, setPrompt] = useState(DEFAULT_TEST_PROMPT)
  const [result, setResult] = useState<TestCredentialResponse | null>(null)
  const [error, setError] = useState('')
  const test = useTestCredential()

  useEffect(() => {
    if (open) {
      setResult(null)
      setError('')
      setPrompt(DEFAULT_TEST_PROMPT)
    }
  }, [open, credential?.id])

  const run = () => {
    if (!credential) return
    if (!prompt.trim()) return toast.error('测试消息不能为空')
    setResult(null)
    setError('')
    test.mutate(
      { id: credential.id, request: { model, prompt: prompt.trim() } },
      {
        onSuccess: (response) => {
          setResult(response)
          toast.success(`凭据 #${response.credentialId} 测试完成`)
        },
        onError: (err) => setError(extractErrorMessage(err)),
      }
    )
  }

  return (
    <ModalShell open={open} title="测试模型调用" width="max-w-4xl" onClose={onClose}>
      {credential && (
        <div className="space-y-4">
          <Card bordered className="bg-base-200">
            <Card.Body className="p-4">
              <div className="flex flex-wrap items-center gap-2">
              <span className="text-lg font-semibold">{credentialName(credential)}</span>
              <Badge tone="neutral">#{credential.id}</Badge>
              <Badge tone={credential.disabled ? 'error' : 'success'}>{credential.disabled ? '已禁用' : '启用'}</Badge>
              {credential.endpoint && <Badge>{credential.endpoint}</Badge>}
            </div>
            </Card.Body>
          </Card>
          <div className="grid gap-3 md:grid-cols-[1fr_240px]">
            <FieldLabel title="测试模型">
              <Select bordered size="sm" value={model} disabled={test.isPending} onChange={(event) => setModel(event.target.value)}>
                {TEST_MODELS.map((option) => (
                  <Select.Option key={option.id} value={option.id}>
                    {option.label}
                  </Select.Option>
                ))}
              </Select>
            </FieldLabel>
            <FieldLabel title="测试消息">
              <Input bordered size="sm" value={prompt} disabled={test.isPending} onChange={(event) => setPrompt(event.target.value)} />
            </FieldLabel>
          </div>
          <div className="rounded-box bg-neutral p-4 font-mono text-sm text-neutral-content">
            {test.isPending && (
              <div className="flex items-center gap-2 text-info">
                <Loader2 className="h-4 w-4 animate-spin" />
                正在等待模型响应...
              </div>
            )}
            {result && (
              <div className="space-y-3">
                <div className="whitespace-pre-wrap break-words text-success">{result.response}</div>
                <div className="border-t border-neutral-content/20 pt-3">耗时 {result.durationMs}ms，模型 {testModelLabel(result.model)}</div>
              </div>
            )}
            {error && <div className="whitespace-pre-wrap break-words text-error">{error}</div>}
            {!test.isPending && !result && !error && <div className="text-neutral-content/60">等待开始测试</div>}
          </div>
          <Modal.Actions>
            <Button type="button" color="ghost" size="sm" onClick={onClose} disabled={test.isPending}>
              关闭
            </Button>
            <Button type="button" color="primary" size="sm" onClick={run} disabled={test.isPending}>
              {test.isPending ? <Loader2 className="h-4 w-4 animate-spin" /> : result || error ? <RotateCw className="h-4 w-4" /> : <Play className="h-4 w-4" />}
              {result || error ? '重试' : '开始测试'}
            </Button>
          </Modal.Actions>
        </div>
      )}
    </ModalShell>
  )
}

interface VerificationResult {
  index: number
  status: 'pending' | 'checking' | 'verifying' | 'verified' | 'duplicate' | 'failed' | 'skipped'
  credential?: AddCredentialRequest
  account?: KamAccount
  error?: string
  model?: string
  response?: string
  email?: string
  credentialId?: number
  rollbackStatus?: 'success' | 'failed' | 'skipped'
  rollbackError?: string
}

async function rollbackCredential(id: number): Promise<{ success: boolean; error?: string }> {
  try {
    await setCredentialDisabled(id, true)
  } catch (error) {
    return { success: false, error: `禁用失败: ${extractErrorMessage(error)}` }
  }
  try {
    await deleteCredential(id)
    return { success: true }
  } catch (error) {
    return { success: false, error: `删除失败: ${extractErrorMessage(error)}` }
  }
}

function statusIcon(status: VerificationResult['status']) {
  if (status === 'checking' || status === 'verifying') return <Loader2 className="h-5 w-5 animate-spin text-info" />
  if (status === 'verified') return <CheckCircle2 className="h-5 w-5 text-success" />
  if (status === 'duplicate' || status === 'skipped') return <AlertCircle className="h-5 w-5 text-warning" />
  if (status === 'failed') return <XCircle className="h-5 w-5 text-error" />
  return <div className="h-5 w-5 rounded-full border border-base-300" />
}

function statusText(result: VerificationResult) {
  if (result.status === 'pending') return '等待中'
  if (result.status === 'checking') return '检查重复...'
  if (result.status === 'verifying') return '验活中...'
  if (result.status === 'verified') return '验活成功'
  if (result.status === 'duplicate') return '重复凭据'
  if (result.status === 'skipped') return '已跳过'
  if (result.rollbackStatus === 'success') return '验活失败（已排除）'
  if (result.rollbackStatus === 'failed') return '验活失败（未排除）'
  return '验活失败（未创建）'
}

function ImportResults({ results, current, total, currentProcessing, importing }: { results: VerificationResult[]; current: number; total: number; currentProcessing?: string; importing: boolean }) {
  if (!importing && results.length === 0) return null
  return (
    <div className="space-y-3">
      <div>
        <div className="mb-1 flex justify-between text-sm">
          <span>{importing ? '验活进度' : '验活完成'}</span>
          <span>{current} / {total}</span>
        </div>
        <Progress color="primary" value={current} max={Math.max(total, 1)} />
        {currentProcessing && <div className="mt-1 text-xs text-base-content/60">{currentProcessing}</div>}
      </div>
      <div className="flex flex-wrap gap-2 text-sm">
        <Badge tone="success">成功 {results.filter((item) => item.status === 'verified').length}</Badge>
        <Badge tone="warning">重复 {results.filter((item) => item.status === 'duplicate').length}</Badge>
        <Badge tone="error">失败 {results.filter((item) => item.status === 'failed').length}</Badge>
        <Badge>跳过 {results.filter((item) => item.status === 'skipped').length}</Badge>
      </div>
      <div className="max-h-72 overflow-y-auto rounded-box border border-base-300">
        {results.map((result) => (
          <div key={result.index} className="flex gap-3 border-b border-base-300 p-3 last:border-0">
            {statusIcon(result.status)}
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">{result.email || `凭据 #${result.index}`}</span>
                <span className="text-xs text-base-content/60">{statusText(result)}</span>
                {result.credentialId && <Badge>#{result.credentialId}</Badge>}
              </div>
              {result.model && <div className="mt-1 text-xs text-base-content/60">模型: {result.model}</div>}
              {result.response && <div className="mt-1 line-clamp-2 text-xs text-base-content/60">响应: {result.response}</div>}
              {result.error && <div className="mt-1 whitespace-pre-wrap break-words text-xs text-error">{result.error}</div>}
              {result.rollbackError && <div className="mt-1 text-xs text-error">回滚失败: {result.rollbackError}</div>}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

export function BatchImportModal({
  open,
  onClose,
  existingCredentials,
  onDone,
}: {
  open: boolean
  onClose: () => void
  existingCredentials: CredentialStatusItem[]
  onDone: () => void
}) {
  const [jsonInput, setJsonInput] = useState('')
  const [importing, setImporting] = useState(false)
  const [progress, setProgress] = useState({ current: 0, total: 0 })
  const [currentProcessing, setCurrentProcessing] = useState('')
  const [results, setResults] = useState<VerificationResult[]>([])
  const [defaults, setDefaults] = useState<CredentialParameterDefaults>(initialParameterDefaults)
  const [verificationMode, setVerificationMode] = useState<ImportVerificationMode>('model_and_subscription')
  const proxyResources = useProxyResources()
  const proxyResourceOptions = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)

  const reset = () => {
    setJsonInput('')
    setProgress({ current: 0, total: 0 })
    setCurrentProcessing('')
    setResults([])
    setDefaults(initialParameterDefaults())
    setVerificationMode('model_and_subscription')
  }

  const appendCredentials = (credentials: AddCredentialRequest[]) => {
    let existing: AddCredentialRequest[] = []
    if (jsonInput.trim()) {
      try {
        existing = parseCredentialImportText(jsonInput)
      } catch {
        existing = []
      }
    }
    setJsonInput(JSON.stringify([...existing, ...credentials], null, 2))
  }

  const handleFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const result = await parseCredentialImportFiles(files)
    if (result.credentials.length) {
      appendCredentials(result.credentials)
      toast.success(`已从 ${files.length} 个文件读取 ${result.credentials.length} 条凭据`)
    }
    if (result.errors.length) toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
    if (!result.credentials.length && !result.errors.length) toast.error('没有读取到有效凭据')
  }

  const run = async (retryCredentials?: AddCredentialRequest[]) => {
    let credentials: AddCredentialRequest[]
    if (retryCredentials) {
      credentials = retryCredentials
    } else {
      try {
        credentials = parseCredentialImportText(jsonInput)
      } catch (error) {
        toast.error(`JSON 格式错误: ${extractErrorMessage(error)}`)
        return
      }
    }
    if (!credentials.length) return toast.error('没有可导入的凭据')
    try {
      credentials = credentials.map((credential) => mergeCredentialDefaults(credential, defaults))
    } catch (error) {
      toast.error(extractErrorMessage(error))
      return
    }

    setImporting(true)
    setProgress({ current: 0, total: credentials.length })
    setResults(credentials.map((credential, index) => ({ index: index + 1, status: 'pending', credential })))

    const existingOauthHashes = new Set(existingCredentials.map((item) => item.refreshTokenHash).filter((item): item is string => Boolean(item)))
    const existingApiKeyHashes = new Set(existingCredentials.map((item) => item.apiKeyHash).filter((item): item is string => Boolean(item)))
    let successCount = 0
    let duplicateCount = 0
    let failCount = 0

    for (let index = 0; index < credentials.length; index += 1) {
      const cred = credentials[index]
      const isApiKeyCred = Boolean(cred.kiroApiKey?.trim()) || cred.authMethod === 'api_key'
      setCurrentProcessing(`正在处理凭据 ${index + 1}/${credentials.length}`)
      setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'checking' } : item)))
      let hash = ''
      if (isApiKeyCred) {
        const apiKey = cred.kiroApiKey?.trim() || ''
        if (!apiKey) {
          failCount += 1
          setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'failed', error: '缺少 kiroApiKey' } : item)))
          setProgress({ current: index + 1, total: credentials.length })
          continue
        }
        hash = await sha256Hex(apiKey)
        if (existingApiKeyHashes.has(hash)) {
          duplicateCount += 1
          setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'duplicate', error: '该凭据已存在' } : item)))
          setProgress({ current: index + 1, total: credentials.length })
          continue
        }
      } else {
        const token = cred.refreshToken?.trim() || ''
        if (!token) {
          failCount += 1
          setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'failed', error: '缺少 refreshToken' } : item)))
          setProgress({ current: index + 1, total: credentials.length })
          continue
        }
        hash = await sha256Hex(token)
        if (existingOauthHashes.has(hash)) {
          duplicateCount += 1
          setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'duplicate', error: '该凭据已存在' } : item)))
          setProgress({ current: index + 1, total: credentials.length })
          continue
        }
      }

      setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'verifying' } : item)))
      let addedId: number | null = null
      try {
        const clientId = cred.clientId?.trim() || undefined
        const clientSecret = cred.clientSecret?.trim() || undefined
        const authMethod = isApiKeyCred ? 'api_key' : cred.authMethod === 'idc' || (clientId && clientSecret) ? 'idc' : 'social'
        if (authMethod === 'social' && (clientId || clientSecret)) throw new Error('idc 模式需要同时提供 clientId 和 clientSecret')

        const added = await addCredential({
          authMethod,
          kiroApiKey: isApiKeyCred ? cred.kiroApiKey?.trim() : undefined,
          refreshToken: isApiKeyCred ? undefined : cred.refreshToken?.trim(),
          email: cred.email?.trim() || undefined,
          profileArn: cred.profileArn?.trim() || undefined,
          priority: cred.priority || 0,
          maxConcurrentRequests: cred.maxConcurrentRequests ?? undefined,
          region: cred.region?.trim() || undefined,
          authRegion: cred.authRegion?.trim() || undefined,
          apiRegion: cred.apiRegion?.trim() || undefined,
          clientId: isApiKeyCred ? undefined : clientId,
          clientSecret: isApiKeyCred ? undefined : clientSecret,
          machineId: cred.machineId?.trim() || undefined,
          proxyUrl: cred.proxyUrl?.trim() || undefined,
          proxyUsername: cred.proxyUsername?.trim() || undefined,
          proxyPassword: cred.proxyPassword?.trim() || undefined,
          proxyResourceId: cred.proxyResourceId || undefined,
          endpoint: cred.endpoint?.trim() || undefined,
        })
        addedId = added.credentialId
        await new Promise((resolve) => setTimeout(resolve, 1000))
        const verification = await verifyImportedCredential(added.credentialId, verificationMode)
        successCount += 1
        if (isApiKeyCred) existingApiKeyHashes.add(hash)
        else existingOauthHashes.add(hash)
        setResults((prev) =>
          prev.map((item, i) =>
            i === index
              ? { ...item, status: 'verified', model: verification.model, response: verification.response, email: added.email || cred.email, credentialId: added.credentialId }
              : item
          )
        )
      } catch (error) {
        failCount += 1
        let rollbackStatus: VerificationResult['rollbackStatus'] = 'skipped'
        let rollbackError: string | undefined
        if (addedId) {
          const rollback = await rollbackCredential(addedId)
          rollbackStatus = rollback.success ? 'success' : 'failed'
          rollbackError = rollback.error
        }
        setResults((prev) =>
          prev.map((item, i) =>
            i === index ? { ...item, status: 'failed', error: extractErrorMessage(error), rollbackStatus, rollbackError } : item
          )
        )
      }
      setProgress({ current: index + 1, total: credentials.length })
    }

    setImporting(false)
    onDone()
    if (failCount === 0 && duplicateCount === 0) toast.success(`成功导入并验活 ${successCount} 个凭据`)
    else toast.info(`验活完成：成功 ${successCount} 个，重复 ${duplicateCount} 个，失败 ${failCount} 个`)
  }

  const failedCredentials = results
    .filter((result): result is VerificationResult & { credential: AddCredentialRequest } => (
      result.status === 'failed' && Boolean(result.credential)
    ))
    .map((result) => result.credential)

  const retryFailed = async () => {
    if (!failedCredentials.length) {
      toast.error('没有可重试的失败凭据')
      return
    }
    await run(failedCredentials)
  }

  return (
    <ModalShell open={open} title="批量导入凭据（自动验活）" width="max-w-4xl" onClose={() => { if (!importing) { reset(); onClose() } }}>
      <div className="space-y-4">
        <div className="flex justify-end">
          <Button tag="label" variant="outline" size="sm">
            <FileUp className="h-4 w-4" />
            选择文件
            <input type="file" accept=".json,.jsonl,.txt,application/json" multiple className="hidden" onChange={handleFile} disabled={importing} />
          </Button>
        </div>
        {results.length === 0 && (
          <CredentialParameterDefaultsPanel
            title="导入默认参数"
            defaults={defaults}
            onChange={setDefaults}
            proxyResources={proxyResourceOptions}
            disabled={importing}
          />
        )}
        {results.length === 0 && (
          <ImportVerificationModeSelect value={verificationMode} onChange={setVerificationMode} disabled={importing} />
        )}
        <Textarea
          bordered
          size="sm"
          className="min-h-48 w-full font-mono text-xs"
          value={jsonInput}
          onChange={(event) => setJsonInput(event.target.value)}
          disabled={importing}
          placeholder={'粘贴 JSON / JSONL 格式凭据，或选择一个/多个文件。每个文件可以是单个对象、数组、JSONL 多行，或导出的 credentials/accounts 容器。'}
        />
        <ImportResults results={results} current={progress.current} total={progress.total} currentProcessing={currentProcessing} importing={importing} />
        <Modal.Actions>
          <Button type="button" color="ghost" size="sm" disabled={importing} onClick={() => { reset(); onClose() }}>
            {results.length ? '关闭' : '取消'}
          </Button>
          {results.length === 0 && (
            <Button type="button" color="primary" size="sm" disabled={importing || !jsonInput.trim()} onClick={() => run()}>
              开始导入并验活
            </Button>
          )}
          {results.length > 0 && failedCredentials.length > 0 && (
            <Button type="button" color="primary" size="sm" disabled={importing} onClick={retryFailed}>
              <RotateCw className="h-4 w-4" />
              重试失败账号
            </Button>
          )}
        </Modal.Actions>
      </div>
    </ModalShell>
  )
}

export function KamImportModal({
  open,
  onClose,
  existingCredentials,
  onDone,
}: {
  open: boolean
  onClose: () => void
  existingCredentials: CredentialStatusItem[]
  onDone: () => void
}) {
  const [jsonInput, setJsonInput] = useState('')
  const [skipErrorAccounts, setSkipErrorAccounts] = useState(true)
  const [importing, setImporting] = useState(false)
  const [progress, setProgress] = useState({ current: 0, total: 0 })
  const [currentProcessing, setCurrentProcessing] = useState('')
  const [results, setResults] = useState<VerificationResult[]>([])
  const [defaults, setDefaults] = useState<CredentialParameterDefaults>(initialParameterDefaults)
  const [verificationMode, setVerificationMode] = useState<ImportVerificationMode>('model_and_subscription')
  const proxyResources = useProxyResources()
  const proxyResourceOptions = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)

  const preview = useMemo(() => {
    if (!jsonInput.trim()) return { accounts: [] as KamAccount[], error: '' }
    try {
      return { accounts: parseKamJson(jsonInput), error: '' }
    } catch (error) {
      return { accounts: [] as KamAccount[], error: extractErrorMessage(error) }
    }
  }, [jsonInput])

  const reset = () => {
    setJsonInput('')
    setProgress({ current: 0, total: 0 })
    setCurrentProcessing('')
    setResults([])
    setDefaults(initialParameterDefaults())
    setVerificationMode('model_and_subscription')
  }

  const handleFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const result = await parseKamFiles(files)
    if (result.accounts.length) {
      let existing: KamAccount[] = []
      if (jsonInput.trim()) {
        try {
          existing = parseKamJson(jsonInput)
        } catch {
          existing = []
        }
      }
      setJsonInput(JSON.stringify({ accounts: [...existing, ...result.accounts] }, null, 2))
      toast.success(`已从 ${files.length} 个文件读取 ${result.accounts.length} 个账号`)
    }
    if (result.errors.length) toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
  }

  const run = async (retryAccounts?: KamAccount[]) => {
    let accounts: KamAccount[]
    if (retryAccounts) {
      accounts = retryAccounts
    } else {
      try {
        accounts = parseKamJson(jsonInput)
      } catch (error) {
        toast.error(`JSON 格式错误: ${extractErrorMessage(error)}`)
        return
      }
    }
    if (!accounts.length) return toast.error('没有可导入的账号')
    try {
      mergeCredentialDefaults({}, defaults)
    } catch (error) {
      toast.error(extractErrorMessage(error))
      return
    }

    setImporting(true)
    setProgress({ current: 0, total: accounts.length })
    setResults(accounts.map((account, index) => ({
      index: index + 1,
      status: skipErrorAccounts && account.status === 'error' ? 'skipped' : 'pending',
      email: account.email || account.nickname,
      account,
    })))

    const existingTokenHashes = new Set(existingCredentials.map((item) => item.refreshTokenHash).filter((item): item is string => Boolean(item)))
    let successCount = 0
    let duplicateCount = 0
    let failCount = 0
    let skippedCount = 0

    for (let index = 0; index < accounts.length; index += 1) {
      const account = accounts[index]
      if (skipErrorAccounts && account.status === 'error') {
        skippedCount += 1
        setProgress({ current: index + 1, total: accounts.length })
        continue
      }
      const token = account.credentials.refreshToken.trim()
      const tokenHash = await sha256Hex(token)
      setCurrentProcessing(`正在处理 ${account.email || account.nickname || `账号 ${index + 1}`}`)
      setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'checking' } : item)))
      if (existingTokenHashes.has(tokenHash)) {
        duplicateCount += 1
        setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'duplicate', error: '该凭据已存在' } : item)))
        setProgress({ current: index + 1, total: accounts.length })
        continue
      }
      setResults((prev) => prev.map((item, i) => (i === index ? { ...item, status: 'verifying' } : item)))
      let addedId: number | null = null
      try {
        const clientId = account.credentials.clientId?.trim() || undefined
        const clientSecret = account.credentials.clientSecret?.trim() || undefined
        const authMethod = clientId && clientSecret ? 'idc' : 'social'
        if (authMethod === 'social' && (clientId || clientSecret)) throw new Error('idc 模式需要同时提供 clientId 和 clientSecret')
        const accountRegion = account.credentials.region?.trim() || undefined
        const baseCredential: AddCredentialRequest = {
          refreshToken: token,
          authMethod,
          email: account.email?.trim() || undefined,
          profileArn: account.credentials.profileArn?.trim() || undefined,
          region: accountRegion,
          authRegion: optionalTrimmed(defaults.authRegion) || accountRegion,
          apiRegion: account.credentials.apiRegion?.trim() || undefined,
          clientId,
          clientSecret,
          machineId: account.machineId?.trim() || undefined,
        }
        const added = await addCredential(mergeCredentialDefaults(baseCredential, { ...defaults, authRegion: '' }))
        addedId = added.credentialId
        await new Promise((resolve) => setTimeout(resolve, 1000))
        const verification = await verifyImportedCredential(added.credentialId, verificationMode)
        successCount += 1
        existingTokenHashes.add(tokenHash)
        setResults((prev) =>
          prev.map((item, i) =>
            i === index
              ? { ...item, status: 'verified', model: verification.model, response: verification.response, email: added.email || account.email, credentialId: added.credentialId }
              : item
          )
        )
      } catch (error) {
        failCount += 1
        let rollbackStatus: VerificationResult['rollbackStatus'] = 'skipped'
        let rollbackError: string | undefined
        if (addedId) {
          const rollback = await rollbackCredential(addedId)
          rollbackStatus = rollback.success ? 'success' : 'failed'
          rollbackError = rollback.error
        }
        setResults((prev) =>
          prev.map((item, i) =>
            i === index ? { ...item, status: 'failed', error: extractErrorMessage(error), rollbackStatus, rollbackError } : item
          )
        )
      }
      setProgress({ current: index + 1, total: accounts.length })
    }

    setImporting(false)
    onDone()
    toast.info(`导入完成：成功 ${successCount}，重复 ${duplicateCount}，失败 ${failCount}，跳过 ${skippedCount}`)
  }

  const failedAccounts = results
    .filter((result): result is VerificationResult & { account: KamAccount } => (
      result.status === 'failed' && Boolean(result.account)
    ))
    .map((result) => result.account)

  const retryFailed = async () => {
    if (!failedAccounts.length) {
      toast.error('没有可重试的失败账号')
      return
    }
    await run(failedAccounts)
  }

  const errorCount = preview.accounts.filter((account) => account.status === 'error').length

  return (
    <ModalShell open={open} title="Kiro Account Manager 导入（自动验活）" width="max-w-4xl" onClose={() => { if (!importing) { reset(); onClose() } }}>
      <div className="space-y-4">
        <div className="flex justify-end">
          <Button tag="label" variant="outline" size="sm">
            <FileUp className="h-4 w-4" />
            选择文件
            <input type="file" accept=".json,.jsonl,.txt,application/json" multiple className="hidden" onChange={handleFile} disabled={importing} />
          </Button>
        </div>
        {results.length === 0 && (
          <CredentialParameterDefaultsPanel
            title="KAM 导入默认参数"
            defaults={defaults}
            onChange={setDefaults}
            proxyResources={proxyResourceOptions}
            disabled={importing}
          />
        )}
        {results.length === 0 && (
          <ImportVerificationModeSelect value={verificationMode} onChange={setVerificationMode} disabled={importing} />
        )}
        <Textarea
          bordered
          size="sm"
          className="min-h-48 w-full font-mono text-xs"
          value={jsonInput}
          onChange={(event) => setJsonInput(event.target.value)}
          disabled={importing}
          placeholder="粘贴 Kiro Account Manager 导出的 JSON，或选择一个/多个文件。支持新版平铺格式和旧版 credentials 嵌套格式。"
        />
        {preview.error && <Alert status="error" className="py-2 text-sm">解析失败: {preview.error}</Alert>}
        {preview.accounts.length > 0 && !results.length && (
          <div className="rounded-box border border-base-300 p-3 text-sm">
            识别到 {preview.accounts.length} 个账号{errorCount > 0 && `，其中 ${errorCount} 个为 error 状态`}
            {errorCount > 0 && (
              <Form.Label className="mt-2 flex w-fit cursor-pointer items-center gap-2 rounded-box px-1 py-1 hover:bg-base-200">
                <Checkbox size="xs" checked={skipErrorAccounts} onChange={(event) => setSkipErrorAccounts(event.target.checked)} />
                跳过 error 状态的账号
              </Form.Label>
            )}
          </div>
        )}
        <ImportResults results={results} current={progress.current} total={progress.total} currentProcessing={currentProcessing} importing={importing} />
        <Modal.Actions>
          <Button type="button" color="ghost" size="sm" disabled={importing} onClick={() => { reset(); onClose() }}>
            {results.length ? '关闭' : '取消'}
          </Button>
          {results.length === 0 && (
            <Button type="button" color="primary" size="sm" disabled={importing || !jsonInput.trim() || Boolean(preview.error) || !preview.accounts.length} onClick={() => run()}>
              开始导入并验活
            </Button>
          )}
          {results.length > 0 && failedAccounts.length > 0 && (
            <Button type="button" color="primary" size="sm" disabled={importing} onClick={retryFailed}>
              <RotateCw className="h-4 w-4" />
              重试失败账号
            </Button>
          )}
        </Modal.Actions>
      </div>
    </ModalShell>
  )
}

function optionalRegionUpdate(enabled: boolean, value: string): string | null | undefined {
  if (!enabled) return undefined
  const trimmed = value.trim()
  return trimmed ? trimmed : null
}

export function BatchEditCredentialsModal({
  open,
  ids,
  onClose,
  onDone,
}: {
  open: boolean
  ids: number[]
  onClose: () => void
  onDone: () => void
}) {
  const [updateRegions, setUpdateRegions] = useState(false)
  const [updateRegion, setUpdateRegion] = useState(false)
  const [updateAuthRegion, setUpdateAuthRegion] = useState(false)
  const [updateApiRegion, setUpdateApiRegion] = useState(false)
  const [regionValue, setRegionValue] = useState('')
  const [authRegionValue, setAuthRegionValue] = useState('')
  const [apiRegionValue, setApiRegionValue] = useState('')
  const [updateConcurrency, setUpdateConcurrency] = useState(false)
  const [concurrencyValue, setConcurrencyValue] = useState('')
  const [updateProxy, setUpdateProxy] = useState(false)
  const [proxyResourceId, setProxyResourceId] = useState('')
  const [proxyUrl, setProxyUrl] = useState('')
  const [proxyUsername, setProxyUsername] = useState('')
  const [proxyPassword, setProxyPassword] = useState('')
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const batchUpdate = useBatchUpdateCredentials()
  const proxyResources = useProxyResources()
  const proxyResourceOptions = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
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

  const setDirectProxyDraft = (key: 'url' | 'username' | 'password', value: string) => {
    if (value.trim()) setProxyResourceId('')
    if (key === 'url') setProxyUrl(value)
    if (key === 'username') setProxyUsername(value)
    if (key === 'password') setProxyPassword(value)
  }

  useEffect(() => {
    if (!open) {
      setUpdateRegions(false)
      setUpdateRegion(false)
      setUpdateAuthRegion(false)
      setUpdateApiRegion(false)
      setRegionValue('')
      setAuthRegionValue('')
      setApiRegionValue('')
      setUpdateConcurrency(false)
      setConcurrencyValue('')
      setUpdateProxy(false)
      setProxyResourceId('')
      setProxyUrl('')
      setProxyUsername('')
      setProxyPassword('')
      setShowProxyUsername(false)
      setShowProxyPassword(false)
    }
  }, [open])

  const submit = () => {
    if (!ids.length) return toast.error('请先选择要修改的凭据')
    if (!updateRegions && !updateConcurrency && !updateProxy) return toast.error('请选择至少一组要修改的参数')

    const request: BatchUpdateCredentialsRequest = { ids }
    if (updateRegions) {
      const regions = {
        region: optionalRegionUpdate(updateRegion, regionValue),
        authRegion: optionalRegionUpdate(updateAuthRegion, authRegionValue),
        apiRegion: optionalRegionUpdate(updateApiRegion, apiRegionValue),
      }
      if (typeof regions.region === 'undefined' && typeof regions.authRegion === 'undefined' && typeof regions.apiRegion === 'undefined') {
        return toast.error('请选择至少一个 Region 字段')
      }
      request.regions = regions
    }

    if (updateConcurrency) {
      try {
        request.concurrency = {
          maxConcurrentRequests: concurrencyValue.trim() ? parseOptionalNonNegativeInteger(concurrencyValue, '账号并发覆盖') : null,
        }
      } catch (error) {
        return toast.error(extractErrorMessage(error))
      }
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
        if (response.failed === 0) toast.success(`成功修改 ${response.success}/${response.total} 个凭据`)
        else toast.warning(`批量修改完成：成功 ${response.success} 个，失败 ${response.failed} 个`)
        onDone()
        onClose()
      },
      onError: (error) => toast.error(`批量修改失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <ModalShell open={open} title={`批量修改 ${ids.length} 个凭据`} width="max-w-3xl" onClose={() => { if (!batchUpdate.isPending) onClose() }}>
      <div className="space-y-4">
        <div className={`rounded-lg border p-3 ${updateRegions ? 'border-primary/40 bg-primary/5' : 'border-base-300 bg-base-200/40'}`}>
          <Form.Label className="mb-3 flex w-fit cursor-pointer items-center gap-2">
            <Checkbox size="sm" checked={updateRegions} disabled={batchUpdate.isPending} onChange={(event) => setRegionsEnabled(event.target.checked)} />
            <span className="text-sm font-semibold">修改 Region</span>
          </Form.Label>
          <div className="grid gap-3 md:grid-cols-3">
            <FieldLabel title="Region 兼容字段" description="空值表示清空该覆盖">
              <div className="space-y-2">
                <Form.Label className="flex w-fit cursor-pointer items-center gap-2 text-xs">
                  <Checkbox size="xs" checked={updateRegion} disabled={!updateRegions || batchUpdate.isPending} onChange={(event) => setUpdateRegion(event.target.checked)} />
                  修改此字段
                </Form.Label>
                <Input bordered size="sm" className="font-mono" value={regionValue} disabled={!updateRegions || !updateRegion || batchUpdate.isPending} onChange={(event) => setRegionValue(event.target.value)} placeholder="us-east-1" />
              </div>
            </FieldLabel>
            <FieldLabel title="Auth Region" description="空值表示清空该覆盖">
              <div className="space-y-2">
                <Form.Label className="flex w-fit cursor-pointer items-center gap-2 text-xs">
                  <Checkbox size="xs" checked={updateAuthRegion} disabled={!updateRegions || batchUpdate.isPending} onChange={(event) => setUpdateAuthRegion(event.target.checked)} />
                  修改此字段
                </Form.Label>
                <Input bordered size="sm" className="font-mono" value={authRegionValue} disabled={!updateRegions || !updateAuthRegion || batchUpdate.isPending} onChange={(event) => setAuthRegionValue(event.target.value)} placeholder="us-east-1" />
              </div>
            </FieldLabel>
            <FieldLabel title="API Region" description="空值表示清空该覆盖">
              <div className="space-y-2">
                <Form.Label className="flex w-fit cursor-pointer items-center gap-2 text-xs">
                  <Checkbox size="xs" checked={updateApiRegion} disabled={!updateRegions || batchUpdate.isPending} onChange={(event) => setUpdateApiRegion(event.target.checked)} />
                  修改此字段
                </Form.Label>
                <Input bordered size="sm" className="font-mono" value={apiRegionValue} disabled={!updateRegions || !updateApiRegion || batchUpdate.isPending} onChange={(event) => setApiRegionValue(event.target.value)} placeholder="us-east-1" />
              </div>
            </FieldLabel>
          </div>
        </div>

        <div className={`rounded-lg border p-3 ${updateConcurrency ? 'border-primary/40 bg-primary/5' : 'border-base-300 bg-base-200/40'}`}>
          <Form.Label className="mb-3 flex w-fit cursor-pointer items-center gap-2">
            <Checkbox size="sm" checked={updateConcurrency} disabled={batchUpdate.isPending} onChange={(event) => setUpdateConcurrency(event.target.checked)} />
            <span className="text-sm font-semibold">修改账号并发覆盖</span>
          </Form.Label>
          <FieldLabel title="账号级最大并发" description="留空改为继承全局，0 表示不限并发">
            <Input bordered size="sm" type="number" min={0} value={concurrencyValue} disabled={!updateConcurrency || batchUpdate.isPending} onChange={(event) => setConcurrencyValue(event.target.value)} />
          </FieldLabel>
        </div>

        <div className={`rounded-lg border p-3 ${updateProxy ? 'border-primary/40 bg-primary/5' : 'border-base-300 bg-base-200/40'}`}>
          <Form.Label className="mb-3 flex w-fit cursor-pointer items-center gap-2">
            <Checkbox size="sm" checked={updateProxy} disabled={batchUpdate.isPending} onChange={(event) => setUpdateProxy(event.target.checked)} />
            <span className="text-sm font-semibold">修改代理</span>
          </Form.Label>
          <div className="form-grid">
            <FieldLabel title="代理资源" description="选择资源会清空凭据直连代理；不选且 URL 为空会清空凭据级代理">
              <Select bordered size="sm" value={proxyResourceId} disabled={!updateProxy || batchUpdate.isPending} onChange={(event) => setProxyResourceDraft(event.target.value)}>
                <Select.Option value="">不绑定</Select.Option>
                {proxyResourceOptions.map((resource) => (
                  <Select.Option key={resource.id} value={String(resource.id)}>
                    {resource.name}
                  </Select.Option>
                ))}
              </Select>
            </FieldLabel>
            <FieldLabel title="独立代理 URL" description={proxyLocked ? '已选择代理资源，保存时会清空直连代理' : '可填 direct 或完整代理 URL'}>
              <Input bordered size="sm" value={proxyUrl} disabled={!updateProxy || proxyLocked || batchUpdate.isPending} onChange={(event) => setDirectProxyDraft('url', event.target.value)} placeholder="socks5h://127.0.0.1:1080" />
            </FieldLabel>
            <FieldLabel title="代理用户名">
              <SecretInput
                value={proxyUsername}
                onChange={(value) => setDirectProxyDraft('username', value)}
                visible={showProxyUsername}
                onToggle={() => setShowProxyUsername((value) => !value)}
                disabled={!updateProxy || proxyLocked || batchUpdate.isPending}
                placeholder="可选"
              />
            </FieldLabel>
            <FieldLabel title="代理密码">
              <SecretInput
                value={proxyPassword}
                onChange={(value) => setDirectProxyDraft('password', value)}
                visible={showProxyPassword}
                onToggle={() => setShowProxyPassword((value) => !value)}
                disabled={!updateProxy || proxyLocked || batchUpdate.isPending}
                placeholder="可选"
              />
            </FieldLabel>
          </div>
        </div>

        <Modal.Actions>
          <Button type="button" color="ghost" size="sm" onClick={onClose} disabled={batchUpdate.isPending}>
            取消
          </Button>
          <Button type="button" color="primary" size="sm" onClick={submit} disabled={batchUpdate.isPending || ids.length === 0}>
            {batchUpdate.isPending && <Loading size="xs" />}
            保存批量修改
          </Button>
        </Modal.Actions>
      </div>
    </ModalShell>
  )
}

export interface VerifyResult {
  id: number
  status: 'pending' | 'verifying' | 'success' | 'failed'
  model?: string
  response?: string
  error?: string
}

export function BatchVerifyModal({
  open,
  verifying,
  progress,
  results,
  onCancel,
  onClose,
}: {
  open: boolean
  verifying: boolean
  progress: { current: number; total: number }
  results: Map<number, VerifyResult>
  onCancel: () => void
  onClose: () => void
}) {
  const items = Array.from(results.values())
  return (
    <ModalShell open={open} title="批量验活" width="max-w-2xl" onClose={onClose}>
      <div className="space-y-4">
        {(verifying || items.length > 0) && (
          <div>
            <div className="mb-1 flex justify-between text-sm">
              <span>验活进度</span>
              <span>{progress.current} / {progress.total}</span>
            </div>
            <Progress color="primary" value={progress.current} max={Math.max(progress.total, 1)} />
          </div>
        )}
        <div className="max-h-96 overflow-y-auto rounded-box border border-base-300">
          {items.map((item) => (
            <div key={item.id} className="border-b border-base-300 p-3 last:border-0">
              <div className="flex justify-between gap-3">
                <div className="font-medium">凭据 #{item.id}</div>
                <Badge tone={item.status === 'success' ? 'success' : item.status === 'failed' ? 'error' : item.status === 'verifying' ? 'info' : 'neutral'}>
                  {item.status === 'success' ? '成功' : item.status === 'failed' ? '失败' : item.status === 'verifying' ? '验活中' : '等待'}
                </Badge>
              </div>
              {item.model && <div className="mt-1 text-xs text-base-content/60">模型: {item.model}</div>}
              {item.response && <div className="mt-1 line-clamp-2 text-xs text-base-content/60">响应: {item.response}</div>}
              {item.error && <div className="mt-1 whitespace-pre-wrap break-words text-xs text-error">{item.error}</div>}
            </div>
          ))}
          {!items.length && <div className="p-6 text-center text-sm text-base-content/60">暂无验活结果</div>}
        </div>
        <Modal.Actions>
          {verifying ? (
            <>
              <Button type="button" color="ghost" size="sm" onClick={onClose}>后台运行</Button>
              <Button type="button" color="error" size="sm" onClick={onCancel}>取消验活</Button>
            </>
          ) : (
            <Button type="button" color="primary" size="sm" onClick={onClose}>关闭</Button>
          )}
        </Modal.Actions>
      </div>
    </ModalShell>
  )
}

const exportFormats: Array<{ value: CredentialExportFormat; label: string; description: string }> = [
  { value: 'json', label: 'JSON 数组', description: '导出为可直接批量导入的凭据数组。' },
  { value: 'backup-json', label: '备份 JSON', description: '带导出时间和格式标识，适合归档。' },
  { value: 'jsonl', label: 'JSONL', description: '每行一个凭据，便于脚本处理。' },
]

function exportFilename(format: CredentialExportFormat): string {
  const stamp = new Date().toISOString().replace(/[:.]/g, '-')
  return `kiro-credentials-${stamp}.${format === 'jsonl' ? 'jsonl' : 'json'}`
}

function downloadBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
  URL.revokeObjectURL(url)
}

export function CredentialExportModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [format, setFormat] = useState<CredentialExportFormat>('json')
  const [exporting, setExporting] = useState(false)

  const run = async () => {
    setExporting(true)
    try {
      const blob = await exportCredentials(format)
      downloadBlob(blob, exportFilename(format))
      toast.success('凭据已导出')
      onClose()
    } catch (error) {
      toast.error(`导出失败: ${extractErrorMessage(error)}`)
    } finally {
      setExporting(false)
    }
  }

  return (
    <ModalShell open={open} title="导出凭据" width="max-w-xl" onClose={onClose}>
      <Alert status="warning" className="mb-3 py-2 text-sm">导出内容包含完整 refreshToken、kiroApiKey、代理等敏感字段。</Alert>
      <div className="space-y-2">
        {exportFormats.map((item) => (
          <Button
            key={item.value}
            type="button"
            size="sm"
            className={`h-auto w-full justify-start rounded-box border p-2.5 text-left ${format === item.value ? 'border-primary bg-primary/10' : 'border-base-300 hover:bg-base-200'}`}
            onClick={() => setFormat(item.value)}
          >
            <div className="font-medium">{item.label}</div>
            <div className="text-xs text-base-content/60">{item.description}</div>
          </Button>
        ))}
      </div>
      <Modal.Actions>
        <Button type="button" color="ghost" size="sm" onClick={onClose} disabled={exporting}>取消</Button>
        <Button type="button" color="primary" size="sm" onClick={run} disabled={exporting}>
          {exporting ? <Loading size="sm" /> : <Download className="h-4 w-4" />}
          导出
        </Button>
      </Modal.Actions>
    </ModalShell>
  )
}
