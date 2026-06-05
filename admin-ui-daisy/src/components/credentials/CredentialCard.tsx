import {
  ChevronDown,
  ChevronUp,
  Gauge,
  MoreHorizontal,
  RefreshCw,
  RotateCcw,
  Router,
  Trash2,
  Wallet,
  Wand2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Button, Card, Checkbox, Dropdown, Input, Join, Loading, Modal, Select, Toggle } from 'react-daisyui'
import { forceRefreshToken, getCredentialInfo } from '@/api/credentials'
import { Badge, EmptyState, LoadingState, ModalShell } from '@/components/ui'
import { formatApproxElapsedMs, formatDate, formatLastUsed, formatNumber, formatQuota, formatUsd } from '@/lib/format'
import { extractErrorMessage } from '@/lib/utils'
import {
  useClearInFlight,
  useDeleteCredential,
  useProxyResources,
  useResetFailure,
  useRuntimeConfig,
  useSetCredentialProxy,
  useSetCredentialConcurrency,
  useSetDisabled,
  useSetPriority,
  useSetWarmup,
} from '@/hooks/use-credentials'
import type { BalanceResponse, CredentialStatusItem } from '@/types/api'

// ============================================================================
// Helper Functions
// ============================================================================

function credentialLabel(credential: CredentialStatusItem) {
  return credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
}

function authLabel(authMethod: string | null) {
  if (authMethod === 'api_key') return 'API Key'
  if (authMethod === 'idc') return 'IdC'
  if (authMethod === 'social') return 'Social'
  return authMethod || 'Unknown'
}

function subscriptionLabel(credential: CredentialStatusItem, balance?: BalanceResponse) {
  return balance?.subscriptionTitle || credential.accountInfo?.subscriptionTitle || credential.subscriptionTitle || '未知'
}

function accountInfoValue(credential: CredentialStatusItem, balance?: BalanceResponse) {
  return balance || credential.accountInfo
}

function numberOrZero(value: number | null | undefined): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}

function sourceLabel(source?: CredentialStatusItem['effectiveProxySource']) {
  const labels: Record<string, string> = {
    credential: '直接代理',
    resource: '代理资源',
    resource_disabled: '代理已禁用',
    resource_missing: '代理不存在',
    global: '全局代理',
    direct: '直连',
  }
  return labels[source || ''] || '未配置'
}

function concurrencyLimitLabel(credential: CredentialStatusItem) {
  const effective = credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests}` : '不限'
  if (typeof credential.maxConcurrentRequestsOverride === 'number') {
    return credential.maxConcurrentRequestsOverride > 0
      ? `账号覆盖：${credential.maxConcurrentRequestsOverride}`
      : '账号覆盖：不限'
  }
  return `继承全局：${effective}`
}

// ============================================================================
// Credential Card Component
// ============================================================================

interface CredentialCardProps {
  credential: CredentialStatusItem
  selected: boolean
  expanded: boolean
  onToggleSelect: () => void
  onToggleExpand: () => void
  onQueryBalance: (id: number) => void
  onTest: (credential: CredentialStatusItem) => void
  balance?: BalanceResponse
  loadingBalance: boolean
}

export function CredentialCard({
  credential,
  selected,
  expanded,
  onToggleSelect,
  onToggleExpand,
  onQueryBalance,
  onTest,
  balance,
  loadingBalance,
}: CredentialCardProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [editingProxy, setEditingProxy] = useState(false)
  const [editingConcurrency, setEditingConcurrency] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [proxyResourceId, setProxyResourceId] = useState(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
  const [concurrencyValue, setConcurrencyValue] = useState(
    typeof credential.maxConcurrentRequestsOverride === 'number'
      ? String(credential.maxConcurrentRequestsOverride)
      : ''
  )

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const setCredentialProxy = useSetCredentialProxy()
  const setCredentialConcurrency = useSetCredentialConcurrency()
  const proxyResources = useProxyResources()
  const proxyResourceOptions = proxyResources.data?.resources || []
  const resetFailure = useResetFailure()
  const deleteCredential = useDeleteCredential()
  const setWarmup = useSetWarmup()
  const clearInFlight = useClearInFlight()
  const runtimeConfig = useRuntimeConfig()
  const queryClient = useQueryClient()

  const warmupTarget = Math.max(0, runtimeConfig.data?.credentialWarmupRequests ?? 3)
  const accountInfo = accountInfoValue(credential, balance)
  const transientFailureStreak = numberOrZero(credential.transientFailureStreak)
  const probationRemainingSecs = numberOrZero(credential.probationRemainingSecs)
  const recentErrorRate = numberOrZero(credential.recentErrorRate)
  const schedulerScore = numberOrZero(credential.schedulerScore)
  const lastTransientErrorAgo = formatApproxElapsedMs(credential.lastErrorAtMs)

  useEffect(() => {
    setPriorityValue(String(credential.priority))
  }, [credential.priority])

  useEffect(() => {
    setProxyResourceId(credential.proxyResourceId ? String(credential.proxyResourceId) : '')
  }, [credential.id, credential.proxyResourceId])

  useEffect(() => {
    setConcurrencyValue(
      typeof credential.maxConcurrentRequestsOverride === 'number'
        ? String(credential.maxConcurrentRequestsOverride)
        : ''
    )
  }, [credential.id, credential.maxConcurrentRequestsOverride])

  const savePriority = () => {
    const priority = Number(priorityValue)
    if (!Number.isInteger(priority) || priority < 0) {
      toast.error('优先级必须是非负整数')
      return
    }
    setPriority.mutate(
      { id: credential.id, priority },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingPriority(false)
        },
        onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const saveProxy = () => {
    setCredentialProxy.mutate(
      {
        id: credential.id,
        request: { proxyResourceId: proxyResourceId ? Number(proxyResourceId) : null },
      },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingProxy(false)
        },
        onError: (error) => toast.error(`代理设置失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const saveConcurrency = () => {
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
      { id: credential.id, request: { maxConcurrentRequests } },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingConcurrency(false)
        },
        onError: (error) => toast.error(`并发限制设置失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const handleDelete = () => {
    if (!credential.disabled) {
      toast.error('请先禁用凭据再删除')
      return
    }
    if (!confirm(`确定删除凭据 #${credential.id} 吗？此操作无法撤销。`)) return
    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (error) => toast.error(`删除失败: ${extractErrorMessage(error)}`),
    })
  }

  const handleForceRefresh = async () => {
    try {
      const res = await forceRefreshToken(credential.id)
      toast.success(res.message)
      queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    } catch (error) {
      toast.error(`刷新失败: ${extractErrorMessage(error)}`)
    }
  }

  return (
    <Card className={`credential-card ${credential.isCurrent ? 'is-current' : ''} ${credential.disabled ? 'is-disabled' : ''}`}>
      <Card.Body className="gap-0 p-0">
        {/* Compact Header - Always Visible */}
        <div className="flex items-center gap-3 p-3">
          <Checkbox size="xs" checked={selected} onChange={onToggleSelect} />

          <button
            type="button"
            className="min-w-0 flex-1 text-left"
            onClick={onToggleExpand}
          >
            <div className="flex items-center gap-2">
              <span className="truncate text-sm font-semibold" title={credentialLabel(credential)}>
                {credentialLabel(credential)}
              </span>
              <Badge size="xs">#{credential.id}</Badge>
              {credential.isCurrent && <Badge tone="primary" size="xs" dot>当前</Badge>}
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-1">
              <Badge tone={credential.disabled ? 'error' : 'success'} size="xs">
                {credential.disabled ? '禁用' : '启用'}
              </Badge>
              <Badge size="xs">{authLabel(credential.authMethod)}</Badge>
              <Badge size="xs">{loadingBalance ? '...' : subscriptionLabel(credential, balance)}</Badge>
              {credential.hasProxy && <Badge tone="info" size="xs">代理</Badge>}
              {typeof credential.maxConcurrentRequestsOverride === 'number' && (
                <Badge tone="info" size="xs">并发覆盖</Badge>
              )}
              {!credential.disabled && credential.cooledDown && (
                <Badge tone="warning" size="xs">冷却 {credential.cooldownRemainingSecs}s</Badge>
              )}
              {!credential.disabled && credential.rateLimited && (
                <Badge tone="warning" size="xs">限流</Badge>
              )}
              {transientFailureStreak > 0 && (
                <Badge tone="error" size="xs">错误 {transientFailureStreak}</Badge>
              )}
            </div>
          </button>

          <div className="flex shrink-0 items-center gap-1">
            <Toggle
              color="primary"
              size="xs"
              checked={!credential.disabled}
              disabled={setDisabled.isPending}
              onChange={() =>
                setDisabled.mutate(
                  { id: credential.id, disabled: !credential.disabled },
                  {
                    onSuccess: (res) => toast.success(res.message),
                    onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
                  }
                )
              }
            />
            <Button
              type="button"
              color="ghost"
              size="xs"
              shape="square"
              onClick={onToggleExpand}
            >
              {expanded ? <ChevronUp className="h-4 w-4" /> : <ChevronDown className="h-4 w-4" />}
            </Button>
          </div>
        </div>

        {/* Expanded Details */}
        {expanded && (
          <div className="animate-slide-down border-t border-base-300/50 bg-base-200/30 p-3">
            {/* Stats Grid */}
            <div className="credential-meta-grid">
              <MetaItem label="优先级" value={
                editingPriority ? (
                  <Join className="mt-0.5">
                    <Input bordered size="xs" className="join-item w-14" type="number" min={0} value={priorityValue} onChange={(e) => setPriorityValue(e.target.value)} />
                    <Button type="button" color="primary" size="xs" className="join-item" onClick={savePriority}>保存</Button>
                    <Button type="button" color="ghost" size="xs" className="join-item" onClick={() => setEditingPriority(false)}>取消</Button>
                  </Join>
                ) : (
                  <button type="button" className="font-semibold text-primary hover:underline" onClick={() => setEditingPriority(true)}>
                    {credential.priority}
                  </button>
                )
              } />
              <MetaItem label="失败/刷新失败" value={`${credential.failureCount} / ${credential.refreshFailureCount}`} error={credential.failureCount > 0 || credential.refreshFailureCount > 0} />
              <MetaItem label="成功请求" value={formatNumber(credential.successCount)} />
              <MetaItem label="近期错误率" value={`${(recentErrorRate * 100).toFixed(1)}%`} error={recentErrorRate > 0} />
              <MetaItem label="调度评分" value={schedulerScore.toFixed(2)} />
              <MetaItem label="最近使用" value={formatLastUsed(credential.lastUsedAt)} />
              <MetaItem label="额度" value={
                loadingBalance ? <Loading size="xs" /> : accountInfo ? `${formatQuota(accountInfo.currentUsage)}/${formatQuota(accountInfo.usageLimit)}` : '未知'
              } />
              <MetaItem label="估算成本" value={formatUsd(credential.estimatedCostUsd)} />
              <MetaItem
                label="并发"
                value={
                  <button type="button" className="flex items-center gap-1 text-primary hover:underline" onClick={() => setEditingConcurrency(true)}>
                    <Gauge className="h-3 w-3" />
                    {credential.inFlightRequests}
                    {credential.maxConcurrentRequests > 0 ? `/${credential.maxConcurrentRequests}` : ' / 不限'}
                    <span className="text-xs text-base-content/45">· {concurrencyLimitLabel(credential)}</span>
                  </button>
                }
                error={credential.maxConcurrentRequests > 0 && credential.inFlightRequests >= credential.maxConcurrentRequests}
              />
              {credential.warmupRemaining > 0 && (
                <MetaItem label="预热剩余" value={credential.warmupRemaining} />
              )}
              {credential.inProbation && (
                <MetaItem label="观察期" value={`${probationRemainingSecs}s`} />
              )}
              <MetaItem label="代理" value={
                <button type="button" className="flex items-center gap-1 text-primary hover:underline" onClick={() => setEditingProxy(true)}>
                  <Router className="h-3 w-3" />
                  {credential.proxyResourceName || sourceLabel(credential.effectiveProxySource)}
                </button>
              } />
            </div>

            {/* Error Info */}
            {credential.lastErrorReason && (
              <div className="mt-3 rounded-lg border border-error/20 bg-error/5 p-2 text-xs">
                <span className="font-semibold text-error">最近错误{lastTransientErrorAgo ? ` (${lastTransientErrorAgo})` : ''}：</span>
                <span className="text-error/80">{credential.lastErrorKind}: {credential.lastErrorReason}</span>
              </div>
            )}

            {/* Actions */}
            <div className="credential-actions">
              <Button type="button" color="ghost" size="xs" onClick={() => onTest(credential)}>
                <Wand2 className="h-3.5 w-3.5" /> 测试
              </Button>
              <Button type="button" color="ghost" size="xs" onClick={() => onQueryBalance(credential.id)} disabled={loadingBalance}>
                {loadingBalance ? <Loading size="xs" /> : <Wallet className="h-3.5 w-3.5" />}
                查询信息
              </Button>
              <Button type="button" color="ghost" size="xs" onClick={handleForceRefresh} disabled={credential.authMethod === 'api_key'}>
                <RefreshCw className="h-3.5 w-3.5" /> 刷新Token
              </Button>
              <Button
                type="button"
                color="ghost"
                size="xs"
                onClick={() => resetFailure.mutate(credential.id, {
                  onSuccess: (res) => toast.success(res.message),
                  onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
                })}
              >
                <RotateCcw className="h-3.5 w-3.5" /> 恢复异常
              </Button>
              <Dropdown end>
                <Dropdown.Toggle button={false}>
                  <Button type="button" color="ghost" size="xs">
                    <MoreHorizontal className="h-3.5 w-3.5" />
                  </Button>
                </Dropdown.Toggle>
                <Dropdown.Menu className="w-40 rounded-lg border border-base-300 bg-base-100 p-1 shadow-lg">
                  <Dropdown.Item onClick={() => setWarmup.mutate(
                    { id: credential.id, warmupRemaining: credential.warmupRemaining > 0 ? 0 : Math.max(1, warmupTarget) },
                    {
                      onSuccess: () => toast.success(credential.warmupRemaining > 0 ? '已关闭预热' : '已开启预热'),
                      onError: (error) => toast.error(`预热设置失败: ${extractErrorMessage(error)}`),
                    }
                  )}>
                    {credential.warmupRemaining > 0 ? '关闭预热' : '开启预热'}
                  </Dropdown.Item>
                  <Dropdown.Item onClick={() => {
                    if (!confirm(`确定清理凭据 #${credential.id} 的当前并发占用吗？`)) return
                    clearInFlight.mutate({ id: credential.id }, {
                      onSuccess: (res) => toast.success(res.message),
                      onError: (error) => toast.error(`清理失败: ${extractErrorMessage(error)}`),
                    })
                  }}>
                    清理并发
                  </Dropdown.Item>
                  <Dropdown.Item className="text-error" onClick={handleDelete}>
                    <Trash2 className="h-3.5 w-3.5" /> 删除
                  </Dropdown.Item>
                </Dropdown.Menu>
              </Dropdown>
            </div>
          </div>
        )}
      </Card.Body>

      <ModalShell open={editingConcurrency} title={`并发限制：${credentialLabel(credential)}`} width="max-w-lg" onClose={() => setEditingConcurrency(false)}>
        <div className="space-y-3">
          <div className="rounded-lg border border-base-300 bg-base-200/60 p-3 text-sm">
            <div className="flex items-center justify-between gap-3">
              <span className="text-base-content/60">当前生效</span>
              <span className="font-semibold">
                {credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests} 并发` : '不限并发'}
              </span>
            </div>
            <div className="mt-1 text-xs text-base-content/55">
              {typeof credential.maxConcurrentRequestsOverride === 'number'
                ? '当前账号已覆盖全局配置。'
                : '当前账号继承全局配置。'}
            </div>
          </div>
          <label className="block">
            <span className="text-sm font-semibold">账号级最大并发</span>
            <Input
              bordered
              size="sm"
              type="number"
              min={0}
              className="mt-2"
              value={concurrencyValue}
              placeholder="留空继承全局，0 表示不限"
              disabled={setCredentialConcurrency.isPending}
              onChange={(event) => setConcurrencyValue(event.target.value)}
            />
            <span className="mt-1 block text-xs text-base-content/55">
              留空表示继承全局“单凭据最大并发请求数”；填 0 表示该账号不限并发；填正整数表示该账号自己的并发上限。
            </span>
          </label>

          <Modal.Actions>
            <Button type="button" color="ghost" size="sm" onClick={() => setEditingConcurrency(false)} disabled={setCredentialConcurrency.isPending}>
              取消
            </Button>
            <Button
              type="button"
              color="ghost"
              size="sm"
              disabled={setCredentialConcurrency.isPending || typeof credential.maxConcurrentRequestsOverride !== 'number'}
              onClick={() => {
                setConcurrencyValue('')
                setCredentialConcurrency.mutate(
                  { id: credential.id, request: { maxConcurrentRequests: null } },
                  {
                    onSuccess: (res) => {
                      toast.success(res.message)
                      setEditingConcurrency(false)
                    },
                    onError: (error) => toast.error(`并发限制设置失败: ${extractErrorMessage(error)}`),
                  }
                )
              }}
            >
              继承全局
            </Button>
            <Button type="button" color="primary" size="sm" onClick={saveConcurrency} disabled={setCredentialConcurrency.isPending}>
              {setCredentialConcurrency.isPending && <Loading size="xs" />}
              保存
            </Button>
          </Modal.Actions>
        </div>
      </ModalShell>

      {/* Proxy Edit Modal */}
      <ModalShell open={editingProxy} title={`绑定代理：${credentialLabel(credential)}`} width="max-w-xl" onClose={() => setEditingProxy(false)}>
        <div className="space-y-3">
          <button
            type="button"
            className={`w-full rounded-lg border p-3 text-left text-sm transition ${proxyResourceId ? 'border-base-300 hover:bg-base-200' : 'border-primary bg-primary/5'}`}
            onClick={() => setProxyResourceId('')}
          >
            <div className="flex items-center justify-between">
              <span className="font-semibold">不绑定代理资源</span>
              {!proxyResourceId && <Badge tone="primary" size="xs">已选</Badge>}
            </div>
            <div className="mt-1 text-xs text-base-content/50">清除凭据上的代理资源绑定</div>
          </button>

          {proxyResources.isLoading ? (
            <LoadingState text="加载代理资源..." />
          ) : proxyResourceOptions.length === 0 ? (
            <EmptyState title="暂无代理资源" description="请先在代理页新增" />
          ) : (
            <div className="max-h-64 space-y-2 overflow-y-auto">
              {proxyResourceOptions.map((resource) => {
                const isSelected = proxyResourceId === String(resource.id)
                return (
                  <button
                    key={resource.id}
                    type="button"
                    className={`w-full rounded-lg border p-3 text-left text-sm transition ${
                      isSelected ? 'border-primary bg-primary/5' : resource.enabled ? 'border-base-300 hover:bg-base-200' : 'border-error/25 bg-error/5 opacity-70'
                    }`}
                    onClick={() => setProxyResourceId(String(resource.id))}
                  >
                    <div className="flex items-center gap-2">
                      <span className="font-semibold">{resource.name}</span>
                      <Badge size="xs">#{resource.id}</Badge>
                      <Badge tone={resource.enabled ? 'success' : 'error'} size="xs">{resource.enabled ? '启用' : '禁用'}</Badge>
                      {isSelected && <Badge tone="primary" size="xs">已选</Badge>}
                    </div>
                    <div className="mt-1 truncate text-xs text-base-content/50">{resource.proxyUrl}</div>
                  </button>
                )
              })}
            </div>
          )}

          <Modal.Actions>
            <Button type="button" color="ghost" size="sm" onClick={() => setEditingProxy(false)} disabled={setCredentialProxy.isPending}>
              取消
            </Button>
            <Button type="button" color="primary" size="sm" onClick={saveProxy} disabled={setCredentialProxy.isPending}>
              {setCredentialProxy.isPending && <Loading size="xs" />}
              保存
            </Button>
          </Modal.Actions>
        </div>
      </ModalShell>
    </Card>
  )
}

// ============================================================================
// Meta Item Component
// ============================================================================

function MetaItem({ label, value, error }: { label: string; value: React.ReactNode; error?: boolean }) {
  return (
    <div>
      <div className="text-[0.68rem] font-medium text-base-content/50">{label}</div>
      <div className={`text-sm font-semibold ${error ? 'text-error' : ''}`}>{value}</div>
    </div>
  )
}
