import {
  AlertCircle,
  CheckCircle2,
  Download,
  FileUp,
  Loader2,
  Play,

  XCircle,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import {
  addCredential,
  deleteCredential,
  exportCredentials,
  getCredentialBalance,
  setCredentialDisabled,
  testCredential,
} from '@/api/credentials'
import {
  Button,
  Checkbox,
  Input,
  Progress,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
  Textarea,
} from '@/components/ui'
import { Field, FieldGrid, ModalShell } from '@/components/patterns'
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import { parseKamFiles, parseKamJson, type KamAccount } from '@/lib/kam-import'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage, sha256Hex } from '@/lib/utils'
import { useAddCredential, useBatchUpdateCredentials, useProxyResources, useTestCredential } from '@/hooks/use-credentials'
import type {
  AddCredentialRequest,
  BatchUpdateCredentialsRequest,
  CredentialExportFormat,
  CredentialStatusItem,
  ProxyResource,
  TestCredentialResponse,
} from '@/types/api'
import { SecretInput } from './credential-inputs'

// ============================================================================
// ImportProgressList — 共用进度列表（BatchImportModal + KamImportModal 共用）
// ============================================================================

function statusIcon(s: ImportResult['status']) {
  if (s === 'success') return <CheckCircle2 className="h-4 w-4 text-success" />
  if (s === 'failed') return <XCircle className="h-4 w-4 text-destructive" />
  if (s === 'importing' || s === 'verifying') return <Loader2 className="h-4 w-4 animate-spin text-primary" />
  if (s === 'skipped') return <AlertCircle className="h-4 w-4 text-warning" />
  return <div className="h-4 w-4 rounded-full bg-muted" />
}

function ImportProgressList({
  results,
  getLabel,
}: {
  results: ImportResult[]
  /** 根据条目索引返回显示标签 */
  getLabel: (index: number) => string
}) {
  return (
    <div className="max-h-72 overflow-y-auto scrollbar-thin space-y-1">
      {results.map((r, i) => (
        <div key={i} className="flex items-start gap-2 rounded-lg bg-muted/30 px-3 py-2">
          <div className="mt-0.5 shrink-0">{statusIcon(r.status)}</div>
          <div className="min-w-0 flex-1 text-xs">
            <div className="font-semibold truncate">{getLabel(i)}</div>
            {r.status === 'success' && r.model && (
              <div className="text-muted-foreground truncate">{r.model}: {r.response?.slice(0, 60)}</div>
            )}
            {r.error && <div className="text-destructive">{r.error}</div>}
            {r.status === 'verifying' && <div className="text-primary">验活中…</div>}
            {r.status === 'importing' && <div className="text-primary">导入中…</div>}
          </div>
        </div>
      ))}
    </div>
  )
}

// ============================================================================
// ImportResultFooter — 共用底部操作栏（关闭/开始/重试/完成）
// ============================================================================

function ImportResultFooter({
  running,
  results,
  failedCount,
  onClose,
  onRun,
  onRetry,
}: {
  running: boolean
  results: ImportResult[]
  failedCount: number
  onClose: () => void
  onRun?: () => void
  onRetry?: () => void
}) {
  const hasPending = results.some((r) => r.status === 'pending')
  const allDone = results.length > 0 && results.every((r) => r.status !== 'pending')
  return (
    <div className="flex justify-end gap-2">
      <Button variant="ghost" size="sm" onClick={onClose} disabled={running}>关闭</Button>
      {!running && hasPending && onRun && (
        <Button size="sm" onClick={onRun}>
          <Play className="h-3.5 w-3.5" />开始导入
        </Button>
      )}
      {!running && failedCount > 0 && onRetry && (
        <Button size="sm" onClick={onRetry}>
          <AlertCircle className="h-3.5 w-3.5" />重试失败账号 ({failedCount})
        </Button>
      )}
      {!running && allDone && failedCount === 0 && (
        <Button size="sm" onClick={onClose}>完成</Button>
      )}
    </div>
  )
}

type AuthMethod = 'social' | 'idc' | 'external_idp' | 'api_key'
type ImportVerificationMode = 'model_and_subscription' | 'subscription_only'

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

interface CredentialParameterDefaults {
  disabled: string
  priority: string
  maxConcurrentRequests: string
  rpm: string
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

function initialParameterDefaults(): CredentialParameterDefaults {
  return { disabled: 'false', priority: '', maxConcurrentRequests: '', rpm: '', region: '', authRegion: '', apiRegion: '', machineId: '', endpoint: '', proxyResourceId: '', proxyUrl: '', proxyUsername: '', proxyPassword: '' }
}

function initialCredentialForm() {
  return { authMethod: 'social' as AuthMethod, refreshToken: '', kiroApiKey: '', profileArn: '', region: '', authRegion: '', apiRegion: '', clientId: '', clientSecret: '', tokenEndpoint: '', issuerUrl: '', scopes: '', email: '', priority: '0', maxConcurrentRequests: '', disabled: 'false', machineId: '', proxyUrl: '', proxyUsername: '', proxyPassword: '', proxyResourceId: '', endpoint: '' }
}

function formFromCredential(c: AddCredentialRequest) {
  return {
    ...initialCredentialForm(),
    authMethod: (c.authMethod || (c.kiroApiKey ? 'api_key' : c.clientId && c.clientSecret ? 'idc' : 'social')) as AuthMethod,
    refreshToken: c.refreshToken || '', kiroApiKey: c.kiroApiKey || '', profileArn: c.profileArn || '',
    region: c.region || '', authRegion: c.authRegion || '', apiRegion: c.apiRegion || '',
    clientId: c.clientId || '', clientSecret: c.clientSecret || '', tokenEndpoint: c.tokenEndpoint || '',
    issuerUrl: c.issuerUrl || '', scopes: c.scopes || '', email: c.email || '',
    priority: String(c.priority ?? 0),
    maxConcurrentRequests: typeof c.maxConcurrentRequests === 'number' ? String(c.maxConcurrentRequests) : '',
    disabled: c.disabled ? 'true' : 'false',
    machineId: c.machineId || '', proxyUrl: c.proxyUrl || '', proxyUsername: c.proxyUsername || '',
    proxyPassword: c.proxyPassword || '', proxyResourceId: c.proxyResourceId ? String(c.proxyResourceId) : '',
    endpoint: c.endpoint || '',
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

function normalizedKamAuthMethod(
  value: unknown,
  clientId?: string,
  clientSecret?: string
): AddCredentialRequest['authMethod'] {
  const compact = optionalTrimmed(value)?.toLowerCase().replace(/[^a-z0-9]/g, '')
  if (compact === 'externalidp' || compact === 'enterprise' || compact === 'iamsso' || compact === 'awsidc') {
    return 'external_idp'
  }
  if (compact === 'idc' || compact === 'builderid' || compact === 'iam' || (clientId && clientSecret)) {
    return 'idc'
  }
  if (compact === 'apikey') return 'api_key'
  return 'social'
}

function parseOptionalNonNegativeInteger(value: string, label: string): number | undefined {
  const t = value.trim()
  if (!t) return undefined
  const n = Number(t)
  if (!Number.isInteger(n) || n < 0) throw new Error(`${label}必须是非负整数`)
  return n
}

function mergeCredentialDefaults(cred: AddCredentialRequest, defaults: CredentialParameterDefaults): AddCredentialRequest {
  const defaultProxyResourceId = parseOptionalNonNegativeInteger(defaults.proxyResourceId, '代理资源 ID')
  const hasDirectProxy = Boolean(optionalTrimmed(cred.proxyUrl) || optionalTrimmed(cred.proxyUsername) || optionalTrimmed(cred.proxyPassword))
  const proxyResourceId = typeof cred.proxyResourceId !== 'undefined' ? cred.proxyResourceId : hasDirectProxy ? undefined : defaultProxyResourceId
  const useProxyResource = typeof proxyResourceId === 'number'
  return {
    ...cred,
    disabled: typeof cred.disabled === 'undefined' || cred.disabled === null ? defaults.disabled === 'true' : cred.disabled,
    priority: cred.priority ?? parseOptionalNonNegativeInteger(defaults.priority, '默认优先级'),
    maxConcurrentRequests: typeof cred.maxConcurrentRequests === 'undefined' ? parseOptionalNonNegativeInteger(defaults.maxConcurrentRequests, '默认账号并发') : cred.maxConcurrentRequests,
    rpm: typeof cred.rpm === 'undefined' ? parseOptionalNonNegativeInteger(defaults.rpm, '默认账号 RPM') : cred.rpm,
    region: optionalTrimmed(cred.region) || optionalTrimmed(defaults.region),
    authRegion: optionalTrimmed(cred.authRegion) || optionalTrimmed(defaults.authRegion),
    apiRegion: optionalTrimmed(cred.apiRegion) || optionalTrimmed(defaults.apiRegion),
    machineId: optionalTrimmed(cred.machineId) || optionalTrimmed(defaults.machineId),
    endpoint: optionalTrimmed(cred.endpoint) || optionalTrimmed(defaults.endpoint),
    proxyResourceId,
    proxyUrl: optionalTrimmed(cred.proxyUrl) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyUrl)),
    proxyUsername: optionalTrimmed(cred.proxyUsername) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyUsername)),
    proxyPassword: optionalTrimmed(cred.proxyPassword) || (useProxyResource ? undefined : optionalTrimmed(defaults.proxyPassword)),
  }
}

async function verifyImportedCredential(credentialId: number, mode: ImportVerificationMode): Promise<{ model: string; response: string }> {
  if (mode === 'subscription_only') {
    const info = await getCredentialBalance(credentialId)
    return { model: '订阅查询', response: `订阅: ${info.subscriptionTitle || '未知'}，用量 ${info.currentUsage}/${info.usageLimit}` }
  }
  const tested = await testCredential(credentialId, { model: DEFAULT_TEST_MODEL, prompt: DEFAULT_TEST_PROMPT })
  try { await getCredentialBalance(credentialId) } catch { /* ignore */ }
  return { model: testModelLabel(tested.model), response: tested.response }
}

async function rollbackCredential(id: number): Promise<{ success: boolean; error?: string }> {
  try { await setCredentialDisabled(id, true) } catch (e) { return { success: false, error: `禁用失败: ${extractErrorMessage(e)}` } }
  try { await deleteCredential(id); return { success: true } } catch (e) { return { success: false, error: `删除失败: ${extractErrorMessage(e)}` } }
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
// CredentialParameterDefaultsPanel
// ============================================================================

function CredentialParameterDefaultsPanel({ defaults, onChange, proxyResources, disabled }: {
  defaults: CredentialParameterDefaults; onChange: (d: CredentialParameterDefaults) => void
  proxyResources: ProxyResource[]; disabled?: boolean
}) {
  const [showPu, setShowPu] = useState(false)
  const [showPp, setShowPp] = useState(false)
  const update = (key: keyof CredentialParameterDefaults, value: string) => {
    if (key === 'proxyResourceId' && value && value !== '__none__') { onChange({ ...defaults, proxyResourceId: value, proxyUrl: '', proxyUsername: '', proxyPassword: '' }); return }
    if ((key === 'proxyUrl' || key === 'proxyUsername' || key === 'proxyPassword') && value.trim()) { onChange({ ...defaults, [key]: value, proxyResourceId: '' }); return }
    if (key === 'region' && value.trim() && !defaults.authRegion.trim()) { onChange({ ...defaults, region: value, authRegion: value }); return }
    onChange({ ...defaults, [key]: value })
  }
  const proxyLocked = Boolean(defaults.proxyResourceId)
  return (
    <div className="rounded-lg bg-muted/30 p-3">
      <div className="mb-3 flex items-start justify-between gap-3">
        <div>
          <div className="text-sm font-semibold">默认参数</div>
          <div className="mt-0.5 text-xs text-muted-foreground">只填充每条账号中缺失的字段。</div>
        </div>
        <Button type="button" variant="ghost" size="xs" disabled={disabled} onClick={() => onChange(initialParameterDefaults())}>清空</Button>
      </div>
      <FieldGrid>
        <Field label="导入后状态" description="账号自身 disabled 字段优先">
          <Select value={defaults.disabled} onValueChange={(v) => update('disabled', v)} disabled={disabled}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="false">启用</SelectItem>
              <SelectItem value="true">禁用</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="默认优先级"><Input type="number" min={0} value={defaults.priority} disabled={disabled} onChange={(e) => update('priority', e.target.value)} /></Field>
        <Field label="默认账号并发" description="留空继承全局，0 不限"><Input type="number" min={0} value={defaults.maxConcurrentRequests} disabled={disabled} onChange={(e) => update('maxConcurrentRequests', e.target.value)} /></Field>
        <Field label="默认账号 RPM" description="留空继承全局，0 不限"><Input type="number" min={0} value={defaults.rpm} disabled={disabled} onChange={(e) => update('rpm', e.target.value)} /></Field>
        <Field label="Region 兼容"><Input className="font-mono" value={defaults.region} disabled={disabled} onChange={(e) => update('region', e.target.value)} placeholder="us-east-1" /></Field>
        <Field label="Auth Region"><Input className="font-mono" value={defaults.authRegion} disabled={disabled} onChange={(e) => update('authRegion', e.target.value)} placeholder="us-east-1" /></Field>
        <Field label="API Region"><Input className="font-mono" value={defaults.apiRegion} disabled={disabled} onChange={(e) => update('apiRegion', e.target.value)} placeholder="us-east-1" /></Field>
        <Field label="Machine ID"><Input value={defaults.machineId} disabled={disabled} onChange={(e) => update('machineId', e.target.value)} /></Field>
        <Field label="端点"><Input value={defaults.endpoint} disabled={disabled} onChange={(e) => update('endpoint', e.target.value)} placeholder="ide / cli" /></Field>
        <Field label="代理资源">
          <Select value={defaults.proxyResourceId || '__none__'} onValueChange={(v) => update('proxyResourceId', v === '__none__' ? '' : v)} disabled={disabled}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="__none__">不绑定</SelectItem>
              {proxyResources.map((r) => <SelectItem key={r.id} value={String(r.id)}>{r.name}</SelectItem>)}
            </SelectContent>
          </Select>
        </Field>
        <Field label="直连代理 URL"><Input value={defaults.proxyUrl} disabled={disabled || proxyLocked} onChange={(e) => update('proxyUrl', e.target.value)} placeholder="socks5h://..." /></Field>
        <Field label="代理用户名"><SecretInput value={defaults.proxyUsername} onChange={(v) => update('proxyUsername', v)} visible={showPu} onToggle={() => setShowPu((v) => !v)} disabled={disabled || proxyLocked} placeholder="可选" /></Field>
        <Field label="代理密码"><SecretInput value={defaults.proxyPassword} onChange={(v) => update('proxyPassword', v)} visible={showPp} onToggle={() => setShowPp((v) => !v)} disabled={disabled || proxyLocked} placeholder="可选" /></Field>
      </FieldGrid>
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
  const proxyResources = useProxyResources()
  const proxyOptions = (proxyResources.data?.resources || []).filter((r) => r.enabled)
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
          clientSecret: am === 'idc' ? prev.clientSecret : '',
          tokenEndpoint: am === 'external_idp' ? prev.tokenEndpoint : '',
          issuerUrl: am === 'external_idp' ? prev.issuerUrl : '',
          scopes: am === 'external_idp' ? prev.scopes : '',
        }
      }
      if (key === 'region' && value.trim() && !prev.authRegion.trim()) return { ...prev, region: value, authRegion: value }
      if (key === 'proxyResourceId' && value && value !== '__none__') return { ...prev, proxyResourceId: value, proxyUrl: '', proxyUsername: '', proxyPassword: '' }
      if ((key === 'proxyUrl' || key === 'proxyUsername' || key === 'proxyPassword') && value.trim()) return { ...prev, [key]: value, proxyResourceId: '' }
      return { ...prev, [key]: value }
    })

  const handleFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files || [])
    e.target.value = ''
    if (!files.length) return
    const result = await parseCredentialImportFiles(files)
    if (!result.credentials[0]) { toast.error(result.errors[0] || '文件中没有有效账号'); return }
    setForm(formFromCredential(result.credentials[0]))
    toast.success(`已填充第一条账号${result.credentials.length > 1 ? `，另有 ${result.credentials.length - 1} 条可批量导入` : ''}`)
    if (result.errors.length) toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
  }

  const submit = (e: React.FormEvent) => {
    e.preventDefault()
    if (isApiKey && !form.kiroApiKey.trim()) return toast.error('请输入 Kiro API Key')
    if (!isApiKey && !form.refreshToken.trim()) return toast.error('请输入 Refresh Token')
    if (form.authMethod === 'idc' && (!form.clientId.trim() || !form.clientSecret.trim())) return toast.error('IdC 认证需要 Client ID 和 Client Secret')
    if (form.authMethod === 'external_idp' && !form.clientId.trim()) return toast.error('External IdP 认证需要 Client ID')
    const priority = Number(form.priority)
    if (!Number.isInteger(priority) || priority < 0) return toast.error('优先级必须是非负整数')
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
      clientSecret: form.authMethod === 'idc' ? form.clientSecret.trim() || undefined : undefined,
      tokenEndpoint: form.authMethod === 'external_idp' ? form.tokenEndpoint.trim() || undefined : undefined,
      issuerUrl: form.authMethod === 'external_idp' ? form.issuerUrl.trim() || undefined : undefined,
      scopes: form.authMethod === 'external_idp' ? form.scopes.trim() || undefined : undefined,
      email: form.email.trim() || undefined,
      priority,
      maxConcurrentRequests,
      disabled: form.disabled === 'true',
      machineId: form.machineId.trim() || undefined,
      proxyResourceId: form.proxyResourceId ? Number(form.proxyResourceId) : undefined,
      proxyUrl: form.proxyUrl.trim() || undefined,
      proxyUsername: form.proxyUsername.trim() || undefined,
      proxyPassword: form.proxyPassword.trim() || undefined,
      endpoint: form.endpoint.trim() || undefined,
    }, {
      onSuccess: async (data) => {
        try {
          const info = await getCredentialBalance(data.credentialId)
          toast.success(`${data.message}，订阅: ${info.subscriptionTitle || '未知'}`)
        } catch (err) {
          toast.warning(`${data.message}，但查询订阅失败: ${extractErrorMessage(err)}`)
        }
        onClose()
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
            <input type="file" className="sr-only" accept=".json,.jsonl,.txt" multiple onChange={handleFile} />
            <Button type="button" variant="outline" size="sm" asChild><span><FileUp className="h-3.5 w-3.5" />从文件填充</span></Button>
          </label>
        </div>

        <FieldGrid>
          {isApiKey ? (
            <Field label="Kiro API Key" className="col-span-2">
              <SecretInput value={form.kiroApiKey} onChange={(v) => update('kiroApiKey', v)} visible={showPu} onToggle={() => setShowPu((v) => !v)} placeholder="sk-..." disabled={add.isPending} />
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
            <Field label="Token Endpoint"><Input className="font-mono" value={form.tokenEndpoint} disabled={add.isPending} onChange={(e) => update('tokenEndpoint', e.target.value)} placeholder="https://.../oauth2/v2.0/token" /></Field>
            <Field label="Issuer URL（可选）"><Input className="font-mono" value={form.issuerUrl} disabled={add.isPending} onChange={(e) => update('issuerUrl', e.target.value)} placeholder="https://..." /></Field>
            <Field label="Scopes（可选）"><Input className="font-mono" value={form.scopes} disabled={add.isPending} onChange={(e) => update('scopes', e.target.value)} placeholder="offline_access ..." /></Field>
          </>}
          <Field label="邮箱（可选）"><Input value={form.email} disabled={add.isPending} onChange={(e) => update('email', e.target.value)} placeholder="user@example.com" /></Field>
          <Field label="Profile ARN（可选）"><Input value={form.profileArn} disabled={add.isPending} onChange={(e) => update('profileArn', e.target.value)} /></Field>
          <Field label="优先级"><Input type="number" min={0} value={form.priority} disabled={add.isPending} onChange={(e) => update('priority', e.target.value)} /></Field>
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
// BatchImportModal
// ============================================================================

interface ImportResult {
  index: number
  credentialId?: number
  email?: string
  status: 'pending' | 'importing' | 'verifying' | 'success' | 'failed' | 'skipped'
  error?: string
  model?: string
  response?: string
}

export function BatchImportModal({ open, onClose, existingCredentials, onDone }: {
  open: boolean; onClose: () => void
  existingCredentials: Array<{ id: number; refreshTokenHash?: string; apiKeyHash?: string }>
  onDone: () => void
}) {
  const [text, setText] = useState('')
  const [defaults, setDefaults] = useState(initialParameterDefaults)
  const [verifyMode, setVerifyMode] = useState<ImportVerificationMode>('subscription_only')
  const [skipVerify, setSkipVerify] = useState(false)
  const [running, setRunning] = useState(false)
  const [results, setResults] = useState<ImportResult[]>([])
  const [parsed, setParsed] = useState<AddCredentialRequest[]>([])
  const [parseError, setParseError] = useState('')
  const proxyResources = useProxyResources()
  const proxyOptions = proxyResources.data?.resources || []
  const cancelRef = useRef(false)

  useEffect(() => {
    if (!open) { setText(''); setDefaults(initialParameterDefaults()); setVerifyMode('subscription_only'); setSkipVerify(false); setResults([]); setParsed([]); setParseError(''); setRunning(false) }
  }, [open])

  const handleParse = () => {
    setParseError('')
    try {
      const items = parseCredentialImportText(text)
      if (!items.length) { setParseError('未找到有效账号，请检查格式'); return }
      const merged = items.map((c) => mergeCredentialDefaults(c, defaults))
      setParsed(merged)
      setResults(merged.map((_, i) => ({ index: i, status: 'pending' })))
    } catch (e) {
      setParseError(extractErrorMessage(e))
    }
  }

  const handleFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files || [])
    e.target.value = ''
    if (!files.length) return
    const result = await parseCredentialImportFiles(files)
    if (!result.credentials.length) { toast.error(result.errors[0] || '文件中没有有效账号'); return }
    const merged = result.credentials.map((c) => mergeCredentialDefaults(c, defaults))
    setParsed(merged)
    setResults(merged.map((_, i) => ({ index: i, status: 'pending' })))
    setText(merged.map((c) => JSON.stringify(c)).join('\n'))
    toast.success(`已解析 ${merged.length} 条账号`)
    if (result.errors.length) toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
  }

  const existingHashes = useMemo(() => {
    const hashes = new Set<string>()
    for (const c of existingCredentials) {
      if (c.refreshTokenHash) hashes.add(c.refreshTokenHash)
      if (c.apiKeyHash) hashes.add(c.apiKeyHash)
    }
    return hashes
  }, [existingCredentials])

  const isDuplicate = async (cred: AddCredentialRequest): Promise<boolean> => {
    if (cred.refreshToken) {
      const hash = await sha256Hex(cred.refreshToken)
      return existingHashes.has(hash)
    }
    if (cred.kiroApiKey) {
      const hash = await sha256Hex(cred.kiroApiKey)
      return existingHashes.has(hash)
    }
    return false
  }

  const runItems = async (items: AddCredentialRequest[], isRetry = false) => {
    setRunning(true); cancelRef.current = false
    const newResults: ImportResult[] = items.map((_, i) => ({ index: i, status: 'pending' }))
    if (!isRetry) setResults([...newResults])

    for (let i = 0; i < items.length; i++) {
      if (cancelRef.current) break
      const cred = items[i]
      newResults[i] = { ...newResults[i], status: 'importing' }
      if (!isRetry) setResults([...newResults])
      try {
        const dup = !isRetry && await isDuplicate(cred)
        if (dup) { newResults[i] = { ...newResults[i], status: 'skipped', error: '重复账号' }; if (!isRetry) setResults([...newResults]); continue }
        const res = await addCredential(cred)
        newResults[i] = { ...newResults[i], credentialId: res.credentialId, email: res.email }
        if (!skipVerify) {
          newResults[i].status = 'verifying'
          if (!isRetry) setResults([...newResults])
          try {
            const verified = await verifyImportedCredential(res.credentialId, verifyMode)
            newResults[i] = { ...newResults[i], status: 'success', model: verified.model, response: verified.response }
          } catch (ve) {
            await rollbackCredential(res.credentialId)
            newResults[i] = { ...newResults[i], status: 'failed', error: `验活失败: ${extractErrorMessage(ve)}` }
          }
        } else {
          newResults[i].status = 'success'
        }
      } catch (e) {
        newResults[i] = { ...newResults[i], status: 'failed', error: extractErrorMessage(e) }
      }
      if (!isRetry) setResults([...newResults])
    }

    if (isRetry) {
      // merge retry outcomes back into the full results list
      setResults((prev) => {
        let retryIdx = 0
        return prev.map((r) => {
          if (r.status === 'failed' && retryIdx < newResults.length) {
            return { ...newResults[retryIdx++], index: r.index }
          }
          return r
        })
      })
    }

    setRunning(false)
    const success = newResults.filter((r) => r.status === 'success').length
    const failed = newResults.filter((r) => r.status === 'failed').length
    const skipped = newResults.filter((r) => r.status === 'skipped').length
    onDone()
    toast.success(`批量导入完成：成功 ${success}，失败 ${failed}，跳过 ${skipped}`)
  }

  const run = () => { if (parsed.length) runItems(parsed) }

  const failedItems = parsed.filter((_, i) => results[i]?.status === 'failed')
  const retryFailed = () => { if (failedItems.length) runItems(failedItems, true) }

  return (
    <ModalShell open={open} title="批量导入账号" width="max-w-3xl" onClose={onClose}>
      <div className="space-y-4">
        {!results.length ? (
          <>
            <div className="flex items-center gap-2">
              <label className="cursor-pointer">
                <input type="file" className="sr-only" accept=".json,.jsonl,.txt" multiple onChange={handleFile} />
                <Button type="button" variant="outline" size="sm" asChild><span><FileUp className="h-3.5 w-3.5" />从文件导入</span></Button>
              </label>
              <span className="text-xs text-muted-foreground">或粘贴 JSON/JSONL 到下方</span>
            </div>
            <Textarea
              className="min-h-[140px] font-mono text-xs"
              placeholder={'[{"refreshToken":"eyJ..."},{"refreshToken":"eyJ..."}]\n// 或每行一个 JSON 对象'}
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
            {parseError && <div className="text-xs text-destructive">{parseError}</div>}
            <CredentialParameterDefaultsPanel defaults={defaults} onChange={setDefaults} proxyResources={proxyOptions} />
            <div className="rounded-lg bg-muted/30 p-3 space-y-2">
              <div className="text-sm font-semibold">验活方式</div>
              <div className="flex items-center gap-3">
                <Checkbox checked={skipVerify} onCheckedChange={(v) => setSkipVerify(Boolean(v))} id="skip-verify" />
                <label htmlFor="skip-verify" className="text-sm cursor-pointer">跳过验活（直接导入，不测试）</label>
              </div>
              {!skipVerify && (
                <Select value={verifyMode} onValueChange={(v) => setVerifyMode(v as ImportVerificationMode)}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="subscription_only">只查询订阅（不请求模型）</SelectItem>
                    <SelectItem value="model_and_subscription">测试模型 + 查询订阅</SelectItem>
                  </SelectContent>
                </Select>
              )}
            </div>
            <div className="flex justify-end gap-2">
              <Button variant="ghost" size="sm" onClick={onClose}>取消</Button>
              <Button size="sm" onClick={handleParse} disabled={!text.trim()}>解析预览</Button>
            </div>
          </>
        ) : (
          <>
            <div className="flex items-center justify-between gap-3">
              <div className="text-sm">
                共 <span className="font-semibold">{parsed.length}</span> 条
                {results.some((r) => r.status !== 'pending') && (
                  <span className="ml-2 text-muted-foreground">
                    成功 {results.filter((r) => r.status === 'success').length} · 失败 {results.filter((r) => r.status === 'failed').length} · 跳过 {results.filter((r) => r.status === 'skipped').length}
                  </span>
                )}
              </div>
              {!running && <Button variant="ghost" size="xs" onClick={() => { setResults([]); setParsed([]) }}>重新编辑</Button>}
            </div>
            {running && <Progress value={Math.round((results.filter((r) => r.status !== 'pending').length / parsed.length) * 100)} className="h-1.5" />}
            <ImportProgressList
              results={results}
              getLabel={(i) => parsed[i]?.email || parsed[i]?.kiroApiKey?.slice(0, 20) || `账号 ${i + 1}`}
            />
            <ImportResultFooter
              running={running}
              results={results}
              failedCount={failedItems.length}
              onClose={onClose}
              onRun={run}
              onRetry={retryFailed}
            />
          </>
        )}
      </div>
    </ModalShell>
  )
}

// ============================================================================
// KamImportModal
// ============================================================================

export function KamImportModal({ open, onClose, onDone }: {
  open: boolean; onClose: () => void
  existingCredentials?: Array<{ id: number; refreshTokenHash?: string; apiKeyHash?: string }>
  onDone: () => void
}) {
  const [text, setText] = useState('')
  const [accounts, setAccounts] = useState<KamAccount[]>([])
  const [skipErrorAccounts, setSkipErrorAccounts] = useState(true)
  const [verifyMode, setVerifyMode] = useState<ImportVerificationMode>('subscription_only')
  const [defaults, setDefaults] = useState(initialParameterDefaults)
  const [running, setRunning] = useState(false)
  const [results, setResults] = useState<ImportResult[]>([])
  const proxyResources = useProxyResources()
  const proxyOptions = proxyResources.data?.resources || []

  const hasErrorAccounts = accounts.some((a) => a.status === 'error')

  useEffect(() => {
    if (!open) {
      setText(''); setAccounts([]); setResults([])
      setDefaults(initialParameterDefaults()); setRunning(false)
      setSkipErrorAccounts(true); setVerifyMode('subscription_only')
    }
  }, [open])

  const handleParse = () => {
    try {
      const result = parseKamJson(text)
      if (!result.length) { toast.error('未找到有效的 KAM 账号'); return }
      setAccounts(result)
      setResults(result.map((_, i) => ({ index: i, status: 'pending' })))
      toast.success(`解析到 ${result.length} 个 KAM 账号`)
    } catch (e) { toast.error(`解析失败: ${extractErrorMessage(e)}`) }
  }

  const handleFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files || [])
    e.target.value = ''
    if (!files.length) return
    try {
      const result = await parseKamFiles(files)
      if (!result.accounts.length) { toast.error('文件中未找到有效 KAM 账号'); return }
      setAccounts(result.accounts)
      setResults(result.accounts.map((_acc, i) => ({ index: i, status: 'pending' })))
      toast.success(`从文件解析到 ${result.accounts.length} 个账号`)
    } catch (e) { toast.error(`文件解析失败: ${extractErrorMessage(e)}`) }
  }

  const runAccounts = async (targetAccounts: KamAccount[], isRetry = false) => {
    setRunning(true)
    const newResults: ImportResult[] = targetAccounts.map((_, i) => ({ index: i, status: 'pending' }))
    if (!isRetry) setResults([...newResults])
    let skippedCount = 0
    for (let i = 0; i < targetAccounts.length; i++) {
      const acc = targetAccounts[i]
      // skip error-status accounts if option enabled (only on first run, not retry)
      if (!isRetry && skipErrorAccounts && acc.status === 'error') {
        newResults[i] = { ...newResults[i], status: 'skipped', error: '跳过 error 状态账号' }
        setResults([...newResults]); skippedCount++; continue
      }
      newResults[i] = { ...newResults[i], status: 'importing' }
      setResults([...newResults])
      try {
        const clientId = optionalTrimmed(acc.credentials.clientId)
        const clientSecret = optionalTrimmed(acc.credentials.clientSecret)
        const authMethod = normalizedKamAuthMethod(acc.credentials.authMethod, clientId, clientSecret)
        const cred: AddCredentialRequest = mergeCredentialDefaults({
          authMethod,
          accessToken: optionalTrimmed(acc.credentials.accessToken),
          expiresAt: optionalTrimmed(acc.credentials.expiresAt),
          refreshToken: optionalTrimmed(acc.credentials.refreshToken),
          clientId,
          clientSecret: authMethod === 'idc' ? clientSecret : undefined,
          tokenEndpoint: authMethod === 'external_idp' ? optionalTrimmed(acc.credentials.tokenEndpoint) : undefined,
          issuerUrl: authMethod === 'external_idp' ? optionalTrimmed(acc.credentials.issuerUrl) : undefined,
          scopes: authMethod === 'external_idp' ? optionalTrimmed(acc.credentials.scopes) : undefined,
          profileArn: optionalTrimmed(acc.credentials.profileArn),
          region: optionalTrimmed(acc.credentials.region),
          apiRegion: optionalTrimmed(acc.credentials.apiRegion),
          email: optionalTrimmed(acc.email),
          machineId: optionalTrimmed(acc.machineId),
        }, defaults)
        const res = await addCredential(cred)
        newResults[i] = { ...newResults[i], credentialId: res.credentialId, email: res.email, status: 'verifying' }
        setResults([...newResults])
        try {
          const verified = await verifyImportedCredential(res.credentialId, verifyMode)
          newResults[i] = { ...newResults[i], status: 'success', model: verified.model, response: verified.response }
        } catch (ve) {
          await rollbackCredential(res.credentialId)
          newResults[i] = { ...newResults[i], status: 'failed', error: `验活失败: ${extractErrorMessage(ve)}` }
        }
      } catch (e) {
        newResults[i] = { ...newResults[i], status: 'failed', error: extractErrorMessage(e) }
      }
      setResults([...newResults])
    }
    if (isRetry) {
      setResults((prev) => {
        let retryIdx = 0
        return prev.map((r) => {
          if (r.status === 'failed' && retryIdx < newResults.length) {
            return { ...newResults[retryIdx++], index: r.index }
          }
          return r
        })
      })
    }
    setRunning(false)
    const success = newResults.filter((r) => r.status === 'success').length
    const failed = newResults.filter((r) => r.status === 'failed').length
    onDone()
    toast.success(`KAM 导入完成：成功 ${success}，失败 ${failed}，跳过 ${skippedCount}`)
  }

  const run = () => { if (accounts.length) runAccounts(accounts) }

  const failedAccounts = accounts.filter((_, i) => results[i]?.status === 'failed')
  const retryFailed = () => { if (failedAccounts.length) runAccounts(failedAccounts, true) }

  return (
    <ModalShell open={open} title="KAM 导入账号" width="max-w-2xl" onClose={onClose}>
      <div className="space-y-4">
        {!accounts.length ? (
          <>
            <div className="flex gap-2">
              <label className="cursor-pointer">
                <input type="file" className="sr-only" accept=".json,.txt" multiple onChange={handleFile} />
                <Button type="button" variant="outline" size="sm" asChild><span><FileUp className="h-3.5 w-3.5" />从文件导入</span></Button>
              </label>
            </div>
            <Textarea className="min-h-[120px] font-mono text-xs" placeholder='粘贴 KAM JSON...' value={text} onChange={(e) => setText(e.target.value)} />
            <CredentialParameterDefaultsPanel defaults={defaults} onChange={setDefaults} proxyResources={proxyOptions} />
            <div className="flex justify-end gap-2">
              <Button variant="ghost" size="sm" onClick={onClose}>取消</Button>
              <Button size="sm" onClick={handleParse} disabled={!text.trim()}>解析预览</Button>
            </div>
          </>
        ) : (
          <>
            <div className="flex items-center justify-between">
              <div className="text-sm">
                共 <span className="font-semibold">{accounts.length}</span> 个账号
                {results.some((r) => r.status !== 'pending') && (
                  <span className="ml-2 text-muted-foreground">
                    成功 {results.filter((r) => r.status === 'success').length} · 失败 {results.filter((r) => r.status === 'failed').length} · 跳过 {results.filter((r) => r.status === 'skipped').length}
                  </span>
                )}
              </div>
              {!running && <Button variant="ghost" size="xs" onClick={() => { setAccounts([]); setResults([]) }}>重新编辑</Button>}
            </div>
            {/* 验活模式和跳过 error 选项（仅未开始时显示） */}
            {results.every((r) => r.status === 'pending') && (
              <div className="rounded-lg bg-muted/30 p-3 space-y-2">
                <div className="text-sm font-semibold">导入选项</div>
                <Select value={verifyMode} onValueChange={(v) => setVerifyMode(v as ImportVerificationMode)} disabled={running}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="subscription_only">只查询订阅（不请求模型）</SelectItem>
                    <SelectItem value="model_and_subscription">测试模型 + 查询订阅</SelectItem>
                  </SelectContent>
                </Select>
                {hasErrorAccounts && (
                  <div className="flex items-center gap-2">
                    <Checkbox
                      id="skip-error-accounts"
                      checked={skipErrorAccounts}
                      onCheckedChange={(v) => setSkipErrorAccounts(Boolean(v))}
                      disabled={running}
                    />
                    <label htmlFor="skip-error-accounts" className="text-sm cursor-pointer">
                      跳过 error 状态的账号（{accounts.filter((a) => a.status === 'error').length} 个）
                    </label>
                  </div>
                )}
              </div>
            )}
            {running && <Progress value={Math.round((results.filter((r) => r.status !== 'pending').length / accounts.length) * 100)} className="h-1.5" />}
            <ImportProgressList
              results={results}
              getLabel={(i) => accounts[i]?.email || `账号 ${i + 1}`}
            />
            <ImportResultFooter
              running={running}
              results={results}
              failedCount={failedAccounts.length}
              onClose={onClose}
              onRun={run}
              onRetry={retryFailed}
            />
          </>
        )}
      </div>
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
    concurrency: '',
    rpm: '',
    proxyResourceId: '',
    proxyUrl: '',
    proxyUsername: '',
    proxyPassword: '',
    region: '',
    authRegion: '',
    apiRegion: '',
  })
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const [enableFields, setEnableFields] = useState({ priority: false, concurrency: false, rpm: false, proxy: false, regions: false })
  const batchUpdate = useBatchUpdateCredentials()
  const proxyResources = useProxyResources()
  const proxyOptions = proxyResources.data?.resources || []

  const proxyLocked = Boolean(fields.proxyResourceId)

  const setProxyResourceDraft = (v: string) => {
    setFields((p) => ({ ...p, proxyResourceId: v, proxyUrl: v ? '' : p.proxyUrl, proxyUsername: v ? '' : p.proxyUsername, proxyPassword: v ? '' : p.proxyPassword }))
  }

  const setDirectProxyDraft = (key: 'proxyUrl' | 'proxyUsername' | 'proxyPassword', v: string) => {
    setFields((p) => ({ ...p, [key]: v, proxyResourceId: v.trim() ? '' : p.proxyResourceId }))
  }

  useEffect(() => {
    if (!open) {
      setFields({ priority: '', concurrency: '', rpm: '', proxyResourceId: '', proxyUrl: '', proxyUsername: '', proxyPassword: '', region: '', authRegion: '', apiRegion: '' })
      setEnableFields({ priority: false, concurrency: false, rpm: false, proxy: false, regions: false })
      setShowProxyUsername(false)
      setShowProxyPassword(false)
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
    if (enableFields.proxy) {
      const resourceId = fields.proxyResourceId ? Number(fields.proxyResourceId) : null
      req.proxy = {
        proxyResourceId: resourceId,
        proxyUrl: resourceId ? undefined : fields.proxyUrl.trim() || undefined,
        proxyUsername: resourceId ? undefined : fields.proxyUsername.trim() || undefined,
        proxyPassword: resourceId ? undefined : fields.proxyPassword.trim() || undefined,
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
    batchUpdate.mutate(req, {
      onSuccess: (res) => {
        toast.success(`批量修改完成：成功 ${res.success}，失败 ${res.failed}`)
        onDone(); onClose()
      },
      onError: (e) => toast.error(`批量修改失败: ${extractErrorMessage(e)}`),
    })
  }

  return (
    <ModalShell open={open} title={`批量修改（${ids.length} 个账号）`} width="max-w-lg" onClose={onClose}>
      <div className="space-y-4">
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 p-3 space-y-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="text-sm font-semibold">调度覆盖</div>
              <Button type="button" variant="ghost" size="xs" onClick={clearSchedulingDraft} disabled={batchUpdate.isPending}>
                清理优先级/并发/RPM
              </Button>
            </div>
            <FieldGrid>
              <Field label="优先级" description="数字越小越优先；留空重置为默认 0">
                <div className="flex items-center gap-2">
                  <Checkbox checked={enableFields.priority} onCheckedChange={(v) => setEnableFields((p) => ({ ...p, priority: Boolean(v) }))} id="batch-priority" />
                  <Input
                    type="number"
                    min={0}
                    value={fields.priority}
                    disabled={!enableFields.priority || batchUpdate.isPending}
                    onChange={(e) => setFields((p) => ({ ...p, priority: e.target.value }))}
                    placeholder="留空重置"
                  />
                </div>
              </Field>
              <Field label="最大并发" description="留空清除覆盖，0 表示不限">
                <div className="flex items-center gap-2">
                  <Checkbox checked={enableFields.concurrency} onCheckedChange={(v) => setEnableFields((p) => ({ ...p, concurrency: Boolean(v) }))} id="batch-concurrency" />
                  <Input
                    type="number"
                    min={0}
                    value={fields.concurrency}
                    disabled={!enableFields.concurrency || batchUpdate.isPending}
                    onChange={(e) => setFields((p) => ({ ...p, concurrency: e.target.value }))}
                    placeholder="留空继承全局"
                  />
                </div>
              </Field>
              <Field label="RPM" description="留空清除覆盖，0 表示不限">
                <div className="flex items-center gap-2">
                  <Checkbox checked={enableFields.rpm} onCheckedChange={(v) => setEnableFields((p) => ({ ...p, rpm: Boolean(v) }))} id="batch-rpm" />
                  <Input
                    type="number"
                    min={0}
                    value={fields.rpm}
                    disabled={!enableFields.rpm || batchUpdate.isPending}
                    onChange={(e) => setFields((p) => ({ ...p, rpm: e.target.value }))}
                    placeholder="留空继承全局"
                  />
                </div>
              </Field>
            </FieldGrid>
          </div>
          <div className="rounded-lg bg-muted/30 p-3 space-y-3">
            <div className="flex items-center gap-2">
              <Checkbox checked={enableFields.proxy} onCheckedChange={(v) => setEnableFields((p) => ({ ...p, proxy: Boolean(v) }))} id="batch-proxy" />
              <label htmlFor="batch-proxy" className="text-sm font-semibold cursor-pointer">代理设置</label>
            </div>
            {enableFields.proxy && (
              <div className="space-y-3">
                <Field label="代理资源" description="选择资源会清空直连代理字段；不选且 URL 为空则清除账号级代理">
                  <Select value={fields.proxyResourceId || '__none__'} onValueChange={(v) => setProxyResourceDraft(v === '__none__' ? '' : v)}>
                    <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="__none__">不绑定（清除代理资源）</SelectItem>
                      {proxyOptions.map((r) => <SelectItem key={r.id} value={String(r.id)}>{r.name}</SelectItem>)}
                    </SelectContent>
                  </Select>
                </Field>
                <div className={`rounded-lg p-3 space-y-2 ${proxyLocked ? 'opacity-50 bg-muted/20' : 'bg-card shadow-sm'}`}>
                  <div className="text-xs font-semibold text-muted-foreground">
                    直连代理{proxyLocked ? '（已选代理资源，保存时会清空）' : ''}
                  </div>
                  <FieldGrid>
                    <Field label="代理 URL">
                      <Input
                        value={fields.proxyUrl}
                        placeholder="socks5h://127.0.0.1:1080"
                        disabled={proxyLocked}
                        onChange={(e) => setDirectProxyDraft('proxyUrl', e.target.value)}
                      />
                    </Field>
                    <Field label="用户名">
                      <SecretInput
                        value={fields.proxyUsername}
                        onChange={(v) => setDirectProxyDraft('proxyUsername', v)}
                        visible={showProxyUsername}
                        onToggle={() => setShowProxyUsername((v) => !v)}
                        disabled={proxyLocked}
                        placeholder="可选"
                      />
                    </Field>
                    <Field label="密码">
                      <SecretInput
                        value={fields.proxyPassword}
                        onChange={(v) => setDirectProxyDraft('proxyPassword', v)}
                        visible={showProxyPassword}
                        onToggle={() => setShowProxyPassword((v) => !v)}
                        disabled={proxyLocked}
                        placeholder="可选"
                      />
                    </Field>
                  </FieldGrid>
                </div>
              </div>
            )}
          </div>
          <div className="rounded-lg bg-muted/30 p-3 space-y-3">
            <div className="flex items-center gap-2">
              <Checkbox checked={enableFields.regions} onCheckedChange={(v) => setEnableFields((p) => ({ ...p, regions: Boolean(v) }))} id="batch-regions" />
              <label htmlFor="batch-regions" className="text-sm font-semibold cursor-pointer">Region 设置</label>
            </div>
            {enableFields.regions && (
              <FieldGrid>
                <Field label="Auth Region"><Input className="font-mono" value={fields.authRegion} onChange={(e) => setFields((p) => ({ ...p, authRegion: e.target.value }))} placeholder="留空清除" /></Field>
                <Field label="API Region"><Input className="font-mono" value={fields.apiRegion} onChange={(e) => setFields((p) => ({ ...p, apiRegion: e.target.value }))} placeholder="留空清除" /></Field>
              </FieldGrid>
            )}
          </div>
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose} disabled={batchUpdate.isPending}>取消</Button>
          <Button size="sm" onClick={submit} disabled={batchUpdate.isPending || !Object.values(enableFields).some(Boolean)}>
            {batchUpdate.isPending && <Spinner size="sm" />}应用修改
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}

// ============================================================================
// CredentialTestModal
// ============================================================================

export function CredentialTestModal({ credential, open, onClose }: {
  credential: CredentialStatusItem | null; open: boolean; onClose: () => void
}) {
  const [model, setModel] = useState(DEFAULT_TEST_MODEL)
  const [prompt, setPrompt] = useState(DEFAULT_TEST_PROMPT)
  const [result, setResult] = useState<TestCredentialResponse | null>(null)
  const testMutation = useTestCredential()

  useEffect(() => { if (!open) { setResult(null); setModel(DEFAULT_TEST_MODEL); setPrompt(DEFAULT_TEST_PROMPT) } }, [open])

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
          <Select value={model} onValueChange={setModel} disabled={testMutation.isPending}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              {TEST_MODELS.map((m) => <SelectItem key={m.id} value={m.id}>{m.label}</SelectItem>)}
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

export function BatchVerifyModal({ open, verifying, progress, results, onCancel, onClose }: {
  open: boolean; verifying: boolean
  progress: { current: number; total: number }
  results: Map<number, VerifyResult>
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
          测试模型：<span className="font-semibold text-foreground">{testModelLabel(DEFAULT_TEST_MODEL)}</span>
          <span className="ml-2 text-muted-foreground/70">（批量验活固定使用默认模型）</span>
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

export function CredentialExportModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [format, setFormat] = useState<CredentialExportFormat>('json')
  const [loading, setLoading] = useState(false)

  const handleExport = async () => {
    setLoading(true)
    try {
      const blob = await exportCredentials(format)
      downloadBlob(blob, exportFilename(format))
      toast.success('导出成功')
      onClose()
    } catch (e) {
      toast.error(`导出失败: ${extractErrorMessage(e)}`)
    } finally {
      setLoading(false)
    }
  }

  return (
    <ModalShell open={open} title="导出账号" width="max-w-sm" onClose={onClose}>
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
