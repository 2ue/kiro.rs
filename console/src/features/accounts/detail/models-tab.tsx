import { useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { RefreshCw, ScanSearch, X } from 'lucide-react'
import { toast } from 'sonner'
import { credentialsApi } from '@/api/endpoints/credentials'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { qk } from '@/queries/keys'
import type { AccountRow } from '../queries'

/** 账号模型白名单：为空表示不限制 */
export function ModelsTab({ row }: { row: AccountRow }) {
  const queryClient = useQueryClient()
  const source = (row.supportedModels ?? []).join('\n')
  const [models, setModels] = useState<string[]>(row.supportedModels ?? [])
  const [synced, setSynced] = useState(source)
  const [input, setInput] = useState('')
  // 服务端白名单变化时同步本地编辑状态（React 推荐的"渲染期调整 state"写法）
  if (synced !== source) {
    setSynced(source)
    setModels(row.supportedModels ?? [])
  }
  const invalidate = () => queryClient.invalidateQueries({ queryKey: qk.credentials.all })

  const save = useMutation({
    mutationFn: (next: string[]) => credentialsApi.setSupportedModels(row.id, next),
    onSuccess: (r) => toast.success(r.count ? `已保存 ${r.count} 个模型` : '已清空白名单，不再限制模型'),
    onSettled: invalidate,
    meta: { error: '保存失败' },
  })
  const sync = useMutation({
    mutationFn: () => credentialsApi.syncSupportedModels(row.id),
    onSuccess: (r) => toast.success(`已从上游同步 ${r.count} 个模型`),
    onSettled: invalidate,
    meta: { error: '同步失败' },
  })
  const discover = useMutation({
    mutationFn: () => credentialsApi.discoverSupportedModels(row.id),
    onSuccess: (r) => {
      setModels(r.supportedModels)
      toast.info(`探测到 ${r.count} 个模型，确认后点击保存`)
    },
    meta: { error: '探测失败' },
  })

  const original = (row.supportedModels ?? []).join('\n')
  const dirty = models.join('\n') !== original
  const add = () => {
    const values = input
      .split(/[\s,]+/)
      .map((v) => v.trim())
      .filter(Boolean)
    if (!values.length) return
    setModels((m) => [...new Set([...m, ...values])])
    setInput('')
  }

  return (
    <div className="space-y-4">
      <p className="text-xs text-muted-foreground">白名单为空时，账号可服务所有模型。设置后，调度器只会把这些模型的请求分配给该账号。</p>
      <div className="flex gap-2">
        <Button size="sm" variant="outline" onClick={() => discover.mutate()} disabled={discover.isPending}>
          <ScanSearch /> 探测
        </Button>
        <Button size="sm" variant="outline" onClick={() => sync.mutate()} disabled={sync.isPending}>
          <RefreshCw className={sync.isPending ? 'animate-spin' : undefined} /> 从上游同步并保存
        </Button>
      </div>
      <div className="flex gap-2">
        <Input
          placeholder="输入模型 ID，回车添加"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              add()
            }
          }}
        />
        <Button size="sm" variant="secondary" onClick={add}>
          添加
        </Button>
      </div>
      {models.length === 0 ? (
        <p className="rounded-lg border border-dashed p-4 text-center text-xs text-muted-foreground">未限制模型</p>
      ) : (
        <ul className="flex flex-wrap gap-1.5">
          {models.map((m) => (
            <li key={m} className="inline-flex items-center gap-1 rounded-md border bg-muted/50 py-0.5 pr-0.5 pl-2 font-mono text-xs">
              {m}
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={`移除 ${m}`}
                onClick={() => setModels((list) => list.filter((x) => x !== m))}
              >
                <X />
              </Button>
            </li>
          ))}
        </ul>
      )}
      {dirty && (
        <div className="flex justify-end gap-2 border-t pt-3">
          <Button variant="ghost" size="sm" onClick={() => setModels(row.supportedModels ?? [])}>
            放弃
          </Button>
          <Button size="sm" onClick={() => save.mutate(models)} disabled={save.isPending}>
            保存白名单
          </Button>
        </div>
      )}
    </div>
  )
}
