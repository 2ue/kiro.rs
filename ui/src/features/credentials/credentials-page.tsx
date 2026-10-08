import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  Download,
  FileUp,
  Filter,
  Plus,
  RefreshCw,
  RotateCcw,
  Server,
  Trash2,
  Upload,
  Wallet,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  forceRefreshToken,
  getCredentialAccountInfo,
  getCredentialInfo,
  getCredentialRuntime,
  getCredentialUsageSummary,
  refreshCredentialInfo,
  testCredential,
} from '@/api/credentials'
import {
  Badge,
  Button,
  Checkbox,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
} from '@/components/ui'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  Pagination,
  SectionCard,
  StatCard,
  StatGrid,
  Toolbar,
  useConfirm,
} from '@/components/patterns'
import { formatCredits, formatFullDate, formatNumber, formatUsdFixed2 } from '@/lib/format'
import {
  buildTestModelOptions,
  defaultTestModelForOptions,
  DEFAULT_TEST_PROMPT,
  testModelLabel,
} from '@/lib/test-models'
import { extractErrorMessage } from '@/lib/utils'
import {
  useCredentialList,
  useCredentialRuntime,
  useCredentialSummary,
  useCredentialAccountInfo,
  useCredentialUsageSummary,
  useCredentialCreditSummary,
  useCredentials,
  useDeleteDisabledCredentials,
  useBatchDeleteCredentials,
  useBatchUpdateCredentials,
  useLoadBalancingMode,
  useProxyResources,
  useResetFailure,
  useSetLoadBalancingMode,
} from '@/hooks/use-credentials'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { useModelCapabilities, useUsageSummary } from '@/hooks/use-usage'
import type {
  BalanceResponse,
  CredentialSortBy,
  CredentialSortOrder,
  CredentialStatusItem,
  LoadBalancingMode,
} from '@/types/api'
import { pageMeta } from '@/types/ui'
import { CredentialCard } from './credential-card'
import {
  AddCredentialModal,
  BatchEditCredentialsModal,
  BatchImportModal,
  BatchVerifyModal,
  CredentialExportModal,
  CredentialTestModal,
  KamImportModal,
  type VerifyResult,
} from './credential-dialogs'
import {
  CREDENTIAL_SUBSCRIPTION_OPTIONS,
  credentialCreditStatus,
  credentialSubscriptionLabel,
  mapById,
  mergeCredentialPlanes,
} from './credential-utils'
import {
  buildCredentialRefreshReport,
  refreshCredentialInfoInBatches,
} from './credential-refresh-utils'

// ============================================================================
// Constants
// ============================================================================

const PAGE_SIZE = 15
const CREDIT_INFO_DETAIL_BATCH_SIZE = 500

function CredentialFilterField({
  label,
  children,
  className,
}: {
  label: string
  children: ReactNode
  className?: string
}) {
  return (
    <label className={`block min-w-0 ${className ?? ''}`}>
      <span className="mb-1 block h-4 whitespace-nowrap text-[0.68rem] font-medium leading-4 text-muted-foreground">{label}</span>
      {children}
    </label>
  )
}

function numericQueryValue(value: string): number | undefined {
  const trimmed = value.trim().replace(/^#/, '')
  if (!trimmed) return undefined
  const parsed = Number(trimmed)
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : undefined
}

interface CreditDetailRow {
  id: number
  email?: string | null
  subscriptionTitle?: string | null
  creditRemaining?: number
  creditLimit?: number
  checkedAt?: string
  consumedCredits: number
  estimatedCostUsd: number
  originalCostUsd: number
  disabled: boolean
  creditEstimateBlocked: boolean
  creditEstimateBlockedReason?: string
  estimatedRemainingCostUsd?: number
}

function compareCreditDetailRows(left: CreditDetailRow, right: CreditDetailRow): number {
  if (left.creditEstimateBlocked !== right.creditEstimateBlocked) {
    return Number(left.creditEstimateBlocked) - Number(right.creditEstimateBlocked)
  }
  return left.id - right.id
}

const SORT_OPTIONS: Array<{ value: CredentialSortBy; label: string }> = [
  { value: 'default', label: '默认排序' },
  { value: 'priority', label: '优先级' },
  { value: 'created_at', label: '创建时间' },
  { value: 'updated_at', label: '更新时间' },
  { value: 'last_used_at', label: '最后使用' },
  { value: 'success_count', label: '成功次数' },
  { value: 'failure_count', label: '失败次数' },
  { value: 'refresh_failure_count', label: '刷新失败' },
  { value: 'in_flight_requests', label: '并发占用' },
  { value: 'scheduler_score', label: '调度评分' },
  { value: 'estimated_cost', label: '本地成本' },
  { value: 'usage_percentage', label: '额度使用率' },
  { value: 'remaining_quota', label: '剩余额度' },
  { value: 'id', label: 'ID' },
]

// ============================================================================
// CredentialsPage
// ============================================================================

export function CredentialsPage() {
  const modelCapabilities = useModelCapabilities()
  const [page, setPage] = useState(1)
  const [allExpanded, setAllExpanded] = useState(true)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [quickQuery, setQuickQuery] = useState('')
  const [modelQuery, setModelQuery] = useState('')
  const [regionFilter, setRegionFilter] = useState('__all__')
  const [statusFilter, setStatusFilter] = useState('__all__')
  const [subscriptionFilter, setSubscriptionFilter] = useState('__all__')
  const [proxyFilter, setProxyFilter] = useState('__all__')
  const [sortBy, setSortBy] = useState<CredentialSortBy>('default')
  const [sortOrder, setSortOrder] = useState<CredentialSortOrder>('desc')
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
  const [batchRefreshing, setBatchRefreshing] = useState(false)
  const [queryingCreditInfo, setQueryingCreditInfo] = useState(false)
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(new Set())
  const [creditDetailsOpen, setCreditDetailsOpen] = useState(false)
  const [creditDetailsLoading, setCreditDetailsLoading] = useState(false)
  const [creditDetailRows, setCreditDetailRows] = useState<CreditDetailRow[]>([])
  const cancelVerifyRef = useRef(false)
  const testModelOptions = useMemo(
    () => buildTestModelOptions(modelCapabilities.data?.models),
    [modelCapabilities.data?.models]
  )
  const batchTestModel = defaultTestModelForOptions(testModelOptions)
  const modelFilterOptions = useMemo(
    () => [...(modelCapabilities.data?.models ?? [])]
      .filter((item) => item.model.trim() && item.model.trim().toLowerCase() !== 'auto')
      .sort((left, right) => left.model.localeCompare(right.model)),
    [modelCapabilities.data?.models],
  )

  const confirmDialog = useConfirm()
  const queryClient = useQueryClient()

  // Derived filter params — sentinel '__all__' avoids empty-string in Select
  const debouncedQuickQuery = useDebouncedValue(quickQuery)
  const debouncedModelQuery = useDebouncedValue(modelQuery)
  const quickCredentialId = numericQueryValue(debouncedQuickQuery)
  const quickText = debouncedQuickQuery.trim()
  const listQuery = useMemo(() => ({
    page,
    limit: PAGE_SIZE,
    q: quickCredentialId === undefined ? quickText || undefined : undefined,
    credentialId: quickCredentialId,
    model: debouncedModelQuery.trim() || undefined,
    region: regionFilter !== '__all__' ? regionFilter : undefined,
    status: statusFilter !== '__all__' ? statusFilter : undefined,
    subscription: subscriptionFilter !== '__all__' ? subscriptionFilter : undefined,
    proxyResourceId: proxyFilter !== '__all__' ? Number(proxyFilter) : undefined,
    sortBy: sortBy !== 'default' ? sortBy : undefined,
    sortOrder: sortBy !== 'default' ? sortOrder : undefined,
  }), [
    page,
    quickCredentialId,
    quickText,
    debouncedModelQuery,
    regionFilter,
    statusFilter,
    subscriptionFilter,
    proxyFilter,
    sortBy,
    sortOrder,
  ])

  const credentials = useCredentialList(listQuery)
  const credentialCatalog = useCredentialList({ page: 1, limit: 500 })
  const allCredentials = useCredentials({
    enabled: batchOpen || kamOpen || creditDetailsOpen || queryingCreditInfo,
  })
  const currentIds = useMemo(() => (credentials.data?.items || []).map((i) => i.id), [credentials.data?.items])
  const credentialSummary = useCredentialSummary()
  const credentialRuntime = useCredentialRuntime(currentIds)
  const credentialAccountInfo = useCredentialAccountInfo(currentIds)
  const credentialUsage = useCredentialUsageSummary(currentIds)
  const creditSummary = useCredentialCreditSummary()
  const proxyResources = useProxyResources()
  const loadBalancing = useLoadBalancingMode()
  const usageSummary = useUsageSummary(10_000)
  const setLoadBalancingMutation = useSetLoadBalancingMode()
  const deleteDisabledCredentials = useDeleteDisabledCredentials()
  const batchDeleteCredentials = useBatchDeleteCredentials()
  const batchUpdateCredentials = useBatchUpdateCredentials()
  const resetFailure = useResetFailure()
  const regionOptions = useMemo(() => {
    const regions = new Set<string>()
    const items = [
      ...(credentialCatalog.data?.items ?? []),
      ...(credentials.data?.items ?? []),
    ]
    for (const credential of items) {
      for (const value of [
        credential.region,
        credential.authRegion,
        credential.apiRegion,
        credential.effectiveAuthRegion,
        credential.effectiveApiRegion,
      ]) {
        const trimmed = value?.trim()
        if (trimmed) regions.add(trimmed)
      }
    }
    if (regionFilter !== '__all__') regions.add(regionFilter)
    return Array.from(regions).sort((left, right) => left.localeCompare(right))
  }, [credentialCatalog.data?.items, credentials.data?.items, regionFilter])

  const currentCredentials = useMemo(() => {
    const runtimeById = mapById(credentialRuntime.data?.items)
    const accountById = mapById(credentialAccountInfo.data?.items)
    const usageById = mapById(credentialUsage.data?.items)
    return (credentials.data?.items || []).map((item) =>
      mergeCredentialPlanes(item, runtimeById.get(item.id), accountById.get(item.id), usageById.get(item.id))
    )
  }, [credentials.data?.items, credentialRuntime.data?.items, credentialAccountInfo.data?.items, credentialUsage.data?.items])

  const importDuplicateCheckCredentials = allCredentials.data?.credentials || currentCredentials
  const totalPages = credentials.data?.totalPages || 0
  const filteredTotal = credentials.data?.filteredTotal ?? credentials.data?.total ?? 0
  const grandTotal = credentials.data?.total ?? 0
  const disabledCount = credentialSummary.data?.disabled ?? Math.max((credentials.data?.total || 0) - (credentials.data?.available || 0), 0)
  const pageTransitionPending = Boolean(
    credentials.data?.page !== undefined &&
    (credentials.isPlaceholderData || (credentials.isFetching && credentials.data.page !== page))
  )
  const hasTextFilters = Boolean(
    quickQuery.trim() ||
    modelQuery.trim(),
  )
  const hasActiveFilters = regionFilter !== '__all__' || statusFilter !== '__all__' || subscriptionFilter !== '__all__' || proxyFilter !== '__all__'
  const hasAnyFilters = hasTextFilters || hasActiveFilters
  const selectedCredentials = currentCredentials.filter((c) => selectedIds.has(c.id))
  const selectedDisabledCount = selectedCredentials.filter((c) => c.disabled).length
  const selectedEnabledCount = selectedCredentials.length - selectedDisabledCount
  const selectedPriorityOverrideCount = selectedCredentials.filter((c) => c.priority !== 0).length
  const selectedConcurrencyOverrideCount = selectedCredentials.filter((c) => typeof c.maxConcurrentRequestsOverride === 'number').length
  const selectedRpmOverrideCount = selectedCredentials.filter((c) => typeof c.rpmOverride === 'number').length
  const creditDetailStats = useMemo(() => {
    const available = creditDetailRows.filter((row) => !row.creditEstimateBlocked && row.creditRemaining != null)
    return {
      availableCreditRemaining: available.reduce((sum, row) => sum + (row.creditRemaining ?? 0), 0),
      totalCreditLimit: creditDetailRows.reduce((sum, row) => sum + (row.creditLimit ?? 0), 0),
      totalConsumedCredits: creditDetailRows.reduce((sum, row) => sum + row.consumedCredits, 0),
      totalEstimatedCostUsd: creditDetailRows.reduce((sum, row) => sum + row.estimatedCostUsd, 0),
      totalOriginalCostUsd: creditDetailRows.reduce((sum, row) => sum + row.originalCostUsd, 0),
      totalEstimatedRemainingCostUsd: available.reduce((sum, row) => sum + (row.estimatedRemainingCostUsd ?? 0), 0),
      unavailableCount: creditDetailRows.filter((row) => row.creditEstimateBlocked).length,
      unqueriedCount: creditDetailRows.filter((row) => !row.creditEstimateBlocked && row.creditRemaining == null).length,
    }
  }, [creditDetailRows])
  const orderedCreditDetailRows = useMemo(
    () => [...creditDetailRows].sort(compareCreditDetailRows),
    [creditDetailRows],
  )

  // Reset page on filter change
  useEffect(() => {
    setPage(1)
    setSelectedIds(new Set())
  }, [
    debouncedQuickQuery,
    debouncedModelQuery,
    regionFilter,
    statusFilter,
    subscriptionFilter,
    proxyFilter,
    sortBy,
    sortOrder,
  ])
  useEffect(() => { setSelectedIds(new Set()) }, [page])
  useEffect(() => {
    if (credentials.data && page > Math.max(credentials.data.totalPages, 1)) setPage(Math.max(credentials.data.totalPages, 1))
  }, [credentials.data, page])

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

  const visibleCredentialIdSet = () => new Set(currentIds)

  const applyBalanceItemsToVisibleCards = (
    items: Array<{ id: number; ok?: boolean; info?: BalanceResponse | null }>,
    visibleIds = visibleCredentialIdSet()
  ) => {
    const nextBalances: Array<[number, BalanceResponse]> = []
    for (const item of items) {
      if (item.ok && item.info && visibleIds.has(item.id)) {
        nextBalances.push([item.id, item.info])
      }
    }
    if (!nextBalances.length) return
    setBalanceMap((prev) => {
      const next = new Map(prev)
      nextBalances.forEach(([id, info]) => next.set(id, info))
      return next
    })
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
    await creditSummary.refetch()
    if (result.ok) toast.success(`账号 #${id} 信息已更新`)
    else toast.error(`查询信息失败: ${extractErrorMessage(result.error)}`)
  }

  const loadCreditDetails = async () => {
    setCreditDetailsLoading(true)
    try {
      const snapshot = (await allCredentials.refetch()).data
      const allItems = [...(snapshot?.credentials ?? [])].sort((a, b) => a.id - b.id)
      const ids = allItems.map((item) => item.id)
      const idBatches: number[][] = []
      for (let i = 0; i < ids.length; i += CREDIT_INFO_DETAIL_BATCH_SIZE) {
        idBatches.push(ids.slice(i, i + CREDIT_INFO_DETAIL_BATCH_SIZE))
      }
      const [accountInfoResponses, usageResponses, runtimeResponse] = await Promise.all([
        Promise.all(idBatches.map((batch) => getCredentialAccountInfo(batch))),
        Promise.all(idBatches.map((batch) => getCredentialUsageSummary(batch))),
        getCredentialRuntime(ids),
      ])
      const accountById = new Map(accountInfoResponses.flatMap((response) => response.items).map((item) => [item.id, item]))
      const usageById = new Map(usageResponses.flatMap((response) => response.items).map((item) => [item.id, item]))
      const runtimeById = new Map(runtimeResponse.items.map((item) => [item.id, item]))
      setCreditDetailRows(
        allItems.map((cred) => {
          const info = accountById.get(cred.id)
          const usage = usageById.get(cred.id)
          const runtime = runtimeById.get(cred.id)
          const creditStatus = credentialCreditStatus({
            disabled: cred.disabled,
            disabledReason: cred.disabledReason,
            lastErrorKind: runtime?.lastErrorKind,
            lastErrorReason: runtime?.lastErrorReason,
          })
          const consumedCredits = usage?.kiroMeteringUsage ?? 0
          const estimatedRemainingCostUsd = creditStatus.available && info && consumedCredits > 0
            ? (usage?.estimatedCostUsd ?? 0) / consumedCredits * info.creditRemaining
            : undefined
          return {
            id: cred.id,
            email: cred.email,
            subscriptionTitle: info?.subscriptionTitle ?? cred.subscriptionTitle,
            creditRemaining: info?.creditRemaining,
            creditLimit: info?.creditLimit,
            checkedAt: info?.checkedAt,
            consumedCredits,
            estimatedCostUsd: usage?.estimatedCostUsd ?? 0,
            originalCostUsd: usage?.originalCostUsd ?? 0,
            disabled: cred.disabled,
            creditEstimateBlocked: !creditStatus.available,
            creditEstimateBlockedReason: creditStatus.reason,
            estimatedRemainingCostUsd,
          }
        })
      )
    } catch (e) {
      toast.error(`加载所有账号积分明细失败: ${extractErrorMessage(e)}`)
    } finally {
      setCreditDetailsLoading(false)
    }
  }

  const openCreditDetails = () => {
    setCreditDetailsOpen(true)
    loadCreditDetails()
  }

  const queryAllCreditInfo = async () => {
    setQueryingCreditInfo(true)
    let ids: number[] = []
    try {
      const snapshot = (await allCredentials.refetch()).data
      ids = (snapshot?.credentials || [])
        .map((credential) => credential.id)
      if (!ids.length) {
        toast.error('没有账号可查询积分')
        return
      }
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        ids.forEach((id) => next.add(id))
        return next
      })
      const visibleIds = visibleCredentialIdSet()
      const responses = await refreshCredentialInfoInBatches(
        ids,
        (batchIds) => refreshCredentialInfo(batchIds, true),
        {
          errorMessage: extractErrorMessage,
          onBatchCompleted: (batchIds, response) => {
            applyBalanceItemsToVisibleCards(response.items, visibleIds)
            setLoadingBalanceIds((prev) => {
              const next = new Set(prev)
              batchIds.forEach((id) => next.delete(id))
              return next
            })
          },
        },
      )
      const report = buildCredentialRefreshReport(responses)
      invalidate()
      await creditSummary.refetch()
      if (creditDetailsOpen) await loadCreditDetails()
      if (report.failed === 0) toast.success(`全部账号积分已更新：成功 ${report.success}/${report.total}`)
      else toast.warning(`全部账号积分更新完成：成功 ${report.success}，失败 ${report.failed}`)
    } catch (e) {
      toast.error(`查询全部账号积分失败: ${extractErrorMessage(e)}`)
    } finally {
      setQueryingCreditInfo(false)
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        ids.forEach((id) => next.delete(id))
        return next
      })
    }
  }

  const batchQueryCreditInfo = async () => {
    const ids = Array.from(selectedIds)
    if (!ids.length) return toast.error('请先选择要查询积分的账号')
    setQueryingCreditInfo(true)
    setLoadingBalanceIds((prev) => {
      const next = new Set(prev)
      ids.forEach((id) => next.add(id))
      return next
    })
    try {
      const responses = await refreshCredentialInfoInBatches(
        ids,
        (batchIds) => refreshCredentialInfo(batchIds, true),
        {
          errorMessage: extractErrorMessage,
          onBatchCompleted: (batchIds, response) => {
            applyBalanceItemsToVisibleCards(response.items)
            setLoadingBalanceIds((prev) => {
              const next = new Set(prev)
              batchIds.forEach((id) => next.delete(id))
              return next
            })
          },
        },
      )
      const report = buildCredentialRefreshReport(responses)
      invalidate()
      await creditSummary.refetch()
      if (report.failed === 0) toast.success(`积分查询完成：成功 ${report.success}/${report.total}`)
      else toast.warning(`积分查询完成：成功 ${report.success}，失败 ${report.failed}`)
    } catch (e) {
      toast.error(`查询积分失败: ${extractErrorMessage(e)}`)
    } finally {
      setQueryingCreditInfo(false)
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        ids.forEach((id) => next.delete(id))
        return next
      })
    }
  }

  const toggleSelect = (id: number) =>
    setSelectedIds((prev) => { const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next })
  const selectAll = () => {
    if (selectedIds.size === currentCredentials.length) setSelectedIds(new Set())
    else setSelectedIds(new Set(currentCredentials.map((c) => c.id)))
  }
  const clearFilters = () => {
    setQuickQuery('')
    setModelQuery('')
    setRegionFilter('__all__')
    setStatusFilter('__all__')
    setSubscriptionFilter('__all__')
    setProxyFilter('__all__')
  }

  const setLbMode = (mode: LoadBalancingMode) => {
    const label = mode === 'priority'
      ? '优先级'
      : mode === 'balanced'
        ? '均衡负载'
        : mode === 'health_balanced'
          ? '健康均衡'
          : '低负载优先'
    setLoadBalancingMutation.mutate(mode, {
      onSuccess: () => toast.success(`已切换为${label}模式`),
      onError: (e) => toast.error(`切换失败: ${extractErrorMessage(e)}`),
    })
  }

  const batchDelete = async () => {
    if (batchRefreshing || batchDeleteCredentials.isPending) return
    const disabledIds = Array.from(selectedIds).filter((id) => currentCredentials.find((c) => c.id === id)?.disabled)
    if (!disabledIds.length) return toast.error('选中项中没有已禁用账号')
    const skippedCount = selectedIds.size - disabledIds.length
    const firstConfirmed = await confirmDialog({
      title: '批量删除账号',
      message: `将删除 ${disabledIds.length} 个已禁用账号${skippedCount ? `，跳过 ${skippedCount} 个未禁用账号` : ''}。删除后无法恢复。是否继续？`,
      confirmText: '继续删除',
      tone: 'danger',
    })
    if (!firstConfirmed) return
    const secondConfirmed = await confirmDialog({
      title: '再次确认删除',
      message: `这是最后一次确认：永久删除这 ${disabledIds.length} 个账号及其运行数据？`,
      confirmText: '确认永久删除',
      tone: 'danger',
    })
    if (!secondConfirmed) return
    setBatchRefreshing(true)
    try {
      const result = await batchDeleteCredentials.mutateAsync(disabledIds)
      setSelectedIds(new Set())
      if (result.failed === 0) toast.success(`成功删除 ${result.success} 个账号`)
      else {
        const firstError = result.errors[0]?.message
        toast.warning(`删除：成功 ${result.success}，失败 ${result.failed}${firstError ? `；首个失败：${firstError}` : ''}`)
      }
    } catch (e) {
      toast.error(`批量删除失败: ${extractErrorMessage(e)}`)
    } finally {
      setBatchRefreshing(false)
    }
  }

  const batchDisableAndDelete = async () => {
    if (batchRefreshing || batchDeleteCredentials.isPending) return
    const ids = Array.from(selectedIds)
    if (!ids.length) return toast.error('请先选择账号')
    const firstConfirmed = await confirmDialog({
      title: '禁用并删除账号',
      message: `将先禁用 ${ids.length} 个账号，其中 ${selectedEnabledCount} 个当前可用账号会立即停止接收新请求。正在执行的请求会继续完成；仍有请求执行的账号不会被删除，但会保持禁用状态。是否继续？`,
      confirmText: '继续禁用并删除',
      tone: 'danger',
    })
    if (!firstConfirmed) return
    const secondConfirmed = await confirmDialog({
      title: '再次确认危险操作',
      message: `这是最后一次确认：禁用并尝试永久删除这 ${ids.length} 个账号及其运行数据？`,
      confirmText: '确认禁用并删除',
      tone: 'danger',
    })
    if (!secondConfirmed) return
    setBatchRefreshing(true)
    try {
      const result = await batchDeleteCredentials.mutateAsync({ ids, disableFirst: true })
      setSelectedIds(new Set())
      if (result.failed === 0) toast.success(`成功禁用并删除 ${result.success} 个账号`)
      else {
        const firstError = result.errors[0]?.message
        toast.warning(`禁用并删除：已删除 ${result.success}，未删除 ${result.failed}${firstError ? `；首个失败：${firstError}` : ''}`)
      }
    } catch (e) {
      toast.error(`禁用并删除失败: ${extractErrorMessage(e)}`)
    } finally {
      setBatchRefreshing(false)
    }
  }

  const batchResetFailure = async () => {
    if (batchRefreshing) return
    const ids = Array.from(selectedIds).filter((id) => (currentCredentials.find((c) => c.id === id)?.failureCount || 0) > 0)
    if (!ids.length) return toast.error('选中项中没有有失败记录的账号')
    setBatchRefreshing(true)
    let success = 0; let fail = 0
    for (const id of ids) { try { await resetFailure.mutateAsync(id); success++ } catch { fail++ } }
    setBatchRefreshing(false)
    setSelectedIds(new Set())
    if (fail === 0) toast.success(`成功恢复 ${success} 个账号`)
    else toast.warning(`恢复：成功 ${success}，失败 ${fail}`)
  }

  const batchForceRefresh = async () => {
    const ids = Array.from(selectedIds).filter((id) => {
      const c = currentCredentials.find((cr) => cr.id === id)
      return c && c.authMethod !== 'api_key'
    })
    if (!ids.length) return toast.error('选中项中没有可刷新 Token 的 OAuth 账号')
    setBatchRefreshing(true); let success = 0; let fail = 0
    for (const id of ids) { try { await forceRefreshToken(id); success++ } catch { fail++ } }
    setBatchRefreshing(false); setSelectedIds(new Set()); invalidate()
    if (fail === 0) toast.success(`成功刷新 ${success} 个账号 Token`)
    else toast.warning(`刷新 Token：成功 ${success}，失败 ${fail}`)
  }

  const batchResetPriority = async () => {
    const ids = selectedCredentials.filter((c) => c.priority !== 0).map((c) => c.id)
    if (!ids.length) return toast.error('选中账号没有自定义优先级')
    batchUpdateCredentials.mutate(
      { ids, priority: { priority: 0 } },
      {
        onSuccess: (res) => {
          invalidate()
          if (res.failed === 0) toast.success(`已重置 ${res.success} 个账号优先级`)
          else toast.warning(`重置优先级：成功 ${res.success}，失败 ${res.failed}`)
        },
        onError: (e) => toast.error(`重置优先级失败: ${extractErrorMessage(e)}`),
      }
    )
  }

  const batchClearConcurrency = async () => {
    const ids = selectedCredentials.filter((c) => typeof c.maxConcurrentRequestsOverride === 'number').map((c) => c.id)
    if (!ids.length) return toast.error('选中账号没有自定义并发')
    batchUpdateCredentials.mutate(
      { ids, concurrency: { maxConcurrentRequests: null } },
      {
        onSuccess: (res) => {
          invalidate()
          if (res.failed === 0) toast.success(`已清除 ${res.success} 个账号并发覆盖`)
          else toast.warning(`清除并发覆盖：成功 ${res.success}，失败 ${res.failed}`)
        },
        onError: (e) => toast.error(`清除并发覆盖失败: ${extractErrorMessage(e)}`),
      }
    )
  }

  const batchClearRpm = async () => {
    const ids = selectedCredentials.filter((c) => typeof c.rpmOverride === 'number').map((c) => c.id)
    if (!ids.length) return toast.error('选中账号没有自定义 RPM')
    batchUpdateCredentials.mutate(
      { ids, rpm: { rpm: null } },
      {
        onSuccess: (res) => {
          invalidate()
          if (res.failed === 0) toast.success(`已清除 ${res.success} 个账号 RPM 覆盖`)
          else toast.warning(`清除 RPM 覆盖：成功 ${res.success}，失败 ${res.failed}`)
        },
        onError: (e) => toast.error(`清除 RPM 覆盖失败: ${extractErrorMessage(e)}`),
      }
    )
  }

  const clearAllDisabled = async () => {
    if (!disabledCount) return toast.error('没有可清除的已禁用账号')
    const ok = await confirmDialog({ title: '清除已禁用账号', message: `确定清除所有 ${disabledCount} 个已禁用账号？此操作无法撤销。`, confirmText: '清除全部', tone: 'danger' })
    if (!ok) return
    const secondOk = await confirmDialog({ title: '再次确认清除', message: `这是最后一次确认：永久清除全部 ${disabledCount} 个已禁用账号及其运行数据？`, confirmText: '确认清除全部', tone: 'danger' })
    if (!secondOk) return
    try {
      const result = await deleteDisabledCredentials.mutateAsync()
      setSelectedIds(new Set())
      if (result.failed === 0) toast.success(`成功清除 ${result.success} 个已禁用账号`)
      else toast.warning(`清除：成功 ${result.success}，失败 ${result.failed}`)
    } catch (e) { toast.error(`清除失败: ${extractErrorMessage(e)}`) }
  }

  const batchVerify = async () => {
    const ids = Array.from(selectedIds)
    if (!ids.length) return toast.error('请先选择要验活的账号')
    setVerifying(true); cancelVerifyRef.current = false; setVerifyOpen(true)
    setVerifyProgress({ current: 0, total: ids.length })
    setVerifyResults(new Map(ids.map((id) => [id, { id, status: 'pending' as const }])))
    let success = 0
    for (let i = 0; i < ids.length; i++) {
      if (cancelVerifyRef.current) break
      const id = ids[i]
      setVerifyResults((prev) => new Map(prev).set(id, { id, status: 'verifying' }))
      try {
        const res = await testCredential(id, { model: batchTestModel, prompt: DEFAULT_TEST_PROMPT })
        success++
        setVerifyResults((prev) => new Map(prev).set(id, { id, status: 'success', model: testModelLabel(res.model), response: res.response }))
      } catch (e) {
        setVerifyResults((prev) => new Map(prev).set(id, { id, status: 'failed', error: extractErrorMessage(e) }))
      }
      setVerifyProgress({ current: i + 1, total: ids.length })
      if (i < ids.length - 1 && !cancelVerifyRef.current) await new Promise((r) => setTimeout(r, 2000))
    }
    setVerifying(false)
    if (!cancelVerifyRef.current) toast.success(`验活完成：成功 ${success}/${ids.length}`)
  }

  // Loading / error
  if (credentials.isLoading && !credentials.data) return <LoadingState text="加载账号列表..." />
  if (credentials.error) return <ErrorState message={extractErrorMessage(credentials.error)} />

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.credentials.title}
        subtitle={pageMeta.credentials.subtitle}
        actions={
          <div className="flex flex-wrap items-center gap-1.5">
            <Select
              value={loadBalancing.data?.mode || 'priority'}
              onValueChange={(v) => setLbMode(v as LoadBalancingMode)}
              disabled={setLoadBalancingMutation.isPending || loadBalancing.isLoading}
            >
              <SelectTrigger size="sm" className="w-32"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="priority">优先级</SelectItem>
                <SelectItem value="balanced">均衡负载</SelectItem>
                <SelectItem value="health_balanced">健康均衡</SelectItem>
                <SelectItem value="weighted_least_inflight">低负载优先</SelectItem>
              </SelectContent>
            </Select>
            <Button variant="outline" size="sm" onClick={() => credentials.refetch()}>
              <RefreshCw className={`h-4 w-4 ${credentials.isFetching ? 'animate-spin' : ''}`} />
            </Button>
            <Button variant="outline" size="sm" onClick={queryAllCreditInfo} disabled={queryingCreditInfo} title="查询全部账号订阅和积分信息，刷新汇总">
              {queryingCreditInfo ? <Spinner size="sm" /> : <Wallet className="h-4 w-4" />}
              <span className="hidden sm:inline">查询全部积分</span>
            </Button>
            <Button variant="outline" size="sm" onClick={() => setKamOpen(true)}>
              <FileUp className="h-4 w-4" /><span className="hidden sm:inline">KAM</span>
            </Button>
            <Button variant="outline" size="sm" onClick={() => setBatchOpen(true)}>
              <Upload className="h-4 w-4" /><span className="hidden sm:inline">批量导入</span>
            </Button>
            <Button variant="outline" size="sm" onClick={() => setExportOpen(true)}>
              <Download className="h-4 w-4" /><span className="hidden sm:inline">导出</span>
            </Button>
            <Button size="sm" onClick={() => setAddOpen(true)}>
              <Plus className="h-4 w-4" />添加账号
            </Button>
          </div>
        }
      />

      {/* Stats */}
      <StatGrid>
        {(() => {
          const summary = credentialSummary.data
          const total = summary?.total ?? grandTotal
          const enabled = summary?.available ?? credentials.data?.available ?? 0
          const schedulable = summary?.schedulable ?? enabled
          const coolingDown = summary?.coolingDown ?? 0
          const inUse = summary?.inUse ?? 0
          const failing = summary?.failing ?? 0
          const inFlight = summary?.globalInFlightRequests ?? 0
          const maxInFlight = summary?.globalMaxConcurrentRequests ?? 0
          const queued = summary?.queuedRequests ?? 0
          const realtime = usageSummary.data?.realtime
          const requests = realtime?.requests ?? 0
          const errors = realtime?.errorRequests ?? 0
          const errorRate = requests > 0 ? errors / requests : 0
          return (
            <>
              <StatCard
                title="账号"
                value={`${formatNumber(enabled)} / ${formatNumber(total)}`}
                valueTitle={`启用 ${formatNumber(enabled)} / 共 ${formatNumber(total)}`}
                desc={(
                  <>
                    <span className={schedulable > 0 ? 'text-success' : 'text-muted-foreground'}>
                      可调度 {formatNumber(schedulable)}
                    </span>
                    <span className="text-muted-foreground"> · </span>
                    <span className={failing > 0 ? 'text-warning' : 'text-muted-foreground'}>
                      异常 {formatNumber(failing)}
                    </span>
                    <span className="text-muted-foreground"> · </span>
                    <span className={disabledCount > 0 ? 'text-destructive' : 'text-muted-foreground'}>
                      禁用 {formatNumber(disabledCount)}
                    </span>
                  </>
                )}
                icon={<Server className="h-5 w-5" />}
                tone="default"
              />
              <StatCard
                title="并发"
                value={`${formatNumber(inFlight)}${maxInFlight > 0 ? ` / ${formatNumber(maxInFlight)}` : ''}`}
                valueTitle={maxInFlight > 0 ? `进行中 ${inFlight} / 上限 ${maxInFlight}` : `进行中 ${inFlight}，不限制`}
                desc={`占用账号 ${formatNumber(inUse)} · 排队 ${formatNumber(queued)}${coolingDown > 0 ? ` · 冷却 ${formatNumber(coolingDown)}` : ''}`}
                tone={queued > 0 ? 'warning' : 'info'}
              />
              <StatCard
                title="RPM"
                value={formatNumber(realtime?.rpm ?? 0)}
                valueTitle={`${realtime?.windowSeconds ?? 60} 秒内 ${formatNumber(requests)} 次请求`}
                desc={`${formatNumber(realtime?.totalTpm ?? 0)} TPM · 错误率 ${(errorRate * 100).toFixed(1)}%`}
                tone={errorRate >= 0.1 ? 'warning' : 'info'}
              />
            </>
          )
        })()}
        <button
          type="button"
          className="relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl bg-card p-4 shadow-sm transition-colors hover:shadow-md focus:outline-none focus:ring-2 focus:ring-primary/30 text-left"
          onClick={openCreditDetails}
          title="查看所有账号积分明细"
        >
          <span className="absolute left-0 top-4 h-8 w-1 rounded-r-full bg-success" />
          <div className="flex items-start justify-between gap-2 pl-2.5">
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1.5 text-[0.72rem] font-semibold text-muted-foreground">
                积分
                {creditSummary.isFetching && <Spinner size="sm" />}
              </div>
              <div className="mt-1 break-words text-2xl font-semibold tracking-tight tabular-nums text-success">
                {formatCredits(creditSummary.data?.enabledCreditRemaining)}
              </div>
            </div>
            <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground/60 mt-1" />
          </div>
          <div className="mt-2 truncate pl-2.5 text-[0.72rem] text-muted-foreground">
            历史估算：{formatUsdFixed2(creditSummary.data?.totalEstimatedCostUsd ?? 0)} · 原始 {formatUsdFixed2(creditSummary.data?.totalOriginalCostUsd ?? 0)}
          </div>
          <div className="mt-1 truncate pl-2.5 text-[0.72rem] text-muted-foreground">
            最近查询：{creditSummary.data?.lastCheckedAt ? formatFullDate(creditSummary.data.lastCheckedAt) : '未查询'}
          </div>
        </button>
      </StatGrid>

      {/* Main section */}
      <SectionCard
        title="账号列表"
        description={
          hasAnyFilters
            ? `筛选后 ${filteredTotal} / 共 ${credentialSummary.data?.total ?? grandTotal}`
            : `共 ${credentialSummary.data?.total ?? grandTotal} 个账号`
        }
      >
        {/* Toolbar */}
        <Toolbar className="mb-3">
          <div className="grid w-full min-w-0 items-end gap-2 sm:grid-cols-2 xl:grid-cols-[minmax(220px,1.5fr)_140px_minmax(180px,1fr)_130px_140px] 2xl:grid-cols-[minmax(230px,1.25fr)_130px_minmax(170px,0.95fr)_120px_130px_minmax(130px,0.85fr)_140px_90px_auto]">
            <CredentialFilterField label="快速定位">
              <Input
                value={quickQuery}
                onChange={(e) => setQuickQuery(e.target.value)}
                placeholder="#ID / 邮箱 / Key / 错误"
                className="h-8 text-xs"
              />
            </CredentialFilterField>
            <CredentialFilterField label="状态">
              <Select value={statusFilter} onValueChange={setStatusFilter}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部状态</SelectItem>
                  <SelectItem value="enabled">启用</SelectItem>
                  <SelectItem value="disabled">已禁用</SelectItem>
                  <SelectItem value="current">当前活跃</SelectItem>
                  <SelectItem value="cooldown">冷却中</SelectItem>
                  <SelectItem value="rate_limited">限流中</SelectItem>
                  <SelectItem value="proxy_blocked">代理不可用</SelectItem>
                  <SelectItem value="error">有错误</SelectItem>
                  <SelectItem value="custom_scheduling">有调度覆盖</SelectItem>
                  <SelectItem value="unknown_subscription">订阅未知</SelectItem>
                </SelectContent>
              </Select>
            </CredentialFilterField>
            <CredentialFilterField label="可用模型">
              <Select
                value={modelQuery || '__all__'}
                onValueChange={(value) => setModelQuery(value === '__all__' ? '' : value)}
              >
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部模型</SelectItem>
                  {modelFilterOptions.map((item) => (
                    <SelectItem key={item.model} value={item.model}>
                      {item.displayName && item.displayName !== item.model
                        ? `${item.displayName} · ${item.model}`
                        : item.model}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </CredentialFilterField>
            <CredentialFilterField label="订阅">
              <Select value={subscriptionFilter} onValueChange={setSubscriptionFilter}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部订阅</SelectItem>
                  {CREDENTIAL_SUBSCRIPTION_OPTIONS.map((option) => (
                    <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>
                  ))}
                  <SelectItem value="unknown">未知订阅</SelectItem>
                </SelectContent>
              </Select>
            </CredentialFilterField>
            <CredentialFilterField label="区域">
              <Select value={regionFilter} onValueChange={setRegionFilter}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部区域</SelectItem>
                  {regionOptions.map((region) => (
                    <SelectItem key={region} value={region}>{region}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </CredentialFilterField>
            <CredentialFilterField label="代理">
              <Select value={proxyFilter} onValueChange={setProxyFilter}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部代理</SelectItem>
                  {(proxyResources.data?.resources || []).map((r) => (
                    <SelectItem key={r.id} value={String(r.id)}>{r.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </CredentialFilterField>
            <CredentialFilterField label="排序">
              <Select value={sortBy} onValueChange={(v) => setSortBy(v as CredentialSortBy)}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {SORT_OPTIONS.map((o) => <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>)}
                </SelectContent>
              </Select>
            </CredentialFilterField>
            <CredentialFilterField label="方向">
              <Select value={sortOrder} onValueChange={(v) => setSortOrder(v as CredentialSortOrder)} disabled={sortBy === 'default'}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="desc">降序</SelectItem>
                  <SelectItem value="asc">升序</SelectItem>
                </SelectContent>
              </Select>
            </CredentialFilterField>
            <div className="flex min-w-[4.5rem] items-end">
              {hasAnyFilters && (
                <Button variant="ghost" size="sm" onClick={clearFilters} className="h-8 w-full">
                  <X className="h-3.5 w-3.5" />清除
                </Button>
              )}
            </div>
          </div>
        </Toolbar>

        {/* Batch actions bar */}
        {selectedIds.size > 0 && (
          <div className="mb-3 flex flex-wrap items-center gap-2 rounded-lg bg-primary/5 px-3 py-2 animate-in fade-in slide-in-from-top-2 duration-150">
            <Badge tone="primary">已选 {selectedIds.size}</Badge>
            <Button variant="outline" size="xs" onClick={batchVerify}>
              <CheckCircle2 className="h-3.5 w-3.5" />验活
            </Button>
            <Button variant="outline" size="xs" onClick={() => setBatchEditOpen(true)}>
              <Filter className="h-3.5 w-3.5" />批量修改
            </Button>
            <Button
              variant="outline"
              size="xs"
              onClick={batchResetPriority}
              disabled={batchUpdateCredentials.isPending || selectedPriorityOverrideCount === 0}
            >
              {batchUpdateCredentials.isPending ? <Spinner size="sm" /> : <RotateCcw className="h-3.5 w-3.5" />}
              重置优先级 ({selectedPriorityOverrideCount})
            </Button>
            <Button
              variant="outline"
              size="xs"
              onClick={batchClearConcurrency}
              disabled={batchUpdateCredentials.isPending || selectedConcurrencyOverrideCount === 0}
            >
              {batchUpdateCredentials.isPending ? <Spinner size="sm" /> : <RotateCcw className="h-3.5 w-3.5" />}
              清除并发 ({selectedConcurrencyOverrideCount})
            </Button>
            <Button
              variant="outline"
              size="xs"
              onClick={batchClearRpm}
              disabled={batchUpdateCredentials.isPending || selectedRpmOverrideCount === 0}
            >
              {batchUpdateCredentials.isPending ? <Spinner size="sm" /> : <RotateCcw className="h-3.5 w-3.5" />}
              清除 RPM ({selectedRpmOverrideCount})
            </Button>
            <Button variant="outline" size="xs" onClick={batchForceRefresh} disabled={batchRefreshing}>
              {batchRefreshing ? <Spinner size="sm" /> : <RefreshCw className="h-3.5 w-3.5" />}刷新Token
            </Button>
            <Button variant="outline" size="xs" onClick={batchQueryCreditInfo} disabled={queryingCreditInfo}>
              {queryingCreditInfo ? <Spinner size="sm" /> : <Wallet className="h-3.5 w-3.5" />}查询积分
            </Button>
            <Button variant="outline" size="xs" onClick={batchResetFailure}>
              <RotateCcw className="h-3.5 w-3.5" />恢复异常
            </Button>
            <Button
              variant="outline" size="xs"
              className="text-destructive hover:bg-destructive/10"
              onClick={batchDisableAndDelete}
              disabled={selectedIds.size === 0 || batchRefreshing || batchDeleteCredentials.isPending}
            >
              {batchDeleteCredentials.isPending ? <Spinner size="sm" /> : <Trash2 className="h-3.5 w-3.5" />}
              禁用并删除 ({selectedIds.size})
            </Button>
            <Button
              variant="outline" size="xs"
              className="text-destructive hover:bg-destructive/10"
              onClick={batchDelete}
              disabled={selectedDisabledCount === 0 || batchRefreshing || batchDeleteCredentials.isPending}
            >
              {batchDeleteCredentials.isPending ? <Spinner size="sm" /> : <Trash2 className="h-3.5 w-3.5" />}
              {batchDeleteCredentials.isPending ? '删除中...' : `删除已禁用 (${selectedDisabledCount})`}
            </Button>
            <Button variant="ghost" size="xs" onClick={() => setSelectedIds(new Set())}>取消</Button>
          </div>
        )}

        {/* Select-all row */}
        <div className="mb-3 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <Checkbox
              checked={currentCredentials.length > 0 && selectedIds.size === currentCredentials.length}
              onCheckedChange={selectAll}
            />
            <span className="text-xs text-muted-foreground">
              {selectedIds.size > 0 ? `已选 ${selectedIds.size} 个` : '全选当前页'}
            </span>
            {credentials.isFetching && !credentials.isLoading && <Spinner size="sm" />}
          </div>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="xs" onClick={() => setAllExpanded((v) => !v)}>
              {allExpanded ? <ChevronUp className="h-3.5 w-3.5" /> : <ChevronDown className="h-3.5 w-3.5" />}
              {allExpanded ? '收起全部' : '展开全部'}
            </Button>
            {disabledCount > 0 && (
              <Button
                variant="destructive" size="xs"
                onClick={clearAllDisabled}
              >
                <Trash2 className="h-3.5 w-3.5" />清除全部已禁用 ({disabledCount})
              </Button>
            )}
          </div>
        </div>

        {/* Credential list */}
        {currentCredentials.length === 0 ? (
          <EmptyState
            icon={<Server className="h-12 w-12" />}
            title="暂无账号"
            description={hasAnyFilters ? '没有匹配当前筛选条件的账号' : '点击添加按钮创建第一个账号'}
            action={
              hasAnyFilters ? (
                <Button variant="outline" size="sm" onClick={clearFilters}>清除筛选</Button>
              ) : (
                <Button size="sm" onClick={() => setAddOpen(true)}>
                  <Plus className="h-4 w-4" />添加账号
                </Button>
              )
            }
          />
        ) : (
          <div className="grid gap-3 lg:grid-cols-2">
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
                expanded={allExpanded}
              />
            ))}
          </div>
        )}

        {/* Pagination */}
        {totalPages > 1 && (
          <div className="mt-4">
            <Pagination
              page={page}
              pageCount={totalPages}
              total={filteredTotal}
              pageSize={PAGE_SIZE}
              onPageChange={setPage}
              pending={pageTransitionPending}
            />
          </div>
        )}
      </SectionCard>

      {/* Modals */}
      <ModalShell open={creditDetailsOpen} title="所有账号积分详情" width="max-w-6xl" onClose={() => setCreditDetailsOpen(false)}>
        {creditDetailsLoading ? (
          <LoadingState text="加载积分明细..." />
        ) : creditDetailRows.length > 0 ? (
          <div className="space-y-4">
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2 xl:grid-cols-4">
              <div className="rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
                <div className="text-xs text-muted-foreground">积分</div>
                <div className="mt-1 text-lg font-semibold tabular-nums text-success">{formatCredits(creditDetailStats.availableCreditRemaining)}</div>
                <div className="text-[0.68rem] text-muted-foreground">仅当前可用账号</div>
              </div>
              <div className="rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
                <div className="text-xs text-muted-foreground">已消耗积分</div>
                <div className="mt-1 text-lg font-semibold tabular-nums">{formatCredits(creditDetailStats.totalConsumedCredits)}</div>
                <div className="text-[0.68rem] text-muted-foreground">所有账号历史记录</div>
              </div>
              <div className="rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
                <div className="text-xs text-muted-foreground">估算计费</div>
                <div className="mt-1 text-lg font-semibold tabular-nums">{formatUsdFixed2(creditDetailStats.totalEstimatedCostUsd)}</div>
                <div className="text-[0.68rem] text-muted-foreground">所有账号历史记录</div>
              </div>
              <div className="rounded-lg border border-border/60 bg-muted/20 px-3 py-2">
                <div className="text-xs text-muted-foreground">预估剩余估算费用</div>
                <div className="mt-1 text-lg font-semibold tabular-nums text-success">{formatUsdFixed2(creditDetailStats.totalEstimatedRemainingCostUsd)}</div>
                <div className="text-[0.68rem] text-muted-foreground">
                  {creditDetailStats.unavailableCount > 0
                    ? `已排除 ${formatNumber(creditDetailStats.unavailableCount)} 个不可用账号`
                    : creditDetailStats.unqueriedCount > 0
                      ? `${formatNumber(creditDetailStats.unqueriedCount)} 个账号未查询余额`
                      : '按历史积分单价估算'}
                </div>
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
              <span>总购买额度：{formatCredits(creditDetailStats.totalCreditLimit)}</span>
              <span>原始计费：{formatUsdFixed2(creditDetailStats.totalOriginalCostUsd)}</span>
              <span>估算公式：估算计费 / 已消耗积分 × 剩余积分</span>
            </div>
            <div className="overflow-hidden rounded-lg bg-card shadow-sm">
              <div className="max-h-[60vh] overflow-auto">
                <table className="w-full min-w-[1300px] text-sm">
                  <thead className="sticky top-0 z-10 bg-card">
                    <tr>
                      <th className="w-16 px-3 py-2 text-left font-semibold text-muted-foreground">序号</th>
                      <th className="w-20 px-3 py-2 text-left font-semibold text-muted-foreground">ID</th>
                      <th className="px-3 py-2 text-left font-semibold text-muted-foreground">账号</th>
                      <th className="w-28 px-3 py-2 text-left font-semibold text-muted-foreground">订阅</th>
                      <th className="w-28 px-3 py-2 text-right font-semibold text-muted-foreground">剩余积分</th>
                      <th className="w-28 px-3 py-2 text-right font-semibold text-muted-foreground">总额</th>
                      <th className="w-32 px-3 py-2 text-right font-semibold text-muted-foreground">已消耗积分</th>
                      <th className="w-32 px-3 py-2 text-right font-semibold text-muted-foreground">估算计费</th>
                      <th className="w-32 px-3 py-2 text-right font-semibold text-muted-foreground">原始计费</th>
                      <th className="w-36 px-3 py-2 text-right font-semibold text-muted-foreground">预估剩余费用</th>
                      <th className="w-24 px-3 py-2 text-left font-semibold text-muted-foreground">状态</th>
                      <th className="w-44 px-3 py-2 text-left font-semibold text-muted-foreground">最近查询</th>
                    </tr>
                  </thead>
                  <tbody>
                    {orderedCreditDetailRows.map((row, i) => (
                      <tr key={row.id} className={i % 2 === 0 ? 'bg-muted/20' : ''}>
                        <td className="px-3 py-2 font-mono text-xs text-muted-foreground">{i + 1}</td>
                        <td className="px-3 py-2 font-mono text-xs text-muted-foreground">#{row.id}</td>
                        <td className="max-w-[240px] truncate px-3 py-2 font-medium">{row.email || `账号 #${row.id}`}</td>
                        <td className="px-3 py-2 text-muted-foreground">{credentialSubscriptionLabel(row.subscriptionTitle)}</td>
                        <td className="px-3 py-2 text-right font-semibold tabular-nums text-success">
                          {row.creditEstimateBlocked ? (
                            <span className="text-destructive" title={row.creditEstimateBlockedReason}>不可用</span>
                          ) : row.creditRemaining != null ? formatCredits(row.creditRemaining) : (
                            <span className="text-muted-foreground">未查询</span>
                          )}
                        </td>
                        <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">{formatCredits(row.creditLimit)}</td>
                        <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">{formatCredits(row.consumedCredits)}</td>
                        <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">{formatUsdFixed2(row.estimatedCostUsd)}</td>
                        <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">{formatUsdFixed2(row.originalCostUsd)}</td>
                        <td className="px-3 py-2 text-right tabular-nums text-success">
                          {row.creditEstimateBlocked
                            ? <span className="text-destructive" title={row.creditEstimateBlockedReason}>不可估算</span>
                            : row.estimatedRemainingCostUsd != null
                              ? formatUsdFixed2(row.estimatedRemainingCostUsd)
                              : <span className="text-muted-foreground">无法估算</span>}
                        </td>
                        <td className="px-3 py-2">
                          <span className={row.creditEstimateBlocked ? 'text-destructive' : 'text-success'}>
                            {row.creditEstimateBlocked ? (row.disabled ? '已禁用' : '不可用') : '启用'}
                          </span>
                        </td>
                        <td className="px-3 py-2 text-xs text-muted-foreground">{row.checkedAt ? formatFullDate(row.checkedAt) : '未查询'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </div>
        ) : (
          <EmptyState title="暂无积分明细" />
        )}
      </ModalShell>
      <AddCredentialModal open={addOpen} onClose={() => setAddOpen(false)} />
      <CredentialTestModal credential={testingCredential} open={Boolean(testingCredential)} onClose={() => setTestingCredential(null)} />
      <BatchEditCredentialsModal open={batchEditOpen} ids={Array.from(selectedIds)} onClose={() => setBatchEditOpen(false)} onDone={() => { invalidate(); setSelectedIds(new Set()) }} />
      <BatchImportModal open={batchOpen} onClose={() => setBatchOpen(false)} existingCredentials={importDuplicateCheckCredentials} onDone={invalidate} />
      <KamImportModal open={kamOpen} onClose={() => setKamOpen(false)} existingCredentials={importDuplicateCheckCredentials} onDone={invalidate} />
      <CredentialExportModal open={exportOpen} onClose={() => setExportOpen(false)} selectedIds={Array.from(selectedIds)} />
      <BatchVerifyModal
        open={verifyOpen}
        verifying={verifying}
        progress={verifyProgress}
        results={verifyResults}
        testModel={batchTestModel}
        onCancel={() => { cancelVerifyRef.current = true; setVerifying(false) }}
        onClose={() => setVerifyOpen(false)}
      />
    </PageContainer>
  )
}
