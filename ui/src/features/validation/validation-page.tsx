import {
  AlertTriangle,
  CheckCircle2,
  FileSearch,
  FileUp,
  RefreshCw,
  Upload,
  XCircle,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { validateExistingCredentials, validateExternalCredentials } from '@/api/credentials'
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
  Spinner,
  Textarea,
} from '@/components/ui'
import type { BadgeProps } from '@/components/ui'
import {
  Callout,
  EmptyState,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
} from '@/components/patterns'
import { formatCompact, formatDate, formatNumber, formatQuota } from '@/lib/format'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import type { AddCredentialRequest, CredentialValidationGroup, CredentialValidationItem, CredentialValidationResponse } from '@/types/api'
import { pageMeta } from '@/types/ui'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function changeTone(kind: string): 'default' | 'success' | 'warning' | 'error' {
  if (kind === 'downgraded' || kind === 'failed') return 'error'
  if (kind === 'upgraded') return 'success'
  if (kind === 'unknown') return 'warning'
  return 'default'
}

function itemTitle(item: CredentialValidationItem): string {
  if (item.id) return `#${item.id}${item.email ? ` ${item.email}` : ''}`
  return `#${item.index ?? '-'}${item.email ? ` ${item.email}` : ''}`
}

function quotaText(item: CredentialValidationItem): string {
  const info = item.current
  if (!info) return '-'
  return `${formatQuota(info.currentUsage)} / ${formatQuota(info.usageLimit)}`
}

function actionTone(checked?: boolean, ok?: boolean | null): 'default' | 'success' | 'error' {
  if (!checked) return 'default'
  if (ok === true) return 'success'
  if (ok === false) return 'error'
  return 'default'
}

function itemErrors(item: CredentialValidationItem): string[] {
  return Array.from(
    new Set(
      [item.usageError, item.livenessError, item.error]
        .map((v) => v?.trim())
        .filter((v): v is string => Boolean(v))
    )
  )
}

// ---------------------------------------------------------------------------
// ActionBadge
// ---------------------------------------------------------------------------

function ActionBadge({ label, checked, ok }: { label: string; checked?: boolean; ok?: boolean | null }) {
  if (!checked) return null
  const tone = actionTone(checked, ok)
  return (
    <Badge tone={tone === 'success' ? 'success' : tone === 'error' ? 'error' : 'neutral'} className="gap-1 text-xs">
      {ok === true && <CheckCircle2 className="h-3 w-3" />}
      {ok === false && <XCircle className="h-3 w-3" />}
      {label}
    </Badge>
  )
}

// ---------------------------------------------------------------------------
// ResultGroup — single validation group
// ---------------------------------------------------------------------------

function ResultGroup({ group }: { group: CredentialValidationGroup }) {
  const tone = changeTone(group.key)
  const badgeTone: BadgeProps['tone'] =
    tone === 'success' ? 'success' :
    tone === 'error' ? 'error' :
    tone === 'warning' ? 'warning' :
    'neutral'

  return (
    <div className="overflow-hidden rounded-xl bg-card shadow-sm">
      <div className="flex items-center justify-between bg-muted/30 px-4 py-2.5">
        <div className="flex items-center gap-2">
          <Badge tone={badgeTone}>{group.title}</Badge>
          <span className="text-sm text-muted-foreground">{group.count} 个</span>
        </div>
      </div>
      <div>
        {group.items.map((item) => (
          <ValidationItemRow
            key={`${item.id ?? 'ext'}-${item.index ?? item.email ?? item.subscriptionTitle}`}
            item={item}
          />
        ))}
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// ValidationItemRow
// ---------------------------------------------------------------------------

function ValidationItemRow({ item }: { item: CredentialValidationItem }) {
  const errors = itemErrors(item)
  return (
    <div className="grid gap-3 px-4 py-3 text-sm xl:grid-cols-[1.25fr_1fr_1fr_1fr_1.25fr]">
      {/* Identity */}
      <div className="min-w-0">
        <div className="truncate font-semibold" title={itemTitle(item)}>{itemTitle(item)}</div>
        <div className="mt-1 flex flex-wrap gap-1">
          {item.disabled != null && (
            <Badge tone={item.disabled ? 'error' : 'success'} className="text-xs">
              {item.disabled ? '已禁用' : '启用'}
            </Badge>
          )}
          {item.matchedExistingCredentialId != null && (
            <Badge tone="info" className="text-xs">匹配系统 #{item.matchedExistingCredentialId}</Badge>
          )}
          {item.existingDisabled && (
            <Badge tone="warning" className="text-xs">系统内已禁用</Badge>
          )}
        </div>
      </div>

      {/* Actions */}
      <div>
        <div className="mb-1 text-xs font-medium text-muted-foreground">动作</div>
        <div className="flex flex-wrap gap-1">
          <ActionBadge label="订阅" checked={item.subscriptionChecked} ok={item.subscriptionOk} />
          <ActionBadge label="用量" checked={item.usageChecked} ok={item.usageOk} />
          <ActionBadge label="验活" checked={item.livenessChecked} ok={item.livenessOk} />
          {!item.subscriptionChecked && !item.usageChecked && !item.livenessChecked && (
            <span className="text-xs text-muted-foreground">-</span>
          )}
        </div>
      </div>

      {/* Subscription */}
      <div>
        <div className="mb-1 text-xs font-medium text-muted-foreground">订阅</div>
        <div className="font-medium">
          {item.subscriptionChecked ? (item.current?.subscriptionTitle ?? item.subscriptionTitle ?? '-') : '-'}
        </div>
      </div>

      {/* Usage */}
      <div>
        <div className="mb-1 text-xs font-medium text-muted-foreground">用量</div>
        <div className="font-mono font-medium">{item.usageChecked ? quotaText(item) : '-'}</div>
      </div>

      {/* Liveness / Status */}
      <div>
        <div className="mb-1 text-xs font-medium text-muted-foreground">验活 / 状态</div>
        <div className="break-words">
          {item.livenessChecked && (
            <div className="mb-1 text-xs text-muted-foreground">
              {item.livenessModel ? testModelLabel(item.livenessModel) : '默认模型'}
              {item.livenessResponse && (
                <span className="ml-1 line-clamp-1">: {item.livenessResponse}</span>
              )}
            </div>
          )}
          {errors.length > 0 ? (
            <div className="space-y-0.5">
              {errors.map((err) => (
                <div key={err} className="text-xs text-destructive">{err}</div>
              ))}
            </div>
          ) : item.current ? (
            <span className="text-xs text-muted-foreground">{formatDate(item.current.checkedAt)}</span>
          ) : item.livenessChecked && item.livenessOk ? (
            <span className="text-xs text-success">验活成功</span>
          ) : (
            <span className="text-xs text-muted-foreground">-</span>
          )}
        </div>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// ValidationResults — shared result renderer
// ---------------------------------------------------------------------------

function ValidationResults({ result }: { result: CredentialValidationResponse | null }) {
  if (!result) {
    return (
      <EmptyState
        title="暂无校验结果"
        description="执行校验后结果将显示在这里"
      />
    )
  }

  return (
    <div className="space-y-4">
      <StatGrid>
        <StatCard title="总数" value={formatCompact(result.total)} valueTitle={formatNumber(result.total)} />
        <StatCard title="成功" value={formatCompact(result.success)} valueTitle={formatNumber(result.success)} tone="success" />
        <StatCard
          title="失败"
          value={formatCompact(result.failed)}
          valueTitle={formatNumber(result.failed)}
          tone={result.failed > 0 ? 'error' : 'default'}
        />
        <StatCard
          title="升级"
          value={formatCompact(result.upgraded)}
          valueTitle={formatNumber(result.upgraded)}
          tone={result.upgraded > 0 ? 'success' : 'default'}
        />
        <StatCard
          title="疑似掉级"
          value={formatCompact(result.downgraded)}
          valueTitle={formatNumber(result.downgraded)}
          tone={result.downgraded > 0 ? 'warning' : 'default'}
        />
        <StatCard title="无变化" value={formatCompact(result.unchanged)} valueTitle={formatNumber(result.unchanged)} />
      </StatGrid>

      {result.groups.length === 0 ? (
        <EmptyState title="暂无分组结果" />
      ) : (
        <div className="space-y-3">
          {result.groups.map((group) => (
            <ResultGroup key={group.key} group={group} />
          ))}
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// ExistingValidationSection
// ---------------------------------------------------------------------------

type ExistingScope = 'all' | 'enabled' | 'disabled'

function ExistingValidationSection({
  onResult,
}: {
  onResult: (r: CredentialValidationResponse) => void
}) {
  const [scope, setScope] = useState<ExistingScope>('all')

  const mutation = useMutation({
    mutationFn: (s: ExistingScope) => validateExistingCredentials({ scope: s, force: true }),
    onSuccess: (data) => {
      onResult(data)
      toast.success(`校验完成：成功 ${data.success}/${data.total}`)
    },
    onError: (error) => {
      toast.error(`校验失败: ${extractErrorMessage(error)}`)
    },
  })

  const handleRun = () => {
    mutation.mutate(scope)
  }

  return (
    <SectionCard
      title="系统账号复查"
      description="对已入库账号查询订阅与用量快照，强制刷新后与上次快照对比差异。"
      actions={
        <Button size="sm" onClick={handleRun} disabled={mutation.isPending}>
          {mutation.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-4 w-4" />}
          执行复查
        </Button>
      }
    >
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-3">
          <span className="text-sm font-medium text-muted-foreground">范围</span>
          <div className="flex rounded-lg bg-muted/60 p-1">
            {(['all', 'enabled', 'disabled'] as ExistingScope[]).map((s) => (
              <button
                key={s}
                type="button"
                onClick={() => setScope(s)}
                disabled={mutation.isPending}
                className={[
                  'rounded-md px-3 py-1 text-sm transition-colors',
                  scope === s
                    ? 'bg-card font-semibold text-primary shadow-sm'
                    : 'text-muted-foreground hover:text-foreground',
                  mutation.isPending ? 'cursor-not-allowed opacity-50' : '',
                ].join(' ')}
              >
                {s === 'all' ? '全部' : s === 'enabled' ? '仅启用' : '仅禁用'}
              </button>
            ))}
          </div>
        </div>
        <Callout tone="warning">
          <div className="flex items-start gap-2">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            <span>掉级判断基于上次保存的信息和本次强制查询结果；首次查询没有历史快照时会标记为未知。existing 接口仅支持订阅与用量对比，不支持模型验活。</span>
          </div>
        </Callout>
      </div>
    </SectionCard>
  )
}

// ---------------------------------------------------------------------------
// ExternalOptions state
// ---------------------------------------------------------------------------

interface ExternalOptions {
  querySubscription: boolean
  queryUsage: boolean
  checkLiveness: boolean
  livenessModel: string
  livenessPrompt: string
}

function initialExternalOptions(): ExternalOptions {
  return {
    querySubscription: true,
    queryUsage: true,
    checkLiveness: false,
    livenessModel: DEFAULT_TEST_MODEL,
    livenessPrompt: DEFAULT_TEST_PROMPT,
  }
}

// ---------------------------------------------------------------------------
// ExternalValidationSection
// ---------------------------------------------------------------------------

function ExternalValidationSection({
  onResult,
}: {
  onResult: (r: CredentialValidationResponse) => void
}) {
  const [raw, setRaw] = useState('')
  const [opts, setOpts] = useState<ExternalOptions>(initialExternalOptions)
  const [selectedFiles, setSelectedFiles] = useState<string[]>([])

  const parsedCount = useMemo(() => {
    if (!raw.trim()) return 0
    try { return parseCredentialImportText(raw).length } catch { return 0 }
  }, [raw])

  const hasAction = opts.querySubscription || opts.queryUsage || opts.checkLiveness

  const setOpt = <K extends keyof ExternalOptions>(key: K, value: ExternalOptions[K]) =>
    setOpts((prev) => ({ ...prev, [key]: value }))

  const appendCredentials = (credentials: AddCredentialRequest[]) => {
    let existing: AddCredentialRequest[] = []
    if (raw.trim()) { try { existing = parseCredentialImportText(raw) } catch { existing = [] } }
    setRaw(JSON.stringify([...existing, ...credentials], null, 2))
  }

  const handleFiles = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (!files.length) return
    const parsed = await parseCredentialImportFiles(files)
    if (parsed.credentials.length) {
      appendCredentials(parsed.credentials)
      setSelectedFiles(files.map((f) => f.name))
      toast.success(`已从 ${files.length} 个文件读取 ${parsed.credentials.length} 条账号`)
    }
    if (parsed.errors.length) toast.warning(`部分文件未读取: ${parsed.errors.slice(0, 3).join('；')}`)
    if (!parsed.credentials.length && !parsed.errors.length) toast.error('没有读取到有效账号')
  }

  const mutation = useMutation({
    mutationFn: (req: Parameters<typeof validateExternalCredentials>[0]) =>
      validateExternalCredentials(req),
    onSuccess: (data) => {
      onResult(data)
      toast.success(`校验完成：成功 ${data.success}/${data.total}`)
    },
    onError: (error) => {
      toast.error(`校验失败: ${extractErrorMessage(error)}`)
    },
  })

  const handleRun = () => {
    let credentials: AddCredentialRequest[]
    try {
      credentials = parseCredentialImportText(raw)
    } catch (error) {
      toast.error(`JSON 解析失败: ${extractErrorMessage(error)}`)
      return
    }
    if (!credentials.length) { toast.error('没有解析到可校验的账号'); return }
    if (!hasAction) { toast.error('请至少选择订阅、用量或验活中的一项'); return }
    mutation.mutate({
      credentials,
      querySubscription: opts.querySubscription,
      queryUsage: opts.queryUsage,
      checkLiveness: opts.checkLiveness,
      livenessModel: opts.livenessModel,
      livenessPrompt: opts.livenessPrompt,
    })
  }

  return (
    <SectionCard
      title="外部 JSON 校验"
      description="校验未入库的外部账号，支持订阅/用量/模型验活，不入库、不改变调度。"
      actions={
        <div className="flex items-center gap-1.5">
          <Button variant="outline" size="sm" asChild disabled={mutation.isPending}>
            <label className="cursor-pointer">
              <FileUp className="h-4 w-4" />
              选择文件
              <input
                type="file"
                accept=".json,.jsonl,.txt,application/json"
                multiple
                className="hidden"
                onChange={handleFiles}
                disabled={mutation.isPending}
              />
            </label>
          </Button>
          <Button
            size="sm"
            onClick={handleRun}
            disabled={mutation.isPending || parsedCount === 0 || !hasAction}
          >
            {mutation.isPending ? <Spinner size="sm" /> : <FileSearch className="h-4 w-4" />}
            校验 JSON
          </Button>
        </div>
      }
    >
      <div className="space-y-3">
        {/* Options */}
        <div className="grid gap-3 rounded-lg bg-muted/30 p-3 lg:grid-cols-[1fr_1.4fr]">
          {/* Check items */}
          <div>
            <div className="mb-2 text-xs font-semibold text-muted-foreground">校验项目</div>
            <div className="flex flex-wrap gap-4 text-sm">
              <label className="flex cursor-pointer items-center gap-2">
                <Checkbox
                  checked={opts.querySubscription}
                  disabled={mutation.isPending}
                  onCheckedChange={(v) => setOpt('querySubscription', Boolean(v))}
                />
                查询订阅
              </label>
              <label className="flex cursor-pointer items-center gap-2">
                <Checkbox
                  checked={opts.queryUsage}
                  disabled={mutation.isPending}
                  onCheckedChange={(v) => setOpt('queryUsage', Boolean(v))}
                />
                查询用量
              </label>
              <label className="flex cursor-pointer items-center gap-2">
                <Checkbox
                  checked={opts.checkLiveness}
                  disabled={mutation.isPending}
                  onCheckedChange={(v) => setOpt('checkLiveness', Boolean(v))}
                />
                模型验活
              </label>
            </div>
            {!hasAction && (
              <div className="mt-2 text-xs text-destructive">至少选择一个校验项目</div>
            )}
          </div>

          {/* Liveness model & prompt */}
          <div className="grid gap-2 sm:grid-cols-2">
            <div className="flex flex-col gap-1">
              <span className="text-xs font-semibold text-muted-foreground">验活模型</span>
              <Select
                value={opts.livenessModel}
                onValueChange={(v) => setOpt('livenessModel', v)}
                disabled={mutation.isPending || !opts.checkLiveness}
              >
                <SelectTrigger size="sm">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {TEST_MODELS.map((m) => (
                    <SelectItem key={m.id} value={m.id}>{m.label}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="flex flex-col gap-1">
              <span className="text-xs font-semibold text-muted-foreground">验活提示词</span>
              <Input
                value={opts.livenessPrompt}
                disabled={mutation.isPending || !opts.checkLiveness}
                onChange={(e) => setOpt('livenessPrompt', e.target.value)}
                placeholder={DEFAULT_TEST_PROMPT}
              />
            </div>
          </div>
        </div>

        {/* JSON textarea */}
        <Textarea
          className="min-h-52 font-mono text-xs"
          value={raw}
          onChange={(e) => setRaw(e.target.value)}
          placeholder="粘贴 KAM JSON、credentials 数组或 JSONL"
        />

        {/* Footer info */}
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <span>
            {raw.trim() && parsedCount === 0
              ? <span className="text-destructive font-medium">解析失败，请检查 JSON 格式</span>
              : `已解析 ${parsedCount} 条可校验账号`
            }
          </span>
          <span className="flex items-center gap-1">
            <Upload className="h-3.5 w-3.5" />
            {selectedFiles.length
              ? `已选择 ${selectedFiles.length} 个文件`
              : '文件内容可直接粘贴到这里'}
          </span>
        </div>
      </div>
    </SectionCard>
  )
}

// ---------------------------------------------------------------------------
// ValidationPage (exported)
// ---------------------------------------------------------------------------

export function ValidationPage() {
  const [result, setResult] = useState<CredentialValidationResponse | null>(null)

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.validation.title}
        subtitle={pageMeta.validation.subtitle}
      />

      <ExistingValidationSection
        onResult={setResult}
      />

      <ExternalValidationSection
        onResult={setResult}
      />

      <SectionCard title="校验结果">
        <ValidationResults result={result} />
      </SectionCard>
    </PageContainer>
  )
}
