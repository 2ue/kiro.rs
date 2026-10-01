import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import type { ColumnDef } from '@tanstack/react-table'
import { FileUp, FlaskConical, Loader2, MoreHorizontal, Pencil, Plus, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { proxiesApi } from '@/api/endpoints/proxies'
import type { ProxyResource, ProxyResourceTestResponse } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { DataTable } from '@/components/data-table/data-table'
import { EmptyState, ErrorState } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { useConfirm } from '@/components/patterns/confirm'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtMs, fmtRelative } from '@/lib/format'
import { qk } from '@/queries/keys'
import { useProxies } from '@/queries/shared'

/** 代理 URL 去掉内嵌凭据后展示 */
function displayUrl(url: string): string {
  try {
    const u = new URL(url)
    u.username = ''
    u.password = ''
    return u.toString().replace(/\/$/, '')
  } catch {
    return url
  }
}

export function ProxiesPage() {
  const proxies = useProxies()
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const [editing, setEditing] = useState<ProxyResource | 'new' | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const [results, setResults] = useState<Record<number, ProxyResourceTestResponse | 'pending'>>({})
  const invalidate = () => queryClient.invalidateQueries({ queryKey: qk.proxies.all })

  const test = async (id: number) => {
    setResults((r) => ({ ...r, [id]: 'pending' }))
    try {
      const res = await proxiesApi.test(id)
      setResults((r) => ({ ...r, [id]: res }))
    } catch (e) {
      setResults((r) => ({ ...r, [id]: { success: false, message: (e as Error).message, proxyUrl: '', testUrl: '', durationMs: 0 } }))
    }
  }
  const testAll = () => (proxies.data?.resources ?? []).filter((p) => p.enabled).forEach((p) => void test(p.id))

  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) => proxiesApi.update(id, { enabled }),
    onSettled: invalidate,
    meta: { error: '更新失败' },
  })
  const remove = useMutation({ mutationFn: (id: number) => proxiesApi.remove(id), onSettled: invalidate, meta: { success: '代理已删除', error: '删除失败' } })

  const columns = useMemo<ColumnDef<ProxyResource, unknown>[]>(
    () => [
      {
        id: 'name',
        header: '名称',
        size: 180,
        cell: ({ row: { original: p } }) => (
          <div className="min-w-0 leading-tight">
            <div className="truncate font-medium">{p.name}</div>
            {p.notes && <div className="truncate text-xs text-muted-foreground">{p.notes}</div>}
          </div>
        ),
      },
      {
        id: 'url',
        header: '地址',
        size: 260,
        meta: { grow: true },
        cell: ({ row: { original: p } }) => (
          <span className="truncate font-mono text-xs">
            {displayUrl(p.proxyUrl)}
            {(p.proxyUsername || p.hasPassword) && <span className="ml-1.5 text-muted-foreground">（含认证）</span>}
          </span>
        ),
      },
      {
        id: 'enabled',
        header: '启用',
        size: 72,
        cell: ({ row: { original: p } }) => (
          <span onClick={(e) => e.stopPropagation()}>
            <Switch
              checked={p.enabled}
              aria-label={`启用 ${p.name}`}
              onCheckedChange={async (enabled) => {
                if (!enabled && p.credentialCount > 0) {
                  const ok = await confirm({
                    title: `停用代理「${p.name}」？`,
                    description: `${p.credentialCount} 个绑定账号会因代理不可用而无法调度，直到重新启用或改绑。`,
                    destructive: true,
                    confirmText: '停用',
                  })
                  if (!ok) return
                }
                toggle.mutate({ id: p.id, enabled })
              }}
            />
          </span>
        ),
      },
      {
        id: 'bound',
        header: '绑定账号',
        size: 96,
        meta: { align: 'right' },
        cell: ({ row: { original: p } }) =>
          p.credentialCount > 0 ? (
            <Link to="/accounts" search={{ proxy: p.id }} className="text-primary hover:underline" onClick={(e) => e.stopPropagation()}>
              {p.credentialCount}
            </Link>
          ) : (
            <span className="text-muted-foreground">0</span>
          ),
      },
      {
        id: 'test',
        header: '最近测试',
        size: 200,
        cell: ({ row: { original: p } }) => {
          const r = results[p.id]
          if (r === 'pending') return <Loader2 className="size-4 animate-spin text-muted-foreground" />
          if (!r) return <span className="text-xs text-muted-foreground">未测试</span>
          return (
            <span title={r.message} className="flex items-center gap-1.5">
              <ToneBadge tone={r.success ? 'success' : 'danger'}>{r.success ? '可用' : '失败'}</ToneBadge>
              <span className="num truncate text-xs text-muted-foreground">{r.success ? fmtMs(r.durationMs) : r.message}</span>
            </span>
          )
        },
      },
      {
        id: 'updated',
        header: '更新',
        size: 90,
        meta: { align: 'right' },
        cell: ({ row: { original: p } }) => <span className="text-xs text-muted-foreground">{fmtRelative(p.updatedAt)}</span>,
      },
      {
        id: 'actions',
        header: '',
        size: 48,
        cell: ({ row: { original: p } }) => (
          <span onClick={(e) => e.stopPropagation()}>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon-xs" aria-label="更多操作">
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => void test(p.id)}>
                  <FlaskConical /> 测试
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => setEditing(p)}>
                  <Pencil /> 编辑
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  variant="destructive"
                  onSelect={async () => {
                    const ok = await confirm({
                      title: `删除代理「${p.name}」？`,
                      description: p.credentialCount > 0 ? `${p.credentialCount} 个账号绑定了该代理，删除后这些账号将无法调度。` : '删除后不可恢复。',
                      destructive: true,
                      confirmText: '删除',
                    })
                    if (ok) remove.mutate(p.id)
                  }}
                >
                  <Trash2 /> 删除
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </span>
        ),
      },
    ],
    [results], // eslint-disable-line react-hooks/exhaustive-deps
  )

  const list = proxies.data?.resources ?? []
  return (
    <Page>
      <PageHeader
        title="代理"
        description="出站代理资源；账号绑定后通过该代理访问 Kiro 上游"
        actions={
          <>
            <Button variant="outline" size="sm" onClick={testAll} disabled={!list.length}>
              <FlaskConical /> 测试全部
            </Button>
            <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
              <FileUp /> 批量导入
            </Button>
            <Button size="sm" onClick={() => setEditing('new')}>
              <Plus /> 添加代理
            </Button>
          </>
        }
      />
      {proxies.error && !proxies.data ? (
        <ErrorState error={proxies.error} onRetry={() => proxies.refetch()} />
      ) : (
        <DataTable
          data={list}
          columns={columns}
          getRowId={(p) => String(p.id)}
          onRowClick={(p) => setEditing(p)}
          rowClassName={(p) => (p.enabled ? undefined : 'text-muted-foreground')}
          empty={proxies.isLoading ? <div className="p-6 text-center text-sm text-muted-foreground">加载中…</div> : <EmptyState title="还没有代理资源" />}
        />
      )}
      <ProxyDialog proxy={editing === 'new' ? undefined : editing ?? undefined} open={editing !== null} onOpenChange={(o) => !o && setEditing(null)} />
      <ImportDialog open={importOpen} onOpenChange={setImportOpen} />
    </Page>
  )
}

function ProxyDialog({ proxy, open, onOpenChange }: { proxy?: ProxyResource; open: boolean; onOpenChange: (v: boolean) => void }) {
  const queryClient = useQueryClient()
  const [form, setForm] = useState({ name: '', proxyUrl: '', proxyUsername: '', proxyPassword: '', enabled: true, notes: '' })
  const [lastOpen, setLastOpen] = useState(false)
  if (open !== lastOpen) {
    setLastOpen(open)
    if (open)
      setForm({
        name: proxy?.name ?? '',
        proxyUrl: proxy?.proxyUrl ?? '',
        proxyUsername: proxy?.proxyUsername ?? '',
        proxyPassword: '',
        enabled: proxy?.enabled ?? true,
        notes: proxy?.notes ?? '',
      })
  }
  const testCfg = useMutation({
    mutationFn: () => proxiesApi.testConfig({ proxyUrl: form.proxyUrl.trim(), proxyUsername: form.proxyUsername.trim() || undefined, proxyPassword: form.proxyPassword || undefined }),
    meta: { error: '测试失败' },
  })
  const save = useMutation({
    mutationFn: () =>
      proxy
        ? proxiesApi.update(proxy.id, {
            name: form.name.trim(),
            proxyUrl: form.proxyUrl.trim(),
            proxyUsername: form.proxyUsername.trim() || undefined,
            clearUsername: !form.proxyUsername.trim() && !!proxy.proxyUsername,
            proxyPassword: form.proxyPassword || undefined,
            enabled: form.enabled,
            notes: form.notes.trim() || undefined,
            clearNotes: !form.notes.trim() && !!proxy.notes,
          })
        : proxiesApi.create({
            name: form.name.trim(),
            proxyUrl: form.proxyUrl.trim(),
            proxyUsername: form.proxyUsername.trim() || undefined,
            proxyPassword: form.proxyPassword || undefined,
            enabled: form.enabled,
            notes: form.notes.trim() || undefined,
          }),
    onSuccess: () => {
      onOpenChange(false)
      queryClient.invalidateQueries({ queryKey: qk.proxies.all })
    },
    meta: { success: proxy ? '代理已保存' : '代理已添加', error: '保存失败' },
  })
  const set = (k: keyof typeof form, v: string | boolean) => setForm((f) => ({ ...f, [k]: v }))

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{proxy ? '编辑代理' : '添加代理'}</DialogTitle>
          <DialogDescription>支持 http://、https://、socks5:// 代理</DialogDescription>
        </DialogHeader>
        <div className="grid gap-3">
          <div className="space-y-1.5">
            <Label htmlFor="px-name">名称</Label>
            <Input id="px-name" value={form.name} onChange={(e) => set('name', e.target.value)} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="px-url">代理地址</Label>
            <Input id="px-url" value={form.proxyUrl} onChange={(e) => set('proxyUrl', e.target.value)} placeholder="socks5://host:1080" className="font-mono text-xs" />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="px-user">用户名</Label>
              <Input id="px-user" value={form.proxyUsername} onChange={(e) => set('proxyUsername', e.target.value)} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="px-pass">密码</Label>
              <Input id="px-pass" type="password" value={form.proxyPassword} onChange={(e) => set('proxyPassword', e.target.value)} placeholder={proxy?.hasPassword ? '不修改' : ''} />
            </div>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="px-notes">备注</Label>
            <Textarea id="px-notes" rows={2} value={form.notes} onChange={(e) => set('notes', e.target.value)} />
          </div>
          <label className="flex items-center justify-between text-sm">
            启用
            <Switch checked={form.enabled} onCheckedChange={(v) => set('enabled', v)} />
          </label>
          {testCfg.data && (
            <p className={`text-xs ${testCfg.data.success ? 'text-success' : 'text-danger'}`}>
              {testCfg.data.success ? `可用 · ${fmtMs(testCfg.data.durationMs)}` : testCfg.data.message}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => testCfg.mutate()} disabled={!form.proxyUrl.trim() || testCfg.isPending}>
            {testCfg.isPending && <Loader2 className="animate-spin" />}测试连接
          </Button>
          <Button onClick={() => save.mutate()} disabled={!form.name.trim() || !form.proxyUrl.trim() || save.isPending}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ImportDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (v: boolean) => void }) {
  const queryClient = useQueryClient()
  const [content, setContent] = useState('')
  const [prefix, setPrefix] = useState('proxy')
  const run = useMutation({
    mutationFn: () => proxiesApi.import({ content, namePrefix: prefix.trim() || undefined, enabled: true, continueOnError: true }),
    onSuccess: (r) => {
      queryClient.invalidateQueries({ queryKey: qk.proxies.all })
      if (r.failed) toast.warning(`导入 ${r.success} 个，失败 ${r.failed} 个`, { description: r.items.filter((i) => !i.ok).slice(0, 3).map((i) => `第 ${i.index + 1} 行：${i.error}`).join('\n') })
      else toast.success(`已导入 ${r.success} 个代理`)
      if (!r.failed) {
        setContent('')
        onOpenChange(false)
      }
    },
    meta: { error: '导入失败' },
  })
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>批量导入代理</DialogTitle>
          <DialogDescription>每行一个，支持 scheme://user:pass@host:port 或 host:port:user:pass</DialogDescription>
        </DialogHeader>
        <Textarea rows={10} value={content} onChange={(e) => setContent(e.target.value)} className="font-mono text-xs" />
        <div className="space-y-1.5">
          <Label htmlFor="px-prefix">名称前缀</Label>
          <Input id="px-prefix" value={prefix} onChange={(e) => setPrefix(e.target.value)} />
        </div>
        <DialogFooter>
          <Button onClick={() => run.mutate()} disabled={!content.trim() || run.isPending}>
            {run.isPending && <Loader2 className="animate-spin" />}导入
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
