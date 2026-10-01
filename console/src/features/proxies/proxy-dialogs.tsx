import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { proxiesApi } from '@/api/endpoints/proxies'
import type { ProxyResource } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { fmtMs } from '@/lib/format'
import { qk } from '@/queries/keys'

export function ProxyDialog({ proxy, open, onOpenChange }: { proxy?: ProxyResource; open: boolean; onOpenChange: (v: boolean) => void }) {
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
    mutationFn: () =>
      proxiesApi.testConfig({
        proxyUrl: form.proxyUrl.trim(),
        proxyUsername: form.proxyUsername.trim() || undefined,
        proxyPassword: form.proxyPassword || undefined,
      }),
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
            <Input
              id="px-url"
              value={form.proxyUrl}
              onChange={(e) => set('proxyUrl', e.target.value)}
              placeholder="socks5://host:1080"
              className="font-mono text-xs"
            />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="px-user">用户名</Label>
              <Input id="px-user" value={form.proxyUsername} onChange={(e) => set('proxyUsername', e.target.value)} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="px-pass">密码</Label>
              <Input
                id="px-pass"
                type="password"
                value={form.proxyPassword}
                onChange={(e) => set('proxyPassword', e.target.value)}
                placeholder={proxy?.hasPassword ? '不修改' : ''}
              />
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

export function ImportDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (v: boolean) => void }) {
  const queryClient = useQueryClient()
  const [content, setContent] = useState('')
  const [prefix, setPrefix] = useState('proxy')
  const run = useMutation({
    mutationFn: () => proxiesApi.import({ content, namePrefix: prefix.trim() || undefined, enabled: true, continueOnError: true }),
    onSuccess: (r) => {
      queryClient.invalidateQueries({ queryKey: qk.proxies.all })
      if (r.failed)
        toast.warning(`导入 ${r.success} 个，失败 ${r.failed} 个`, {
          description: r.items
            .filter((i) => !i.ok)
            .slice(0, 3)
            .map((i) => `第 ${i.index + 1} 行：${i.error}`)
            .join('\n'),
        })
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
