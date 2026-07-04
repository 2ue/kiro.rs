import {
  ChevronDown,
  ChevronUp,
  MoreHorizontal,
  RefreshCw,
  RotateCcw,
  Router,
  Trash2,
  Wallet,
  Wand2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import {
  Badge,
  Button,
  Checkbox,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Spinner,
  Switch,
} from '@/components/ui'
import { EmptyState, Field, FieldGrid, LoadingState, ModalShell, useConfirm } from '@/components/patterns'
import { ProgressRing } from '@/components/charts'
import {
  formatApproxElapsedMs,
  formatCompact,
  formatCredits,
  formatFullDate,
  formatLastUsed,
  formatMeteringUsage,
  formatNumber,
  formatQuota,
  formatUsd,
} from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useClearInFlight,
  useDeleteCredential,
  useForceRefreshToken,
  useProxyResources,
  useResetFailure,
  useRuntimeConfig,
  useSetCredentialConcurrency,
  useSetCredentialProxy,
  useSetCredentialRegions,
  useSetCredentialRpm,
  useSetDisabled,
  useSetPriority,
  useSetWarmup,
} from '@/hooks/use-credentials'
import type { BalanceResponse, CredentialStatusItem } from '@/types/api'
import {
  accountInfoValue,
  authLabel,
  credentialLabel,
  dispatchStatusLabel,
  endpointLabel,
  formatResetAt,
  numberOrZero,
  proxySummary,
  subscriptionBadgeMeta,
} from './credential-utils'
import { SecretInput } from './credential-inputs'

// ============================================================================
// Sub-components
// ============================================================================

function SummaryCell({ label, value, error, onClick }: { label: string; value: React.ReactNode; error?: boolean; onClick?: () => void }) {
  const content = (
    <>
      <span className="text-[0.68rem] font-medium text-muted-foreground uppercase tracking-wide truncate">{label}</span>
      <span className={`text-xs font-semibold tabular truncate ${error ? 'text-destructive' : onClick ? 'text-primary' : 'text-foreground'}`}>
        {value}
      </span>
    </>
  )
  if (onClick) {
    return (
      <button type="button" onClick={onClick} title={`修改${label}`}
        className="flex flex-col gap-0.5 min-w-0 items-start rounded px-1 -mx-1 text-left transition-colors hover:bg-muted/60">
        {content}
      </button>
    )
  }
  return <div className="flex flex-col gap-0.5 min-w-0">{content}</div>
}

function MetaItem({ label, value, detail, error }: {
  label: string; value: React.ReactNode; detail?: React.ReactNode; error?: boolean
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[0.68rem] font-medium text-muted-foreground">{label}</span>
      <span className={`text-sm font-semibold truncate ${error ? 'text-destructive' : ''}`}>{value}</span>
      {detail && <span className="text-xs text-muted-foreground truncate">{detail}</span>}
    </div>
  )
}

// ============================================================================
// CredentialCard exports
// ============================================================================

export interface CredentialCardProps {
  credential: CredentialStatusItem
  selected: boolean
  onToggleSelect: () => void
  onQueryBalance: (id: number) => void
  onTest: (credential: CredentialStatusItem) => void
  balance?: BalanceResponse
  loadingBalance: boolean
  expanded: boolean
}

export function CredentialCard({
  credential, selected, onToggleSelect, onQueryBalance, onTest, balance, loadingBalance, expanded,
}: CredentialCardProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [editingProxy, setEditingProxy] = useState(false)
  const [editingConcurrency, setEditingConcurrency] = useState(false)
  const [editingRpm, setEditingRpm] = useState(false)
  const [editingRegions, setEditingRegions] = useState(false)
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)

  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [concurrencyValue, setConcurrencyValue] = useState(
    typeof credential.maxConcurrentRequestsOverride === 'number' ? String(credential.maxConcurrentRequestsOverride) : ''
  )
  const [rpmValue, setRpmValue] = useState(
    typeof credential.rpmOverride === 'number' ? String(credential.rpmOverride) : ''
  )
  const [regionValue, setRegionValue] = useState(credential.region || '')
  const [authRegionValue, setAuthRegionValue] = useState(credential.authRegion || '')
  const [apiRegionValue, setApiRegionValue] = useState(credential.apiRegion || '')
  const [proxyResourceId, setProxyResourceId] = useState(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
  const [proxyUrl, setProxyUrl] = useState(credential.proxyUrl || '')
  const [proxyUsername, setProxyUsername] = useState(credential.proxyUsername || '')
  const [proxyPassword, setProxyPassword] = useState(credential.proxyPassword || '')
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const setCredentialProxy = useSetCredentialProxy()
  const setCredentialConcurrency = useSetCredentialConcurrency()
  const setCredentialRpm = useSetCredentialRpm()
  const setCredentialRegions = useSetCredentialRegions()
  const proxyResources = useProxyResources()
  const proxyResourceOptions = proxyResources.data?.resources || []
  const resetFailure = useResetFailure()
  const deleteCredential = useDeleteCredential()
  const setWarmup = useSetWarmup()
  const clearInFlight = useClearInFlight()
  const forceRefresh = useForceRefreshToken()
  const runtimeConfig = useRuntimeConfig()
  const confirmDialog = useConfirm()

  const warmupTarget = Math.max(0, runtimeConfig.data?.credentialWarmupRequests ?? 3)
  const accountInfo = accountInfoValue(credential, balance)
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
  const dispatchStatus = dispatchStatusLabel(credential, probationRemainingSecs)
  const subMeta = subscriptionBadgeMeta(credential, balance)
  const epLabel = endpointLabel(credential.endpoint)
  const hasFailures = credential.failureCount > 0 || credential.refreshFailureCount > 0
  const canClearInFlight = credential.inFlightRequests > 0
  const quotaDetail = accountInfo
    ? `检查 ${formatFullDate(accountInfo.checkedAt)}${accountInfo.nextResetAt ? ` · 重置 ${formatResetAt(accountInfo.nextResetAt)}` : ''}`
    : undefined

  const concurrencyPct =
    credential.maxConcurrentRequests > 0
      ? Math.round((credential.inFlightRequests / credential.maxConcurrentRequests) * 100)
      : 0
  const ringColor =
    concurrencyPct >= 90
      ? 'hsl(var(--destructive))'
      : concurrencyPct >= 70
        ? 'hsl(var(--warning))'
        : 'hsl(var(--success))'

  const resetProxyDraft = () => {
    setProxyResourceId(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
    setProxyUrl(credential.proxyUrl || '')
    setProxyUsername(credential.proxyUsername || '')
    setProxyPassword(credential.proxyPassword || '')
    setShowProxyUsername(false)
    setShowProxyPassword(false)
  }

  useEffect(() => { setPriorityValue(String(credential.priority)) }, [credential.priority])
  useEffect(() => {
    setConcurrencyValue(typeof credential.maxConcurrentRequestsOverride === 'number' ? String(credential.maxConcurrentRequestsOverride) : '')
  }, [credential.id, credential.maxConcurrentRequestsOverride])
  useEffect(() => {
    setRpmValue(typeof credential.rpmOverride === 'number' ? String(credential.rpmOverride) : '')
  }, [credential.id, credential.rpmOverride])
  useEffect(() => {
    setRegionValue(credential.region || '')
    setAuthRegionValue(credential.authRegion || '')
    setApiRegionValue(credential.apiRegion || '')
  }, [credential.id, credential.region, credential.authRegion, credential.apiRegion])
  useEffect(() => { resetProxyDraft() }, [credential.id, credential.proxyResourceId, credential.proxyUrl, credential.proxyUsername, credential.proxyPassword])

  const savePriority = () => {
    const p = Number(priorityValue)
    if (!Number.isInteger(p) || p < 0) { toast.error('优先级必须是非负整数'); return }
    setPriority.mutate({ id: credential.id, priority: p }, {
      onSuccess: (res) => { toast.success(res.message); setEditingPriority(false) },
      onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
    })
  }

  const adjustPriority = (delta: number) => {
    if (setPriority.isPending) return
    const p = Math.max(0, credential.priority + delta)
    if (p === credential.priority) return
    setPriority.mutate({ id: credential.id, priority: p }, {
      onSuccess: (res) => toast.success(res.message),
      onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveProxy = () => {
    const url = proxyUrl.trim()
    if (!proxyResourceId && !url && (proxyUsername.trim() || proxyPassword.trim())) {
      toast.error('直接代理 URL 为空时不能单独设置用户名/密码')
      return
    }
    setCredentialProxy.mutate({
      id: credential.id,
      request: {
        proxyResourceId: proxyResourceId ? Number(proxyResourceId) : null,
        proxyUrl: proxyResourceId ? undefined : url || undefined,
        proxyUsername: proxyResourceId ? undefined : proxyUsername.trim() || undefined,
        proxyPassword: proxyResourceId ? undefined : proxyPassword.trim() || undefined,
      },
    }, {
      onSuccess: (res) => { toast.success(res.message); setEditingProxy(false) },
      onError: (e) => toast.error(`代理设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveConcurrency = () => {
    const trimmed = concurrencyValue.trim()
    let maxConcurrentRequests: number | null = null
    if (trimmed) {
      const parsed = Number(trimmed)
      if (!Number.isInteger(parsed) || parsed < 0) { toast.error('并发限制必须是非负整数'); return }
      maxConcurrentRequests = parsed
    }
    setCredentialConcurrency.mutate({ id: credential.id, request: { maxConcurrentRequests } }, {
      onSuccess: (res) => { toast.success(res.message); setEditingConcurrency(false) },
      onError: (e) => toast.error(`并发设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveRpm = () => {
    const trimmed = rpmValue.trim()
    let rpm: number | null = null
    if (trimmed) {
      const parsed = Number(trimmed)
      if (!Number.isInteger(parsed) || parsed < 0) { toast.error('RPM 限制必须是非负整数'); return }
      rpm = parsed
    }
    setCredentialRpm.mutate({ id: credential.id, request: { rpm } }, {
      onSuccess: (res) => { toast.success(res.message); setEditingRpm(false) },
      onError: (e) => toast.error(`RPM 设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveRegions = () => {
    setCredentialRegions.mutate({
      id: credential.id,
      request: { region: regionValue.trim() || null, authRegion: authRegionValue.trim() || null, apiRegion: apiRegionValue.trim() || null },
    }, {
      onSuccess: (res) => { toast.success(res.message); setEditingRegions(false) },
      onError: (e) => toast.error(`Region 设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const handleDelete = () => {
    if (!credential.disabled) { toast.error('请先禁用账号再删除'); return }
    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => { toast.success(res.message); setShowDeleteConfirm(false) },
      onError: (e) => toast.error(`删除失败: ${extractErrorMessage(e)}`),
    })
  }

  // --------------------------------------------------------------------------
  // Render
  // --------------------------------------------------------------------------
  const statusTone = credential.disabled ? 'error' : credential.cooledDown ? 'warning' : credential.rateLimited ? 'info' : 'success'
  const statusLabel = credential.disabled ? (credential.disabledReason || '禁用') : credential.cooledDown ? `冷却` : credential.rateLimited ? '限流' : '正常'

  return (
    <div className={[
      'relative overflow-hidden rounded-xl bg-card shadow-sm transition-shadow hover:shadow-md',
      credential.disabled ? 'opacity-75' : '',
    ].join(' ')}>
      {credential.isCurrent && <span className="absolute inset-y-3 left-0 w-1 rounded-r-full bg-primary" />}

      {/* ── Header ── */}
      <div className="flex items-start gap-2.5 px-3 pt-3 pb-2">
        <Checkbox checked={selected} onCheckedChange={onToggleSelect} className="mt-0.5 shrink-0" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="max-w-[220px] truncate text-sm font-semibold" title={credentialLabel(credential)}>
              {credentialLabel(credential)}
            </span>
            <span className="tabular text-xs text-muted-foreground font-mono">#{credential.id}</span>
            {credential.isCurrent && <Badge tone="primary">当前</Badge>}
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-1">
            <Badge tone={statusTone}>{statusLabel}</Badge>
            <Badge>{authLabel(credential.authMethod)}</Badge>
            <Badge tone={subMeta.tone} title={subMeta.title}>
              {loadingBalance ? '…' : subMeta.label}
            </Badge>
            {epLabel && <Badge title={credential.endpoint}>{epLabel}</Badge>}
            {credential.hasProxy && <Badge tone="info">代理</Badge>}
            {typeof credential.maxConcurrentRequestsOverride === 'number' && <Badge tone="info">并发覆盖</Badge>}
            {typeof credential.rpmOverride === 'number' && <Badge tone="info">RPM覆盖</Badge>}
            {credential.hasProfileArn && <Badge tone="secondary">Profile ARN</Badge>}
            {!credential.disabled && credential.cooledDown && credential.cooldownRemainingSecs > 0 && (
              <Badge tone="error" className="animate-pulse" title={credential.cooldownReason || undefined}>冷却 {credential.cooldownRemainingSecs}s</Badge>
            )}
            {!credential.disabled && credential.rateLimited && (
              <Badge tone="warning" className="animate-pulse">限流 {credential.rateLimitRemainingSecs}s</Badge>
            )}
            {transientFailureStreak > 0 && <Badge tone="error">错误 {transientFailureStreak}</Badge>}
            {credential.warmupRemaining > 0 && <Badge tone="warning">预热 {credential.warmupRemaining}</Badge>}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1 pt-0.5">
          <Switch
            checked={!credential.disabled}
            disabled={setDisabled.isPending}
            onCheckedChange={() => setDisabled.mutate(
              { id: credential.id, disabled: !credential.disabled },
              {
                onSuccess: (res) => toast.success(res.message),
                onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
              }
            )}
          />
        </div>
      </div>

      {/* ── Summary Row ── */}
      <div className="grid grid-cols-6 gap-2 px-3 pb-2 pt-1">
        <SummaryCell label="优先级" value={credential.priority} onClick={() => setEditingPriority(true)} />
        <SummaryCell
          label="并发"
          value={
            credential.maxConcurrentRequests > 0
              ? `${credential.inFlightRequests}/${credential.maxConcurrentRequests}`
              : `${credential.inFlightRequests}/∞`
          }
          error={credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests}
          onClick={() => setEditingConcurrency(true)}
        />
        <SummaryCell
          label="RPM"
          value={
            credential.rpm != null
              ? credential.rpm === 0 ? '不限' : String(credential.rpm)
              : '-'
          }
          onClick={() => setEditingRpm(true)}
        />
        <SummaryCell label="调度状态" value={dispatchStatus} error={dispatchStatus !== '可调度'} />
        <SummaryCell label="成功" value={<span title={formatNumber(credential.successCount)}>{formatCompact(credential.successCount)}</span>} />
        <SummaryCell label="最近" value={formatLastUsed(credential.lastUsedAt)} />
      </div>

      {/* ── Action Strip ── */}
      <div className="flex flex-wrap gap-0.5 px-2 pb-2 pt-0.5">
        {/* 高频操作：直接展示 */}
        <Button type="button" variant="ghost" size="xs" onClick={() => onTest(credential)}>
          <Wand2 className="h-3.5 w-3.5" /> 测试
        </Button>
        <Button type="button" variant="ghost" size="xs" onClick={() => onQueryBalance(credential.id)} disabled={loadingBalance}>
          {loadingBalance ? <Spinner size="sm" /> : <Wallet className="h-3.5 w-3.5" />} 查询
        </Button>
        <Button type="button" variant="ghost" size="xs"
          disabled={forceRefresh.isPending || credential.authMethod === 'api_key'}
          title={credential.authMethod === 'api_key' ? 'API Key 无需刷新' : '强制刷新 Token'}
          onClick={() => forceRefresh.mutate(credential.id, {
            onSuccess: (res) => toast.success(res.message),
            onError: (e) => toast.error(`刷新失败: ${extractErrorMessage(e)}`),
          })}>
          <RefreshCw className={`h-3.5 w-3.5 ${forceRefresh.isPending ? 'animate-spin' : ''}`} /> 刷新Token
        </Button>
        <Button type="button" variant="ghost" size="xs"
          disabled={resetFailure.isPending || !hasFailures}
          onClick={() => resetFailure.mutate(credential.id, {
            onSuccess: (res) => toast.success(res.message),
            onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
          })}>
          <RotateCcw className="h-3.5 w-3.5" /> 恢复异常
        </Button>
        <Button type="button" variant="ghost" size="xs" disabled={setPriority.isPending || credential.priority === 0} onClick={() => adjustPriority(-1)}>
          <ChevronUp className="h-3.5 w-3.5" /> 优先级
        </Button>
        <Button type="button" variant="ghost" size="xs" disabled={setPriority.isPending} onClick={() => adjustPriority(1)}>
          <ChevronDown className="h-3.5 w-3.5" /> 优先级
        </Button>

        {/* 低频/危险操作：收入 ... 菜单 */}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button type="button" variant="ghost" size="xs" title="更多操作">
              <MoreHorizontal className="h-3.5 w-3.5" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuLabel>更多操作</DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              disabled={setWarmup.isPending}
              onSelect={() => setWarmup.mutate(
                { id: credential.id, warmupRemaining: credential.warmupRemaining > 0 ? 0 : Math.max(1, warmupTarget) },
                {
                  onSuccess: () => toast.success(credential.warmupRemaining > 0 ? '已关闭预热' : '已开启预热'),
                  onError: (e) => toast.error(`预热设置失败: ${extractErrorMessage(e)}`),
                }
              )}
            >
              {credential.warmupRemaining > 0 ? '关闭预热' : '开启预热'}
            </DropdownMenuItem>
            <DropdownMenuItem
              disabled={clearInFlight.isPending || !canClearInFlight}
              onSelect={async () => {
                if (!canClearInFlight) return
                const ok = await confirmDialog({ title: '清理并发', message: `清理账号 #${credential.id} 当前并发占用？`, confirmText: '清理', tone: 'danger' })
                if (!ok) return
                clearInFlight.mutate({ id: credential.id }, {
                  onSuccess: (res) => toast.success(res.message),
                  onError: (e) => toast.error(`清理失败: ${extractErrorMessage(e)}`),
                })
              }}
            >
              清理并发
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              destructive
              disabled={!credential.disabled}
              onSelect={() => { if (credential.disabled) setShowDeleteConfirm(true) }}
            >
              <Trash2 className="h-3.5 w-3.5" /> 删除
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {/* ── Expanded Detail ── */}
      {expanded && (
        <div className="space-y-4 px-3 pb-3 pt-2 animate-in fade-in-0 slide-in-from-top-2 duration-200">

          {/* 调度运行态 */}
          <div>
            <div className="mb-2 text-[0.68rem] font-semibold text-muted-foreground uppercase tracking-wide">基础信息</div>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
              <MetaItem label="创建时间" value={credential.createdAt ? formatFullDate(credential.createdAt) : '-'} />
              <MetaItem label="更新时间" value={credential.updatedAt ? formatFullDate(credential.updatedAt) : '-'} />
              {credential.email && credential.maskedApiKey && (
                <MetaItem label="API Key" value={<span className="font-mono truncate block" title={credential.maskedApiKey}>{credential.maskedApiKey}</span>} />
              )}
            </div>
          </div>

          {/* 调度运行态 */}
          <div>
            <div className="mb-2 text-[0.68rem] font-semibold text-muted-foreground uppercase tracking-wide">调度 / 运行态</div>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
              <div className="flex items-center gap-3">
                {credential.inFlightRequests > 0 && credential.maxConcurrentRequests > 0 ? (
                  <ProgressRing
                    value={concurrencyPct}
                    size={44}
                    strokeWidth={5}
                    color={ringColor}
                    label={<span className="text-[0.6rem] tabular">{credential.inFlightRequests}</span>}
                  />
                ) : null}
                <MetaItem
                  label="在途请求"
                  value={`${credential.inFlightRequests}${credential.maxConcurrentRequests > 0 ? `/${credential.maxConcurrentRequests}` : ' / ∞'}`}
                  detail={
                    credential.inFlightRequests > 0
                      ? `最老 ${credential.oldestInFlightAgeSecs}s · 闲置 ${credential.newestInFlightIdleSecs}s`
                      : typeof credential.maxConcurrentRequestsOverride === 'number'
                        ? `账号覆盖：${credential.maxConcurrentRequestsOverride}`
                        : credential.maxConcurrentRequests > 0
                          ? `继承全局：${credential.maxConcurrentRequests}`
                          : '不限制'
                  }
                />
              </div>
              <div className="flex flex-col gap-0.5">
                <button
                  type="button"
                  className="text-left rounded px-1 -mx-1 transition-colors hover:bg-muted/60"
                  onClick={() => setEditingRpm(true)}
                  title="修改RPM"
                >
                  <MetaItem
                    label="RPM 限制"
                    value={
                      credential.rpm != null
                        ? credential.rpm === 0 ? '不限制' : `${credential.rpm} /min`
                        : '-'
                    }
                    detail={
                      typeof credential.rpmOverride === 'number'
                        ? credential.rpmOverride === 0 ? '账号覆盖：不限制' : `账号覆盖：${credential.rpmOverride}`
                        : '继承全局'
                    }
                  />
                </button>
              </div>
              <MetaItem label="近期错误率" value={`${(recentErrorRate * 100).toFixed(1)}%`} error={recentErrorRate > 0} />
              <MetaItem label="延迟 EWMA" value={credential.latencyEwmaMs == null ? '未知' : `${Math.round(credential.latencyEwmaMs)}ms`} />
              <MetaItem label="调度评分" value={schedulerScore.toFixed(2)} />
              <MetaItem label="总调度" value={<span title={formatNumber(schedulerSelectionCount)}>{formatCompact(schedulerSelectionCount)}</span>} />
              <MetaItem
                label="近期调度"
                value={`${formatNumber(recentSelection60s)}/60s`}
                detail={`10s ${formatNumber(recentSelection10s)} · 5m ${formatNumber(recentSelection5m)}`}
              />
              <MetaItem
                label="调度压力"
                value={schedulerSelectionPressure.toFixed(2)}
                error={schedulerSelectionPressure > 1}
              />
              <MetaItem label="失败/刷新失败" value={`${credential.failureCount} / ${credential.refreshFailureCount}`} error={hasFailures} />
              <MetaItem
                label="Lease 回收"
                value={credential.inFlightLeaseMaxSecs > 0 ? `${credential.inFlightLeaseMaxSecs}s` : '-'}
              />
              {credential.warmupRemaining > 0 && (
                <MetaItem label="预热剩余" value={credential.warmupRemaining} />
              )}
              {credential.inProbation && (
                <MetaItem label="观察期" value={`${probationRemainingSecs}s`} />
              )}
              {(credential.pricedRequests > 0 || credential.unpricedRequests > 0) && (
                <MetaItem
                  label="计价请求覆盖"
                  value={<span title={`${formatNumber(credential.pricedRequests)}/${formatNumber(credential.pricedRequests + credential.unpricedRequests)}`}>{formatCompact(credential.pricedRequests)}/{formatCompact(credential.pricedRequests + credential.unpricedRequests)}</span>}
                  error={credential.unpricedRequests > 0}
                />
              )}
              {credential.cooldownReason && (
                <MetaItem label="冷却原因" value={<span className="truncate text-destructive text-xs">{credential.cooldownReason}</span>} />
              )}
            </div>
            {credential.cooldowns && credential.cooldowns.length > 0 && (
              <div className="mt-2 flex flex-wrap gap-1.5 text-xs text-muted-foreground">
                {credential.cooldowns.map((cd) => (
                  <span
                    key={`${cd.global ? 'global' : cd.model}-${cd.remainingSecs}`}
                    className="rounded bg-muted/40 px-2 py-0.5"
                    title={cd.reason || undefined}
                  >
                    {cd.global ? '全部模型' : cd.model || '-'} · 冷却 {cd.remainingSecs}s
                  </span>
                ))}
              </div>
            )}
          </div>

          {/* 额度 */}
          <div>
            <div className="mb-2 text-[0.68rem] font-semibold text-muted-foreground uppercase tracking-wide">额度与费用</div>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
              <MetaItem
                label="用量额度"
                value={loadingBalance ? <Spinner size="sm" /> : accountInfo
                  ? `${formatQuota(accountInfo.currentUsage)} / ${formatQuota(accountInfo.usageLimit)}`
                  : '未查询'}
                detail={quotaDetail}
              />
              <MetaItem
                label="剩余积分"
                value={loadingBalance ? <Spinner size="sm" /> : accountInfo
                  ? formatCredits(accountInfo.creditRemaining)
                  : '未查询'}
                detail={accountInfo ? `总额 ${formatCredits(accountInfo.creditLimit)}` : undefined}
              />
              <MetaItem label="估算成本" value={formatUsd(credential.estimatedCostUsd)} />
              <MetaItem label="Kiro计量" value={formatMeteringUsage(credential.kiroMeteringUsage)} />
            </div>
          </div>

          {/* 网络 */}
          <div>
            <div className="mb-2 text-[0.68rem] font-semibold text-muted-foreground uppercase tracking-wide">网络配置</div>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
              <MetaItem
                label="Auth Region"
                value={
                  <button type="button" className="font-mono text-primary hover:underline text-sm"
                    onClick={() => setEditingRegions(true)}>
                    {credential.effectiveAuthRegion || '-'}
                  </button>
                }
                detail={credential.authRegion || credential.region ? '账号覆盖' : '继承全局'}
              />
              <MetaItem
                label="API Region"
                value={
                  <button type="button" className="font-mono text-primary hover:underline text-sm"
                    onClick={() => setEditingRegions(true)}>
                    {credential.effectiveApiRegion || '-'}
                  </button>
                }
                detail={credential.apiRegion ? '账号覆盖' : '继承全局'}
              />
              <MetaItem
                label="代理"
                value={
                  <button type="button" className="flex items-center gap-1 text-primary hover:underline text-sm"
                    onClick={() => { resetProxyDraft(); setEditingProxy(true) }}>
                    <Router className="h-3 w-3 shrink-0" />
                    <span className="truncate">{proxySummary(credential)}</span>
                  </button>
                }
              />
            </div>
          </div>

          {/* 最近错误 */}
          {credential.lastErrorReason && (
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs">
              <span className="font-semibold text-destructive">
                最近错误{lastTransientErrorAgo ? ` (${lastTransientErrorAgo})` : ''}：
              </span>
              <span className="text-muted-foreground">{credential.lastErrorKind}: {credential.lastErrorReason}</span>
            </div>
          )}
        </div>
      )}

      {/* ── Priority Modal ── */}
      <ModalShell open={editingPriority} title={`优先级：${credentialLabel(credential)}`} width="max-w-md"
        onClose={() => { setEditingPriority(false); setPriorityValue(String(credential.priority)) }}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前优先级</span>
              <span className="font-semibold">{credential.priority}</span>
            </div>
            <div className="mt-1 text-xs text-muted-foreground">数值越小优先级越高；不能小于 0。</div>
          </div>
          <Field label="新优先级">
            <Input type="number" min={0} value={priorityValue} disabled={setPriority.isPending}
              onChange={(e) => setPriorityValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => { setEditingPriority(false); setPriorityValue(String(credential.priority)) }} disabled={setPriority.isPending}>取消</Button>
            <Button size="sm" onClick={savePriority} disabled={setPriority.isPending}>
              {setPriority.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Concurrency Modal ── */}
      <ModalShell open={editingConcurrency} title={`并发限制：${credentialLabel(credential)}`} width="max-w-lg"
        onClose={() => setEditingConcurrency(false)}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前生效</span>
              <span className="font-semibold">{credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests} 并发` : '不限'}</span>
            </div>
            <div className="mt-1 text-xs text-muted-foreground">
              {typeof credential.maxConcurrentRequestsOverride === 'number' ? '当前已覆盖全局。' : '当前继承全局。'}
            </div>
          </div>
          <Field label="账号级最大并发" description="留空继承全局；0 表示该账号不限；正整数设置上限。">
            <Input type="number" min={0} value={concurrencyValue} placeholder="留空继承全局，0 表示不限"
              disabled={setCredentialConcurrency.isPending} onChange={(e) => setConcurrencyValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setEditingConcurrency(false)} disabled={setCredentialConcurrency.isPending}>取消</Button>
            <Button variant="ghost" size="sm"
              disabled={setCredentialConcurrency.isPending || typeof credential.maxConcurrentRequestsOverride !== 'number'}
              onClick={() => {
                setConcurrencyValue('')
                setCredentialConcurrency.mutate({ id: credential.id, request: { maxConcurrentRequests: null } }, {
                  onSuccess: (res) => { toast.success(res.message); setEditingConcurrency(false) },
                  onError: (e) => toast.error(`设置失败: ${extractErrorMessage(e)}`),
                })
              }}>继承全局</Button>
            <Button size="sm" onClick={saveConcurrency} disabled={setCredentialConcurrency.isPending}>
              {setCredentialConcurrency.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── RPM Modal ── */}
      <ModalShell open={editingRpm} title={`RPM 限制：${credentialLabel(credential)}`} width="max-w-lg"
        onClose={() => setEditingRpm(false)}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前生效</span>
              <span className="font-semibold">
                {credential.rpm != null
                  ? credential.rpm === 0 ? '不限制' : `${credential.rpm} 次/分钟`
                  : '-'}
              </span>
            </div>
            <div className="mt-1 text-xs text-muted-foreground">
              {typeof credential.rpmOverride === 'number' ? '当前已覆盖全局。' : '当前继承全局。'}
            </div>
          </div>
          <Field label="账号级 RPM 限制" description="留空继承全局；0 表示该账号不限；正整数设置每分钟最大请求数。">
            <Input type="number" min={0} value={rpmValue} placeholder="留空继承全局，0 表示不限"
              disabled={setCredentialRpm.isPending} onChange={(e) => setRpmValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setEditingRpm(false)} disabled={setCredentialRpm.isPending}>取消</Button>
            <Button variant="ghost" size="sm"
              disabled={setCredentialRpm.isPending || typeof credential.rpmOverride !== 'number'}
              onClick={() => {
                setRpmValue('')
                setCredentialRpm.mutate({ id: credential.id, request: { rpm: null } }, {
                  onSuccess: (res) => { toast.success(res.message); setEditingRpm(false) },
                  onError: (e) => toast.error(`设置失败: ${extractErrorMessage(e)}`),
                })
              }}>继承全局</Button>
            <Button size="sm" onClick={saveRpm} disabled={setCredentialRpm.isPending}>
              {setCredentialRpm.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Regions Modal ── */}
      <ModalShell open={editingRegions} title={`Region：${credentialLabel(credential)}`} width="max-w-lg"
        onClose={() => { if (setCredentialRegions.isPending) return; setEditingRegions(false) }}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 p-3 text-sm grid gap-2 sm:grid-cols-2">
            <div>
              <div className="text-xs text-muted-foreground">当前 Auth Region</div>
              <div className="font-mono font-semibold">{credential.effectiveAuthRegion || '-'}</div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">当前 API Region</div>
              <div className="font-mono font-semibold">{credential.effectiveApiRegion || '-'}</div>
            </div>
          </div>
          <Field label="Region 兼容字段" description="未设置 Auth Region 时作为 token 刷新回退字段。">
            <Input className="font-mono" value={regionValue} disabled={setCredentialRegions.isPending}
              onChange={(e) => setRegionValue(e.target.value)} placeholder="留空继承全局" />
          </Field>
          <Field label="Auth Region">
            <Input className="font-mono" value={authRegionValue} disabled={setCredentialRegions.isPending}
              onChange={(e) => setAuthRegionValue(e.target.value)} placeholder="如 us-east-1，留空继承" />
          </Field>
          <Field label="API Region">
            <Input className="font-mono" value={apiRegionValue} disabled={setCredentialRegions.isPending}
              onChange={(e) => setApiRegionValue(e.target.value)} placeholder="如 us-east-1，留空继承" />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setEditingRegions(false)} disabled={setCredentialRegions.isPending}>取消</Button>
            <Button variant="ghost" size="sm"
              disabled={setCredentialRegions.isPending || (!credential.region && !credential.authRegion && !credential.apiRegion)}
              onClick={() => {
                setRegionValue(''); setAuthRegionValue(''); setApiRegionValue('')
                setCredentialRegions.mutate({ id: credential.id, request: { region: null, authRegion: null, apiRegion: null } }, {
                  onSuccess: (res) => { toast.success(res.message); setEditingRegions(false) },
                  onError: (e) => toast.error(`设置失败: ${extractErrorMessage(e)}`),
                })
              }}>清空覆盖</Button>
            <Button size="sm" onClick={saveRegions} disabled={setCredentialRegions.isPending}>
              {setCredentialRegions.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Proxy Modal ── */}
      <ModalShell open={editingProxy} title={`绑定代理：${credentialLabel(credential)}`} width="max-w-xl"
        onClose={() => { if (setCredentialProxy.isPending) return; resetProxyDraft(); setEditingProxy(false) }}>
        <div className="space-y-3">
          <button type="button"
            className={`w-full rounded-lg border p-3 text-left text-sm transition-colors ${!proxyResourceId ? 'border-primary bg-primary/5' : 'border-border hover:bg-muted'}`}
            onClick={() => setProxyResourceId('')}>
            <div className="flex items-center justify-between">
              <span className="font-semibold">不绑定代理资源</span>
              {!proxyResourceId && <Badge tone="primary">已选</Badge>}
            </div>
            <div className="mt-1 text-xs text-muted-foreground">可使用下方直连代理或全局代理。</div>
          </button>
          <div className={`rounded-lg border p-3 ${proxyResourceId ? 'opacity-60' : 'border-border bg-card'}`}>
            <div className="mb-3 text-sm font-semibold">账号直连代理</div>
            <FieldGrid>
              <Field label="代理 URL">
                <Input value={proxyUrl} placeholder="socks5h://127.0.0.1:1080"
                  disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)}
                  onChange={(e) => { if (e.target.value.trim()) setProxyResourceId(''); setProxyUrl(e.target.value) }} />
              </Field>
              <Field label="用户名">
                <SecretInput value={proxyUsername} onChange={(v) => { if (v.trim()) setProxyResourceId(''); setProxyUsername(v) }}
                  visible={showProxyUsername} onToggle={() => setShowProxyUsername((v) => !v)}
                  disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)} placeholder="可选" />
              </Field>
              <Field label="密码">
                <SecretInput value={proxyPassword} onChange={(v) => { if (v.trim()) setProxyResourceId(''); setProxyPassword(v) }}
                  visible={showProxyPassword} onToggle={() => setShowProxyPassword((v) => !v)}
                  disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)} placeholder="可选" />
              </Field>
            </FieldGrid>
          </div>
          {proxyResources.isLoading ? (
            <LoadingState text="加载代理资源..." />
          ) : proxyResourceOptions.length === 0 ? (
            <EmptyState title="暂无代理资源" description="请先在代理页添加" />
          ) : (
            <div className="max-h-56 overflow-y-auto scrollbar-thin space-y-1.5">
              {proxyResourceOptions.map((r) => {
                const sel = proxyResourceId === String(r.id)
                return (
                  <button key={r.id} type="button"
                    className={`w-full rounded-lg border p-2.5 text-left text-sm transition-colors ${sel ? 'border-primary bg-primary/5' : r.enabled ? 'border-border hover:bg-muted' : 'border-destructive/25 bg-destructive/5 opacity-60'}`}
                    onClick={() => { setProxyResourceId(String(r.id)); setProxyUrl(''); setProxyUsername(''); setProxyPassword('') }}>
                    <div className="flex items-center gap-1.5">
                      <span className="font-semibold">{r.name}</span>
                      <span className="text-xs text-muted-foreground font-mono">#{r.id}</span>
                      <Badge tone={r.enabled ? 'success' : 'error'}>{r.enabled ? '启用' : '禁用'}</Badge>
                      {sel && <Badge tone="primary">已选</Badge>}
                    </div>
                    <div className="mt-0.5 truncate text-xs text-muted-foreground">{r.proxyUrl}</div>
                  </button>
                )
              })}
            </div>
          )}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => { resetProxyDraft(); setEditingProxy(false) }} disabled={setCredentialProxy.isPending}>取消</Button>
            <Button size="sm" onClick={saveProxy} disabled={setCredentialProxy.isPending}>
              {setCredentialProxy.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Delete Confirm ── */}
      <ModalShell open={showDeleteConfirm} title={`确认删除账号 #${credential.id}`} width="max-w-md"
        onClose={() => setShowDeleteConfirm(false)}>
        <div className="space-y-3 text-sm">
          <p>此操作会永久删除该账号，无法撤销。</p>
          <div className="rounded-lg bg-muted/30 px-3 py-2">
            <div className="font-semibold">{credentialLabel(credential)}</div>
            <div className="mt-1 text-xs text-muted-foreground">只有已禁用账号允许删除。</div>
          </div>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setShowDeleteConfirm(false)} disabled={deleteCredential.isPending}>取消</Button>
            <Button variant="destructive" size="sm" onClick={handleDelete} disabled={deleteCredential.isPending || !credential.disabled}>
              {deleteCredential.isPending && <Spinner size="sm" />}确认删除
            </Button>
          </div>
        </div>
      </ModalShell>
    </div>
  )
}
