import {
  ChevronDown,
  ChevronUp,
  Eye,
  EyeOff,
  Gauge,
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
  Input,
  Spinner,
  Switch,
} from '@/components/ui'
import { EmptyState, Field, FieldGrid, LoadingState, ModalShell, useConfirm } from '@/components/patterns'
import {
  formatApproxElapsedMs,
  formatCredits,
  formatFullDate,
  formatLastUsed,
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
  useSetDisabled,
  useSetPriority,
  useSetWarmup,
} from '@/hooks/use-credentials'
import type { BalanceResponse, CredentialStatusItem } from '@/types/api'

// ============================================================================
// Helpers
// ============================================================================

export function credentialLabel(credential: CredentialStatusItem) {
  return credential.email || credential.maskedApiKey || `账号 #${credential.id}`
}

function authLabel(authMethod: string | null) {
  if (authMethod === 'api_key') return 'API Key'
  if (authMethod === 'idc') return 'IdC'
  if (authMethod === 'social') return 'Social'
  return authMethod || 'Unknown'
}

type CredentialBadgeTone = 'neutral' | 'primary' | 'secondary' | 'success' | 'warning' | 'error' | 'info'

function subscriptionLabel(credential: CredentialStatusItem, balance?: BalanceResponse) {
  return balance?.subscriptionTitle || credential.accountInfo?.subscriptionTitle || credential.subscriptionTitle || '未知'
}

function subscriptionBadgeMeta(credential: CredentialStatusItem, balance?: BalanceResponse): { label: string; tone: CredentialBadgeTone; title?: string } {
  const raw = subscriptionLabel(credential, balance)
  const normalized = raw.toLowerCase().replace(/[_\s-]+/g, ' ')
  if (!raw || raw === '未知') return { label: '未知套餐', tone: 'secondary' }
  if (normalized.includes('power')) return { label: 'Power', tone: 'primary', title: raw }
  if (normalized.includes('pro plus') || normalized.includes('pro+')) return { label: 'Pro+', tone: 'primary', title: raw }
  if (normalized.includes('pro')) return { label: 'Pro', tone: 'primary', title: raw }
  if (normalized.includes('free')) return { label: 'Free', tone: 'secondary', title: raw }
  if (normalized.includes('trial') || normalized.includes('试用')) return { label: 'Trial', tone: 'info', title: raw }
  return { label: raw, tone: 'neutral', title: raw }
}

function endpointLabel(endpoint?: string | null) {
  if (!endpoint) return ''
  const value = endpoint.trim()
  if (!value) return ''
  const lower = value.toLowerCase()
  if (lower === 'ide') return 'IDE'
  if (lower === 'idc') return 'IDC'
  if (lower === 'api_key') return 'API Key'
  if (lower.includes('power')) return 'Power 入口'
  return value.replace(/_/g, ' ').toUpperCase()
}

function accountInfoValue(credential: CredentialStatusItem, balance?: BalanceResponse) {
  return balance || credential.accountInfo
}

function numberOrZero(value: number | null | undefined): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}

function sourceLabel(source?: CredentialStatusItem['effectiveProxySource']) {
  const labels: Record<string, string> = {
    credential: '直接代理', resource: '代理资源', resource_disabled: '代理已禁用',
    resource_missing: '代理不存在', global: '全局代理', direct: '直连',
  }
  return labels[source || ''] || '未配置'
}

function proxySummary(credential: CredentialStatusItem): string {
  const label = sourceLabel(credential.effectiveProxySource)
  if (credential.proxyResourceName && (credential.effectiveProxySource === 'resource' || credential.effectiveProxySource === 'resource_disabled' || credential.effectiveProxySource === 'resource_missing')) {
    return `${label}：${credential.proxyResourceName}`
  }
  return label
}

function concurrencyLimitLabel(credential: CredentialStatusItem) {
  const effective = credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests}` : '不限'
  if (typeof credential.maxConcurrentRequestsOverride === 'number') {
    return credential.maxConcurrentRequestsOverride > 0 ? `账号覆盖：${credential.maxConcurrentRequestsOverride}` : '账号覆盖：不限'
  }
  return `继承全局：${effective}`
}

function formatResetAt(value?: number | null): string {
  if (!value) return '-'
  return new Date(value * 1000).toLocaleString('zh-CN', { hour12: false, month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
}

function pricedCoverageLabel(credential: CredentialStatusItem): string {
  const total = credential.pricedRequests + credential.unpricedRequests
  if (total <= 0) return '-'
  return `${formatNumber(credential.pricedRequests)}/${formatNumber(total)}`
}

function dispatchStatusLabel(credential: CredentialStatusItem, probationRemainingSecs: number): string {
  if (credential.cooledDown) return `冷却中 ${credential.cooldownRemainingSecs}s`
  if (credential.rateLimited) return `本地限流 ${credential.rateLimitRemainingSecs}s`
  if (credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests) return `并发已满 ${credential.inFlightRequests}/${credential.maxConcurrentRequests}`
  if (credential.inProbation) return `恢复观察 ${probationRemainingSecs}s`
  if (credential.warmupRemaining > 0) return `预热剩余 ${credential.warmupRemaining} 次`
  return '可调度'
}

// ============================================================================
// SecretInput
// ============================================================================

function SecretInput({ value, onChange, visible, onToggle, disabled, placeholder }: {
  value: string; onChange: (value: string) => void; visible: boolean; onToggle: () => void; disabled?: boolean; placeholder?: string
}) {
  return (
    <div className="relative">
      <Input className="pr-10" type={visible ? 'text' : 'password'} value={value} placeholder={placeholder} disabled={disabled} onChange={(e) => onChange(e.target.value)} />
      <Button type="button" variant="ghost" size="icon-sm" className="absolute right-1 top-1" onClick={onToggle} disabled={disabled} title={visible ? '隐藏' : '显示'}>
        {visible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
      </Button>
    </div>
  )
}

// ============================================================================
// SummaryItem / MetaItem
// ============================================================================

function SummaryItem({ label, value, error }: { label: string; value: React.ReactNode; error?: boolean }) {
  return (
    <div className="flex flex-col gap-0.5">
      <div className="text-[0.68rem] font-medium text-muted-foreground">{label}</div>
      <div className={`truncate text-xs font-semibold ${error ? 'text-destructive' : ''}`} title={typeof value === 'string' ? value : undefined}>{value}</div>
    </div>
  )
}

function MetaItem({ label, value, detail, error }: { label: string; value: React.ReactNode; detail?: React.ReactNode; error?: boolean }) {
  return (
    <div className="flex flex-col gap-0.5">
      <div className="text-[0.68rem] font-medium text-muted-foreground">{label}</div>
      <div className={`min-w-0 truncate text-sm font-semibold ${error ? 'text-destructive' : ''}`}>{value}</div>
      {detail && <div className="mt-0.5 truncate text-xs font-medium text-muted-foreground" title={typeof detail === 'string' ? detail : undefined}>{detail}</div>}
    </div>
  )
}

function TruncatedValue({ value, mono }: { value: string; mono?: boolean }) {
  return <span className={`block truncate ${mono ? 'font-mono' : ''}`} title={value}>{value}</span>
}

// ============================================================================
// CredentialCard
// ============================================================================

export interface CredentialCardProps {
  credential: CredentialStatusItem
  selected: boolean
  onToggleSelect: () => void
  onQueryBalance: (id: number) => void
  onTest: (credential: CredentialStatusItem) => void
  balance?: BalanceResponse
  loadingBalance: boolean
}

export function CredentialCard({ credential, selected, onToggleSelect, onQueryBalance, onTest, balance, loadingBalance }: CredentialCardProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [editingProxy, setEditingProxy] = useState(false)
  const [editingConcurrency, setEditingConcurrency] = useState(false)
  const [editingRegions, setEditingRegions] = useState(false)
  const [detailsOpen, setDetailsOpen] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [regionValue, setRegionValue] = useState(credential.region || '')
  const [authRegionValue, setAuthRegionValue] = useState(credential.authRegion || '')
  const [apiRegionValue, setApiRegionValue] = useState(credential.apiRegion || '')
  const [proxyResourceId, setProxyResourceId] = useState(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
  const [proxyUrl, setProxyUrl] = useState(credential.proxyUrl || '')
  const [proxyUsername, setProxyUsername] = useState(credential.proxyUsername || '')
  const [proxyPassword, setProxyPassword] = useState(credential.proxyPassword || '')
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)
  const [concurrencyValue, setConcurrencyValue] = useState(typeof credential.maxConcurrentRequestsOverride === 'number' ? String(credential.maxConcurrentRequestsOverride) : '')

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const setCredentialProxy = useSetCredentialProxy()
  const setCredentialConcurrency = useSetCredentialConcurrency()
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
  const subscriptionMeta = subscriptionBadgeMeta(credential, balance)
  const endpointMeta = endpointLabel(credential.endpoint)
  const hasFailures = credential.failureCount > 0 || credential.refreshFailureCount > 0
  const hasPricingCoverage = credential.pricedRequests > 0 || credential.unpricedRequests > 0
  const canClearInFlight = credential.inFlightRequests > 0
  const quotaDetail = accountInfo ? `检查 ${formatFullDate(accountInfo.checkedAt)}${accountInfo.nextResetAt ? ` · 重置 ${formatResetAt(accountInfo.nextResetAt)}` : ''}` : undefined

  const resetProxyDraft = () => {
    setProxyResourceId(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
    setProxyUrl(credential.proxyUrl || ''); setProxyUsername(credential.proxyUsername || ''); setProxyPassword(credential.proxyPassword || '')
    setShowProxyUsername(false); setShowProxyPassword(false)
  }

  const openProxyEditor = () => { resetProxyDraft(); setEditingProxy(true) }
  const closeProxyEditor = () => { if (setCredentialProxy.isPending) return; resetProxyDraft(); setEditingProxy(false) }

  useEffect(() => { setPriorityValue(String(credential.priority)) }, [credential.priority])
  useEffect(() => { resetProxyDraft() }, [credential.id, credential.proxyResourceId, credential.proxyUrl, credential.proxyUsername, credential.proxyPassword])
  useEffect(() => { setConcurrencyValue(typeof credential.maxConcurrentRequestsOverride === 'number' ? String(credential.maxConcurrentRequestsOverride) : '') }, [credential.id, credential.maxConcurrentRequestsOverride])
  useEffect(() => { setRegionValue(credential.region || ''); setAuthRegionValue(credential.authRegion || ''); setApiRegionValue(credential.apiRegion || '') }, [credential.id, credential.region, credential.authRegion, credential.apiRegion])

  const savePriority = () => {
    const priority = Number(priorityValue)
    if (!Number.isInteger(priority) || priority < 0) { toast.error('优先级必须是非负整数'); return }
    setPriority.mutate({ id: credential.id, priority }, {
      onSuccess: (res) => { toast.success(res.message); setEditingPriority(false) },
      onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
    })
  }

  const adjustPriority = (delta: number) => {
    if (setPriority.isPending) return
    const priority = Math.max(0, credential.priority + delta)
    if (priority === credential.priority) return
    setPriority.mutate({ id: credential.id, priority }, {
      onSuccess: (res) => toast.success(res.message),
      onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
    })
  }

  const saveProxy = () => {
    const directProxyUrl = proxyUrl.trim(); const directProxyUsername = proxyUsername.trim(); const directProxyPassword = proxyPassword.trim()
    if (!proxyResourceId && !directProxyUrl && (directProxyUsername || directProxyPassword)) { toast.error('直接代理 URL 为空时不能单独保存代理账号或密码'); return }
    setCredentialProxy.mutate({
      id: credential.id,
      request: { proxyResourceId: proxyResourceId ? Number(proxyResourceId) : null, proxyUrl: proxyResourceId ? undefined : directProxyUrl || undefined, proxyUsername: proxyResourceId ? undefined : directProxyUsername || undefined, proxyPassword: proxyResourceId ? undefined : directProxyPassword || undefined },
    }, {
      onSuccess: (res) => { toast.success(res.message); setEditingProxy(false) },
      onError: (error) => toast.error(`代理设置失败: ${extractErrorMessage(error)}`),
    })
  }

  const saveConcurrency = () => {
    const trimmed = concurrencyValue.trim()
    let maxConcurrentRequests: number | null = null
    if (trimmed) {
      const parsed = Number(trimmed)
      if (!Number.isInteger(parsed) || parsed < 0) { toast.error('账号并发限制必须是非负整数'); return }
      maxConcurrentRequests = parsed
    }
    setCredentialConcurrency.mutate({ id: credential.id, request: { maxConcurrentRequests } }, {
      onSuccess: (res) => { toast.success(res.message); setEditingConcurrency(false) },
      onError: (error) => toast.error(`并发限制设置失败: ${extractErrorMessage(error)}`),
    })
  }

  const saveRegions = () => {
    setCredentialRegions.mutate({ id: credential.id, request: { region: regionValue.trim() || null, authRegion: authRegionValue.trim() || null, apiRegion: apiRegionValue.trim() || null } }, {
      onSuccess: (res) => { toast.success(res.message); setEditingRegions(false) },
      onError: (error) => toast.error(`Region 设置失败: ${extractErrorMessage(error)}`),
    })
  }

  const handleDelete = () => {
    if (!credential.disabled) { toast.error('请先禁用账号再删除'); return }
    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => { toast.success(res.message); setShowDeleteConfirm(false) },
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

  const handleForceRefresh = () => {
    forceRefresh.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (error) => toast.error(`刷新失败: ${extractErrorMessage(error)}`),
    })
  }

  return (
    <div className={`rounded-xl border bg-card shadow-sm ${credential.isCurrent ? 'border-primary/50' : 'border-border'} ${credential.disabled ? 'opacity-70' : ''}`}>
      {/* Card Header */}
      <div className="flex items-start gap-3 p-3">
        <Checkbox checked={selected} onCheckedChange={onToggleSelect} />
        <button type="button" className="min-w-0 flex-1 text-left" onClick={() => setDetailsOpen((v) => !v)} aria-expanded={detailsOpen}>
          <div className="flex items-center gap-2">
            <span className="truncate text-sm font-semibold" title={credentialLabel(credential)}>{credentialLabel(credential)}</span>
            <Badge>#{credential.id}</Badge>
            {credential.isCurrent && <Badge tone="primary">当前</Badge>}
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-1">
            <Badge tone={credential.disabled ? 'error' : 'success'}>{credential.disabled ? '禁用' : '启用'}</Badge>
            {credential.disabled && credential.disabledReason && <Badge tone="error" title={credential.disabledReason}>{credential.disabledReason}</Badge>}
            <Badge tone={subscriptionMeta.tone} title={subscriptionMeta.title}>{loadingBalance ? '查询中' : subscriptionMeta.label}</Badge>
            <Badge>{authLabel(credential.authMethod)}</Badge>
            {endpointMeta && <Badge title={`入口：${credential.endpoint}`}>{endpointMeta}</Badge>}
            {credential.hasProfileArn && <Badge tone="secondary">Profile ARN</Badge>}
            {credential.hasProxy && <Badge tone="info">代理</Badge>}
            {typeof credential.maxConcurrentRequestsOverride === 'number' && <Badge tone="info">并发覆盖</Badge>}
            {!credential.disabled && credential.cooledDown && (
              <Badge tone="warning" title={(credential.cooldowns || []).map((item) => `${item.global ? '全部模型' : item.model || '-'} ${item.remainingSecs}s${item.reason ? ` ${item.reason}` : ''}`).join('\n')}>
                冷却 {credential.cooldownRemainingSecs}s
              </Badge>
            )}
            {!credential.disabled && credential.rateLimited && <Badge tone="warning">限流 {credential.rateLimitRemainingSecs}s</Badge>}
            {transientFailureStreak > 0 && <Badge tone="error">错误 {transientFailureStreak}</Badge>}
          </div>
        </button>
        <div className="flex shrink-0 items-center gap-1 pt-0.5">
          <Button type="button" variant="ghost" size="icon-sm" onClick={() => setDetailsOpen((v) => !v)} title={detailsOpen ? '关闭详情' : '查看详情'}>
            {detailsOpen ? <ChevronUp className="h-3.5 w-3.5" /> : <ChevronDown className="h-3.5 w-3.5" />}
          </Button>
          <Switch
            checked={!credential.disabled}
            disabled={setDisabled.isPending}
            onCheckedChange={() => setDisabled.mutate({ id: credential.id, disabled: !credential.disabled }, {
              onSuccess: (res) => toast.success(res.message),
              onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
            })}
          />
        </div>
      </div>

      {/* Summary row */}
      <div className="grid grid-cols-5 gap-2 border-t border-border/50 px-3 py-2">
        <SummaryItem label="优先级" value={credential.priority} />
        <SummaryItem label="并发" value={`${credential.inFlightRequests}${credential.maxConcurrentRequests > 0 ? `/${credential.maxConcurrentRequests}` : ' / 不限'}`} error={credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests} />
        <SummaryItem label="调度" value={dispatchStatus} error={dispatchStatus !== '可调度'} />
        <SummaryItem label="成功请求" value={formatNumber(credential.successCount)} />
        <SummaryItem label="最近使用" value={formatLastUsed(credential.lastUsedAt)} />
      </div>

      {/* Action buttons */}
      <div className="flex flex-wrap gap-1 border-t border-border/50 px-2 py-1.5">
        <Button type="button" variant="ghost" size="xs" onClick={() => onTest(credential)}><Wand2 className="h-3.5 w-3.5" /> 测试</Button>
        <Button type="button" variant="ghost" size="xs" onClick={() => onQueryBalance(credential.id)} disabled={loadingBalance}>
          {loadingBalance ? <Spinner size="sm" /> : <Wallet className="h-3.5 w-3.5" />} 查询信息
        </Button>
        <Button type="button" variant="ghost" size="xs" onClick={handleForceRefresh} disabled={forceRefresh.isPending || credential.authMethod === 'api_key'} title={credential.authMethod === 'api_key' ? 'API Key 账号无需刷新 Token' : '强制刷新 Token'}>
          <RefreshCw className={`h-3.5 w-3.5 ${forceRefresh.isPending ? 'animate-spin' : ''}`} /> 刷新Token
        </Button>
        <Button type="button" variant="ghost" size="xs" disabled={resetFailure.isPending || !hasFailures} onClick={() => resetFailure.mutate(credential.id, { onSuccess: (res) => toast.success(res.message), onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`) })}>
          <RotateCcw className="h-3.5 w-3.5" /> 恢复异常
        </Button>
        <Button type="button" variant="ghost" size="xs" disabled={setPriority.isPending || credential.priority === 0} onClick={() => adjustPriority(-1)} title="提高优先级">
          <ChevronUp className="h-3.5 w-3.5" /> 优先级
        </Button>
        <Button type="button" variant="ghost" size="xs" disabled={setPriority.isPending} onClick={() => adjustPriority(1)} title="降低优先级">
          <ChevronDown className="h-3.5 w-3.5" /> 优先级
        </Button>
        <Button type="button" variant="ghost" size="xs" onClick={() => setWarmup.mutate({ id: credential.id, warmupRemaining: credential.warmupRemaining > 0 ? 0 : Math.max(1, warmupTarget) }, { onSuccess: () => toast.success(credential.warmupRemaining > 0 ? '已关闭预热' : '已开启预热'), onError: (error) => toast.error(`预热设置失败: ${extractErrorMessage(error)}`) })}>
          {credential.warmupRemaining > 0 ? '关闭预热' : '开启预热'}
        </Button>
        <Button type="button" variant="ghost" size="xs" disabled={clearInFlight.isPending || !canClearInFlight} onClick={async () => {
          if (!canClearInFlight) return
          const confirmed = await confirmDialog({ title: '清理并发占用', message: `确定清理账号 #${credential.id} 的当前并发占用吗？真实仍在运行的请求可能因此不再计入并发限制。`, confirmText: '清理', tone: 'danger' })
          if (!confirmed) return
          clearInFlight.mutate({ id: credential.id }, { onSuccess: (res) => toast.success(res.message), onError: (error) => toast.error(`清理失败: ${extractErrorMessage(error)}`) })
        }}>清理并发</Button>
        <Button type="button" variant="ghost" size="xs" className="text-destructive hover:bg-destructive/10 hover:text-destructive" disabled={!credential.disabled} onClick={() => { if (!credential.disabled) return; setShowDeleteConfirm(true) }}>
          <Trash2 className="h-3.5 w-3.5" /> 删除
        </Button>
      </div>

      {/* Details Modal */}
      <ModalShell open={detailsOpen} title={`账号详情：${credentialLabel(credential)}`} width="max-w-5xl" onClose={() => setDetailsOpen(false)}>
        <div className="space-y-4">
          <div>
            <div className="mb-2 text-xs font-semibold text-muted-foreground uppercase tracking-wide">基础</div>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <MetaItem label="优先级" value={<button type="button" className="font-semibold text-primary hover:underline" onClick={() => setEditingPriority(true)}>{credential.priority}</button>} />
              <MetaItem label="失败/刷新失败" value={`${credential.failureCount} / ${credential.refreshFailureCount}`} error={credential.failureCount > 0 || credential.refreshFailureCount > 0} />
              <MetaItem label="成功请求" value={formatNumber(credential.successCount)} />
              <MetaItem label="最近使用" value={formatLastUsed(credential.lastUsedAt)} />
              {credential.email && credential.maskedApiKey && <MetaItem label="API Key" value={<TruncatedValue value={credential.maskedApiKey} mono />} />}
              <MetaItem label="创建时间" value={formatFullDate(credential.createdAt)} />
              <MetaItem label="更新时间" value={formatFullDate(credential.updatedAt)} />
            </div>
          </div>

          <div>
            <div className="mb-2 text-xs font-semibold text-muted-foreground uppercase tracking-wide">调度</div>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <MetaItem label="状态" value={dispatchStatus} error={dispatchStatus !== '可调度'} />
              <MetaItem label="近期错误率" value={`${(recentErrorRate * 100).toFixed(1)}%`} error={recentErrorRate > 0} />
              <MetaItem label="耗时 EWMA" value={credential.latencyEwmaMs == null ? '未知' : `${Math.round(credential.latencyEwmaMs)}ms`} />
              <MetaItem label="调度评分" value={schedulerScore.toFixed(2)} />
              <MetaItem label="总调度" value={formatNumber(schedulerSelectionCount)} />
              <MetaItem label="近期调度" value={`${formatNumber(recentSelection60s)}/60s`} detail={`10s ${formatNumber(recentSelection10s)} · 5m ${formatNumber(recentSelection5m)}`} />
              <MetaItem label="调度压力" value={schedulerSelectionPressure.toFixed(2)} error={schedulerSelectionPressure > 1} />
              <MetaItem label="并发" value={
                <button type="button" className="group block min-w-0 text-left text-primary hover:underline" onClick={() => setEditingConcurrency(true)}>
                  <span className="flex items-center gap-1 font-semibold"><Gauge className="h-3 w-3 shrink-0" /><span className="whitespace-nowrap">{credential.inFlightRequests}{credential.maxConcurrentRequests > 0 ? `/${credential.maxConcurrentRequests}` : ' / 不限'}</span></span>
                  <span className="mt-0.5 block truncate text-xs font-medium text-muted-foreground">{concurrencyLimitLabel(credential)}</span>
                  {credential.inFlightRequests > 0 && <span className="mt-0.5 block truncate text-xs font-medium text-muted-foreground">最老 {credential.oldestInFlightAgeSecs}s · 闲置 {credential.newestInFlightIdleSecs}s</span>}
                </button>
              } error={credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests} />
              <MetaItem label="Lease 回收" value={credential.inFlightLeaseMaxSecs > 0 ? `${credential.inFlightLeaseMaxSecs}s` : '-'} />
              {credential.warmupRemaining > 0 && <MetaItem label="预热剩余" value={credential.warmupRemaining} />}
              {credential.inProbation && <MetaItem label="观察期" value={`${probationRemainingSecs}s`} />}
              {credential.cooldownReason && <MetaItem label="冷却原因" value={<TruncatedValue value={credential.cooldownReason} />} error />}
            </div>
          </div>

          <div>
            <div className="mb-2 text-xs font-semibold text-muted-foreground uppercase tracking-wide">额度与费用</div>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <MetaItem label="额度" value={loadingBalance ? <Spinner size="sm" /> : accountInfo ? `${formatQuota(accountInfo.currentUsage)}/${formatQuota(accountInfo.usageLimit)}` : '未知'} detail={quotaDetail} />
              <MetaItem label="剩余积分" value={loadingBalance ? <Spinner size="sm" /> : accountInfo ? formatCredits(accountInfo.creditRemaining) : '未知'} detail={accountInfo ? `总额 ${formatCredits(accountInfo.creditLimit)}` : undefined} />
              <MetaItem label="估算成本" value={formatUsd(credential.estimatedCostUsd)} />
              {hasPricingCoverage && <MetaItem label="计价请求" value={pricedCoverageLabel(credential)} error={credential.unpricedRequests > 0} />}
            </div>
          </div>

          <div>
            <div className="mb-2 text-xs font-semibold text-muted-foreground uppercase tracking-wide">网络</div>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <MetaItem label="Auth Region" value={<button type="button" className="font-mono text-primary hover:underline" onClick={() => setEditingRegions(true)}>{credential.effectiveAuthRegion || '-'}</button>} detail={credential.authRegion || credential.region ? '账号覆盖' : '继承全局'} />
              <MetaItem label="API Region" value={<button type="button" className="font-mono text-primary hover:underline" onClick={() => setEditingRegions(true)}>{credential.effectiveApiRegion || '-'}</button>} detail={credential.apiRegion ? '账号覆盖' : '继承全局'} />
              <MetaItem label="代理" value={<button type="button" className="flex min-w-0 max-w-full items-center gap-1 text-primary hover:underline" onClick={openProxyEditor}><Router className="h-3 w-3 shrink-0" /><span className="truncate">{proxySummary(credential)}</span></button>} />
            </div>
          </div>

          {credential.cooldowns && credential.cooldowns.length > 0 && (
            <div className="flex flex-wrap gap-1.5 text-xs text-muted-foreground">
              {credential.cooldowns.map((cooldown) => (
                <span key={`${cooldown.global ? 'global' : cooldown.model}-${cooldown.remainingSecs}`} className="rounded-lg border border-border bg-muted px-2 py-1" title={cooldown.reason || undefined}>
                  {cooldown.global ? '全部模型' : cooldown.model || '-'} · 冷却 {cooldown.remainingSecs}s
                </span>
              ))}
            </div>
          )}

          {credential.lastErrorReason && (
            <div className="rounded-lg border border-border bg-muted/40 p-2 text-xs">
              <span className="mr-1 inline-block h-3 w-1 rounded-full bg-destructive align-[-1px]" />
              <span className="font-semibold text-destructive">最近错误{lastTransientErrorAgo ? ` (${lastTransientErrorAgo})` : ''}：</span>
              <span className="text-muted-foreground">{credential.lastErrorKind}: {credential.lastErrorReason}</span>
            </div>
          )}
        </div>
      </ModalShell>

      {/* Priority Edit Modal */}
      <ModalShell open={editingPriority} title={`优先级：${credentialLabel(credential)}`} width="max-w-md" onClose={() => { setEditingPriority(false); setPriorityValue(String(credential.priority)) }}>
        <div className="space-y-3">
          <div className="rounded-lg border border-border bg-muted/40 p-3 text-sm">
            <div className="flex items-center justify-between gap-3"><span className="text-muted-foreground">当前优先级</span><span className="font-semibold">{credential.priority}</span></div>
            <div className="mt-1 text-xs text-muted-foreground">数值越小优先级越高；不能小于 0。</div>
          </div>
          <Field label="新优先级">
            <Input type="number" min={0} value={priorityValue} disabled={setPriority.isPending} onChange={(e) => setPriorityValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" size="sm" onClick={() => { setEditingPriority(false); setPriorityValue(String(credential.priority)) }} disabled={setPriority.isPending}>取消</Button>
            <Button type="button" size="sm" onClick={savePriority} disabled={setPriority.isPending}>{setPriority.isPending && <Spinner size="sm" />}保存</Button>
          </div>
        </div>
      </ModalShell>

      {/* Regions Edit Modal */}
      <ModalShell open={editingRegions} title={`Region：${credentialLabel(credential)}`} width="max-w-lg" onClose={() => { if (setCredentialRegions.isPending) return; setEditingRegions(false); setRegionValue(credential.region || ''); setAuthRegionValue(credential.authRegion || ''); setApiRegionValue(credential.apiRegion || '') }}>
        <div className="space-y-3">
          <div className="rounded-lg border border-border bg-muted/40 p-3 text-sm">
            <div className="grid gap-2 sm:grid-cols-2">
              <div><div className="text-xs text-muted-foreground">当前 Auth Region</div><div className="font-mono font-semibold">{credential.effectiveAuthRegion || '-'}</div></div>
              <div><div className="text-xs text-muted-foreground">当前 API Region</div><div className="font-mono font-semibold">{credential.effectiveApiRegion || '-'}</div></div>
            </div>
          </div>
          <Field label="Region 兼容字段" description="未设置 Auth Region 时会作为 token 刷新的回退字段。">
            <Input className="font-mono" value={regionValue} disabled={setCredentialRegions.isPending} onChange={(e) => setRegionValue(e.target.value)} placeholder="留空继承全局" />
          </Field>
          <Field label="Auth Region">
            <Input className="font-mono" value={authRegionValue} disabled={setCredentialRegions.isPending} onChange={(e) => setAuthRegionValue(e.target.value)} placeholder="如 us-east-1，留空继承" />
          </Field>
          <Field label="API Region">
            <Input className="font-mono" value={apiRegionValue} disabled={setCredentialRegions.isPending} onChange={(e) => setApiRegionValue(e.target.value)} placeholder="如 us-east-1，留空继承" />
          </Field>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" size="sm" onClick={() => setEditingRegions(false)} disabled={setCredentialRegions.isPending}>取消</Button>
            <Button type="button" variant="ghost" size="sm" disabled={setCredentialRegions.isPending || (!credential.region && !credential.authRegion && !credential.apiRegion)} onClick={() => {
              setRegionValue(''); setAuthRegionValue(''); setApiRegionValue('')
              setCredentialRegions.mutate({ id: credential.id, request: { region: null, authRegion: null, apiRegion: null } }, { onSuccess: (res) => { toast.success(res.message); setEditingRegions(false) }, onError: (error) => toast.error(`Region 设置失败: ${extractErrorMessage(error)}`) })
            }}>清空覆盖</Button>
            <Button type="button" size="sm" onClick={saveRegions} disabled={setCredentialRegions.isPending}>{setCredentialRegions.isPending && <Spinner size="sm" />}保存</Button>
          </div>
        </div>
      </ModalShell>

      {/* Concurrency Edit Modal */}
      <ModalShell open={editingConcurrency} title={`并发限制：${credentialLabel(credential)}`} width="max-w-lg" onClose={() => setEditingConcurrency(false)}>
        <div className="space-y-3">
          <div className="rounded-lg border border-border bg-muted/40 p-3 text-sm">
            <div className="flex items-center justify-between gap-3"><span className="text-muted-foreground">当前生效</span><span className="font-semibold">{credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests} 并发` : '不限并发'}</span></div>
            <div className="mt-1 text-xs text-muted-foreground">{typeof credential.maxConcurrentRequestsOverride === 'number' ? '当前账号已覆盖全局配置。' : '当前账号继承全局配置。'}</div>
          </div>
          <Field label="账号级最大并发" description="留空表示继承全局；填 0 表示该账号不限并发；填正整数表示该账号自己的并发上限。">
            <Input type="number" min={0} value={concurrencyValue} placeholder="留空继承全局，0 表示不限" disabled={setCredentialConcurrency.isPending} onChange={(e) => setConcurrencyValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" size="sm" onClick={() => setEditingConcurrency(false)} disabled={setCredentialConcurrency.isPending}>取消</Button>
            <Button type="button" variant="ghost" size="sm" disabled={setCredentialConcurrency.isPending || typeof credential.maxConcurrentRequestsOverride !== 'number'} onClick={() => {
              setConcurrencyValue('')
              setCredentialConcurrency.mutate({ id: credential.id, request: { maxConcurrentRequests: null } }, { onSuccess: (res) => { toast.success(res.message); setEditingConcurrency(false) }, onError: (error) => toast.error(`并发限制设置失败: ${extractErrorMessage(error)}`) })
            }}>继承全局</Button>
            <Button type="button" size="sm" onClick={saveConcurrency} disabled={setCredentialConcurrency.isPending}>{setCredentialConcurrency.isPending && <Spinner size="sm" />}保存</Button>
          </div>
        </div>
      </ModalShell>

      {/* Delete Confirm Modal */}
      <ModalShell open={showDeleteConfirm} title={`确认删除账号 #${credential.id}`} width="max-w-md" onClose={() => setShowDeleteConfirm(false)}>
        <div className="space-y-3 text-sm">
          <p>此操作会永久删除该账号，无法撤销。</p>
          <div className="rounded-lg border border-border bg-muted/40 p-3">
            <div className="font-semibold">{credentialLabel(credential)}</div>
            <div className="mt-1 text-xs text-muted-foreground">只有已禁用账号允许删除。</div>
          </div>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" size="sm" onClick={() => setShowDeleteConfirm(false)} disabled={deleteCredential.isPending}>取消</Button>
            <Button type="button" variant="destructive" size="sm" onClick={handleDelete} disabled={deleteCredential.isPending || !credential.disabled}>
              {deleteCredential.isPending && <Spinner size="sm" />}确认删除
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* Proxy Edit Modal */}
      <ModalShell open={editingProxy} title={`绑定代理：${credentialLabel(credential)}`} width="max-w-xl" onClose={closeProxyEditor}>
        <div className="space-y-3">
          <button type="button" className={`w-full rounded-lg border p-3 text-left text-sm transition ${proxyResourceId ? 'border-border hover:bg-muted' : 'border-primary bg-primary/5'}`} onClick={() => { setProxyResourceId('') }}>
            <div className="flex items-center justify-between"><span className="font-semibold">不绑定代理资源</span>{!proxyResourceId && <Badge tone="primary">已选</Badge>}</div>
            <div className="mt-1 text-xs text-muted-foreground">不绑定代理资源时可以使用下面的账号直连代理。</div>
          </button>
          <div className={`rounded-lg border p-3 ${proxyResourceId ? 'border-border bg-muted/40 opacity-70' : 'border-border bg-card'}`}>
            <div className="mb-3">
              <div className="text-sm font-semibold">账号直连代理</div>
              <div className="mt-1 text-xs leading-4 text-muted-foreground">不绑定代理资源时生效；选择代理资源保存后会清除这些直连代理字段。</div>
            </div>
            <FieldGrid>
              <Field label="代理 URL">
                <Input value={proxyUrl} placeholder="socks5h://127.0.0.1:1080" disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)} onChange={(e) => { if (e.target.value.trim()) setProxyResourceId(''); setProxyUrl(e.target.value) }} />
              </Field>
              <Field label="代理用户名">
                <SecretInput value={proxyUsername} onChange={(v) => { if (v.trim()) setProxyResourceId(''); setProxyUsername(v) }} visible={showProxyUsername} onToggle={() => setShowProxyUsername((v) => !v)} disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)} placeholder="可选" />
              </Field>
              <Field label="代理密码">
                <SecretInput value={proxyPassword} onChange={(v) => { if (v.trim()) setProxyResourceId(''); setProxyPassword(v) }} visible={showProxyPassword} onToggle={() => setShowProxyPassword((v) => !v)} disabled={setCredentialProxy.isPending || Boolean(proxyResourceId)} placeholder="可选" />
              </Field>
            </FieldGrid>
          </div>
          {proxyResources.isLoading ? (
            <LoadingState text="加载代理资源..." />
          ) : proxyResourceOptions.length === 0 ? (
            <EmptyState title="暂无代理资源" description="请先在代理页新增" />
          ) : (
            <div className="max-h-64 space-y-2 overflow-y-auto scrollbar-thin">
              {proxyResourceOptions.map((resource) => {
                const isSelected = proxyResourceId === String(resource.id)
                return (
                  <button key={resource.id} type="button" className={`w-full rounded-lg border p-3 text-left text-sm transition ${isSelected ? 'border-primary bg-primary/5' : resource.enabled ? 'border-border hover:bg-muted' : 'border-destructive/25 bg-destructive/5 opacity-70'}`} onClick={() => { setProxyResourceId(String(resource.id)); setProxyUrl(''); setProxyUsername(''); setProxyPassword('') }}>
                    <div className="flex items-center gap-2">
                      <span className="font-semibold">{resource.name}</span>
                      <Badge>#{resource.id}</Badge>
                      <Badge tone={resource.enabled ? 'success' : 'error'}>{resource.enabled ? '启用' : '禁用'}</Badge>
                      {isSelected && <Badge tone="primary">已选</Badge>}
                    </div>
                    <div className="mt-1 truncate text-xs text-muted-foreground">{resource.proxyUrl}</div>
                  </button>
                )
              })}
            </div>
          )}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" size="sm" onClick={closeProxyEditor} disabled={setCredentialProxy.isPending}>取消</Button>
            <Button type="button" size="sm" onClick={saveProxy} disabled={setCredentialProxy.isPending}>{setCredentialProxy.isPending && <Spinner size="sm" />}保存</Button>
          </div>
        </div>
      </ModalShell>
    </div>
  )
}
