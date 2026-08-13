import { useState } from 'react'
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
import { parseCredentialImportFiles, parseCredentialImportText } from '@/lib/credential-import'
import type { AddCredentialRequest } from '@/types/api'

interface BatchImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type CredentialInput = AddCredentialRequest & { region?: string }
type ImportVerificationMode = 'model_and_subscription' | 'subscription_only'

interface VerificationResult {
  index: number
  status: 'pending' | 'checking' | 'importing' | 'verifying' | 'verified' | 'duplicate' | 'failed'
  credential?: CredentialInput
  error?: string
  model?: string
  response?: string
  email?: string
  credentialId?: number
  rollbackStatus?: 'success' | 'failed' | 'skipped'
  rollbackError?: string
}

async function verifyImportedCredential(
  credentialId: number,
  mode: ImportVerificationMode,
  refreshInfoAfterModelTest: boolean
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
  if (refreshInfoAfterModelTest) {
    try {
      await getCredentialBalance(credentialId)
    } catch (error) {
      toast.warning(`账号 #${credentialId} 验活成功，但查询信息失败: ${extractErrorMessage(error)}`)
    }
  }
  return {
    model: testModelLabel(testResult.model),
    response: testResult.response,
  }
}

export function BatchImportDialog({ open, onOpenChange }: BatchImportDialogProps) {
  const [jsonInput, setJsonInput] = useState('')
  const [verificationMode, setVerificationMode] = useState<ImportVerificationMode>('subscription_only')
  const [skipVerification, setSkipVerification] = useState(false)
  const [refreshInfoAfterModelTest, setRefreshInfoAfterModelTest] = useState(false)
  const [autoDiscoverSupportedModels, setAutoDiscoverSupportedModels] = useState(false)
  const [importing, setImporting] = useState(false)
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
      return {
        success: false,
        error: `禁用失败: ${extractErrorMessage(error)}`,
      }
    }

    try {
      await deleteCredential(id)
      return { success: true }
    } catch (error) {
      return {
        success: false,
        error: `删除失败: ${extractErrorMessage(error)}`,
      }
    }
  }

  const resetForm = () => {
    setJsonInput('')
    setProgress({ current: 0, total: 0 })
    setCurrentProcessing('')
    setResults([])
    setDefaults(initialParameterDefaults())
    setVerificationMode('subscription_only')
    setSkipVerification(false)
    setRefreshInfoAfterModelTest(false)
    setAutoDiscoverSupportedModels(false)
  }

  const appendCredentialsToInput = (credentials: AddCredentialRequest[]) => {
    const current = jsonInput.trim()
    let existing: AddCredentialRequest[] = []
    if (current) {
      try {
        existing = parseCredentialImportText(current)
      } catch {
        existing = []
      }
    }
    setJsonInput(JSON.stringify([...existing, ...credentials], null, 2))
  }

  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (files.length === 0) {
      return
    }

    const result = await parseCredentialImportFiles(files)
    if (result.credentials.length > 0) {
      appendCredentialsToInput(result.credentials)
      toast.success(`已从 ${files.length} 个文件读取 ${result.credentials.length} 条账号`)
    }
    if (result.errors.length > 0) {
      toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
    }
    if (result.credentials.length === 0 && result.errors.length === 0) {
      toast.error('没有读取到有效账号')
    }
  }

  const handleBatchImport = async (retryCredentials?: CredentialInput[]) => {
    let credentials: CredentialInput[]
    if (retryCredentials) {
      credentials = retryCredentials
    } else {
      // 先单独解析 JSON，给出精准的错误提示
      try {
        credentials = parseCredentialImportText(jsonInput)
      } catch (error) {
        toast.error('JSON 格式错误: ' + extractErrorMessage(error))
        return
      }
    }

    if (credentials.length === 0) {
      toast.error('没有可导入的账号')
      return
    }

    try {
      credentials = credentials.map(credential => mergeCredentialDefaults(credential, defaults))
    } catch (error) {
      toast.error(extractErrorMessage(error))
      return
    }

    try {
      setImporting(true)
      setProgress({ current: 0, total: credentials.length })

      // 2. 初始化结果
      const initialResults: VerificationResult[] = credentials.map((credential, i) => ({
        index: i + 1,
        status: 'pending',
        credential,
      }))
      setResults(initialResults)

      // 3. 检测重复：OAuth 与 API Key 分别使用对应的 hash 集合
      const existingOauthHashes = new Set(
        existingCredentials?.credentials
          .map(c => c.refreshTokenHash)
          .filter((hash): hash is string => Boolean(hash)) || []
      )
      const existingApiKeyHashes = new Set(
        existingCredentials?.credentials
          .map(c => c.apiKeyHash)
          .filter((hash): hash is string => Boolean(hash)) || []
      )

      let successCount = 0
      let duplicateCount = 0
      let failCount = 0
      let rollbackSuccessCount = 0
      let rollbackFailedCount = 0
      let rollbackSkippedCount = 0

      // 4. 导入并验活
      for (let i = 0; i < credentials.length; i++) {
        const cred = credentials[i]
        const isApiKeyCred = !!(cred.kiroApiKey?.trim()) || cred.authMethod === 'api_key'

        // 更新状态为检查中
        setCurrentProcessing(`正在处理账号 ${i + 1}/${credentials.length}`)
        setResults(prev => {
          const newResults = [...prev]
          newResults[i] = { ...newResults[i], status: 'checking' }
          return newResults
        })

        // 客户端去重：OAuth 基于 refreshToken hash，API Key 基于 kiroApiKey hash
        let credHash = ''
        if (isApiKeyCred) {
          const apiKey = cred.kiroApiKey?.trim() || ''
          if (!apiKey) {
            setResults(prev => {
              const newResults = [...prev]
              newResults[i] = {
                ...newResults[i],
                status: 'failed',
                error: '缺少 kiroApiKey',
              }
              return newResults
            })
            failCount++
            setProgress({ current: i + 1, total: credentials.length })
            continue
          }
          credHash = await sha256Hex(apiKey)
          if (existingApiKeyHashes.has(credHash)) {
            duplicateCount++
            const existingCred = existingCredentials?.credentials.find(c => c.apiKeyHash === credHash)
            setResults(prev => {
              const newResults = [...prev]
              newResults[i] = {
                ...newResults[i],
                status: 'duplicate',
                error: '该账号已存在',
                email: existingCred?.email || undefined
              }
              return newResults
            })
            setProgress({ current: i + 1, total: credentials.length })
            continue
          }
        } else {
          const token = cred.refreshToken?.trim() || ''
          if (!token) {
            setResults(prev => {
              const newResults = [...prev]
              newResults[i] = {
                ...newResults[i],
                status: 'failed',
                error: '缺少 refreshToken',
              }
              return newResults
            })
            failCount++
            setProgress({ current: i + 1, total: credentials.length })
            continue
          }
          credHash = await sha256Hex(token)
          if (existingOauthHashes.has(credHash)) {
            duplicateCount++
            const existingCred = existingCredentials?.credentials.find(c => c.refreshTokenHash === credHash)
            setResults(prev => {
              const newResults = [...prev]
              newResults[i] = {
                ...newResults[i],
                status: 'duplicate',
                error: '该账号已存在',
                email: existingCred?.email || undefined
              }
              return newResults
            })
            setProgress({ current: i + 1, total: credentials.length })
            continue
          }
        }

        // 更新状态为导入/验活中
        setResults(prev => {
          const newResults = [...prev]
          newResults[i] = { ...newResults[i], status: skipVerification ? 'importing' : 'verifying' }
          return newResults
        })

        let addedCredId: number | null = null

        try {
          // 添加账号
          if (isApiKeyCred) {
            // API Key 账号
            const addedCred = await addCredential({
              authMethod: 'api_key',
              kiroApiKey: optionalTrimmed(cred.kiroApiKey),
              email: optionalTrimmed(cred.email),
              profileArn: optionalTrimmed(cred.profileArn),
              priority: cred.priority || 0,
              maxConcurrentRequests: cred.maxConcurrentRequests ?? undefined,
              rpm: cred.rpm ?? undefined,
              disabled: cred.disabled ?? false,
              region: optionalTrimmed(cred.region),
              authRegion: optionalTrimmed(cred.authRegion) || optionalTrimmed(cred.region),
              apiRegion: optionalTrimmed(cred.apiRegion),
              machineId: optionalTrimmed(cred.machineId),
              proxyUrl: optionalTrimmed(cred.proxyUrl),
              proxyUsername: optionalTrimmed(cred.proxyUsername),
              proxyPassword: optionalTrimmed(cred.proxyPassword),
              proxyResourceId: cred.proxyResourceId || undefined,
              endpoint: optionalTrimmed(cred.endpoint),
              enableOverageAfterImport: cred.enableOverageAfterImport ?? undefined,
              autoDiscoverSupportedModels,
            })

            addedCredId = addedCred.credentialId

            if (skipVerification) {
              successCount++
              existingApiKeyHashes.add(credHash)
              setCurrentProcessing(addedCred.email ? `导入成功: ${addedCred.email}` : `导入成功: 账号 ${i + 1}`)
              setResults(prev => {
                const newResults = [...prev]
                newResults[i] = {
                  ...newResults[i],
                  status: 'verified',
                  email: addedCred.email || cred.email || undefined,
                  credentialId: addedCred.credentialId
                }
                return newResults
              })
              setProgress({ current: i + 1, total: credentials.length })
              continue
            }

            // 延迟 1 秒
            await new Promise(resolve => setTimeout(resolve, 1000))

            const verification = await verifyImportedCredential(
              addedCred.credentialId,
              verificationMode,
              refreshInfoAfterModelTest
            )

            successCount++
            existingApiKeyHashes.add(credHash)
            setCurrentProcessing(addedCred.email ? `验活成功: ${addedCred.email}` : `验活成功: 账号 ${i + 1}`)
            setResults(prev => {
              const newResults = [...prev]
              newResults[i] = {
                ...newResults[i],
                status: 'verified',
                model: verification.model,
                response: verification.response,
                email: addedCred.email || cred.email || undefined,
                credentialId: addedCred.credentialId
              }
              return newResults
            })
            setProgress({ current: i + 1, total: credentials.length })
            continue
          }

          // OAuth 账号
          const token = cred.refreshToken!.trim()
          const clientId = optionalTrimmed(cred.clientId)
          const clientSecret = optionalTrimmed(cred.clientSecret)
          const authMethod = cred.authMethod === 'external_idp'
            ? 'external_idp'
            : cred.authMethod === 'idc' || (clientId && clientSecret)
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

          const addedCred = await addCredential({
            refreshToken: token,
            authMethod,
            accessToken: optionalTrimmed(cred.accessToken),
            expiresAt: optionalTrimmed(cred.expiresAt),
            email: optionalTrimmed(cred.email),
            profileArn: optionalTrimmed(cred.profileArn),
            region: optionalTrimmed(cred.region),
            authRegion: optionalTrimmed(cred.authRegion) || optionalTrimmed(cred.region),
            apiRegion: optionalTrimmed(cred.apiRegion),
            clientId,
            clientSecret: authMethod === 'idc' ? clientSecret : undefined,
            tokenEndpoint: authMethod === 'external_idp' ? optionalTrimmed(cred.tokenEndpoint) : undefined,
            issuerUrl: authMethod === 'external_idp' ? optionalTrimmed(cred.issuerUrl) : undefined,
            scopes: authMethod === 'external_idp' ? optionalTrimmed(cred.scopes) : undefined,
            priority: cred.priority || 0,
            maxConcurrentRequests: cred.maxConcurrentRequests ?? undefined,
            rpm: cred.rpm ?? undefined,
            disabled: cred.disabled ?? false,
            machineId: optionalTrimmed(cred.machineId),
            proxyUrl: optionalTrimmed(cred.proxyUrl),
            proxyUsername: optionalTrimmed(cred.proxyUsername),
            proxyPassword: optionalTrimmed(cred.proxyPassword),
            proxyResourceId: cred.proxyResourceId || undefined,
            endpoint: optionalTrimmed(cred.endpoint),
            enableOverageAfterImport: cred.enableOverageAfterImport ?? undefined,
            autoDiscoverSupportedModels,
          })

          addedCredId = addedCred.credentialId

          if (skipVerification) {
            successCount++
            existingOauthHashes.add(credHash)
            setCurrentProcessing(addedCred.email ? `导入成功: ${addedCred.email}` : `导入成功: 账号 ${i + 1}`)
            setResults(prev => {
              const newResults = [...prev]
              newResults[i] = {
                ...newResults[i],
                status: 'verified',
                email: addedCred.email || cred.email || undefined,
                credentialId: addedCred.credentialId
              }
              return newResults
            })
            setProgress({ current: i + 1, total: credentials.length })
            continue
          }

          // 延迟 1 秒
          await new Promise(resolve => setTimeout(resolve, 1000))

          const verification = await verifyImportedCredential(
            addedCred.credentialId,
            verificationMode,
            refreshInfoAfterModelTest
          )

          // 验活成功
          successCount++
          existingOauthHashes.add(credHash)
          setCurrentProcessing(addedCred.email ? `验活成功: ${addedCred.email}` : `验活成功: 账号 ${i + 1}`)
          setResults(prev => {
            const newResults = [...prev]
            newResults[i] = {
              ...newResults[i],
              status: 'verified',
              model: verification.model,
              response: verification.response,
              email: addedCred.email || cred.email || undefined,
              credentialId: addedCred.credentialId
            }
            return newResults
          })
        } catch (error) {
          // 验活失败，尝试回滚（先禁用再删除）
          let rollbackStatus: VerificationResult['rollbackStatus'] = 'skipped'
          let rollbackError: string | undefined

          if (addedCredId) {
            const rollbackResult = await rollbackCredential(addedCredId)
            if (rollbackResult.success) {
              rollbackStatus = 'success'
              rollbackSuccessCount++
            } else {
              rollbackStatus = 'failed'
              rollbackFailedCount++
              rollbackError = rollbackResult.error
            }
          } else {
            rollbackSkippedCount++
          }

          failCount++
          setResults(prev => {
            const newResults = [...prev]
            newResults[i] = {
              ...newResults[i],
              status: 'failed',
              error: extractErrorMessage(error),
              email: undefined,
              rollbackStatus,
              rollbackError,
            }
            return newResults
          })
        }

        setProgress({ current: i + 1, total: credentials.length })
      }

      // 显示结果
      if (failCount === 0 && duplicateCount === 0) {
        toast.success(skipVerification ? `成功导入 ${successCount} 个账号` : `成功导入并验活 ${successCount} 个账号`)
      } else {
        const failureSummary = failCount > 0
          ? `，失败 ${failCount} 个（已排除 ${rollbackSuccessCount}，未排除 ${rollbackFailedCount}，无需排除 ${rollbackSkippedCount}）`
          : ''
        toast.info(`${skipVerification ? '导入' : '验活'}完成：成功 ${successCount} 个，重复 ${duplicateCount} 个${failureSummary}`)

        if (rollbackFailedCount > 0) {
          toast.warning(`有 ${rollbackFailedCount} 个失败账号回滚未完成，请手动禁用并删除`)
        }
      }
    } catch (error) {
      toast.error('导入失败: ' + extractErrorMessage(error))
    } finally {
      setImporting(false)
    }
  }

  const failedCredentials = results
    .filter((result): result is VerificationResult & { credential: CredentialInput } => (
      result.status === 'failed' && Boolean(result.credential)
    ))
    .map((result) => result.credential)

  const handleRetryFailed = async () => {
    if (failedCredentials.length === 0) {
      toast.error('没有可重试的失败账号')
      return
    }
    await handleBatchImport(failedCredentials)
  }

  const getStatusIcon = (status: VerificationResult['status']) => {
    switch (status) {
      case 'pending':
        return <div className="w-5 h-5 rounded-full border-2 border-gray-300" />
      case 'checking':
      case 'importing':
      case 'verifying':
        return <Loader2 className="w-5 h-5 animate-spin text-blue-500" />
      case 'verified':
        return <CheckCircle2 className="w-5 h-5 text-green-500" />
      case 'duplicate':
        return <AlertCircle className="w-5 h-5 text-yellow-500" />
      case 'failed':
        return <XCircle className="w-5 h-5 text-red-500" />
    }
  }

  const getStatusText = (result: VerificationResult) => {
    switch (result.status) {
      case 'pending':
        return '等待中'
      case 'checking':
        return '检查重复...'
      case 'importing':
        return '导入中...'
      case 'verifying':
        return '验活中...'
      case 'verified':
        return result.model ? '验活成功' : '导入成功'
      case 'duplicate':
        return '重复账号'
      case 'failed':
        if (skipVerification) return '导入失败'
        if (result.rollbackStatus === 'success') return '验活失败（已排除）'
        if (result.rollbackStatus === 'failed') return '验活失败（未排除）'
        return '验活失败（未创建）'
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        // 关闭时清空表单（但不在导入过程中清空）
        if (!newOpen && !importing) {
          resetForm()
        }
        onOpenChange(newOpen)
      }}
    >
      <DialogContent className="sm:max-w-2xl max-h-[80vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>批量导入账号（默认查询订阅）</DialogTitle>
        </DialogHeader>

        <div className="flex-1 overflow-y-auto space-y-4 py-4">
          <div className="space-y-2">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <label className="text-sm font-medium">
                JSON / JSONL 格式账号
              </label>
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
              placeholder={'粘贴 JSON / JSONL 格式的账号，或选择一个/多个文件\n\n每个文件可以是单个对象、数组、jsonl 多行，或导出的 { "credentials": [...] } / { "accounts": [...] }\n\nOAuth: [{"refreshToken":"...","clientId":"...","clientSecret":"..."}]\nAPI Key: [{"kiroApiKey":"ksk_xxx"}]\n\n支持 region 字段自动映射为 authRegion'}
              value={jsonInput}
              onChange={(e) => setJsonInput(e.target.value)}
              disabled={importing}
              className="flex min-h-[200px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
            />
            <p className="text-xs text-muted-foreground">
              支持单选或多选文件。
            </p>
          </div>

          <CredentialParameterDefaultsPanel
            defaults={defaults}
            onChange={setDefaults}
            proxyResources={proxyResourceOptions}
            disabled={importing}
            title="导入默认参数"
          />

          <div className="rounded-md border bg-muted/20 p-3">
            <div className="text-sm font-semibold">
              验活方式
            </div>
            <label className="mt-2 flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={skipVerification}
                onChange={(event) => setSkipVerification(event.target.checked)}
                disabled={importing}
                className="rounded border-gray-300"
              />
              跳过验活
            </label>
            {!skipVerification && (
              <div className="mt-2 space-y-2">
                <select
                  id="batchImportVerificationMode"
                  value={verificationMode}
                  onChange={(event) => setVerificationMode(event.target.value as ImportVerificationMode)}
                  disabled={importing}
                  className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <option value="subscription_only">查询订阅/积分</option>
                  <option value="model_and_subscription">测试模型</option>
                </select>
                {verificationMode === 'model_and_subscription' && (
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={refreshInfoAfterModelTest}
                      onChange={(event) => setRefreshInfoAfterModelTest(event.target.checked)}
                      disabled={importing}
                      className="rounded border-gray-300"
                    />
                    同步查询订阅/积分
                  </label>
                )}
              </div>
            )}
          </div>

          <div className="rounded-md border bg-muted/20 p-3">
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={autoDiscoverSupportedModels}
                onChange={(event) => setAutoDiscoverSupportedModels(event.target.checked)}
                disabled={importing}
                className="rounded border-gray-300"
              />
              自动发现模型限制
            </label>
          </div>

          {(importing || results.length > 0) && (
            <>
              {/* 进度条 */}
              <div className="space-y-2">
                <div className="flex justify-between text-sm">
                  <span>{importing ? '验活进度' : '验活完成'}</span>
                  <span>{progress.current} / {progress.total}</span>
                </div>
                <div className="w-full bg-secondary rounded-full h-2">
                  <div
                    className="bg-primary h-2 rounded-full transition-all"
                    style={{ width: `${(progress.current / progress.total) * 100}%` }}
                  />
                </div>
                {importing && currentProcessing && (
                  <div className="text-xs text-muted-foreground">
                    {currentProcessing}
                  </div>
                )}
              </div>

              {/* 统计 */}
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
              </div>

              {/* 结果列表 */}
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
                          <div className="text-xs text-muted-foreground mt-1">
                            模型: {result.model}
                          </div>
                        )}
                        {result.response && (
                          <div className="text-xs text-muted-foreground mt-1 line-clamp-2">
                            响应: {result.response}
                          </div>
                        )}
                        {result.error && (
                          <div className="text-xs text-red-600 dark:text-red-400 mt-1">
                            {result.error}
                          </div>
                        )}
                        {result.rollbackError && (
                          <div className="text-xs text-red-600 dark:text-red-400 mt-1">
                            回滚失败: {result.rollbackError}
                          </div>
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
            onClick={() => {
              onOpenChange(false)
              resetForm()
            }}
            disabled={importing}
          >
            {importing ? '验活中...' : results.length > 0 ? '关闭' : '取消'}
          </Button>
          {results.length > 0 && failedCredentials.length > 0 && (
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
              onClick={() => handleBatchImport()}
              disabled={importing || !jsonInput.trim()}
            >
              开始导入
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
