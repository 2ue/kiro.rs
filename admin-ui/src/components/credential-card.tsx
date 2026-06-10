import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { RefreshCw, ChevronUp, ChevronDown, Wallet, Trash2, Loader2, PlayCircle, Router, Gauge, Eye, EyeOff } from 'lucide-react'
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
  useProxyResources,
  useRuntimeConfig,
  useSetCredentialProxy,
  useSetCredentialConcurrency,
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

function formatApproxElapsedMs(value?: number | null): string | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) return null
  const diff = Date.now() - value
  if (diff < 0) return '约刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `约${seconds}秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `约${minutes}分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `约${hours}小时前`
  const days = Math.floor(hours / 24)
  return `约${days}天前`
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

function numberOrZero(value: number | null | undefined): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}

function sourceLabel(source?: CredentialStatusItem['effectiveProxySource']) {
  if (source === 'credential') return '直接代理'
  if (source === 'resource') return '代理资源'
  if (source === 'resource_disabled') return '代理资源已禁用'
  if (source === 'resource_missing') return '代理资源不存在'
  if (source === 'global') return '全局代理'
  if (source === 'direct') return '直连'
  return '未配置代理'
}

function proxySummary(credential: CredentialStatusItem): string {
  const label = sourceLabel(credential.effectiveProxySource)
  if (
    credential.proxyResourceName &&
    (credential.effectiveProxySource === 'resource' ||
      credential.effectiveProxySource === 'resource_disabled' ||
      credential.effectiveProxySource === 'resource_missing')
  ) {
    return `${label}：${credential.proxyResourceName}`
  }
  return label
}

function concurrencyLimitLabel(credential: CredentialStatusItem): string {
  const effective = credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests}` : '不限'
  if (typeof credential.maxConcurrentRequestsOverride === 'number') {
    return credential.maxConcurrentRequestsOverride > 0
      ? `账号覆盖：${credential.maxConcurrentRequestsOverride}`
      : '账号覆盖：不限'
  }
  return `继承全局：${effective}`
}

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
  const [editingProxy, setEditingProxy] = useState(false)
  const [editingConcurrency, setEditingConcurrency] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [proxyResourceId, setProxyResourceId] = useState(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
  const [proxyUrl, setProxyUrl] = useState(credential.proxyUrl || '')
  const [proxyUsername, setProxyUsername] = useState(credential.proxyUsername || '')
  const [proxyPassword, setProxyPassword] = useState(credential.proxyPassword || '')
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const [concurrencyValue, setConcurrencyValue] = useState(
    typeof credential.maxConcurrentRequestsOverride === 'number'
      ? String(credential.maxConcurrentRequestsOverride)
      : ''
  )
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const resetFailure = useResetFailure()
  const deleteCredential = useDeleteCredential()
  const forceRefresh = useForceRefreshToken()
  const setWarmup = useSetWarmup()
  const clearInFlight = useClearInFlight()
  const setCredentialProxy = useSetCredentialProxy()
  const setCredentialConcurrency = useSetCredentialConcurrency()
  const proxyResources = useProxyResources()
  const runtimeConfig = useRuntimeConfig()
  const proxyResourceOptions = proxyResources.data?.resources || []
  const displayName = credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
  const warmupTarget = Math.max(0, runtimeConfig.data?.credentialWarmupRequests ?? 3)
  const accountInfo = balance || credential.accountInfo
  const subscriptionTitle = balance?.subscriptionTitle || credential.accountInfo?.subscriptionTitle || credential.subscriptionTitle || '未知'
  const transientFailureStreak = numberOrZero(credential.transientFailureStreak)
  const probationRemainingSecs = numberOrZero(credential.probationRemainingSecs)
  const recentErrorRate = numberOrZero(credential.recentErrorRate)
  const schedulerScore = numberOrZero(credential.schedulerScore)
  const schedulerSelectionCount = numberOrZero(credential.schedulerSelectionCount)
  const recentSelection10s = numberOrZero(credential.recentSchedulerSelectionCount10s)
  const recentSelection60s = numberOrZero(credential.recentSchedulerSelectionCount60s)
  const recentSelection5m = numberOrZero(credential.recentSchedulerSelectionCount5m)
  const schedulerSelectionPressure = numberOrZero(credential.schedulerSelectionPressure)
  const lastTransientErrorAgo = formatApproxElapsedMs(credential.lastErrorAtMs)

  const resetProxyDraft = () => {
    setProxyResourceId(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
    setProxyUrl(credential.proxyUrl || '')
    setProxyUsername(credential.proxyUsername || '')
    setProxyPassword(credential.proxyPassword || '')
    setShowProxyUsername(false)
    setShowProxyPassword(false)
  }

  const openProxyEditor = () => {
    resetProxyDraft()
    setEditingProxy(true)
  }

  const closeProxyEditor = () => {
    if (setCredentialProxy.isPending) return
    resetProxyDraft()
    setEditingProxy(false)
  }

  useEffect(() => {
    setPriorityValue(String(credential.priority))
  }, [credential.priority])

  useEffect(() => {
    resetProxyDraft()
  }, [credential.id, credential.proxyResourceId, credential.proxyUrl, credential.proxyUsername, credential.proxyPassword])

  useEffect(() => {
    setConcurrencyValue(
      typeof credential.maxConcurrentRequestsOverride === 'number'
        ? String(credential.maxConcurrentRequestsOverride)
        : ''
    )
  }, [credential.id, credential.maxConcurrentRequestsOverride])

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

  const handleProxySave = () => {
    const directProxyUrl = proxyUrl.trim()
    const directProxyUsername = proxyUsername.trim()
    const directProxyPassword = proxyPassword.trim()
    if (!proxyResourceId && !directProxyUrl && (directProxyUsername || directProxyPassword)) {
      toast.error('直接代理 URL 为空时不能单独保存代理账号或密码')
      return
    }
    setCredentialProxy.mutate(
      {
        id: credential.id,
        request: {
          proxyResourceId: proxyResourceId ? Number(proxyResourceId) : null,
          proxyUrl: proxyResourceId ? undefined : directProxyUrl || undefined,
          proxyUsername: proxyResourceId ? undefined : directProxyUsername || undefined,
          proxyPassword: proxyResourceId ? undefined : directProxyPassword || undefined,
        },
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingProxy(false)
        },
        onError: (err) => {
          toast.error('代理设置失败: ' + extractErrorMessage(err))
        },
      }
    )
  }

  const handleConcurrencySave = () => {
    const trimmed = concurrencyValue.trim()
    let maxConcurrentRequests: number | null = null
    if (trimmed) {
      const parsed = Number(trimmed)
      if (!Number.isInteger(parsed) || parsed < 0) {
        toast.error('账号并发限制必须是非负整数')
        return
      }
      maxConcurrentRequests = parsed
    }
    setCredentialConcurrency.mutate(
      {
        id: credential.id,
        request: { maxConcurrentRequests },
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingConcurrency(false)
        },
        onError: (err) => {
          toast.error('并发限制设置失败: ' + extractErrorMessage(err))
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
                  <Badge
                    variant="outline"
                    title={(credential.cooldowns || [])
                      .map((item) => `${item.global ? '全部模型' : item.model || '-'} ${item.remainingSecs}s${item.reason ? ` ${item.reason}` : ''}`)
                      .join('\n')}
                  >
                    冷却 {credential.cooldownRemainingSecs}s
                  </Badge>
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
                {!credential.disabled && credential.inProbation && (
                  <Badge variant="secondary">观察 {probationRemainingSecs}s</Badge>
                )}
                {transientFailureStreak > 0 && (
                  <Badge variant="outline">瞬态错误 {transientFailureStreak}</Badge>
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
                {credential.hasProxy && (
                  <Badge variant="outline">代理</Badge>
                )}
                {typeof credential.maxConcurrentRequestsOverride === 'number' && (
                  <Badge variant="outline">并发覆盖</Badge>
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
              <span className="text-muted-foreground">并发限制：</span>
              <button
                type="button"
                className="ml-1 inline-flex items-center gap-1 font-medium hover:underline"
                onClick={() => setEditingConcurrency(true)}
              >
                <Gauge className="h-3.5 w-3.5" />
                {concurrencyLimitLabel(credential)}
              </button>
            </div>
            <div>
              <span className="text-muted-foreground">近期错误率：</span>
              <span className={recentErrorRate > 0 ? 'font-medium text-red-500' : 'font-medium'}>
                {(recentErrorRate * 100).toFixed(1)}%
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">耗时 EWMA：</span>
              <span className="font-medium">
                {credential.latencyEwmaMs == null ? '未知' : `${Math.round(credential.latencyEwmaMs)}ms`}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">调度评分：</span>
              <span className="font-medium">{schedulerScore.toFixed(2)}</span>
            </div>
            <div>
              <span className="text-muted-foreground">总调度：</span>
              <span className="font-medium">{schedulerSelectionCount}</span>
            </div>
            <div>
              <span className="text-muted-foreground">近期调度：</span>
              <span className="font-medium">
                {recentSelection60s}/60s
              </span>
              <span className="ml-1 text-xs text-muted-foreground">
                10s {recentSelection10s} / 5m {recentSelection5m}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">调度压力：</span>
              <span className={schedulerSelectionPressure > 1 ? 'font-medium text-amber-600' : 'font-medium'}>
                {schedulerSelectionPressure.toFixed(2)}
              </span>
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
            {(credential.cooledDown || credential.rateLimited || credential.inProbation || credential.warmupRemaining > 0 || (credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests)) && (
              <div className="col-span-2">
                <span className="text-muted-foreground">调度状态：</span>
                <span className="font-medium">
                  {credential.cooledDown
                    ? `冷却中 ${credential.cooldownRemainingSecs}s`
                    : credential.rateLimited
                      ? `本地限流 ${credential.rateLimitRemainingSecs}s`
                      : credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests
                        ? `并发已满 ${credential.inFlightRequests}/${credential.maxConcurrentRequests}`
                        : credential.inProbation
                          ? `恢复观察 ${probationRemainingSecs}s`
                        : `预热剩余 ${credential.warmupRemaining} 次`}
                </span>
                {credential.cooldownReason && (
                  <span className="ml-1 text-xs text-muted-foreground">
                    {credential.cooldownReason}
                  </span>
                )}
                {credential.cooldowns && credential.cooldowns.length > 0 && (
                  <div className="mt-1 flex flex-wrap gap-1 text-xs text-muted-foreground">
                    {credential.cooldowns.map((cooldown) => (
                      <span
                        key={`${cooldown.global ? 'global' : cooldown.model}-${cooldown.remainingSecs}`}
                        className="rounded border px-1.5 py-0.5"
                        title={cooldown.reason || undefined}
                      >
                        {cooldown.global ? '全部模型' : cooldown.model || '-'} · {cooldown.remainingSecs}s
                      </span>
                    ))}
                  </div>
                )}
              </div>
            )}
            {credential.lastErrorReason && (
              <div className="col-span-2">
                <span className="text-muted-foreground">
                  最近瞬态错误{lastTransientErrorAgo ? `(${lastTransientErrorAgo})` : ''}：
                </span>
                <span className="font-medium">{credential.lastErrorKind || 'unknown'}</span>
                <span className="ml-1 text-xs text-muted-foreground">{credential.lastErrorReason}</span>
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
            <div className="col-span-2">
              <span className="text-muted-foreground">代理：</span>
              <button
                type="button"
                className="ml-1 inline-flex max-w-full items-center gap-1 truncate font-medium hover:underline"
                onClick={openProxyEditor}
                title="配置该凭据的代理"
              >
                <Router className="h-3.5 w-3.5 shrink-0" />
                <span className="truncate">{proxySummary(credential)}</span>
              </button>
            </div>
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
              disabled={forceRefresh.isPending || credential.authMethod === 'api_key'}
              title={credential.authMethod === 'api_key' ? 'API Key 凭据无需刷新 Token' : '强制刷新 Token'}
            >
              <RefreshCw className={`h-4 w-4 mr-1 ${forceRefresh.isPending ? 'animate-spin' : ''}`} />
              刷新 Token
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={handleToggleWarmup}
              disabled={setWarmup.isPending}
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
              disabled={loadingBalance}
              title="查询订阅、额度和用量并更新卡片"
            >
              {loadingBalance ? <Loader2 className="h-4 w-4 mr-1 animate-spin" /> : <Wallet className="h-4 w-4 mr-1" />}
              {loadingBalance ? '查询中' : '查询信息'}
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

      <Dialog
        open={editingProxy}
        onOpenChange={(open) => {
          if (open) {
            openProxyEditor()
          } else {
            closeProxyEditor()
          }
        }}
      >
        <DialogContent className="max-h-[85vh] max-w-2xl overflow-y-auto">
          <DialogHeader>
            <DialogTitle>绑定代理：{displayName}</DialogTitle>
            <DialogDescription>
              选择代理资源会使用资源里的代理 URL/账号/密码；不绑定资源时，可以为该凭据单独配置直接代理。
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-3">
            <button
              type="button"
              className={`w-full rounded-md border p-3 text-left text-sm transition ${proxyResourceId ? 'hover:bg-muted/50' : 'border-primary bg-primary/5'}`}
              onClick={() => setProxyResourceId('')}
            >
              <div className="flex items-center justify-between gap-3">
                <span className="font-medium">不绑定代理资源</span>
                {!proxyResourceId && <Badge variant="default">已选</Badge>}
              </div>
              <div className="mt-1 text-xs text-muted-foreground">使用全局代理或直连配置。</div>
            </button>

            <div className={`rounded-md border p-3 ${proxyResourceId ? 'bg-muted/30 opacity-70' : 'bg-background'}`}>
              <div className="mb-3">
                <div className="text-sm font-medium">凭据直连代理</div>
                <div className="mt-1 text-xs leading-5 text-muted-foreground">
                  不绑定代理资源时生效；选择代理资源保存后会清除这些直连代理字段。
                </div>
              </div>
              <div className="grid gap-3 md:grid-cols-2">
                <label className="block md:col-span-2">
                  <span className="text-sm font-medium">代理 URL</span>
                  <Input
                    className="mt-2"
                    value={proxyUrl}
                    placeholder="socks5h://127.0.0.1:1080"
                    disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)}
                    onChange={(event) => setProxyUrl(event.target.value)}
                  />
                </label>
                <label className="block">
                  <span className="text-sm font-medium">代理用户名</span>
                  <div className="mt-2">
                    <SecretInput
                      value={proxyUsername}
                      onChange={setProxyUsername}
                      visible={showProxyUsername}
                      onToggle={() => setShowProxyUsername((value) => !value)}
                      disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)}
                      placeholder="可选"
                    />
                  </div>
                </label>
                <label className="block">
                  <span className="text-sm font-medium">代理密码</span>
                  <div className="mt-2">
                    <SecretInput
                      value={proxyPassword}
                      onChange={setProxyPassword}
                      visible={showProxyPassword}
                      onToggle={() => setShowProxyPassword((value) => !value)}
                      disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)}
                      placeholder="可选"
                    />
                  </div>
                </label>
              </div>
            </div>

            {proxyResources.isLoading ? (
              <div className="py-8 text-center text-sm text-muted-foreground">
                <div className="mx-auto mb-3 h-8 w-8 animate-spin rounded-full border-b-2 border-primary" />
                加载代理资源...
              </div>
            ) : proxyResourceOptions.length === 0 ? (
              <Card>
                <CardContent className="py-8 text-center text-sm text-muted-foreground">暂无代理资源，请先在代理页新增</CardContent>
              </Card>
            ) : (
              <div className="max-h-80 space-y-2 overflow-y-auto">
                {proxyResourceOptions.map((resource) => {
                  const selected = proxyResourceId === String(resource.id)
                  return (
                    <button
                      key={resource.id}
                      type="button"
                      className={`w-full rounded-md border p-3 text-left text-sm transition ${
                        selected ? 'border-primary bg-primary/5' : resource.enabled ? 'hover:bg-muted/50' : 'border-red-200 bg-red-50 opacity-80 hover:bg-red-100 dark:border-red-900 dark:bg-red-950/30'
                      }`}
                      onClick={() => setProxyResourceId(String(resource.id))}
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-medium">{resource.name}</span>
                        <Badge variant="outline">#{resource.id}</Badge>
                        <Badge variant={resource.enabled ? 'success' : 'destructive'}>{resource.enabled ? '启用' : '已禁用'}</Badge>
                        {selected && <Badge variant="default">已选</Badge>}
                      </div>
                      <div className="mt-1 truncate text-xs text-muted-foreground">{resource.proxyUrl}</div>
                    </button>
                  )
                })}
              </div>
            )}
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={closeProxyEditor} disabled={setCredentialProxy.isPending}>
              取消
            </Button>
            <Button onClick={handleProxySave} disabled={setCredentialProxy.isPending}>
              {setCredentialProxy.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              保存绑定
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={editingConcurrency} onOpenChange={setEditingConcurrency}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>并发限制：{displayName}</DialogTitle>
            <DialogDescription>
              留空表示继承全局“单凭据最大并发请求数”；填 0 表示该账号不限并发；填正整数表示该账号自己的并发上限。
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-3">
            <div className="rounded-md border bg-muted/30 p-3 text-sm">
              <div className="flex items-center justify-between gap-3">
                <span className="text-muted-foreground">当前生效</span>
                <span className="font-medium">
                  {credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests} 并发` : '不限并发'}
                </span>
              </div>
              <div className="mt-1 text-xs text-muted-foreground">
                {typeof credential.maxConcurrentRequestsOverride === 'number'
                  ? '当前账号已覆盖全局配置。'
                  : '当前账号继承全局配置。'}
              </div>
            </div>
            <label className="block">
              <span className="text-sm font-medium">账号级最大并发</span>
              <Input
                className="mt-2"
                type="number"
                min="0"
                value={concurrencyValue}
                placeholder="留空继承全局，0 表示不限"
                disabled={setCredentialConcurrency.isPending}
                onChange={(event) => setConcurrencyValue(event.target.value)}
              />
            </label>
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={() => setEditingConcurrency(false)} disabled={setCredentialConcurrency.isPending}>
              取消
            </Button>
            <Button
              variant="outline"
              onClick={() => {
                setConcurrencyValue('')
                setCredentialConcurrency.mutate(
                  { id: credential.id, request: { maxConcurrentRequests: null } },
                  {
                    onSuccess: (res) => {
                      toast.success(res.message)
                      setEditingConcurrency(false)
                    },
                    onError: (err) => toast.error('并发限制设置失败: ' + extractErrorMessage(err)),
                  }
                )
              }}
              disabled={setCredentialConcurrency.isPending || typeof credential.maxConcurrentRequestsOverride !== 'number'}
            >
              继承全局
            </Button>
            <Button onClick={handleConcurrencySave} disabled={setCredentialConcurrency.isPending}>
              {setCredentialConcurrency.isPending && <Loader2 className="h-4 w-4 animate-spin" />}
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

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
