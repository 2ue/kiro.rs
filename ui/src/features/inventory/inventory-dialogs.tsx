import { useEffect, useMemo, useState } from 'react'
import { FileUp } from 'lucide-react'
import { toast } from 'sonner'
import { Badge, Button, Checkbox, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Textarea } from '@/components/ui'
import { Callout, Field, FieldGrid, ModalShell } from '@/components/patterns'
import { useProxyResources } from '@/hooks/use-credentials'
import { useImportInventory, usePushInventory, useUpdateInventory } from '@/hooks/use-inventory'
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import { extractErrorMessage } from '@/lib/utils'
import type { AddCredentialRequest, InventoryActionResponse, InventoryImportResponse, InventoryItem } from '@/types/api'
import {
  CredentialParameterDefaultsPanel,
  initialParameterDefaults,
  parameterDefaultsToImportDefaults,
  validateProxyAssignment,
} from '@/features/credentials/credential-dialogs'
import { SecretInput } from '@/features/credentials/credential-inputs'

type ProxyMode = 'single' | 'round_robin' | 'direct_url' | 'none'

// ============================================================================
// Import into the inventory: region and proxy only, no scheduling parameters.
// ============================================================================

export function InventoryImportModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [text, setText] = useState('')
  const [authRegion, setAuthRegion] = useState('us-east-1')
  const [apiRegion, setApiRegion] = useState('us-east-1')
  const [proxyMode, setProxyMode] = useState<ProxyMode>('single')
  const [proxyResourceId, setProxyResourceId] = useState('')
  const [proxyResourceIds, setProxyResourceIds] = useState<string[]>([])
  const [proxyUrl, setProxyUrl] = useState('')
  const [proxyUsername, setProxyUsername] = useState('')
  const [proxyPassword, setProxyPassword] = useState('')
  const [showSecret, setShowSecret] = useState(false)
  const [check, setCheck] = useState(true)
  const [source, setSource] = useState('')
  const [result, setResult] = useState<InventoryImportResponse | null>(null)
  const [error, setError] = useState('')
  const proxyResources = useProxyResources()
  const proxies = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const importInventory = useImportInventory()

  useEffect(() => {
    if (!open) {
      setText(''); setResult(null); setError(''); setSource('')
      setProxyMode('single'); setProxyResourceId(''); setProxyResourceIds([])
      setProxyUrl(''); setProxyUsername(''); setProxyPassword(''); setCheck(true)
    }
  }, [open])

  const parsed = useMemo<AddCredentialRequest[] | string>(() => {
    if (!text.trim()) return []
    try { return parseCredentialImportText(text) } catch (e) { return extractErrorMessage(e) }
  }, [text])
  const parsedCount = Array.isArray(parsed) ? parsed.length : 0

  const handleFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const read = await parseCredentialImportFiles(files)
    if (!read.credentials.length) { toast.error(read.errors[0] || '文件中没有有效账号'); return }
    setText(read.credentials.map((c) => JSON.stringify(c)).join('\n'))
    setResult(null)
    toast.success(`已读取 ${read.credentials.length} 条账号`)
    if (read.errors.length) toast.warning(`部分文件未读取: ${read.errors.slice(0, 3).join('；')}`)
  }

  const submit = async () => {
    setError('')
    if (!Array.isArray(parsed)) { setError(parsed); return }
    if (!parsed.length) { setError('未找到有效账号，请检查格式'); return }
    if (!authRegion.trim()) { setError('请填写 Auth Region：刷新 token 和查询额度都依赖它'); return }
    if (proxyMode === 'single' && !proxyResourceId) { setError('请选择代理资源，或改为"不使用代理"'); return }
    if (proxyMode === 'round_robin' && proxyResourceIds.length === 0) { setError('请至少选择一个轮换代理'); return }
    if (proxyMode === 'direct_url' && !proxyUrl.trim()) { setError('请填写代理 URL'); return }
    try {
      const response = await importInventory.mutateAsync({
        credentials: parsed,
        check,
        source: source.trim() || undefined,
        defaults: {
          authRegion: authRegion.trim(),
          apiRegion: apiRegion.trim() || undefined,
          ...(proxyMode === 'single' ? { proxyResourceId: Number(proxyResourceId) } : {}),
          ...(proxyMode === 'round_robin' ? { proxyResourceIds: proxyResourceIds.map(Number) } : {}),
          ...(proxyMode === 'direct_url' ? { proxyUrl: proxyUrl.trim(), proxyUsername: proxyUsername || undefined, proxyPassword: proxyPassword || undefined } : {}),
        },
      })
      setResult(response)
      toast.success(`入库 ${response.imported}，跳过 ${response.skipped}，失败 ${response.failed}`)
    } catch (e) {
      setError(extractErrorMessage(e))
    }
  }

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title="导入到库存"
      description="库存账号不参与调度，只做保管和自动续期。优先级、并发等调度参数在推送到账号池时再选。"
      width="max-w-3xl"
      footer={result ? (
        <>
          <Button variant="outline" onClick={() => setResult(null)}>继续导入</Button>
          <Button onClick={onClose}>完成</Button>
        </>
      ) : (
        <>
          <Button variant="outline" onClick={onClose} disabled={importInventory.isPending}>取消</Button>
          <Button onClick={() => void submit()} disabled={importInventory.isPending || parsedCount === 0}>
            {importInventory.isPending ? (check ? '导入并查询中…' : '导入中…') : parsedCount ? `导入 ${parsedCount} 个账号` : '导入'}
          </Button>
        </>
      )}
    >
      {result ? (
        <div className="space-y-3 text-sm">
          <div className="flex flex-wrap gap-2">
            <Badge tone="success">入库 {result.imported}</Badge>
            <Badge tone="warning">跳过 {result.skipped}</Badge>
            <Badge tone="error">失败 {result.failed}</Badge>
          </div>
          <div className="max-h-[50vh] divide-y overflow-y-auto rounded-lg border">
            {result.items.map((item) => (
              <div key={item.index} className="flex items-start justify-between gap-3 px-3 py-2 text-xs">
                <span className="min-w-0 truncate">#{item.index} {item.email || ''}{item.inventoryId ? ` → 库存 #${item.inventoryId}` : ''}</span>
                <span className={item.ok ? (item.message ? 'text-warning' : 'text-success') : item.skipped ? 'text-muted-foreground' : 'text-destructive'}>
                  {item.message || (item.ok ? '已入库' : '失败')}
                </span>
              </div>
            ))}
          </div>
        </div>
      ) : (
        <div className="space-y-5 text-sm">
          <section className="space-y-2">
            <div className="flex items-center justify-between gap-2">
              <h3 className="text-[13px] font-medium">账号数据</h3>
              <label className="cursor-pointer">
                <input type="file" className="sr-only" accept=".json,.jsonl,.txt" multiple onChange={(e) => void handleFile(e)} />
                <Button type="button" variant="outline" size="sm" asChild><span><FileUp className="h-3.5 w-3.5" />从文件导入</span></Button>
              </label>
            </div>
            <Textarea
              className="min-h-[120px] font-mono text-xs"
              placeholder={'[{"refreshToken":"eyJ..."}]\n// 或每行一个 JSON 对象；支持账号页导出的文件'}
              value={text}
              onChange={(e) => { setText(e.target.value); setResult(null) }}
            />
            <p className="text-xs text-muted-foreground">
              {typeof parsed === 'string' ? <span className="text-destructive">{parsed}</span> : `识别到 ${parsedCount} 个账号。已在账号池使用的账号会被跳过。`}
            </p>
          </section>

          <section className="space-y-2">
            <h3 className="text-[13px] font-medium">区域</h3>
            <FieldGrid>
              <Field label="Auth Region" description="刷新 token 用；账号自带的值优先" required>
                <Input className="font-mono" value={authRegion} onChange={(e) => setAuthRegion(e.target.value)} placeholder="us-east-1" />
              </Field>
              <Field label="API Region" description="调用模型和查询额度用；留空则从 profileArn 推导">
                <Input className="font-mono" value={apiRegion} onChange={(e) => setApiRegion(e.target.value)} placeholder="us-east-1" />
              </Field>
            </FieldGrid>
          </section>

          <section className="space-y-2">
            <h3 className="text-[13px] font-medium">代理</h3>
            <p className="text-xs leading-5 text-muted-foreground">
              建议绑定代理：续期、查询和推送到账号池后的调用都走同一个出口，账号环境保持一致。账号自带代理时优先使用账号的。
            </p>
            <Select value={proxyMode} onValueChange={(value) => setProxyMode(value as ProxyMode)}>
              <SelectTrigger size="sm" className="sm:w-60"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="single">绑定一个代理资源</SelectItem>
                <SelectItem value="round_robin">多个代理资源轮换</SelectItem>
                <SelectItem value="direct_url">直接填写代理 URL</SelectItem>
                <SelectItem value="none">不使用代理</SelectItem>
              </SelectContent>
            </Select>
            {proxyMode === 'single' && (
              <Select value={proxyResourceId || '__none__'} onValueChange={(value) => setProxyResourceId(value === '__none__' ? '' : value)}>
                <SelectTrigger size="sm" className="sm:w-80"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__none__">选择代理资源</SelectItem>
                  {proxies.map((resource) => <SelectItem key={resource.id} value={String(resource.id)}>{resource.name}</SelectItem>)}
                </SelectContent>
              </Select>
            )}
            {proxyMode === 'round_robin' && (
              <div className="grid max-h-40 gap-1 overflow-y-auto rounded-md border p-2 sm:grid-cols-2">
                {proxies.map((resource) => (
                  <label key={resource.id} className="flex min-w-0 items-center gap-2 rounded px-2 py-1 hover:bg-muted/60">
                    <Checkbox
                      checked={proxyResourceIds.includes(String(resource.id))}
                      onCheckedChange={(checked) => setProxyResourceIds((current) => checked === true ? [...current, String(resource.id)] : current.filter((id) => id !== String(resource.id)))}
                    />
                    <span className="truncate">{resource.name}</span>
                  </label>
                ))}
                {proxies.length === 0 && <span className="text-xs text-muted-foreground">没有可用的代理资源</span>}
              </div>
            )}
            {proxyMode === 'direct_url' && (
              <FieldGrid>
                <Field label="代理 URL"><Input value={proxyUrl} onChange={(e) => setProxyUrl(e.target.value)} placeholder="socks5h://host:port" /></Field>
                <Field label="用户名"><SecretInput value={proxyUsername} onChange={setProxyUsername} visible={showSecret} onToggle={() => setShowSecret((v) => !v)} placeholder="可选" /></Field>
                <Field label="密码"><SecretInput value={proxyPassword} onChange={setProxyPassword} visible={showSecret} onToggle={() => setShowSecret((v) => !v)} placeholder="可选" /></Field>
              </FieldGrid>
            )}
            {proxyMode === 'none' && <Callout tone="warning">不使用代理时，续期和查询走本机出口，和推送后的调用出口可能不一致。</Callout>}
          </section>

          <section className="space-y-2">
            <label className="flex items-start gap-2 rounded-lg border px-3 py-2.5">
              <Checkbox checked={check} onCheckedChange={(value) => setCheck(value === true)} className="mt-0.5" />
              <span>
                <span className="block text-[13px] font-medium">导入时查询额度和订阅</span>
                <span className="block text-xs text-muted-foreground">刷新一次 token 并查询额度、订阅。查询失败的账号也会入库，卡片上标出原因。</span>
              </span>
            </label>
            <Field label="来源备注" description="可选，例如批次号、渠道">
              <Input value={source} onChange={(e) => setSource(e.target.value)} placeholder="例如 2026-10 批次" />
            </Field>
          </section>
          {error && <Callout tone="error">{error}</Callout>}
        </div>
      )}
    </ModalShell>
  )
}

// ============================================================================
// Push to the pool: the pool import's parameter panel, without the JSON.
// ============================================================================

export function InventoryPushModal({ open, items, onClose, onDone }: {
  open: boolean
  items: InventoryItem[]
  onClose: () => void
  onDone: () => void
}) {
  const [defaults, setDefaults] = useState(initialParameterDefaults)
  const [autoDiscover, setAutoDiscover] = useState(false)
  const [result, setResult] = useState<InventoryActionResponse | null>(null)
  const [error, setError] = useState('')
  const proxyResources = useProxyResources()
  const proxies = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const push = usePushInventory()
  const keepsProxy = items.filter((item) => item.proxyResourceId ? item.proxyUsable : Boolean(item.proxyUrl))
  const needsProxy = items.length - keepsProxy.length

  useEffect(() => {
    if (!open) { setDefaults(initialParameterDefaults()); setResult(null); setError(''); setAutoDiscover(false) }
  }, [open])

  const submit = async () => {
    setError('')
    const proxyError = validateProxyAssignment(defaults)
    if (proxyError) { setError(proxyError); return }
    try {
      const response = await push.mutateAsync({
        ids: items.map((item) => item.id),
        defaults: parameterDefaultsToImportDefaults(defaults),
        autoDiscoverSupportedModels: autoDiscover,
      })
      setResult(response)
      if (response.failed === 0) toast.success(`已推送 ${response.succeeded} 个账号到账号池`)
      else toast.warning(`推送成功 ${response.succeeded}，失败 ${response.failed}`)
      onDone()
    } catch (e) {
      setError(extractErrorMessage(e))
    }
  }

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={`推送 ${items.length} 个账号到账号池`}
      description="推送后账号由账号池接管：账号池负责续期和调度，库存只保留记录并显示账号池里的状态。"
      width="max-w-3xl"
      footer={result ? <Button onClick={onClose}>完成</Button> : (
        <>
          <Button variant="outline" onClick={onClose} disabled={push.isPending}>取消</Button>
          <Button onClick={() => void submit()} disabled={push.isPending || items.length === 0}>{push.isPending ? '推送中…' : '推送到账号池'}</Button>
        </>
      )}
    >
      {result ? (
        <div className="max-h-[55vh] divide-y overflow-y-auto rounded-lg border text-xs">
          {result.items.map((item) => {
            const account = items.find((i) => i.id === item.id)
            return (
              <div key={item.id} className="flex items-start justify-between gap-3 px-3 py-2">
                <span className="min-w-0 truncate">库存 #{item.id} {account?.email || ''}</span>
                <span className={item.ok ? 'text-success' : 'text-destructive'}>{item.ok ? `已入池 #${item.credentialId}` : item.message}</span>
              </div>
            )
          })}
        </div>
      ) : (
        <div className="space-y-4 text-sm">
          <Callout tone="info">
            {keepsProxy.length > 0 && `${keepsProxy.length} 个账号保留原来的代理（代理仍可用）。`}
            {needsProxy > 0 && `${needsProxy} 个账号没有可用代理，使用下面选择的代理。`}
            区域沿用库存里的设置；下面的区域只填补账号缺失的值。
          </Callout>
          <CredentialParameterDefaultsPanel
            defaults={defaults}
            onChange={setDefaults}
            proxyResources={proxies}
            hideProxy={needsProxy === 0}
          />
          <label className="flex items-center gap-2 rounded-lg bg-muted/30 px-3 py-2.5">
            <Checkbox checked={autoDiscover} onCheckedChange={(value) => setAutoDiscover(value === true)} />
            <span>自动发现模型限制（API Key 账号）</span>
          </label>
          {error && <Callout tone="error">{error}</Callout>}
        </div>
      )}
    </ModalShell>
  )
}

// ============================================================================
// Edit region, proxy and note of one stored account.
// ============================================================================

export function InventoryEditModal({ item, onClose }: { item: InventoryItem | null; onClose: () => void }) {
  const [authRegion, setAuthRegion] = useState('')
  const [apiRegion, setApiRegion] = useState('')
  const [proxyResourceId, setProxyResourceId] = useState('')
  const [note, setNote] = useState('')
  const proxyResources = useProxyResources()
  const proxies = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const update = useUpdateInventory()

  useEffect(() => {
    if (item) {
      setAuthRegion(item.authRegion || '')
      setApiRegion(item.apiRegion || '')
      setProxyResourceId(item.proxyResourceId ? String(item.proxyResourceId) : '')
      setNote(item.note || '')
    }
  }, [item])

  const save = async () => {
    if (!item) return
    try {
      await update.mutateAsync({
        id: item.id,
        payload: {
          authRegion,
          apiRegion,
          proxyResourceId: proxyResourceId ? Number(proxyResourceId) : null,
          note,
        },
      })
      toast.success('已保存')
      onClose()
    } catch (e) {
      toast.error(`保存失败：${extractErrorMessage(e)}`)
    }
  }

  return (
    <ModalShell
      open={Boolean(item)}
      onClose={onClose}
      title={`编辑库存账号 #${item?.id ?? ''}`}
      width="max-w-lg"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>取消</Button>
          <Button onClick={() => void save()} disabled={update.isPending}>保存</Button>
        </>
      }
    >
      <FieldGrid>
        <Field label="Auth Region"><Input className="font-mono" value={authRegion} onChange={(e) => setAuthRegion(e.target.value)} /></Field>
        <Field label="API Region"><Input className="font-mono" value={apiRegion} onChange={(e) => setApiRegion(e.target.value)} /></Field>
        <Field label="代理资源" description={item?.proxyUrl ? `当前直连代理 ${item.proxyUrl}；选择代理资源会替换它` : undefined}>
          <Select value={proxyResourceId || '__none__'} onValueChange={(value) => setProxyResourceId(value === '__none__' ? '' : value)}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="__none__">不绑定</SelectItem>
              {proxies.map((resource) => <SelectItem key={resource.id} value={String(resource.id)}>{resource.name}</SelectItem>)}
            </SelectContent>
          </Select>
        </Field>
        <Field label="备注"><Input value={note} onChange={(e) => setNote(e.target.value)} /></Field>
      </FieldGrid>
    </ModalShell>
  )
}
