import { useEffect, useState } from 'react'
import {
  Copy,
  Edit3,
  Eye,
  EyeOff,
  KeyRound,
  Plus,
  Save,
  Trash2,
  Wand2,
  X,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  Callout,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  useConfirm,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Spinner,
  Switch,
  Tooltip,
} from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import { storage } from '@/lib/storage'
import {
  createRequestApiKey,
  deleteRequestApiKey,
  getAccessKeys,
  updateAdminApiKey,
  updateRequestApiKey,
} from '@/api/credentials'
import type {
  AccessKeysResponse,
  RequestAdmissionConfig,
  RequestApiKeyItem,
} from '@/types/api'

const DEFAULT_REQUEST_ADMISSION: RequestAdmissionConfig = {
  rpm: 300,
  maxConcurrentRequests: 32,
  maxQueuedRequests: 64,
  queueTimeoutMs: 1000,
}

// ─── 工具 ──────────────────────────────────────────────────────────────────────

const REQUEST_API_KEY_PREFIX = 'sk-kiro-rs-'

function generateLocalRequestApiKey(): string {
  const bytes = new Uint8Array(32)
  const cryptoApi = globalThis.crypto
  if (!cryptoApi?.getRandomValues) throw new Error('当前环境不支持安全随机数生成')
  cryptoApi.getRandomValues(bytes)
  const binary = Array.from(bytes, (b) => String.fromCharCode(b)).join('')
  return `${REQUEST_API_KEY_PREFIX}${btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}`
}

function fillWithGeneratedKey(setValue: (value: string) => void) {
  try {
    setValue(generateLocalRequestApiKey())
  } catch (error) {
    toast.error(error instanceof Error ? error.message : '无法安全生成请求 Key')
  }
}

function accessKeyItems(response: AccessKeysResponse | null): RequestApiKeyItem[] {
  if (!response) return []
  if (response.requestApiKeys?.length) {
    return response.requestApiKeys.map((item, index) => ({
      ...item,
      name: item.name ?? `请求 Key ${index + 1}`,
      enabled: item.enabled ?? true,
    }))
  }
  if (!response.requestApiKey) return []
  return [{
    id: 'legacy-primary',
    apiKey: response.requestApiKey,
    maskedApiKey: response.maskedRequestApiKey,
    primary: true,
    name: '兼容旧主 Key',
    enabled: true,
  }]
}

async function copyText(label: string, value?: string) {
  if (!value) { toast.error(`${label} 为空，无法复制`); return }
  try {
    await navigator.clipboard.writeText(value)
    toast.success(`${label} 已复制`)
  } catch (e) {
    toast.error(`复制失败: ${extractErrorMessage(e)}`)
  }
}

function RequestApiKeyDialog({
  open,
  item,
  initialApiKey,
  defaultAdmission,
  saving,
  onOpenChange,
  onSave,
}: {
  open: boolean
  item?: RequestApiKeyItem
  defaultAdmission: RequestAdmissionConfig
  initialApiKey?: string
  saving: boolean
  onOpenChange: (open: boolean) => void
  onSave: (policy: {
    apiKey: string
    name: string
    enabled: boolean
    requestAdmission: RequestAdmissionConfig
  }) => void
}) {
  const isEdit = Boolean(item)
  const admission = item?.requestAdmission ?? defaultAdmission
  const [apiKey, setApiKey] = useState(initialApiKey ?? item?.apiKey ?? '')
  const [name, setName] = useState(item?.name ?? '新请求 Key')
  const [enabled, setEnabled] = useState(item?.enabled ?? true)
  const [rpm, setRpm] = useState(admission.rpm)
  const [concurrent, setConcurrent] = useState(admission.maxConcurrentRequests)
  const [queued, setQueued] = useState(admission.maxQueuedRequests)
  const [timeout, setTimeout] = useState(admission.queueTimeoutMs)

  useEffect(() => {
    setApiKey(initialApiKey ?? item?.apiKey ?? '')
    setName(item?.name ?? '新请求 Key')
    setEnabled(item?.enabled ?? true)
    setRpm(admission.rpm)
    setConcurrent(admission.maxConcurrentRequests)
    setQueued(admission.maxQueuedRequests)
    setTimeout(admission.queueTimeoutMs)
  }, [open, initialApiKey, item?.id, item?.apiKey, item?.name, item?.enabled, admission.rpm, admission.maxConcurrentRequests, admission.maxQueuedRequests, admission.queueTimeoutMs])

  const numeric = (value: string, max: number) =>
    Math.min(max, Math.max(0, Number.parseInt(value, 10) || 0))

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent width="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{isEdit ? '编辑请求 Key' : '新增请求 Key'}</DialogTitle>
          <DialogDescription>
            每个请求 Key 可单独设置名称、启用状态、RPM、并发和队列限制。
          </DialogDescription>
        </DialogHeader>
        <DialogBody>
          <div className="grid gap-4 sm:grid-cols-2">
            <label className="space-y-1 text-xs text-muted-foreground sm:col-span-2">
              <span>请求 Key</span>
              <div className="flex gap-2">
                <Input value={apiKey} disabled={saving} className="font-mono text-xs" onChange={(event) => setApiKey(event.target.value)} />
                <Button type="button" variant="outline" size="sm" disabled={saving} onClick={() => fillWithGeneratedKey(setApiKey)}>
                  <Wand2 className="h-4 w-4" />随机生成
                </Button>
              </div>
            </label>
            <label className="space-y-1 text-xs text-muted-foreground">
              <span>名称</span>
              <Input value={name} maxLength={80} disabled={saving} onChange={(event) => setName(event.target.value)} />
            </label>
            <div className="flex items-end justify-between gap-3 pb-1">
              <span className="text-sm">启用此 Key</span>
              <Switch checked={enabled} disabled={saving} onCheckedChange={setEnabled} />
            </div>
            <label className="space-y-1 text-xs text-muted-foreground">
              <span>每分钟请求数，0 为不限</span>
              <Input type="number" min={0} max={1_000_000} value={rpm} disabled={saving} onChange={(event) => setRpm(numeric(event.target.value, 1_000_000))} />
            </label>
            <label className="space-y-1 text-xs text-muted-foreground">
              <span>同时请求数，0 为不限</span>
              <Input type="number" min={0} max={10_000} value={concurrent} disabled={saving} onChange={(event) => setConcurrent(numeric(event.target.value, 10_000))} />
            </label>
            <label className="space-y-1 text-xs text-muted-foreground">
              <span>最多排队数</span>
              <Input type="number" min={0} max={100_000} value={queued} disabled={saving} onChange={(event) => setQueued(numeric(event.target.value, 100_000))} />
            </label>
            <label className="space-y-1 text-xs text-muted-foreground">
              <span>最长等待毫秒</span>
              <Input type="number" min={0} max={300_000} value={timeout} disabled={saving} onChange={(event) => setTimeout(numeric(event.target.value, 300_000))} />
            </label>
          </div>
          <p className="mt-4 text-xs leading-5 text-muted-foreground">
            此上限按实例、按 Key 生效；排队请求不会自动切换到外部池。
          </p>
        </DialogBody>
        <DialogFooter>
          <Button type="button" variant="outline" disabled={saving} onClick={() => onOpenChange(false)}>取消</Button>
          <Button
            type="button"
            disabled={saving || apiKey.trim().length < 8}
            onClick={() => onSave({
              apiKey: apiKey.trim(),
              name: name.trim(),
              enabled,
              requestAdmission: {
                rpm,
                maxConcurrentRequests: concurrent,
                maxQueuedRequests: queued,
                queueTimeoutMs: timeout,
              },
            })}
          >
            {saving ? <Spinner size="sm" /> : <Save className="h-4 w-4" />}保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

// ─── RequestKeysSection ───────────────────────────────────────────────────────

interface RequestKeysSectionProps {
  keys: AccessKeysResponse | null
  loading: boolean
  creating: boolean
  processingKeyId: string | null
  visibleIds: Set<string>
  onOpenCreate: (initialApiKey?: string) => void
  onOpenEdit: (item: RequestApiKeyItem) => void
  onToggleVisible: (id: string) => void
  onDelete: (item: RequestApiKeyItem) => void
  defaultAdmission: RequestAdmissionConfig
}

function RequestKeysSection({
  keys,
  loading,
  creating,
  processingKeyId,
  visibleIds,
  onOpenCreate,
  onOpenEdit,
  onToggleVisible,
  onDelete,
  defaultAdmission,
}: RequestKeysSectionProps) {
  const requestKeys = accessKeyItems(keys)

  return (
    <SectionCard
      title="请求调用 Key"
      description="给客户端调用模型接口时使用。可以按客户端分配不同 Key，新增或删除后立即生效。"
      actions={
        <Button size="sm" disabled={loading || creating} onClick={() => onOpenCreate()}>
          <Plus className="h-4 w-4" />
          新增 Key
        </Button>
      }
    >
      {/* Key 列表：每个 Key 一行，策略编辑默认收起。 */}
      <div className="mt-3 overflow-hidden rounded-md border">
        <div className="hidden grid-cols-[minmax(0,1.35fr)_minmax(0,1.4fr)_minmax(0,1fr)_auto] gap-3 bg-muted/40 px-4 py-2 text-[11px] font-medium text-muted-foreground md:grid">
          <span>名称 / 状态</span>
          <span>请求 Key</span>
          <span>准入策略</span>
          <span className="text-right">操作</span>
        </div>
        {loading && <div className="px-4 py-3 text-sm text-muted-foreground">加载中...</div>}
        {!loading && requestKeys.length === 0 && (
          <div className="px-4 py-3 text-sm text-destructive">未配置请求 Key，请先生成或手动添加。</div>
        )}
        {!loading && requestKeys.map((item) => {
          const visible = visibleIds.has(item.id)
          const busy = processingKeyId === item.id
          const admission = item.requestAdmission ?? defaultAdmission
          return (
            <div key={item.id} role="listitem" className="border-t px-4 py-3 first:border-t-0">
              <div className="grid gap-3 md:grid-cols-[minmax(0,1.35fr)_minmax(0,1.4fr)_minmax(0,1fr)_auto] md:items-center">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="truncate text-sm font-semibold">{item.name}</span>
                    {!item.enabled && <Badge tone="error" className="cursor-default">已停用</Badge>}
                    {item.primary && (
                      <Tooltip label="兼容旧 apiKey 字段的主 Key">
                        <Badge tone="primary" className="cursor-default">主 Key</Badge>
                      </Tooltip>
                    )}
                  </div>
                  <div className="mt-1 font-mono text-[0.68rem] text-muted-foreground">ID {item.id.slice(0, 12)}</div>
                </div>
                <div className="min-w-0">
                  <Input
                    readOnly
                    aria-label={`${item.name} 请求调用 Key`}
                    className="w-full min-w-0 font-mono text-xs"
                    value={visible ? item.apiKey : item.maskedApiKey}
                    disabled={busy}
                  />
                </div>
                <div className="flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground">
                  <span className="rounded border bg-muted/50 px-2 py-1">RPM {admission.rpm || '不限'}</span>
                  <span className="rounded border bg-muted/50 px-2 py-1">并发 {admission.maxConcurrentRequests || '不限'}</span>
                  <span className="rounded border bg-muted/50 px-2 py-1">队列 {admission.maxQueuedRequests || 0}</span>
                </div>
                <div className="flex flex-wrap justify-start gap-1.5 md:justify-end">
                  <Button variant="outline" size="xs" disabled={busy} onClick={() => onToggleVisible(item.id)} title={visible ? '隐藏 Key' : '显示 Key'}>
                    {visible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                    <span className="hidden lg:inline">{visible ? '隐藏' : '显示'}</span>
                  </Button>
                  <Button variant="outline" size="xs" disabled={busy} onClick={() => copyText('请求 Key', item.apiKey)}>
                    <Copy className="h-3.5 w-3.5" /><span className="hidden lg:inline">复制</span>
                  </Button>
                  <Button variant="ghost" size="xs" disabled={busy} onClick={() => onOpenEdit(item)}>
                    <Edit3 className="h-3.5 w-3.5" /><span className="hidden lg:inline">编辑</span>
                  </Button>
                  <Button variant="outline" size="xs" className="text-destructive hover:text-destructive" disabled={busy || requestKeys.length <= 1} onClick={() => onDelete(item)}>
                    <Trash2 className="h-3.5 w-3.5" /><span className="hidden lg:inline">删除</span>
                  </Button>
                </div>
              </div>
            </div>
          )
        })}
      </div>
    </SectionCard>
  )
}

// ─── SecurityPage ─────────────────────────────────────────────────────────────

export function SecurityPage() {
  const [keys, setKeys] = useState<AccessKeysResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [showAdminKey, setShowAdminKey] = useState(false)
  const [creating, setCreating] = useState(false)
  const [processingKeyId, setProcessingKeyId] = useState<string | null>(null)
  const [visibleIds, setVisibleIds] = useState<Set<string>>(new Set())
  const [keyDialogOpen, setKeyDialogOpen] = useState(false)
  const [keyDialogItem, setKeyDialogItem] = useState<RequestApiKeyItem | undefined>()
  const [keyDialogInitialApiKey, setKeyDialogInitialApiKey] = useState('')
  const [nextAdminKey, setNextAdminKey] = useState('')
  const [newKeyPlaintext, setNewKeyPlaintext] = useState<string | null>(null)
  const confirm = useConfirm()

  const loadKeys = async () => {
    setLoading(true)
    try { setKeys(await getAccessKeys()) }
    catch (e) { toast.error(`读取访问密钥失败: ${extractErrorMessage(e)}`) }
    finally { setLoading(false) }
  }

  useEffect(() => { void loadKeys() }, [])

  const setKeysAndReset = (response: AccessKeysResponse) => {
    setKeys(response)
    setVisibleIds((prev) => {
      const valid = new Set(accessKeyItems(response).map((i) => i.id))
      return new Set(Array.from(prev).filter((id) => valid.has(id)))
    })
  }

  const openCreateDialog = (initialApiKey = '') => {
    setKeyDialogItem(undefined)
    setKeyDialogInitialApiKey(initialApiKey || generateLocalRequestApiKey())
    setKeyDialogOpen(true)
  }

  const openEditDialog = (item: RequestApiKeyItem) => {
    setKeyDialogItem(item)
    setKeyDialogInitialApiKey(item.apiKey)
    setKeyDialogOpen(true)
  }

  const handleSaveKeyDialog = async (policy: {
    apiKey: string
    name: string
    enabled: boolean
    requestAdmission: RequestAdmissionConfig
  }) => {
    setCreating(true)
    try {
      const response = keyDialogItem
        ? await updateRequestApiKey(keyDialogItem.id, policy)
        : await createRequestApiKey(policy)
      setKeysAndReset(response)
      setKeyDialogOpen(false)
      if (!keyDialogItem) {
        const created = accessKeyItems(response).find((item) => item.apiKey === policy.apiKey)
        if (created) {
          setNewKeyPlaintext(created.apiKey)
          setVisibleIds((prev) => new Set(prev).add(created.id))
        }
      }
      toast.success(keyDialogItem ? '请求 Key 已保存并立即生效' : '请求 Key 已新增并立即生效')
    } catch (e) { toast.error(`${keyDialogItem ? '保存' : '新增'}失败: ${extractErrorMessage(e)}`) }
    finally { setCreating(false) }
  }

  const handleDelete = async (item: RequestApiKeyItem) => {
    const requestKeys = accessKeyItems(keys)
    if (requestKeys.length <= 1) return toast.error('至少需要保留一个请求 Key')
    const ok = await confirm({
      title: '删除请求 Key',
      message: `确认删除 ${item.maskedApiKey}？删除后，使用该 Key 的客户端会立即认证失败。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!ok) return
    setProcessingKeyId(item.id)
    try {
      const response = await deleteRequestApiKey(item.id)
      setKeysAndReset(response)
      toast.success('请求 Key 已删除')
    } catch (e) { toast.error(`删除失败: ${extractErrorMessage(e)}`) }
    finally { setProcessingKeyId(null) }
  }

  const handleSaveAdminKey = async () => {
    const adminApiKey = nextAdminKey.trim()
    if (!adminApiKey) return toast.error('请输入新的登录 Key')
    if (adminApiKey.length < 8) return toast.error('登录 Key 至少需要 8 个字符')
    const ok = await confirm({
      title: '修改登录 Key（高危操作）',
      message: '保存后，旧的登录 Key 立即失效。当前页面会自动切换到新 Key，但其他已登录会话会立即失效。确认继续？',
      confirmText: '确认修改',
      tone: 'danger',
    })
    if (!ok) return
    setSaving(true)
    try {
      const response = await updateAdminApiKey({ adminApiKey })
      storage.setApiKey(response.adminApiKey)
      window.dispatchEvent(new CustomEvent('kiro-admin-key-updated'))
      setKeysAndReset(response)
      setNextAdminKey('')
      toast.success('登录 Key 已更新，当前会话已自动切换')
    } catch (e) { toast.error(`更新失败: ${extractErrorMessage(e)}`) }
    finally { setSaving(false) }
  }

  if (loading) return <LoadingState text="加载访问密钥..." />

  const adminKeyValue = showAdminKey ? keys?.adminApiKey : keys?.maskedAdminApiKey

  return (
    <PageContainer>
      <PageHeader
        title="安全"
        subtitle="请求 Key 用于客户端调用模型接口，登录 Key 用于后台管理登录，两者相互独立"
      />

      {/* 新增 Key 一次性明文提示 */}
      {newKeyPlaintext && (
        <Callout tone="warning">
          <div className="space-y-2">
            <div className="font-semibold text-sm">新请求 Key 已生成，请立即复制保存</div>
            <div className="flex items-center gap-2">
              <code className="flex-1 rounded bg-muted px-2 py-1 font-mono text-xs break-all">{newKeyPlaintext}</code>
              <Button size="xs" variant="outline" onClick={() => copyText('新请求 Key', newKeyPlaintext ?? undefined)}>
                <Copy className="h-3.5 w-3.5" />复制
              </Button>
              <Button size="xs" variant="ghost" onClick={() => setNewKeyPlaintext(null)}>
                <X className="h-3.5 w-3.5" />
              </Button>
            </div>
            <div className="text-xs text-muted-foreground">关闭后无法再次查看完整 Key。</div>
          </div>
        </Callout>
      )}

      {/* 请求 Key 管理 */}
      <RequestKeysSection
        keys={keys}
        loading={loading}
        creating={creating}
        processingKeyId={processingKeyId}
        visibleIds={visibleIds}
        onOpenCreate={openCreateDialog}
        onOpenEdit={openEditDialog}
        onToggleVisible={(id) => setVisibleIds((prev) => { const next = new Set(prev); next.has(id) ? next.delete(id) : next.add(id); return next })}
        onDelete={handleDelete}
        defaultAdmission={keys?.defaultRequestAdmission ?? DEFAULT_REQUEST_ADMISSION}
      />

      <RequestApiKeyDialog
        open={keyDialogOpen}
        item={keyDialogItem}
        initialApiKey={keyDialogInitialApiKey}
        defaultAdmission={keys?.defaultRequestAdmission ?? DEFAULT_REQUEST_ADMISSION}
        saving={creating}
        onOpenChange={setKeyDialogOpen}
        onSave={handleSaveKeyDialog}
      />

      {/* 登录 Key 管理 */}
      <SectionCard
        title="后台登录 Key"
        description="这是登录页输入的密码，也用于管理后台的后续操作。修改后当前浏览器会自动切换新 Key。"
      >
        {/* 当前值查看 */}
        <div className="flex flex-col gap-2 sm:flex-row">
          <Input
            readOnly
            aria-label="当前后台登录 Key"
            className="w-full min-w-0 flex-1 font-mono text-xs"
            value={loading ? '加载中...' : adminKeyValue ?? '未配置'}
          />
          <div className="flex gap-2 sm:shrink-0">
            <Button size="sm" onClick={() => setShowAdminKey((v) => !v)}>
              {showAdminKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              {showAdminKey ? '隐藏' : '显示'}
            </Button>
            <Button size="sm" variant="outline" onClick={() => copyText('登录 Key', keys?.adminApiKey)}>
              <Copy className="h-4 w-4" />复制
            </Button>
          </div>
        </div>

        {/* 修改区域 */}
        <div className="mt-5 space-y-2">
          <div className="text-sm font-semibold">修改登录 Key</div>
          <div className="rounded-lg border border-amber-300 bg-amber-50 px-3 py-2 text-xs leading-5 text-amber-900 dark:border-amber-800/60 dark:bg-amber-950/30 dark:text-amber-100">
            高危操作：保存后旧 Key 立即失效；所有其他已登录会话需重新登录。当前页面会自动写入新 Key。
          </div>
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input
              type="password"
              className="w-full min-w-0 flex-1"
              value={nextAdminKey}
              placeholder="输入新的登录 Key（至少 8 个字符）"
              disabled={saving}
              onChange={(e) => setNextAdminKey(e.target.value)}
            />
            <Button
              size="sm"
              className="shrink-0"
              disabled={saving || !nextAdminKey.trim()}
              onClick={handleSaveAdminKey}
            >
              {saving ? <Spinner size="sm" /> : <KeyRound className="h-4 w-4" />}
              修改登录 Key
            </Button>
          </div>
        </div>
      </SectionCard>

      {/* 接入说明 */}
      <SectionCard title="客户端接入说明" description="如何配置客户端使用本代理">
        <div className="space-y-3 text-sm text-muted-foreground">
          <p>将客户端的 API Base URL 设置为本代理地址，API Key 设置为上方任意一个「请求调用 Key」。</p>
          <p>例如在 Claude Code 中：</p>
          <pre className="rounded-lg bg-muted px-3 py-2 text-xs font-mono overflow-x-auto">
{`ANTHROPIC_API_KEY=<请求 Key>
ANTHROPIC_BASE_URL=http://<代理地址>/`}
          </pre>
          <p>每个请求 Key 可独立分发给不同客户端，删除后对应客户端立即无法访问。</p>
        </div>
      </SectionCard>
    </PageContainer>
  )
}
