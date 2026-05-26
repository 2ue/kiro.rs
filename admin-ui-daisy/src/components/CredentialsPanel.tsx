import {
  CheckCircle2,
  ChevronDown,
  Download,
  FileUp,
  Loader2,
  Plus,
  RefreshCw,
  RotateCcw,
  Trash2,
  Upload,
  Wallet,
  Wand2,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Button, Card, Checkbox, Input, Join, Loading, Toggle } from 'react-daisyui'
import { forceRefreshToken, getCredentialBalance, getCredentials, testCredential } from '@/api/credentials'
import { Badge, EmptyState, ErrorState, FieldLabel, LoadingState, SectionCard, StatCard } from '@/components/common'
import {
  AddCredentialModal,
  BalanceModal,
  BatchImportModal,
  BatchVerifyModal,
  CredentialExportModal,
  CredentialTestModal,
  KamImportModal,
  type VerifyResult,
} from '@/components/CredentialDialogs'
import { formatLastUsed, formatNumber, formatUsd } from '@/lib/format'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, testModelLabel } from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import {
  useClearInFlight,
  useCredentials,
  useCredentialsPage,
  useDeleteCredential,
  useLoadBalancingMode,
  useResetFailure,
  useRuntimeConfig,
  useSetDisabled,
  useSetLoadBalancingMode,
  useSetPriority,
  useSetWarmup,
} from '@/hooks/use-credentials'
import type { BalanceResponse, CredentialStatusItem } from '@/types/api'

function credentialLabel(credential: CredentialStatusItem) {
  return credential.email || credential.maskedApiKey || `凭据 #${credential.id}`
}

function authLabel(authMethod: string | null) {
  if (authMethod === 'api_key') return 'API Key'
  if (authMethod === 'idc') return 'IdC'
  if (authMethod === 'social') return 'Social'
  return authMethod || 'Unknown'
}

function CredentialCard({
  credential,
  selected,
  onToggleSelect,
  onViewBalance,
  onTest,
  balance,
  loadingBalance,
}: {
  credential: CredentialStatusItem
  selected: boolean
  onToggleSelect: () => void
  onViewBalance: (id: number) => void
  onTest: (credential: CredentialStatusItem) => void
  balance?: BalanceResponse
  loadingBalance: boolean
}) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const resetFailure = useResetFailure()
  const deleteCredential = useDeleteCredential()
  const forceRefresh = forceRefreshToken
  const setWarmup = useSetWarmup()
  const clearInFlight = useClearInFlight()
  const runtimeConfig = useRuntimeConfig()
  const queryClient = useQueryClient()
  const warmupTarget = Math.max(0, runtimeConfig.data?.credentialWarmupRequests ?? 3)
  const balanceVisible = loadingBalance || Boolean(balance)

  useEffect(() => {
    setPriorityValue(String(credential.priority))
  }, [credential.priority])

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
      const res = await forceRefresh(credential.id)
      toast.success(res.message)
      queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    } catch (error) {
      toast.error(`刷新失败: ${extractErrorMessage(error)}`)
    }
  }

  return (
    <Card className={`credential-card transition ${credential.isCurrent ? 'is-current' : ''}`}>
      <Card.Body className="gap-3 p-3">
        <div className="flex items-start justify-between gap-3">
          <div className="flex min-w-0 gap-2.5">
            <Checkbox size="xs" className="mt-1" checked={selected} onChange={onToggleSelect} />
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-1.5">
                <h3 className="max-w-[260px] truncate text-sm font-semibold" title={credentialLabel(credential)}>
                  {credentialLabel(credential)}
                </h3>
                <Badge>#{credential.id}</Badge>
                {credential.isCurrent && <Badge tone="primary">当前</Badge>}
                <Badge tone={credential.disabled ? 'error' : 'success'}>{credential.disabled ? '已禁用' : '启用'}</Badge>
              </div>
              <div className="mt-1.5 flex flex-wrap gap-1">
                {credential.disabled && credential.disabledReason && <Badge tone="error">{credential.disabledReason}</Badge>}
                {!credential.disabled && credential.cooledDown && <Badge tone="warning">冷却 {credential.cooldownRemainingSecs}s</Badge>}
                {!credential.disabled && credential.rateLimited && <Badge tone="warning">限流 {credential.rateLimitRemainingSecs}s</Badge>}
                {!credential.disabled && credential.maxConcurrentRequests > 0 && (
                  <Badge tone={credential.inFlightRequests >= credential.maxConcurrentRequests ? 'error' : 'neutral'} title={`最老占用 ${credential.oldestInFlightAgeSecs}s，最近活跃 ${credential.newestInFlightIdleSecs}s 前`}>
                    并发 {credential.inFlightRequests}/{credential.maxConcurrentRequests}
                  </Badge>
                )}
                {!credential.disabled && credential.warmupRemaining > 0 && <Badge tone="secondary">预热 {credential.warmupRemaining}</Badge>}
                <Badge>{authLabel(credential.authMethod)}</Badge>
                {credential.endpoint && credential.endpoint !== 'ide' && <Badge>{credential.endpoint}</Badge>}
                {credential.hasProxy && <Badge tone="info">代理</Badge>}
              </div>
            </div>
          </div>
          <Toggle
            color="primary"
            size="sm"
            className="shrink-0"
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
        </div>

        <div className="credential-meta-grid">
          <div>
            <div className="text-[0.72rem] font-medium text-base-content/50">优先级</div>
            {editingPriority ? (
              <Join className="mt-1">
                <Input bordered size="xs" className="join-item w-20" type="number" min={0} value={priorityValue} onChange={(event) => setPriorityValue(event.target.value)} />
                <Button type="button" color="primary" size="xs" className="join-item" onClick={savePriority}>保存</Button>
                <Button type="button" color="ghost" size="xs" className="join-item" onClick={() => setEditingPriority(false)}>取消</Button>
              </Join>
            ) : (
              <Button type="button" color="ghost" size="xs" className="h-auto min-h-0 px-1 font-semibold" onClick={() => setEditingPriority(true)}>
                {credential.priority}
              </Button>
            )}
          </div>
          <div>
            <div className="text-[0.72rem] font-medium text-base-content/50">失败 / 刷新失败</div>
            <div className={credential.failureCount || credential.refreshFailureCount ? 'font-semibold text-error' : 'font-semibold'}>
              {credential.failureCount} / {credential.refreshFailureCount}
            </div>
          </div>
          <div>
            <div className="text-[0.72rem] font-medium text-base-content/50">成功请求</div>
            <div className="font-semibold">{formatNumber(credential.successCount)}</div>
          </div>
          <div>
            <div className="text-[0.72rem] font-medium text-base-content/50">最近使用</div>
            <div className="font-semibold">{formatLastUsed(credential.lastUsedAt)}</div>
          </div>
          <div>
            <div className="text-[0.72rem] font-medium text-base-content/50">估算费用</div>
            <div className="font-semibold">{formatUsd(credential.estimatedCostUsd)}</div>
          </div>
          {balanceVisible && (
            <div>
              <div className="text-[0.72rem] font-medium text-base-content/50">订阅余额</div>
              {loadingBalance ? (
                <Loading size="sm" className="mt-1" />
              ) : balance ? (
                <div>
                  <div className="font-semibold">{balance.subscriptionTitle || '未知'}</div>
                  <div className="text-xs text-base-content/50">剩余 {formatUsd(balance.remaining)}</div>
                </div>
              ) : null}
            </div>
            )}
        </div>

        <div className="credential-actions">
          <Button type="button" color="ghost" size="xs" onClick={() => onTest(credential)}>
            <Wand2 className="h-3.5 w-3.5" />
            测试
          </Button>
          <Button type="button" color="ghost" size="xs" onClick={() => onViewBalance(credential.id)}>
            <Wallet className="h-3.5 w-3.5" />
            余额
          </Button>
          <Button type="button" color="ghost" size="xs" onClick={handleForceRefresh} disabled={credential.authMethod === 'api_key'}>
            <RefreshCw className="h-3.5 w-3.5" />
            刷新 Token
          </Button>
          <Button
            type="button"
            color="ghost"
            size="xs"
            onClick={() =>
              resetFailure.mutate(credential.id, {
                onSuccess: (res) => toast.success(res.message),
                onError: (error) => toast.error(`操作失败: ${extractErrorMessage(error)}`),
              })
            }
          >
            <RotateCcw className="h-3.5 w-3.5" />
            恢复异常
          </Button>
          <Button
            type="button"
            color="ghost"
            size="xs"
            onClick={() =>
              setWarmup.mutate(
                { id: credential.id, warmupRemaining: credential.warmupRemaining > 0 ? 0 : Math.max(1, warmupTarget) },
                {
                  onSuccess: () => toast.success(credential.warmupRemaining > 0 ? '已关闭预热' : '已开启预热'),
                  onError: (error) => toast.error(`预热设置失败: ${extractErrorMessage(error)}`),
                }
              )
            }
          >
            <ChevronDown className="h-3.5 w-3.5" />
            {credential.warmupRemaining > 0 ? '关闭预热' : '开启预热'}
          </Button>
          <Button
            type="button"
            color="ghost"
            size="xs"
            onClick={() => {
              if (!confirm(`确定清理凭据 #${credential.id} 的当前并发占用吗？`)) return
              clearInFlight.mutate(
                { id: credential.id },
                {
                  onSuccess: (res) => toast.success(res.message),
                  onError: (error) => toast.error(`清理失败: ${extractErrorMessage(error)}`),
                }
              )
            }}
          >
            清理并发
          </Button>
          <Button type="button" color="ghost" size="xs" className="text-error hover:bg-error/10" onClick={handleDelete} disabled={!credential.disabled}>
            <Trash2 className="h-3.5 w-3.5" />
            删除
          </Button>
        </div>
      </Card.Body>
    </Card>
  )
}

export function CredentialsPanel() {
  const [page, setPage] = useState(1)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(new Set())
  const [selectedBalanceId, setSelectedBalanceId] = useState<number | null>(null)
  const [testingCredential, setTestingCredential] = useState<CredentialStatusItem | null>(null)
  const [addOpen, setAddOpen] = useState(false)
  const [batchOpen, setBatchOpen] = useState(false)
  const [kamOpen, setKamOpen] = useState(false)
  const [exportOpen, setExportOpen] = useState(false)
  const [verifyOpen, setVerifyOpen] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [verifyResults, setVerifyResults] = useState<Map<number, VerifyResult>>(new Map())
  const [queryingInfo, setQueryingInfo] = useState(false)
  const [batchRefreshing, setBatchRefreshing] = useState(false)
  const cancelVerifyRef = useRef(false)
  const itemsPerPage = 12
  const queryClient = useQueryClient()
  const credentials = useCredentialsPage({ page, limit: itemsPerPage })
  const allCredentials = useCredentials({ enabled: batchOpen || kamOpen })
  const loadBalancing = useLoadBalancingMode()
  const setLoadBalancing = useSetLoadBalancingMode()
  const deleteCredential = useDeleteCredential()
  const resetFailure = useResetFailure()
  const currentCredentials = useMemo(() => credentials.data?.credentials || [], [credentials.data?.credentials])
  const importDuplicateCheckCredentials = allCredentials.data?.credentials || currentCredentials
  const totalPages = credentials.data?.totalPages || 0
  const selectedDisabledCount = Array.from(selectedIds).filter((id) => currentCredentials.find((item) => item.id === id)?.disabled).length
  const disabledCredentialCount = Math.max((credentials.data?.total || 0) - (credentials.data?.available || 0), 0)

  useEffect(() => {
    setSelectedIds(new Set())
  }, [page])

  useEffect(() => {
    if (credentials.data && page > Math.max(credentials.data.totalPages, 1)) setPage(Math.max(credentials.data.totalPages, 1))
  }, [credentials.data, page])

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
  }

  const toggleSelect = (id: number) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const queryCurrentPageInfo = async () => {
    const ids = currentCredentials.filter((item) => !item.disabled).map((item) => item.id)
    if (!ids.length) return toast.error('当前页没有可查询的启用凭据')
    setQueryingInfo(true)
    let success = 0
    let fail = 0
    for (const id of ids) {
      setLoadingBalanceIds((prev) => new Set(prev).add(id))
      try {
        const balance = await getCredentialBalance(id)
        setBalanceMap((prev) => new Map(prev).set(id, balance))
        success += 1
      } catch {
        fail += 1
      } finally {
        setLoadingBalanceIds((prev) => {
          const next = new Set(prev)
          next.delete(id)
          return next
        })
      }
    }
    setQueryingInfo(false)
    if (fail === 0) toast.success(`查询完成：成功 ${success}/${ids.length}`)
    else toast.warning(`查询完成：成功 ${success} 个，失败 ${fail} 个`)
  }

  const batchDelete = async () => {
    const disabledIds = Array.from(selectedIds).filter((id) => currentCredentials.find((item) => item.id === id)?.disabled)
    if (!disabledIds.length) return toast.error('选中的凭据中没有已禁用项')
    if (!confirm(`确定删除 ${disabledIds.length} 个已禁用凭据吗？此操作无法撤销。`)) return
    let success = 0
    let fail = 0
    for (const id of disabledIds) {
      try {
        await deleteCredential.mutateAsync(id)
        success += 1
      } catch {
        fail += 1
      }
    }
    setSelectedIds(new Set())
    if (fail === 0) toast.success(`成功删除 ${success} 个已禁用凭据`)
    else toast.warning(`删除已禁用凭据：成功 ${success} 个，失败 ${fail} 个`)
  }

  const batchResetFailure = async () => {
    const ids = Array.from(selectedIds).filter((id) => (currentCredentials.find((item) => item.id === id)?.failureCount || 0) > 0)
    if (!ids.length) return toast.error('选中的凭据中没有失败的凭据')
    let success = 0
    let fail = 0
    for (const id of ids) {
      try {
        await resetFailure.mutateAsync(id)
        success += 1
      } catch {
        fail += 1
      }
    }
    setSelectedIds(new Set())
    if (fail === 0) toast.success(`成功恢复 ${success} 个凭据`)
    else toast.warning(`成功 ${success} 个，失败 ${fail} 个`)
  }

  const batchForceRefresh = async () => {
    const ids = Array.from(selectedIds).filter((id) => {
      const cred = currentCredentials.find((item) => item.id === id)
      return cred && !cred.disabled && cred.authMethod !== 'api_key'
    })
    if (!ids.length) return toast.error('选中的凭据中没有可刷新 Token 的 OAuth 凭据')
    setBatchRefreshing(true)
    let success = 0
    let fail = 0
    for (const id of ids) {
      try {
        await forceRefreshToken(id)
        success += 1
      } catch {
        fail += 1
      }
    }
    setBatchRefreshing(false)
    setSelectedIds(new Set())
    invalidate()
    if (fail === 0) toast.success(`成功刷新 ${success} 个凭据的 Token`)
    else toast.warning(`刷新 Token：成功 ${success} 个，失败 ${fail} 个`)
  }

  const clearAllDisabled = async () => {
    let all
    try {
      all = await getCredentials()
    } catch (error) {
      return toast.error(`加载凭据失败: ${extractErrorMessage(error)}`)
    }
    const disabled = all.credentials.filter((item) => item.disabled)
    if (!disabled.length) return toast.error('没有可清除的已禁用凭据')
    if (!confirm(`确定清除所有 ${disabled.length} 个已禁用凭据吗？此操作无法撤销。`)) return
    let success = 0
    let fail = 0
    for (const item of disabled) {
      try {
        await deleteCredential.mutateAsync(item.id)
        success += 1
      } catch {
        fail += 1
      }
    }
    setSelectedIds(new Set())
    if (fail === 0) toast.success(`成功清除所有 ${success} 个已禁用凭据`)
    else toast.warning(`清除已禁用凭据：成功 ${success} 个，失败 ${fail} 个`)
  }

  const batchVerify = async () => {
    const ids = Array.from(selectedIds)
    if (!ids.length) return toast.error('请先选择要验活的凭据')
    setVerifying(true)
    cancelVerifyRef.current = false
    setVerifyOpen(true)
    setVerifyProgress({ current: 0, total: ids.length })
    setVerifyResults(new Map(ids.map((id) => [id, { id, status: 'pending' as const }])))
    let success = 0
    for (let i = 0; i < ids.length; i += 1) {
      if (cancelVerifyRef.current) break
      const id = ids[i]
      setVerifyResults((prev) => new Map(prev).set(id, { id, status: 'verifying' }))
      try {
        const response = await testCredential(id, { model: DEFAULT_TEST_MODEL, prompt: DEFAULT_TEST_PROMPT })
        success += 1
        setVerifyResults((prev) => new Map(prev).set(id, { id, status: 'success', model: testModelLabel(response.model), response: response.response }))
      } catch (error) {
        setVerifyResults((prev) => new Map(prev).set(id, { id, status: 'failed', error: extractErrorMessage(error) }))
      }
      setVerifyProgress({ current: i + 1, total: ids.length })
      if (i < ids.length - 1 && !cancelVerifyRef.current) await new Promise((resolve) => setTimeout(resolve, 2000))
    }
    setVerifying(false)
    if (!cancelVerifyRef.current) toast.success(`验活完成：成功 ${success}/${ids.length}`)
  }

  const toggleLoadBalancing = () => {
    const next = loadBalancing.data?.mode === 'priority' ? 'balanced' : 'priority'
    setLoadBalancing.mutate(next, {
      onSuccess: () => toast.success(`已切换到${next === 'priority' ? '优先级模式' : '均衡负载模式'}`),
      onError: (error) => toast.error(`切换失败: ${extractErrorMessage(error)}`),
    })
  }

  if (credentials.isLoading) return <LoadingState />
  if (credentials.error) return <ErrorState text={extractErrorMessage(credentials.error)} />

  return (
    <div className="space-y-4">
      <div className="metric-grid">
        <StatCard title="凭据总数" value={formatNumber(credentials.data?.total || 0)} />
        <StatCard title="可用凭据" value={formatNumber(credentials.data?.available || 0)} tone="success" />
        <StatCard title="当前活跃" value={`#${credentials.data?.currentId || '-'}`} desc={loadBalancing.data?.mode === 'priority' ? '优先级模式' : '均衡负载模式'} tone="info" />
        <StatCard title="已禁用" value={formatNumber(disabledCredentialCount)} tone={disabledCredentialCount ? 'warning' : 'default'} />
      </div>

      <SectionCard
        title="凭据管理"
        actions={
          <>
            <Button type="button" variant="outline" size="sm" onClick={toggleLoadBalancing} disabled={setLoadBalancing.isPending || loadBalancing.isLoading}>
              {loadBalancing.data?.mode === 'priority' ? '优先级模式' : '均衡负载'}
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={queryCurrentPageInfo} disabled={queryingInfo || currentCredentials.length === 0}>
              {queryingInfo ? <Loading size="xs" /> : <Wallet className="h-4 w-4" />}
              查询信息
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => setKamOpen(true)}>
              <FileUp className="h-4 w-4" />
              KAM 导入
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => setBatchOpen(true)}>
              <Upload className="h-4 w-4" />
              批量导入
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => setExportOpen(true)}>
              <Download className="h-4 w-4" />
              导出
            </Button>
            <Button type="button" color="primary" size="sm" onClick={() => setAddOpen(true)}>
              <Plus className="h-4 w-4" />
              添加凭据
            </Button>
          </>
        }
      >
        {selectedIds.size > 0 && (
          <div className="mb-3 flex flex-wrap items-center gap-2 rounded-box border border-base-300 bg-base-200 px-2.5 py-2">
            <Badge tone="primary">已选择 {selectedIds.size} 个</Badge>
            <Button type="button" variant="outline" size="xs" onClick={batchVerify}>
              <CheckCircle2 className="h-3.5 w-3.5" />
              批量验活
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchForceRefresh} disabled={batchRefreshing}>
              {batchRefreshing ? <Loading size="xs" /> : <RefreshCw className="h-3.5 w-3.5" />}
              批量刷新 Token
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchResetFailure}>
              <RotateCcw className="h-3.5 w-3.5" />
              恢复异常
            </Button>
            <Button type="button" color="error" variant="outline" size="xs" onClick={batchDelete} disabled={selectedDisabledCount === 0}>
              <Trash2 className="h-3.5 w-3.5" />
              批量删除
            </Button>
            <Button type="button" color="ghost" size="xs" onClick={() => setSelectedIds(new Set())}>
              取消选择
            </Button>
          </div>
        )}

        {disabledCredentialCount > 0 && (
          <div className="mb-3 flex justify-end">
            <Button type="button" color="error" variant="outline" size="sm" onClick={clearAllDisabled}>
              <Trash2 className="h-4 w-4" />
              清除已禁用
            </Button>
          </div>
        )}

        {currentCredentials.length === 0 ? (
          <EmptyState text={(credentials.data?.total || 0) === 0 ? '暂无凭据' : '当前页暂无凭据'} />
        ) : (
          <div className="grid gap-3 lg:grid-cols-2 xl:grid-cols-3">
            {currentCredentials.map((credential) => (
              <CredentialCard
                key={credential.id}
                credential={credential}
                selected={selectedIds.has(credential.id)}
                onToggleSelect={() => toggleSelect(credential.id)}
                onViewBalance={setSelectedBalanceId}
                onTest={setTestingCredential}
                balance={balanceMap.get(credential.id)}
                loadingBalance={loadingBalanceIds.has(credential.id)}
              />
            ))}
          </div>
        )}

        {totalPages > 1 && (
          <div className="mt-4 flex items-center justify-center gap-3">
            <Button type="button" variant="outline" size="sm" disabled={page <= 1} onClick={() => setPage((value) => Math.max(1, value - 1))}>
              上一页
            </Button>
            <span className="text-sm text-base-content/60">第 {page} / {totalPages} 页（共 {credentials.data?.total || 0} 个凭据）</span>
            <Button type="button" variant="outline" size="sm" disabled={page >= totalPages} onClick={() => setPage((value) => Math.min(totalPages, value + 1))}>
              下一页
            </Button>
          </div>
        )}
      </SectionCard>

      <AddCredentialModal open={addOpen} onClose={() => setAddOpen(false)} />
      <CredentialTestModal credential={testingCredential} open={Boolean(testingCredential)} onClose={() => setTestingCredential(null)} />
      <BalanceModal credentialId={selectedBalanceId} open={selectedBalanceId !== null} onClose={() => setSelectedBalanceId(null)} />
      <BatchImportModal open={batchOpen} onClose={() => setBatchOpen(false)} existingCredentials={importDuplicateCheckCredentials} onDone={invalidate} />
      <KamImportModal open={kamOpen} onClose={() => setKamOpen(false)} existingCredentials={importDuplicateCheckCredentials} onDone={invalidate} />
      <CredentialExportModal open={exportOpen} onClose={() => setExportOpen(false)} />
      <BatchVerifyModal
        open={verifyOpen}
        verifying={verifying}
        progress={verifyProgress}
        results={verifyResults}
        onCancel={() => {
          cancelVerifyRef.current = true
          setVerifying(false)
        }}
        onClose={() => setVerifyOpen(false)}
      />
    </div>
  )
}
