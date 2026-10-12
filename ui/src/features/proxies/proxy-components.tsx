import * as React from 'react'
import {
  Activity,
  AlertCircle,
  CheckCircle2,
  Edit3,
  Eye,
  EyeOff,
  Loader2,
  Plus,
  Trash2,
  Upload,
  Users,
  XCircle,
} from 'lucide-react'
import { toast } from 'sonner'
import { formatDate } from '@/lib/format'
import { extractErrorMessage, cn } from '@/lib/utils'
import {
  useCreateProxyResource,
  useCredentials,
  useDeleteProxyResource,
  useImportProxyResources,
  useSetCredentialProxy,
  useTestProxyResource,
  useUpdateProxyResource,
} from '@/hooks/use-credentials'
import type {
  CredentialStatusItem,
  ProxyResource,
  ProxyResourceImportEntry,
  ProxyResourceTestResponse,
} from '@/types/api'
import { ModalShell, Field, FieldGrid, EmptyState, LoadingState, useConfirm } from '@/components/patterns'
import {
  Badge,
  Button,
  Checkbox,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Textarea,
  Switch,
  Spinner,
  Progress,
} from '@/components/ui'

// ============================================================================
// ProxyTestResult
// ============================================================================

export function ProxyTestResult({ result }: { result: ProxyResourceTestResponse | null }) {
  if (!result) return null
  return (
    <div className={cn(
      'rounded-lg p-3 text-xs',
      result.success
        ? 'bg-success/5 text-success'
        : 'border-destructive/30 bg-destructive/5 text-destructive'
    )}>
      <div className="font-semibold">{result.message}</div>
      <div className="mt-1 text-muted-foreground">
        耗时 {result.durationMs}ms
        {typeof result.status === 'number' ? ` · HTTP ${result.status}` : ''} · {result.testUrl}
      </div>
      {result.responsePreview && (
        <div className="mt-1 break-words font-mono text-foreground/70">{result.responsePreview}</div>
      )}
    </div>
  )
}

// ============================================================================
// SecretInput
// ============================================================================

function SecretInput({ value, onChange, visible, onToggle }: {
  value: string; onChange: (v: string) => void; visible: boolean; onToggle: () => void
}) {
  return (
    <div className="relative">
      <Input
        className="pr-10"
        type={visible ? 'text' : 'password'}
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
      <Button
        variant="ghost"
        size="icon-xs"
        className="absolute right-1 top-1/2 -translate-y-1/2"
        onClick={onToggle}
        title={visible ? '隐藏' : '显示'}
      >
        {visible ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
      </Button>
    </div>
  )
}

// ============================================================================
// CredentialBindingPicker
// ============================================================================

const credentialLabel = (c: CredentialStatusItem) => c.email || c.maskedApiKey || `账号 #${c.id}`

export function CredentialBindingPicker({ credentials, selectedIds, onToggle }: {
  credentials: CredentialStatusItem[]; selectedIds: Set<number>; onToggle: (id: number) => void
}) {
  if (!credentials.length) return <EmptyState title="暂无可绑定账号" />
  const sorted = [...credentials].sort((a, b) => {
    if (a.disabled !== b.disabled) return a.disabled ? 1 : -1
    return a.id - b.id
  })
  return (
    <div className="scrollbar-thin max-h-72 space-y-2 overflow-y-auto rounded-xl bg-muted/30 p-2">
      {sorted.map((credential) => {
        const selected = selectedIds.has(credential.id)
        return (
          <label
            key={credential.id}
            className={cn(
              'flex cursor-pointer items-start gap-3 rounded-lg border p-2 text-sm transition-colors',
              selected
                ? 'border-primary bg-primary/5'
                : credential.disabled
                  ? 'border-destructive/25 bg-destructive/5 opacity-80'
                  : 'border-border bg-card hover:bg-muted'
            )}
          >
            <Checkbox className="mt-0.5" checked={selected} onCheckedChange={() => onToggle(credential.id)} />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="truncate font-semibold" title={credentialLabel(credential)}>
                  {credentialLabel(credential)}
                </span>
                <Badge>#{credential.id}</Badge>
                <Badge tone={credential.disabled ? 'error' : 'success'}>
                  {credential.disabled ? '已禁用' : '启用'}
                </Badge>
                {credential.proxyResourceName && <Badge tone="info">{credential.proxyResourceName}</Badge>}
              </div>
              <div className="mt-1 truncate text-xs text-muted-foreground">
                {credential.effectiveProxyUrl || '当前未使用代理资源'}
              </div>
            </div>
          </label>
        )
      })}
    </div>
  )
}

// ============================================================================
// ProxyEditorModal
// ============================================================================

type ProxyForm = {
  name: string; proxyUrl: string; proxyUsername: string; proxyPassword: string; notes: string; enabled: boolean
}

const emptyForm = (): ProxyForm => ({ name: '', proxyUrl: '', proxyUsername: '', proxyPassword: '', notes: '', enabled: true })

const formFromResource = (r: ProxyResource): ProxyForm => ({
  name: r.name, proxyUrl: r.proxyUrl, proxyUsername: r.proxyUsername || '',
  proxyPassword: r.proxyPassword || '', notes: r.notes || '', enabled: r.enabled,
})

const PROXY_PROTOCOLS = new Set(['http', 'https', 'socks5', 'socks5h'])

type ProxyImportItemStatus = 'pending' | 'testing' | 'passed' | 'failed' | 'skipped' | 'importing' | 'imported'

type ProxyImportProgressItem = {
  index: number
  label: string
  entry?: ProxyResourceImportEntry
  status: ProxyImportItemStatus
  message?: string
  result?: ProxyResourceTestResponse
  resourceId?: number
}

function decodeProxyPart(value: string): string {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

function looksLikeHostPort(value: string): boolean {
  const trimmed = value.trim()
  if (trimmed.startsWith('[')) {
    const close = trimmed.indexOf(']:')
    return close >= 0 && /^\d+$/.test(trimmed.slice(close + 2))
  }
  const port = trimmed.slice(trimmed.lastIndexOf(':') + 1)
  return /^\d+$/.test(port)
}

function parseProxyImportLine(line: string): ProxyResourceImportEntry {
  const value = line.trim()
  if (!value) throw new Error('代理项不能为空')

  if (value.includes('://')) {
    try {
      const parsed = new URL(value)
      const username = decodeProxyPart(parsed.username)
      const password = parsed.password ? decodeProxyPart(parsed.password) : undefined
      if (username || password) {
        parsed.username = ''
        parsed.password = ''
        return {
          proxyUrl: parsed.toString(),
          proxyUsername: username || undefined,
          proxyPassword: password,
        }
      }
    } catch {
      // Let the normalizer return the same validation error as the backend.
    }
    return { proxyUrl: value }
  }

  const at = value.indexOf('@')
  if (at > 0) {
    const credentials = value.slice(0, at)
    const address = value.slice(at + 1)
    const separator = credentials.indexOf(':')
    if (separator > 0) {
      return {
        proxyUrl: address.trim(),
        proxyUsername: credentials.slice(0, separator).trim(),
        proxyPassword: credentials.slice(separator + 1).trim(),
      }
    }
  }

  if (value.startsWith('[')) {
    const close = value.indexOf(']:')
    if (close >= 0) {
      const address = value.slice(0, close + 1)
      const fields = value.slice(close + 2).split(':')
      if (fields.length >= 3 && /^\d+$/.test(fields[0])) {
        const port = fields.shift()!
        const username = fields.shift()?.trim() || undefined
        const password = fields.join(':').trim() || undefined
        return {
          proxyUrl: `${address}:${port}`,
          proxyUsername: username,
          proxyPassword: password,
        }
      }
    }
  }

  let parts: string[]
  let separator: 'colon' | 'named'
  if (value.includes('----')) {
    parts = value.split('----').map((part) => part.trim())
    separator = 'named'
  } else if (value.includes('|')) {
    parts = value.split('|').map((part) => part.trim())
    separator = 'named'
  } else if (value.includes('\t')) {
    parts = value.split('\t').map((part) => part.trim())
    separator = 'named'
  } else if (value.includes(',')) {
    parts = value.split(',').map((part) => part.trim())
    separator = 'named'
  } else {
    const colonParts = value.split(':')
    parts = colonParts.length >= 4
      ? [colonParts[0], colonParts[1], colonParts[2], colonParts.slice(3).join(':')]
      : colonParts
    separator = 'colon'
  }

  if (separator === 'colon' && parts.length === 4) {
    return {
      proxyUrl: `${parts[0]}:${parts[1]}`,
      proxyUsername: parts[2] || undefined,
      proxyPassword: parts[3] || undefined,
    }
  }
  if (separator === 'named' && parts.length === 4) {
    return {
      name: parts[0] || undefined,
      proxyUrl: parts[1] || undefined,
      proxyUsername: parts[2] || undefined,
      proxyPassword: parts[3] || undefined,
    }
  }
  if (parts.length === 3) {
    if (separator === 'named' && looksLikeHostPort(parts[0])) {
      return {
        proxyUrl: parts[0],
        proxyUsername: parts[1] || undefined,
        proxyPassword: parts[2] || undefined,
      }
    }
    if (separator === 'named') {
      return {
        name: parts[0] || undefined,
        proxyUrl: parts[1] || undefined,
        proxyUsername: parts[2] || undefined,
      }
    }
  }
  if (separator === 'named' && parts.length === 2) {
    return { name: parts[0] || undefined, proxyUrl: parts[1] || undefined }
  }
  return { proxyUrl: value }
}

function entryFromProxyImportValue(value: unknown): ProxyResourceImportEntry {
  if (typeof value === 'string') return parseProxyImportLine(value)
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('代理项必须是字符串或对象')
  }
  const object = value as Record<string, unknown>
  const stringValue = (key: string, ...aliases: string[]) => {
    const raw = [key, ...aliases].map((candidate) => object[candidate]).find((candidate) => typeof candidate === 'string')
    return typeof raw === 'string' ? raw : undefined
  }
  return {
    name: stringValue('name'),
    proxyUrl: stringValue('proxyUrl', 'url', 'proxy', 'address'),
    proxyUsername: stringValue('proxyUsername', 'username', 'user'),
    proxyPassword: stringValue('proxyPassword', 'password', 'pass'),
    enabled: typeof object.enabled === 'boolean' ? object.enabled : undefined,
    notes: stringValue('notes'),
  }
}

function parseProxyImportValues(content: string): unknown[] {
  const trimmed = content.trim()
  if (!trimmed) return []
  try {
    const parsed: unknown = JSON.parse(trimmed)
    return Array.isArray(parsed) ? parsed : [parsed]
  } catch {
    return trimmed.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).map((line) => {
      if (line.startsWith('{') || line.startsWith('[')) {
        try {
          const parsed: unknown = JSON.parse(line)
          return Array.isArray(parsed) ? parsed : parsed
        } catch {
          if (line.startsWith('{')) throw new Error('JSONL 解析失败')
        }
      }
      return line
    }).flat()
  }
}

function normalizeProxyImportEntry(
  entry: ProxyResourceImportEntry,
  defaultProtocol: string,
  namePrefix: string,
  index: number,
): ProxyResourceImportEntry {
  const rawUrl = entry.proxyUrl?.trim()
  if (!rawUrl) throw new Error('代理 URL 不能为空')
  const protocol = defaultProtocol.trim().replace(/:\/+$/, '').toLowerCase()
  const normalizedUrl = rawUrl.includes('://') ? rawUrl : `${protocol}://${rawUrl}`
  const parsed = new URL(normalizedUrl)
  const scheme = parsed.protocol.replace(/:$/, '').toLowerCase()
  if (!PROXY_PROTOCOLS.has(scheme)) {
    throw new Error(`不支持的代理协议: ${scheme}`)
  }
  if (!parsed.hostname || (!parsed.port && scheme !== 'http' && scheme !== 'https')) {
    throw new Error('代理 URL 必须包含主机名和端口')
  }
  let username = entry.proxyUsername?.trim() || undefined
  let password = entry.proxyPassword?.trim() || undefined
  if (!username && parsed.username) username = decodeProxyPart(parsed.username)
  if (!password && parsed.password) password = decodeProxyPart(parsed.password)
  parsed.username = ''
  parsed.password = ''
  return {
    ...entry,
    name: entry.name?.trim() || `${namePrefix}-${index}`,
    proxyUrl: parsed.toString(),
    proxyUsername: username,
    proxyPassword: password,
  }
}

function proxyImportDedupeKey(entry: ProxyResourceImportEntry): string {
  return [
    (entry.proxyUrl || '').trim().replace(/\/$/, '').toLowerCase(),
    (entry.proxyUsername || '').trim(),
    (entry.proxyPassword || '').trim(),
  ].join('\n')
}

function buildProxyImportProgress(
  content: string,
  defaultProtocol: string,
  namePrefix: string,
  deduplicate: boolean,
): ProxyImportProgressItem[] {
  const seen = new Set<string>()
  return parseProxyImportValues(content).map((value, offset) => {
    const index = offset + 1
    try {
      const normalized = normalizeProxyImportEntry(
        entryFromProxyImportValue(value),
        defaultProtocol,
        namePrefix || 'proxy',
        index,
      )
      if (deduplicate) {
        const key = proxyImportDedupeKey(normalized)
        if (!seen.has(key)) {
          seen.add(key)
        } else {
          return {
            index,
            label: normalized.name || `${namePrefix || 'proxy'}-${index}`,
            entry: normalized,
            status: 'skipped',
            message: '本次内容中重复，已跳过',
          }
        }
      }
      return {
        index,
        label: normalized.name || `${namePrefix || 'proxy'}-${index}`,
        entry: normalized,
        status: 'pending',
      }
    } catch (error) {
      return {
        index,
        label: `${namePrefix || 'proxy'}-${index}`,
        status: 'failed',
        message: error instanceof Error ? error.message : '代理格式无效',
      }
    }
  })
}

async function runWithConcurrency<T>(
  items: T[],
  concurrency: number,
  worker: (item: T) => Promise<void>,
): Promise<void> {
  let cursor = 0
  const workerCount = Math.min(Math.max(concurrency, 1), items.length)
  await Promise.all(Array.from({ length: workerCount }, async () => {
    while (cursor < items.length) {
      const item = items[cursor]
      cursor += 1
      await worker(item)
    }
  }))
}

export function ProxyEditorModal({ open, resource, onClose }: {
  open: boolean; resource: ProxyResource | null; onClose: () => void
}) {
  const [form, setForm] = React.useState(() => (resource ? formFromResource(resource) : emptyForm()))
  const [selectedIds, setSelectedIds] = React.useState<Set<number>>(new Set())
  const [bindingReady, setBindingReady] = React.useState(false)
  const [showUsername, setShowUsername] = React.useState(false)
  const [showPassword, setShowPassword] = React.useState(false)
  const [testResult, setTestResult] = React.useState<ProxyResourceTestResponse | null>(null)
  const create = useCreateProxyResource()
  const update = useUpdateProxyResource()
  const setCredentialProxy = useSetCredentialProxy()
  const testProxy = useTestProxyResource()
  const credentials = useCredentials({ enabled: open })
  const isEditing = Boolean(resource)
  const allCredentials = credentials.data?.credentials || []

  React.useEffect(() => {
    if (!open) return
    let cancelled = false
    setForm(resource ? formFromResource(resource) : emptyForm())
    setSelectedIds(new Set())
    setBindingReady(false)
    setTestResult(null)
    setShowUsername(false)
    setShowPassword(false)
    credentials.refetch().then((result) => {
      if (cancelled || result.error || !result.data) return
      const next = result.data.credentials || []
      setSelectedIds(new Set(resource ? next.filter((c) => c.proxyResourceId === resource.id).map((c) => c.id) : []))
      setBindingReady(true)
    })
    return () => { cancelled = true }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, resource?.id])

  const set = (key: keyof ProxyForm, value: string | boolean) =>
    setForm((prev) => ({ ...prev, [key]: value }))

  const toggleCredential = (id: number) =>
    setSelectedIds((prev) => { const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next })

  const syncBindings = async (resourceId: number) => {
    const ops = allCredentials
      .map((c) => {
        const selected = selectedIds.has(c.id)
        if (selected && c.proxyResourceId !== resourceId) return { id: c.id, proxyResourceId: resourceId }
        if (isEditing && !selected && c.proxyResourceId === resourceId) return { id: c.id, proxyResourceId: null }
        return null
      })
      .filter((op): op is { id: number; proxyResourceId: number | null } => Boolean(op))
    if (!ops.length) return { ok: 0, fail: 0 }
    const results = await Promise.allSettled(
      ops.map((op) => setCredentialProxy.mutateAsync({ id: op.id, request: { proxyResourceId: op.proxyResourceId } }))
    )
    return { ok: results.filter((r) => r.status === 'fulfilled').length, fail: results.filter((r) => r.status === 'rejected').length }
  }

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!form.name.trim()) return toast.error('请输入代理名称')
    if (!form.proxyUrl.trim()) return toast.error('请输入代理 URL')
    if (!bindingReady) return toast.error('账号列表仍在加载，请稍后再保存')
    if (credentials.isError || !credentials.data) return toast.error(`账号列表加载失败，无法同步绑定: ${extractErrorMessage(credentials.error)}`)
    const request = {
      name: form.name.trim(), proxyUrl: form.proxyUrl.trim(),
      proxyUsername: form.proxyUsername.trim() || undefined,
      proxyPassword: form.proxyPassword.trim() || undefined,
      enabled: form.enabled,
      notes: form.notes.trim() || undefined,
      clearUsername: isEditing && !form.proxyUsername.trim(),
      clearPassword: isEditing && !form.proxyPassword.trim(),
      clearNotes: isEditing && !form.notes.trim(),
    }
    try {
      const saved = isEditing && resource
        ? await update.mutateAsync({ id: resource.id, request })
        : await create.mutateAsync(request)
      const binding = await syncBindings(saved.id)
      if (binding.fail > 0) toast.warning(`代理已保存，账号绑定成功 ${binding.ok} 个，失败 ${binding.fail} 个`)
      else if (binding.ok > 0) toast.success(`代理已保存，已同步 ${binding.ok} 个账号绑定`)
      else toast.success(isEditing ? '代理资源已更新' : '代理资源已创建')
      onClose()
    } catch (error) {
      toast.error(`${isEditing ? '更新' : '创建'}失败: ${extractErrorMessage(error)}`)
    }
  }

  const testCurrent = async () => {
    if (!form.proxyUrl.trim()) return toast.error('请输入代理 URL')
    try {
      const result = await testProxy.mutateAsync({
        request: { proxyUrl: form.proxyUrl.trim(), proxyUsername: form.proxyUsername.trim() || undefined, proxyPassword: form.proxyPassword.trim() || undefined },
      })
      setTestResult(result)
      if (result.success) toast.success(result.message)
      else toast.error(result.message)
    } catch (error) {
      toast.error(`代理测试失败: ${extractErrorMessage(error)}`)
    }
  }

  const pending = create.isPending || update.isPending || setCredentialProxy.isPending || testProxy.isPending

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={isEditing && resource ? `编辑代理：${resource.name}` : '新增代理 / 家宽'}
      width="max-w-3xl"
      footer={
        <>
          <Button variant="outline" size="sm" onClick={onClose} disabled={pending}>取消</Button>
          <Button size="sm" disabled={pending || !bindingReady} onClick={submit}>
            {pending ? <Spinner size="sm" /> : <Plus className="size-4" />}
            {isEditing ? '保存代理' : '新增代理'}
          </Button>
        </>
      }
    >
      <form className="space-y-4" onSubmit={submit}>
        <FieldGrid>
          <Field label="名称">
            <Input value={form.name} onChange={(e) => set('name', e.target.value)} placeholder="住宅家宽 A / US Proxy 1" />
          </Field>
          <Field label="代理 URL" description="支持 http://、https://、socks5://、socks5h://；不写协议头时按 http 解析">
            <Input value={form.proxyUrl} onChange={(e) => set('proxyUrl', e.target.value)} placeholder="127.0.0.1:8080 或 socks5://127.0.0.1:1080" />
          </Field>
          <Field label="用户名">
            <SecretInput value={form.proxyUsername} onChange={(v) => set('proxyUsername', v)} visible={showUsername} onToggle={() => setShowUsername((v) => !v)} />
          </Field>
          <Field label="密码" description={isEditing ? '留空保存会清除当前密码' : undefined}>
            <SecretInput value={form.proxyPassword} onChange={(v) => set('proxyPassword', v)} visible={showPassword} onToggle={() => setShowPassword((v) => !v)} />
          </Field>
          <Field label="备注">
            <Textarea className="min-h-20" value={form.notes} onChange={(e) => set('notes', e.target.value)} />
          </Field>
          <div className="flex items-center justify-between gap-3 self-end rounded-xl bg-muted/30 p-3">
            <div>
              <div className="text-xs font-semibold">启用状态</div>
              <div className="text-xs text-muted-foreground">禁用后绑定该资源的账号不会回退到全局代理或直连。</div>
            </div>
            <Switch checked={form.enabled} onCheckedChange={() => set('enabled', !form.enabled)} />
          </div>
        </FieldGrid>

        <div className="space-y-2 rounded-xl bg-muted/30 p-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <div className="text-sm font-semibold">代理连通性</div>
              <div className="text-xs text-muted-foreground">保存前先用当前表单配置测试真实出网。</div>
            </div>
            <Button variant="ghost" size="sm" onClick={testCurrent} disabled={pending || !form.proxyUrl.trim()}>
              {testProxy.isPending ? <Spinner size="sm" /> : <Activity className="size-4" />}
              测试代理
            </Button>
          </div>
          <ProxyTestResult result={testResult} />
        </div>

        <div className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <div>
              <div className="text-sm font-semibold">绑定账号</div>
              <div className="text-xs text-muted-foreground">可直接选择要绑定到该代理资源的账号；已禁用账号仍可选择，但会用红色标识。</div>
            </div>
            <Badge tone="info">{selectedIds.size} 已选</Badge>
          </div>
          {!bindingReady && (credentials.isLoading || credentials.isFetching) ? (
            <LoadingState text="加载账号..." />
          ) : !bindingReady && credentials.isError ? (
            <div className="rounded-xl border border-destructive/30 bg-destructive/5 p-4 text-center text-sm text-destructive">
              账号列表加载失败：{extractErrorMessage(credentials.error)}
            </div>
          ) : (
            <CredentialBindingPicker credentials={allCredentials} selectedIds={selectedIds} onToggle={toggleCredential} />
          )}
        </div>
      </form>
    </ModalShell>
  )
}

export function ProxyImportModal({
  open,
  onClose,
  onImported,
}: {
  open: boolean
  onClose: () => void
  onImported?: (items: Array<{ resourceId: number; result: ProxyResourceTestResponse }>) => void
}) {
  const [content, setContent] = React.useState('')
  const [namePrefix, setNamePrefix] = React.useState('proxy')
  const [defaultProtocol, setDefaultProtocol] = React.useState('http')
  const [deduplicate, setDeduplicate] = React.useState(true)
  const [testBeforeImport, setTestBeforeImport] = React.useState(true)
  const [progressItems, setProgressItems] = React.useState<ProxyImportProgressItem[]>([])
  const [running, setRunning] = React.useState(false)
  const [completedRun, setCompletedRun] = React.useState(false)
  const importResources = useImportProxyResources()
  const testProxy = useTestProxyResource()

  React.useEffect(() => {
    if (open) {
      setContent('')
      setNamePrefix('proxy')
      setDefaultProtocol('http')
      setDeduplicate(true)
      setTestBeforeImport(true)
      setProgressItems([])
      setRunning(false)
      setCompletedRun(false)
    }
  }, [open])

  const updateProgressItem = React.useCallback((index: number, patch: Partial<ProxyImportProgressItem>) => {
    setProgressItems((previous) => previous.map((item) => (
      item.index === index ? { ...item, ...patch } : item
    )))
  }, [])

  const submit = async () => {
    if (!content.trim()) return toast.error('请粘贴代理内容或选择文件')
    if (running) return

    if (!testBeforeImport) {
      setRunning(true)
      try {
        const result = await importResources.mutateAsync({
          content,
          namePrefix: namePrefix.trim() || 'proxy',
          defaultProtocol,
          deduplicate,
          continueOnError: true,
        })
        const skippedText = result.skipped > 0 ? `，跳过重复 ${result.skipped}` : ''
        if (result.failed > 0) {
          toast.warning(`代理导入完成：成功 ${result.success}${skippedText}，失败 ${result.failed}`)
        } else {
          toast.success(`已导入 ${result.success} 个代理${skippedText}`)
        }
        setCompletedRun(true)
      } catch (error) {
        toast.error(`代理导入失败: ${extractErrorMessage(error)}`)
      } finally {
        setRunning(false)
      }
      return
    }

    let parsed: ProxyImportProgressItem[]
    try {
      parsed = buildProxyImportProgress(
        content,
        defaultProtocol,
        namePrefix.trim() || 'proxy',
        deduplicate,
      )
    } catch (error) {
      toast.error(`代理内容解析失败: ${extractErrorMessage(error)}`)
      return
    }
    if (!parsed.length) return toast.error('没有解析到有效的代理内容')

    setProgressItems(parsed)
    setCompletedRun(false)
    setRunning(true)

    const testable = parsed.filter((item) => item.status === 'pending' && item.entry)
    const testResults = new Map<number, ProxyResourceTestResponse>()
    await runWithConcurrency(testable, 6, async (item) => {
      updateProgressItem(item.index, { status: 'testing', message: '正在测活…' })
      try {
        const result = await testProxy.mutateAsync({
          request: {
            proxyUrl: item.entry?.proxyUrl,
            proxyUsername: item.entry?.proxyUsername,
            proxyPassword: item.entry?.proxyPassword,
          },
        })
        testResults.set(item.index, result)
        updateProgressItem(item.index, {
          status: result.success ? 'passed' : 'failed',
          message: result.success ? `测活通过 · ${result.durationMs}ms` : result.message,
          result,
        })
      } catch (error) {
        updateProgressItem(item.index, {
          status: 'failed',
          message: `测活请求失败: ${extractErrorMessage(error)}`,
        })
      }
    })

    const passedItems = testable.filter((item) => testResults.get(item.index)?.success)
    if (!passedItems.length) {
      setRunning(false)
      setCompletedRun(true)
      toast.warning(`代理测活完成：通过 0/${testable.length}，没有代理写入数据库`)
      return
    }

    setProgressItems((previous) => previous.map((item) => (
      passedItems.some((passed) => passed.index === item.index)
        ? { ...item, status: 'importing', message: '测活通过，正在写入数据库…' }
        : item
    )))

    try {
      const result = await importResources.mutateAsync({
        resources: passedItems.map((item) => item.entry!),
        namePrefix: namePrefix.trim() || 'proxy',
        defaultProtocol,
        deduplicate,
        continueOnError: true,
      })
      const imported = result.items
        .map((item) => ({ item, progress: passedItems[item.index - 1] }))
        .filter((value): value is {
          item: (typeof result.items)[number]
          progress: ProxyImportProgressItem
        } => Boolean(value.progress))
      const importedWithResults: Array<{ resourceId: number; result: ProxyResourceTestResponse }> = []
      for (const { item, progress } of imported) {
        const testResult = testResults.get(progress.index)
        if (item.ok && typeof item.resourceId === 'number' && testResult) {
          importedWithResults.push({ resourceId: item.resourceId, result: testResult })
          updateProgressItem(progress.index, {
            status: 'imported',
            resourceId: item.resourceId,
            message: `已导入 · ${testResult.durationMs}ms`,
            result: testResult,
          })
        } else if (item.skipped) {
          updateProgressItem(progress.index, {
            status: 'skipped',
            message: item.warning || '数据库中已存在，跳过导入',
          })
        } else {
          updateProgressItem(progress.index, {
            status: 'failed',
            message: item.error || '写入数据库失败',
          })
        }
      }
      onImported?.(importedWithResults)
      const skippedText = result.skipped > 0 ? `，跳过重复 ${result.skipped}` : ''
      if (result.failed > 0) {
        toast.warning(`代理测活并导入完成：成功 ${result.success}${skippedText}，失败 ${result.failed}`)
      } else {
        toast.success(`代理测活并导入完成：成功 ${result.success}${skippedText}`)
      }
      setCompletedRun(true)
    } catch (error) {
      setProgressItems((previous) => previous.map((item) => (
        item.status === 'importing'
          ? { ...item, status: 'failed', message: `写入数据库失败: ${extractErrorMessage(error)}` }
          : item
      )))
      toast.error(`代理导入失败: ${extractErrorMessage(error)}`)
    } finally {
      setRunning(false)
    }
  }

  const readFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0]
    if (!file) return
    setContent(await file.text())
    setProgressItems([])
    setCompletedRun(false)
    event.target.value = ''
  }

  const processedCount = progressItems.filter((item) => (
    item.status !== 'pending' && item.status !== 'testing' && item.status !== 'importing'
  )).length
  const passedCount = progressItems.filter((item) => item.status === 'passed' || item.status === 'imported').length
  const failedCount = progressItems.filter((item) => item.status === 'failed').length
  const progressValue = progressItems.length ? Math.round((processedCount / progressItems.length) * 100) : 0
  const isBusy = running || importResources.isPending

  const progressIcon = (status: ProxyImportItemStatus) => {
    if (status === 'testing' || status === 'importing') return <Loader2 className="size-4 animate-spin text-primary" />
    if (status === 'passed' || status === 'imported') return <CheckCircle2 className="size-4 text-success" />
    if (status === 'failed') return <XCircle className="size-4 text-destructive" />
    if (status === 'skipped') return <AlertCircle className="size-4 text-warning" />
    return <span className="size-4 rounded-full bg-muted" />
  }

  return (
    <ModalShell
      open={open}
      onClose={() => !isBusy && onClose()}
      title="批量导入代理"
      width="max-w-3xl"
      footer={(
        <>
          <Button variant="outline" size="sm" onClick={onClose} disabled={isBusy}>关闭</Button>
          <Button size="sm" onClick={submit} disabled={isBusy || !content.trim()}>
            {isBusy ? <Spinner size="sm" /> : <Upload className="size-4" />}
            {isBusy ? `处理中 ${processedCount}/${progressItems.length || '…'}` : completedRun ? '再次导入' : testBeforeImport ? '测活并导入' : '导入代理'}
          </Button>
        </>
      )}
    >
      <div className="space-y-4">
        <FieldGrid>
          <Field label="默认名称前缀">
            <Input value={namePrefix} onChange={(event) => setNamePrefix(event.target.value)} placeholder="proxy" />
          </Field>
          <Field label="缺省协议">
            <Select value={defaultProtocol} onValueChange={setDefaultProtocol}>
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="http">http</SelectItem>
                <SelectItem value="https">https</SelectItem>
                <SelectItem value="socks5">socks5</SelectItem>
                <SelectItem value="socks5h">socks5h</SelectItem>
              </SelectContent>
            </Select>
          </Field>
        </FieldGrid>
        <div className="grid gap-2 rounded-lg bg-muted/30 p-3 sm:grid-cols-2">
          <label className="flex items-start gap-2 text-sm">
            <Checkbox checked={deduplicate} onCheckedChange={(checked) => setDeduplicate(Boolean(checked))} />
            <span>
              <span className="block font-medium">自动去重</span>
              <span className="block text-xs text-muted-foreground">跳过本次和已有资源里的相同代理。</span>
            </span>
          </label>
          <label className="flex items-start gap-2 text-sm">
            <Checkbox checked={testBeforeImport} onCheckedChange={(checked) => setTestBeforeImport(Boolean(checked))} disabled={isBusy} />
            <span>
              <span className="block font-medium">测活通过后导入</span>
              <span className="block text-xs text-muted-foreground">并发测试代理，只有测活通过的代理才会写入数据库。</span>
            </span>
          </label>
        </div>
        <Field label="代理内容" description={`每行一个；支持 host:port、host:port:username:password、username:password@host:port、|、----、逗号、JSON 数组和 JSONL。缺少协议头时按 ${defaultProtocol} 解析。`}>
          <Textarea
            rows={12}
            value={content}
            onChange={(event) => setContent(event.target.value)}
            placeholder={'1.2.3.4:8080:user:pass\nuser:pass@2.3.4.5:3128\nsocks5://127.0.0.1:1080'}
            disabled={isBusy}
          />
        </Field>
        <div className="flex items-center gap-3">
          <Input type="file" accept=".txt,.json,.jsonl,.csv,text/plain,application/json" onChange={readFile} disabled={isBusy} />
          <span className="shrink-0 text-xs text-muted-foreground">导入文件会覆盖当前文本</span>
        </div>
        {progressItems.length > 0 && (
          <div className="space-y-3 rounded-xl border border-border/60 p-3">
            <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
              <div className="font-semibold">
                {running ? '代理测活进度' : '代理导入结果'}
              </div>
              <div className="text-xs text-muted-foreground">
                已处理 {processedCount}/{progressItems.length} · 通过 {passedCount} · 失败 {failedCount}
              </div>
            </div>
            <Progress value={progressValue} tone={failedCount > 0 ? 'warning' : 'primary'} className="h-1.5" />
            <div className="max-h-64 space-y-1 overflow-y-auto scrollbar-thin">
              {progressItems.map((item) => (
                <div key={item.index} className="flex items-start gap-2 rounded-lg bg-muted/30 px-3 py-2">
                  <div className="mt-0.5 shrink-0">{progressIcon(item.status)}</div>
                  <div className="min-w-0 flex-1 text-xs">
                    <div className="truncate font-semibold">{item.label}</div>
                    {item.status === 'passed' || item.status === 'imported' ? (
                      <div className="mt-1 flex items-center gap-2">
                        <Badge tone={item.result && item.result.durationMs < 200 ? 'success' : item.result && item.result.durationMs <= 500 ? 'info' : 'warning'}>
                          {item.result?.durationMs ?? 0}ms
                        </Badge>
                        <span className="truncate text-muted-foreground">{item.message}</span>
                      </div>
                    ) : (
                      <div className={cn(
                        'mt-1 truncate',
                        item.status === 'failed' ? 'text-destructive' : item.status === 'testing' || item.status === 'importing' ? 'text-primary' : 'text-muted-foreground',
                      )}>
                        {item.message || (item.status === 'pending' ? '等待测活…' : '已跳过')}
                      </div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </ModalShell>
  )
}

// ============================================================================
// ProxyResourceCard
// ============================================================================

function maskSecret(value?: string | null): string {
  if (!value) return '-'
  return '*'.repeat(Math.min(Math.max(value.length, 6), 16))
}

function Detail({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-muted-foreground">{label}</div>
      <div>{children}</div>
    </div>
  )
}

export function ProxyResourceCard({ resource, onEdit }: { resource: ProxyResource; onEdit: (r: ProxyResource) => void }) {
  const [showSecrets, setShowSecrets] = React.useState(false)
  const [testResult, setTestResult] = React.useState<ProxyResourceTestResponse | null>(null)
  const [bindingOpen, setBindingOpen] = React.useState(false)
  const [selectedIds, setSelectedIds] = React.useState<Set<number>>(new Set())
  const [bindingReady, setBindingReady] = React.useState(false)
  const [savingBindings, setSavingBindings] = React.useState(false)
  const update = useUpdateProxyResource()
  const remove = useDeleteProxyResource()
  const testProxy = useTestProxyResource()
  const setCredentialProxy = useSetCredentialProxy()
  const confirm = useConfirm()
  const credentials = useCredentials({ enabled: bindingOpen })

  const allCredentials = credentials.data?.credentials || []

  // 打开绑定弹层时加载账号并初始化选中状态
  React.useEffect(() => {
    if (!bindingOpen) return
    let cancelled = false
    setBindingReady(false)
    credentials.refetch().then((result) => {
      if (cancelled || result.error || !result.data) return
      const list = result.data.credentials || []
      setSelectedIds(new Set(list.filter((c) => c.proxyResourceId === resource.id).map((c) => c.id)))
      setBindingReady(true)
    })
    return () => { cancelled = true }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bindingOpen, resource.id])

  const toggleCredential = (id: number) =>
    setSelectedIds((prev) => { const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next })

  const saveBindings = async () => {
    if (!bindingReady || !credentials.data) return toast.error('账号列表尚未加载完成')
    setSavingBindings(true)
    try {
      const ops = allCredentials
        .map((c) => {
          const selected = selectedIds.has(c.id)
          if (selected && c.proxyResourceId !== resource.id) return { id: c.id, proxyResourceId: resource.id }
          if (!selected && c.proxyResourceId === resource.id) return { id: c.id, proxyResourceId: null as null }
          return null
        })
        .filter((op): op is { id: number; proxyResourceId: number | null } => Boolean(op))
      if (!ops.length) {
        toast.success('绑定关系无变化')
        setBindingOpen(false)
        return
      }
      const results = await Promise.allSettled(
        ops.map((op) => setCredentialProxy.mutateAsync({ id: op.id, request: { proxyResourceId: op.proxyResourceId } }))
      )
      const ok = results.filter((r) => r.status === 'fulfilled').length
      const fail = results.filter((r) => r.status === 'rejected').length
      if (fail > 0) toast.warning(`绑定成功 ${ok} 个，失败 ${fail} 个`)
      else {
        toast.success(`已同步 ${ok} 个账号绑定`)
        setBindingOpen(false)
      }
    } catch (error) {
      toast.error(`保存绑定失败: ${extractErrorMessage(error)}`)
    } finally {
      setSavingBindings(false)
    }
  }

  const toggleEnabled = () => update.mutate(
    { id: resource.id, request: { enabled: !resource.enabled } },
    {
      onSuccess: () => toast.success(resource.enabled ? '代理已禁用' : '代理已启用'),
      onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
    }
  )

  const testSavedProxy = async () => {
    try {
      const result = await testProxy.mutateAsync({
        id: resource.id,
        request: { proxyUrl: resource.proxyUrl, proxyUsername: resource.proxyUsername || undefined, proxyPassword: resource.proxyPassword || undefined },
      })
      setTestResult(result)
      if (result.success) toast.success(result.message)
      else toast.error(result.message)
    } catch (error) {
      toast.error(`代理测试失败: ${extractErrorMessage(error)}`)
    }
  }

  const deleteResource = async () => {
    if (resource.credentialCount > 0) {
      toast.warning(`代理「${resource.name}」当前仍绑定 ${resource.credentialCount} 个账号，请先在“账号绑定”里取消勾选并保存后再删除。`)
      return
    }
    const confirmed = await confirm({
      title: '删除代理资源',
      message: `确认删除代理资源「${resource.name}」？`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!confirmed) return
    remove.mutate(resource.id, {
      onSuccess: () => toast.success('代理资源已删除'),
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <div className="rounded-lg bg-card shadow-sm">
      <div className="p-4">
        <div className="flex flex-col gap-3">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-1.5">
                <h3 className="truncate text-sm font-semibold" title={resource.name}>{resource.name}</h3>
                <Badge>#{resource.id}</Badge>
                <Badge tone={resource.enabled ? 'success' : 'error'}>{resource.enabled ? '启用' : '已禁用'}</Badge>
                {resource.hasPassword && <Badge tone="info">密码</Badge>}
              </div>
              <div className="mt-1 truncate text-xs text-muted-foreground" title={resource.proxyUrl}>{resource.proxyUrl}</div>
            </div>
            <Switch checked={resource.enabled} disabled={update.isPending} onCheckedChange={toggleEnabled} />
          </div>

          <div className="grid gap-2 text-xs sm:grid-cols-2">
            <Detail label="用户名">
              <span className="font-mono font-semibold">{showSecrets ? resource.proxyUsername || '-' : maskSecret(resource.proxyUsername)}</span>
            </Detail>
            <Detail label="密码">
              <span className="font-mono font-semibold">{showSecrets ? resource.proxyPassword || '-' : resource.hasPassword ? maskSecret(resource.proxyPassword || '******') : '-'}</span>
            </Detail>
            <Detail label="绑定账号"><span className="font-semibold">{resource.credentialCount}</span></Detail>
            <Detail label="创建时间"><span className="font-semibold">{formatDate(resource.createdAt)}</span></Detail>
            <Detail label="更新时间"><span className="font-semibold">{formatDate(resource.updatedAt)}</span></Detail>
            {resource.notes && (
              <div className="sm:col-span-2">
                <div className="text-muted-foreground">备注</div>
                <div className="whitespace-pre-wrap break-words font-semibold">{resource.notes}</div>
              </div>
            )}
          </div>

          <ProxyTestResult result={testResult} />

          <div className="flex flex-wrap gap-1.5">
            <Button variant="ghost" size="xs" onClick={testSavedProxy} disabled={testProxy.isPending}>
              {testProxy.isPending ? <Spinner size="sm" /> : <Activity className="size-3.5" />}测试
            </Button>
            <Button variant="ghost" size="xs" onClick={() => setShowSecrets((v) => !v)}>
              {showSecrets ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
              {showSecrets ? '隐藏账号密码' : '显示账号密码'}
            </Button>
            <Button variant="ghost" size="xs" onClick={() => onEdit(resource)}>
              <Edit3 className="size-3.5" />编辑
            </Button>
            <Button
              variant="ghost" size="xs"
              className="text-destructive hover:bg-destructive/10 hover:text-destructive"
              onClick={deleteResource}
              disabled={remove.isPending}
            >
              <Trash2 className="size-3.5" />删除
            </Button>
            <Button
              variant="ghost" size="xs"
              className={cn(bindingOpen ? 'text-primary' : '')}
              onClick={() => setBindingOpen(true)}
            >
              <Users className="size-3.5" />账号绑定
            </Button>
          </div>
        </div>
      </div>

      <ModalShell
        open={bindingOpen}
        onClose={() => {
          if (!savingBindings && !setCredentialProxy.isPending) setBindingOpen(false)
        }}
        title={`账号绑定：${resource.name}`}
        description="勾选账号后保存绑定关系；已禁用账号以红色标识。"
        width="max-w-2xl"
        footer={(
          <>
            <Button
              variant="outline"
              onClick={() => setBindingOpen(false)}
              disabled={savingBindings || setCredentialProxy.isPending}
            >
              取消
            </Button>
            <Button
              disabled={!bindingReady || savingBindings || setCredentialProxy.isPending}
              onClick={saveBindings}
            >
              {savingBindings ? <Spinner size="sm" /> : null}
              保存绑定
            </Button>
          </>
        )}
      >
        <div className="space-y-3">
          <div className="flex items-center justify-end">
            <Badge tone="info">{selectedIds.size} 已选</Badge>
          </div>
          {!bindingReady && (credentials.isLoading || credentials.isFetching) ? (
            <LoadingState text="加载账号..." />
          ) : !bindingReady && credentials.isError ? (
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-center text-xs text-destructive">
              账号列表加载失败：{extractErrorMessage(credentials.error)}
            </div>
          ) : (
            <CredentialBindingPicker
              credentials={allCredentials}
              selectedIds={selectedIds}
              onToggle={toggleCredential}
            />
          )}
        </div>
      </ModalShell>
    </div>
  )
}
