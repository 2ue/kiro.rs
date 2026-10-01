import { useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Eye, EyeOff, KeyRound, MoreHorizontal, Pencil, Plus, Trash2 } from 'lucide-react'
import { systemApi } from '@/api/endpoints/system'
import type { RequestAdmissionConfig, RequestApiKeyItem } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { CopyButton } from '@/components/patterns/copy-button'
import { ErrorState, LoadingRows } from '@/components/patterns/data-state'
import { NumberInput } from '@/components/patterns/fields'
import { Page, PageHeader, Section } from '@/components/patterns/page-header'
import { useConfirm } from '@/components/patterns/confirm'
import { ToneBadge } from '@/components/status/tone-badge'
import { authStorage } from '@/lib/auth-storage'
import { qk } from '@/queries/keys'

function randomKey(prefix: string): string {
  const bytes = crypto.getRandomValues(new Uint8Array(24))
  return prefix + Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

function SecretValue({ value, masked }: { value: string; masked: string }) {
  const [show, setShow] = useState(false)
  return (
    <span className="inline-flex min-w-0 items-center gap-1">
      <span className="truncate font-mono text-xs">{show ? value : masked}</span>
      <Button variant="ghost" size="icon-xs" aria-label={show ? '隐藏' : '显示'} onClick={() => setShow((s) => !s)}>
        {show ? <EyeOff /> : <Eye />}
      </Button>
      <CopyButton value={value} label="复制 Key" />
    </span>
  )
}

export function AccessPage() {
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const keys = useQuery({ queryKey: qk.system.accessKeys, queryFn: systemApi.accessKeys })
  const [editing, setEditing] = useState<RequestApiKeyItem | 'new' | null>(null)
  const [rotateOpen, setRotateOpen] = useState(false)
  const setData = (data: Awaited<ReturnType<typeof systemApi.accessKeys>>) => queryClient.setQueryData(qk.system.accessKeys, data)

  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) => systemApi.updateRequestKey(id, { enabled }),
    onSuccess: setData,
    meta: { error: '更新失败' },
  })
  const remove = useMutation({
    mutationFn: (id: string) => systemApi.deleteRequestKey(id),
    onSuccess: setData,
    meta: { success: 'Key 已删除', error: '删除失败' },
  })

  if (keys.isLoading)
    return (
      <Page>
        <LoadingRows />
      </Page>
    )
  if (keys.error || !keys.data)
    return (
      <Page>
        <ErrorState error={keys.error} onRetry={() => keys.refetch()} />
      </Page>
    )
  const d = keys.data
  const origin = window.location.origin

  return (
    <Page>
      <PageHeader title="访问控制" description="管理后台 Admin Key 与下游调用使用的请求 API Key" />

      <Section
        title="请求 API Key"
        description="下游客户端调用 /v1/messages 等接口时使用；每个 Key 可单独设置准入限制"
        contentClassName="p-0"
        actions={
          <Button size="sm" onClick={() => setEditing('new')}>
            <Plus /> 新建 Key
          </Button>
        }
      >
        <ul className="divide-y">
          {d.requestApiKeys.map((k) => (
            <li key={k.id} className="flex flex-wrap items-center gap-3 px-4 py-3">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 text-sm font-medium">
                  {k.name || '未命名'}
                  {k.primary && <ToneBadge tone="primary">主 Key</ToneBadge>}
                  {!k.enabled && <ToneBadge>已停用</ToneBadge>}
                  <span className="font-mono text-xs text-muted-foreground">{k.id}</span>
                </div>
                <SecretValue value={k.apiKey} masked={k.maskedApiKey} />
              </div>
              <span className="text-xs text-muted-foreground">
                {k.requestAdmission
                  ? `RPM ${k.requestAdmission.rpm || '不限'} · 并发 ${k.requestAdmission.maxConcurrentRequests || '不限'}`
                  : '使用默认准入'}
              </span>
              <Button variant="link" size="xs" asChild>
                <Link to="/requests" search={{ keyId: k.id }}>
                  查看请求
                </Link>
              </Button>
              <Switch
                checked={k.enabled}
                aria-label={`启用 ${k.name}`}
                onCheckedChange={async (enabled) => {
                  if (
                    !enabled &&
                    !(await confirm({
                      title: `停用 Key「${k.name || k.id}」？`,
                      description: '使用该 Key 的客户端会立即收到 401。',
                      destructive: true,
                      confirmText: '停用',
                    }))
                  )
                    return
                  toggle.mutate({ id: k.id, enabled })
                }}
              />
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="ghost" size="icon-xs" aria-label="更多操作">
                    <MoreHorizontal />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem onSelect={() => setEditing(k)}>
                    <Pencil /> 编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    variant="destructive"
                    disabled={d.requestApiKeys.length <= 1}
                    onSelect={async () => {
                      if (
                        await confirm({
                          title: `删除 Key「${k.name || k.id}」？`,
                          description: '删除后使用该 Key 的客户端立即失效，不可恢复。',
                          destructive: true,
                          confirmText: '删除',
                        })
                      )
                        remove.mutate(k.id)
                    }}
                  >
                    <Trash2 /> 删除
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </li>
          ))}
        </ul>
      </Section>

      <Section
        title="Admin Key"
        description="登录本控制台与访问 /api/admin 使用；轮换后旧 Key 立即失效"
        actions={
          <Button size="sm" variant="outline" onClick={() => setRotateOpen(true)}>
            <KeyRound /> 轮换
          </Button>
        }
      >
        <SecretValue value={d.adminApiKey} masked={d.maskedAdminApiKey} />
      </Section>

      <Section title="接入示例" description="将 YOUR_API_KEY 替换为上面的请求 API Key">
        <Tabs defaultValue="cc">
          <TabsList>
            <TabsTrigger value="cc">Claude Code</TabsTrigger>
            <TabsTrigger value="curl">curl</TabsTrigger>
          </TabsList>
          <TabsContent value="cc">
            <Snippet code={`export ANTHROPIC_BASE_URL="${origin}"\nexport ANTHROPIC_AUTH_TOKEN="YOUR_API_KEY"\nclaude`} />
          </TabsContent>
          <TabsContent value="curl">
            <Snippet
              code={`curl ${origin}/v1/messages \\\n  -H "x-api-key: YOUR_API_KEY" \\\n  -H "anthropic-version: 2023-06-01" \\\n  -H "content-type: application/json" \\\n  -d '{"model":"claude-sonnet-4-5","max_tokens":256,"messages":[{"role":"user","content":"hi"}]}'`}
            />
          </TabsContent>
        </Tabs>
      </Section>

      <KeyDialog
        item={editing === 'new' ? undefined : (editing ?? undefined)}
        defaults={d.defaultRequestAdmission}
        open={editing !== null}
        onOpenChange={(o) => !o && setEditing(null)}
        onSaved={setData}
      />
      <RotateDialog open={rotateOpen} onOpenChange={setRotateOpen} onSaved={setData} />
    </Page>
  )
}

function Snippet({ code }: { code: string }) {
  return (
    <div className="relative mt-3">
      <div className="absolute top-2 right-2">
        <CopyButton value={code} />
      </div>
      <pre className="overflow-x-auto rounded-lg bg-muted/60 p-4 font-mono text-xs leading-relaxed">{code}</pre>
    </div>
  )
}

function KeyDialog({
  item,
  defaults,
  open,
  onOpenChange,
  onSaved,
}: {
  item?: RequestApiKeyItem
  defaults: RequestAdmissionConfig
  open: boolean
  onOpenChange: (v: boolean) => void
  onSaved: (d: Awaited<ReturnType<typeof systemApi.accessKeys>>) => void
}) {
  const [name, setName] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [custom, setCustom] = useState(false)
  const [adm, setAdm] = useState<RequestAdmissionConfig>(defaults)
  const [lastOpen, setLastOpen] = useState(false)
  if (open !== lastOpen) {
    setLastOpen(open)
    if (open) {
      setName(item?.name ?? '')
      setApiKey(item ? '' : randomKey('sk-kiro-'))
      setCustom(!!item?.requestAdmission)
      setAdm(item?.requestAdmission ?? defaults)
    }
  }
  const save = useMutation({
    mutationFn: () =>
      item
        ? systemApi.updateRequestKey(item.id, {
            name: name.trim(),
            apiKey: apiKey.trim() || undefined,
            requestAdmission: custom ? adm : undefined,
          })
        : systemApi.createRequestKey({
            name: name.trim(),
            apiKey: apiKey.trim(),
            enabled: true,
            requestAdmission: custom ? adm : undefined,
          }),
    onSuccess: (d) => {
      onSaved(d)
      onOpenChange(false)
    },
    meta: { success: item ? 'Key 已更新' : 'Key 已创建，请复制保存', error: '保存失败' },
  })
  const setA = (k: keyof RequestAdmissionConfig, v: number | null) => setAdm((a) => ({ ...a, [k]: v ?? 0 }))

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{item ? '编辑请求 Key' : '新建请求 Key'}</DialogTitle>
          <DialogDescription>每实例分别计数；多实例部署时总量约为实例数倍</DialogDescription>
        </DialogHeader>
        <div className="grid gap-3">
          <div className="space-y-1.5">
            <Label htmlFor="k-name">名称</Label>
            <Input id="k-name" value={name} onChange={(e) => setName(e.target.value)} placeholder="例如 team-a" />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="k-key">{item ? '替换 Key（留空不修改）' : 'Key'}</Label>
            <div className="flex gap-2">
              <Input id="k-key" value={apiKey} onChange={(e) => setApiKey(e.target.value)} className="font-mono text-xs" />
              <Button variant="outline" size="sm" onClick={() => setApiKey(randomKey('sk-kiro-'))}>
                生成
              </Button>
            </div>
          </div>
          <label className="flex items-center justify-between border-t pt-3 text-sm font-medium">
            单独设置准入限制
            <Switch checked={custom} onCheckedChange={setCustom} />
          </label>
          {custom && (
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label>RPM</Label>
                <NumberInput value={adm.rpm} min={0} onChange={(v) => setA('rpm', v)} />
              </div>
              <div className="space-y-1.5">
                <Label>并发</Label>
                <NumberInput value={adm.maxConcurrentRequests} min={0} onChange={(v) => setA('maxConcurrentRequests', v)} />
              </div>
              <div className="space-y-1.5">
                <Label>队列</Label>
                <NumberInput value={adm.maxQueuedRequests} min={0} onChange={(v) => setA('maxQueuedRequests', v)} />
              </div>
              <div className="space-y-1.5">
                <Label>等待</Label>
                <NumberInput value={adm.queueTimeoutMs} min={0} suffix="ms" onChange={(v) => setA('queueTimeoutMs', v)} />
              </div>
              <p className="col-span-2 text-xs text-muted-foreground">0 表示不限制</p>
            </div>
          )}
        </div>
        <DialogFooter>
          <Button onClick={() => save.mutate()} disabled={(!item && apiKey.trim().length < 8) || save.isPending}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function RotateDialog({
  open,
  onOpenChange,
  onSaved,
}: {
  open: boolean
  onOpenChange: (v: boolean) => void
  onSaved: (d: Awaited<ReturnType<typeof systemApi.accessKeys>>) => void
}) {
  const [value, setValue] = useState('')
  const [lastOpen, setLastOpen] = useState(false)
  if (open !== lastOpen) {
    setLastOpen(open)
    if (open) setValue(randomKey('kiro-admin-'))
  }
  const save = useMutation({
    mutationFn: () => systemApi.updateAdminKey(value.trim()),
    onSuccess: (d) => {
      // 轮换后当前会话立即改用新 Key，避免被登出
      authStorage.set(value.trim(), authStorage.isRemembered())
      onSaved(d)
      onOpenChange(false)
    },
    meta: { success: 'Admin Key 已轮换，当前会话已切换到新 Key', error: '轮换失败' },
  })
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>轮换 Admin Key</DialogTitle>
          <DialogDescription>旧 Key 会立即失效，其他已登录的浏览器需要用新 Key 重新登录。请先复制保存新 Key。</DialogDescription>
        </DialogHeader>
        <div className="flex gap-2">
          <Input value={value} onChange={(e) => setValue(e.target.value)} className="font-mono text-xs" aria-label="新的 Admin Key" />
          <CopyButton value={value} size="icon-sm" />
        </div>
        {value.trim().length < 8 && <p className="text-xs text-danger">至少 8 个字符</p>}
        <DialogFooter>
          <Button variant="destructive" onClick={() => save.mutate()} disabled={value.trim().length < 8 || save.isPending}>
            确认轮换
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
