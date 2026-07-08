import {
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Download,
  FileUp,
  Filter,
  Plus,
  RefreshCw,
  RotateCcw,
  Search,
  Server,
  Trash2,
  Upload,
  Wallet,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Button, Checkbox, Input, Loading } from 'react-daisyui'
import { forceRefreshToken, getCredentialAccountInfo, getCredentialInfo, getCredentials, refreshCredentialInfo, testCredential } from '@/api/credentials'
import { Badge, EmptyState, ErrorState, LoadingState, ModalShell, SectionCard, Select, StatCard, useConfirm } from '@/components/ui'
import { CredentialCard } from '@/components/credentials'
import {
  AddCredentialModal,
  BatchEditCredentialsModal,
  BatchImportModal,
  BatchVerifyModal,
  CredentialExportModal,
  CredentialTestModal,
  KamImportModal,
  type VerifyResult,
} from '@/components/CredentialDialogs'
import { formatCredits, formatFullDate, formatNumber } from '@/lib/format'
import {
  buildTestModelOptions,
  defaultTestModelForOptions,
  DEFAULT_TEST_PROMPT,
  testModelLabel,
} from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import { useModelCapabilities } from '@/hooks/use-usage'
import {
  useCredentials,
  useCredentialAccountInfo,
  useCredentialCreditSummary,
  useCredentialList,
  useCredentialRuntime,
  useCredentialSummary,
  useCredentialUsageSummary,
  useBatchUpdateCredentials,
  useDeleteCredential,
  useDeleteDisabledCredentials,
  useLoadBalancingMode,
  useProxyResources,
  useResetFailure,
  useRuntimeConfig,
  useSetLoadBalancingMode,
} from '@/hooks/use-credentials'
import type {
  BalanceResponse,
  CredentialAccountInfo,
  CredentialAccountInfoItem,
  CredentialListItem,
  CredentialRuntimeItem,
  CredentialSortBy,
  CredentialSortOrder,
  CredentialStatusItem,
  CredentialUsageSummaryItem,
  LoadBalancingMode,
} from '@/types/api'

const credentialSortOptions: Array<{ value: CredentialSortBy; label: string }> = [
  { value: 'default', label: '默认排序' },
  { value: 'created_at', label: '创建时间' },
  { value: 'updated_at', label: '更新时间' },
  { value: 'priority', label: '优先级' },
  { value: 'last_used_at', label: '最后使用' },
  { value: 'success_count', label: '成功次数' },
  { value: 'failure_count', label: '失败次数' },
  { value: 'refresh_failure_count', label: '刷新失败' },
  { value: 'estimated_cost', label: '本地成本' },
  { value: 'usage_percentage', label: '额度使用率' },
  { value: 'remaining_quota', label: '剩余额度' },
  { value: 'in_flight_requests', label: '并发占用' },
  { value: 'scheduler_score', label: '调度评分' },
  { value: 'id', label: 'ID' },
]

const runtimeOwnedSorts = new Set<CredentialSortBy>([
  'last_used_at',
  'success_count',
  'failure_count',
  'refresh_failure_count',
  'estimated_cost',
  'usage_percentage',
  'remaining_quota',
  'in_flight_requests',
  'scheduler_score',
])
const credentialInfoRefreshBatchSize = 500

interface CreditDetailRow {
  id: number
  email?: string | null
  subscriptionTitle?: string | null
  creditRemaining?: number
  creditLimit?: number
  checkedAt?: string
}

function mapById<T extends { id: number }>(items: T[] | undefined): Map<number, T> {
  return new Map((items || []).map((item) => [item.id, item]))
}

function accountInfoFromItem(item?: CredentialAccountInfo): CredentialAccountInfo | undefined {
  return item
}

function mergeCredentialPlanes(
  base: CredentialListItem,
  runtime?: CredentialRuntimeItem,
  accountInfo?: CredentialAccountInfo,
  usage?: CredentialUsageSummaryItem
): CredentialStatusItem {
  return {
    ...base,
    failureCount: runtime?.failureCount ?? 0,
    isCurrent: runtime?.isCurrent ?? false,
    expiresAt: runtime?.expiresAt ?? null,
    accountInfo: accountInfoFromItem(accountInfo),
    successCount: runtime?.successCount ?? 0,
    lastUsedAt: runtime?.lastUsedAt ?? null,
    refreshFailureCount: runtime?.refreshFailureCount ?? 0,
    cooledDown: runtime?.cooledDown ?? false,
    cooldownRemainingSecs: runtime?.cooldownRemainingSecs ?? 0,
    cooldownReason: runtime?.cooldownReason,
    cooldowns: runtime?.cooldowns ?? [],
    rateLimited: runtime?.rateLimited ?? false,
    rateLimitRemainingSecs: runtime?.rateLimitRemainingSecs ?? 0,
    inFlightRequests: runtime?.inFlightRequests ?? 0,
    oldestInFlightAgeSecs: runtime?.oldestInFlightAgeSecs ?? 0,
    newestInFlightIdleSecs: runtime?.newestInFlightIdleSecs ?? 0,
    maxConcurrentRequests: runtime?.maxConcurrentRequests ?? base.maxConcurrentRequests,
    inFlightLeaseMaxSecs: runtime?.inFlightLeaseMaxSecs ?? 0,
    transientFailureStreak: runtime?.transientFailureStreak ?? 0,
    recentErrorRate: runtime?.recentErrorRate ?? 0,
    latencyEwmaMs: runtime?.latencyEwmaMs ?? null,
    lastErrorKind: runtime?.lastErrorKind,
    lastErrorReason: runtime?.lastErrorReason,
    lastErrorAtMs: runtime?.lastErrorAtMs ?? null,
    inProbation: runtime?.inProbation ?? false,
    probationRemainingSecs: runtime?.probationRemainingSecs ?? 0,
    schedulerSelectionCount: runtime?.schedulerSelectionCount ?? 0,
    recentSchedulerSelectionCount10s: runtime?.recentSchedulerSelectionCount10s ?? 0,
    recentSchedulerSelectionCount60s: runtime?.recentSchedulerSelectionCount60s ?? 0,
    recentSchedulerSelectionCount5m: runtime?.recentSchedulerSelectionCount5m ?? 0,
    schedulerSelectionPressure: runtime?.schedulerSelectionPressure ?? 0,
    schedulerScore: runtime?.schedulerScore ?? 0,
    estimatedCostUsd: usage?.estimatedCostUsd ?? 0,
    kiroMeteringUsage: usage?.kiroMeteringUsage ?? 0,
    pricedRequests: usage?.pricedRequests ?? 0,
    unpricedRequests: usage?.unpricedRequests ?? 0,
  }
}

export function CredentialsPanel() {
  const modelCapabilities = useModelCapabilities()
  // State
  const [page, setPage] = useState(1)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(new Set())
  const [testingCredential, setTestingCredential] = useState<CredentialStatusItem | null>(null)
  const [addOpen, setAddOpen] = useState(false)
  const [batchOpen, setBatchOpen] = useState(false)
  const [batchEditOpen, setBatchEditOpen] = useState(false)
  const [kamOpen, setKamOpen] = useState(false)
  const [exportOpen, setExportOpen] = useState(false)
  const [verifyOpen, setVerifyOpen] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [verifyResults, setVerifyResults] = useState<Map<number, VerifyResult>>(new Map())
  const [queryingInfo, setQueryingInfo] = useState(false)
  const [queryText, setQueryText] = useState('')
  const [statusFilter, setStatusFilter] = useState('all')
  const [authFilter, setAuthFilter] = useState('all')
  const [subscriptionFilter, setSubscriptionFilter] = useState('all')
  const [proxyFilter, setProxyFilter] = useState('all')
  const [sortBy, setSortBy] = useState<CredentialSortBy>('default')
  const [sortOrder, setSortOrder] = useState<CredentialSortOrder>('desc')
  const [batchRefreshing, setBatchRefreshing] = useState(false)
  const [showFilters, setShowFilters] = useState(false)
  const [creditDetailsOpen, setCreditDetailsOpen] = useState(false)
  const [creditDetailsLoading, setCreditDetailsLoading] = useState(false)
  const [creditDetailRows, setCreditDetailRows] = useState<CreditDetailRow[]>([])
  const cancelVerifyRef = useRef(false)
  const itemsPerPage = 15
  const confirmDialog = useConfirm()
  const testModelOptions = useMemo(
    () => buildTestModelOptions(modelCapabilities.data?.models),
    [modelCapabilities.data?.models]
  )
  const batchTestModel = defaultTestModelForOptions(testModelOptions)

  // Hooks
  const queryClient = useQueryClient()
  const listQuery = {
    page,
    limit: itemsPerPage,
    q: queryText.trim() || undefined,
    status: statusFilter !== 'all' ? statusFilter : undefined,
    authMethod: authFilter !== 'all' ? authFilter : undefined,
    subscription: subscriptionFilter !== 'all' ? subscriptionFilter : undefined,
    proxyResourceId: proxyFilter !== 'all' ? Number(proxyFilter) : undefined,
    sortBy: sortBy !== 'default' && !runtimeOwnedSorts.has(sortBy) ? sortBy : undefined,
    sortOrder: sortBy !== 'default' && !runtimeOwnedSorts.has(sortBy) ? sortOrder : undefined,
  }
  const credentials = useCredentialList(listQuery)
  const allCredentials = useCredentials({ enabled: batchOpen || kamOpen })
  const currentCredentialIds = useMemo(() => (credentials.data?.items || []).map((item) => item.id), [credentials.data?.items])
  const credentialSummary = useCredentialSummary()
  const credentialRuntime = useCredentialRuntime(currentCredentialIds)
  const credentialAccountInfo = useCredentialAccountInfo(currentCredentialIds)
  const credentialUsage = useCredentialUsageSummary(currentCredentialIds)
  const creditSummary = useCredentialCreditSummary()
  const proxyResources = useProxyResources()
  const loadBalancing = useLoadBalancingMode()
  const runtimeConfig = useRuntimeConfig()
  const setLoadBalancing = useSetLoadBalancingMode()
  const deleteCredential = useDeleteCredential()
  const deleteDisabledCredentials = useDeleteDisabledCredentials()
  const batchUpdateCredentials = useBatchUpdateCredentials()
  const resetFailure = useResetFailure()

  // Derived state
  const currentCredentials = useMemo(() => {
    const runtimeById = mapById(credentialRuntime.data?.items)
    const accountById = mapById(credentialAccountInfo.data?.items)
    const usageById = mapById(credentialUsage.data?.items)
    return (credentials.data?.items || []).map((item) =>
      mergeCredentialPlanes(item, runtimeById.get(item.id), accountById.get(item.id), usageById.get(item.id))
    )
  }, [credentialAccountInfo.data?.items, credentialRuntime.data?.items, credentialUsage.data?.items, credentials.data?.items])
  const importDuplicateCheckCredentials = allCredentials.data?.credentials || currentCredentials
  const totalPages = credentials.data?.totalPages || 0
  const credentialsPage = credentials.data?.page
  const pageTransitionPending = credentialsPage !== undefined && (credentials.isPlaceholderData || (credentials.isFetching && credentialsPage !== page))
  const selectedDisabledCount = Array.from(selectedIds).filter((id) => currentCredentials.find((item) => item.id === id)?.disabled).length
  const selectedCredentials = currentCredentials.filter((credential) => selectedIds.has(credential.id))
  const selectedPriorityOverrideCount = selectedCredentials.filter((credential) => credential.priority !== 0).length
  const selectedConcurrencyOverrideCount = selectedCredentials.filter((credential) => typeof credential.maxConcurrentRequestsOverride === 'number').length
  const selectedRpmOverrideCount = selectedCredentials.filter((credential) => typeof credential.rpmOverride === 'number').length
  const disabledCredentialCount = credentialSummary.data?.disabled ?? Math.max((credentials.data?.total || 0) - (credentials.data?.available || 0), 0)
  const hasActiveFilters = statusFilter !== 'all' || authFilter !== 'all' || subscriptionFilter !== 'all' || proxyFilter !== 'all'
  const defaultCredentialConcurrency = runtimeConfig.data?.credentialMaxConcurrentRequests ?? 0
  const concurrencyOverrides = currentCredentials
    .map((credential) => credential.maxConcurrentRequestsOverride)
    .filter((value): value is number => typeof value === 'number')
  const concurrencyOverrideValues = Array.from(new Set(concurrencyOverrides)).sort((a, b) => a - b)
  const concurrencyOverrideDesc = concurrencyOverrides.length
    ? `覆盖 ${concurrencyOverrides.length} 个：${concurrencyOverrideValues.map((value) => (value > 0 ? String(value) : '不限')).join('/')}`
    : '账号未覆盖'

  // Effects
  useEffect(() => {
    setSelectedIds(new Set())
  }, [page])

  useEffect(() => {
    setPage(1)
    setSelectedIds(new Set())
  }, [queryText, statusFilter, authFilter, subscriptionFilter, proxyFilter, sortBy, sortOrder])

  useEffect(() => {
    if (credentials.data && page > Math.max(credentials.data.totalPages, 1)) {
      setPage(Math.max(credentials.data.totalPages, 1))
    }
  }, [credentials.data, page])

  // Handlers
  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    queryClient.invalidateQueries({ queryKey: ['credential-list'] })
    queryClient.invalidateQueries({ queryKey: ['credential-summary'] })
    queryClient.invalidateQueries({ queryKey: ['credential-runtime'] })
    queryClient.invalidateQueries({ queryKey: ['credential-account-info'] })
    queryClient.invalidateQueries({ queryKey: ['credential-usage-summary'] })
    queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
    queryClient.invalidateQueries({ queryKey: ['credential-credit-summary'] })
  }

  const toggleSelect = (id: number) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const selectAll = () => {
    if (selectedIds.size === currentCredentials.length) {
      setSelectedIds(new Set())
    } else {
      setSelectedIds(new Set(currentCredentials.map((c) => c.id)))
    }
  }

  const clearFilters = () => {
    setQueryText('')
    setStatusFilter('all')
    setAuthFilter('all')
    setSubscriptionFilter('all')
    setProxyFilter('all')
  }

  const fetchBalanceForCredential = async (id: number) => {
    setLoadingBalanceIds((prev) => new Set(prev).add(id))
    try {
      const balance = await getCredentialInfo(id, true)
      setBalanceMap((prev) => new Map(prev).set(id, balance))
      return { ok: true as const, balance }
    } catch (error) {
      return { ok: false as const, error }
    } finally {
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        next.delete(id)
        return next
      })
    }
  }

  const queryCredentialBalance = async (id: number) => {
    const result = await fetchBalanceForCredential(id)
    invalidate()
    if (result.ok) toast.success(`账号 #${id} 信息已更新`)
    else toast.error(`查询信息失败: ${extractErrorMessage(result.error)}`)
  }

  const queryCurrentPageInfo = async (enabledOnly = false) => {
    const ids = currentCredentials.filter((item) => !enabledOnly || !item.disabled).map((item) => item.id)
    if (!ids.length) return toast.error(enabledOnly ? '当前页没有启用账号可查询' : '当前页没有可查询信息的账号')
    setQueryingInfo(true)
    setLoadingBalanceIds((prev) => {
      const next = new Set(prev)
      ids.forEach((id) => next.add(id))
      return next
    })
    try {
      const data = await refreshCredentialInfo(ids, true)
      setBalanceMap((prev) => {
        const next = new Map(prev)
        data.items.forEach((item) => {
          if (item.ok && item.info) next.set(item.id, item.info)
        })
        return next
      })
      invalidate()
      if (data.failed === 0) toast.success(`查询完成：成功 ${data.success}/${data.total}`)
      else toast.warning(`查询完成：成功 ${data.success} 个，失败 ${data.failed} 个`)
    } catch (error) {
      toast.error(`查询信息失败: ${extractErrorMessage(error)}`)
    } finally {
      setQueryingInfo(false)
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        ids.forEach((id) => next.delete(id))
        return next
      })
    }
  }

  const loadCreditDetails = async () => {
    setCreditDetailsLoading(true)
    try {
      const all = await getCredentials()
      const enabledCredentials = all.credentials
        .filter((item) => !item.disabled)
        .sort((left, right) => left.id - right.id)
      const ids = enabledCredentials.map((item) => item.id)
      const infoMap = new Map<number, CredentialAccountInfoItem>()
      for (let index = 0; index < ids.length; index += credentialInfoRefreshBatchSize) {
        const batchIds = ids.slice(index, index + credentialInfoRefreshBatchSize)
        const data = await getCredentialAccountInfo(batchIds)
        data.items.forEach((item) => infoMap.set(item.id, item))
      }
      setCreditDetailRows(
        enabledCredentials.map((credential) => {
          const info = infoMap.get(credential.id)
          return {
            id: credential.id,
            email: credential.email,
            subscriptionTitle: info?.subscriptionTitle ?? credential.subscriptionTitle,
            creditRemaining: info?.creditRemaining,
            creditLimit: info?.creditLimit,
            checkedAt: info?.checkedAt,
          }
        })
      )
    } catch (error) {
      toast.error(`加载积分明细失败: ${extractErrorMessage(error)}`)
    } finally {
      setCreditDetailsLoading(false)
    }
  }

  const openCreditDetails = () => {
    setCreditDetailsOpen(true)
    loadCreditDetails()
  }

  const updateAllCreditInfo = async () => {
    setQueryingInfo(true)
    try {
      const all = await getCredentials()
      const ids = all.credentials.map((item) => item.id)
      if (!ids.length) {
        toast.error('没有可查询信息的账号')
        return
      }
      setLoadingBalanceIds(new Set(ids))
      let total = 0
      let success = 0
      let failed = 0
      for (let index = 0; index < ids.length; index += credentialInfoRefreshBatchSize) {
        const batchIds = ids.slice(index, index + credentialInfoRefreshBatchSize)
        const data = await refreshCredentialInfo(batchIds, true)
        total += data.total
        success += data.success
        failed += data.failed
        setBalanceMap((prev) => {
          const next = new Map(prev)
          data.items.forEach((item) => {
            if (item.ok && item.info) next.set(item.id, item.info)
          })
          return next
        })
        setLoadingBalanceIds((prev) => {
          const next = new Set(prev)
          batchIds.forEach((id) => next.delete(id))
          return next
        })
      }
      invalidate()
      await creditSummary.refetch()
      if (creditDetailsOpen) await loadCreditDetails()
      if (failed === 0) toast.success(`积分统计已更新：成功 ${success}/${total}`)
      else toast.warning(`积分统计更新完成：成功 ${success} 个，失败 ${failed} 个`)
    } catch (error) {
      toast.error(`更新积分统计失败: ${extractErrorMessage(error)}`)
    } finally {
      setQueryingInfo(false)
      setLoadingBalanceIds(new Set())
    }
  }

  const batchDelete = async () => {
    const disabledIds = Array.from(selectedIds).filter((id) => currentCredentials.find((item) => item.id === id)?.disabled)
    if (!disabledIds.length) return toast.error('选中的账号中没有已禁用项')
    const confirmed = await confirmDialog({
      title: '删除已禁用账号',
      message: `确定删除 ${disabledIds.length} 个已禁用账号吗？此操作无法撤销。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!confirmed) return
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
    if (fail === 0) toast.success(`成功删除 ${success} 个已禁用账号`)
    else toast.warning(`删除已禁用账号：成功 ${success} 个，失败 ${fail} 个`)
  }

  const batchResetFailure = async () => {
    const ids = Array.from(selectedIds).filter((id) => (currentCredentials.find((item) => item.id === id)?.failureCount || 0) > 0)
    if (!ids.length) return toast.error('选中的账号中没有失败的账号')
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
    if (fail === 0) toast.success(`成功恢复 ${success} 个账号`)
    else toast.warning(`成功 ${success} 个，失败 ${fail} 个`)
  }

  const batchForceRefresh = async () => {
    const ids = Array.from(selectedIds).filter((id) => {
      const cred = currentCredentials.find((item) => item.id === id)
      return cred && cred.authMethod !== 'api_key'
    })
    if (!ids.length) return toast.error('选中的账号中没有可刷新 Token 的 OAuth 账号')
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
    if (fail === 0) toast.success(`成功刷新 ${success} 个账号的 Token`)
    else toast.warning(`刷新 Token：成功 ${success} 个，失败 ${fail} 个`)
  }

  const batchQueryInfo = async () => {
    const ids = Array.from(selectedIds)
    if (!ids.length) return toast.error('请先选择要查询信息的账号')
    setQueryingInfo(true)
    setLoadingBalanceIds((prev) => {
      const next = new Set(prev)
      ids.forEach((id) => next.add(id))
      return next
    })
    try {
      const response = await refreshCredentialInfo(ids, true)
      setBalanceMap((prev) => {
        const next = new Map(prev)
        response.items.forEach((item) => {
          if (item.ok && item.info) next.set(item.id, item.info)
        })
        return next
      })
      invalidate()
      await creditSummary.refetch()
      if (creditDetailsOpen) await loadCreditDetails()
      if (response.failed === 0) toast.success(`查询完成：成功 ${response.success}/${response.total}`)
      else toast.warning(`查询完成：成功 ${response.success} 个，失败 ${response.failed} 个`)
    } catch (error) {
      toast.error(`查询信息失败: ${extractErrorMessage(error)}`)
    } finally {
      setQueryingInfo(false)
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        ids.forEach((id) => next.delete(id))
        return next
      })
    }
  }

  const batchResetPriority = () => {
    const ids = selectedCredentials.filter((credential) => credential.priority !== 0).map((credential) => credential.id)
    if (!ids.length) return toast.error('选中的账号没有自定义优先级')
    batchUpdateCredentials.mutate(
      { ids, priority: { priority: 0 } },
      {
        onSuccess: (response) => {
          invalidate()
          if (response.failed === 0) toast.success(`已重置 ${response.success} 个账号优先级`)
          else toast.warning(`重置优先级：成功 ${response.success} 个，失败 ${response.failed} 个`)
        },
        onError: (error) => toast.error(`重置优先级失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const batchClearConcurrency = () => {
    const ids = selectedCredentials.filter((credential) => typeof credential.maxConcurrentRequestsOverride === 'number').map((credential) => credential.id)
    if (!ids.length) return toast.error('选中的账号没有自定义并发')
    batchUpdateCredentials.mutate(
      { ids, concurrency: { maxConcurrentRequests: null } },
      {
        onSuccess: (response) => {
          invalidate()
          if (response.failed === 0) toast.success(`已清除 ${response.success} 个账号并发覆盖`)
          else toast.warning(`清除并发覆盖：成功 ${response.success} 个，失败 ${response.failed} 个`)
        },
        onError: (error) => toast.error(`清除并发覆盖失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const batchClearRpm = () => {
    const ids = selectedCredentials.filter((credential) => typeof credential.rpmOverride === 'number').map((credential) => credential.id)
    if (!ids.length) return toast.error('选中的账号没有自定义 RPM')
    batchUpdateCredentials.mutate(
      { ids, rpm: { rpm: null } },
      {
        onSuccess: (response) => {
          invalidate()
          if (response.failed === 0) toast.success(`已清除 ${response.success} 个账号 RPM 覆盖`)
          else toast.warning(`清除 RPM 覆盖：成功 ${response.success} 个，失败 ${response.failed} 个`)
        },
        onError: (error) => toast.error(`清除 RPM 覆盖失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const clearAllDisabled = async () => {
    if (!disabledCredentialCount) return toast.error('没有可清除的已禁用账号')
    const confirmed = await confirmDialog({
      title: '清除已禁用账号',
      message: `确定清除所有 ${disabledCredentialCount} 个已禁用账号吗？此操作无法撤销。`,
      confirmText: '清除',
      tone: 'danger',
    })
    if (!confirmed) return
    try {
      const result = await deleteDisabledCredentials.mutateAsync()
      setSelectedIds(new Set())
      if (result.failed === 0) toast.success(`成功清除所有 ${result.success} 个已禁用账号`)
      else toast.warning(`清除已禁用账号：成功 ${result.success} 个，失败 ${result.failed} 个`)
    } catch (error) {
      toast.error(`清除已禁用账号失败: ${extractErrorMessage(error)}`)
    }
  }

  const batchVerify = async () => {
    const ids = Array.from(selectedIds)
    if (!ids.length) return toast.error('请先选择要验活的账号')
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
        const response = await testCredential(id, { model: batchTestModel, prompt: DEFAULT_TEST_PROMPT })
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

  const setLoadBalancingMode = (next: LoadBalancingMode) => {
    setLoadBalancing.mutate(next, {
      onSuccess: () => toast.success(`已切换到${next === 'priority' ? '优先级模式' : next === 'balanced' ? '均衡负载模式' : '健康均衡模式'}`),
      onError: (error) => toast.error(`切换失败: ${extractErrorMessage(error)}`),
    })
  }

  // Loading and error states
  if (credentials.isLoading) return <LoadingState text="加载账号列表..." />
  if (credentials.error) return <ErrorState message={extractErrorMessage(credentials.error)} />

  return (
    <div className="space-y-4">
      <div className="credit-summary-panel p-3">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <Wallet className="h-4 w-4 text-primary" />
              <h2 className="text-sm font-semibold">积分统计</h2>
              {creditSummary.isFetching && <Loading size="xs" />}
            </div>
          </div>
          <Button type="button" color="primary" size="sm" onClick={updateAllCreditInfo} disabled={queryingInfo}>
            {queryingInfo ? <Loading size="xs" /> : <RefreshCw className="h-4 w-4" />}
            更新积分统计
          </Button>
        </div>
        <div className="mt-3 grid gap-2 sm:grid-cols-2">
          <button
            type="button"
            className="rounded-lg border border-transparent bg-base-200/55 px-3 py-2 text-left transition hover:border-primary/40 hover:bg-base-100 focus:outline-none focus:ring-2 focus:ring-primary/30"
            onClick={openCreditDetails}
            title="查看明细"
          >
            <div className="flex items-center justify-between gap-2">
              <div className="text-[0.7rem] font-semibold text-base-content/50">剩余可用积分</div>
              <ChevronRight className="h-3.5 w-3.5 text-base-content/35" />
            </div>
            <div className="mt-0.5 break-all text-lg font-bold text-success">{formatCredits(creditSummary.data?.enabledCreditRemaining)}</div>
          </button>
          <div className="rounded-lg border border-base-300/60 bg-base-200/45 px-3 py-2">
            <div className="text-[0.7rem] font-semibold text-base-content/50">最近查询</div>
            <div className="mt-0.5 text-sm font-semibold">{creditSummary.data?.lastCheckedAt ? formatFullDate(creditSummary.data.lastCheckedAt) : '未查询'}</div>
          </div>
        </div>
      </div>

      {/* Stats Grid */}
      <div className="metric-grid">
        <StatCard
          title="账号总数"
          value={formatNumber(credentialSummary.data?.total ?? credentials.data?.total ?? 0)}
          icon={<Server className="h-5 w-5" />}
        />
        <StatCard
          title="可用账号"
          value={formatNumber(credentialSummary.data?.available ?? credentials.data?.available ?? 0)}
          tone="success"
        />
        <StatCard
          title="当前活跃"
          value={`#${credentialSummary.data?.currentId || '-'}`}
          desc={loadBalancing.data?.mode === 'priority' ? '优先级模式' : loadBalancing.data?.mode === 'balanced' ? '均衡负载' : loadBalancing.data?.mode === 'weighted_least_inflight' ? '低负载优先' : '健康均衡'}
          tone="primary"
        />
        <StatCard
          title="调度容量"
          value={`${credentialSummary.data?.globalInFlightRequests || 0}/${credentialSummary.data?.globalMaxConcurrentRequests || '∞'}`}
          desc={`排队 ${credentialSummary.data?.queuedRequests || 0}`}
          tone="info"
        />
        <StatCard
          title="默认单账号并发"
          value={defaultCredentialConcurrency || '不限'}
          desc={concurrencyOverrideDesc}
          tone="info"
        />
        <StatCard
          title="已禁用"
          value={formatNumber(disabledCredentialCount)}
          tone={disabledCredentialCount ? 'warning' : 'default'}
        />
      </div>

      {/* Main Section */}
      <SectionCard
        title="账号列表"
        description={`共 ${credentials.data?.filteredTotal ?? credentials.data?.total ?? 0} 个账号`}
        actions={
          <div className="flex flex-wrap items-center gap-1.5">
            <Select
              bordered
              size="sm"
              value={loadBalancing.data?.mode || 'priority'}
              disabled={setLoadBalancing.isPending || loadBalancing.isLoading}
              onChange={(e) => setLoadBalancingMode(e.target.value as LoadBalancingMode)}
              className="w-32"
            >
              <Select.Option value="priority">优先级</Select.Option>
              <Select.Option value="balanced">均衡负载</Select.Option>
              <Select.Option value="health_balanced">健康均衡</Select.Option>
              <Select.Option value="weighted_least_inflight">低负载优先</Select.Option>
            </Select>
            <Button type="button" variant="outline" size="sm" onClick={() => credentials.refetch()}>
              <RefreshCw className="h-4 w-4" />
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => setKamOpen(true)}>
              <FileUp className="h-4 w-4" />
              <span className="hidden sm:inline">KAM</span>
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => setBatchOpen(true)}>
              <Upload className="h-4 w-4" />
              <span className="hidden sm:inline">导入</span>
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => setExportOpen(true)}>
              <Download className="h-4 w-4" />
              <span className="hidden sm:inline">导出</span>
            </Button>
            <Button type="button" color="primary" size="sm" onClick={() => setAddOpen(true)}>
              <Plus className="h-4 w-4" />
              添加
            </Button>
          </div>
        }
      >
        {/* Search and Filters */}
        <div className="toolbar-panel mb-4 space-y-3 p-3">
          <div className="flex flex-col gap-2 sm:flex-row">
            <div className="relative flex-1">
              <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-base-content/40" />
              <Input
                bordered
                size="sm"
                className="w-full pl-9"
                value={queryText}
                onChange={(e) => setQueryText(e.target.value)}
                placeholder="搜索邮箱、ID、订阅、代理、错误..."
              />
            </div>
            <Select bordered size="sm" className="sm:w-40" value={sortBy} onChange={(e) => setSortBy(e.target.value as CredentialSortBy)}>
              {credentialSortOptions.map((option) => (
                <Select.Option key={option.value} value={option.value}>{option.label}</Select.Option>
              ))}
            </Select>
            <Select bordered size="sm" className="sm:w-28" value={sortOrder} disabled={sortBy === 'default'} onChange={(e) => setSortOrder(e.target.value as CredentialSortOrder)}>
              <Select.Option value="desc">降序</Select.Option>
              <Select.Option value="asc">升序</Select.Option>
            </Select>
            <div className="flex gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                className={hasActiveFilters ? 'border-primary text-primary' : ''}
                onClick={() => setShowFilters(!showFilters)}
              >
                <Filter className="h-4 w-4" />
                筛选
                {hasActiveFilters && <Badge tone="primary" size="xs">{[statusFilter, authFilter, subscriptionFilter, proxyFilter].filter(f => f !== 'all').length}</Badge>}
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => queryCurrentPageInfo(false)}
                disabled={queryingInfo || currentCredentials.length === 0}
              >
                {queryingInfo ? <Loading size="xs" /> : <Wallet className="h-4 w-4" />}
                <span className="hidden sm:inline">查询信息</span>
              </Button>
            </div>
          </div>

          {/* Filter Panel */}
          {showFilters && (
            <div className="animate-slide-down rounded-lg border border-base-300/60 bg-base-100/65 p-3">
              <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
                <Select bordered size="sm" value={statusFilter} onChange={(e) => setStatusFilter(e.target.value)}>
                  <Select.Option value="all">全部状态</Select.Option>
                  <Select.Option value="enabled">启用</Select.Option>
                  <Select.Option value="disabled">已禁用</Select.Option>
                  <Select.Option value="current">当前活跃</Select.Option>
                  <Select.Option value="cooldown">冷却中</Select.Option>
                  <Select.Option value="rate_limited">限流中</Select.Option>
                  <Select.Option value="proxy_blocked">代理不可用</Select.Option>
                  <Select.Option value="custom_scheduling">有调度覆盖</Select.Option>
                  <Select.Option value="custom_priority">自定义优先级</Select.Option>
                  <Select.Option value="custom_concurrency">自定义并发</Select.Option>
                  <Select.Option value="custom_rpm">自定义 RPM</Select.Option>
                  <Select.Option value="error">有错误</Select.Option>
                  <Select.Option value="unknown_subscription">未知订阅</Select.Option>
                </Select>
                <Select bordered size="sm" value={authFilter} onChange={(e) => setAuthFilter(e.target.value)}>
                  <Select.Option value="all">全部认证</Select.Option>
                  <Select.Option value="social">Social</Select.Option>
                  <Select.Option value="idc">IdC</Select.Option>
                  <Select.Option value="external_idp">External IdP</Select.Option>
                  <Select.Option value="api_key">API Key</Select.Option>
                </Select>
                <Select bordered size="sm" value={subscriptionFilter} onChange={(e) => setSubscriptionFilter(e.target.value)}>
                  <Select.Option value="all">全部订阅</Select.Option>
                  <Select.Option value="pro_plus">Pro+</Select.Option>
                  <Select.Option value="pro">Pro</Select.Option>
                  <Select.Option value="trial">试用</Select.Option>
                  <Select.Option value="free">Free</Select.Option>
                  <Select.Option value="unknown">未知</Select.Option>
                </Select>
                <Select bordered size="sm" value={proxyFilter} onChange={(e) => setProxyFilter(e.target.value)}>
                  <Select.Option value="all">全部代理</Select.Option>
                  {(proxyResources.data?.resources || []).map((r) => (
                    <Select.Option key={r.id} value={String(r.id)}>{r.name}</Select.Option>
                  ))}
                </Select>
              </div>
              {hasActiveFilters && (
                <div className="mt-2 flex justify-end">
                  <Button type="button" color="ghost" size="xs" onClick={clearFilters}>
                    <X className="h-3.5 w-3.5" /> 清除筛选
                  </Button>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Batch Actions */}
        {selectedIds.size > 0 && (
          <div className="mb-4 flex flex-wrap items-center gap-2 rounded-lg border border-primary/30 bg-primary/5 p-2">
            <Badge tone="primary">已选 {selectedIds.size}</Badge>
            <Button type="button" variant="outline" size="xs" onClick={batchVerify}>
              <CheckCircle2 className="h-3.5 w-3.5" /> 验活
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={() => setBatchEditOpen(true)}>
              <Filter className="h-3.5 w-3.5" /> 批量修改
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchResetPriority} disabled={batchUpdateCredentials.isPending || selectedPriorityOverrideCount === 0}>
              <RotateCcw className="h-3.5 w-3.5" /> 重置优先级 ({selectedPriorityOverrideCount})
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchClearConcurrency} disabled={batchUpdateCredentials.isPending || selectedConcurrencyOverrideCount === 0}>
              <RotateCcw className="h-3.5 w-3.5" /> 清除并发 ({selectedConcurrencyOverrideCount})
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchClearRpm} disabled={batchUpdateCredentials.isPending || selectedRpmOverrideCount === 0}>
              <RotateCcw className="h-3.5 w-3.5" /> 清除 RPM ({selectedRpmOverrideCount})
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchForceRefresh} disabled={batchRefreshing}>
              {batchRefreshing ? <Loading size="xs" /> : <RefreshCw className="h-3.5 w-3.5" />}
              刷新Token
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchQueryInfo} disabled={queryingInfo}>
              {queryingInfo ? <Loading size="xs" /> : <Wallet className="h-3.5 w-3.5" />}
              查询信息
            </Button>
            <Button type="button" variant="outline" size="xs" onClick={batchResetFailure}>
              <RotateCcw className="h-3.5 w-3.5" /> 恢复异常
            </Button>
            <Button type="button" color="error" variant="outline" size="xs" onClick={batchDelete} disabled={selectedDisabledCount === 0}>
              <Trash2 className="h-3.5 w-3.5" /> 删除
            </Button>
            <Button type="button" color="ghost" size="xs" onClick={() => setSelectedIds(new Set())}>
              取消
            </Button>
          </div>
        )}

        {/* Quick Actions */}
        <div className="mb-3 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Checkbox
              size="xs"
              checked={selectedIds.size === currentCredentials.length && currentCredentials.length > 0}
              indeterminate={selectedIds.size > 0 && selectedIds.size < currentCredentials.length}
              onChange={selectAll}
            />
            <span className="text-xs text-base-content/50">
              {selectedIds.size > 0 ? `已选 ${selectedIds.size}` : '全选'}
            </span>
          </div>
          {disabledCredentialCount > 0 && (
            <Button type="button" color="error" variant="outline" size="xs" onClick={clearAllDisabled}>
              <Trash2 className="h-3.5 w-3.5" /> 清除已禁用 ({disabledCredentialCount})
            </Button>
          )}
        </div>

        {/* Credential List */}
        {currentCredentials.length === 0 ? (
          <EmptyState
            icon={<Server className="h-12 w-12" />}
            title="暂无账号"
            description={hasActiveFilters ? '没有匹配当前筛选条件的账号' : '点击添加按钮创建第一个账号'}
            action={
              hasActiveFilters ? (
                <Button type="button" variant="outline" size="sm" onClick={clearFilters}>清除筛选</Button>
              ) : (
                <Button type="button" color="primary" size="sm" onClick={() => setAddOpen(true)}>
                  <Plus className="h-4 w-4" /> 添加账号
                </Button>
              )
            }
          />
        ) : (
          <div className="credential-grid">
            {currentCredentials.map((credential) => (
              <CredentialCard
                key={credential.id}
                credential={credential}
                selected={selectedIds.has(credential.id)}
                onToggleSelect={() => toggleSelect(credential.id)}
                onQueryBalance={queryCredentialBalance}
                onTest={setTestingCredential}
                balance={balanceMap.get(credential.id)}
                loadingBalance={loadingBalanceIds.has(credential.id)}
              />
            ))}
          </div>
        )}

        {/* Pagination */}
        {totalPages > 1 && (
          <div className="mt-4 flex items-center justify-center gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={page <= 1 || pageTransitionPending}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <span className="min-w-[120px] text-center text-sm text-base-content/60">
              {page} / {totalPages}
            </span>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={page >= totalPages || pageTransitionPending}
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
            >
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        )}
      </SectionCard>

      {/* Modals */}
      <ModalShell open={creditDetailsOpen} title="剩余可用积分明细" width="max-w-4xl" onClose={() => setCreditDetailsOpen(false)}>
        {creditDetailsLoading ? (
          <LoadingState text="加载积分明细..." />
        ) : creditDetailRows.length ? (
          <div className="overflow-hidden rounded-lg border border-base-300">
            <div className="max-h-[60vh] overflow-auto">
              <table className="table table-zebra table-sm">
                <thead className="sticky top-0 z-10 bg-base-100">
                  <tr>
                    <th className="w-20">ID</th>
                    <th>账号</th>
                    <th className="w-28">订阅</th>
                    <th className="w-28 text-right">剩余</th>
                    <th className="w-28 text-right">总额</th>
                    <th className="w-40">最近查询</th>
                  </tr>
                </thead>
                <tbody>
                  {creditDetailRows.map((row) => (
                    <tr key={row.id}>
                      <td className="font-mono text-xs">#{row.id}</td>
                      <td className="max-w-[260px] truncate font-medium">{row.email || `账号 #${row.id}`}</td>
                      <td>{row.subscriptionTitle || '未知'}</td>
                      <td className="text-right font-semibold text-success">{formatCredits(row.creditRemaining)}</td>
                      <td className="text-right">{formatCredits(row.creditLimit)}</td>
                      <td className="text-xs text-base-content/60">{row.checkedAt ? formatFullDate(row.checkedAt) : '未查询'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        ) : (
          <EmptyState title="暂无积分明细" />
        )}
      </ModalShell>
      <AddCredentialModal open={addOpen} onClose={() => setAddOpen(false)} />
      <CredentialTestModal credential={testingCredential} open={Boolean(testingCredential)} onClose={() => setTestingCredential(null)} />
      <BatchEditCredentialsModal
        open={batchEditOpen}
        ids={Array.from(selectedIds)}
        onClose={() => setBatchEditOpen(false)}
        onDone={() => {
          invalidate()
          setSelectedIds(new Set())
        }}
      />
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
