import { useEffect, useMemo, useState } from 'react'
import { Check, Download, FileUp, Pencil, Trash2, X } from 'lucide-react'
import { toast } from 'sonner'
import { Badge, Button, Checkbox, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Spinner, Textarea } from '@/components/ui'
import { Callout, Field, FieldGrid, ModalShell, useConfirm } from '@/components/patterns'
import { exportInventory } from '@/api/inventory'
import { useProxyResources } from '@/hooks/use-credentials'
import {
  useDeleteInventoryGroup,
  useImportInventory,
  useInventoryGroups,
  usePushInventory,
  useRenameInventoryGroup,
  useSetInventoryGroup,
  useUpdateInventory,
} from '@/hooks/use-inventory'
import { parseImportInputs, readImportFiles, type ParsedImportAccount } from '@/lib/credential-import/sources'
import { downloadBlob, timestampedFilename } from '@/lib/download'
import { extractErrorMessage } from '@/lib/utils'
import type {
  AddCredentialRequest,
  InventoryActionResponse,
  InventoryExportFormat,
  InventoryExportScope,
  InventoryGroupItem,
  InventoryImportResponse,
  InventoryItem,
} from '@/types/api'
import { InventoryGroupPicker, INVENTORY_GROUP_NAME_MAX } from './inventory-group-picker'
import {
  CredentialParameterDefaultsPanel,
  FormSection,
  initialParameterDefaults,
  parameterDefaultsToImportDefaults,
  validateProxyAssignment,
} from '@/features/credentials/credential-dialogs'
import {
  ProxyAssignmentField,
  initialProxyAssignment,
  validateProxyAssignmentValue,
  type ProxyAssignmentValue,
} from '@/features/proxies/proxy-assignment-field'

/** 已有分组名（不含"未分组"）。 */
export function namedGroups(groups: InventoryGroupItem[] | undefined): string[] {
  return (groups ?? []).flatMap((group) => (group.name ? [group.name] : []))
}

// ============================================================================
// Import into the inventory: region and proxy only, no scheduling parameters.
// ============================================================================

export function InventoryImportModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [text, setText] = useState('')
  const [authRegion, setAuthRegion] = useState('us-east-1')
  const [apiRegion, setApiRegion] = useState('us-east-1')
  const [proxy, setProxy] = useState<ProxyAssignmentValue>(() => initialProxyAssignment('single'))
  const [check, setCheck] = useState(true)
  const [importProxy, setImportProxy] = useState(false)
  const [source, setSource] = useState('')
  const [group, setGroup] = useState('')
  const [result, setResult] = useState<InventoryImportResponse | null>(null)
  const [error, setError] = useState('')
  const proxyResources = useProxyResources()
  const proxies = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const importInventory = useImportInventory()
  const groups = useInventoryGroups(open)

  useEffect(() => {
    if (!open) {
      setText(''); setResult(null); setError(''); setSource(''); setGroup('')
      setProxy(initialProxyAssignment('single')); setCheck(true); setImportProxy(false)
    }
  }, [open])

  const parsedAccounts = useMemo<ParsedImportAccount[] | string>(() => {
    if (!text.trim()) return []
    const [input] = parseImportInputs([{ name: '粘贴内容', text }])
    return input.error ?? input.result?.accounts ?? []
  }, [text])
  // 来源文件里的代理只在勾选“导入代理”时使用；停用状态不进号库
  const parsed = useMemo<AddCredentialRequest[] | string>(() => {
    if (!Array.isArray(parsedAccounts)) return parsedAccounts
    return parsedAccounts.map((account) => (importProxy ? { ...account.credential, ...account.sourceProxy } : account.credential))
  }, [parsedAccounts, importProxy])
  const parsedCount = Array.isArray(parsed) ? parsed.length : 0
  const sourceProxyCount = Array.isArray(parsedAccounts) ? parsedAccounts.filter((account) => account.sourceProxy).length : 0

  const handleFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const read = await readImportFiles(files)
    const errors = [...read.errors]
    // 带上来源代理，重新解析时仍能识别出来；是否使用由“导入代理”决定
    const credentials = parseImportInputs(read.inputs).flatMap((input) => {
      if (input.error) errors.push(`${input.name}: ${input.error}`)
      return input.result?.accounts.map((account) => ({ ...account.credential, ...account.sourceProxy })) ?? []
    })
    if (!credentials.length) { toast.error(errors[0] || '文件中没有有效账号'); return }
    setText(credentials.map((c) => JSON.stringify(c)).join('\n'))
    setResult(null)
    toast.success(`已读取 ${credentials.length} 条账号`)
    if (errors.length) toast.warning(`部分文件未读取: ${errors.slice(0, 3).join('；')}`)
  }

  const submit = async () => {
    setError('')
    if (!Array.isArray(parsed)) { setError(parsed); return }
    if (!parsed.length) { setError('未找到有效账号，请检查格式'); return }
    if (!authRegion.trim()) { setError('请填写 Auth Region：刷新 token 和查询额度都依赖它'); return }
    const proxyError = validateProxyAssignmentValue(proxy, { requireSingle: true })
    if (proxyError) { setError(proxyError); return }
    try {
      const response = await importInventory.mutateAsync({
        credentials: parsed,
        check,
        source: source.trim() || undefined,
        group: group.trim() || undefined,
        defaults: {
          authRegion: authRegion.trim(),
          apiRegion: apiRegion.trim() || undefined,
          ...(proxy.mode === 'single' ? { proxyResourceId: Number(proxy.resourceId) } : {}),
          ...(proxy.mode === 'round_robin' ? { proxyResourceIds: proxy.resourceIds.map(Number) } : {}),
          ...(proxy.mode === 'direct_url' ? { proxyUrl: proxy.url.trim(), proxyUsername: proxy.username || undefined, proxyPassword: proxy.password || undefined } : {}),
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
      width="max-w-5xl"
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
        <div className="space-y-4 text-sm">
          <FormSection
            title="账号数据"
            actions={(
              <label className="cursor-pointer">
                <input type="file" className="sr-only" accept=".json,.jsonl,.txt,.csv" multiple onChange={(e) => void handleFile(e)} />
                <Button type="button" variant="outline" size="sm" asChild><span><FileUp className="h-3.5 w-3.5" />从文件导入</span></Button>
              </label>
            )}
          >
            <Textarea
              className="min-h-[160px] font-mono text-xs"
              placeholder={'[{"refreshToken":"eyJ..."}]\n// 或每行一个 JSON 对象；支持账号页导出的文件'}
              value={text}
              onChange={(e) => { setText(e.target.value); setResult(null) }}
            />
            <p className="mt-2 text-xs text-muted-foreground">
              {typeof parsed === 'string' ? <span className="text-destructive">{parsed}</span> : `识别到 ${parsedCount} 个账号。已在账号池使用的账号会被跳过。`}
            </p>
          </FormSection>

          <FormSection title="区域">
            <FieldGrid>
              <Field label="Auth Region" description="刷新 token 用；账号自带的值优先" required>
                <Input className="font-mono" value={authRegion} onChange={(e) => setAuthRegion(e.target.value)} placeholder="us-east-1" />
              </Field>
              <Field label="API Region" description="调用模型和查询额度用；留空则从 profileArn 推导">
                <Input className="font-mono" value={apiRegion} onChange={(e) => setApiRegion(e.target.value)} placeholder="us-east-1" />
              </Field>
            </FieldGrid>
          </FormSection>

          <FormSection title="代理" description="建议绑定代理：续期、查询和推送到账号池后的调用都走同一个出口，账号环境保持一致。勾选“导入代理”时，账号自带的代理优先。">
            <ProxyAssignmentField
              value={proxy}
              onChange={setProxy}
              proxyResources={proxies}
              modes={['single', 'round_robin', 'direct_url', 'none']}
            />
            <label className="mt-3 flex items-center gap-2">
              <Checkbox checked={importProxy} onCheckedChange={(value) => setImportProxy(value === true)} />
              <span>导入代理<span className="ml-1 text-xs text-muted-foreground">（{sourceProxyCount} 个账号自带代理；默认不导入）</span></span>
            </label>
            {proxy.mode === 'none' && <Callout tone="warning" className="mt-3">不使用代理时，续期和查询走本机出口，和推送后的调用出口可能不一致。</Callout>}
          </FormSection>

          <FormSection title="导入选项">
            <div className="grid gap-4 sm:grid-cols-2">
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
            <Field label="分组" description="可选；选已有分组或新建一个，库存页可按分组筛选">
              <InventoryGroupPicker value={group} groups={namedGroups(groups.data?.groups)} onChange={setGroup} className="w-full" />
            </Field>
            </div>
          </FormSection>
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
  const [restoreOrigin, setRestoreOrigin] = useState(true)
  const [result, setResult] = useState<InventoryActionResponse | null>(null)
  const [error, setError] = useState('')
  const proxyResources = useProxyResources()
  const proxies = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const push = usePushInventory()
  const keepsProxy = items.filter((item) => item.proxyResourceId ? item.proxyUsable : Boolean(item.proxyUrl))
  const needsProxy = items.length - keepsProxy.length
  const fromPool = items.filter((item) => item.origin).length

  useEffect(() => {
    if (!open) { setDefaults(initialParameterDefaults()); setResult(null); setError(''); setAutoDiscover(false); setRestoreOrigin(true) }
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
        restoreOriginSettings: restoreOrigin,
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
      width="max-w-5xl"
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
          <FormSection title="推送选项">
            {fromPool > 0 && (
              <label className="mb-2 flex items-start gap-2">
                <Checkbox checked={restoreOrigin} onCheckedChange={(value) => setRestoreOrigin(value === true)} className="mt-0.5" />
                <span>
                  恢复移入前的调度参数
                  <span className="ml-1 text-xs text-muted-foreground">（{fromPool} 个账号是从账号池移入的：沿用它们原来的优先级、RPM、并发和模型列表，上面的默认值只补它们没有的项）</span>
                </span>
              </label>
            )}
            <label className="flex items-center gap-2">
              <Checkbox checked={autoDiscover} onCheckedChange={(value) => setAutoDiscover(value === true)} />
              <span>自动发现模型限制（API Key 账号）</span>
            </label>
          </FormSection>
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
  const [group, setGroup] = useState('')
  const proxyResources = useProxyResources()
  const proxies = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const update = useUpdateInventory()
  const groups = useInventoryGroups(Boolean(item))

  useEffect(() => {
    if (item) {
      setAuthRegion(item.authRegion || '')
      setApiRegion(item.apiRegion || '')
      setProxyResourceId(item.proxyResourceId ? String(item.proxyResourceId) : '')
      setNote(item.note || '')
      setGroup(item.group || '')
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
          group: group.trim() || null,
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
        <Field label="分组">
          <InventoryGroupPicker value={group} groups={namedGroups(groups.data?.groups)} onChange={setGroup} className="w-full" />
        </Field>
      </FieldGrid>
    </ModalShell>
  )
}

// ============================================================================
// Set or clear the group of the selected accounts.
// ============================================================================

export function InventorySetGroupModal({ open, ids, onClose, onDone }: {
  open: boolean
  ids: number[]
  onClose: () => void
  onDone: () => void
}) {
  const [group, setGroup] = useState('')
  const groups = useInventoryGroups(open)
  const setInventoryGroup = useSetInventoryGroup()

  useEffect(() => { if (!open) setGroup('') }, [open])

  const save = async () => {
    const name = group.trim()
    try {
      const result = await setInventoryGroup.mutateAsync({ ids, group: name || null })
      toast.success(name ? `已把 ${result.updated} 个账号移到分组「${name}」` : `已清除 ${result.updated} 个账号的分组`)
      onDone()
      onClose()
    } catch (e) {
      toast.error(`设置分组失败：${extractErrorMessage(e)}`)
    }
  }

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title="设置分组"
      description={`选中的 ${ids.length} 个库存账号`}
      width="max-w-md"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={setInventoryGroup.isPending}>取消</Button>
          <Button onClick={() => void save()} disabled={setInventoryGroup.isPending}>
            {setInventoryGroup.isPending && <Spinner size="sm" />}{group.trim() ? '移到该分组' : '清除分组'}
          </Button>
        </>
      }
    >
      <Field label="分组" description="选已有分组、新建一个，或选“不分组”清除分组">
        <InventoryGroupPicker value={group} groups={namedGroups(groups.data?.groups)} onChange={setGroup} className="w-full" />
      </Field>
    </ModalShell>
  )
}

// ============================================================================
// Rename or dissolve groups.
// ============================================================================

function GroupRow({ group }: { group: InventoryGroupItem & { name: string } }) {
  const [editing, setEditing] = useState(false)
  const [name, setName] = useState(group.name)
  const rename = useRenameInventoryGroup()
  const dissolve = useDeleteInventoryGroup()
  const confirm = useConfirm()

  const save = async () => {
    const next = name.trim()
    if (!next || next === group.name) { setEditing(false); setName(group.name); return }
    try {
      await rename.mutateAsync({ name: group.name, newName: next })
      toast.success(`已重命名为「${next}」`)
      setEditing(false)
    } catch (e) {
      toast.error(`重命名失败：${extractErrorMessage(e)}`)
    }
  }

  const remove = async () => {
    const ok = await confirm({
      title: '解散分组',
      message: `分组「${group.name}」里的 ${group.total} 个账号会保留，变成未分组。`,
      confirmText: '解散',
    })
    if (!ok) return
    try {
      await dissolve.mutateAsync(group.name)
      toast.success(`已解散分组「${group.name}」`)
    } catch (e) {
      toast.error(`解散失败：${extractErrorMessage(e)}`)
    }
  }

  return (
    <div className="flex items-center justify-between gap-3 px-3 py-2 text-xs">
      {editing ? (
        <Input
          autoFocus
          value={name}
          maxLength={INVENTORY_GROUP_NAME_MAX}
          className="h-7 text-xs"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') void save(); if (e.key === 'Escape') { setEditing(false); setName(group.name) } }}
          aria-label="分组名"
        />
      ) : (
        <span className="min-w-0 truncate font-medium" title={group.name}>{group.name}</span>
      )}
      <div className="flex shrink-0 items-center gap-1">
        <span className="tabular-nums text-muted-foreground">在库 {group.stored} · 已入池 {group.pushed}{group.invalid ? ` · 失效 ${group.invalid}` : ''}</span>
        {editing ? (
          <>
            <Button variant="ghost" size="icon-xs" onClick={() => void save()} disabled={rename.isPending} aria-label="保存"><Check /></Button>
            <Button variant="ghost" size="icon-xs" onClick={() => { setEditing(false); setName(group.name) }} aria-label="取消"><X /></Button>
          </>
        ) : (
          <>
            <Button variant="ghost" size="icon-xs" onClick={() => setEditing(true)} aria-label={`重命名分组 ${group.name}`}><Pencil /></Button>
            <Button variant="ghost" size="icon-xs" className="text-destructive" onClick={() => void remove()} disabled={dissolve.isPending} aria-label={`解散分组 ${group.name}`}><Trash2 /></Button>
          </>
        )}
      </div>
    </div>
  )
}

export function InventoryGroupsModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const groups = useInventoryGroups(open)
  const named = (groups.data?.groups ?? []).filter((group): group is InventoryGroupItem & { name: string } => Boolean(group.name))
  const ungrouped = groups.data?.groups.find((group) => !group.name)
  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title="管理分组"
      description="重命名会作用到组内全部账号；解散分组不会删除账号。新分组在导入、编辑或设置分组时直接输入即可。"
      width="max-w-lg"
      footer={<Button onClick={onClose}>完成</Button>}
    >
      {groups.isLoading ? (
        <div className="flex justify-center py-6"><Spinner /></div>
      ) : named.length === 0 ? (
        <p className="py-6 text-center text-sm text-muted-foreground">还没有分组</p>
      ) : (
        <div className="max-h-[50vh] divide-y overflow-y-auto rounded-lg border">
          {named.map((group) => <GroupRow key={group.name} group={group} />)}
        </div>
      )}
      {ungrouped && <p className="mt-3 text-xs text-muted-foreground">未分组 {ungrouped.total} 个</p>}
    </ModalShell>
  )
}

// ============================================================================
// Export stored accounts in the pool's credential format.
// ============================================================================

export function InventoryExportModal({ open, onClose, selectedIds, filter, filterLabel, filteredCount }: {
  open: boolean
  onClose: () => void
  selectedIds: number[]
  /** 库存页当前的筛选条件。 */
  filter: { status?: string; group?: string; q?: string }
  filterLabel: string
  filteredCount: number
}) {
  const [format, setFormat] = useState<InventoryExportFormat>('json')
  const [scope, setScope] = useState<'selected' | 'filtered' | 'all'>('filtered')
  const [loading, setLoading] = useState(false)
  const filtered = Boolean(filter.status || filter.group || filter.q)

  useEffect(() => {
    if (open) setScope(selectedIds.length > 0 ? 'selected' : filtered ? 'filtered' : 'all')
  }, [open, selectedIds.length, filtered])

  const handleExport = async () => {
    setLoading(true)
    try {
      const exportScope: InventoryExportScope = scope === 'selected'
        ? { ids: selectedIds }
        : { filter: scope === 'filtered' ? filter : {} }
      const { blob, count, skipped } = await exportInventory(format, exportScope)
      downloadBlob(blob, timestampedFilename('kiro-inventory', format))
      toast.success(`已导出 ${count ?? ''} 个账号${skipped ? `，跳过已入池 ${skipped} 个（不含密钥）` : ''}`)
      onClose()
    } catch (e) {
      toast.error(`导出失败：${extractErrorMessage(e)}`)
    } finally {
      setLoading(false)
    }
  }

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title="导出库存账号"
      width="max-w-md"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={loading}>取消</Button>
          <Button onClick={() => void handleExport()} disabled={loading}>
            {loading ? <Spinner size="sm" /> : <Download />}导出
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <Field label="导出格式" description="和账号页导出的格式一致，可以直接导回账号池或库存。">
          <Select value={format} onValueChange={(value) => setFormat(value as InventoryExportFormat)} disabled={loading}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="json">JSON 数组</SelectItem>
              <SelectItem value="jsonl">JSONL（每行一个）</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="导出范围" description="只导出在库和失效的账号；已入池的账号由账号池保管，库存里不含密钥。">
          <Select value={scope} onValueChange={(value) => setScope(value as typeof scope)} disabled={loading}>
            <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
            <SelectContent>
              {selectedIds.length > 0 && <SelectItem value="selected">选中账号（{selectedIds.length} 个）</SelectItem>}
              {filtered && <SelectItem value="filtered">当前筛选：{filterLabel}（{filteredCount} 个）</SelectItem>}
              <SelectItem value="all">全部库存账号</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Callout tone="warning">导出文件包含 refreshToken 等密钥，请妥善保管。</Callout>
      </div>
    </ModalShell>
  )
}
