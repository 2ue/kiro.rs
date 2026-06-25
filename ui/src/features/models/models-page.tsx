import { useState } from 'react'
import {
  Plus,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  useConfirm,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Dialog,
  DialogBody,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
  Spinner,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import { formatNumber } from '@/lib/format'
import {
  useDeleteManualModel,
  useModelCapabilities,
  useModelPricing,
  useSyncModelCapabilities,
  useSyncModelPricing,
  useUpsertManualModel,
} from '@/hooks/use-usage'
import type { ModelCapabilityItem, UpsertManualModelRequest } from '@/types/api'

// ─── 工具 ──────────────────────────────────────────────────────────────────────

function isManual(item: ModelCapabilityItem): boolean {
  return item.source === 'manual'
}

function formatTokens(v?: number): string {
  if (v == null) return '—'
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`
  if (v >= 1000) return `${(v / 1000).toFixed(0)}K`
  return String(v)
}

function formatUsdPerM(perToken?: number): string {
  if (perToken == null) return '—'
  return `$${(perToken * 1_000_000).toFixed(2)}`
}

// ─── ModelsPage ───────────────────────────────────────────────────────────────

export function ModelsPage() {
  const capabilities = useModelCapabilities()
  const pricing = useModelPricing()
  const syncCap = useSyncModelCapabilities()
  const syncPrice = useSyncModelPricing()
  const deleteModel = useDeleteManualModel()
  const confirm = useConfirm()

  const [editTarget, setEditTarget] = useState<ModelCapabilityItem | null>(null)
  const [addOpen, setAddOpen] = useState(false)

  const models = capabilities.data?.models ?? []
  const priceMap = new Map((pricing.data?.models ?? []).map((p) => [p.model, p.pricing]))

  const handleDelete = async (item: ModelCapabilityItem) => {
    const ok = await confirm({
      title: '删除手动模型',
      message: `确定删除手动添加的模型 "${item.model}"？此操作无法撤销。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!ok) return
    deleteModel.mutate(item.model, {
      onSuccess: () => toast.success(`已删除模型 ${item.model}`),
      onError: (e) => toast.error(`删除失败: ${extractErrorMessage(e)}`),
    })
  }

  const capLastSync = capabilities.data?.lastSyncedAt
    ? new Date(capabilities.data.lastSyncedAt).toLocaleString('zh-CN')
    : '—'
  const priceLastSync = pricing.data?.lastSyncedAt
    ? new Date(pricing.data.lastSyncedAt).toLocaleString('zh-CN')
    : '—'

  return (
    <PageContainer>
      <PageHeader
        title="模型能力"
        subtitle="查看同步来的模型列表、手动维护能力与定价信息"
        actions={
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={syncCap.isPending}
              onClick={() =>
                syncCap.mutate(undefined, {
                  onSuccess: () => toast.success('能力目录已同步'),
                  onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
                })
              }
            >
              {syncCap.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-4 w-4" />}
              同步能力
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={syncPrice.isPending}
              onClick={() =>
                syncPrice.mutate(undefined, {
                  onSuccess: () => toast.success('价格目录已同步'),
                  onError: (e) => toast.error(`同步失败: ${extractErrorMessage(e)}`),
                })
              }
            >
              {syncPrice.isPending ? <Spinner size="sm" /> : <RefreshCw className="h-4 w-4" />}
              同步价格
            </Button>
            <Button size="sm" onClick={() => setAddOpen(true)}>
              <Plus className="h-4 w-4" />
              手动添加
            </Button>
          </div>
        }
      />

      {/* 同步状态摘要 */}
      <div className="grid gap-3 sm:grid-cols-2">
        <SectionCard title="能力目录" description={`来源: ${capabilities.data?.source ?? '—'} · 最后同步: ${capLastSync}`}>
          <div className="flex items-center gap-2">
            <Badge tone={capabilities.data?.available ? 'success' : 'error'}>
              {capabilities.data?.available ? '可用' : '不可用'}
            </Badge>
            <span className="text-sm text-muted-foreground">
              共 {formatNumber(capabilities.data?.modelCount ?? 0)} 个模型
            </span>
            {capabilities.data?.lastError && (
              <span className="text-xs text-destructive truncate max-w-xs">{capabilities.data.lastError}</span>
            )}
          </div>
        </SectionCard>
        <SectionCard title="价格目录" description={`来源: ${pricing.data?.source ?? '—'} · 最后同步: ${priceLastSync}`}>
          <div className="flex items-center gap-2">
            <Badge tone={pricing.data?.available ? 'success' : 'error'}>
              {pricing.data?.available ? '可用' : '不可用'}
            </Badge>
            <span className="text-sm text-muted-foreground">
              共 {formatNumber(pricing.data?.modelCount ?? 0)} 个模型
            </span>
            {pricing.data?.lastError && (
              <span className="text-xs text-destructive truncate max-w-xs">{pricing.data.lastError}</span>
            )}
          </div>
        </SectionCard>
      </div>

      {/* 模型能力清单 */}
      <SectionCard
        title="模型清单"
        description="系统同步的模型不可删除，手动添加的可编辑或删除"
        noPadding
      >
        {capabilities.isLoading ? (
          <LoadingState text="加载模型列表..." className="py-8" />
        ) : capabilities.error ? (
          <ErrorState message={extractErrorMessage(capabilities.error)} />
        ) : models.length === 0 ? (
          <EmptyState
            title="暂无模型"
            description="点击「同步能力」获取最新模型列表，或手动添加"
            action={
              <Button size="sm" onClick={() => setAddOpen(true)}>
                <Plus className="h-4 w-4" />手动添加
              </Button>
            }
          />
        ) : (
          <div className="scrollbar-thin overflow-x-auto">
            <Table className="min-w-[700px]">
              <TableHeader>
                <TableRow>
                  <TableHead>模型 ID</TableHead>
                  <TableHead>显示名称</TableHead>
                  <TableHead className="text-right">最大输入</TableHead>
                  <TableHead className="text-right">最大输出</TableHead>
                  <TableHead>Caching</TableHead>
                  <TableHead className="text-right">输入价格/M</TableHead>
                  <TableHead className="text-right">输出价格/M</TableHead>
                  <TableHead>来源</TableHead>
                  <TableHead className="w-20" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {models.map((item) => {
                  const price = priceMap.get(item.model)
                  const manual = isManual(item)
                  return (
                    <TableRow key={item.model}>
                      <TableCell className="font-mono text-xs max-w-[180px]">
                        <div className="truncate" title={item.model}>{item.model}</div>
                      </TableCell>
                      <TableCell className="text-sm text-muted-foreground max-w-[140px]">
                        <div className="truncate">{item.displayName || '—'}</div>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatTokens(item.maxInputTokens)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatTokens(item.maxOutputTokens)}
                      </TableCell>
                      <TableCell>
                        {item.supportsPromptCaching ? (
                          <Badge tone="success">支持</Badge>
                        ) : (
                          <span className="text-xs text-muted-foreground/50">—</span>
                        )}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatUsdPerM(price?.inputCostPerToken)}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs tabular-nums">
                        {formatUsdPerM(price?.outputCostPerToken)}
                      </TableCell>
                      <TableCell>
                        <Badge tone={manual ? 'primary' : 'neutral'}>
                          {manual ? '手动' : item.source ?? '同步'}
                        </Badge>
                      </TableCell>
                      <TableCell>
                        {manual && (
                          <div className="flex items-center gap-1">
                            <Button
                              variant="ghost"
                              size="xs"
                              onClick={() => setEditTarget(item)}
                            >
                              编辑
                            </Button>
                            <Button
                              variant="ghost"
                              size="xs"
                              className="text-destructive hover:text-destructive"
                              onClick={() => handleDelete(item)}
                            >
                              <Trash2 className="h-3.5 w-3.5" />
                            </Button>
                          </div>
                        )}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </div>
        )}
      </SectionCard>

      {/* 弹窗 */}
      <EditModelDialog
        open={addOpen}
        onClose={() => setAddOpen(false)}
        onSaved={() => {}}
      />
      {editTarget && (
        <EditModelDialog
          open
          initial={editTarget}
          onClose={() => setEditTarget(null)}
          onSaved={() => setEditTarget(null)}
        />
      )}
    </PageContainer>
  )
}

// ─── 能力编辑弹窗 ─────────────────────────────────────────────────────────────

interface EditModelDialogProps {
  open: boolean
  initial?: ModelCapabilityItem
  onClose: () => void
  onSaved: () => void
}

function EditModelDialog({ open, initial, onClose, onSaved }: EditModelDialogProps) {
  const upsert = useUpsertManualModel()
  const [model, setModel] = useState(initial?.model ?? '')
  const [displayName, setDisplayName] = useState(initial?.displayName ?? '')
  const [maxInput, setMaxInput] = useState(String(initial?.maxInputTokens ?? ''))
  const [maxOutput, setMaxOutput] = useState(String(initial?.maxOutputTokens ?? ''))
  const [caching, setCaching] = useState(initial?.supportsPromptCaching ?? false)
  const [inputTypes, setInputTypes] = useState((initial?.supportedInputTypes ?? ['text']).join(', '))

  const handleSave = () => {
    if (!model.trim()) return toast.error('模型名称不能为空')
    const payload: UpsertManualModelRequest = {
      model: model.trim().toLowerCase(),
      displayName: displayName.trim() || undefined,
      maxInputTokens: maxInput ? Number(maxInput) : undefined,
      maxOutputTokens: maxOutput ? Number(maxOutput) : undefined,
      supportsPromptCaching: caching,
      supportedInputTypes: inputTypes.split(',').map((s) => s.trim()).filter(Boolean),
    }
    upsert.mutate(payload, {
      onSuccess: () => {
        toast.success(`模型 ${payload.model} 已保存`)
        onSaved()
        onClose()
      },
      onError: (e) => toast.error(`保存失败: ${extractErrorMessage(e)}`),
    })
  }

  return (
    <Dialog open={open} onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{initial ? '编辑模型能力' : '手动添加模型'}</DialogTitle>
        </DialogHeader>
        <DialogBody className="space-y-3">
          <div className="space-y-1.5">
            <Label>模型 ID</Label>
            <Input
              value={model}
              placeholder="claude-opus-4-5"
              disabled={!!initial}
              onChange={(e) => setModel(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label>显示名称（可选）</Label>
            <Input value={displayName} placeholder="Claude Opus 4.5" onChange={(e) => setDisplayName(e.target.value)} />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label>最大输入 Token</Label>
              <Input type="number" value={maxInput} placeholder="200000" onChange={(e) => setMaxInput(e.target.value)} />
            </div>
            <div className="space-y-1.5">
              <Label>最大输出 Token</Label>
              <Input type="number" value={maxOutput} placeholder="32000" onChange={(e) => setMaxOutput(e.target.value)} />
            </div>
          </div>
          <div className="space-y-1.5">
            <Label>支持的输入类型（逗号分隔）</Label>
            <Input value={inputTypes} placeholder="text, image" onChange={(e) => setInputTypes(e.target.value)} />
          </div>
          <div className="flex items-center gap-3">
            <Switch checked={caching} onCheckedChange={setCaching} />
            <span className="text-sm">支持 Prompt Caching</span>
          </div>
        </DialogBody>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>取消</Button>
          <Button size="sm" disabled={upsert.isPending} onClick={handleSave}>
            {upsert.isPending ? <Spinner size="sm" /> : null}
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
