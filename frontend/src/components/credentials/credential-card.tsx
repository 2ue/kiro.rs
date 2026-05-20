import { useState } from 'react'
import { toast } from 'sonner'
import {
  CheckCircle2,
  Loader2,
  PlayCircle,
  RefreshCw,
  Trash2,
  Wallet,
} from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Progress } from '@/components/ui/progress'
import { Switch } from '@/components/ui/switch'
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import {
  useDeleteCredential,
  useForceRefreshToken,
  useResetFailure,
  useSetDisabled,
} from '@/hooks/use-credentials'
import {
  extractErrorMessage,
  formatRelative,
  formatUsd,
} from '@/lib/utils'
import { findPriceForModel } from '@/lib/pricing'
import type {
  BalanceResponse,
  CredentialStatusItem,
  ModelPrice,
} from '@/types/api'
import { CredentialTestDialog } from './credential-test-dialog'

interface CredentialCardProps {
  credential: CredentialStatusItem
  selected: boolean
  onToggleSelect: () => void
  onViewBalance: (id: number) => void
  balance: BalanceResponse | null
  loadingBalance: boolean
  pricing: ModelPrice[] | undefined
  todayCostUsd: number
  totalCostUsd: number
  todayTokens: number
  totalTokens: number
}

function authMethodLabel(method: string | null) {
  switch (method) {
    case 'social':
      return 'Social'
    case 'idc':
      return 'IdC'
    case 'api_key':
      return 'API Key'
    default:
      return method ?? '未知'
  }
}

function disabledReasonLabel(reason: string | undefined) {
  switch (reason) {
    case 'Manual':
      return '手动停用'
    case 'TooManyFailures':
      return '失败次数过多'
    case 'TooManyRefreshFailures':
      return '刷新失败过多'
    case 'QuotaExceeded':
      return '配额耗尽'
    case 'InvalidRefreshToken':
      return '令牌失效'
    case 'InvalidConfig':
      return '配置错误'
    default:
      return reason ?? null
  }
}

function schedulingStatusMeta(credential: CredentialStatusItem) {
  const until = credential.schedulingUntil
    ? `至 ${formatRelative(credential.schedulingUntil)}`
    : undefined
  switch (credential.schedulingStatus) {
    case 'rate_limited':
      return {
        label: '429 冷却',
        title: [credential.schedulingReason, until].filter(Boolean).join(' · '),
        variant: 'warning' as const,
      }
    case 'quota_cooldown':
      return {
        label: '配额冷却',
        title: [credential.schedulingReason, until].filter(Boolean).join(' · '),
        variant: 'warning' as const,
      }
    case 'temp_unschedulable':
      return {
        label: '临时不可调度',
        title: [credential.schedulingReason, until].filter(Boolean).join(' · '),
        variant: 'warning' as const,
      }
    case 'manual_recovery_required':
      return {
        label: '需人工恢复',
        title: credential.schedulingReason,
        variant: 'destructive' as const,
      }
    default:
      return null
  }
}

export function CredentialCard({
  credential,
  selected,
  onToggleSelect,
  onViewBalance,
  balance,
  loadingBalance,
  pricing,
  todayCostUsd,
  totalCostUsd,
  todayTokens,
  totalTokens,
}: CredentialCardProps) {
  const [busy, setBusy] = useState(false)
  const [testOpen, setTestOpen] = useState(false)

  const setDisabled = useSetDisabled()
  const resetFailure = useResetFailure()
  const refreshToken = useForceRefreshToken()
  const deleteCred = useDeleteCredential()

  const displayName =
    credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
  const reasonLabel = disabledReasonLabel(credential.disabledReason)
  const schedulingMeta = schedulingStatusMeta(credential)
  const remainingPercent = balance
    ? Math.max(0, Math.min(100, 100 - balance.usagePercentage))
    : null

  const samplePrice = pricing && pricing.length > 0
    ? findPriceForModel('claude-opus-4-7', pricing) ?? pricing[0]
    : null

  const handleToggle = (checked: boolean) => {
    setBusy(true)
    setDisabled.mutate(
      { id: credential.id, disabled: !checked },
      {
        onSuccess: (res) => toast.success(res.message),
        onError: (err) => toast.error(extractErrorMessage(err)),
        onSettled: () => setBusy(false),
      },
    )
  }

  const handleReset = () => {
    resetFailure.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error(extractErrorMessage(err)),
    })
  }

  const handleRefresh = () => {
    refreshToken.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error(extractErrorMessage(err)),
    })
  }

  const handleDelete = () => {
    if (!credential.disabled) {
      toast.error('删除前请先停用')
      return
    }
    if (!confirm(`确认删除凭据 #${credential.id}?此操作无法撤销。`)) return
    deleteCred.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error(extractErrorMessage(err)),
    })
  }

  return (
    <Card
      className={
        credential.isCurrent
          ? 'ring-2 ring-primary/60'
          : credential.disabled
            ? 'opacity-60'
            : ''
      }
    >
      <CardHeader className="space-y-2 pb-2">
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-center gap-2">
            <Checkbox
              checked={selected}
              onCheckedChange={onToggleSelect}
              aria-label="选择凭据"
            />
            <div className="min-w-0">
              <CardTitle
                className="truncate text-base font-semibold"
                title={displayName}
              >
                {displayName}
              </CardTitle>
              <div className="mt-1 flex flex-wrap items-center gap-1.5 text-xs">
                <Badge variant="outline">#{credential.id}</Badge>
                {credential.isCurrent && (
                  <Badge variant="success">使用中</Badge>
                )}
                <Badge variant="secondary">
                  {authMethodLabel(credential.authMethod)}
                </Badge>
                {credential.endpoint && (
                  <Badge variant="outline">{credential.endpoint}</Badge>
                )}
                {credential.disabled && (
                  <Badge variant="destructive">已停用</Badge>
                )}
                {reasonLabel && (
                  <Badge variant="warning">{reasonLabel}</Badge>
                )}
                {schedulingMeta && (
                  <Badge variant={schedulingMeta.variant} title={schedulingMeta.title}>
                    {schedulingMeta.label}
                  </Badge>
                )}
              </div>
            </div>
          </div>
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>启用</span>
            <Switch
              checked={!credential.disabled}
              onCheckedChange={handleToggle}
              disabled={busy}
            />
          </div>
        </div>
      </CardHeader>

      <CardContent className="space-y-3 text-sm">
        {/* 用量进度条 */}
        <div className="space-y-1">
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <span>剩余额度</span>
            <span className="font-medium tabular-nums text-foreground">
              {loadingBalance ? (
                <Loader2 className="inline h-3 w-3 animate-spin" />
              ) : balance ? (
                `${balance.remaining.toFixed(1)} / ${balance.usageLimit.toFixed(1)}`
              ) : (
                '未查询'
              )}
            </span>
          </div>
          <Progress value={remainingPercent ?? 0} />
          <div className="flex items-center justify-between text-xs text-muted-foreground">
            <span>{balance?.subscriptionTitle ?? '订阅未知'}</span>
            <span>
              {balance ? `${balance.usagePercentage.toFixed(1)}% 已用` : ''}
            </span>
          </div>
        </div>

        {/* 4 项关键指标 */}
        <div className="grid grid-cols-2 gap-2 rounded-md border bg-muted/30 p-2 text-xs">
          <div>
            <div className="text-muted-foreground">今日 Token</div>
            <div className="font-semibold tabular-nums">
              {todayTokens.toLocaleString('zh-CN')}
            </div>
          </div>
          <div>
            <div className="text-muted-foreground">今日花费</div>
            <div className="font-semibold tabular-nums">
              {todayCostUsd ? formatUsd(todayCostUsd, 4) : '—'}
            </div>
          </div>
          <div>
            <div className="text-muted-foreground">累计 Token</div>
            <div className="font-semibold tabular-nums">
              {totalTokens.toLocaleString('zh-CN')}
            </div>
          </div>
          <div>
            <div className="text-muted-foreground">累计花费</div>
            <div className="font-semibold tabular-nums">
              {totalCostUsd ? formatUsd(totalCostUsd, 4) : '—'}
            </div>
          </div>
        </div>

        {/* 健康状态 */}
        <div className="grid grid-cols-3 gap-2 text-xs">
          <Tooltip>
            <TooltipTrigger asChild>
              <div>
                <div className="text-muted-foreground">优先级</div>
                <div className="font-medium">{credential.priority}</div>
              </div>
            </TooltipTrigger>
            <TooltipContent>数字越小,越优先被使用</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <div>
                <div className="text-muted-foreground">成功</div>
                <div className="font-medium">
                  {credential.successCount.toLocaleString('zh-CN')}
                </div>
              </div>
            </TooltipTrigger>
            <TooltipContent>API 累计成功次数</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <div>
                <div className="text-muted-foreground">失败</div>
                <div
                  className={
                    credential.failureCount > 0
                      ? 'font-medium text-destructive'
                      : 'font-medium'
                  }
                >
                  {credential.failureCount}
                </div>
              </div>
            </TooltipTrigger>
            <TooltipContent>连续失败次数</TooltipContent>
          </Tooltip>
        </div>

        <div className="text-xs text-muted-foreground">
          上次调用 · {formatRelative(credential.lastUsedAt)}
          {samplePrice && pricing && (
            <span className="ml-2 opacity-70">
              · 计价:{samplePrice.modelId}
            </span>
          )}
        </div>

        {/* 操作按钮 */}
        <div className="flex flex-wrap gap-2 border-t pt-2">
          <Button
            size="sm"
            variant="outline"
            onClick={() => onViewBalance(credential.id)}
          >
            <Wallet className="h-4 w-4" />
            查询余额
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => setTestOpen(true)}
          >
            <PlayCircle className="h-4 w-4" />
            测试
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={handleReset}
            disabled={
              resetFailure.isPending ||
              (credential.failureCount === 0 &&
                credential.refreshFailureCount === 0)
            }
          >
            <CheckCircle2 className="h-4 w-4" />
            重置失败
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={handleRefresh}
            disabled={
              refreshToken.isPending ||
              credential.disabled ||
              credential.authMethod === 'api_key'
            }
            title={
              credential.authMethod === 'api_key'
                ? 'API Key 无需刷新'
                : credential.disabled
                  ? '已停用,无法刷新'
                  : '强制刷新 Token'
            }
          >
            <RefreshCw
              className={`h-4 w-4 ${refreshToken.isPending ? 'animate-spin' : ''}`}
            />
            刷新令牌
          </Button>
          {credential.disabled && (
            <Button
              size="sm"
              variant="ghost"
              className="text-destructive"
              onClick={handleDelete}
              disabled={deleteCred.isPending}
            >
              <Trash2 className="h-4 w-4" />
              删除
            </Button>
          )}
        </div>
      </CardContent>
      <CredentialTestDialog
        credential={credential}
        open={testOpen}
        onOpenChange={setTestOpen}
      />
    </Card>
  )
}
