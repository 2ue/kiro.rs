// 统一的账号导入：自动识别来源（也可以手动指定），预览后按设定的并发导入。

import { AlertCircle, CheckCircle2, FileUp, Loader2, Play, Square, X, XCircle } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'
import { addCredential, deleteCredential, getCredentialBalance, setCredentialDisabled, testCredential } from '@/api/credentials'
import {
  Badge,
  Button,
  Checkbox,
  Input,
  Progress,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Textarea,
} from '@/components/ui'
import { Callout, Field, ModalShell } from '@/components/patterns'
import { useProxyResources } from '@/hooks/use-credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import {
  IMPORT_SOURCE_OPTIONS,
  importSourceLabel,
  parseImportInputs,
  readImportFiles,
  type ImportSource,
  type ParsedImportAccount,
} from '@/lib/credential-import/sources'
import { buildTestModelOptions, defaultTestModelForOptions, DEFAULT_TEST_PROMPT, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage, sha256Hex } from '@/lib/utils'
import type { AddCredentialRequest } from '@/types/api'
import {
  CredentialParameterDefaultsPanel,
  FormSection,
  initialParameterDefaults,
  mergeCredentialDefaults,
  validateProxyAssignment,
} from './credential-dialogs'
import { authLabel, credentialSubscriptionLabel, providerLabel } from './credential-utils'

type ImportVerificationMode = 'model_and_subscription' | 'subscription_only'
type RowStatus = 'pending' | 'importing' | 'verifying' | 'success' | 'failed' | 'skipped'

const MAX_CONCURRENCY = 20
const PASTE_INPUT_NAME = '粘贴内容'
const DATA_PLACEHOLDER = [
  '支持：本项目导出、KAM / Kiro-Go 导出包、kiro-manager-lite 导出或整库备份、Kiro-Go config.json、',
  'JSON 数组 / JSONL、KAM / lite 的卡密 / CSV / TXT 文本、每行一个 ksk_ API Key（可带 |region 或 ----region）',
].join('\n')

const SOURCE_STATUS_LABELS: Record<string, string> = {
  banned: '封禁',
  suspended: '暂停',
  disabled: '上游停用',
  error: '错误',
  trashed: '回收站',
  expired: '已过期',
  refreshing: '刷新中',
  unknown: '未知',
}

/** 来源工具已经判定为不可用的状态，默认跳过。 */
const FLAGGED_SOURCE_STATUSES = new Set(['banned', 'suspended', 'disabled', 'error', 'trashed'])

interface ImportRow {
  label: string
  /** 来自哪个文件，或粘贴内容。 */
  origin: string
  account: ParsedImportAccount
  request: AddCredentialRequest
  status: RowStatus
  /** 跳过原因：预览时查出的重复 / 来源异常，或服务端查重。 */
  skipReason?: string
  credentialId?: number
  error?: string
  warning?: string
  model?: string
  response?: string
}

function sourceStatusLabel(status: string) {
  return SOURCE_STATUS_LABELS[status] ?? status
}

function maskSecret(value: string) {
  return value.length > 14 ? `${value.slice(0, 8)}…${value.slice(-4)}` : value
}

function accountLabel(credential: AddCredentialRequest, index: number) {
  return credential.email || (credential.kiroApiKey ? maskSecret(credential.kiroApiKey) : `账号 ${index + 1}`)
}

function accountSecret(credential: AddCredentialRequest) {
  return credential.kiroApiKey || credential.refreshToken || ''
}

/** 只有用户勾选时，才把来源文件里的代理和停用状态写进请求。 */
function withSourceFields(account: ParsedImportAccount, importProxy: boolean, importDisabled: boolean): AddCredentialRequest {
  return {
    ...account.credential,
    ...(importDisabled && account.sourceDisabled !== undefined ? { disabled: account.sourceDisabled } : {}),
    ...(importProxy && account.sourceProxy ? account.sourceProxy : {}),
  }
}

/** 服务端查重命中：AdminServiceError::Conflict（HTTP 409，“凭据已存在（... 重复）”）。 */
function isDuplicateCredentialError(error: unknown) {
  const status = (error as { response?: { status?: number } } | null)?.response?.status
  return status === 409 || extractErrorMessage(error).includes('凭据已存在')
}

function countBy<T>(items: T[], key: (item: T) => string | undefined): Array<[string, number]> {
  const counts = new Map<string, number>()
  for (const item of items) {
    const value = key(item)
    if (value) counts.set(value, (counts.get(value) ?? 0) + 1)
  }
  return Array.from(counts.entries())
}

async function verifyImportedCredential(
  credentialId: number,
  mode: ImportVerificationMode,
  model: string,
  refreshInfoAfterModelTest: boolean,
): Promise<{ model: string; response: string }> {
  if (mode === 'subscription_only') {
    const info = await getCredentialBalance(credentialId)
    return { model: '订阅查询', response: `订阅: ${credentialSubscriptionLabel(info.subscriptionTitle)}，用量 ${info.currentUsage}/${info.usageLimit}` }
  }
  const tested = await testCredential(credentialId, { model, prompt: DEFAULT_TEST_PROMPT })
  if (refreshInfoAfterModelTest) {
    try { await getCredentialBalance(credentialId) } catch { /* ignore */ }
  }
  return { model: testModelLabel(tested.model), response: tested.response }
}

async function rollbackCredential(id: number): Promise<{ success: boolean; error?: string }> {
  try { await setCredentialDisabled(id, true) } catch (e) { return { success: false, error: `禁用失败: ${extractErrorMessage(e)}` } }
  try { await deleteCredential(id); return { success: true } } catch (e) { return { success: false, error: `删除失败: ${extractErrorMessage(e)}` } }
}

function RowStatusIcon({ status }: { status: RowStatus }) {
  if (status === 'success') return <CheckCircle2 className="h-4 w-4 text-success" />
  if (status === 'failed') return <XCircle className="h-4 w-4 text-destructive" />
  if (status === 'importing' || status === 'verifying') return <Loader2 className="h-4 w-4 animate-spin text-primary" />
  if (status === 'skipped') return <AlertCircle className="h-4 w-4 text-warning" />
  return <div className="h-4 w-4 rounded-full bg-muted" />
}

function ImportRowList({ rows }: { rows: ImportRow[] }) {
  return (
    <div className="space-y-1">
      {rows.map((row, index) => {
        const provider = providerLabel(row.request.provider)
        const sourceStatus = row.account.sourceStatus
        return (
          <div key={index} className="flex items-start gap-2 rounded-lg bg-muted/30 px-3 py-2">
            <div className="mt-0.5 shrink-0"><RowStatusIcon status={row.status} /></div>
            <div className="min-w-0 flex-1 text-xs">
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="min-w-0 truncate font-semibold">{row.label}</span>
                <Badge size="xs">{authLabel(row.request.authMethod)}</Badge>
                {provider && <Badge tone="info" size="xs">{provider}</Badge>}
                {sourceStatus && (
                  <Badge tone={FLAGGED_SOURCE_STATUSES.has(sourceStatus) ? 'warning' : 'neutral'} size="xs">
                    来源状态：{sourceStatusLabel(sourceStatus)}
                  </Badge>
                )}
                {row.request.disabled && <Badge size="xs">导入后禁用</Badge>}
                {row.credentialId !== undefined && <span className="text-muted-foreground">#{row.credentialId}</span>}
                <span className="ml-auto shrink-0 text-muted-foreground">{row.origin}</span>
              </div>
              {row.status === 'importing' && <div className="text-primary">导入中…</div>}
              {row.status === 'verifying' && <div className="text-primary">验活中…</div>}
              {row.status === 'success' && row.model && (
                <div className="truncate text-muted-foreground">{row.model}: {row.response?.slice(0, 80)}</div>
              )}
              {row.skipReason && <div className="text-warning">{row.skipReason}</div>}
              {row.warning && <div className="text-warning">{row.warning}</div>}
              {row.error && <div className="text-destructive">{row.error}</div>}
            </div>
          </div>
        )
      })}
    </div>
  )
}

export function ImportCredentialsModal({ open, onClose, existingCredentials, onDone }: {
  open: boolean
  onClose: () => void
  existingCredentials: Array<{ id: number; refreshTokenHash?: string; apiKeyHash?: string; tags?: string[] }>
  onDone: () => void
}) {
  const [text, setText] = useState('')
  const [files, setFiles] = useState<Array<{ name: string; text: string }>>([])
  const [source, setSource] = useState<ImportSource>('auto')
  const [importProxy, setImportProxy] = useState(false)
  const [importDisabled, setImportDisabled] = useState(false)
  const [skipFlagged, setSkipFlagged] = useState(true)
  const [defaults, setDefaults] = useState(initialParameterDefaults)
  const [verifyMode, setVerifyMode] = useState<ImportVerificationMode>('subscription_only')
  const [skipVerify, setSkipVerify] = useState(false)
  const [refreshInfoAfterModelTest, setRefreshInfoAfterModelTest] = useState(false)
  const [autoDiscoverSupportedModels, setAutoDiscoverSupportedModels] = useState(false)
  const [concurrency, setConcurrency] = useState('1')
  const [rows, setRows] = useState<ImportRow[]>([])
  const [running, setRunning] = useState(false)
  const [stopping, setStopping] = useState(false)
  const [formError, setFormError] = useState('')
  const cancelRef = useRef(false)
  const proxyResources = useProxyResources()
  const proxyOptions = (proxyResources.data?.resources || []).filter((resource) => resource.enabled)
  const tagOptions = useMemo(
    () => Array.from(new Set(existingCredentials.flatMap((credential) => credential.tags ?? []))).sort((a, b) => a.localeCompare(b)),
    [existingCredentials],
  )
  const modelCapabilities = useModelCapabilities()
  const testModelOptions = useMemo(
    () => buildTestModelOptions(modelCapabilities.data?.models),
    [modelCapabilities.data?.models],
  )
  const importTestModel = defaultTestModelForOptions(testModelOptions)

  useEffect(() => {
    if (!open) {
      setText('')
      setFiles([])
      setSource('auto')
      setImportProxy(false)
      setImportDisabled(false)
      setSkipFlagged(true)
      setDefaults(initialParameterDefaults())
      setVerifyMode('subscription_only')
      setSkipVerify(false)
      setRefreshInfoAfterModelTest(false)
      setAutoDiscoverSupportedModels(false)
      setConcurrency('1')
      setRows([])
      setRunning(false)
      setStopping(false)
      setFormError('')
    }
  }, [open])

  const parsedInputs = useMemo(() => {
    const pasted = text.trim() ? [{ name: PASTE_INPUT_NAME, text }] : []
    return parseImportInputs([...files, ...pasted], source)
  }, [files, text, source])
  const parsedAccounts = useMemo(
    () => parsedInputs.flatMap((input) => (input.result?.accounts ?? []).map((account) => ({ account, origin: input.name }))),
    [parsedInputs],
  )
  const skippedEntries = useMemo(
    () => parsedInputs.flatMap((input) => (input.result?.skipped ?? []).map((reason) => `${input.name} · ${reason}`)),
    [parsedInputs],
  )
  const stats = useMemo(() => {
    const accounts = parsedAccounts.map(({ account }) => account)
    const flagged = accounts.filter((account) => account.sourceStatus && FLAGGED_SOURCE_STATUSES.has(account.sourceStatus))
    return {
      auth: countBy(accounts, (account) => authLabel(account.credential.authMethod)),
      provider: countBy(accounts, (account) => providerLabel(account.credential.provider)),
      withProxy: accounts.filter((account) => account.sourceProxy).length,
      withDisabled: accounts.filter((account) => account.sourceDisabled === true).length,
      flaggedTotal: flagged.length,
      flagged: countBy(flagged, (account) => (account.sourceStatus ? sourceStatusLabel(account.sourceStatus) : undefined)),
    }
  }, [parsedAccounts])

  const existingHashes = useMemo(() => {
    const hashes = new Set<string>()
    for (const credential of existingCredentials) {
      if (credential.refreshTokenHash) hashes.add(credential.refreshTokenHash)
      if (credential.apiKeyHash) hashes.add(credential.apiKeyHash)
    }
    return hashes
  }, [existingCredentials])

  const handleFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const selected = Array.from(event.target.files || [])
    event.target.value = ''
    if (!selected.length) return
    const read = await readImportFiles(selected)
    setFiles((prev) => [...prev.filter((file) => !read.inputs.some((input) => input.name === file.name)), ...read.inputs])
    if (read.errors.length) toast.warning(`部分文件未读取: ${read.errors.slice(0, 3).join('；')}`)
  }

  /** 合并默认参数，并在文件内、与账号池两层去重；跳过的条目在预览里直接标出原因。 */
  const handlePreview = async () => {
    setFormError('')
    const workerCount = Number(concurrency)
    if (!Number.isInteger(workerCount) || workerCount < 1 || workerCount > MAX_CONCURRENCY) {
      setFormError(`并发数必须是 1 到 ${MAX_CONCURRENCY} 的整数`)
      return
    }
    const proxyError = validateProxyAssignment(defaults)
    if (proxyError) { setFormError(proxyError); return }
    if (!parsedAccounts.length) { setFormError('没有可导入的账号，请检查内容或来源'); return }
    try {
      const firstSeen = new Map<string, number>()
      const nextRows: ImportRow[] = []
      let assignIndex = 0
      for (const [index, { account, origin }] of parsedAccounts.entries()) {
        const secret = accountSecret(account.credential)
        const duplicateOf = firstSeen.get(secret)
        let skipReason: string | undefined
        if (duplicateOf !== undefined) {
          skipReason = `与第 ${duplicateOf + 1} 条重复`
        } else {
          firstSeen.set(secret, index)
          if (existingHashes.has(await sha256Hex(secret))) skipReason = '账号池中已存在'
          else if (skipFlagged && account.sourceStatus && FLAGGED_SOURCE_STATUSES.has(account.sourceStatus)) {
            skipReason = `来源标记为${sourceStatusLabel(account.sourceStatus)}，已跳过`
          }
        }
        // 代理轮换只在实际导入的账号之间循环
        const request = mergeCredentialDefaults(
          withSourceFields(account, importProxy, importDisabled),
          defaults,
          skipReason ? 0 : assignIndex++,
        )
        nextRows.push({
          label: accountLabel(request, index),
          origin,
          account,
          request,
          status: skipReason ? 'skipped' : 'pending',
          skipReason,
        })
      }
      setRows(nextRows)
    } catch (error) {
      setFormError(extractErrorMessage(error))
    }
  }

  const importRow = async (
    row: ImportRow,
    update: (patch: Partial<ImportRow>) => void,
  ): Promise<RowStatus> => {
    update({ status: 'importing', skipReason: undefined, error: undefined, warning: undefined, model: undefined, response: undefined })
    let credentialId: number
    try {
      const response = await addCredential({ ...row.request, autoDiscoverSupportedModels })
      credentialId = response.credentialId
      update({ credentialId, warning: response.warning, label: response.email || row.label })
    } catch (error) {
      if (isDuplicateCredentialError(error)) {
        update({ status: 'skipped', skipReason: `服务端查重：${extractErrorMessage(error)}` })
        return 'skipped'
      }
      update({ status: 'failed', error: extractErrorMessage(error) })
      return 'failed'
    }
    if (skipVerify) {
      update({ status: 'success' })
      return 'success'
    }
    update({ status: 'verifying' })
    try {
      const verified = await verifyImportedCredential(credentialId, verifyMode, importTestModel, refreshInfoAfterModelTest)
      update({ status: 'success', model: verified.model, response: verified.response })
      return 'success'
    } catch (error) {
      const rollback = await rollbackCredential(credentialId)
      update({
        status: 'failed',
        credentialId: rollback.success ? undefined : credentialId,
        error: `验活失败: ${extractErrorMessage(error)}${rollback.success ? '，已删除' : `；回滚${rollback.error}`}`,
      })
      return 'failed'
    }
  }

  /** 按并发数起若干个 worker，从同一个队列里取账号；ID 由服务端分配，前端不做假设。 */
  const runRows = async (indices: number[]) => {
    if (!indices.length) return
    const snapshot = rows
    const queue = [...indices]
    const outcomes: RowStatus[] = []
    const updateAt = (index: number) => (patch: Partial<ImportRow>) =>
      setRows((prev) => prev.map((row, i) => (i === index ? { ...row, ...patch } : row)))
    const worker = async () => {
      for (let index = queue.shift(); index !== undefined; index = queue.shift()) {
        if (cancelRef.current) break
        outcomes.push(await importRow(snapshot[index], updateAt(index)))
      }
    }
    cancelRef.current = false
    setStopping(false)
    setRunning(true)
    await Promise.all(Array.from({ length: Math.min(Number(concurrency), queue.length) }, worker))
    const stopped = cancelRef.current
    setRunning(false)
    setStopping(false)
    onDone()
    const count = (status: RowStatus) => outcomes.filter((outcome) => outcome === status).length
    toast.success(`导入${stopped ? '已停止' : '完成'}：成功 ${count('success')}，失败 ${count('failed')}，跳过 ${count('skipped')}`)
  }

  const indicesWith = (status: RowStatus) => rows.flatMap((row, index) => (row.status === status ? [index] : []))
  const countWith = (status: RowStatus) => rows.filter((row) => row.status === status).length
  const pendingCount = countWith('pending')
  const failedCount = countWith('failed')
  const finishedCount = countWith('success') + failedCount + countWith('skipped')
  const verifyLabel = skipVerify ? '跳过验活' : verifyMode === 'subscription_only' ? '查询订阅/积分' : '测试模型'

  const footer = !rows.length ? (
    <>
      <Button variant="outline" size="sm" onClick={onClose}>取消</Button>
      <Button size="sm" onClick={() => void handlePreview()} disabled={!parsedAccounts.length}>
        预览{parsedAccounts.length ? ` ${parsedAccounts.length} 个账号` : ''}
      </Button>
    </>
  ) : (
    <>
      {!running && <Button variant="ghost" size="sm" className="mr-auto" onClick={() => setRows([])}>返回修改</Button>}
      {running ? (
        <Button variant="outline" size="sm" disabled={stopping} onClick={() => { cancelRef.current = true; setStopping(true) }}>
          <Square className="h-3.5 w-3.5" />{stopping ? '正在停止…' : '停止'}
        </Button>
      ) : (
        <Button variant="outline" size="sm" onClick={onClose}>关闭</Button>
      )}
      {!running && failedCount > 0 && (
        <Button variant="outline" size="sm" onClick={() => void runRows(indicesWith('failed'))}>
          <AlertCircle className="h-3.5 w-3.5" />重试失败 ({failedCount})
        </Button>
      )}
      {!running && pendingCount > 0 && (
        <Button size="sm" onClick={() => void runRows(indicesWith('pending'))}>
          <Play className="h-3.5 w-3.5" />开始导入 ({pendingCount})
        </Button>
      )}
      {!running && pendingCount === 0 && failedCount === 0 && <Button size="sm" onClick={onClose}>完成</Button>}
    </>
  )

  const pinned = rows.length ? (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
        <span>共 <span className="font-semibold">{rows.length}</span> 条</span>
        <span className="text-muted-foreground">
          待导入 {pendingCount} · 成功 {countWith('success')} · 失败 {failedCount} · 跳过 {countWith('skipped')}
        </span>
        <span className="ml-auto text-xs text-muted-foreground">并发 {concurrency} · {verifyLabel}</span>
      </div>
      {running && <Progress value={Math.round((finishedCount / rows.length) * 100)} className="h-1.5" />}
    </div>
  ) : undefined

  return (
    <ModalShell
      open={open}
      title="导入账号"
      description="粘贴内容或选择文件，自动识别来源；预览确认后再开始导入。"
      width="max-w-6xl"
      onClose={() => { if (!running) onClose() }}
      pinned={pinned}
      footer={footer}
    >
      {rows.length ? (
        <ImportRowList rows={rows} />
      ) : (
        <div className="space-y-4">
          <div className="grid gap-4 lg:grid-cols-[minmax(0,5fr)_minmax(0,6fr)]">
            <div className="space-y-4">
              <FormSection
                title="账号数据"
                description="可以多选文件，每个文件单独识别来源；粘贴的内容也会一起导入。"
                actions={(
                  <label className="cursor-pointer">
                    <input type="file" className="sr-only" accept=".json,.jsonl,.txt,.csv" multiple onChange={(e) => void handleFile(e)} />
                    <Button type="button" variant="outline" size="sm" asChild><span><FileUp className="h-3.5 w-3.5" />选择文件</span></Button>
                  </label>
                )}
              >
                <Field label="来源" description="自动识别按内容结构判断；识别不对时手动指定。">
                  <Select value={source} onValueChange={(value) => setSource(value as ImportSource)}>
                    <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {IMPORT_SOURCE_OPTIONS.map((option) => (
                        <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
                <Textarea
                  className="mt-3 min-h-[180px] font-mono text-xs"
                  placeholder={DATA_PLACEHOLDER}
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                />
                {files.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1.5">
                    {files.map((file) => (
                      <span key={file.name} className="inline-flex items-center gap-1 rounded-md bg-muted py-0.5 pl-2 pr-0.5 text-xs">
                        {file.name}
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon-xs"
                          className="size-5"
                          aria-label={`移除 ${file.name}`}
                          onClick={() => setFiles((prev) => prev.filter((item) => item.name !== file.name))}
                        >
                          <X />
                        </Button>
                      </span>
                    ))}
                  </div>
                )}
              </FormSection>

              {parsedInputs.length > 0 && (
                <FormSection title="识别结果">
                  <div className="space-y-1.5 text-xs">
                    {parsedInputs.map((input) => (
                      <div key={input.name} className="flex flex-wrap items-baseline gap-x-2">
                        <span className="font-medium">{input.name}</span>
                        {input.error ? (
                          <span className="text-destructive">{input.error}</span>
                        ) : input.result && (
                          <span className="text-muted-foreground">
                            {importSourceLabel(input.result.source)}
                            {input.result.source !== input.result.detected && `（内容结构像${importSourceLabel(input.result.detected)}）`}
                            {` · ${input.result.accounts.length} 个账号`}
                            {input.result.skipped.length > 0 && ` · ${input.result.skipped.length} 条无法导入`}
                          </span>
                        )}
                      </div>
                    ))}
                  </div>
                  {parsedAccounts.length > 0 && (
                    <div className="mt-3 flex flex-wrap gap-1.5">
                      {stats.auth.map(([label, count]) => <Badge key={`auth-${label}`}>{label} {count}</Badge>)}
                      {stats.provider.map(([label, count]) => <Badge key={`provider-${label}`} tone="info">{label} {count}</Badge>)}
                    </div>
                  )}
                  {skippedEntries.length > 0 && (
                    <div className="mt-3 space-y-0.5 text-xs text-warning">
                      {skippedEntries.slice(0, 5).map((entry, index) => <div key={index} className="truncate">{entry}</div>)}
                      {skippedEntries.length > 5 && <div>另有 {skippedEntries.length - 5} 条</div>}
                    </div>
                  )}
                </FormSection>
              )}

              <FormSection title="来源字段" description="来源文件里的代理和停用状态默认不导入，按默认参数处理。">
                <div className="space-y-2.5 text-sm">
                  <label className="flex items-start gap-2">
                    <Checkbox className="mt-0.5" checked={importProxy} onCheckedChange={(value) => setImportProxy(value === true)} />
                    <span>
                      导入代理
                      <span className="ml-1 text-xs text-muted-foreground">（{stats.withProxy} 个账号带代理；代理资源 ID 只在导出它的实例里有效）</span>
                    </span>
                  </label>
                  <label className="flex items-start gap-2">
                    <Checkbox className="mt-0.5" checked={importDisabled} onCheckedChange={(value) => setImportDisabled(value === true)} />
                    <span>
                      导入禁用状态
                      <span className="ml-1 text-xs text-muted-foreground">（{stats.withDisabled} 个账号标记为停用）</span>
                    </span>
                  </label>
                  <label className="flex items-start gap-2">
                    <Checkbox className="mt-0.5" checked={skipFlagged} onCheckedChange={(value) => setSkipFlagged(value === true)} />
                    <span>
                      跳过来源标记为封禁 / 错误 / 回收站的账号
                      <span className="ml-1 text-xs text-muted-foreground">
                        （{stats.flaggedTotal} 个{stats.flagged.length > 0 && `：${stats.flagged.map(([label, count]) => `${label} ${count}`).join(' · ')}`}）
                      </span>
                    </span>
                  </label>
                </div>
              </FormSection>
            </div>

            <div className="space-y-4">
              <FormSection title="导入选项">
                <div className="grid gap-4 sm:grid-cols-2">
                  <Field
                    label="验活方式"
                    description={skipVerify ? '导入后不做任何检查' : verifyMode === 'subscription_only' ? '刷新 token 并查询订阅和积分，不消耗模型额度' : '发一次模型请求确认账号可用'}
                  >
                    <Select
                      value={skipVerify ? 'skip' : verifyMode}
                      onValueChange={(value) => {
                        if (value === 'skip') setSkipVerify(true)
                        else { setSkipVerify(false); setVerifyMode(value as ImportVerificationMode) }
                      }}
                    >
                      <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="subscription_only">查询订阅/积分（不请求模型）</SelectItem>
                        <SelectItem value="model_and_subscription">只测试模型</SelectItem>
                        <SelectItem value="skip">跳过验活</SelectItem>
                      </SelectContent>
                    </Select>
                  </Field>
                  <Field label="并发数" description={`同时导入的账号数，1 到 ${MAX_CONCURRENCY}；验活失败的账号会被删除`}>
                    <Input type="number" min={1} max={MAX_CONCURRENCY} value={concurrency} onChange={(e) => setConcurrency(e.target.value)} />
                  </Field>
                  <div className="flex flex-col gap-2.5 text-sm sm:col-span-2">
                    <label className="flex items-center gap-2">
                      <Checkbox checked={autoDiscoverSupportedModels} onCheckedChange={(value) => setAutoDiscoverSupportedModels(value === true)} />
                      自动发现模型限制
                    </label>
                    {!skipVerify && verifyMode === 'model_and_subscription' && (
                      <label className="flex items-center gap-2">
                        <Checkbox checked={refreshInfoAfterModelTest} onCheckedChange={(value) => setRefreshInfoAfterModelTest(value === true)} />
                        同时查询订阅/积分
                      </label>
                    )}
                  </div>
                </div>
              </FormSection>
              <CredentialParameterDefaultsPanel defaults={defaults} onChange={setDefaults} proxyResources={proxyOptions} tagOptions={tagOptions} />
            </div>
          </div>
          {formError && <Callout tone="error">{formError}</Callout>}
        </div>
      )}
    </ModalShell>
  )
}
