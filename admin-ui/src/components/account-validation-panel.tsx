import { AlertTriangle, FileSearch, FileUp, RefreshCw, Upload } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { validateExistingCredentials, validateExternalCredentials } from '@/api/credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, buildTestModelOptions, defaultTestModelForOptions, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import type { AddCredentialRequest, CredentialValidationGroup, CredentialValidationItem, CredentialValidationResponse } from '@/types/api'

function formatNumber(value: number | null | undefined): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '0'
  return new Intl.NumberFormat('zh-CN').format(value as number)
}

function formatQuota(value: number | null | undefined): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 }).format(value as number)
}

function formatDate(value?: string | null): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', { hour12: false })
}

function badgeVariant(key: string): 'default' | 'secondary' | 'destructive' | 'outline' {
  if (key === 'downgraded' || key === 'failed') return 'destructive'
  if (key === 'upgraded' || key === 'pro' || key === 'pro_plus') return 'default'
  return 'secondary'
}

function itemTitle(item: CredentialValidationItem) {
  if (item.id) return `#${item.id} ${item.email || ''}`.trim()
  return `#${item.index || '-'} ${item.email || ''}`.trim()
}

function quotaText(item: CredentialValidationItem) {
  if (!item.current) return '-'
  return `${formatQuota(item.current.currentUsage)}/${formatQuota(item.current.usageLimit)}`
}

function actionBadgeVariant(checked?: boolean, ok?: boolean | null): 'default' | 'secondary' | 'destructive' | 'outline' {
  if (!checked) return 'outline'
  if (ok === true) return 'default'
  if (ok === false) return 'destructive'
  return 'secondary'
}

function ActionBadge({ label, checked, ok }: { label: string; checked?: boolean; ok?: boolean | null }) {
  return (
    <Badge variant={actionBadgeVariant(checked, ok)}>
      {label}{checked ? ok === true ? ' OK' : ok === false ? ' 失败' : ' 未知' : ' 未检查'}
    </Badge>
  )
}

function ResultGroup({ group }: { group: CredentialValidationGroup }) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-base">
          <Badge variant={badgeVariant(group.key)}>{group.title}</Badge>
          <span className="text-sm font-normal text-muted-foreground">{group.count} 个</span>
        </CardTitle>
      </CardHeader>
      <CardContent className="divide-y p-0">
        {group.items.map((item) => (
          <div key={`${item.id || 'external'}-${item.index || item.email || item.subscriptionTitle}`} className="grid gap-3 px-4 py-3 text-sm lg:grid-cols-[1.4fr_1.2fr_1fr_1.3fr_1.2fr]">
            <div className="min-w-0">
              <div className="truncate font-semibold" title={itemTitle(item)}>{itemTitle(item)}</div>
              <div className="mt-1 flex flex-wrap gap-1">
                {item.disabled !== null && item.disabled !== undefined && <Badge variant={item.disabled ? 'destructive' : 'secondary'}>{item.disabled ? '已禁用' : '启用'}</Badge>}
                {item.matchedExistingCredentialId && <Badge variant="outline">匹配系统 #{item.matchedExistingCredentialId}</Badge>}
                {item.existingDisabled && <Badge variant="destructive">系统内已禁用</Badge>}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">校验项目</div>
              <div className="mt-1 flex flex-wrap gap-1">
                <ActionBadge label="订阅" checked={item.subscriptionChecked} ok={item.subscriptionOk} />
                <ActionBadge label="用量" checked={item.usageChecked} ok={item.usageOk} />
                <ActionBadge label="验活" checked={item.livenessChecked} ok={item.livenessOk} />
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">订阅</div>
              <div className="font-medium">{item.current?.subscriptionTitle || item.subscriptionTitle || '-'}</div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">额度</div>
              <div className="font-mono font-medium">{quotaText(item)}</div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">状态</div>
              <div className="break-words">
                {item.error || item.usageError || item.livenessError
                  ? <span className="text-destructive">{item.error || item.usageError || item.livenessError}</span>
                  : item.current ? formatDate(item.current.checkedAt) : '-'}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">验活响应</div>
              <div className="line-clamp-2 break-words text-xs">
                {item.livenessChecked
                  ? `${item.livenessModel ? testModelLabel(item.livenessModel) : '默认模型'}${item.livenessResponse ? `: ${item.livenessResponse}` : ''}`
                  : '-'}
              </div>
            </div>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}

function Results({ result }: { result: CredentialValidationResponse | null }) {
  if (!result) {
    return <Card><CardContent className="py-8 text-center text-muted-foreground">暂无校验结果</CardContent></Card>
  }
  return (
    <div className="space-y-4">
      <div className="grid gap-4 md:grid-cols-3 lg:grid-cols-6">
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">总数</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold">{formatNumber(result.total)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">成功</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-green-600">{formatNumber(result.success)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">失败</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-amber-600">{formatNumber(result.failed)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">升级</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-emerald-600">{formatNumber(result.upgraded)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">疑似掉级</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-red-600">{formatNumber(result.downgraded)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">无变化</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-muted-foreground">{formatNumber(result.unchanged)}</div></CardContent></Card>
      </div>
      {result.groups.length === 0 ? <Card><CardContent className="py-8 text-center text-muted-foreground">暂无分组结果</CardContent></Card> : result.groups.map(group => <ResultGroup key={group.key} group={group} />)}
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
  const modelCapabilities = useModelCapabilities()
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
  const modelOptions = useMemo(
    () => buildTestModelOptions(modelCapabilities.data?.models),
    [modelCapabilities.data?.models]
  )
  const defaultLivenessModel = defaultTestModelForOptions(modelOptions)
  const hasExternalAction = externalOptions.querySubscription || externalOptions.queryUsage || externalOptions.checkLiveness

  useEffect(() => {
    if (modelOptions.some((option) => option.id === externalOptions.livenessModel)) return
    setExternalOptions((prev) => ({ ...prev, livenessModel: defaultLivenessModel }))
  }, [defaultLivenessModel, externalOptions.livenessModel, modelOptions])

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

  const validateExternal = async () => {
    let credentials
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
    if (parsed.errors.length) {
      toast.warning(`部分文件未读取: ${parsed.errors.slice(0, 3).join('；')}`)
    }
    if (!parsed.credentials.length && !parsed.errors.length) {
      toast.error('没有读取到有效账号')
    }
  }

  const updateExternalOption = <K extends keyof ExternalValidationOptions>(key: K, value: ExternalValidationOptions[K]) => {
    setExternalOptions((prev) => ({ ...prev, [key]: value }))
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <CardTitle>系统账号复查</CardTitle>
              <CardDescription>强制查询系统内账号的订阅、额度和用量，并和上次快照比较。禁用账号也可以手动复查。</CardDescription>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => validateExisting('all')} disabled={loading}><RefreshCw className="h-4 w-4 mr-2" />全部复查</Button>
              <Button variant="outline" size="sm" onClick={() => validateExisting('enabled')} disabled={loading}>仅启用</Button>
              <Button variant="outline" size="sm" onClick={() => validateExisting('disabled')} disabled={loading}>仅禁用</Button>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="flex items-start gap-2 rounded-md border border-amber-200 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
            <div>掉级判断基于上次保存的信息和本次强制查询结果；首次查询没有历史快照时会标记为未知。</div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <CardTitle>外部 JSON 校验</CardTitle>
              <CardDescription>粘贴或选择 Kiro Account Manager / 批量导入格式 JSON，不导入系统、不改变调度。</CardDescription>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button asChild variant="outline" size="sm" disabled={loading}>
                <label>
                  <FileUp className="h-4 w-4 mr-2" />
                  选择文件
                  <input type="file" accept=".json,.jsonl,.txt,application/json" multiple className="hidden" onChange={handleExternalFiles} disabled={loading} />
                </label>
              </Button>
              <Button size="sm" onClick={validateExternal} disabled={loading || parsedCount === 0 || !hasExternalAction}><FileSearch className="h-4 w-4 mr-2" />校验 JSON</Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="grid gap-3 rounded-md border bg-muted/20 p-3 lg:grid-cols-[1fr_1.4fr]">
            <div>
              <div className="mb-2 text-xs font-semibold text-muted-foreground">校验项目</div>
              <div className="flex flex-wrap gap-3 text-sm">
                <label className="inline-flex items-center gap-2">
                  <Checkbox checked={externalOptions.querySubscription} disabled={loading} onCheckedChange={(checked) => updateExternalOption('querySubscription', checked === true)} />
                  查询订阅
                </label>
                <label className="inline-flex items-center gap-2">
                  <Checkbox checked={externalOptions.queryUsage} disabled={loading} onCheckedChange={(checked) => updateExternalOption('queryUsage', checked === true)} />
                  查询用量
                </label>
                <label className="inline-flex items-center gap-2">
                  <Checkbox checked={externalOptions.checkLiveness} disabled={loading} onCheckedChange={(checked) => updateExternalOption('checkLiveness', checked === true)} />
                  模型验活
                </label>
              </div>
              {!hasExternalAction && <div className="mt-2 text-xs text-destructive">至少选择一个校验项目</div>}
            </div>
            <div className="grid gap-2 sm:grid-cols-2">
              <label className="space-y-1">
                <span className="text-xs font-semibold text-muted-foreground">验活模型</span>
                <select
                  className="h-9 w-full rounded-md border bg-background px-3 text-sm"
                  value={externalOptions.livenessModel}
                  disabled={loading || !externalOptions.checkLiveness}
                  onChange={(event) => updateExternalOption('livenessModel', event.target.value)}
                >
                  {modelOptions.map((model) => (
                    <option key={model.id} value={model.id}>{model.label}</option>
                  ))}
                </select>
              </label>
              <label className="space-y-1">
                <span className="text-xs font-semibold text-muted-foreground">验活提示词</span>
                <Input value={externalOptions.livenessPrompt} disabled={loading || !externalOptions.checkLiveness} onChange={(event) => updateExternalOption('livenessPrompt', event.target.value)} />
              </label>
            </div>
          </div>
          <textarea
            className="min-h-[220px] w-full rounded-md border bg-background px-3 py-2 font-mono text-xs"
            value={raw}
            onChange={event => setRaw(event.target.value)}
            placeholder="粘贴 KAM JSON、credentials 数组或 JSONL"
          />
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <span>已解析 {parsedCount} 条可校验账号</span>
            <span className="inline-flex items-center gap-1">
              <Upload className="h-3.5 w-3.5" />
              {selectedFiles.length ? `已选择 ${selectedFiles.length} 个文件` : '文件内容可直接粘贴到这里'}
            </span>
          </div>
        </CardContent>
      </Card>

      <Results result={result} />
    </div>
  )
}
