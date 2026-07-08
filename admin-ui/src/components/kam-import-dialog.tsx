import { useState, useMemo } from 'react'
import { toast } from 'sonner'
import { CheckCircle2, XCircle, AlertCircle, Loader2, FileUp, RotateCw } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { useCredentials, useAddCredential, useDeleteCredential, useProxyResources } from '@/hooks/use-credentials'
import {
  CredentialParameterDefaultsPanel,
  initialParameterDefaults,
  mergeCredentialDefaults,
  optionalTrimmed,
} from '@/components/credential-parameter-defaults'
import { getCredentialBalance, setCredentialDisabled, testCredential } from '@/api/credentials'
import { extractErrorMessage, sha256Hex } from '@/lib/utils'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, testModelLabel } from '@/lib/test-models'
import { camelizeKeys } from '@/lib/object-keys'
import type { AddCredentialRequest } from '@/types/api'

interface KamImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

// KAM 导出 JSON 中的账号结构
interface KamAccount {
  email?: string
  userId?: string | null
  nickname?: string
  credentials: {
    accessToken?: string
    expiresAt?: string
    refreshToken: string
    clientId?: string
    clientSecret?: string
    tokenEndpoint?: string
    issuerUrl?: string
    scopes?: string
    profileArn?: string
    region?: string
    apiRegion?: string
    authMethod?: string
    startUrl?: string
  }
  machineId?: string
  status?: string
}

interface VerificationResult {
  index: number
  status: 'pending' | 'checking' | 'verifying' | 'verified' | 'duplicate' | 'failed' | 'skipped'
  account?: KamAccount
  error?: string
  model?: string
  response?: string
  email?: string
  credentialId?: number
  rollbackStatus?: 'success' | 'failed' | 'skipped'
  rollbackError?: string
}

type ImportVerificationMode = 'model_and_subscription' | 'subscription_only'

type JsonObject = Record<string, unknown>

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function stringField(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined
}

function stringLikeField(value: unknown): string | undefined {
  if (typeof value === 'string' && value.trim()) {
    const trimmed = value.trim()
    if (/^\d+(\.\d+)?$/.test(trimmed)) return timestampToIsoString(Number(trimmed))
    return trimmed
  }
  if (typeof value === 'number' && Number.isFinite(value)) {
    return timestampToIsoString(value)
  }
  return undefined
}

function timestampToIsoString(value: number): string | undefined {
  if (!Number.isFinite(value)) return undefined
  const millis = value > 10_000_000_000 ? value : value * 1000
  const date = new Date(millis)
  return Number.isFinite(date.getTime()) ? date.toISOString() : undefined
}

async function verifyImportedCredential(
  credentialId: number,
  mode: ImportVerificationMode
): Promise<{ model: string; response: string }> {
  if (mode === 'subscription_only') {
    const info = await getCredentialBalance(credentialId)
    return {
      model: '订阅查询',
      response: `订阅: ${info.subscriptionTitle || '未知'}，用量 ${info.currentUsage}/${info.usageLimit}`,
    }
  }

  const testResult = await testCredential(credentialId, {
    model: DEFAULT_TEST_MODEL,
    prompt: DEFAULT_TEST_PROMPT,
  })
  try {
    await getCredentialBalance(credentialId)
  } catch (error) {
    toast.warning(`账号 #${credentialId} 验活成功，但查询信息失败: ${extractErrorMessage(error)}`)
  }
  return {
    model: testModelLabel(testResult.model),
    response: testResult.response,
  }
}

// 兼容 KAM 1.8.3 新版平铺格式，统一转换为旧格式（credentials 嵌套结构）
function normalizeKamAccount(item: unknown): unknown {
  const normalized = camelizeKeys(item)
  if (!isObject(normalized)) return normalized
  const obj = normalized
  const nested = isObject(obj.credentials) ? obj.credentials : undefined
  const source = nested ?? obj
  const refreshToken = stringField(source.refreshToken)
  if (!refreshToken) {
    return normalized
  }

  return {
    email: stringField(obj.email),
    userId: typeof obj.userId === 'string' || obj.userId === null ? (obj.userId as string | null) : undefined,
    nickname: stringField(obj.nickname) ?? stringField(obj.label),
    status: stringField(obj.status),
    machineId: stringField(obj.machineId) ?? stringField(source.machineId),
    credentials: {
      accessToken: stringField(source.accessToken),
      expiresAt: stringLikeField(source.expiresAt) ?? stringLikeField(source.expired),
      refreshToken,
      clientId: stringField(source.clientId),
      clientSecret: stringField(source.clientSecret),
      tokenEndpoint: stringField(source.tokenEndpoint),
      issuerUrl: stringField(source.issuerUrl),
      scopes: stringField(source.scopes) ?? stringField(source.scope),
      profileArn: stringField(source.profileArn),
      region: stringField(source.region),
      apiRegion: stringField(source.apiRegion),
      authMethod: stringField(source.authMethod),
      startUrl: stringField(source.startUrl),
    },
  }
}

function normalizedKamAuthMethod(method: unknown): AddCredentialRequest['authMethod'] | undefined {
  const compact = stringField(method)?.toLowerCase().replace(/[^a-z0-9]/g, '')
  if (!compact) return undefined
  if (compact === 'externalidp' || compact === 'enterprise' || compact === 'iamsso' || compact === 'awsidc') return 'external_idp'
  if (compact === 'idc' || compact === 'builderid' || compact === 'iam') return 'idc'
  if (compact === 'social') return 'social'
  if (compact === 'apikey') return 'api_key'
  return undefined
}

// 校验元素是否为有效的 KAM 账号结构
function isValidKamAccount(item: unknown): item is KamAccount {
  if (typeof item !== 'object' || item === null) return false
  const obj = item as Record<string, unknown>
  if (typeof obj.credentials !== 'object' || obj.credentials === null) return false
  const cred = obj.credentials as Record<string, unknown>
  return typeof cred.refreshToken === 'string' && cred.refreshToken.trim().length > 0
}

// 解析 KAM 导出 JSON，支持单账号和多账号格式
function parseKamJson(raw: string): KamAccount[] {
  const parsed = camelizeKeys(JSON.parse(raw)) as Record<string, unknown>

  let rawItems: unknown[]

  // 标准 KAM 导出格式：{ version, accounts: [...] }
  if (parsed.accounts && Array.isArray(parsed.accounts)) {
    rawItems = parsed.accounts
  }
  // 直接数组（含 KAM 1.8.3 新版平铺格式）
  else if (Array.isArray(parsed)) {
    rawItems = parsed
  }
  // 单个账号对象（旧格式，有 credentials 字段）
  else if (parsed.credentials && typeof parsed.credentials === 'object') {
    rawItems = [parsed]
  }
  // 单个账号对象（新格式，refreshToken 平铺）
  else if (typeof parsed.refreshToken === 'string') {
    rawItems = [parsed]
  }
  else {
    throw new Error('无法识别的 KAM JSON 格式')
  }

  // 兼容新格式：将平铺账号统一转换为 credentials 嵌套结构
  const normalizedItems = rawItems.map(normalizeKamAccount)
  const validAccounts = normalizedItems.filter(isValidKamAccount)

  if (rawItems.length > 0 && validAccounts.length === 0) {
    throw new Error(`共 ${rawItems.length} 条记录，但均缺少有效的 credentials.refreshToken`)
  }

  if (validAccounts.length < rawItems.length) {
    const skipped = rawItems.length - validAccounts.length
    console.warn(`KAM 导入：跳过 ${skipped} 条缺少有效 credentials.refreshToken 的记录`)
  }

  return validAccounts
}

async function parseKamFiles(files: File[]): Promise<{ accounts: KamAccount[]; errors: string[] }> {
  const accounts: KamAccount[] = []
  const errors: string[] = []

  for (const file of files) {
    try {
      const parsed = parseKamJson(await file.text())
      if (parsed.length === 0) {
        errors.push(`${file.name}: 未找到有效账号`)
      } else {
        accounts.push(...parsed)
      }
    } catch (error) {
      errors.push(`${file.name}: ${extractErrorMessage(error)}`)
    }
  }

  return { accounts, errors }
}

export function KamImportDialog({ open, onOpenChange }: KamImportDialogProps) {
  const [jsonInput, setJsonInput] = useState('')
  const [verificationMode, setVerificationMode] = useState<ImportVerificationMode>('subscription_only')
  const [importing, setImporting] = useState(false)
  const [skipErrorAccounts, setSkipErrorAccounts] = useState(true)
  const [progress, setProgress] = useState({ current: 0, total: 0 })
  const [currentProcessing, setCurrentProcessing] = useState<string>('')
  const [results, setResults] = useState<VerificationResult[]>([])
  const [defaults, setDefaults] = useState(initialParameterDefaults)

  const { data: existingCredentials } = useCredentials({ enabled: open })
  const { mutateAsync: addCredential } = useAddCredential()
  const { mutateAsync: deleteCredential } = useDeleteCredential()
  const proxyResources = useProxyResources()
  const proxyResourceOptions = (proxyResources.data?.resources || []).filter(resource => resource.enabled)

  const rollbackCredential = async (id: number): Promise<{ success: boolean; error?: string }> => {
    try {
      await setCredentialDisabled(id, true)
    } catch (error) {
      return { success: false, error: `禁用失败: ${extractErrorMessage(error)}` }
    }
    try {
      await deleteCredential(id)
      return { success: true }
    } catch (error) {
      return { success: false, error: `删除失败: ${extractErrorMessage(error)}` }
    }
  }

  const resetForm = () => {
    setJsonInput('')
    setProgress({ current: 0, total: 0 })
    setCurrentProcessing('')
    setResults([])
    setDefaults(initialParameterDefaults())
    setVerificationMode('subscription_only')
  }

  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (files.length === 0) {
      return
    }

    const result = await parseKamFiles(files)
    if (result.accounts.length > 0) {
      const current = jsonInput.trim()
      let existing: KamAccount[] = []
      if (current) {
        try {
          existing = parseKamJson(current)
        } catch {
          existing = []
        }
      }
      setJsonInput(JSON.stringify({ accounts: [...existing, ...result.accounts] }, null, 2))
      toast.success(`已从 ${files.length} 个文件读取 ${result.accounts.length} 个账号`)
    }
    if (result.errors.length > 0) {
      toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
    }
    if (result.accounts.length === 0 && result.errors.length === 0) {
      toast.error('没有读取到有效账号')
    }
  }

  const handleImport = async (retryAccounts?: KamAccount[]) => {
    let validAccounts: KamAccount[]
    if (retryAccounts) {
      validAccounts = retryAccounts
    } else {
      // 先单独解析 JSON，给出精准的错误提示
      try {
        const accounts = parseKamJson(jsonInput)

        if (accounts.length === 0) {
          toast.error('没有可导入的账号')
          return
        }

        validAccounts = accounts.filter(a => a.credentials?.refreshToken)
        if (validAccounts.length === 0) {
          toast.error('没有包含有效 refreshToken 的账号')
          return
        }
      } catch (error) {
        toast.error('JSON 格式错误: ' + extractErrorMessage(error))
        return
      }
    }

    try {

      setImporting(true)
      setProgress({ current: 0, total: validAccounts.length })

      // 初始化结果，标记 error 状态的账号
      const initialResults: VerificationResult[] = validAccounts.map((account, i) => {
        if (skipErrorAccounts && account.status === 'error') {
          return { index: i + 1, status: 'skipped' as const, email: account.email || account.nickname, account }
        }
        return { index: i + 1, status: 'pending' as const, email: account.email || account.nickname, account }
      })
      setResults(initialResults)

      // 重复检测
      const existingTokenHashes = new Set(
        existingCredentials?.credentials
          .map(c => c.refreshTokenHash)
          .filter((hash): hash is string => Boolean(hash)) || []
      )

      let successCount = 0
      let duplicateCount = 0
      let failCount = 0
      let skippedCount = 0

      for (let i = 0; i < validAccounts.length; i++) {
        const account = validAccounts[i]

        // 跳过 error 状态的账号
        if (skipErrorAccounts && account.status === 'error') {
          skippedCount++
          setProgress({ current: i + 1, total: validAccounts.length })
          continue
        }

        const cred = account.credentials
        const token = cred.refreshToken.trim()
        const tokenHash = await sha256Hex(token)

        setCurrentProcessing(`正在处理 ${account.email || account.nickname || `账号 ${i + 1}`}`)
        setResults(prev => {
          const next = [...prev]
          next[i] = { ...next[i], status: 'checking' }
          return next
        })

        // 检查重复
        if (existingTokenHashes.has(tokenHash)) {
          duplicateCount++
          const existingCred = existingCredentials?.credentials.find(c => c.refreshTokenHash === tokenHash)
          setResults(prev => {
            const next = [...prev]
            next[i] = { ...next[i], status: 'duplicate', error: '该账号已存在', email: existingCred?.email || account.email }
            return next
          })
          setProgress({ current: i + 1, total: validAccounts.length })
          continue
        }

        // 验活中
        setResults(prev => {
          const next = [...prev]
          next[i] = { ...next[i], status: 'verifying' }
          return next
        })

        let addedCredId: number | null = null

        try {
          const clientId = stringField(cred.clientId)
          const clientSecret = stringField(cred.clientSecret)
          const authMethod = normalizedKamAuthMethod(cred.authMethod) === 'external_idp'
            ? 'external_idp'
            : normalizedKamAuthMethod(cred.authMethod) === 'idc' || (clientId && clientSecret)
              ? 'idc'
              : 'social'

          if (authMethod === 'idc' && (!clientId || !clientSecret)) {
            throw new Error('idc 模式需要同时提供 clientId 和 clientSecret')
          }
          if (authMethod === 'external_idp' && !clientId) {
            throw new Error('external_idp 模式需要提供 clientId')
          }
          if (authMethod === 'social' && (clientId || clientSecret)) {
            throw new Error('social 模式不应提供 clientId 或 clientSecret；企业 SSO 请设置 authMethod 为 external_idp')
          }

          const accountRegion = optionalTrimmed(cred.region) || optionalTrimmed(defaults.region)
          const baseCredential: AddCredentialRequest = {
            refreshToken: token,
            authMethod,
            accessToken: stringField(cred.accessToken),
            expiresAt: stringLikeField(cred.expiresAt),
            email: stringField(account.email),
            profileArn: stringField(cred.profileArn),
            region: stringField(cred.region),
            authRegion: optionalTrimmed(defaults.authRegion) || accountRegion,
            apiRegion: stringField(cred.apiRegion),
            clientId,
            clientSecret: authMethod === 'idc' ? clientSecret : undefined,
            tokenEndpoint: authMethod === 'external_idp' ? stringField(cred.tokenEndpoint) : undefined,
            issuerUrl: authMethod === 'external_idp' ? stringField(cred.issuerUrl) : undefined,
            scopes: authMethod === 'external_idp' ? stringField(cred.scopes) : undefined,
            machineId: stringField(account.machineId),
          }
          const addedCred = await addCredential(mergeCredentialDefaults(baseCredential, { ...defaults, authRegion: '' }))

          addedCredId = addedCred.credentialId

          await new Promise(resolve => setTimeout(resolve, 1000))

          const verification = await verifyImportedCredential(addedCred.credentialId, verificationMode)

          successCount++
          existingTokenHashes.add(tokenHash)
          setCurrentProcessing(`验活成功: ${addedCred.email || account.email || `账号 ${i + 1}`}`)
          setResults(prev => {
            const next = [...prev]
            next[i] = {
              ...next[i],
              status: 'verified',
              model: verification.model,
              response: verification.response,
              email: addedCred.email || account.email,
              credentialId: addedCred.credentialId,
            }
            return next
          })
        } catch (error) {
          let rollbackStatus: VerificationResult['rollbackStatus'] = 'skipped'
          let rollbackError: string | undefined

          if (addedCredId) {
            const result = await rollbackCredential(addedCredId)
            if (result.success) {
              rollbackStatus = 'success'
            } else {
              rollbackStatus = 'failed'
              rollbackError = result.error
            }
          }

          failCount++
          setResults(prev => {
            const next = [...prev]
            next[i] = {
              ...next[i],
              status: 'failed',
              error: extractErrorMessage(error),
              rollbackStatus,
              rollbackError,
            }
            return next
          })
        }

        setProgress({ current: i + 1, total: validAccounts.length })
      }

      // 汇总
      const parts: string[] = []
      if (successCount > 0) parts.push(`成功 ${successCount}`)
      if (duplicateCount > 0) parts.push(`重复 ${duplicateCount}`)
      if (failCount > 0) parts.push(`失败 ${failCount}`)
      if (skippedCount > 0) parts.push(`跳过 ${skippedCount}`)

      if (failCount === 0 && duplicateCount === 0 && skippedCount === 0) {
        toast.success(`成功导入并验活 ${successCount} 个账号`)
      } else {
        toast.info(`导入完成：${parts.join('，')}`)
      }
    } catch (error) {
      toast.error('导入失败: ' + extractErrorMessage(error))
    } finally {
      setImporting(false)
    }
  }

  const failedAccounts = results
    .filter((result): result is VerificationResult & { account: KamAccount } => (
      result.status === 'failed' && Boolean(result.account)
    ))
    .map((result) => result.account)

  const handleRetryFailed = async () => {
    if (failedAccounts.length === 0) {
      toast.error('没有可重试的失败账号')
      return
    }
    await handleImport(failedAccounts)
  }

  const getStatusIcon = (status: VerificationResult['status']) => {
    switch (status) {
      case 'pending':
        return <div className="w-5 h-5 rounded-full border-2 border-gray-300" />
      case 'checking':
      case 'verifying':
        return <Loader2 className="w-5 h-5 animate-spin text-blue-500" />
      case 'verified':
        return <CheckCircle2 className="w-5 h-5 text-green-500" />
      case 'duplicate':
        return <AlertCircle className="w-5 h-5 text-yellow-500" />
      case 'skipped':
        return <AlertCircle className="w-5 h-5 text-gray-400" />
      case 'failed':
        return <XCircle className="w-5 h-5 text-red-500" />
    }
  }

  const getStatusText = (result: VerificationResult) => {
    switch (result.status) {
      case 'pending': return '等待中'
      case 'checking': return '检查重复...'
      case 'verifying': return '验活中...'
      case 'verified': return '验活成功'
      case 'duplicate': return '重复账号'
      case 'skipped': return '已跳过（error 状态）'
      case 'failed':
        if (result.rollbackStatus === 'success') return '验活失败（已排除）'
        if (result.rollbackStatus === 'failed') return '验活失败（未排除）'
        return '验活失败（未创建）'
    }
  }

  // 预览解析结果
  const { previewAccounts, parseError } = useMemo(() => {
    if (!jsonInput.trim()) return { previewAccounts: [] as KamAccount[], parseError: '' }
    try {
      return { previewAccounts: parseKamJson(jsonInput), parseError: '' }
    } catch (e) {
      return { previewAccounts: [] as KamAccount[], parseError: extractErrorMessage(e) }
    }
  }, [jsonInput])

  const errorAccountCount = previewAccounts.filter(a => a.status === 'error').length

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        if (!newOpen && importing) return
        if (!newOpen) resetForm()
        onOpenChange(newOpen)
      }}
    >
      <DialogContent className="sm:max-w-2xl max-h-[80vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>KAM 账号导入（默认查询订阅）</DialogTitle>
        </DialogHeader>

        <div className="flex-1 overflow-y-auto space-y-4 py-4">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <label className="text-sm font-medium">KAM 导出 JSON</label>
              <Button type="button" variant="outline" size="sm" disabled={importing} asChild>
                <label className="cursor-pointer">
                  <FileUp className="h-4 w-4 mr-2" />
                  选择文件
                  <input
                    type="file"
                    accept=".json,.jsonl,.txt,application/json"
                    multiple
                    className="hidden"
                    onChange={handleFileSelect}
                    disabled={importing}
                  />
                </label>
              </Button>
            </div>
            <textarea
              placeholder={'粘贴 Kiro Account Manager 导出的 JSON，或选择一个/多个文件\n\n每个文件可以包含单个账号，也可以包含 accounts 数组或账号数组。\n\n支持 KAM 1.8.3+ 新版平铺格式：\n[\n  {\n    "email": "...",\n    "refreshToken": "...",\n    "clientId": "...",\n    "clientSecret": "...",\n    "region": "us-east-1"\n  }\n]\n\n也支持旧版嵌套格式：\n{\n  "version": "1.5.0",\n  "accounts": [\n    {\n      "email": "...",\n      "credentials": {\n        "refreshToken": "...",\n        "clientId": "...",\n        "clientSecret": "...",\n        "region": "us-east-1"\n      }\n    }\n  ]\n}'}
              value={jsonInput}
              onChange={(e) => setJsonInput(e.target.value)}
              disabled={importing}
              className="flex min-h-[200px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
            />
            <p className="text-xs text-muted-foreground">
              支持单选或多选文件，每个文件可包含单个账号或多个账号。
            </p>
          </div>

          <CredentialParameterDefaultsPanel
            defaults={defaults}
            onChange={setDefaults}
            proxyResources={proxyResourceOptions}
            disabled={importing}
            title="KAM 导入默认参数"
          />

          <div className="rounded-md border bg-muted/20 p-3">
            <label htmlFor="kamImportVerificationMode" className="text-sm font-semibold">
              验活方式
            </label>
            <select
              id="kamImportVerificationMode"
              value={verificationMode}
              onChange={(event) => setVerificationMode(event.target.value as ImportVerificationMode)}
              disabled={importing}
              className="mt-2 flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
            >
              <option value="subscription_only">只查询订阅（不请求模型）</option>
              <option value="model_and_subscription">测试模型 + 查询订阅</option>
            </select>
            <p className="mt-1 text-xs leading-5 text-muted-foreground">
              只查询订阅时不会发送模型测试请求；订阅查询失败的账号仍会按验活失败回滚。
            </p>
          </div>

          {/* 解析预览 */}
          {parseError && (
            <div className="text-sm text-red-600 dark:text-red-400">解析失败: {parseError}</div>
          )}
          {previewAccounts.length > 0 && !importing && results.length === 0 && (
            <div className="space-y-2">
              <div className="text-sm text-muted-foreground">
                识别到 {previewAccounts.length} 个账号
                {errorAccountCount > 0 && `（其中 ${errorAccountCount} 个为 error 状态）`}
              </div>
              {errorAccountCount > 0 && (
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={skipErrorAccounts}
                    onChange={(e) => setSkipErrorAccounts(e.target.checked)}
                    className="rounded border-gray-300"
                  />
                  跳过 error 状态的账号
                </label>
              )}
            </div>
          )}

          {/* 导入进度和结果 */}
          {(importing || results.length > 0) && (
            <>
              <div className="space-y-2">
                <div className="flex justify-between text-sm">
                  <span>{importing ? '导入进度' : '导入完成'}</span>
                  <span>{progress.current} / {progress.total}</span>
                </div>
                <div className="w-full bg-secondary rounded-full h-2">
                  <div
                    className="bg-primary h-2 rounded-full transition-all"
                    style={{ width: `${progress.total > 0 ? (progress.current / progress.total) * 100 : 0}%` }}
                  />
                </div>
                {importing && currentProcessing && (
                  <div className="text-xs text-muted-foreground">{currentProcessing}</div>
                )}
              </div>

              <div className="flex gap-4 text-sm">
                <span className="text-green-600 dark:text-green-400">
                  ✓ 成功: {results.filter(r => r.status === 'verified').length}
                </span>
                <span className="text-yellow-600 dark:text-yellow-400">
                  ⚠ 重复: {results.filter(r => r.status === 'duplicate').length}
                </span>
                <span className="text-red-600 dark:text-red-400">
                  ✗ 失败: {results.filter(r => r.status === 'failed').length}
                </span>
                <span className="text-gray-500">
                  ○ 跳过: {results.filter(r => r.status === 'skipped').length}
                </span>
              </div>

              <div className="border rounded-md divide-y max-h-[300px] overflow-y-auto">
                {results.map((result) => (
                  <div key={result.index} className="p-3">
                    <div className="flex items-start gap-3">
                      {getStatusIcon(result.status)}
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="text-sm font-medium">
                            {result.email || `账号 #${result.index}`}
                          </span>
                          <span className="text-xs text-muted-foreground">
                            {getStatusText(result)}
                          </span>
                        </div>
                        {result.model && (
                          <div className="text-xs text-muted-foreground mt-1">模型: {result.model}</div>
                        )}
                        {result.response && (
                          <div className="text-xs text-muted-foreground mt-1 line-clamp-2">
                            响应: {result.response}
                          </div>
                        )}
                        {result.error && (
                          <div className="text-xs text-red-600 dark:text-red-400 mt-1">{result.error}</div>
                        )}
                        {result.rollbackError && (
                          <div className="text-xs text-red-600 dark:text-red-400 mt-1">回滚失败: {result.rollbackError}</div>
                        )}
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => { onOpenChange(false); resetForm() }}
            disabled={importing}
          >
            {importing ? '导入中...' : results.length > 0 ? '关闭' : '取消'}
          </Button>
          {results.length > 0 && failedAccounts.length > 0 && (
            <Button
              type="button"
              variant="outline"
              onClick={handleRetryFailed}
              disabled={importing}
            >
              <RotateCw className="h-4 w-4 mr-2" />
              重试失败账号
            </Button>
          )}
          {results.length === 0 && (
            <Button
              type="button"
              onClick={() => handleImport()}
              disabled={importing || !jsonInput.trim() || previewAccounts.length === 0 || !!parseError}
            >
              开始导入
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
