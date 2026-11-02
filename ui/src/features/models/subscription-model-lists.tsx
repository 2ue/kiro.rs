import { useEffect, useMemo, useState } from 'react'
import { Pencil, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'
import { EmptyState, ErrorState, LoadingState, ModalShell, SectionCard } from '@/components/patterns'
import {
  Badge,
  Button,
  Checkbox,
  Label,
  Spinner,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Textarea,
} from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import {
  useModelCapabilities,
  useSetSubscriptionModelList,
  useSubscriptionModelLists,
  useSyncSubscriptionModelList,
} from '@/hooks/use-usage'
import type { SubscriptionModelListItem } from '@/types/api'

function sourceText(item: SubscriptionModelListItem): string {
  if (item.models.length === 0) return '未设置'
  const via = item.sourceCredentialId ? `（账号 #${item.sourceCredentialId}）` : ''
  return item.source === 'auto' ? `自动获取${via}` : `手动${via}`
}

function formatTime(value?: string | null): string {
  if (!value) return '—'
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN')
}

/** 按订阅等级统一设置允许模型。账号自己设置了支持模型时以账号为准。 */
export function SubscriptionModelListsSection() {
  const lists = useSubscriptionModelLists()
  const sync = useSyncSubscriptionModelList()
  const [editing, setEditing] = useState<SubscriptionModelListItem | null>(null)
  const [syncingTier, setSyncingTier] = useState<string | null>(null)

  const handleSync = (item: SubscriptionModelListItem) => {
    setSyncingTier(item.tier)
    sync.mutate(
      { tier: item.tier },
      {
        onSuccess: (data) => {
          const updated = data.items.find((row) => row.tier === item.tier)
          toast.success(`已同步 ${item.label}：${updated?.models.length ?? 0} 个模型`)
        },
        onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
        onSettled: () => setSyncingTier(null),
      },
    )
  }

  return (
    <SectionCard
      title="订阅等级模型"
      description="按订阅等级统一限制可调度的模型。账号自己设置了支持模型时以账号为准；未设置列表的等级不限制。Free 在第一次调用到 Free 账号时自动获取一次。"
      noPadding
    >
      {lists.isLoading ? (
        <LoadingState text="加载订阅等级模型..." className="py-8" />
      ) : lists.error ? (
        <ErrorState message={extractErrorMessage(lists.error)} />
      ) : !lists.data?.items.length ? (
        <EmptyState title="暂无订阅等级" description="账号的订阅信息查询后会出现在这里" />
      ) : (
        <div className="scrollbar-thin overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>订阅等级</TableHead>
                <TableHead>允许模型</TableHead>
                <TableHead>来源</TableHead>
                <TableHead>更新时间</TableHead>
                <TableHead className="text-right">账号</TableHead>
                <TableHead className="text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {lists.data.items.map((item) => (
                <TableRow key={item.tier}>
                  <TableCell className="whitespace-nowrap font-medium">{item.label}</TableCell>
                  <TableCell className="max-w-[28rem]">
                    {item.models.length === 0 ? (
                      <span className="text-muted-foreground">不限制</span>
                    ) : (
                      <div className="flex flex-wrap gap-1">
                        {item.models.map((model) => (
                          <Badge key={model} tone="neutral" className="font-mono text-[11px]">{model}</Badge>
                        ))}
                      </div>
                    )}
                  </TableCell>
                  <TableCell className="whitespace-nowrap text-xs text-muted-foreground">{sourceText(item)}</TableCell>
                  <TableCell className="whitespace-nowrap text-xs text-muted-foreground">{formatTime(item.updatedAt)}</TableCell>
                  <TableCell
                    className="whitespace-nowrap text-right text-xs tabular-nums"
                    title="按等级列表限制的账号 / 该等级账号总数（设置了自身支持模型的账号不受等级列表限制）"
                  >
                    {item.tierLimitedCount} / {item.credentialCount}
                  </TableCell>
                  <TableCell className="whitespace-nowrap text-right">
                    <div className="flex justify-end gap-1">
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        title="从该等级的一个账号重新获取"
                        aria-label={`同步 ${item.label} 的模型列表`}
                        disabled={item.credentialCount === 0 || syncingTier !== null}
                        onClick={() => handleSync(item)}
                      >
                        {syncingTier === item.tier ? <Spinner size="sm" /> : <RefreshCw />}
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        title="编辑"
                        aria-label={`编辑 ${item.label} 的模型列表`}
                        onClick={() => setEditing(item)}
                      >
                        <Pencil />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      <EditSubscriptionModelListDialog item={editing} onClose={() => setEditing(null)} />
    </SectionCard>
  )
}

function EditSubscriptionModelListDialog({ item, onClose }: { item: SubscriptionModelListItem | null; onClose: () => void }) {
  const capabilities = useModelCapabilities()
  const save = useSetSubscriptionModelList()
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [extra, setExtra] = useState('')

  const catalog = useMemo(
    () => (capabilities.data?.models ?? []).map((model) => model.model).sort((a, b) => a.localeCompare(b)),
    [capabilities.data?.models],
  )

  useEffect(() => {
    if (!item) return
    const known = new Set(catalog)
    setSelected(new Set(item.models.filter((model) => known.has(model))))
    setExtra(item.models.filter((model) => !known.has(model)).join('\n'))
  }, [item, catalog])

  if (!item) return null

  const toggle = (model: string, checked: boolean) => {
    setSelected((current) => {
      const next = new Set(current)
      if (checked) next.add(model)
      else next.delete(model)
      return next
    })
  }

  const models = [
    ...selected,
    ...extra.split(/[\n,]/).map((value) => value.trim()).filter(Boolean),
  ]

  const handleSave = () => {
    save.mutate(
      { tier: item.tier, models },
      {
        onSuccess: () => {
          toast.success(models.length === 0 ? `已清除 ${item.label} 的列表（不限制）` : `已保存 ${item.label}：${new Set(models).size} 个模型`)
          onClose()
        },
        onError: (e) => toast.error(`保存失败: ${extractErrorMessage(e)}`),
      },
    )
  }

  return (
    <ModalShell open title={`编辑 ${item.label} 允许模型`} width="max-w-lg" onClose={onClose}>
      <div className="space-y-4">
        <p className="text-xs text-muted-foreground">
          只影响没有设置自身支持模型的 {item.label} 账号。全部不选并清空下方输入即为不限制。
        </p>
        <div className="space-y-2">
          <Label>模型目录</Label>
          {catalog.length === 0 ? (
            <p className="text-xs text-muted-foreground">模型目录为空，可在下方直接填写模型 ID。</p>
          ) : (
            <div className="scrollbar-thin grid max-h-64 grid-cols-1 gap-1.5 overflow-y-auto rounded-md border p-2 sm:grid-cols-2">
              {catalog.map((model) => {
                const id = `tier-model-${item.tier}-${model}`
                return (
                  <div key={model} className="flex items-center gap-2">
                    <Checkbox id={id} checked={selected.has(model)} onCheckedChange={(checked) => toggle(model, checked === true)} />
                    <Label htmlFor={id} className="cursor-pointer font-mono text-xs font-normal">{model}</Label>
                  </div>
                )
              })}
            </div>
          )}
        </div>
        <div className="space-y-2">
          <Label htmlFor={`tier-extra-${item.tier}`}>其他模型 ID</Label>
          <Textarea
            id={`tier-extra-${item.tier}`}
            value={extra}
            onChange={(e) => setExtra(e.target.value)}
            placeholder="每行一个，目录里没有的模型填在这里"
            rows={3}
            className="font-mono text-xs"
          />
        </div>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onClose} disabled={save.isPending}>取消</Button>
          <Button size="sm" onClick={handleSave} disabled={save.isPending}>
            {save.isPending && <Spinner size="sm" />}保存
          </Button>
        </div>
      </div>
    </ModalShell>
  )
}
