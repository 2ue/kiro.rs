import { AlertTriangle, CheckCircle2, FileSearch, FileUp, RefreshCw, Upload, XCircle } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button, Checkbox, Input, Loading, Textarea } from 'react-daisyui'
import { validateExistingCredentials, validateExternalCredentials } from '@/api/credentials'
import { Badge, EmptyState, SectionCard, Select, StatCard } from '@/components/common'
import { formatDate, formatNumber, formatQuota } from '@/lib/format'
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import type { AddCredentialRequest, CredentialValidationGroup, CredentialValidationItem, CredentialValidationResponse } from '@/types/api'

function changeTone(kind: string): 'neutral' | 'success' | 'warning' | 'error' | 'info' | 'secondary' {
  if (kind === 'downgraded' || kind === 'failed') return 'error'
  if (kind === 'upgraded') return 'success'
  if (kind === 'unknown') return 'warning'
  return 'secondary'
}

function itemTitle(item: CredentialValidationItem) {
  if (item.id) return `#${item.id} ${item.email || ''}`.trim()
  return `#${item.index || '-'} ${item.email || ''}`.trim()
}

function quotaText(item: CredentialValidationItem) {
  const info = item.current
  if (!info) return '-'
  return `${formatQuota(info.currentUsage)}/${formatQuota(info.usageLimit)}`
}

function actionTone(checked?: boolean, ok?: boolean | null): 'neutral' | 'success' | 'warning' | 'error' | 'info' | 'secondary' {
  if (!checked) return 'secondary'
  if (ok === true) return 'success'
  if (ok === false) return 'error'
  return 'warning'
}

function ActionBadge({ label, checked, ok }: { label: string; checked?: boolean; ok?: boolean | null }) {
  if (!checked) return null
  return (
    <Badge tone={actionTone(checked, ok)}>
      {ok === true && <CheckCircle2 className="h-3 w-3" />}
      {ok === false && <XCircle className="h-3 w-3" />}
      {label}
    </Badge>
  )
}

function itemErrors(item: CredentialValidationItem) {
  const errors = [item.usageError, item.livenessError, item.error]
    .map((value) => value?.trim())
    .filter((value): value is string => Boolean(value))
  return Array.from(new Set(errors))
}

function ResultGroup({ group }: { group: CredentialValidationGroup }) {
  return (
    <div className="rounded-box border border-base-300 bg-base-100">
      <div className="flex items-center justify-between border-b border-base-300 px-3 py-2">
        <div className="flex items-center gap-2">
          <Badge tone={changeTone(group.key)}>{group.title}</Badge>
          <span className="text-sm text-base-content/60">{group.count} 个</span>
        </div>
      </div>
      <div className="divide-y divide-base-200">
        {group.items.map((item) => (
          <div key={`${item.id || 'external'}-${item.index || item.email || item.subscriptionTitle}`} className="grid gap-3 px-3 py-2 text-sm xl:grid-cols-[1.25fr_1fr_1fr_1fr_1.25fr]">
            <div className="min-w-0">
              <div className="truncate font-semibold" title={itemTitle(item)}>
                {itemTitle(item)}
              </div>
              <div className="mt-0.5 flex flex-wrap gap-1">
                {item.disabled !== null && item.disabled !== undefined && <Badge tone={item.disabled ? 'error' : 'success'}>{item.disabled ? '已禁用' : '启用'}</Badge>}
                {item.matchedExistingCredentialId && <Badge tone="info">匹配系统 #{item.matchedExistingCredentialId}</Badge>}
                {item.existingDisabled && <Badge tone="warning">系统内已禁用</Badge>}
              </div>
            </div>
            <div>
              <div className="text-xs text-base-content/50">动作</div>
              <div className="mt-1 flex flex-wrap gap-1">
                <ActionBadge label="订阅" checked={item.subscriptionChecked} ok={item.subscriptionOk} />
                <ActionBadge label="用量" checked={item.usageChecked} ok={item.usageOk} />
                <ActionBadge label="验活" checked={item.livenessChecked} ok={item.livenessOk} />
                {!item.subscriptionChecked && !item.usageChecked && !item.livenessChecked && <span className="text-base-content/50">-</span>}
              </div>
            </div>
            <div>
              <div className="text-xs text-base-content/50">订阅</div>
              <div className="font-medium">{item.subscriptionChecked ? item.current?.subscriptionTitle || item.subscriptionTitle || '-' : '-'}</div>
            </div>
            <div>
              <div className="text-xs text-base-content/50">用量</div>
              <div className="font-mono font-medium">{item.usageChecked ? quotaText(item) : '-'}</div>
            </div>
            <div>
              <div className="text-xs text-base-content/50">验活 / 状态</div>
              <div className="break-words">
                {item.livenessChecked && (
                  <div className="mb-1 text-xs text-base-content/60">
                    {item.livenessModel ? testModelLabel(item.livenessModel) : '默认模型'}
                    {item.livenessResponse && <span className="ml-1 line-clamp-1">: {item.livenessResponse}</span>}
                  </div>
                )}
                {itemErrors(item).length ? (
                  <div className="space-y-1">
                    {itemErrors(item).map((error) => <div key={error} className="text-error">{error}</div>)}
                  </div>
                ) : item.current ? formatDate(item.current.checkedAt) : item.livenessChecked && item.livenessOk ? '验活成功' : '-'}
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

function ValidationResults({ result }: { result: CredentialValidationResponse | null }) {
  if (!result) return <EmptyState text="暂无校验结果" />
  return (
    <div className="space-y-3">
      <div className="metric-grid">
        <StatCard title="总数" value={formatNumber(result.total)} />
        <StatCard title="成功" value={formatNumber(result.success)} tone="success" />
        <StatCard title="失败" value={formatNumber(result.failed)} tone={result.failed ? 'warning' : 'default'} />
        <StatCard title="疑似掉级" value={formatNumber(result.downgraded)} tone={result.downgraded ? 'warning' : 'default'} />
      </div>
      {result.groups.length === 0 ? <EmptyState text="暂无分组结果" /> : result.groups.map((group) => <ResultGroup key={group.key} group={group} />)}
    </div>
  )
}

interface ExternalValidationOptions {
  querySubscription: boolean
  queryUsage: boolean
  checkLiveness: boolean
  livenessModel: string
  livenessPrompt: string
}

function initialExternalOptions(): ExternalValidationOptions {
  return {
    querySubscription: true,
    queryUsage: true,
    checkLiveness: false,
    livenessModel: DEFAULT_TEST_MODEL,
    livenessPrompt: DEFAULT_TEST_PROMPT,
  }
}

export function AccountValidationPanel() {
  const [raw, setRaw] = useState('')
  const [result, setResult] = useState<CredentialValidationResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [externalOptions, setExternalOptions] = useState<ExternalValidationOptions>(initialExternalOptions)
  const [selectedFiles, setSelectedFiles] = useState<string[]>([])
  const parsedCount = useMemo(() => {
    try {
      return raw.trim() ? parseCredentialImportText(raw).length : 0
    } catch {
      return 0
    }
  }, [raw])
  const hasExternalAction = externalOptions.querySubscription || externalOptions.queryUsage || externalOptions.checkLiveness

  const validateExisting = async (scope: 'all' | 'enabled' | 'disabled') => {
    setLoading(true)
    try {
      const data = await validateExistingCredentials({ scope, force: true })
      setResult(data)
      toast.success(`校验完成：成功 ${data.success}/${data.total}`)
    } catch (error) {
      toast.error(`校验失败: ${extractErrorMessage(error)}`)
    } finally {
      setLoading(false)
    }
  }

  const appendExternalCredentials = (credentials: AddCredentialRequest[]) => {
    let existing: AddCredentialRequest[] = []
    if (raw.trim()) {
      try {
        existing = parseCredentialImportText(raw)
      } catch {
        existing = []
      }
    }
    setRaw(JSON.stringify([...existing, ...credentials], null, 2))
  }

  const handleExternalFiles = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const parsed = await parseCredentialImportFiles(files)
    if (parsed.credentials.length) {
      appendExternalCredentials(parsed.credentials)
      setSelectedFiles(files.map((file) => file.name))
      toast.success(`已从 ${files.length} 个文件读取 ${parsed.credentials.length} 条账号`)
    }
    if (parsed.errors.length) toast.warning(`部分文件未读取: ${parsed.errors.slice(0, 3).join('；')}`)
    if (!parsed.credentials.length && !parsed.errors.length) toast.error('没有读取到有效账号')
  }

  const updateExternalOption = <K extends keyof ExternalValidationOptions>(key: K, value: ExternalValidationOptions[K]) => {
    setExternalOptions((prev) => ({ ...prev, [key]: value }))
  }

  const validateExternal = async () => {
    let credentials: AddCredentialRequest[]
    try {
      credentials = parseCredentialImportText(raw)
    } catch (error) {
      toast.error(`JSON 解析失败: ${extractErrorMessage(error)}`)
      return
    }
    if (credentials.length === 0) {
      toast.error('没有解析到可校验的账号')
      return
    }
    if (!hasExternalAction) {
      toast.error('请至少选择订阅、用量或验活中的一项')
      return
    }
    setLoading(true)
    try {
      const data = await validateExternalCredentials({
        credentials,
        querySubscription: externalOptions.querySubscription,
        queryUsage: externalOptions.queryUsage,
        checkLiveness: externalOptions.checkLiveness,
        livenessModel: externalOptions.livenessModel,
        livenessPrompt: externalOptions.livenessPrompt,
      })
      setResult(data)
      toast.success(`校验完成：成功 ${data.success}/${data.total}`)
    } catch (error) {
      toast.error(`校验失败: ${extractErrorMessage(error)}`)
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="space-y-4">
      <SectionCard
        title="系统账号复查"
        description="强制查询系统内账号的订阅、额度和用量，并和上次快照比较。禁用账号也可以手动复查，不会参与调度。"
        actions={
          <>
            <Button type="button" variant="outline" size="sm" onClick={() => validateExisting('all')} disabled={loading}>
              {loading ? <Loading size="xs" /> : <RefreshCw className="h-4 w-4" />}
              全部复查
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => validateExisting('enabled')} disabled={loading}>
              仅启用
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => validateExisting('disabled')} disabled={loading}>
              仅禁用
            </Button>
          </>
        }
      >
        <div className="flex items-start gap-2 rounded-box border border-base-300 bg-base-100 p-3 text-sm text-base-content/70">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
          <div>掉级判断基于上次保存的信息和本次强制查询结果；首次查询没有历史快照时会标记为未知。</div>
        </div>
      </SectionCard>

      <SectionCard
        title="外部 JSON 校验"
        description="粘贴或选择 Kiro Account Manager / 批量导入格式 JSON，不导入系统、不改变调度。"
        actions={
          <>
            <Button tag="label" variant="outline" size="sm" disabled={loading}>
              <FileUp className="h-4 w-4" />
              选择文件
              <input type="file" accept=".json,.jsonl,.txt,application/json" multiple className="hidden" onChange={handleExternalFiles} disabled={loading} />
            </Button>
            <Button type="button" color="primary" size="sm" onClick={validateExternal} disabled={loading || parsedCount === 0 || !hasExternalAction}>
              {loading ? <Loading size="xs" /> : <FileSearch className="h-4 w-4" />}
              校验 JSON
            </Button>
          </>
        }
      >
        <div className="space-y-3">
          <div className="grid gap-3 rounded-box border border-base-300 bg-base-100 p-3 lg:grid-cols-[1fr_1.4fr]">
            <div>
              <div className="mb-2 text-xs font-semibold text-base-content/70">校验项目</div>
              <div className="flex flex-wrap gap-3 text-sm">
                <label className="inline-flex items-center gap-2">
                  <Checkbox size="sm" checked={externalOptions.querySubscription} disabled={loading} onChange={(event) => updateExternalOption('querySubscription', event.target.checked)} />
                  查询订阅
                </label>
                <label className="inline-flex items-center gap-2">
                  <Checkbox size="sm" checked={externalOptions.queryUsage} disabled={loading} onChange={(event) => updateExternalOption('queryUsage', event.target.checked)} />
                  查询用量
                </label>
                <label className="inline-flex items-center gap-2">
                  <Checkbox size="sm" checked={externalOptions.checkLiveness} disabled={loading} onChange={(event) => updateExternalOption('checkLiveness', event.target.checked)} />
                  模型验活
                </label>
              </div>
              {!hasExternalAction && <div className="mt-2 text-xs text-error">至少选择一个校验项目</div>}
            </div>
            <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
              <label className="form-control min-w-0">
                <span className="label-text mb-1 text-xs font-semibold text-base-content/70">验活模型</span>
                <Select bordered size="sm" value={externalOptions.livenessModel} disabled={loading || !externalOptions.checkLiveness} onChange={(event) => updateExternalOption('livenessModel', event.target.value)}>
                  {TEST_MODELS.map((model) => (
                    <Select.Option key={model.id} value={model.id}>{model.label}</Select.Option>
                  ))}
                </Select>
              </label>
              <label className="form-control min-w-0">
                <span className="label-text mb-1 text-xs font-semibold text-base-content/70">验活提示词</span>
                <Input bordered size="sm" value={externalOptions.livenessPrompt} disabled={loading || !externalOptions.checkLiveness} onChange={(event) => updateExternalOption('livenessPrompt', event.target.value)} />
              </label>
            </div>
          </div>
          <Textarea bordered className="min-h-52 w-full font-mono text-xs" value={raw} onChange={(event) => setRaw(event.target.value)} placeholder="粘贴 KAM JSON、credentials 数组或 JSONL" />
          <div className="flex items-center justify-between text-xs text-base-content/60">
            <span>已解析 {parsedCount} 条可校验账号</span>
            <span className="inline-flex items-center gap-1">
              <Upload className="h-3.5 w-3.5" />
              {selectedFiles.length ? `已选择 ${selectedFiles.length} 个文件` : '文件内容可直接粘贴到这里'}
            </span>
          </div>
        </div>
      </SectionCard>

      <SectionCard title="校验结果">
        <ValidationResults result={result} />
      </SectionCard>
    </div>
  )
}
