import {
  CheckCircle2,
  Download,
  FileUp,
  Loader2,
  Play,
  XCircle,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  exportCredentials,
  getCredentialBalance,
  getRouteScenarioOptions,
} from '@/api/credentials'
import { RouteSelector } from '@/features/route-scenarios/route-selector'
import {
  Button,
  Checkbox,
  Input,
  Progress,
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
  Spinner,
  Textarea,
} from '@/components/ui'
import { Field, FieldGrid, ModalShell } from '@/components/patterns'
import { parseImportText, readImportFiles } from '@/lib/credential-import/sources'
import { resolveCredentialAuthMethod } from '@/lib/credential-auth'
import {
  buildTestModelOptions,
  defaultTestModelForOptions,
  DEFAULT_TEST_MODEL,
  DEFAULT_TEST_PROMPT,
  testModelLabel,
} from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import { useAddCredential, useBatchUpdateCredentials, useCredentials, useProxyResources, useTestCredential } from '@/hooks/use-credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import type {
  AddCredentialRequest,
  BatchCredentialImportDefaults,
  BatchUpdateCredentialsRequest,
  CredentialExportFilter,
  CredentialExportFormat,
  CredentialExportScope,
  CredentialStatusItem,
  ExternalPoolRouteMode,
  ProxyResource,
  TestCredentialResponse,
} from '@/types/api'
import { SecretInput } from './credential-inputs'
import { CredentialTagsField } from '@/components/credential-tags-field'
import { credentialSubscriptionLabel } from './credential-utils'
import {
  ProxyAssignmentField,
  initialProxyAssignment,
  validateProxyAssignmentValue,
  type ProxyAssignmentValue,
} from '@/features/proxies/proxy-assignment-field'

type AuthMethod = 'social' | 'idc' | 'external_idp' | 'api_key'
type ProxyAssignmentMode = 'single' | 'round_robin' | 'direct_url' | 'none'

// ============================================================================
// Verify result type (used by BatchVerifyModal)
// ============================================================================
export interface VerifyResult {
  id: number
  status: 'pending' | 'verifying' | 'success' | 'failed'
  model?: string
  response?: string
  error?: string
}

// ============================================================================
// Helpers
// ============================================================================

export interface CredentialParameterDefaults {
  disabled: string
  priority: string
  fast: string
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
  tags: string[]
}

export function initialParameterDefaults(): CredentialParameterDefaults {
  return {
    disabled: 'false',
    priority: '',
    fast: '',
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
    tags: [],
  }
}

function initialCredentialForm() {
  return { authMethod: 'social' as AuthMethod, refreshToken: '', kiroApiKey: '', profileArn: '', region: '', authRegion: '', apiRegion: '', clientId: '', clientSecret: '', tokenEndpoint: '', issuerUrl: '', scopes: '', email: '', provider: '', priority: '0', fast: '', maxConcurrentRequests: '', disabled: 'false', machineId: '', proxyUrl: '', proxyUsername: '', proxyPassword: '', proxyResourceId: '', endpoint: '', enableOverageAfterImport: false, tags: [] as string[] }
}

function formFromCredential(c: AddCredentialRequest) {
  return {
    ...initialCredentialForm(),
    authMethod: resolveCredentialAuthMethod(c),
    refreshToken: c.refreshToken || '', kiroApiKey: c.kiroApiKey || '', profileArn: c.profileArn || '',
    region: c.region || '', authRegion: c.authRegion || '', apiRegion: c.apiRegion || '',
    clientId: c.clientId || '', clientSecret: c.clientSecret || '', tokenEndpoint: c.tokenEndpoint || '',
    issuerUrl: c.issuerUrl || '', scopes: c.scopes || '', email: c.email || '', provider: c.provider || '',
    priority: String(c.priority ?? 0),
    fast: c.fast ? String(c.fast) : '',
    maxConcurrentRequests: typeof c.maxConcurrentRequests === 'number' ? String(c.maxConcurrentRequests) : '',
    disabled: c.disabled ? 'true' : 'false',
    machineId: c.machineId || '', proxyUrl: c.proxyUrl || '', proxyUsername: c.proxyUsername || '',
    proxyPassword: c.proxyPassword || '', proxyResourceId: c.proxyResourceId ? String(c.proxyResourceId) : '',
    endpoint: c.endpoint || '',
    enableOverageAfterImport: c.enableOverageAfterImport === true,
    tags: c.tags || [],
  }
}

function optionalTrimmed(v: unknown): string | undefined {
  const t =
    typeof v === 'string'
      ? v.trim()
      : typeof v === 'number' && Number.isFinite(v)
        ? String(Math.trunc(v))
        : ''
  return t || undefined
}

function splitKiroApiKeyDraft(value: string): { key: string; region?: string } {
  const trimmed = value.trim()
  const [rawKey, rawRegion] = trimmed.split('|', 2)
  return {
    key: rawKey?.trim() || value,
    region: rawRegion?.trim() || undefined,
  }
}

function parseOptionalNonNegativeInteger(value: string, label: string): number | undefined {
  const t = value.trim()
  if (!t) return undefined
  const n = Number(t)
  if (!Number.isInteger(n) || n < 0) throw new Error(`${label}必须是非负整数`)
  return n
}

/** 把默认参数填进账号缺失的字段；`importIndex` 用于代理轮换。 */
export function mergeCredentialDefaults(
  cred: AddCredentialRequest,
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
    : defaults.proxyMode === 'single' ? parseOptionalNonNegativeInteger(defaults.proxyResourceId, '代理资源 ID') : undefined
  const defaultDirect = defaults.proxyMode === 'direct_url'
  const hasDirectProxy = Boolean(optionalTrimmed(cred.proxyUrl) || optionalTrimmed(cred.proxyUsername) || optionalTrimmed(cred.proxyPassword))
  const proxyResourceId = typeof cred.proxyResourceId !== 'undefined' ? cred.proxyResourceId : hasDirectProxy ? undefined : defaultProxyResourceId
  const useProxyResource = typeof proxyResourceId === 'number'
  return {
    ...cred,
    disabled: typeof cred.disabled === 'undefined' || cred.disabled === null ? defaults.disabled === 'true' : cred.disabled,
    priority: cred.priority ?? parseOptionalNonNegativeInteger(defaults.priority, '默认优先级'),
    // 账号自带非 0 的 fast 优先，与后端 apply_batch_import_defaults 一致。
    fast: cred.fast || parseOptionalNonNegativeInteger(defaults.fast, '默认 Fast'),
    maxConcurrentRequests: typeof cred.maxConcurrentRequests === 'undefined' ? parseOptionalNonNegativeInteger(defaults.maxConcurrentRequests, '默认账号并发') : cred.maxConcurrentRequests,
    rpm: typeof cred.rpm === 'undefined' ? parseOptionalNonNegativeInteger(defaults.rpm, '默认账号 RPM') : cred.rpm,
    region: optionalTrimmed(cred.region) || optionalTrimmed(defaults.region),
    authRegion: optionalTrimmed(cred.authRegion) || optionalTrimmed(defaults.authRegion),
    apiRegion: optionalTrimmed(cred.apiRegion) || optionalTrimmed(defaults.apiRegion),
    machineId: optionalTrimmed(cred.machineId) || optionalTrimmed(defaults.machineId),
    endpoint: optionalTrimmed(cred.endpoint) || optionalTrimmed(defaults.endpoint),
    enableOverageAfterImport: typeof cred.enableOverageAfterImport === 'undefined' || cred.enableOverageAfterImport === null ? defaults.enableOverageAfterImport : cred.enableOverageAfterImport,
    proxyResourceId,
    proxyUrl: optionalTrimmed(cred.proxyUrl) || (useProxyResource || !defaultDirect ? undefined : optionalTrimmed(defaults.proxyUrl)),
    proxyUsername: optionalTrimmed(cred.proxyUsername) || (useProxyResource || !defaultDirect ? undefined : optionalTrimmed(defaults.proxyUsername)),
    proxyPassword: optionalTrimmed(cred.proxyPassword) || (useProxyResource || !defaultDirect ? undefined : optionalTrimmed(defaults.proxyPassword)),
    tags: cred.tags?.length ? cred.tags : defaults.tags,
  }
}

/** The parameter panel as the server-side import defaults (used when pushing inventory accounts). */
export function parameterDefaultsToImportDefaults(defaults: CredentialParameterDefaults): BatchCredentialImportDefaults {
  const proxyResourceId = parseOptionalNonNegativeInteger(defaults.proxyResourceId, '代理资源 ID')
  const maxConcurrentRequests = parseOptionalNonNegativeInteger(defaults.maxConcurrentRequests, '默认账号并发')
  const rpm = parseOptionalNonNegativeInteger(defaults.rpm, '默认账号 RPM')
  return {
    disabled: defaults.disabled === 'true',
    priority: parseOptionalNonNegativeInteger(defaults.priority, '默认优先级'),
    fast: parseOptionalNonNegativeInteger(defaults.fast, '默认 Fast'),
    maxConcurrentRequests,
    rpm,
    authRegion: optionalTrimmed(defaults.authRegion) || optionalTrimmed(defaults.region),
    apiRegion: optionalTrimmed(defaults.apiRegion),
    endpoint: optionalTrimmed(defaults.endpoint),
    enableOverageAfterImport: defaults.enableOverageAfterImport,
    tags: defaults.tags,
    ...(defaults.proxyMode === 'round_robin'
      ? { proxyResourceIds: defaults.proxyResourceIds.map(Number).filter(Number.isFinite) }
      : defaults.proxyMode === 'single'
        ? proxyResourceId !== undefined ? { proxyResourceId } : {}
        : defaults.proxyMode === 'direct_url'
          ? { proxyUrl: optionalTrimmed(defaults.proxyUrl), proxyUsername: optionalTrimmed(defaults.proxyUsername), proxyPassword: optionalTrimmed(defaults.proxyPassword) }
          : {}),
  }
}

export function validateProxyAssignment(defaults: CredentialParameterDefaults): string | undefined {
  return validateProxyAssignmentValue(defaultsToProxyAssignment(defaults))
}

/** The parameter panel's proxy fields as the shared proxy-assignment value, and back. */
function defaultsToProxyAssignment(defaults: CredentialParameterDefaults): ProxyAssignmentValue {
  return {
    mode: defaults.proxyMode,
    resourceId: defaults.proxyResourceId,
    resourceIds: defaults.proxyResourceIds,
    url: defaults.proxyUrl,
    username: defaults.proxyUsername,
    password: defaults.proxyPassword,
  }
}

function proxyAssignmentToDefaults(defaults: CredentialParameterDefaults, value: ProxyAssignmentValue): CredentialParameterDefaults {
  const mode = value.mode === 'clear' ? 'none' : value.mode
  return {
    ...defaults,
    proxyMode: mode,
    proxyResourceId: mode === 'single' ? value.resourceId : '',
    proxyResourceIds: mode === 'round_robin' ? value.resourceIds : [],
    proxyUrl: mode === 'direct_url' ? value.url : '',
    proxyUsername: mode === 'direct_url' ? value.username : '',
    proxyPassword: mode === 'direct_url' ? value.password : '',
  }
}

function exportFilename(format: CredentialExportFormat): string {
  const stamp = new Date().toISOString().replace(/[:.]/g, '-')
  return `kiro-credentials-${stamp}.${format === 'jsonl' ? 'jsonl' : 'json'}`
}

function downloadBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url; a.download = filename
  document.body.appendChild(a); a.click(); a.remove()
  URL.revokeObjectURL(url)
}

// ============================================================================
// FormSection — one titled block of a large dialog
// ============================================================================

export function FormSection({ title, description, actions, children, className }: {
  title: React.ReactNode
  description?: React.ReactNode
  actions?: React.ReactNode
  children: React.ReactNode
  className?: string
}) {
  return (
    <section className={`rounded-lg border border-border/70 bg-card p-4 ${className ?? ''}`}>
      <div className="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold">{title}</h3>
          {description && <p className="mt-0.5 text-xs leading-5 text-muted-foreground">{description}</p>}
        </div>
        {actions && <div className="flex shrink-0 items-center gap-1">{actions}</div>}
      </div>
      {children}
    </section>
  )
}

// ============================================================================
// CredentialParameterDefaultsPanel
// ============================================================================

export function CredentialParameterDefaultsPanel({ defaults, onChange, proxyResources, tagOptions = [], disabled, hideProxy }: {
  defaults: CredentialParameterDefaults; onChange: (d: CredentialParameterDefaults) => void
  proxyResources: ProxyResource[]; tagOptions?: string[]; disabled?: boolean
  /** Hide the proxy fields when every account already brings a usable proxy. */
  hideProxy?: boolean
}) {
  type StringDefaultKey = Exclude<keyof CredentialParameterDefaults, 'enableOverageAfterImport' | 'proxyMode' | 'proxyResourceIds' | 'tags'>
  const update = (key: StringDefaultKey, value: string) => onChange({ ...defaults, [key]: value })
  return (
    <div className="@container space-y-4">
      <FormSection
        title="默认参数"
        description="只填充每条账号里缺失的字段；账号自身带的值优先。"
        actions={<Button type="button" variant="ghost" size="xs" disabled={disabled} onClick={() => onChange(initialParameterDefaults())}>清空</Button>}
      >
        <div className="grid gap-4 @md:grid-cols-2 @3xl:grid-cols-3">
          <Field label="导入后状态">
            <Select value={defaults.disabled} onValueChange={(v) => update('disabled', v)} disabled={disabled}>
              <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="false">启用</SelectItem>
                <SelectItem value="true">禁用</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field label="默认优先级" description="数字越小越优先"><Input type="number" min={0} value={defaults.priority} disabled={disabled} onChange={(e) => update('priority', e.target.value)} /></Field>
          <Field label="默认 Fast" description="数字越大越先打满；留空或 0 关闭"><Input type="number" min={0} value={defaults.fast} disabled={disabled} onChange={(e) => update('fast', e.target.value)} placeholder="留空关闭" /></Field>
          <Field label="端点"><Input value={defaults.endpoint} disabled={disabled} onChange={(e) => update('endpoint', e.target.value)} placeholder="ide / cli" /></Field>
          <Field label="默认账号并发" description="留空继承全局，0 不限"><Input type="number" min={0} value={defaults.maxConcurrentRequests} disabled={disabled} onChange={(e) => update('maxConcurrentRequests', e.target.value)} /></Field>
          <Field label="默认账号 RPM" description="留空继承全局，0 不限"><Input type="number" min={0} value={defaults.rpm} disabled={disabled} onChange={(e) => update('rpm', e.target.value)} /></Field>
          <Field label="Machine ID"><Input value={defaults.machineId} disabled={disabled} onChange={(e) => update('machineId', e.target.value)} /></Field>
          <Field label="Auth Region"><Input className="font-mono" value={defaults.authRegion} disabled={disabled} onChange={(e) => update('authRegion', e.target.value)} placeholder="us-east-1" /></Field>
          <Field label="API Region"><Input className="font-mono" value={defaults.apiRegion} disabled={disabled} onChange={(e) => update('apiRegion', e.target.value)} placeholder="us-east-1" /></Field>
          <Field label="Region 兼容" description="未设置 Auth Region 时的回退"><Input className="font-mono" value={defaults.region} disabled={disabled} onChange={(e) => update('region', e.target.value)} placeholder="us-east-1" /></Field>
          <div className="space-y-1.5 @md:col-span-2 @3xl:col-span-3">
            <div className="text-sm font-medium">账号标签</div>
            <CredentialTagsField
              value={defaults.tags}
              options={tagOptions}
              onChange={(tags) => onChange({ ...defaults, tags })}
              disabled={disabled}
            />
            <p className="text-[0.7rem] leading-4 text-muted-foreground">文件中已有标签优先；没有标签的账号使用这里的默认标签。</p>
          </div>
          <label className="flex items-center gap-2 text-sm @md:col-span-2 @3xl:col-span-3">
            <Checkbox
              id="default-enable-overage"
              checked={defaults.enableOverageAfterImport}
              disabled={disabled}
              onCheckedChange={(v) => onChange({ ...defaults, enableOverageAfterImport: Boolean(v) })}
            />
            导入后尝试开启超额
          </label>
        </div>
      </FormSection>
      {!hideProxy && (
        <FormSection title="代理" description="轮换按导入顺序循环分配；账号自身带的代理优先。">
          <ProxyAssignmentField
            value={defaultsToProxyAssignment(defaults)}
            onChange={(value) => onChange(proxyAssignmentToDefaults(defaults, value))}
            proxyResources={proxyResources.filter((resource) => resource.enabled)}
            modes={['single', 'round_robin', 'direct_url', 'none']}
            disabled={disabled}
          />
        </FormSection>
      )}
    </div>
  )
}

// ============================================================================
// AddCredentialModal
// ============================================================================

export function AddCredentialModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [form, setForm] = useState(initialCredentialForm)
  const [showPu, setShowPu] = useState(false)
  const [showPp, setShowPp] = useState(false)
  const add = useAddCredential()
  const { data: existingCredentials } = useCredentials({ enabled: open })
  const proxyResources = useProxyResources()
  const proxyOptions = (proxyResources.data?.resources || []).filter((r) => r.enabled)
  const tagOptions = useMemo(
    () => Array.from(new Set(existingCredentials?.credentials.flatMap((credential) => credential.tags ?? []) || [])).sort((a, b) => a.localeCompare(b)),
    [existingCredentials?.credentials],
  )
  const isApiKey = form.authMethod === 'api_key'

  useEffect(() => { if (!open) { setForm(initialCredentialForm()); setShowPu(false); setShowPp(false) } }, [open])

  const update = (key: keyof ReturnType<typeof initialCredentialForm>, value: string) =>
    setForm((prev) => {
      if (key === 'authMethod') {
        const am = value as AuthMethod
        return {
          ...prev,
          authMethod: am,
          refreshToken: am === 'api_key' ? '' : prev.refreshToken,
          kiroApiKey: am === 'api_key' ? prev.kiroApiKey : '',
          clientId: am === 'idc' || am === 'external_idp' ? prev.clientId : '',
          clientSecret: am === 'idc' || am === 'external_idp' ? prev.clientSecret : '',
          tokenEndpoint: am === 'external_idp' ? prev.tokenEndpoint : '',
          issuerUrl: am === 'external_idp' ? prev.issuerUrl : '',
          scopes: am === 'external_idp' ? prev.scopes : '',
        }
      }
      if (key === 'kiroApiKey') {
        const parsed = splitKiroApiKeyDraft(value)
        if (parsed.region) {
          return {
            ...prev,
            kiroApiKey: parsed.key,
            region: prev.region.trim() ? prev.region : parsed.region,
            authRegion: prev.authRegion.trim() ? prev.authRegion : parsed.region,
            apiRegion: prev.apiRegion.trim() ? prev.apiRegion : parsed.region,
            endpoint: prev.endpoint.trim() ? prev.endpoint : 'cli',
          }
        }
      }
      if (key === 'proxyResourceId' && value && value !== '__none__') return { ...prev, proxyResourceId: value, proxyUrl: '', proxyUsername: '', proxyPassword: '' }
      if ((key === 'proxyUrl' || key === 'proxyUsername' || key === 'proxyPassword') && value.trim()) return { ...prev, [key]: value, proxyResourceId: '' }
      return { ...prev, [key]: value }
    })

  const handleFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files || [])
    e.target.value = ''
    if (!files.length) return
    const read = await readImportFiles(files)
    const errors = [...read.errors]
    const credentials = read.inputs.flatMap(({ name, text }) => {
      try {
        return parseImportText(text).accounts.map((account) => account.credential)
      } catch (error) {
        errors.push(`${name}: ${extractErrorMessage(error)}`)
        return []
      }
    })
    if (!credentials[0]) { toast.error(errors[0] || '文件中没有有效账号'); return }
    setForm(formFromCredential(credentials[0]))
    toast.success(`已填充第一条账号${credentials.length > 1 ? `，另有 ${credentials.length - 1} 条可批量导入` : ''}`)
    if (errors.length) toast.warning(`部分文件未读取: ${errors.slice(0, 3).join('；')}`)
  }

  const submit = (e: React.FormEvent) => {
    e.preventDefault()
    if (isApiKey && !form.kiroApiKey.trim()) return toast.error('请输入 Kiro API Key')
    if (!isApiKey && !form.refreshToken.trim()) return toast.error('请输入 Refresh Token')
    if (form.authMethod === 'idc' && (!form.clientId.trim() || !form.clientSecret.trim())) return toast.error('IdC 认证需要 Client ID 和 Client Secret')
    if (form.authMethod === 'external_idp' && !form.clientId.trim()) return toast.error('External IdP 认证需要 Client ID')
    const priority = Number(form.priority)
    if (!Number.isInteger(priority) || priority < 0) return toast.error('优先级必须是非负整数')
    const fast = form.fast.trim() ? Number(form.fast) : 0
    if (!Number.isInteger(fast) || fast < 0) return toast.error('Fast 值必须是非负整数')
    let maxConcurrentRequests: number | undefined
    try { maxConcurrentRequests = parseOptionalNonNegativeInteger(form.maxConcurrentRequests, '账号并发') } catch (err) { return toast.error(extractErrorMessage(err)) }
    add.mutate({
      authMethod: form.authMethod,
      refreshToken: isApiKey ? undefined : form.refreshToken.trim(),
      kiroApiKey: isApiKey ? form.kiroApiKey.trim() : undefined,
      profileArn: form.profileArn.trim() || undefined,
      region: form.region.trim() || undefined,
      authRegion: form.authRegion.trim() || undefined,
      apiRegion: form.apiRegion.trim() || undefined,
      clientId: isApiKey ? undefined : form.clientId.trim() || undefined,
      clientSecret: form.authMethod === 'idc' || form.authMethod === 'external_idp' ? form.clientSecret.trim() || undefined : undefined,
      tokenEndpoint: form.authMethod === 'external_idp' ? form.tokenEndpoint.trim() || undefined : undefined,
      issuerUrl: form.authMethod === 'external_idp' ? form.issuerUrl.trim() || undefined : undefined,
      scopes: form.authMethod === 'external_idp' ? form.scopes.trim() || undefined : undefined,
      email: form.email.trim() || undefined,
      provider: form.provider.trim() || undefined,
      priority,
      fast,
      maxConcurrentRequests,
      disabled: form.disabled === 'true',
      machineId: form.machineId.trim() || undefined,
      proxyResourceId: form.proxyResourceId ? Number(form.proxyResourceId) : undefined,
      proxyUrl: form.proxyUrl.trim() || undefined,
      proxyUsername: form.proxyUsername.trim() || undefined,
      proxyPassword: form.proxyPassword.trim() || undefined,
      endpoint: form.endpoint.trim() || undefined,
      enableOverageAfterImport: form.enableOverageAfterImport,
      tags: form.tags,
    }, {
      onSuccess: async (data) => {
        if (data.warning) toast.warning(data.warning)
        try {
          const info = await getCredentialBalance(data.credentialId)
          toast.success(`${data.message}，订阅: ${credentialSubscriptionLabel(info.subscriptionTitle)}`)
        } catch (err) {
          toast.warning(`${data.message}，但查询订阅失败: ${extractErrorMessage(err)}`)
        }
        setForm(initialCredentialForm())
        setShowPu(false)
        setShowPp(false)
      },
      onError: (err) => toast.error(`添加失败: ${extractErrorMessage(err)}`),
    })
  }

  return (
    <ModalShell open={open} title="添加单个账号" width="max-w-2xl" onClose={onClose}>
      <form onSubmit={submit} className="space-y-4">
        <div className="flex items-center gap-2">
          <Field label="认证方式" className="w-40 shrink-0">
            <Select value={form.authMethod} onValueChange={(v) => update('authMethod', v)}>
              <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="social">Social</SelectItem>
                <SelectItem value="idc">IdC</SelectItem>
                <SelectItem value="external_idp">External IdP</SelectItem>
                <SelectItem value="api_key">API Key</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <div className="flex-1" />
          <label className="cursor-pointer">
            <input type="file" className="sr-only" accept=".json,.jsonl,.txt,.csv" multiple onChange={handleFile} />
            <Button type="button" variant="outline" size="sm" asChild><span><FileUp className="h-3.5 w-3.5" />从文件填充</span></Button>
          </label>
        </div>
        <div className="flex items-center gap-2 rounded-lg bg-muted/30 px-3 py-2">
          <Checkbox
            id="add-enable-overage"
            checked={form.enableOverageAfterImport}
            disabled={add.isPending}
            onCheckedChange={(v) => setForm((prev) => ({ ...prev, enableOverageAfterImport: Boolean(v) }))}
          />
          <label htmlFor="add-enable-overage" className="text-sm cursor-pointer">添加后尝试开启超额</label>
        </div>

        <FieldGrid>
          {isApiKey ? (
            <Field label="Kiro API Key" className="col-span-2">
              <SecretInput value={form.kiroApiKey} onChange={(v) => update('kiroApiKey', v)} visible={showPu} onToggle={() => setShowPu((v) => !v)} placeholder="ksk_... 或 ksk_...|eu-central-1" disabled={add.isPending} />
            </Field>
          ) : (
            <Field label="Refresh Token" className="col-span-2">
              <SecretInput value={form.refreshToken} onChange={(v) => update('refreshToken', v)} visible={showPu} onToggle={() => setShowPu((v) => !v)} placeholder="eyJ..." disabled={add.isPending} />
            </Field>
          )}
          {form.authMethod === 'idc' && <>
            <Field label="Client ID"><Input value={form.clientId} disabled={add.isPending} onChange={(e) => update('clientId', e.target.value)} /></Field>
            <Field label="Client Secret"><SecretInput value={form.clientSecret} onChange={(v) => update('clientSecret', v)} visible={showPp} onToggle={() => setShowPp((v) => !v)} disabled={add.isPending} /></Field>
          </>}
          {form.authMethod === 'external_idp' && <>
            <Field label="Client ID"><Input value={form.clientId} disabled={add.isPending} onChange={(e) => update('clientId', e.target.value)} /></Field>
            <Field label="Client Secret（可选）"><SecretInput value={form.clientSecret} onChange={(v) => update('clientSecret', v)} visible={showPp} onToggle={() => setShowPp((v) => !v)} disabled={add.isPending} /></Field>
            <Field label="Token Endpoint"><Input className="font-mono" value={form.tokenEndpoint} disabled={add.isPending} onChange={(e) => update('tokenEndpoint', e.target.value)} placeholder="https://.../oauth2/v2.0/token" /></Field>
            <Field label="Issuer URL（可选）"><Input className="font-mono" value={form.issuerUrl} disabled={add.isPending} onChange={(e) => update('issuerUrl', e.target.value)} placeholder="https://..." /></Field>
            <Field label="Scopes（可选）"><Input className="font-mono" value={form.scopes} disabled={add.isPending} onChange={(e) => update('scopes', e.target.value)} placeholder="offline_access ..." /></Field>
          </>}
          <Field label="邮箱（可选）"><Input value={form.email} disabled={add.isPending} onChange={(e) => update('email', e.target.value)} placeholder="user@example.com" /></Field>
          <Field label="身份提供方（可选）" description="Google / Github / BuilderId / Enterprise，用于识别账号来源和认证方式"><Input value={form.provider} disabled={add.isPending} onChange={(e) => update('provider', e.target.value)} placeholder="留空则按认证方式推断" /></Field>
          <div className="sm:col-span-2 space-y-1.5">
            <div className="text-sm font-medium">账号标签</div>
            <div className="text-xs leading-5 text-muted-foreground">可点击已有标签，也可以输入新标签后按 Enter。</div>
            <CredentialTagsField
              value={form.tags}
              options={tagOptions}
              onChange={(tags) => setForm((prev) => ({ ...prev, tags }))}
              disabled={add.isPending}
            />
          </div>
          <Field label="Profile ARN（可选）"><Input value={form.profileArn} disabled={add.isPending} onChange={(e) => update('profileArn', e.target.value)} /></Field>
          <Field label="优先级"><Input type="number" min={0} value={form.priority} disabled={add.isPending} onChange={(e) => update('priority', e.target.value)} /></Field>
          <Field label="Fast（可选）" description="数字越大越先打满；留空或 0 关闭"><Input type="number" min={0} value={form.fast} disabled={add.isPending} onChange={(e) => update('fast', e.target.value)} placeholder="留空关闭" /></Field>
          <Field label="初始状态" description="新增后默认查询订阅，不测试模型">
            <Select value={form.disabled} onValueChange={(v) => update('disabled', v)} disabled={add.isPending}>
              <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="false">启用</SelectItem>
                <SelectItem value="true">禁用</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          <Field label="账号并发（可选）" description="留空继承全局，0 不限"><Input type="number" min={0} value={form.maxConcurrentRequests} disabled={add.isPending} onChange={(e) => update('maxConcurrentRequests', e.target.value)} /></Field>
          <Field label="Auth Region"><Input className="font-mono" value={form.authRegion} disabled={add.isPending} onChange={(e) => update('authRegion', e.target.value)} placeholder="us-east-1" /></Field>
          <Field label="API Region"><Input className="font-mono" value={form.apiRegion} disabled={add.isPending} onChange={(e) => update('apiRegion', e.target.value)} placeholder="us-east-1" /></Field>
          <Field label="代理资源">
            <Select value={form.proxyResourceId || '__none__'} onValueChange={(v) => update('proxyResourceId', v === '__none__' ? '' : v)} disabled={add.isPending}>
              <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">不绑定</SelectItem>
                {proxyOptions.map((r) => <SelectItem key={r.id} value={String(r.id)}>{r.name}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          <Field label="端点"><Input value={form.endpoint} disabled={add.isPending} onChange={(e) => update('endpoint', e.target.value)} placeholder="ide / cli" /></Field>
        </FieldGrid>

        <div className="flex justify-end gap-2 pt-2">
          <Button type="button" variant="ghost" size="sm" onClick={onClose} disabled={add.isPending}>取消</Button>
          <Button type="submit" size="sm" disabled={add.isPending}>{add.isPending && <Spinner size="sm" />}添加</Button>
        </div>
      </form>
    </ModalShell>
  )
}

// ============================================================================
// BatchEditCredentialsModal
// ============================================================================

export function BatchEditCredentialsModal({ open, ids, onClose, onDone }: {
  open: boolean; ids: number[]; onClose: () => void; onDone: () => void
}) {
  const [fields, setFields] = useState({
    priority: '',
    fast: '',
    concurrency: '',
    rpm: '',
    region: '',
    authRegion: '',
    apiRegion: '',
  })
  const [proxyAssignment, setProxyAssignment] = useState<ProxyAssignmentValue>(() => initialProxyAssignment('round_robin'))
  const [rateLimitAutoDisableEnabled, setRateLimitAutoDisableEnabled] = useState(true)
  const [enableFields, setEnableFields] = useState({ priority: false, fast: false, routePolicy: false, concurrency: false, rpm: false, rateLimitAutoDisable: false, proxy: false, regions: false })
  const [routeModeDraft, setRouteModeDraft] = useState<ExternalPoolRouteMode>('allow_all')
  const [routeRulesDraft, setRouteRulesDraft] = useState<string[]>([])
  const scenarioOptions = useQuery({ queryKey: ['route-scenario-options'], queryFn: getRouteScenarioOptions, enabled: open })
  const routeOptions = scenarioOptions.data?.routes ?? []
  const batchUpdate = useBatchUpdateCredentials()
  const proxyResources = useProxyResources()
  const proxyOptions = proxyResources.data?.resources || []

  useEffect(() => {
    if (!open) {
      setFields({ priority: '', fast: '', concurrency: '', rpm: '', region: '', authRegion: '', apiRegion: '' })
      setProxyAssignment(initialProxyAssignment('round_robin'))
      setRateLimitAutoDisableEnabled(true)
      setEnableFields({ priority: false, fast: false, routePolicy: false, concurrency: false, rpm: false, rateLimitAutoDisable: false, proxy: false, regions: false })
      setRouteModeDraft('allow_all')
      setRouteRulesDraft([])
    }
  }, [open])

  const clearSchedulingDraft = () => {
    setFields((p) => ({ ...p, priority: '', concurrency: '', rpm: '' }))
    setEnableFields((p) => ({ ...p, priority: true, concurrency: true, rpm: true }))
  }

  const submit = async () => {
    if (!ids.length) { toast.error('没有选中账号'); return }
    const req: BatchUpdateCredentialsRequest = { ids }
    if (enableFields.priority) {
      const v = fields.priority.trim()
      let priority = 0
      if (v) {
        const n = Number(v)
        if (!Number.isInteger(n) || n < 0) { toast.error('优先级必须是非负整数'); return }
        priority = n
      }
      req.priority = { priority }
    }
    if (enableFields.fast) {
      const v = fields.fast.trim()
      const fast = v ? Number(v) : 0
      if (!Number.isInteger(fast) || fast < 0) { toast.error('Fast 值必须是非负整数'); return }
      req.fast = { fast }
    }
    if (enableFields.routePolicy) {
      const routeRules = routeModeDraft === 'allow_all' ? [] : routeRulesDraft
      if (routeModeDraft === 'allow_list' && routeRules.length === 0) {
        toast.error('只允许模式至少需要一条路由规则，否则这些账号不会承接任何请求')
        return
      }
      req.routePolicy = { routeMode: routeModeDraft, routeRules }
    }
    if (enableFields.proxy) {
      const proxyError = validateProxyAssignmentValue(proxyAssignment, { requireSingle: true })
      if (proxyError) { toast.error(proxyError); return }
      if (proxyAssignment.mode === 'round_robin') {
        // The server assigns these in turn in the order of `ids`.
        req.proxyResourceIds = proxyAssignment.resourceIds.map(Number)
      } else if (proxyAssignment.mode === 'single') {
        req.proxy = { proxyResourceId: Number(proxyAssignment.resourceId) }
      } else if (proxyAssignment.mode === 'direct_url') {
        req.proxy = {
          proxyResourceId: null,
          proxyUrl: proxyAssignment.url.trim(),
          proxyUsername: proxyAssignment.username.trim() || undefined,
          proxyPassword: proxyAssignment.password.trim() || undefined,
        }
      } else {
        req.proxy = { proxyResourceId: null }
      }
    }
    if (enableFields.regions) {
      req.regions = {
        region: fields.region.trim() || null,
        authRegion: fields.authRegion.trim() || null,
        apiRegion: fields.apiRegion.trim() || null,
      }
    }
    if (enableFields.concurrency) {
      const v = fields.concurrency.trim()
      let maxConcurrentRequests: number | null = null
      if (v) {
        const n = Number(v)
        if (!Number.isInteger(n) || n < 0) { toast.error('并发限制必须是非负整数'); return }
        maxConcurrentRequests = n
      }
      req.concurrency = { maxConcurrentRequests }
    }
    if (enableFields.rpm) {
      const v = fields.rpm.trim()
      let rpm: number | null = null
      if (v) {
        const n = Number(v)
        if (!Number.isInteger(n) || n < 0) { toast.error('RPM 必须是非负整数'); return }
        rpm = n
      }
      req.rpm = { rpm }
    }
    if (enableFields.rateLimitAutoDisable) {
      req.rateLimitAutoDisable = { enabled: rateLimitAutoDisableEnabled }
    }
    batchUpdate.mutate(req, {
      onSuccess: (res) => {
        toast.success(`批量修改完成：成功 ${res.success}，失败 ${res.failed}`)
        onDone(); onClose()
      },
      onError: (e) => toast.error(`批量修改失败: ${extractErrorMessage(e)}`),
    })
  }

  return (
    <ModalShell
      open={open}
      title={`批量修改（${ids.length} 个账号）`}
      description="只修改勾选的项，没勾选的保持原样。"
      width="max-w-4xl"
      onClose={onClose}
      footer={(
        <>
          <span className="mr-auto self-center text-xs text-muted-foreground">
            {Object.values(enableFields).some(Boolean) ? `将修改 ${Object.values(enableFields).filter(Boolean).length} 项` : '勾选要修改的项'}
          </span>
          <Button variant="outline" size="sm" onClick={onClose} disabled={batchUpdate.isPending}>取消</Button>
          <Button size="sm" onClick={submit} disabled={batchUpdate.isPending || !Object.values(enableFields).some(Boolean)}>
            {batchUpdate.isPending && <Spinner size="sm" />}应用修改
          </Button>
        </>
      )}
    >
      <div className="space-y-4">
        <FormSection
          title="调度"
          description="勾选左侧的框才会修改该项。"
          actions={(
            <Button type="button" variant="ghost" size="xs" onClick={clearSchedulingDraft} disabled={batchUpdate.isPending}>
              清理优先级/并发/RPM
            </Button>
          )}
        >
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <BatchField id="batch-priority" label="优先级" description="数字越小越优先；留空重置为 0"
              enabled={enableFields.priority} onEnabledChange={(v) => setEnableFields((p) => ({ ...p, priority: v }))}>
              <Input type="number" min={0} value={fields.priority} disabled={!enableFields.priority || batchUpdate.isPending}
                onChange={(e) => setFields((p) => ({ ...p, priority: e.target.value }))} placeholder="留空重置" />
            </BatchField>
            <BatchField id="batch-fast" label="Fast" description="数字越大越先打满；0 或留空关闭"
              enabled={enableFields.fast} onEnabledChange={(v) => setEnableFields((p) => ({ ...p, fast: v }))}>
              <Input type="number" min={0} value={fields.fast} disabled={!enableFields.fast || batchUpdate.isPending}
                onChange={(e) => setFields((p) => ({ ...p, fast: e.target.value }))} placeholder="留空关闭" />
            </BatchField>
            <BatchField id="batch-rate-limit-auto-disable" label="429 自动禁用" description="关闭后 429 只进入冷却，不自动禁用"
              enabled={enableFields.rateLimitAutoDisable} onEnabledChange={(v) => setEnableFields((p) => ({ ...p, rateLimitAutoDisable: v }))}>
              <Select value={rateLimitAutoDisableEnabled ? 'on' : 'off'} onValueChange={(v) => setRateLimitAutoDisableEnabled(v === 'on')}
                disabled={!enableFields.rateLimitAutoDisable || batchUpdate.isPending}>
                <SelectTrigger size="sm" aria-label="429 自动禁用"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="on">开启</SelectItem>
                  <SelectItem value="off">关闭（只冷却）</SelectItem>
                </SelectContent>
              </Select>
            </BatchField>
            <BatchField id="batch-concurrency" label="最大并发" description="留空清除覆盖，0 表示不限"
              enabled={enableFields.concurrency} onEnabledChange={(v) => setEnableFields((p) => ({ ...p, concurrency: v }))}>
              <Input type="number" min={0} value={fields.concurrency} disabled={!enableFields.concurrency || batchUpdate.isPending}
                onChange={(e) => setFields((p) => ({ ...p, concurrency: e.target.value }))} placeholder="留空继承全局" />
            </BatchField>
            <BatchField id="batch-rpm" label="RPM" description="留空清除覆盖，0 表示不限"
              enabled={enableFields.rpm} onEnabledChange={(v) => setEnableFields((p) => ({ ...p, rpm: v }))}>
              <Input type="number" min={0} value={fields.rpm} disabled={!enableFields.rpm || batchUpdate.isPending}
                onChange={(e) => setFields((p) => ({ ...p, rpm: e.target.value }))} placeholder="留空继承全局" />
            </BatchField>
          </div>
        </FormSection>

        <FormSection
          title={<SectionToggle id="batch-route-policy" checked={enableFields.routePolicy} onChange={(v) => setEnableFields((p) => ({ ...p, routePolicy: v }))}>路由限制</SectionToggle>}
          description="从路由策略里已有的路由中选择。"
        >
          {enableFields.routePolicy ? (
            <div className="grid gap-4 sm:grid-cols-[14rem_1fr]">
              <Field label="路由模式">
                <Select value={routeModeDraft} onValueChange={(v) => setRouteModeDraft(v as ExternalPoolRouteMode)} disabled={batchUpdate.isPending}>
                  <SelectTrigger size="sm" aria-label="路由模式"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="allow_all">全部入口允许</SelectItem>
                    <SelectItem value="allow_list">只允许下列入口</SelectItem>
                    <SelectItem value="deny_list">禁止下列入口</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
              <Field label="路由">
                <RouteSelector
                  routes={routeOptions}
                  value={routeRulesDraft}
                  disabled={routeModeDraft === 'allow_all' || batchUpdate.isPending}
                  onChange={setRouteRulesDraft}
                />
              </Field>
            </div>
          ) : <p className="text-xs text-muted-foreground">不修改</p>}
        </FormSection>

        <FormSection
          title={<SectionToggle id="batch-proxy" checked={enableFields.proxy} onChange={(v) => setEnableFields((p) => ({ ...p, proxy: v }))}>代理</SectionToggle>}
          description={`轮换时按账号顺序循环分配给这 ${ids.length} 个账号；清除会去掉账号自己的代理，改走全局代理或直连。`}
        >
          {enableFields.proxy ? (
            <ProxyAssignmentField
              value={proxyAssignment}
              onChange={setProxyAssignment}
              proxyResources={proxyOptions.filter((resource) => resource.enabled)}
              modes={['round_robin', 'single', 'direct_url', 'clear']}
              disabled={batchUpdate.isPending}
            />
          ) : <p className="text-xs text-muted-foreground">不修改</p>}
        </FormSection>

        <FormSection
          title={<SectionToggle id="batch-regions" checked={enableFields.regions} onChange={(v) => setEnableFields((p) => ({ ...p, regions: v }))}>Region</SectionToggle>}
        >
          {enableFields.regions ? (
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Auth Region"><Input className="font-mono" value={fields.authRegion} onChange={(e) => setFields((p) => ({ ...p, authRegion: e.target.value }))} placeholder="留空清除" /></Field>
              <Field label="API Region"><Input className="font-mono" value={fields.apiRegion} onChange={(e) => setFields((p) => ({ ...p, apiRegion: e.target.value }))} placeholder="留空清除" /></Field>
            </div>
          ) : <p className="text-xs text-muted-foreground">不修改</p>}
        </FormSection>
      </div>
    </ModalShell>
  )
}

/** A batch-edit field that only applies when its box is ticked. */
function BatchField({ id, label, description, enabled, onEnabledChange, children }: {
  id: string
  label: string
  description?: string
  enabled: boolean
  onEnabledChange: (enabled: boolean) => void
  children: React.ReactNode
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <label htmlFor={id} className="flex cursor-pointer items-center gap-2 text-sm font-medium">
        <Checkbox id={id} checked={enabled} onCheckedChange={(v) => onEnabledChange(Boolean(v))} aria-label={`修改${label}`} />
        {label}
      </label>
      {children}
      {description && <p className="text-[0.7rem] leading-4 text-muted-foreground">{description}</p>}
    </div>
  )
}

function SectionToggle({ id, checked, onChange, children }: { id: string; checked: boolean; onChange: (checked: boolean) => void; children: React.ReactNode }) {
  return (
    <label htmlFor={id} className="flex cursor-pointer items-center gap-2">
      <Checkbox id={id} checked={checked} onCheckedChange={(v) => onChange(Boolean(v))} />
      {children}
    </label>
  )
}

// ============================================================================
// CredentialTestModal
// ============================================================================

export function CredentialTestModal({ credential, open, onClose }: {
  credential: CredentialStatusItem | null; open: boolean; onClose: () => void
}) {
  const modelCapabilities = useModelCapabilities()
  const [model, setModel] = useState(DEFAULT_TEST_MODEL)
  const [prompt, setPrompt] = useState(DEFAULT_TEST_PROMPT)
  const [result, setResult] = useState<TestCredentialResponse | null>(null)
  const userSelectedModelRef = useRef(false)
  const testMutation = useTestCredential()
  const modelOptions = useMemo(
    () => buildTestModelOptions(modelCapabilities.data?.models, credential?.supportedModels),
    [modelCapabilities.data?.models, credential?.supportedModels]
  )
  const defaultModel = defaultTestModelForOptions(modelOptions)

  useEffect(() => {
    if (!open) {
      setResult(null)
      setModel(DEFAULT_TEST_MODEL)
      setPrompt(DEFAULT_TEST_PROMPT)
      userSelectedModelRef.current = false
      return
    }
    setResult(null)
    setModel(defaultModel)
    setPrompt(DEFAULT_TEST_PROMPT)
    userSelectedModelRef.current = false
  }, [open, credential?.id])

  useEffect(() => {
    if (!open || userSelectedModelRef.current) return
    setModel(defaultModel)
  }, [open, defaultModel])

  const selectModel = (value: string) => {
    userSelectedModelRef.current = true
    setModel(value)
  }

  const run = () => {
    if (!credential) return
    testMutation.mutate({ id: credential.id, request: { model, prompt } }, {
      onSuccess: (res) => { setResult(res); toast.success(`测试成功 (${res.durationMs}ms)`) },
      onError: (e) => toast.error(`测试失败: ${extractErrorMessage(e)}`),
    })
  }

  if (!credential) return null

  return (
    <ModalShell open={open} title={`测试账号 #${credential.id}`} width="max-w-lg" onClose={onClose}>
      <div className="space-y-3">
        <Field label="测试模型">
          <Select value={model} onValueChange={selectModel} disabled={testMutation.isPending}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              {modelOptions.map((m) => <SelectItem key={m.id} value={m.id}>{m.label}</SelectItem>)}
            </SelectContent>
          </Select>
        </Field>
        <Field label="测试 Prompt">
          <Textarea className="font-mono text-xs" rows={3} value={prompt} disabled={testMutation.isPending} onChange={(e) => setPrompt(e.target.value)} />
        </Field>
        {result && (
          <div className="rounded-lg bg-success/5 p-3 space-y-1 text-sm">
            <div className="flex items-center gap-2 font-semibold text-success"><CheckCircle2 className="h-4 w-4" />测试成功 ({result.durationMs}ms)</div>
            <div className="text-xs text-muted-foreground">模型：{testModelLabel(result.model)}</div>
            <div className="mt-1 text-xs whitespace-pre-wrap break-words text-foreground">{result.response}</div>
          </div>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose}>关闭</Button>
          <Button size="sm" onClick={run} disabled={testMutation.isPending}>
            {testMutation.isPending ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Play className="h-3.5 w-3.5" />}
            {testMutation.isPending ? '测试中…' : '开始测试'}
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}

// ============================================================================
// BatchVerifyModal
// ============================================================================

export function BatchVerifyModal({ open, verifying, progress, results, testModel, onCancel, onClose }: {
  open: boolean; verifying: boolean
  progress: { current: number; total: number }
  results: Map<number, VerifyResult>
  testModel: string
  onCancel: () => void; onClose: () => void
}) {
  const items = Array.from(results.values())
  const successCount = items.filter((r) => r.status === 'success').length
  const failedCount = items.filter((r) => r.status === 'failed').length
  const pct = progress.total > 0 ? Math.round((progress.current / progress.total) * 100) : 0

  const icon = (s: VerifyResult['status']) => {
    if (s === 'success') return <CheckCircle2 className="h-3.5 w-3.5 text-success" />
    if (s === 'failed') return <XCircle className="h-3.5 w-3.5 text-destructive" />
    if (s === 'verifying') return <Loader2 className="h-3.5 w-3.5 animate-spin text-primary" />
    return <div className="h-3.5 w-3.5 rounded-full bg-muted" />
  }

  return (
    <ModalShell open={open} title="批量验活" width="max-w-lg" onClose={onClose}>
      <div className="space-y-3">
        <div className="rounded-lg bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
          测试模型：<span className="font-semibold text-foreground">{testModelLabel(testModel)}</span>
          <span className="ml-2 text-muted-foreground/70">（批量验活使用当前模型列表的默认模型）</span>
        </div>
        {verifying && (
          <div className="space-y-1.5">
            <div className="flex justify-between text-xs text-muted-foreground">
              <span>进度 {progress.current}/{progress.total}</span>
              <span>{pct}%</span>
            </div>
            <Progress value={pct} className="h-1.5" />
          </div>
        )}
        {!verifying && items.length > 0 && (
          <div className="text-sm">
            验活完成：成功 <span className="font-semibold text-success">{successCount}</span>，失败 <span className="font-semibold text-destructive">{failedCount}</span>
          </div>
        )}
        <div className="max-h-72 overflow-y-auto scrollbar-thin space-y-1">
          {items.map((r) => (
            <div key={r.id} className="flex items-start gap-2 rounded-lg bg-muted/30 px-3 py-2">
              <div className="mt-0.5 shrink-0">{icon(r.status)}</div>
              <div className="min-w-0 flex-1 text-xs">
                <div className="font-semibold">账号 #{r.id}</div>
                {r.status === 'success' && r.response && <div className="text-muted-foreground truncate">{r.model}: {r.response.slice(0, 60)}</div>}
                {r.error && <div className="text-destructive">{r.error}</div>}
              </div>
            </div>
          ))}
        </div>
        <div className="flex justify-end gap-2">
          {verifying ? (
            <>
              <Button variant="ghost" size="sm" onClick={onClose}>后台运行</Button>
              <Button variant="outline" size="sm" onClick={onCancel}>取消</Button>
            </>
          ) : (
            <Button size="sm" onClick={onClose}>关闭</Button>
          )}
        </div>
      </div>
    </ModalShell>
  )
}

// ============================================================================
// CredentialExportModal
// ============================================================================

/** 状态快捷导出：按账号页顶部状态条的同一套规则筛选。 */
export const CREDENTIAL_EXPORT_STATUS_SCOPES = [
  { status: 'normal', label: '全部正常账号' },
  { status: 'unavailable', label: '全部不可用账号（禁用 / 额度用尽 / 账号异常）' },
  { status: 'account_error', label: '全部账号异常' },
  { status: 'quota_exhausted', label: '全部额度用尽' },
  { status: 'cooling', label: '全部冷却账号' },
  { status: 'disabled', label: '全部已禁用账号' },
] as const

export type CredentialExportStatus = (typeof CREDENTIAL_EXPORT_STATUS_SCOPES)[number]['status']

type ExportScopeValue = 'selected' | 'filtered' | 'all' | `status:${CredentialExportStatus}`

export function CredentialExportModal({
  open,
  onClose,
  selectedIds = [],
  totalCount,
  filter,
  filteredCount,
  statusCounts,
}: {
  open: boolean
  onClose: () => void
  selectedIds?: number[]
  totalCount: number
  /** 账号列表当前的筛选条件；没有筛选时不传。 */
  filter?: CredentialExportFilter
  filteredCount: number
  statusCounts: Partial<Record<CredentialExportStatus, number>>
}) {
  const [format, setFormat] = useState<CredentialExportFormat>('json')
  const [scope, setScope] = useState<ExportScopeValue>('all')
  const [loading, setLoading] = useState(false)
  const selectedCount = selectedIds.length

  useEffect(() => {
    if (open) setScope(selectedCount > 0 ? 'selected' : filter ? 'filtered' : 'all')
  }, [open, selectedCount, filter])

  const scopeLabel = (value: ExportScopeValue) => {
    if (value === 'selected') return '选中账号'
    if (value === 'filtered') return '当前筛选结果'
    if (value === 'all') return '全部账号'
    const status = value.slice('status:'.length)
    return CREDENTIAL_EXPORT_STATUS_SCOPES.find((item) => item.status === status)?.label ?? value
  }

  const handleExport = async () => {
    setLoading(true)
    try {
      const exportScope: CredentialExportScope = scope === 'selected'
        ? { ids: selectedIds }
        : scope === 'filtered'
          ? { filter: filter ?? {} }
          : scope === 'all'
            ? { filter: {} }
            : { filter: { status: scope.slice('status:'.length) } }
      const { blob, count } = await exportCredentials(format, exportScope)
      downloadBlob(blob, exportFilename(format))
      toast.success(count === null ? `已导出${scopeLabel(scope)}` : `已导出${scopeLabel(scope)} ${count} 个`)
      onClose()
    } catch (e) {
      toast.error(`导出失败: ${extractErrorMessage(e)}`)
    } finally {
      setLoading(false)
    }
  }

  return (
    <ModalShell open={open} title="导出账号" width="max-w-md" onClose={onClose}>
      <div className="space-y-4">
        <Field label="导出格式">
          <Select value={format} onValueChange={(v) => setFormat(v as CredentialExportFormat)} disabled={loading}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="json">JSON 数组</SelectItem>
              <SelectItem value="backup-json">完整备份 JSON</SelectItem>
              <SelectItem value="jsonl">JSONL（每行一个）</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="导出范围" description="只导出账号配置，不含运行状态；状态按点击导出时的实时结果计算。">
          <Select value={scope} onValueChange={(v) => setScope(v as ExportScopeValue)} disabled={loading}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              {selectedCount > 0 && <SelectItem value="selected">选中账号（{selectedCount} 个）</SelectItem>}
              {filter && (
                <SelectItem value="filtered" disabled={filteredCount === 0}>当前筛选结果（{filteredCount} 个）</SelectItem>
              )}
              <SelectItem value="all">全部账号（{totalCount} 个）</SelectItem>
              <SelectSeparator />
              <SelectGroup>
                <SelectLabel>按状态</SelectLabel>
                {CREDENTIAL_EXPORT_STATUS_SCOPES.map(({ status, label }) => {
                  const count = statusCounts[status]
                  return (
                    <SelectItem key={status} value={`status:${status}`} disabled={count === 0}>
                      {label}{count === undefined ? '' : `（${count} 个）`}
                    </SelectItem>
                  )
                })}
              </SelectGroup>
            </SelectContent>
          </Select>
        </Field>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose} disabled={loading}>取消</Button>
          <Button size="sm" onClick={handleExport} disabled={loading}>
            {loading ? <Spinner size="sm" /> : <Download className="h-3.5 w-3.5" />}导出
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}
