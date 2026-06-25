import * as React from 'react'
import { AlertTriangle, CheckCircle2, FileSearch, FileUp, RefreshCw, Upload, XCircle } from 'lucide-react'
import { toast } from 'sonner'
import { validateExistingCredentials, validateExternalCredentials } from '@/api/credentials'
import { formatDate, formatNumber, formatQuota } from '@/lib/format'
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import type {
  AddCredentialRequest,
  CredentialValidationGroup,
  CredentialValidationItem,
  CredentialValidationResponse,
} from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  EmptyState,
  Callout,
  Field,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Checkbox,
  Input,
  Textarea,
  Spinner,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  type BadgeProps,
} from '@/components/ui'

type Tone = NonNullable<BadgeProps['tone']>

function changeTone(kind: string): Tone {
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

function actionTone(checked?: boolean, ok?: boolean | null): Tone {
  if (!checked) return 'secondary'
  if (ok === true) return 'success'
  if (ok === false) return 'error'
  return 'warning'
}

function ActionBadge({ label, checked, ok }: { label: string; checked?: boolean; ok?: boolean | null }) {
  if (!checked) return null
  return (
    <Badge tone={actionTone(checked, ok)}>
      {ok === true && <CheckCircle2 className="size-3" />}
      {ok === false && <XCircle className="size-3" />}
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
    <div className="overflow-hidden rounded-xl border border-border bg-card">
      <div className="flex items-center gap-2 border-b border-border bg-muted/40 px-3 py-2">
        <Badge tone={changeTone(group.key)}>{group.title}</Badge>
        <span className="text-sm text-muted-foreground">{group.count} 个</span>
      </div>
      <div className="divide-y divide-border">
        {group.items.map((item) => (
          <div
            key={`${item.id || 'external'}-${item.index || item.email || item.subscriptionTitle}`}
            className="grid gap-3 px-3 py-2.5 text-sm xl:grid-cols-[1.25fr_1fr_1fr_1fr_1.25fr]"
          >
            <div className="min-w-0">
              <div className="truncate font-semibold" title={itemTitle(item)}>
                {itemTitle(item)}
              </div>
              <div className="mt-1 flex flex-wrap gap-1">
                {item.disabled !== null && item.disabled !== undefined && (
                  <Badge tone={item.disabled ? 'error' : 'success'}>
                    {item.disabled ? '已禁用' : '启用'}
                  </Badge>
                )}
                {item.matchedExistingCredentialId && (
                  <Badge tone="info">匹配系统 #{item.matchedExistingCredentialId}</Badge>
                )}
                {item.existingDisabled && <Badge tone="warning">系统内已禁用</Badge>}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">动作</div>
              <div className="mt-1 flex flex-wrap gap-1">
                <ActionBadge label="订阅" checked={item.subscriptionChecked} ok={item.subscriptionOk} />
                <ActionBadge label="用量" checked={item.usageChecked} ok={item.usageOk} />
                <ActionBadge label="验活" checked={item.livenessChecked} ok={item.livenessOk} />
                {!item.subscriptionChecked && !item.usageChecked && !item.livenessChecked && (
                  <span className="text-muted-foreground">-</span>
                )}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">订阅</div>
              <div className="font-medium">
                {item.subscriptionChecked
                  ? item.current?.subscriptionTitle || item.subscriptionTitle || '-'
                  : '-'}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">用量</div>
              <div className="font-mono font-medium tabular-nums">
                {item.usageChecked ? quotaText(item) : '-'}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">验活 / 状态</div>
              <div className="break-words">
                {item.livenessChecked && (
                  <div className="mb-1 text-xs text-muted-foreground">
                    {item.livenessModel ? testModelLabel(item.livenessModel) : '默认模型'}
                    {item.livenessResponse && (
                      <span className="ml-1 line-clamp-1">: {item.livenessResponse}</span>
                    )}
                  </div>
                )}
                {itemErrors(item).length ? (
                  <div className="space-y-1">
                    {itemErrors(item).map((error) => (
                      <div key={error} className="text-destructive">
                        {error}
                      </div>
                    ))}
                  </div>
                ) : item.current ? (
                  formatDate(item.current.checkedAt)
                ) : item.livenessChecked && item.livenessOk ? (
                  '验活成功'
                ) : (
                  '-'
                )}
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

function ValidationResults({ result }: { result: CredentialValidationResponse | null }) {
  if (!result) return <EmptyState title="暂无校验结果" />
  return (
    <div className="space-y-4">
      <StatGrid>
        <StatCard title="总数" value={formatNumber(result.total)} />
        <StatCard title="成功" value={formatNumber(result.success)} tone="success" />
        <StatCard title="失败" value={formatNumber(result.failed)} tone={result.failed ? 'warning' : 'default'} />
        <StatCard
          title="疑似掉级"
          value={formatNumber(result.downgraded)}
          tone={result.downgraded ? 'warning' : 'default'}
        />
      </StatGrid>
      {result.groups.length === 0 ? (
        <EmptyState title="暂无分组结果" />
      ) : (
        result.groups.map((group) => <ResultGroup key={group.key} group={group} />)
      )}
    </div>
  )
}

interface ExternalOptions {
  querySubscription: boolean
  queryUsage: boolean
  checkLiveness: boolean
  livenessModel: string
  livenessPrompt: string
}

const initialExternalOptions = (): ExternalOptions => ({
  querySubscription: true,
  queryUsage: true,
  checkLiveness: false,
  livenessModel: DEFAULT_TEST_MODEL,
  livenessPrompt: DEFAULT_TEST_PROMPT,
})

export function ValidationPage() {
  const [raw, setRaw] = React.useState('')
  const [result, setResult] = React.useState<CredentialValidationResponse | null>(null)
  const [loading, setLoading] = React.useState(false)
  const [options, setOptions] = React.useState<ExternalOptions>(initialExternalOptions)
  const [selectedFiles, setSelectedFiles] = React.useState<string[]>([])

  const parsedCount = React.useMemo(() => {
    try {
      return raw.trim() ? parseCredentialImportText(raw).length : 0
    } catch {
      return 0
    }
  }, [raw])
  const hasAction = options.querySubscription || options.queryUsage || options.checkLiveness

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

  const appendCredentials = (credentials: AddCredentialRequest[]) => {
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

  const handleFiles = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const parsed = await parseCredentialImportFiles(files)
    if (parsed.credentials.length) {
      appendCredentials(parsed.credentials)
      setSelectedFiles(files.map((file) => file.name))
      toast.success(`已从 ${files.length} 个文件读取 ${parsed.credentials.length} 条账号`)
    }
    if (parsed.errors.length) toast.warning(`部分文件未读取: ${parsed.errors.slice(0, 3).join('；')}`)
    if (!parsed.credentials.length && !parsed.errors.length) toast.error('没有读取到有效账号')
  }

  const update = <K extends keyof ExternalOptions>(key: K, value: ExternalOptions[K]) => {
    setOptions((prev) => ({ ...prev, [key]: value }))
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
    if (!hasAction) {
      toast.error('请至少选择订阅、用量或验活中的一项')
      return
    }
    setLoading(true)
    try {
      const data = await validateExternalCredentials({
        credentials,
        querySubscription: options.querySubscription,
        queryUsage: options.queryUsage,
        checkLiveness: options.checkLiveness,
        livenessModel: options.livenessModel,
        livenessPrompt: options.livenessPrompt,
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
    <PageContainer>
      <PageHeader title={pageMeta.validation.title} subtitle={pageMeta.validation.subtitle} />

      <SectionCard
        title="系统账号复查"
        description="强制查询系统内账号的订阅、额度和用量，并和上次快照比较。禁用账号也可手动复查，不参与调度。"
        actions={
          <>
            <Button variant="outline" size="sm" onClick={() => validateExisting('all')} disabled={loading}>
              {loading ? <Spinner size="sm" /> : <RefreshCw className="size-4" />}
              全部复查
            </Button>
            <Button variant="outline" size="sm" onClick={() => validateExisting('enabled')} disabled={loading}>
              仅启用
            </Button>
            <Button variant="outline" size="sm" onClick={() => validateExisting('disabled')} disabled={loading}>
              仅禁用
            </Button>
          </>
        }
      >
        <Callout tone="warning">
          <span className="flex items-start gap-2">
            <AlertTriangle className="mt-0.5 size-4 shrink-0" />
            掉级判断基于上次保存的信息和本次强制查询结果；首次查询没有历史快照时会标记为未知。
          </span>
        </Callout>
      </SectionCard>

      <SectionCard
        title="外部 JSON 校验"
        description="粘贴或选择 Kiro Account Manager / 批量导入格式 JSON，不导入系统、不改变调度。"
        actions={
          <>
            <Button variant="outline" size="sm" disabled={loading} asChild>
              <label className="cursor-pointer">
                <FileUp className="size-4" />
                选择文件
                <input
                  type="file"
                  accept=".json,.jsonl,.txt,application/json"
                  multiple
                  className="hidden"
                  onChange={handleFiles}
                  disabled={loading}
                />
              </label>
            </Button>
            <Button size="sm" onClick={validateExternal} disabled={loading || parsedCount === 0 || !hasAction}>
              {loading ? <Spinner size="sm" /> : <FileSearch className="size-4" />}
              校验 JSON
            </Button>
          </>
        }
      >
        <div className="space-y-3">
          <div className="grid gap-4 rounded-xl border border-border bg-muted/30 p-4 lg:grid-cols-[1fr_1.4fr]">
            <div>
              <div className="mb-2 text-xs font-semibold text-foreground/80">校验项目</div>
              <div className="flex flex-wrap gap-4 text-sm">
                <label className="inline-flex items-center gap-2">
                  <Checkbox
                    checked={options.querySubscription}
                    disabled={loading}
                    onCheckedChange={(v) => update('querySubscription', v === true)}
                  />
                  查询订阅
                </label>
                <label className="inline-flex items-center gap-2">
                  <Checkbox
                    checked={options.queryUsage}
                    disabled={loading}
                    onCheckedChange={(v) => update('queryUsage', v === true)}
                  />
                  查询用量
                </label>
                <label className="inline-flex items-center gap-2">
                  <Checkbox
                    checked={options.checkLiveness}
                    disabled={loading}
                    onCheckedChange={(v) => update('checkLiveness', v === true)}
                  />
                  模型验活
                </label>
              </div>
              {!hasAction && <div className="mt-2 text-xs text-destructive">至少选择一个校验项目</div>}
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="验活模型">
                <Select
                  value={options.livenessModel}
                  disabled={loading || !options.checkLiveness}
                  onValueChange={(v) => update('livenessModel', v)}
                >
                  <SelectTrigger size="sm">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {TEST_MODELS.map((model) => (
                      <SelectItem key={model.id} value={model.id}>
                        {model.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Field>
              <Field label="验活提示词">
                <Input
                  value={options.livenessPrompt}
                  disabled={loading || !options.checkLiveness}
                  onChange={(e) => update('livenessPrompt', e.target.value)}
                />
              </Field>
            </div>
          </div>
          <Textarea
            className="min-h-52 w-full font-mono text-xs"
            value={raw}
            onChange={(e) => setRaw(e.target.value)}
            placeholder="粘贴 KAM JSON、credentials 数组或 JSONL"
          />
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <span>已解析 {parsedCount} 条可校验账号</span>
            <span className="inline-flex items-center gap-1">
              <Upload className="size-3.5" />
              {selectedFiles.length ? `已选择 ${selectedFiles.length} 个文件` : '文件内容可直接粘贴到这里'}
            </span>
          </div>
        </div>
      </SectionCard>

      <SectionCard title="校验结果">
        <ValidationResults result={result} />
      </SectionCard>
    </PageContainer>
  )
}
