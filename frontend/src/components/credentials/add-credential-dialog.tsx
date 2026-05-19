import { useState, type FormEvent } from 'react'
import { toast } from 'sonner'
import { Loader2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useAddCredential } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

interface AddCredentialDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type AuthMethod = 'social' | 'idc' | 'api_key'

export function AddCredentialDialog({ open, onOpenChange }: AddCredentialDialogProps) {
  const [authMethod, setAuthMethod] = useState<AuthMethod>('social')
  const [refreshToken, setRefreshToken] = useState('')
  const [kiroApiKey, setKiroApiKey] = useState('')
  const [email, setEmail] = useState('')
  const [priority, setPriority] = useState('0')
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')
  const [authRegion, setAuthRegion] = useState('')
  const [apiRegion, setApiRegion] = useState('')
  const [machineId, setMachineId] = useState('')
  const [endpoint, setEndpoint] = useState('')

  const { mutate, isPending } = useAddCredential()

  const reset = () => {
    setAuthMethod('social')
    setRefreshToken('')
    setKiroApiKey('')
    setEmail('')
    setPriority('0')
    setClientId('')
    setClientSecret('')
    setAuthRegion('')
    setApiRegion('')
    setMachineId('')
    setEndpoint('')
  }

  const isApiKey = authMethod === 'api_key'

  const onSubmit = (e: FormEvent) => {
    e.preventDefault()
    if (isApiKey && !kiroApiKey.trim()) {
      toast.error('请输入 Kiro API Key')
      return
    }
    if (!isApiKey && !refreshToken.trim()) {
      toast.error('请输入 Refresh Token')
      return
    }
    if (authMethod === 'idc' && (!clientId.trim() || !clientSecret.trim())) {
      toast.error('IdC 认证需要同时填写 Client ID 与 Client Secret')
      return
    }
    const parsed = Number(priority)
    if (!Number.isInteger(parsed) || parsed < 0) {
      toast.error('优先级必须是非负整数')
      return
    }

    mutate(
      {
        authMethod,
        refreshToken: isApiKey ? undefined : refreshToken.trim(),
        kiroApiKey: isApiKey ? kiroApiKey.trim() : undefined,
        clientId: isApiKey ? undefined : clientId.trim() || undefined,
        clientSecret: isApiKey ? undefined : clientSecret.trim() || undefined,
        email: email.trim() || undefined,
        priority: parsed,
        authRegion: authRegion.trim() || undefined,
        apiRegion: apiRegion.trim() || undefined,
        machineId: machineId.trim() || undefined,
        endpoint: endpoint.trim() || undefined,
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          reset()
          onOpenChange(false)
        },
        onError: (err) => toast.error(extractErrorMessage(err)),
      },
    )
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !isPending) reset()
        onOpenChange(next)
      }}
    >
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>添加凭据</DialogTitle>
          <DialogDescription>
            支持 Social OAuth、IdC OAuth、API Key 三种认证方式。
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={onSubmit} className="space-y-3">
          <Tabs value={authMethod} onValueChange={(v) => setAuthMethod(v as AuthMethod)}>
            <TabsList>
              <TabsTrigger value="social">Social OAuth</TabsTrigger>
              <TabsTrigger value="idc">IdC OAuth</TabsTrigger>
              <TabsTrigger value="api_key">API Key</TabsTrigger>
            </TabsList>
            <TabsContent value="social" className="space-y-3">
              <div className="space-y-1.5">
                <Label htmlFor="rt">Refresh Token</Label>
                <Input
                  id="rt"
                  placeholder="aor..."
                  value={refreshToken}
                  onChange={(e) => setRefreshToken(e.target.value)}
                />
              </div>
            </TabsContent>
            <TabsContent value="idc" className="space-y-3">
              <div className="space-y-1.5">
                <Label htmlFor="rt-idc">Refresh Token</Label>
                <Input
                  id="rt-idc"
                  placeholder="aor..."
                  value={refreshToken}
                  onChange={(e) => setRefreshToken(e.target.value)}
                />
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div className="space-y-1.5">
                  <Label htmlFor="client-id">Client ID</Label>
                  <Input
                    id="client-id"
                    value={clientId}
                    onChange={(e) => setClientId(e.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="client-secret">Client Secret</Label>
                  <Input
                    id="client-secret"
                    value={clientSecret}
                    onChange={(e) => setClientSecret(e.target.value)}
                  />
                </div>
              </div>
            </TabsContent>
            <TabsContent value="api_key" className="space-y-3">
              <div className="space-y-1.5">
                <Label htmlFor="kiro-api-key">Kiro API Key</Label>
                <Input
                  id="kiro-api-key"
                  type="password"
                  placeholder="ksk_..."
                  value={kiroApiKey}
                  onChange={(e) => setKiroApiKey(e.target.value)}
                />
              </div>
            </TabsContent>
          </Tabs>

          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="email">邮箱(可选,便于识别)</Label>
              <Input
                id="email"
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="priority">优先级</Label>
              <Input
                id="priority"
                type="number"
                min="0"
                value={priority}
                onChange={(e) => setPriority(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="auth-region">Auth Region</Label>
              <Input
                id="auth-region"
                placeholder="us-east-1"
                value={authRegion}
                onChange={(e) => setAuthRegion(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="api-region">API Region</Label>
              <Input
                id="api-region"
                placeholder="us-east-1"
                value={apiRegion}
                onChange={(e) => setApiRegion(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="machine-id">Machine ID(可选)</Label>
              <Input
                id="machine-id"
                value={machineId}
                onChange={(e) => setMachineId(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="endpoint">Endpoint</Label>
              <Select value={endpoint || 'ide'} onValueChange={setEndpoint}>
                <SelectTrigger id="endpoint">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="ide">ide</SelectItem>
                </SelectContent>
              </Select>
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
              {isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              添加
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
