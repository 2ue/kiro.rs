import * as React from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, Download, Pencil, Play, Plus, RotateCcw, Route, Save, Trash2, Upload, X } from 'lucide-react'
import { toast } from 'sonner'
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
} from '@/components/ui'
import {
  Callout,
  EmptyState,
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  SectionCard,
  useConfirm,
} from '@/components/patterns'
import {
  applyRouteScenario,
  createRouteScenario,
  deleteRouteScenario,
  getCurrentRouteScenarioContent,
  getRouteScenarioOptions,
  getRouteScenarios,
  previewRouteScenario,
  updateRouteScenario,
} from '@/api/credentials'
import { formatDate } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import type {
  AccountRoutePolicy,
  ExternalPoolRouteMode,
  RoutePolicy,
  RouteScenario,
  RouteScenarioAccountOption,
  RouteScenarioApplyResponse,
  RouteScenarioContent,
  RouteScenarioItemKey,
  RouteScenarioPreview,
} from '@/types/api'
import { pageMeta } from '@/types/ui'
import { RouteSelector } from './route-selector'

const MODE_LABELS: Record<ExternalPoolRouteMode, string> = {
  allow_all: '不限制',
  allow_list: '仅允许所选路由',
  deny_list: '禁止所选路由',
}

const SCENARIO_FILE_FORMAT = 'kiro-rs-route-scenario'

function emptyContent(): RouteScenarioContent {
  return {
    local: { routeMode: 'allow_all', routeRules: [] },
    external: { routeMode: 'allow_all', routeRules: [] },
    localAccounts: [],
    externalAccounts: [],
  }
}

export function policySummary(policy: RoutePolicy): string {
  if (policy.routeMode === 'allow_all') return '不限制'
  const routes = policy.routeRules.join('、') || '（未选路由）'
  return policy.routeMode === 'allow_list' ? `仅允许 ${routes}` : `禁止 ${routes}`
}

/** Route policy editor: mode plus a route picker (routes are selected, never typed). */
function PolicyEditor({
  value,
  routes,
  onChange,
  idPrefix,
}: {
  value: RoutePolicy
  routes: string[]
  onChange: (next: RoutePolicy) => void
  idPrefix: string
}) {
  return (
    <div className="grid gap-3 md:grid-cols-[14rem_1fr]">
      <div className="space-y-1.5">
        <label className="text-xs font-medium text-muted-foreground" htmlFor={`${idPrefix}-mode`}>模式</label>
        <Select
          value={value.routeMode}
          onValueChange={(mode) => onChange({ routeMode: mode as ExternalPoolRouteMode, routeRules: mode === 'allow_all' ? [] : value.routeRules })}
        >
          <SelectTrigger id={`${idPrefix}-mode`} size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            {(Object.keys(MODE_LABELS) as ExternalPoolRouteMode[]).map((mode) => (
              <SelectItem key={mode} value={mode}>{MODE_LABELS[mode]}</SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="min-w-0 space-y-1.5">
        <div className="text-xs font-medium text-muted-foreground">路由</div>
        <RouteSelector
          routes={routes}
          value={value.routeRules}
          disabled={value.routeMode === 'allow_all'}
          onChange={(routeRules) => onChange({ ...value, routeRules })}
        />
      </div>
    </div>
  )
}

/** Accounts with their own rule; every unlisted account is reset to "unrestricted". */
function AccountRulesEditor({
  title,
  description,
  accounts,
  value,
  routes,
  onChange,
}: {
  title: string
  description: string
  accounts: RouteScenarioAccountOption[]
  value: AccountRoutePolicy[]
  routes: string[]
  onChange: (next: AccountRoutePolicy[]) => void
}) {
  const [picking, setPicking] = React.useState(false)
  const [selected, setSelected] = React.useState<Set<number>>(new Set())
  const [batchPolicy, setBatchPolicy] = React.useState<RoutePolicy>({ routeMode: 'deny_list', routeRules: [] })
  const byId = React.useMemo(() => new Map(accounts.map((account) => [account.id, account])), [accounts])
  const listed = new Set(value.map((entry) => entry.id))

  const update = (id: number, policy: RoutePolicy) =>
    onChange(value.map((entry) => (entry.id === id ? { id, ...policy } : entry)))
  const remove = (ids: Set<number>) => {
    onChange(value.filter((entry) => !ids.has(entry.id)))
    setSelected(new Set())
  }
  const applyBatch = () => {
    onChange(value.map((entry) => (selected.has(entry.id) ? { id: entry.id, ...batchPolicy } : entry)))
    setSelected(new Set())
  }
  const toggle = (id: number, checked: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev)
      if (checked) next.add(id)
      else next.delete(id)
      return next
    })

  return (
    <SectionCard
      title={title}
      description={description}
      actions={<Button size="sm" variant="outline" onClick={() => setPicking(true)}><Plus />添加账号</Button>}
    >
      {value.length === 0 ? (
        <p className="text-sm text-muted-foreground">没有单独规则的账号，全部按“不限制”处理。</p>
      ) : (
        <div className="space-y-3">
          {selected.size > 0 && (
            <div className="space-y-3 rounded-md border bg-muted/40 p-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-sm font-medium">已选 {selected.size} 个账号，批量设置为：</span>
                <div className="flex gap-2">
                  <Button size="sm" onClick={applyBatch} disabled={batchPolicy.routeMode === 'allow_list' && batchPolicy.routeRules.length === 0}>应用到所选</Button>
                  <Button size="sm" variant="outline" onClick={() => remove(selected)}><Trash2 />移除所选</Button>
                </div>
              </div>
              <PolicyEditor value={batchPolicy} routes={routes} onChange={setBatchPolicy} idPrefix={`${title}-batch`} />
            </div>
          )}
          <div className="divide-y rounded-md border">
            {value.map((entry) => {
              const account = byId.get(entry.id)
              return (
                <div key={entry.id} className="space-y-3 p-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <Checkbox
                      checked={selected.has(entry.id)}
                      onCheckedChange={(checked) => toggle(entry.id, Boolean(checked))}
                      aria-label={`选择账号 #${entry.id}`}
                    />
                    <span className="font-mono text-xs text-muted-foreground">#{entry.id}</span>
                    <span className="min-w-0 truncate text-sm font-medium">{account?.label ?? '账号已不存在'}</span>
                    {!account && <Badge tone="error">已删除，应用时跳过</Badge>}
                    <Button className="ml-auto" size="icon-xs" variant="ghost" aria-label={`移除账号 #${entry.id}`} onClick={() => remove(new Set([entry.id]))}><X /></Button>
                  </div>
                  <PolicyEditor value={entry} routes={routes} onChange={(policy) => update(entry.id, policy)} idPrefix={`${title}-${entry.id}`} />
                </div>
              )
            })}
          </div>
        </div>
      )}
      <AccountPicker
        open={picking}
        title={`添加${title}`}
        accounts={accounts.filter((account) => !listed.has(account.id))}
        onClose={() => setPicking(false)}
        onAdd={(ids) => {
          onChange([...value, ...ids.map((id) => ({ id, routeMode: 'deny_list' as const, routeRules: [] }))])
          setPicking(false)
        }}
      />
    </SectionCard>
  )
}

function AccountPicker({
  open,
  title,
  accounts,
  onClose,
  onAdd,
}: {
  open: boolean
  title: string
  accounts: RouteScenarioAccountOption[]
  onClose: () => void
  onAdd: (ids: number[]) => void
}) {
  const [search, setSearch] = React.useState('')
  const [picked, setPicked] = React.useState<Set<number>>(new Set())
  React.useEffect(() => {
    if (!open) {
      setSearch('')
      setPicked(new Set())
    }
  }, [open])
  const keyword = search.trim().toLowerCase()
  const visible = accounts.filter((account) =>
    !keyword || account.label.toLowerCase().includes(keyword) || String(account.id).includes(keyword),
  )
  const allVisiblePicked = visible.length > 0 && visible.every((account) => picked.has(account.id))
  const toggleAll = () =>
    setPicked((prev) => {
      const next = new Set(prev)
      for (const account of visible) {
        if (allVisiblePicked) next.delete(account.id)
        else next.add(account.id)
      }
      return next
    })

  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title={title}
      description="选中的账号会加入场景，默认规则为“禁止所选路由”，加入后再设置路由。"
      footer={
        <>
          <Button variant="outline" onClick={onClose}>取消</Button>
          <Button onClick={() => onAdd(Array.from(picked))} disabled={picked.size === 0}>添加 {picked.size || ''}</Button>
        </>
      }
    >
      <div className="space-y-3">
        <Input placeholder="按名称、email 或 ID 搜索" value={search} onChange={(event) => setSearch(event.target.value)} aria-label="搜索账号" />
        {visible.length === 0 ? (
          <p className="py-6 text-center text-sm text-muted-foreground">没有可添加的账号</p>
        ) : (
          <>
            <label className="flex items-center gap-2 text-sm">
              <Checkbox checked={allVisiblePicked} onCheckedChange={toggleAll} aria-label="全选当前列表" />
              全选当前列表（{visible.length}）
            </label>
            <div className="max-h-96 divide-y overflow-y-auto rounded-md border">
              {visible.map((account) => (
                <label key={account.id} className="flex cursor-pointer items-center gap-3 p-2.5 hover:bg-muted/50">
                  <Checkbox
                    checked={picked.has(account.id)}
                    onCheckedChange={(checked) =>
                      setPicked((prev) => {
                        const next = new Set(prev)
                        if (checked) next.add(account.id)
                        else next.delete(account.id)
                        return next
                      })
                    }
                    aria-label={`选择 ${account.label}`}
                  />
                  <span className="font-mono text-xs text-muted-foreground">#{account.id}</span>
                  <span className="min-w-0 flex-1 truncate text-sm">{account.label}</span>
                  <span className="shrink-0 text-xs text-muted-foreground">当前：{policySummary(account)}</span>
                </label>
              ))}
            </div>
          </>
        )}
      </div>
    </ModalShell>
  )
}

type EditorState = { id: number | null; name: string; description: string; content: RouteScenarioContent }

function ScenarioEditor({
  state,
  routes,
  localAccounts,
  externalAccounts,
  saving,
  onChange,
  onCancel,
  onSave,
}: {
  state: EditorState
  routes: string[]
  localAccounts: RouteScenarioAccountOption[]
  externalAccounts: RouteScenarioAccountOption[]
  saving: boolean
  onChange: (next: EditorState) => void
  onCancel: () => void
  onSave: () => void
}) {
  const setContent = (patch: Partial<RouteScenarioContent>) => onChange({ ...state, content: { ...state.content, ...patch } })
  const known = new Set(routes)
  const unknownRoutes = Array.from(new Set([
    state.content.local,
    state.content.external,
    ...state.content.localAccounts,
    ...state.content.externalAccounts,
  ].flatMap((policy) => policy.routeRules).filter((route) => !known.has(route))))

  return (
    <div className="space-y-4">
      <SectionCard
        title={state.id ? '编辑场景' : '新建场景'}
        description="场景只保存路由规则。应用时写回本地整体、外部整体和各账号的路由设置，调度方式不变。"
        actions={
          <div className="flex gap-2">
            <Button variant="outline" onClick={onCancel} disabled={saving}>取消</Button>
            <Button onClick={onSave} disabled={saving}><Save />{saving ? '保存中...' : '保存场景'}</Button>
          </div>
        }
      >
        <div className="grid gap-3 md:grid-cols-2">
          <div className="space-y-1.5">
            <label className="text-sm font-medium" htmlFor="scenario-name">名称</label>
            <Input id="scenario-name" value={state.name} maxLength={64} onChange={(event) => onChange({ ...state, name: event.target.value })} placeholder="例如：日常、本地额度紧张" />
          </div>
          <div className="space-y-1.5">
            <label className="text-sm font-medium" htmlFor="scenario-description">说明</label>
            <Textarea id="scenario-description" className="min-h-9" value={state.description} maxLength={500} onChange={(event) => onChange({ ...state, description: event.target.value })} placeholder="可选" />
          </div>
        </div>
        {unknownRoutes.length > 0 && (
          <Callout tone="warning" className="mt-3">
            这些路由已不在路由策略里：{unknownRoutes.join('、')}。请在下面移除，或先到路由策略页补回。
          </Callout>
        )}
      </SectionCard>

      <SectionCard title="本地整体" description="哪些路由可以进入本地账号。">
        <PolicyEditor value={state.content.local} routes={routes} onChange={(local) => setContent({ local })} idPrefix="local-pool" />
      </SectionCard>
      <AccountRulesEditor
        title="本地账号"
        description="只对需要单独限制的账号添加规则；没列出的本地账号应用时恢复为“不限制”。"
        accounts={localAccounts}
        value={state.content.localAccounts}
        routes={routes}
        onChange={(localAccounts) => setContent({ localAccounts })}
      />
      <SectionCard title="外部整体" description="哪些路由可以进入外部账号。">
        <PolicyEditor value={state.content.external} routes={routes} onChange={(external) => setContent({ external })} idPrefix="external-pool" />
      </SectionCard>
      <AccountRulesEditor
        title="外部账号"
        description="只对需要单独限制的外部账号添加规则；没列出的外部账号应用时恢复为“不限制”。"
        accounts={externalAccounts}
        value={state.content.externalAccounts}
        routes={routes}
        onChange={(externalAccounts) => setContent({ externalAccounts })}
      />
    </div>
  )
}

function itemKey(item: RouteScenarioItemKey) {
  return `${item.kind}:${item.id}`
}

function isPoolItem(item: RouteScenarioItemKey) {
  return item.kind === 'local_pool' || item.kind === 'external_pool'
}

const KIND_LABELS: Record<RouteScenarioItemKey['kind'], string> = {
  local_pool: '本地整体',
  external_pool: '外部整体',
  local_account: '本地账号',
  external_account: '外部账号',
}

function PreviewDialog({
  scenario,
  preview,
  loading,
  applying,
  onClose,
  onApply,
}: {
  scenario: RouteScenario | null
  preview: RouteScenarioPreview | undefined
  loading: boolean
  applying: boolean
  onClose: () => void
  onApply: () => void
}) {
  const uncovered = [
    ...(preview?.uncoveredLocalRoutes ?? []).map((route) => `本地：${route}`),
    ...(preview?.uncoveredExternalRoutes ?? []).map((route) => `外部：${route}`),
  ]
  return (
    <ModalShell
      open={Boolean(scenario)}
      onClose={onClose}
      width="max-w-3xl"
      title={`应用场景 · ${scenario?.name ?? ''}`}
      description="确认下面的变化后应用。没有变化的项不会写入。"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={applying}>取消</Button>
          <Button onClick={onApply} disabled={loading || applying || !preview}><Play />{applying ? '应用中...' : '确认应用'}</Button>
        </>
      }
    >
      {loading || !preview ? (
        <LoadingState text="计算变化..." />
      ) : (
        <div className="space-y-3">
          {uncovered.length > 0 && (
            <Callout tone="error">这些路由在应用后没有任何可用账号，请求会失败：{uncovered.join('、')}</Callout>
          )}
          {preview.unknownRoutes.length > 0 && (
            <Callout tone="warning">场景里有已不存在的路由：{preview.unknownRoutes.join('、')}</Callout>
          )}
          {preview.missingAccounts.length > 0 && (
            <Callout tone="warning">
              场景里有已删除的账号，应用时跳过：{preview.missingAccounts.map((item) => `${KIND_LABELS[item.kind]} #${item.id}`).join('、')}
            </Callout>
          )}
          <p className="text-sm text-muted-foreground">将修改 {preview.changes.length} 项，{preview.unchanged} 项保持不变。</p>
          {preview.changes.length > 0 && (
            <div className="max-h-96 divide-y overflow-y-auto rounded-md border text-sm">
              {preview.changes.map((change) => (
                <div key={itemKey(change)} className="grid gap-1 p-2.5 md:grid-cols-[10rem_1fr]">
                  <div className="min-w-0">
                    {isPoolItem(change) ? (
                      <div className="truncate font-medium">{KIND_LABELS[change.kind]}</div>
                    ) : (
                      <>
                        <div className="text-xs text-muted-foreground">{KIND_LABELS[change.kind]} #{change.id}</div>
                        <div className="truncate font-medium" title={change.label}>{change.label}</div>
                      </>
                    )}
                  </div>
                  <div className="min-w-0 text-xs leading-5">
                    <div className="text-muted-foreground line-through">{change.before}</div>
                    <div>{change.after}</div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </ModalShell>
  )
}

function ResultDialog({
  result,
  retrying,
  onRetry,
  onClose,
}: {
  result: { scenario: RouteScenario; response: RouteScenarioApplyResponse } | null
  retrying: boolean
  onRetry: () => void
  onClose: () => void
}) {
  const response = result?.response
  const failed = response?.items.filter((item) => !item.success) ?? []
  return (
    <ModalShell
      open={Boolean(result)}
      onClose={onClose}
      width="max-w-3xl"
      title={`应用结果 · ${result?.scenario.name ?? ''}`}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={retrying}>关闭</Button>
          {failed.length > 0 && (
            <Button onClick={onRetry} disabled={retrying}><RotateCcw />{retrying ? '重试中...' : `重试失败的 ${failed.length} 项`}</Button>
          )}
        </>
      }
    >
      {response && (
        <div className="space-y-3">
          <div className="flex flex-wrap gap-2">
            <Badge tone="success">成功 {response.success}</Badge>
            <Badge tone={response.failed ? 'error' : 'neutral'}>失败 {response.failed}</Badge>
            <Badge tone="neutral">无变化跳过 {response.skipped}</Badge>
          </div>
          {response.items.length === 0 ? (
            <p className="text-sm text-muted-foreground">所有项已经是场景里的值，没有写入。</p>
          ) : (
            <div className="max-h-96 divide-y overflow-y-auto rounded-md border text-sm">
              {[...failed, ...response.items.filter((item) => item.success)].map((item) => (
                <div key={itemKey(item)} className="flex items-start gap-3 p-2.5">
                  <Badge tone={item.success ? 'success' : 'error'}>{item.success ? '成功' : '失败'}</Badge>
                  <div className="min-w-0">
                    <div className="truncate font-medium">{isPoolItem(item) ? KIND_LABELS[item.kind] : `${KIND_LABELS[item.kind]} #${item.id} · ${item.label}`}</div>
                    {item.error && <div className="text-xs text-destructive">{item.error}</div>}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </ModalShell>
  )
}

function downloadScenario(scenario: RouteScenario) {
  const file = { format: SCENARIO_FILE_FORMAT, version: 1, name: scenario.name, description: scenario.description ?? '', content: scenario.content }
  const blob = new Blob([`${JSON.stringify(file, null, 2)}\n`], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `route-scenario-${scenario.name.replace(/[^\p{L}\p{N}._-]+/gu, '-') || scenario.id}.json`
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}

export function parseScenarioFile(text: string): { name: string; description: string; content: RouteScenarioContent } {
  const data = JSON.parse(text)
  if (!data || data.format !== SCENARIO_FILE_FORMAT || typeof data.content !== 'object') {
    throw new Error(`不是路由场景文件：缺少 "format": "${SCENARIO_FILE_FORMAT}"`)
  }
  const policy = (value: unknown): RoutePolicy => {
    const raw = (value ?? {}) as Partial<RoutePolicy>
    const mode = raw.routeMode && raw.routeMode in MODE_LABELS ? raw.routeMode : 'allow_all'
    return { routeMode: mode, routeRules: Array.isArray(raw.routeRules) ? raw.routeRules.filter((rule): rule is string => typeof rule === 'string') : [] }
  }
  const accounts = (value: unknown): AccountRoutePolicy[] =>
    (Array.isArray(value) ? value : [])
      .filter((entry) => entry && Number.isInteger(entry.id) && entry.id > 0)
      .map((entry) => ({ id: entry.id, ...policy(entry) }))
  return {
    name: typeof data.name === 'string' ? data.name : '',
    description: typeof data.description === 'string' ? data.description : '',
    content: {
      local: policy(data.content.local),
      external: policy(data.content.external),
      localAccounts: accounts(data.content.localAccounts),
      externalAccounts: accounts(data.content.externalAccounts),
    },
  }
}

export function RouteScenariosPage() {
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const scenarios = useQuery({ queryKey: ['route-scenarios'], queryFn: getRouteScenarios })
  const options = useQuery({ queryKey: ['route-scenario-options'], queryFn: getRouteScenarioOptions })
  const [editor, setEditor] = React.useState<EditorState | null>(null)
  const [previewing, setPreviewing] = React.useState<RouteScenario | null>(null)
  const [result, setResult] = React.useState<{ scenario: RouteScenario; response: RouteScenarioApplyResponse } | null>(null)
  const fileInput = React.useRef<HTMLInputElement>(null)

  const preview = useQuery({
    queryKey: ['route-scenario-preview', previewing?.id],
    queryFn: () => previewRouteScenario(previewing!.id),
    enabled: Boolean(previewing),
    staleTime: 0,
  })

  const refreshAfterApply = () => {
    queryClient.invalidateQueries({ queryKey: ['route-scenarios'] })
    queryClient.invalidateQueries({ queryKey: ['route-scenario-options'] })
    queryClient.invalidateQueries({ queryKey: ['runtime-config'] })
    queryClient.invalidateQueries({ queryKey: ['external-pools'] })
    queryClient.invalidateQueries({ queryKey: ['external-pools-status'] })
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    queryClient.invalidateQueries({ queryKey: ['credential-list'] })
    queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
  }

  const save = useMutation({
    mutationFn: (state: EditorState) => {
      const req = { name: state.name.trim(), description: state.description.trim() || undefined, content: state.content }
      return state.id ? updateRouteScenario(state.id, req) : createRouteScenario(req)
    },
    onSuccess: () => {
      toast.success('场景已保存')
      setEditor(null)
      queryClient.invalidateQueries({ queryKey: ['route-scenarios'] })
    },
    onError: (error) => toast.error(`保存失败：${extractErrorMessage(error)}`),
  })

  const apply = useMutation({
    mutationFn: ({ scenario, only }: { scenario: RouteScenario; only: RouteScenarioItemKey[] }) => applyRouteScenario(scenario.id, only),
    onSuccess: (response, { scenario, only }) => {
      setPreviewing(null)
      refreshAfterApply()
      if (only.length > 0 && result) {
        // Merge retry outcomes into the previous result so every item stays visible.
        const retried = new Map(response.items.map((item) => [itemKey(item), item]))
        const items = result.response.items.map((item) => retried.get(itemKey(item)) ?? item)
        const success = items.filter((item) => item.success).length
        setResult({ scenario, response: { ...result.response, items, success, failed: items.length - success } })
      } else {
        setResult({ scenario, response })
      }
      if (response.failed === 0) toast.success(only.length > 0 ? '失败项已重试成功' : `场景“${scenario.name}”已应用`)
      else toast.error(`${response.failed} 项写入失败，可在结果里重试`)
    },
    onError: (error) => toast.error(`应用失败：${extractErrorMessage(error)}`),
  })

  const remove = async (scenario: RouteScenario) => {
    const ok = await confirm({ title: '删除场景？', message: `删除“${scenario.name}”只删除这份场景，不会改动当前路由设置。`, confirmText: '删除', tone: 'danger' })
    if (!ok) return
    try {
      await deleteRouteScenario(scenario.id)
      toast.success('场景已删除')
      queryClient.invalidateQueries({ queryKey: ['route-scenarios'] })
    } catch (error) {
      toast.error(`删除失败：${extractErrorMessage(error)}`)
    }
  }

  const startFromCurrent = async () => {
    try {
      const content = await getCurrentRouteScenarioContent()
      setEditor({ id: null, name: '', description: '', content })
    } catch (error) {
      toast.error(`读取当前设置失败：${extractErrorMessage(error)}`)
    }
  }

  const importFile = async (file: File) => {
    try {
      const parsed = parseScenarioFile(await file.text())
      setEditor({ id: null, ...parsed })
    } catch (error) {
      toast.error(`导入失败：${extractErrorMessage(error)}`)
    }
  }

  if (scenarios.isLoading || options.isLoading) return <LoadingState text="加载路由场景..." />
  if (scenarios.error || options.error) return <ErrorState message={extractErrorMessage(scenarios.error ?? options.error)} />

  const list = scenarios.data?.scenarios ?? []
  const routes = options.data?.routes ?? []
  const activeId = scenarios.data?.activeScenarioId ?? null

  if (editor) {
    return (
      <PageContainer>
        <PageHeader title={pageMeta.routeScenarios.title} subtitle={pageMeta.routeScenarios.subtitle} />
        <ScenarioEditor
          state={editor}
          routes={routes}
          localAccounts={options.data?.localAccounts ?? []}
          externalAccounts={options.data?.externalAccounts ?? []}
          saving={save.isPending}
          onChange={setEditor}
          onCancel={() => setEditor(null)}
          onSave={() => {
            if (!editor.name.trim()) {
              toast.error('请填写场景名称')
              return
            }
            save.mutate(editor)
          }}
        />
      </PageContainer>
    )
  }

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.routeScenarios.title}
        subtitle={pageMeta.routeScenarios.subtitle}
        actions={
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => fileInput.current?.click()}><Upload />导入</Button>
            <Button variant="outline" onClick={startFromCurrent}><Copy />从当前设置生成</Button>
            <Button onClick={() => setEditor({ id: null, name: '', description: '', content: emptyContent() })}><Plus />新建场景</Button>
          </div>
        }
      />
      <input
        ref={fileInput}
        type="file"
        accept="application/json,.json"
        className="hidden"
        onChange={(event) => {
          const file = event.target.files?.[0]
          event.target.value = ''
          if (file) void importFile(file)
        }}
      />
      {list.length === 0 ? (
        <EmptyState
          icon={<Route />}
          title="还没有路由场景"
          description="可以从当前设置生成第一个场景，再按需要修改。"
          action={<Button onClick={startFromCurrent}><Copy />从当前设置生成</Button>}
        />
      ) : (
        <div className="grid gap-4 xl:grid-cols-2">
          {list.map((scenario) => {
            const active = scenario.id === activeId
            const accountCount = scenario.content.localAccounts.length + scenario.content.externalAccounts.length
            return (
              <SectionCard
                key={scenario.id}
                title={
                  <span className="flex flex-wrap items-center gap-2">
                    {scenario.name}
                    {active && <Badge tone={scenarios.data?.activeInSync ? 'success' : 'warning'} dot>{scenarios.data?.activeInSync ? '使用中' : '使用中（已被手动修改）'}</Badge>}
                  </span>
                }
                description={scenario.description || undefined}
                actions={
                  <div className="flex shrink-0 gap-1">
                    <Button size="icon-xs" variant="ghost" title="编辑" aria-label={`编辑 ${scenario.name}`} onClick={() => setEditor({ id: scenario.id, name: scenario.name, description: scenario.description ?? '', content: scenario.content })}><Pencil /></Button>
                    <Button size="icon-xs" variant="ghost" title="复制" aria-label={`复制 ${scenario.name}`} onClick={() => setEditor({ id: null, name: `${scenario.name} 副本`, description: scenario.description ?? '', content: structuredClone(scenario.content) })}><Copy /></Button>
                    <Button size="icon-xs" variant="ghost" title="导出 JSON" aria-label={`导出 ${scenario.name}`} onClick={() => downloadScenario(scenario)}><Download /></Button>
                    <Button size="icon-xs" variant="ghost" className="text-destructive hover:text-destructive" title="删除" aria-label={`删除 ${scenario.name}`} onClick={() => void remove(scenario)}><Trash2 /></Button>
                  </div>
                }
              >
                <div className="space-y-2 text-sm">
                  <div><span className="text-muted-foreground">本地整体：</span>{policySummary(scenario.content.local)}</div>
                  <div><span className="text-muted-foreground">外部整体：</span>{policySummary(scenario.content.external)}</div>
                  <div className="text-muted-foreground">
                    单独规则：本地 {scenario.content.localAccounts.length} 个、外部 {scenario.content.externalAccounts.length} 个账号
                    {accountCount === 0 && '（其余账号全部不限制）'}
                  </div>
                  <div className="flex items-center justify-between gap-2 pt-1">
                    <span className="text-xs text-muted-foreground">更新于 {formatDate(scenario.updatedAt)}</span>
                    <Button size="sm" onClick={() => setPreviewing(scenario)}><Play />应用</Button>
                  </div>
                </div>
              </SectionCard>
            )
          })}
        </div>
      )}
      <PreviewDialog
        scenario={previewing}
        preview={preview.data}
        loading={preview.isFetching}
        applying={apply.isPending}
        onClose={() => setPreviewing(null)}
        onApply={() => previewing && apply.mutate({ scenario: previewing, only: [] })}
      />
      <ResultDialog
        result={result}
        retrying={apply.isPending}
        onClose={() => setResult(null)}
        onRetry={() => {
          if (!result) return
          const only = result.response.items.filter((item) => !item.success).map(({ kind, id }) => ({ kind, id }))
          apply.mutate({ scenario: result.scenario, only })
        }}
      />
    </PageContainer>
  )
}
