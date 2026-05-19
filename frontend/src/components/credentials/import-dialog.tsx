import { useState } from 'react'
import { useDropzone } from 'react-dropzone'
import { toast } from 'sonner'
import {
  AlertCircle,
  CheckCircle2,
  FileJson,
  Loader2,
  UploadCloud,
  XCircle,
} from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/components/ui/tabs'
import { Textarea } from '@/components/ui/textarea'
import { Progress } from '@/components/ui/progress'
import {
  useAddCredential,
  useCredentialsList,
  useDeleteCredential,
} from '@/hooks/use-credentials'
import {
  getCredentialBalance,
  setCredentialDisabled,
} from '@/api/admin'
import { extractErrorMessage, sha256Hex } from '@/lib/utils'

interface ImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

interface CredentialInput {
  refreshToken?: string
  clientId?: string
  clientSecret?: string
  email?: string
  region?: string
  authRegion?: string
  apiRegion?: string
  priority?: number
  machineId?: string
  kiroApiKey?: string
  authMethod?: string
  endpoint?: string
}

interface VerificationResult {
  index: number
  status:
    | 'pending'
    | 'checking'
    | 'verifying'
    | 'verified'
    | 'duplicate'
    | 'failed'
  email?: string
  usage?: string
  error?: string
  credentialId?: number
  rollbackStatus?: 'success' | 'failed' | 'skipped'
}

function getStatusIcon(status: VerificationResult['status']) {
  switch (status) {
    case 'pending':
      return <div className="h-4 w-4 rounded-full border-2 border-muted" />
    case 'checking':
    case 'verifying':
      return <Loader2 className="h-4 w-4 animate-spin text-info" />
    case 'verified':
      return <CheckCircle2 className="h-4 w-4 text-success" />
    case 'duplicate':
      return <AlertCircle className="h-4 w-4 text-warning" />
    case 'failed':
      return <XCircle className="h-4 w-4 text-destructive" />
  }
}

function statusText(r: VerificationResult): string {
  switch (r.status) {
    case 'pending':
      return '等待中'
    case 'checking':
      return '检查重复...'
    case 'verifying':
      return '验证中...'
    case 'verified':
      return `验证成功${r.usage ? ' · ' + r.usage : ''}`
    case 'duplicate':
      return '重复凭据'
    case 'failed':
      if (r.rollbackStatus === 'success') return '验证失败(已回滚)'
      if (r.rollbackStatus === 'failed') return '验证失败(回滚失败)'
      return '验证失败'
  }
}

export function ImportCredentialsDialog({ open, onOpenChange }: ImportDialogProps) {
  const [tab, setTab] = useState<'paste' | 'file'>('paste')
  const [jsonInput, setJsonInput] = useState('')
  const [importing, setImporting] = useState(false)
  const [progress, setProgress] = useState({ current: 0, total: 0 })
  const [results, setResults] = useState<VerificationResult[]>([])

  const { data: existing } = useCredentialsList(open)
  const addMutation = useAddCredential()
  const deleteMutation = useDeleteCredential()

  const onDrop = (files: File[]) => {
    const file = files[0]
    if (!file) return
    if (file.size > 5 * 1024 * 1024) {
      toast.error('文件不能超过 5MB')
      return
    }
    const reader = new FileReader()
    reader.onload = () => {
      const content = String(reader.result ?? '')
      setJsonInput(content)
      setTab('paste')
      toast.success(`已加载文件 ${file.name}`)
    }
    reader.onerror = () => toast.error('读取文件失败')
    reader.readAsText(file)
  }

  const dropzone = useDropzone({
    onDrop,
    accept: { 'application/json': ['.json'] },
    multiple: false,
  })

  const reset = () => {
    setJsonInput('')
    setProgress({ current: 0, total: 0 })
    setResults([])
  }

  const rollback = async (id: number): Promise<VerificationResult['rollbackStatus']> => {
    try {
      await setCredentialDisabled(id, true)
      await deleteMutation.mutateAsync(id)
      return 'success'
    } catch (err) {
      console.warn('rollback failed', err)
      return 'failed'
    }
  }

  const handleImport = async () => {
    let creds: CredentialInput[]
    try {
      const parsed = JSON.parse(jsonInput)
      creds = Array.isArray(parsed) ? parsed : [parsed]
    } catch (err) {
      toast.error('JSON 格式错误: ' + extractErrorMessage(err))
      return
    }
    if (creds.length === 0) {
      toast.error('没有可导入的凭据')
      return
    }

    setImporting(true)
    setProgress({ current: 0, total: creds.length })
    setResults(creds.map((_, i) => ({ index: i + 1, status: 'pending' })))

    const oauthHashes = new Set(
      existing?.credentials
        .map((c) => c.refreshTokenHash)
        .filter((x): x is string => Boolean(x)) ?? [],
    )
    const apiKeyHashes = new Set(
      existing?.credentials
        .map((c) => c.apiKeyHash)
        .filter((x): x is string => Boolean(x)) ?? [],
    )

    let success = 0
    let duplicates = 0
    let failed = 0

    for (let i = 0; i < creds.length; i++) {
      const c = creds[i]
      const isApiKey = !!c.kiroApiKey?.trim() || c.authMethod === 'api_key'

      setResults((prev) => {
        const next = [...prev]
        next[i] = { ...next[i], status: 'checking' }
        return next
      })

      const idHash = isApiKey
        ? c.kiroApiKey?.trim()
          ? await sha256Hex(c.kiroApiKey.trim())
          : null
        : c.refreshToken?.trim()
          ? await sha256Hex(c.refreshToken.trim())
          : null
      if (!idHash) {
        failed++
        setResults((prev) => {
          const next = [...prev]
          next[i] = {
            ...next[i],
            status: 'failed',
            error: isApiKey ? '缺少 kiroApiKey' : '缺少 refreshToken',
          }
          return next
        })
        setProgress({ current: i + 1, total: creds.length })
        continue
      }
      if (isApiKey ? apiKeyHashes.has(idHash) : oauthHashes.has(idHash)) {
        duplicates++
        const existingCred = existing?.credentials.find((x) =>
          isApiKey ? x.apiKeyHash === idHash : x.refreshTokenHash === idHash,
        )
        setResults((prev) => {
          const next = [...prev]
          next[i] = {
            ...next[i],
            status: 'duplicate',
            email: existingCred?.email,
          }
          return next
        })
        setProgress({ current: i + 1, total: creds.length })
        continue
      }

      setResults((prev) => {
        const next = [...prev]
        next[i] = { ...next[i], status: 'verifying' }
        return next
      })

      let createdId: number | null = null
      try {
        if (isApiKey) {
          const r = await addMutation.mutateAsync({
            authMethod: 'api_key',
            kiroApiKey: c.kiroApiKey?.trim(),
            email: c.email?.trim() || undefined,
            priority: c.priority ?? 0,
            authRegion: c.authRegion?.trim() || c.region?.trim() || undefined,
            apiRegion: c.apiRegion?.trim() || undefined,
            machineId: c.machineId?.trim() || undefined,
            endpoint: c.endpoint?.trim() || undefined,
          })
          createdId = r.credentialId
          await new Promise((res) => setTimeout(res, 800))
          const balance = await getCredentialBalance(r.credentialId)
          success++
          apiKeyHashes.add(idHash)
          setResults((prev) => {
            const next = [...prev]
            next[i] = {
              ...next[i],
              status: 'verified',
              email: r.email ?? c.email,
              usage: `${balance.currentUsage.toFixed(1)}/${balance.usageLimit.toFixed(1)}`,
              credentialId: r.credentialId,
            }
            return next
          })
        } else {
          const clientId = c.clientId?.trim() || undefined
          const clientSecret = c.clientSecret?.trim() || undefined
          const authMethod = clientId && clientSecret ? 'idc' : 'social'
          if (authMethod === 'social' && (clientId || clientSecret)) {
            throw new Error('idc 模式需要 clientId 与 clientSecret 同时存在')
          }
          const r = await addMutation.mutateAsync({
            refreshToken: c.refreshToken!.trim(),
            authMethod,
            clientId,
            clientSecret,
            email: c.email?.trim() || undefined,
            authRegion: c.authRegion?.trim() || c.region?.trim() || undefined,
            apiRegion: c.apiRegion?.trim() || undefined,
            priority: c.priority ?? 0,
            machineId: c.machineId?.trim() || undefined,
            endpoint: c.endpoint?.trim() || undefined,
          })
          createdId = r.credentialId
          await new Promise((res) => setTimeout(res, 800))
          const balance = await getCredentialBalance(r.credentialId)
          success++
          oauthHashes.add(idHash)
          setResults((prev) => {
            const next = [...prev]
            next[i] = {
              ...next[i],
              status: 'verified',
              email: r.email ?? c.email,
              usage: `${balance.currentUsage.toFixed(1)}/${balance.usageLimit.toFixed(1)}`,
              credentialId: r.credentialId,
            }
            return next
          })
        }
      } catch (err) {
        failed++
        let rollbackStatus: VerificationResult['rollbackStatus'] = 'skipped'
        if (createdId !== null) {
          rollbackStatus = await rollback(createdId)
        }
        setResults((prev) => {
          const next = [...prev]
          next[i] = {
            ...next[i],
            status: 'failed',
            error: extractErrorMessage(err),
            rollbackStatus,
          }
          return next
        })
      }
      setProgress({ current: i + 1, total: creds.length })
    }

    setImporting(false)
    if (failed === 0 && duplicates === 0) {
      toast.success(`成功导入 ${success} 个凭据`)
    } else {
      toast.info(
        `导入完成:成功 ${success},重复 ${duplicates},失败 ${failed}`,
      )
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !importing) reset()
        onOpenChange(next)
      }}
    >
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>批量导入凭据</DialogTitle>
          <DialogDescription>
            支持 JSON 文本粘贴或拖拽上传 .json 文件,导入时自动验证并去重。
          </DialogDescription>
        </DialogHeader>

        <Tabs value={tab} onValueChange={(v) => setTab(v as 'paste' | 'file')}>
          <TabsList>
            <TabsTrigger value="paste">粘贴 JSON</TabsTrigger>
            <TabsTrigger value="file">上传文件</TabsTrigger>
          </TabsList>

          <TabsContent value="paste" className="space-y-2">
            <Textarea
              value={jsonInput}
              onChange={(e) => setJsonInput(e.target.value)}
              placeholder={`OAuth 示例:\n[{"refreshToken":"..."}]\n\nAPI Key 示例:\n[{"kiroApiKey":"ksk_..."}]`}
              className="min-h-[200px] font-mono text-xs"
              disabled={importing}
            />
            <div className="flex items-center justify-between text-xs text-muted-foreground">
              <span>导入时会自动验活,失败会被自动回滚</span>
              <span>{jsonInput.length} 字符</span>
            </div>
          </TabsContent>

          <TabsContent value="file" className="space-y-2">
            <div
              {...dropzone.getRootProps()}
              className={`flex h-40 cursor-pointer flex-col items-center justify-center gap-2 rounded-lg border-2 border-dashed transition-colors ${
                dropzone.isDragActive
                  ? 'border-primary bg-primary/5'
                  : 'border-border hover:bg-muted/40'
              }`}
            >
              <input {...dropzone.getInputProps()} />
              <UploadCloud className="h-6 w-6 text-muted-foreground" />
              <div className="text-sm font-medium">
                {dropzone.isDragActive
                  ? '松开以读取文件'
                  : '点击或拖拽 .json 文件到此处'}
              </div>
              <div className="text-xs text-muted-foreground">
                <FileJson className="mr-1 inline h-3 w-3" />
                单个文件,最大 5 MB
              </div>
            </div>
          </TabsContent>
        </Tabs>

        {(importing || results.length > 0) && (
          <div className="space-y-2">
            <div className="flex items-center justify-between text-xs">
              <span>{importing ? '验证中' : '已完成'}</span>
              <span>
                {progress.current} / {progress.total}
              </span>
            </div>
            <Progress
              value={
                progress.total === 0
                  ? 0
                  : (progress.current / progress.total) * 100
              }
            />
            <div className="max-h-[260px] divide-y overflow-y-auto rounded-md border">
              {results.map((r) => (
                <div key={r.index} className="flex items-start gap-2 p-2 text-xs">
                  {getStatusIcon(r.status)}
                  <div className="flex-1 space-y-0.5">
                    <div className="font-medium">
                      {r.email ?? `凭据 #${r.index}`}
                      <span className="ml-2 text-muted-foreground">
                        {statusText(r)}
                      </span>
                    </div>
                    {r.error && <div className="text-destructive">{r.error}</div>}
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => {
              if (!importing) {
                reset()
                onOpenChange(false)
              }
            }}
            disabled={importing}
          >
            {results.length > 0 ? '关闭' : '取消'}
          </Button>
          {results.length === 0 && (
            <Button
              onClick={handleImport}
              disabled={importing || !jsonInput.trim()}
            >
              {importing && <Loader2 className="h-4 w-4 animate-spin" />}
              开始导入并验活
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
