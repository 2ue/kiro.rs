import { useState } from 'react'
import { toast } from 'sonner'
import { RefreshCw, ChevronUp, ChevronDown, Wallet, Trash2, Loader2, PlayCircle } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { CredentialStatusItem, BalanceResponse } from '@/types/api'
import { extractErrorMessage } from '@/lib/utils'
import {
  useSetDisabled,
  useSetPriority,
  useSetWarmup,
  useClearInFlight,
  useResetFailure,
  useDeleteCredential,
  useForceRefreshToken,
  useRuntimeConfig,
} from '@/hooks/use-credentials'

interface CredentialCardProps {
  credential: CredentialStatusItem
  onQueryBalance: (id: number) => void
  onTestCredential: (credential: CredentialStatusItem) => void
  selected: boolean
  onToggleSelect: () => void
  balance: BalanceResponse | null
  loadingBalance: boolean
}

function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未使用'
  const date = new Date(lastUsedAt)
  const now = new Date()
  const diff = now.getTime() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  const days = Math.floor(hours / 24)
  return `${days} 天前`
}

function formatDateTime(value: string | null): string {
  if (!value) return '未知'
  return new Date(value).toLocaleString('zh-CN', {
    hour12: false,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function formatUsd(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: value >= 1 ? 2 : 6,
    maximumFractionDigits: value >= 1 ? 2 : 6,
  }).format(value)
}

function formatQuota(value: number): string {
  if (!Number.isFinite(value)) return '-'
  return new Intl.NumberFormat('zh-CN', {
    minimumFractionDigits: value >= 1 ? 2 : 6,
    maximumFractionDigits: value >= 1 ? 2 : 6,
  }).format(value)
}

export function CredentialCard({
  credential,
  onQueryBalance,
  onTestCredential,
  selected,
  onToggleSelect,
  balance,
  loadingBalance,
}: CredentialCardProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const resetFailure = useResetFailure()
  const deleteCredential = useDeleteCredential()
  const forceRefresh = useForceRefreshToken()
  const setWarmup = useSetWarmup()
  const clearInFlight = useClearInFlight()
  const runtimeConfig = useRuntimeConfig()
  const displayName = credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
  const warmupTarget = Math.max(0, runtimeConfig.data?.credentialWarmupRequests ?? 3)
  const accountInfo = balance || credential.accountInfo
  const subscriptionTitle = balance?.subscriptionTitle || credential.accountInfo?.subscriptionTitle || credential.subscriptionTitle || '未知'

  const handleToggleDisabled = () => {
    setDisabled.mutate(
      { id: credential.id, disabled: !credential.disabled },
      {
        onSuccess: (res) => {
          toast.success(res.message)
        },
        onError: (err) => {
          toast.error('操作失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handlePriorityChange = () => {
    const newPriority = parseInt(priorityValue, 10)
    if (isNaN(newPriority) || newPriority < 0) {
      toast.error('优先级必须是非负整数')
      return
    }
    setPriority.mutate(
      { id: credential.id, priority: newPriority },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingPriority(false)
        },
        onError: (err) => {
          toast.error('操作失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handleReset = () => {
    resetFailure.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('操作失败: ' + extractErrorMessage(err))
      },
    })
  }

  const handleForceRefresh = () => {
    forceRefresh.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('刷新失败: ' + extractErrorMessage(err))
      },
    })
  }

  const handleClearInFlight = () => {
    if (!confirm(`确定清理凭据 #${credential.id} 的当前并发占用吗？真实仍在运行的请求可能因此不再计入并发限制。`)) {
      return
    }
    clearInFlight.mutate(
      { id: credential.id },
      {
        onSuccess: (res) => toast.success(res.message),
        onError: (err) => toast.error('清理失败: ' + extractErrorMessage(err)),
      }
    )
  }

  const handleToggleWarmup = () => {
    const nextWarmup = credential.warmupRemaining > 0 ? 0 : Math.max(1, warmupTarget)
    setWarmup.mutate(
      { id: credential.id, warmupRemaining: nextWarmup },
      {
        onSuccess: () => {
          toast.success(nextWarmup > 0 ? `凭据 #${credential.id} 已开启预热` : `凭据 #${credential.id} 已关闭预热`)
        },
        onError: (err) => {
          toast.error('预热设置失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handleDelete = () => {
    if (!credential.disabled) {
      toast.error('请先禁用凭据再删除')
      setShowDeleteDialog(false)
      return
    }

    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
        setShowDeleteDialog(false)
      },
      onError: (err) => {
        toast.error('删除失败: ' + extractErrorMessage(err))
      },
    })
  }

  return (
    <>
      <Card className={credential.isCurrent ? 'ring-2 ring-primary' : ''}>
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Checkbox
                checked={selected}
                onCheckedChange={onToggleSelect}
              />
              <CardTitle className="flex min-w-0 flex-wrap items-center gap-2 text-lg">
                <span className="max-w-[280px] truncate">{displayName}</span>
                <Badge variant="outline">#{credential.id}</Badge>
                {credential.isCurrent && (
                  <Badge variant="success">当前</Badge>
                )}
                {credential.disabled && (
                  <Badge variant="destructive">已禁用</Badge>
                )}
                {credential.disabled && credential.disabledReason && (
                  <Badge variant="outline">{credential.disabledReason}</Badge>
                )}
                {!credential.disabled && credential.cooledDown && (
                  <Badge variant="outline">冷却 {credential.cooldownRemainingSecs}s</Badge>
                )}
                {!credential.disabled && credential.rateLimited && (
                  <Badge variant="outline">限流 {credential.rateLimitRemainingSecs}s</Badge>
                )}
                {!credential.disabled && credential.maxConcurrentRequests > 0 && (
                  <Badge
                    variant={credential.inFlightRequests >= credential.maxConcurrentRequests ? 'destructive' : 'outline'}
                    title={
                      credential.inFlightRequests > 0
                        ? `最老占用 ${credential.oldestInFlightAgeSecs}s，最近活跃 ${credential.newestInFlightIdleSecs}s 前`
                        : undefined
                    }
                  >
                    并发 {credential.inFlightRequests}/{credential.maxConcurrentRequests}
                  </Badge>
                )}
                {!credential.disabled && credential.warmupRemaining > 0 && (
                  <Badge variant="secondary">预热 {credential.warmupRemaining}</Badge>
                )}
                {credential.authMethod && (
                  <Badge variant="secondary">
                    {credential.authMethod === 'api_key' ? 'API Key' :
                     credential.authMethod === 'idc' ? 'IdC' :
                     credential.authMethod === 'social' ? 'Social' :
                     credential.authMethod}
                  </Badge>
                )}
                {credential.endpoint && (
                  <Badge variant="outline">{credential.endpoint}</Badge>
                )}
              </CardTitle>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted-foreground">启用</span>
              <Switch
                checked={!credential.disabled}
                onCheckedChange={handleToggleDisabled}
                disabled={setDisabled.isPending}
              />
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          {/* 信息网格 */}
          <div className="grid grid-cols-2 gap-4 text-sm">
            <div>
              <span className="text-muted-foreground">优先级：</span>
              {editingPriority ? (
                <div className="inline-flex items-center gap-1 ml-1">
                  <Input
                    type="number"
                    value={priorityValue}
                    onChange={(e) => setPriorityValue(e.target.value)}
                    className="w-16 h-7 text-sm"
                    min="0"
                  />
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 w-7 p-0"
                    onClick={handlePriorityChange}
                    disabled={setPriority.isPending}
                  >
                    ✓
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 w-7 p-0"
                    onClick={() => {
                      setEditingPriority(false)
                      setPriorityValue(String(credential.priority))
                    }}
                  >
                    ✕
                  </Button>
                </div>
              ) : (
                <span
                  className="font-medium cursor-pointer hover:underline ml-1"
                  onClick={() => setEditingPriority(true)}
                >
                  {credential.priority}
                  <span className="text-xs text-muted-foreground ml-1">(点击编辑)</span>
                </span>
              )}
            </div>
            <div>
              <span className="text-muted-foreground">失败次数：</span>
              <span className={credential.failureCount > 0 ? 'text-red-500 font-medium' : ''}>
                {credential.failureCount}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">刷新失败：</span>
              <span className={credential.refreshFailureCount > 0 ? 'text-red-500 font-medium' : ''}>
                {credential.refreshFailureCount}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">订阅等级：</span>
              <span className="font-medium">
                {loadingBalance ? (
                  <Loader2 className="inline w-3 h-3 animate-spin" />
                ) : subscriptionTitle}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">成功次数：</span>
              <span className="font-medium">{credential.successCount}</span>
            </div>
            <div>
              <span className="text-muted-foreground">当前并发：</span>
              <span className="font-medium">
                {credential.inFlightRequests}
                {credential.maxConcurrentRequests > 0 ? `/${credential.maxConcurrentRequests}` : '（不限）'}
              </span>
              {credential.inFlightRequests > 0 && (
                <span className="ml-1 text-xs text-muted-foreground">
                  最老 {credential.oldestInFlightAgeSecs}s
                  {credential.inFlightLeaseMaxSecs > 0 ? ` / 回收 ${credential.inFlightLeaseMaxSecs}s` : ''}
                </span>
              )}
            </div>
            <div>
              <span className="text-muted-foreground">本地估算成本：</span>
              <span className="font-medium">{formatUsd(credential.estimatedCostUsd || 0)}</span>
            </div>
            {(credential.pricedRequests > 0 || credential.unpricedRequests > 0) && (
              <div>
                <span className="text-muted-foreground">计价请求：</span>
                <span className="font-medium">
                  {credential.pricedRequests}/{credential.pricedRequests + credential.unpricedRequests}
                </span>
              </div>
            )}
            {(credential.cooledDown || credential.rateLimited || credential.warmupRemaining > 0 || (credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests)) && (
              <div className="col-span-2">
                <span className="text-muted-foreground">调度状态：</span>
                <span className="font-medium">
                  {credential.cooledDown
                    ? `冷却中 ${credential.cooldownRemainingSecs}s`
                    : credential.rateLimited
                      ? `本地限流 ${credential.rateLimitRemainingSecs}s`
                      : credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests
                        ? `并发已满 ${credential.inFlightRequests}/${credential.maxConcurrentRequests}`
                        : `预热剩余 ${credential.warmupRemaining} 次`}
                </span>
                {credential.cooldownReason && (
                  <span className="ml-1 text-xs text-muted-foreground">
                    {credential.cooldownReason}
                  </span>
                )}
              </div>
            )}
            <div className="col-span-2">
              <span className="text-muted-foreground">最后调用：</span>
              <span className="font-medium">{formatLastUsed(credential.lastUsedAt)}</span>
            </div>
            <div>
              <span className="text-muted-foreground">创建时间：</span>
              <span className="font-medium">{formatDateTime(credential.createdAt)}</span>
            </div>
            <div>
              <span className="text-muted-foreground">更新时间：</span>
              <span className="font-medium">{formatDateTime(credential.updatedAt)}</span>
            </div>
            {credential.email && (
              <div className="col-span-2">
                <span className="text-muted-foreground">邮箱：</span>
                <span className="font-medium">{credential.email}</span>
              </div>
            )}
            {credential.maskedApiKey && (
              <div className="col-span-2">
                <span className="text-muted-foreground">API Key：</span>
                <span className="font-mono font-medium">{credential.maskedApiKey}</span>
              </div>
            )}
            <div className="col-span-2">
              <span className="text-muted-foreground">额度：</span>
              {loadingBalance ? (
                <span className="text-sm ml-1">
                  <Loader2 className="inline w-3 h-3 animate-spin" /> 加载中...
                </span>
              ) : accountInfo ? (
                <span className="font-medium ml-1">
                  {formatQuota(accountInfo.currentUsage)}/{formatQuota(accountInfo.usageLimit)}
                  <span className="text-xs text-muted-foreground ml-1">
                    {formatDateTime(accountInfo.checkedAt)}
                    {accountInfo.nextResetAt ? ` · 重置 ${new Date(accountInfo.nextResetAt * 1000).toLocaleString('zh-CN', { hour12: false })}` : ''}
                  </span>
                </span>
              ) : (
                <span className="text-sm text-muted-foreground ml-1">未知</span>
              )}
            </div>
            {credential.hasProxy && (
              <div className="col-span-2">
                <span className="text-muted-foreground">代理：</span>
                <span className="font-medium">{credential.proxyUrl}</span>
              </div>
            )}
            {credential.hasProfileArn && (
              <div className="col-span-2">
                <Badge variant="secondary">有 Profile ARN</Badge>
              </div>
            )}
          </div>

          {/* 操作按钮 */}
          <div className="flex flex-wrap gap-2 pt-2 border-t">
            <Button
              size="sm"
              variant="outline"
              onClick={handleReset}
              disabled={resetFailure.isPending || (credential.failureCount === 0 && credential.refreshFailureCount === 0)}
            >
              <RefreshCw className="h-4 w-4 mr-1" />
              重置失败
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={handleForceRefresh}
              disabled={forceRefresh.isPending || credential.disabled || credential.authMethod === 'api_key'}
              title={credential.authMethod === 'api_key' ? 'API Key 凭据无需刷新 Token' : credential.disabled ? '已禁用的凭据无法刷新 Token' : '强制刷新 Token'}
            >
              <RefreshCw className={`h-4 w-4 mr-1 ${forceRefresh.isPending ? 'animate-spin' : ''}`} />
              刷新 Token
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={handleToggleWarmup}
              disabled={setWarmup.isPending || credential.disabled}
              title={credential.disabled ? '已禁用的凭据无法调整预热' : undefined}
            >
              {credential.warmupRemaining > 0 ? '关闭预热' : `预热 ${warmupTarget} 次`}
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={handleClearInFlight}
              disabled={clearInFlight.isPending || credential.inFlightRequests === 0}
              title={credential.inFlightRequests === 0 ? '当前没有并发占用' : '清理异常并发占用'}
            >
              清理并发
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                const newPriority = Math.max(0, credential.priority - 1)
                setPriority.mutate(
                  { id: credential.id, priority: newPriority },
                  {
                    onSuccess: (res) => toast.success(res.message),
                    onError: (err) => toast.error('操作失败: ' + extractErrorMessage(err)),
                  }
                )
              }}
              disabled={setPriority.isPending || credential.priority === 0}
            >
              <ChevronUp className="h-4 w-4 mr-1" />
              提高优先级
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                const newPriority = credential.priority + 1
                setPriority.mutate(
                  { id: credential.id, priority: newPriority },
                  {
                    onSuccess: (res) => toast.success(res.message),
                    onError: (err) => toast.error('操作失败: ' + extractErrorMessage(err)),
                  }
                )
              }}
              disabled={setPriority.isPending}
            >
              <ChevronDown className="h-4 w-4 mr-1" />
              降低优先级
            </Button>
            <Button
              size="sm"
              variant="default"
              onClick={() => onTestCredential(credential)}
              title="测试模型调用"
            >
              <PlayCircle className="h-4 w-4 mr-1" />
              测试
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => onQueryBalance(credential.id)}
              disabled={loadingBalance || credential.disabled}
              title={credential.disabled ? '已禁用的凭据无法查询额度' : '查询额度并更新卡片'}
            >
              {loadingBalance ? <Loader2 className="h-4 w-4 mr-1 animate-spin" /> : <Wallet className="h-4 w-4 mr-1" />}
              {loadingBalance ? '查询中' : '查询额度'}
            </Button>
            <Button
              size="sm"
              variant="destructive"
              onClick={() => setShowDeleteDialog(true)}
              disabled={!credential.disabled}
              title={!credential.disabled ? '需要先禁用凭据才能删除' : undefined}
            >
              <Trash2 className="h-4 w-4 mr-1" />
              删除
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* 删除确认对话框 */}
      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除凭据</DialogTitle>
            <DialogDescription>
              您确定要删除凭据 #{credential.id} 吗？此操作无法撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
              disabled={deleteCredential.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteCredential.isPending || !credential.disabled}
            >
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
