import { useState } from 'react'
import { toast } from 'sonner'
import { Eye, EyeOff } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useAddCredential, useProxyResources } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import { parseCredentialImportFiles } from '@/lib/credential-import'

interface AddCredentialDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type AuthMethod = 'social' | 'idc' | 'external_idp' | 'api_key'

function SecretInput({
  value,
  onChange,
  visible,
  onToggle,
  disabled,
  placeholder,
}: {
  value: string
  onChange: (value: string) => void
  visible: boolean
  onToggle: () => void
  disabled?: boolean
  placeholder?: string
}) {
  return (
    <div className="relative">
      <Input
        className="pr-10"
        type={visible ? 'text' : 'password'}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="absolute right-1 top-1 h-8 w-8"
        onClick={onToggle}
        disabled={disabled}
        title={visible ? '隐藏' : '显示'}
      >
        {visible ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
      </Button>
    </div>
  )
}

export function AddCredentialDialog({ open, onOpenChange }: AddCredentialDialogProps) {
  const [refreshToken, setRefreshToken] = useState('')
  const [kiroApiKey, setKiroApiKey] = useState('')
  const [authMethod, setAuthMethod] = useState<AuthMethod>('social')
  const [profileArn, setProfileArn] = useState('')
  const [region, setRegion] = useState('')
  const [authRegion, setAuthRegion] = useState('')
  const [apiRegion, setApiRegion] = useState('')
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')
  const [tokenEndpoint, setTokenEndpoint] = useState('')
  const [issuerUrl, setIssuerUrl] = useState('')
  const [scopes, setScopes] = useState('')
  const [email, setEmail] = useState('')
  const [priority, setPriority] = useState('0')
  const [maxConcurrentRequests, setMaxConcurrentRequests] = useState('')
  const [rpm, setRpm] = useState('')
  const [machineId, setMachineId] = useState('')
  const [proxyResourceId, setProxyResourceId] = useState('')
  const [proxyUrl, setProxyUrl] = useState('')
  const [proxyUsername, setProxyUsername] = useState('')
  const [proxyPassword, setProxyPassword] = useState('')
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const [endpoint, setEndpoint] = useState('')

  const { mutate, isPending } = useAddCredential()
  const proxyResources = useProxyResources()
  const proxyResourceOptions = (proxyResources.data?.resources || []).filter(resource => resource.enabled)

  const resetForm = () => {
    setRefreshToken('')
    setKiroApiKey('')
    setAuthMethod('social')
    setProfileArn('')
    setRegion('')
    setAuthRegion('')
    setApiRegion('')
    setClientId('')
    setClientSecret('')
    setTokenEndpoint('')
    setIssuerUrl('')
    setScopes('')
    setEmail('')
    setPriority('0')
    setMaxConcurrentRequests('')
    setRpm('')
    setMachineId('')
    setProxyResourceId('')
    setProxyUrl('')
    setProxyUsername('')
    setProxyPassword('')
    setShowProxyUsername(false)
    setShowProxyPassword(false)
    setEndpoint('')
  }

  const isApiKey = authMethod === 'api_key'

  const fillFromCredential = (credential: {
    authMethod?: AuthMethod
    refreshToken?: string
    kiroApiKey?: string
    profileArn?: string
    region?: string
    authRegion?: string
    apiRegion?: string
    clientId?: string
    clientSecret?: string
    tokenEndpoint?: string
    issuerUrl?: string
    scopes?: string
    email?: string
    priority?: number
    maxConcurrentRequests?: number | null
    rpm?: number | null
    machineId?: string
    proxyUrl?: string
    proxyUsername?: string
    proxyPassword?: string
    proxyResourceId?: number | null
    endpoint?: string
  }) => {
    setAuthMethod(credential.authMethod || (credential.kiroApiKey ? 'api_key' : credential.clientId && credential.clientSecret ? 'idc' : 'social'))
    setRefreshToken(credential.refreshToken || '')
    setKiroApiKey(credential.kiroApiKey || '')
    setProfileArn(credential.profileArn || '')
    setRegion(credential.region || '')
    setAuthRegion(credential.authRegion || '')
    setApiRegion(credential.apiRegion || '')
    setClientId(credential.clientId || '')
    setClientSecret(credential.clientSecret || '')
    setTokenEndpoint(credential.tokenEndpoint || '')
    setIssuerUrl(credential.issuerUrl || '')
    setScopes(credential.scopes || '')
    setEmail(credential.email || '')
    setPriority(String(credential.priority ?? 0))
    setMaxConcurrentRequests(typeof credential.maxConcurrentRequests === 'number' ? String(credential.maxConcurrentRequests) : '')
    setRpm(typeof credential.rpm === 'number' ? String(credential.rpm) : '')
    setMachineId(credential.machineId || '')
    if (credential.proxyResourceId) {
      setProxyResourceId(String(credential.proxyResourceId))
      setProxyUrl('')
      setProxyUsername('')
      setProxyPassword('')
    } else {
      setProxyResourceId('')
      setProxyUrl(credential.proxyUrl || '')
      setProxyUsername(credential.proxyUsername || '')
      setProxyPassword(credential.proxyPassword || '')
    }
    setShowProxyUsername(false)
    setShowProxyPassword(false)
    setEndpoint(credential.endpoint || '')
  }

  const handleAuthMethodChange = (nextAuthMethod: AuthMethod) => {
    setAuthMethod(nextAuthMethod)
    if (nextAuthMethod === 'api_key') {
      setRefreshToken('')
      setClientId('')
      setClientSecret('')
      setTokenEndpoint('')
      setIssuerUrl('')
      setScopes('')
      return
    }
    setKiroApiKey('')
    if (nextAuthMethod === 'social') {
      setClientId('')
      setClientSecret('')
      setTokenEndpoint('')
      setIssuerUrl('')
      setScopes('')
    } else if (nextAuthMethod === 'idc') {
      setTokenEndpoint('')
      setIssuerUrl('')
      setScopes('')
    } else if (nextAuthMethod === 'external_idp') {
      setClientSecret('')
    }
  }

  const handleRegionChange = (value: string) => {
    setRegion(value)
    if (value.trim() && !authRegion.trim()) {
      setAuthRegion(value)
    }
  }

  const handleProxyResourceChange = (value: string) => {
    setProxyResourceId(value)
    if (value) {
      setProxyUrl('')
      setProxyUsername('')
      setProxyPassword('')
    }
  }

  const setDirectProxyDraft = (setter: (value: string) => void, value: string) => {
    setter(value)
    if (value.trim()) {
      setProxyResourceId('')
    }
  }

  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files || [])
    event.target.value = ''
    if (files.length === 0) {
      return
    }

    const result = await parseCredentialImportFiles(files)
    const first = result.credentials[0]
    if (!first) {
      toast.error(result.errors[0] || '文件中没有有效凭据')
      return
    }

    fillFromCredential(first)
    const suffix = result.credentials.length > 1 ? `，已取第一条，另有 ${result.credentials.length - 1} 条可用批量导入` : ''
    toast.success(`已从文件填充凭据${suffix}`)
    if (result.errors.length > 0) {
      toast.warning(`部分文件未读取: ${result.errors.slice(0, 3).join('；')}`)
    }
  }

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    // 验证必填字段
    if (isApiKey) {
      if (!kiroApiKey.trim()) {
        toast.error('请输入 Kiro API Key')
        return
      }
    } else {
      if (!refreshToken.trim()) {
        toast.error('请输入 Refresh Token')
        return
      }
      // IdC/Builder-ID/IAM 需要额外字段
      if (authMethod === 'idc' && (!clientId.trim() || !clientSecret.trim())) {
        toast.error('IdC/Builder-ID/IAM 认证需要填写 Client ID 和 Client Secret')
        return
      }
      if (authMethod === 'external_idp' && !clientId.trim()) {
        toast.error('External IdP 认证需要填写 Client ID')
        return
      }
    }

    const parsedPriority = Number(priority)
    if (!Number.isInteger(parsedPriority) || parsedPriority < 0) {
      toast.error('优先级必须是非负整数')
      return
    }
    let parsedMaxConcurrentRequests: number | undefined
    if (maxConcurrentRequests.trim()) {
      const parsed = Number(maxConcurrentRequests)
      if (!Number.isInteger(parsed) || parsed < 0) {
        toast.error('账号并发覆盖必须是非负整数')
        return
      }
      parsedMaxConcurrentRequests = parsed
    }
    let parsedRpm: number | undefined
    if (rpm.trim()) {
      const parsed = Number(rpm)
      if (!Number.isInteger(parsed) || parsed < 0) {
        toast.error('账号 RPM 覆盖必须是非负整数')
        return
      }
      parsedRpm = parsed
    }
    const directProxyUrl = proxyUrl.trim()
    const directProxyUsername = proxyUsername.trim()
    const directProxyPassword = proxyPassword.trim()
    if (!proxyResourceId && !directProxyUrl && (directProxyUsername || directProxyPassword)) {
      toast.error('直接代理 URL 为空时不能单独保存代理账号或密码')
      return
    }

    mutate(
      {
        authMethod,
        refreshToken: isApiKey ? undefined : refreshToken.trim(),
        kiroApiKey: isApiKey ? kiroApiKey.trim() : undefined,
        profileArn: profileArn.trim() || undefined,
        region: region.trim() || undefined,
        authRegion: authRegion.trim() || undefined,
        apiRegion: apiRegion.trim() || undefined,
        clientId: isApiKey ? undefined : clientId.trim() || undefined,
        clientSecret: authMethod === 'idc' ? clientSecret.trim() || undefined : undefined,
        tokenEndpoint: authMethod === 'external_idp' ? tokenEndpoint.trim() || undefined : undefined,
        issuerUrl: authMethod === 'external_idp' ? issuerUrl.trim() || undefined : undefined,
        scopes: authMethod === 'external_idp' ? scopes.trim() || undefined : undefined,
        email: email.trim() || undefined,
        priority: parsedPriority,
        maxConcurrentRequests: parsedMaxConcurrentRequests,
        rpm: parsedRpm,
        machineId: machineId.trim() || undefined,
        proxyResourceId: proxyResourceId ? Number(proxyResourceId) : undefined,
        proxyUrl: proxyResourceId ? undefined : directProxyUrl || undefined,
        proxyUsername: proxyResourceId ? undefined : directProxyUsername || undefined,
        proxyPassword: proxyResourceId ? undefined : directProxyPassword || undefined,
        endpoint: endpoint.trim() || undefined,
      },
      {
        onSuccess: (data) => {
          toast.success(data.message)
          onOpenChange(false)
          resetForm()
        },
        onError: (error: unknown) => {
          toast.error(`添加失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg max-h-[85vh] flex flex-col">
        <DialogHeader>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <DialogTitle>添加凭据</DialogTitle>
            <Button type="button" variant="outline" size="sm" disabled={isPending} asChild>
              <label className="cursor-pointer">
                从文件填充
                <input
                  type="file"
                  accept=".json,.jsonl,.txt,application/json"
                  className="hidden"
                  onChange={handleFileSelect}
                  disabled={isPending}
                />
              </label>
            </Button>
          </div>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="flex flex-col min-h-0 flex-1">
          <div className="space-y-4 py-4 overflow-y-auto flex-1 pr-1">
            {/* 认证方式 */}
            <div className="space-y-2">
              <label htmlFor="authMethod" className="text-sm font-medium">
                认证方式
              </label>
              <select
                id="authMethod"
                value={authMethod}
                onChange={(e) => handleAuthMethodChange(e.target.value as AuthMethod)}
                disabled={isPending}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="social">Social</option>
                <option value="idc">IdC/Builder-ID/IAM</option>
                <option value="external_idp">External IdP</option>
                <option value="api_key">API Key</option>
              </select>
            </div>

            {/* Kiro API Key (API Key 模式) */}
            {isApiKey && (
              <div className="space-y-2">
                <label htmlFor="kiroApiKey" className="text-sm font-medium">
                  Kiro API Key <span className="text-red-500">*</span>
                </label>
                <Input
                  id="kiroApiKey"
                  type="password"
                  placeholder="格式: ksk_xxxxxxxx"
                  value={kiroApiKey}
                  onChange={(e) => setKiroApiKey(e.target.value)}
                  disabled={isPending}
                />
              </div>
            )}

            {/* Refresh Token (OAuth 模式) */}
            {!isApiKey && (
              <div className="space-y-2">
                <label htmlFor="refreshToken" className="text-sm font-medium">
                  Refresh Token <span className="text-red-500">*</span>
                </label>
                <Input
                  id="refreshToken"
                  type="password"
                  placeholder="请输入 Refresh Token"
                  value={refreshToken}
                  onChange={(e) => setRefreshToken(e.target.value)}
                  disabled={isPending}
                />
              </div>
            )}

            {/* 账号邮箱 */}
            <div className="space-y-2">
              <label htmlFor="email" className="text-sm font-medium">
                账号邮箱
              </label>
              <Input
                id="email"
                type="email"
                placeholder="可选，用于管理页识别账号"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                disabled={isPending}
              />
            </div>

            {/* Region 配置 */}
            <div className="space-y-2">
              <label className="text-sm font-medium">Region 配置</label>
              <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
                <div>
                  <Input
                    id="region"
                    placeholder="Region 兼容字段"
                    value={region}
                    onChange={(e) => handleRegionChange(e.target.value)}
                    disabled={isPending}
                    className="font-mono"
                  />
                </div>
                <div>
                  <Input
                    id="authRegion"
                    placeholder="Auth Region"
                    value={authRegion}
                    onChange={(e) => setAuthRegion(e.target.value)}
                    disabled={isPending}
                    className="font-mono"
                  />
                </div>
                <div>
                  <Input
                    id="apiRegion"
                    placeholder="API Region"
                    value={apiRegion}
                    onChange={(e) => setApiRegion(e.target.value)}
                    disabled={isPending}
                    className="font-mono"
                  />
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                `us-east-1` 这类值是 AWS 区域。Region 是兼容字段；Auth Region 用于 Token 刷新，API Region 用于 API 请求，均可留空使用全局配置。
              </p>
            </div>

            {/* IdC/Builder-ID/IAM 额外字段 */}
            {authMethod === 'idc' && (
              <>
                <div className="space-y-2">
                  <label htmlFor="clientId" className="text-sm font-medium">
                    Client ID <span className="text-red-500">*</span>
                  </label>
                  <Input
                    id="clientId"
                    placeholder="请输入 Client ID"
                    value={clientId}
                    onChange={(e) => setClientId(e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <div className="space-y-2">
                  <label htmlFor="clientSecret" className="text-sm font-medium">
                    Client Secret <span className="text-red-500">*</span>
                  </label>
                  <Input
                    id="clientSecret"
                    type="password"
                    placeholder="请输入 Client Secret"
                    value={clientSecret}
                    onChange={(e) => setClientSecret(e.target.value)}
                    disabled={isPending}
                  />
                </div>
              </>
            )}

            {authMethod === 'external_idp' && (
              <>
                <div className="space-y-2">
                  <label htmlFor="externalClientId" className="text-sm font-medium">
                    Client ID <span className="text-red-500">*</span>
                  </label>
                  <Input
                    id="externalClientId"
                    placeholder="请输入 Client ID"
                    value={clientId}
                    onChange={(e) => setClientId(e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <div className="space-y-2">
                  <label htmlFor="tokenEndpoint" className="text-sm font-medium">
                    Token Endpoint
                  </label>
                  <Input
                    id="tokenEndpoint"
                    placeholder="https://.../oauth2/v2.0/token"
                    value={tokenEndpoint}
                    onChange={(e) => setTokenEndpoint(e.target.value)}
                    disabled={isPending}
                    className="font-mono"
                  />
                </div>
                <div className="space-y-2">
                  <label htmlFor="issuerUrl" className="text-sm font-medium">
                    Issuer URL
                  </label>
                  <Input
                    id="issuerUrl"
                    placeholder="https://..."
                    value={issuerUrl}
                    onChange={(e) => setIssuerUrl(e.target.value)}
                    disabled={isPending}
                    className="font-mono"
                  />
                </div>
                <div className="space-y-2">
                  <label htmlFor="scopes" className="text-sm font-medium">
                    Scopes
                  </label>
                  <Input
                    id="scopes"
                    placeholder="offline_access ..."
                    value={scopes}
                    onChange={(e) => setScopes(e.target.value)}
                    disabled={isPending}
                    className="font-mono"
                  />
                </div>
              </>
            )}

            {/* 优先级 */}
            <div className="space-y-2">
              <label htmlFor="priority" className="text-sm font-medium">
                优先级
              </label>
              <Input
                id="priority"
                type="number"
                min="0"
                placeholder="数字越小优先级越高"
                value={priority}
                onChange={(e) => setPriority(e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                数字越小优先级越高，默认为 0
              </p>
            </div>

            <div className="space-y-2">
              <label htmlFor="maxConcurrentRequests" className="text-sm font-medium">
                账号并发覆盖
              </label>
              <Input
                id="maxConcurrentRequests"
                type="number"
                min="0"
                placeholder="留空继承全局，0 表示不限"
                value={maxConcurrentRequests}
                onChange={(e) => setMaxConcurrentRequests(e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                只作用于当前凭据；留空时继承全局单凭据并发配置。
              </p>
            </div>

            <div className="space-y-2">
              <label htmlFor="rpm" className="text-sm font-medium">
                账号 RPM 覆盖
              </label>
              <Input
                id="rpm"
                type="number"
                min="0"
                placeholder="留空继承全局，0 表示不限"
                value={rpm}
                onChange={(e) => setRpm(e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                限制当前账号每分钟被分配的请求数。
              </p>
            </div>

            {/* Machine ID */}
            <div className="space-y-2">
              <label htmlFor="machineId" className="text-sm font-medium">
                Machine ID
              </label>
              <Input
                id="machineId"
                placeholder="留空使用配置中字段, 否则由刷新Token自动派生"
                value={machineId}
                onChange={(e) => setMachineId(e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                可选，64 位十六进制字符串，留空使用配置中字段, 否则由刷新Token自动派生
              </p>
            </div>

            {/* 端点 */}
            <div className="space-y-2">
              <label htmlFor="endpoint" className="text-sm font-medium">
                端点
              </label>
              <Input
                id="endpoint"
                placeholder="留空使用默认端点（如 ide / cli）"
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                可选。决定该凭据走哪套 Kiro API。留空使用全局 defaultEndpoint
              </p>
            </div>

            {/* 代理配置 */}
            <div className="space-y-2">
              <label className="text-sm font-medium">代理资源</label>
              <select
                id="proxyResourceId"
                value={proxyResourceId}
                onChange={(e) => handleProxyResourceChange(e.target.value)}
                disabled={isPending}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="">不绑定代理资源</option>
                {proxyResourceOptions.map((resource) => (
                  <option key={resource.id} value={resource.id}>
                    {resource.name}
                  </option>
                ))}
              </select>
              <p className="text-xs text-muted-foreground">
                新增凭据会立即验证 Token，只能选择已启用的代理资源；选择资源会清空直连代理，填写直连代理会自动取消资源。
              </p>
            </div>

            <div className={`space-y-3 rounded-md border p-3 ${proxyResourceId ? 'bg-muted/30 opacity-70' : 'bg-background'}`}>
              <div>
                <div className="text-sm font-medium">凭据直连代理</div>
                <p className="mt-1 text-xs leading-5 text-muted-foreground">
                  不绑定代理资源时生效；选择代理资源时这些字段不会随新增请求提交。
                </p>
              </div>
              <div className="space-y-2">
                <label htmlFor="proxyUrl" className="text-sm font-medium">
                  代理 URL
                </label>
                <Input
                  id="proxyUrl"
                  placeholder="socks5h://127.0.0.1:1080"
                  value={proxyUrl}
                  onChange={(event) => setDirectProxyDraft(setProxyUrl, event.target.value)}
                  disabled={isPending || Boolean(proxyResourceId)}
                />
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-2">
                  <label className="text-sm font-medium">代理用户名</label>
                  <SecretInput
                    value={proxyUsername}
                    onChange={(value) => setDirectProxyDraft(setProxyUsername, value)}
                    visible={showProxyUsername}
                    onToggle={() => setShowProxyUsername((value) => !value)}
                    disabled={isPending || Boolean(proxyResourceId)}
                    placeholder="可选"
                  />
                </div>
                <div className="space-y-2">
                  <label className="text-sm font-medium">代理密码</label>
                  <SecretInput
                    value={proxyPassword}
                    onChange={(value) => setDirectProxyDraft(setProxyPassword, value)}
                    visible={showProxyPassword}
                    onToggle={() => setShowProxyPassword((value) => !value)}
                    disabled={isPending || Boolean(proxyResourceId)}
                    placeholder="可选"
                  />
                </div>
              </div>
            </div>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={isPending}
            >
              取消
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending ? '添加中...' : '添加'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
