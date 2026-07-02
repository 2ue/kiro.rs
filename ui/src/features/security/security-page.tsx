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
  Input,
  Spinner,
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
import type { AccessKeysResponse, RequestApiKeyItem } from '@/types/api'

// ─── 工具 ──────────────────────────────────────────────────────────────────────

const REQUEST_API_KEY_PREFIX = 'sk-kiro-rs-'

function generateLocalRequestApiKey(): string {
  const bytes = new Uint8Array(32)
  const c = globalThis.crypto
  if (c?.getRandomValues) c.getRandomValues(bytes)
  else for (let i = 0; i < bytes.length; i++) bytes[i] = Math.floor(Math.random() * 256)
  const binary = Array.from(bytes, (b) => String.fromCharCode(b)).join('')
  return `${REQUEST_API_KEY_PREFIX}${btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}`
}

function accessKeyItems(response: AccessKeysResponse | null): RequestApiKeyItem[] {
  if (!response) return []
  if (response.requestApiKeys?.length) return response.requestApiKeys
  if (!response.requestApiKey) return []
  return [{ id: 'legacy-primary', apiKey: response.requestApiKey, maskedApiKey: response.maskedRequestApiKey, primary: true }]
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

// ─── RequestKeysSection ───────────────────────────────────────────────────────

interface RequestKeysSectionProps {
  keys: AccessKeysResponse | null
  loading: boolean
  creating: boolean
  processingKeyId: string | null
  visibleIds: Set<string>
  editingId: string | null
  editDraft: string
  manualDraft: string
  onManualDraftChange: (v: string) => void
  onGenerate: () => void
  onAddManual: () => void
  onToggleVisible: (id: string) => void
  onStartEdit: (item: RequestApiKeyItem) => void
  onCancelEdit: () => void
  onSaveEdit: (item: RequestApiKeyItem) => void
  onEditDraftChange: (v: string) => void
  onDelete: (item: RequestApiKeyItem) => void
}

function RequestKeysSection({
  keys,
  loading,
  creating,
  processingKeyId,
  visibleIds,
  editingId,
  editDraft,
  manualDraft,
  onManualDraftChange,
  onGenerate,
  onAddManual,
  onToggleVisible,
  onStartEdit,
  onCancelEdit,
  onSaveEdit,
  onEditDraftChange,
  onDelete,
}: RequestKeysSectionProps) {
  const requestKeys = accessKeyItems(keys)

  return (
    <SectionCard
      title="请求调用 Key"
      description="给客户端调用模型接口时使用。可以按客户端分配不同 Key，新增或删除后立即生效。"
      actions={
        <Button size="sm" disabled={loading || creating} onClick={onGenerate}>
          {creating ? <Spinner size="sm" /> : <Wand2 className="h-4 w-4" />}
          随机生成并新增
        </Button>
      }
    >
      {/* 手动新增行 */}
      <div className="flex flex-col gap-2 sm:flex-row">
        <Input
          className="w-full min-w-0 font-mono text-xs"
          value={manualDraft}
          placeholder="手动输入要新增的请求 Key"
          disabled={loading || creating}
          onChange={(e) => onManualDraftChange(e.target.value)}
        />
        <Button
          variant="outline"
          size="sm"
          className="shrink-0"
          disabled={loading || creating}
          onClick={() => onManualDraftChange(generateLocalRequestApiKey())}
        >
          <Wand2 className="h-4 w-4" />随机填充
        </Button>
        <Button
          size="sm"
          className="shrink-0"
          disabled={loading || creating || !manualDraft.trim()}
          onClick={onAddManual}
        >
          <Plus className="h-4 w-4" />新增
        </Button>
      </div>

      {/* Key 列表 */}
      <div className="mt-3 rounded-lg bg-muted/20">
        {loading && <div className="px-4 py-3 text-sm text-muted-foreground">加载中...</div>}
        {!loading && requestKeys.length === 0 && (
          <div className="px-4 py-3 text-sm text-destructive">未配置请求 Key，请先生成或手动添加。</div>
        )}
        {!loading && requestKeys.map((item) => {
          const visible = visibleIds.has(item.id)
          const busy = processingKeyId === item.id
          const editing = editingId === item.id
          return (
            <div key={item.id} className="px-4 py-3">
              <div className="mb-2 flex flex-col gap-2 md:flex-row md:items-center md:justify-between">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-semibold">请求 Key</span>
                  {item.primary && (
                    <Tooltip label="功能与其他请求 Key 相同，仅标记为首个创建的 Key">
                      <Badge tone="primary" className="cursor-default">主 Key</Badge>
                    </Tooltip>
                  )}
                  <span className="font-mono text-[0.68rem] text-muted-foreground">{item.id.slice(0, 12)}</span>
                </div>
                <div className="flex flex-wrap gap-2">
                  <Button variant="outline" size="xs" disabled={busy || editing} onClick={() => onToggleVisible(item.id)}>
                    {visible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                    {visible ? '隐藏' : '显示'}
                  </Button>
                  <Button variant="outline" size="xs" disabled={busy || editing} onClick={() => copyText('请求 Key', item.apiKey)}>
                    <Copy className="h-3.5 w-3.5" />复制
                  </Button>
                  {!editing && (
                    <Button variant="ghost" size="xs" disabled={busy || Boolean(editingId)} onClick={() => onStartEdit(item)}>
                      <Edit3 className="h-3.5 w-3.5" />编辑
                    </Button>
                  )}
                  <Button
                    variant="outline"
                    size="xs"
                    className="text-destructive hover:text-destructive"
                    disabled={busy || editing || requestKeys.length <= 1}
                    onClick={() => onDelete(item)}
                  >
                    <Trash2 className="h-3.5 w-3.5" />删除
                  </Button>
                </div>
              </div>
              <div className="space-y-2">
                <Input
                  readOnly={!editing}
                  aria-label="请求调用 Key"
                  className="w-full min-w-0 font-mono text-xs"
                  value={editing ? editDraft : visible ? item.apiKey : item.maskedApiKey}
                  disabled={busy}
                  onChange={(e) => onEditDraftChange(e.target.value)}
                />
                {editing && (
                  <div className="flex flex-wrap justify-end gap-2">
                    <Button variant="outline" size="sm" disabled={busy} onClick={() => onEditDraftChange(generateLocalRequestApiKey())}>
                      <Wand2 className="h-4 w-4" />随机生成
                    </Button>
                    <Button size="sm" disabled={busy || !editDraft.trim()} onClick={() => onSaveEdit(item)}>
                      {busy ? <Spinner size="sm" /> : <Save className="h-4 w-4" />}保存
                    </Button>
                    <Button variant="outline" size="sm" disabled={busy} onClick={onCancelEdit}>
                      <X className="h-4 w-4" />取消
                    </Button>
                  </div>
                )}
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
  const [manualDraft, setManualDraft] = useState('')
  const [visibleIds, setVisibleIds] = useState<Set<string>>(new Set())
  const [editingId, setEditingId] = useState<string | null>(null)
  const [editDraft, setEditDraft] = useState('')
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
    setEditingId(null)
    setEditDraft('')
    setVisibleIds((prev) => {
      const valid = new Set(accessKeyItems(response).map((i) => i.id))
      return new Set(Array.from(prev).filter((id) => valid.has(id)))
    })
  }

  const handleGenerate = async () => {
    setCreating(true)
    try {
      const before = new Set(accessKeyItems(keys).map((i) => i.id))
      const response = await createRequestApiKey({})
      setKeysAndReset(response)
      const created = accessKeyItems(response).find((i) => !before.has(i.id))
      if (created) {
        setNewKeyPlaintext(created.apiKey)
        setVisibleIds((prev) => new Set(prev).add(created.id))
      }
      toast.success('请求 Key 已生成并立即生效')
    } catch (e) { toast.error(`生成失败: ${extractErrorMessage(e)}`) }
    finally { setCreating(false) }
  }

  const handleAddManual = async () => {
    const apiKey = manualDraft.trim()
    if (!apiKey) return toast.error('请输入要新增的请求 Key')
    if (apiKey.length < 8) return toast.error('请求 Key 至少需要 8 个字符')
    setCreating(true)
    try {
      const response = await createRequestApiKey({ apiKey })
      setKeysAndReset(response)
      setManualDraft('')
      toast.success('请求 Key 已新增并立即生效')
    } catch (e) { toast.error(`新增失败: ${extractErrorMessage(e)}`) }
    finally { setCreating(false) }
  }

  const handleSaveEdit = async (item: RequestApiKeyItem) => {
    const apiKey = editDraft.trim()
    if (!apiKey) return toast.error('请输入新的请求 Key')
    if (apiKey.length < 8) return toast.error('请求 Key 至少需要 8 个字符')
    if (apiKey === item.apiKey) { setEditingId(null); setEditDraft(''); return }
    setProcessingKeyId(item.id)
    try {
      const response = await updateRequestApiKey(item.id, { apiKey })
      setKeysAndReset(response)
      toast.success('请求 Key 已保存，旧 Key 立即失效')
    } catch (e) { toast.error(`保存失败: ${extractErrorMessage(e)}`) }
    finally { setProcessingKeyId(null) }
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
        editingId={editingId}
        editDraft={editDraft}
        manualDraft={manualDraft}
        onManualDraftChange={setManualDraft}
        onGenerate={handleGenerate}
        onAddManual={handleAddManual}
        onToggleVisible={(id) => setVisibleIds((prev) => { const next = new Set(prev); next.has(id) ? next.delete(id) : next.add(id); return next })}
        onStartEdit={(item) => { setEditingId(item.id); setEditDraft(item.apiKey) }}
        onCancelEdit={() => { setEditingId(null); setEditDraft('') }}
        onSaveEdit={handleSaveEdit}
        onEditDraftChange={setEditDraft}
        onDelete={handleDelete}
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
          <div className="rounded-lg border border-warning/30 bg-warning/10 px-3 py-2 text-xs text-warning-foreground leading-5">
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
