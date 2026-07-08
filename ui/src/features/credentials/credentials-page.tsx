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
import { useEffect, useMemo, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { forceRefreshToken, getCredentials, getCredentialAccountInfo, getCredentialInfo, refreshCredentialInfo, testCredential } from '@/api/credentials'
import {
  Badge,
  Button,
  Checkbox,
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
  ToolbarActions,
  ToolbarSearch,
  useConfirm,
} from '@/components/patterns'
import { formatCompact, formatCredits, formatFullDate, formatNumber } from '@/lib/format'
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
  useDeleteCredential,
  useDeleteDisabledCredentials,
  useBatchUpdateCredentials,
  useLoadBalancingMode,
  useProxyResources,
  useResetFailure,
  useRuntimeConfig,
  useSetLoadBalancingMode,
} from '@/hooks/use-credentials'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { useModelCapabilities } from '@/hooks/use-usage'
import type {
  BalanceResponse,
  CredentialAccountInfoItem,
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
import { mapById, mergeCredentialPlanes } from './credential-utils'

// ============================================================================
// Constants
// ============================================================================

const PAGE_SIZE = 15
const CREDIT_INFO_BATCH_SIZE = 500

interface CreditDetailRow {
  id: number
  email?: string | null
  subscriptionTitle?: string | null
  creditRemaining?: number
  creditLimit?: number
  checkedAt?: string
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
  const [queryText, setQueryText] = useState('')
  const [statusFilter, setStatusFilter] = useState('__all__')
  const [authFilter, setAuthFilter] = useState('__all__')
  const [subscriptionFilter, setSubscriptionFilter] = useState('__all__')
  const [proxyFilter, setProxyFilter] = useState('__all__')
  const [sortBy, setSortBy] = useState<CredentialSortBy>('default')
  const [sortOrder, setSortOrder] = useState<CredentialSortOrder>('desc')
  const [showFilters, setShowFilters] = useState(false)
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

  const confirmDialog = useConfirm()
  const queryClient = useQueryClient()

  // Derived filter params — sentinel '__all__' avoids empty-string in Select
  const debouncedQueryText = useDebouncedValue(queryText)
  const listQuery = useMemo(() => ({
    page,
    limit: PAGE_SIZE,
    q: debouncedQueryText.trim() || undefined,
    status: statusFilter !== '__all__' ? statusFilter : undefined,
    authMethod: authFilter !== '__all__' ? authFilter : undefined,
    subscription: subscriptionFilter !== '__all__' ? subscriptionFilter : undefined,
    proxyResourceId: proxyFilter !== '__all__' ? Number(proxyFilter) : undefined,
    sortBy: sortBy !== 'default' ? sortBy : undefined,
    sortOrder: sortBy !== 'default' ? sortOrder : undefined,
  }), [page, debouncedQueryText, statusFilter, authFilter, subscriptionFilter, proxyFilter, sortBy, sortOrder])

  const credentials = useCredentialList(listQuery)
  const allCredentials = useCredentials({ enabled: batchOpen || kamOpen })
  const currentIds = useMemo(() => (credentials.data?.items || []).map((i) => i.id), [credentials.data?.items])
  const credentialSummary = useCredentialSummary()
  const credentialRuntime = useCredentialRuntime(currentIds)
  const credentialAccountInfo = useCredentialAccountInfo(currentIds)
  const credentialUsage = useCredentialUsageSummary(currentIds)
  const creditSummary = useCredentialCreditSummary()
  const proxyResources = useProxyResources()
  const loadBalancing = useLoadBalancingMode()
  const runtimeConfig = useRuntimeConfig()
  const setLoadBalancingMutation = useSetLoadBalancingMode()
  const deleteCredential = useDeleteCredential()
  const deleteDisabledCredentials = useDeleteDisabledCredentials()
  const batchUpdateCredentials = useBatchUpdateCredentials()
  const resetFailure = useResetFailure()

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
  const hasActiveFilters = statusFilter !== '__all__' || authFilter !== '__all__' || subscriptionFilter !== '__all__' || proxyFilter !== '__all__'
  const activeFilterCount = [statusFilter, authFilter, subscriptionFilter, proxyFilter].filter((f) => f !== '__all__').length
  const selectedCredentials = currentCredentials.filter((c) => selectedIds.has(c.id))
  const selectedDisabledCount = selectedCredentials.filter((c) => c.disabled).length
  const selectedPriorityOverrideCount = selectedCredentials.filter((c) => c.priority !== 0).length
  const selectedConcurrencyOverrideCount = selectedCredentials.filter((c) => typeof c.maxConcurrentRequestsOverride === 'number').length
  const selectedRpmOverrideCount = selectedCredentials.filter((c) => typeof c.rpmOverride === 'number').length
  const defaultCredentialConcurrency = runtimeConfig.data?.credentialMaxConcurrentRequests ?? 0
  const concurrencyOverrides = currentCredentials
    .map((c) => c.maxConcurrentRequestsOverride)
    .filter((v): v is number => typeof v === 'number')
  const concurrencyOverrideDesc = concurrencyOverrides.length
    ? `${concurrencyOverrides.length} 个账号已覆盖`
    : '账号未覆盖'

  // Reset page on filter change
  useEffect(() => { setPage(1); setSelectedIds(new Set()) }, [debouncedQueryText, statusFilter, authFilter, subscriptionFilter, proxyFilter, sortBy, sortOrder])
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
      const all = await getCredentials()
      const enabledCredentials = all.credentials
        .filter((item) => !item.disabled)
        .sort((a, b) => a.id - b.id)
      const ids = enabledCredentials.map((item) => item.id)
      const infoMap = new Map<number, CredentialAccountInfoItem>()
      for (let i = 0; i < ids.length; i += CREDIT_INFO_BATCH_SIZE) {
        const batchIds = ids.slice(i, i + CREDIT_INFO_BATCH_SIZE)
        const data = await getCredentialAccountInfo(batchIds)
        data.items.forEach((item) => infoMap.set(item.id, item))
      }
      setCreditDetailRows(
        enabledCredentials.map((cred) => {
          const info = infoMap.get(cred.id)
          return {
            id: cred.id,
            email: cred.email,
            subscriptionTitle: info?.subscriptionTitle ?? cred.subscriptionTitle,
            creditRemaining: info?.creditRemaining,
            creditLimit: info?.creditLimit,
            checkedAt: info?.checkedAt,
          }
        })
      )
    } catch (e) {
      toast.error(`加载积分明细失败: ${extractErrorMessage(e)}`)
    } finally {
      setCreditDetailsLoading(false)
    }
  }

  const openCreditDetails = () => {
    setCreditDetailsOpen(true)
    loadCreditDetails()
  }

  const updateAllCreditInfo = async () => {
    setQueryingCreditInfo(true)
    setLoadingBalanceIds((prev) => {
      const next = new Set(prev)
      currentIds.forEach((id) => next.add(id))
      return next
    })
    try {
      const all = await getCredentials()
      const ids = all.credentials.map((item) => item.id)
      if (!ids.length) { toast.error('没有可查询信息的账号'); return }
      let total = 0; let success = 0; let failed = 0
      const visibleIds = visibleCredentialIdSet()
      for (let i = 0; i < ids.length; i += CREDIT_INFO_BATCH_SIZE) {
        const batchIds = ids.slice(i, i + CREDIT_INFO_BATCH_SIZE)
        const data = await refreshCredentialInfo(batchIds, true)
        total += data.total; success += data.success; failed += data.failed
        applyBalanceItemsToVisibleCards(data.items, visibleIds)
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
      else toast.warning(`积分统计更新完成：成功 ${success}，失败 ${failed}`)
    } catch (e) {
      toast.error(`更新积分统计失败: ${extractErrorMessage(e)}`)
    } finally {
      setQueryingCreditInfo(false)
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        currentIds.forEach((id) => next.delete(id))
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
      let success = 0; let failed = 0; let total = 0
      for (let i = 0; i < ids.length; i += CREDIT_INFO_BATCH_SIZE) {
        const batchIds = ids.slice(i, i + CREDIT_INFO_BATCH_SIZE)
        const data = await refreshCredentialInfo(batchIds, true)
        total += data.total; success += data.success; failed += data.failed
        applyBalanceItemsToVisibleCards(data.items)
        setLoadingBalanceIds((prev) => {
          const next = new Set(prev)
          batchIds.forEach((id) => next.delete(id))
          return next
        })
      }
      invalidate()
      await creditSummary.refetch()
      if (failed === 0) toast.success(`积分查询完成：成功 ${success}/${total}`)
      else toast.warning(`积分查询完成：成功 ${success}，失败 ${failed}`)
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
  const clearFilters = () => { setQueryText(''); setStatusFilter('__all__'); setAuthFilter('__all__'); setSubscriptionFilter('__all__'); setProxyFilter('__all__') }

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
    if (batchRefreshing) return
    const disabledIds = Array.from(selectedIds).filter((id) => currentCredentials.find((c) => c.id === id)?.disabled)
    if (!disabledIds.length) return toast.error('选中项中没有已禁用账号')
    const ok = await confirmDialog({ title: '批量删除', message: `确定删除 ${disabledIds.length} 个已禁用账号？此操作无法撤销。`, confirmText: '删除', tone: 'danger' })
    if (!ok) return
    setBatchRefreshing(true)
    let success = 0; let fail = 0
    for (const id of disabledIds) { try { await deleteCredential.mutateAsync(id); success++ } catch { fail++ } }
    setBatchRefreshing(false)
    setSelectedIds(new Set())
    if (fail === 0) toast.success(`成功删除 ${success} 个账号`)
    else toast.warning(`删除：成功 ${success}，失败 ${fail}`)
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
            <Button variant="outline" size="sm" onClick={updateAllCreditInfo} disabled={queryingCreditInfo} title="更新积分统计">
              {queryingCreditInfo ? <Spinner size="sm" /> : <Wallet className="h-4 w-4" />}
              <span className="hidden sm:inline">更新积分</span>
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
        <StatCard
          title="账号总数"
          value={formatCompact(credentialSummary.data?.total ?? grandTotal)}
          valueTitle={formatNumber(credentialSummary.data?.total ?? grandTotal)}
          icon={<Server className="h-5 w-5" />}
        />
        <StatCard
          title="可用账号"
          value={formatCompact(credentialSummary.data?.available ?? credentials.data?.available ?? 0)}
          valueTitle={formatNumber(credentialSummary.data?.available ?? credentials.data?.available ?? 0)}
          tone="success"
        />
        <StatCard
          title="当前活跃"
          value={`#${credentialSummary.data?.currentId || '-'}`}
          desc={
            loadBalancing.data?.mode === 'priority'
              ? '优先级模式'
              : loadBalancing.data?.mode === 'balanced'
                ? '均衡负载'
                : loadBalancing.data?.mode === 'health_balanced'
                  ? '健康均衡'
                  : '低负载优先'
          }
          tone="primary"
        />
        <StatCard
          title="全局并发"
          value={`${credentialSummary.data?.globalInFlightRequests ?? 0} / ${credentialSummary.data?.globalMaxConcurrentRequests ?? '∞'}`}
          desc={`排队 ${credentialSummary.data?.queuedRequests ?? 0}`}
          tone="info"
        />
        <StatCard
          title="默认单账号并发"
          value={defaultCredentialConcurrency > 0 ? String(defaultCredentialConcurrency) : '不限制'}
          desc={concurrencyOverrideDesc}
          tone="info"
        />
        <StatCard
          title="已禁用"
          value={formatCompact(disabledCount)}
          valueTitle={formatNumber(disabledCount)}
          tone={disabledCount > 0 ? 'warning' : 'default'}
        />
        <button
          type="button"
          className="relative flex min-h-[6.5rem] flex-col justify-between overflow-hidden rounded-xl bg-card p-4 shadow-sm transition-colors hover:shadow-md focus:outline-none focus:ring-2 focus:ring-primary/30 text-left"
          onClick={openCreditDetails}
          title="查看积分明细"
        >
          <span className="absolute left-0 top-4 h-8 w-1 rounded-r-full bg-success" />
          <div className="flex items-start justify-between gap-2 pl-2.5">
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1.5 text-[0.72rem] font-semibold text-muted-foreground">
                剩余可用积分
                {creditSummary.isFetching && <Spinner size="sm" />}
              </div>
              <div className="mt-1 break-words text-2xl font-semibold tracking-tight tabular-nums text-success">
                {formatCredits(creditSummary.data?.enabledCreditRemaining)}
              </div>
            </div>
            <ChevronRight className="h-4 w-4 shrink-0 text-muted-foreground/60 mt-1" />
          </div>
          <div className="mt-2 truncate pl-2.5 text-[0.72rem] text-muted-foreground">
            最近查询：{creditSummary.data?.lastCheckedAt ? formatFullDate(creditSummary.data.lastCheckedAt) : '未查询'}
          </div>
        </button>
      </StatGrid>

      {/* Main section */}
      <SectionCard
        title="账号列表"
        description={
          hasActiveFilters || queryText
            ? `筛选后 ${filteredTotal} / 共 ${credentialSummary.data?.total ?? grandTotal}`
            : `共 ${credentialSummary.data?.total ?? grandTotal} 个账号`
        }
      >
        {/* Toolbar */}
        <Toolbar className="mb-3">
          <ToolbarSearch
            value={queryText}
            onChange={setQueryText}
            placeholder="搜索邮箱、#ID、订阅、代理、Region、priority:0、rpm:60..."
          />
          <ToolbarActions>
            <Select value={sortBy} onValueChange={(v) => setSortBy(v as CredentialSortBy)}>
              <SelectTrigger size="sm" className="w-36"><SelectValue /></SelectTrigger>
              <SelectContent>
                {SORT_OPTIONS.map((o) => <SelectItem key={o.value} value={o.value}>{o.label}</SelectItem>)}
              </SelectContent>
            </Select>
            <Select value={sortOrder} onValueChange={(v) => setSortOrder(v as CredentialSortOrder)} disabled={sortBy === 'default'}>
              <SelectTrigger size="sm" className="w-20"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="desc">降序</SelectItem>
                <SelectItem value="asc">升序</SelectItem>
              </SelectContent>
            </Select>
            <Button
              variant="outline"
              size="sm"
              className={hasActiveFilters ? 'border-primary text-primary' : ''}
              onClick={() => setShowFilters((v) => !v)}
            >
              <Filter className="h-3.5 w-3.5" />
              筛选
              {activeFilterCount > 0 && <Badge tone="primary">{activeFilterCount}</Badge>}
            </Button>
          </ToolbarActions>
        </Toolbar>

        {/* Filter Panel */}
        {showFilters && (
          <div className="mb-3 rounded-lg bg-muted/30 p-3 animate-in fade-in-0 duration-150">
            <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
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
                  <SelectItem value="custom_scheduling">有调度覆盖</SelectItem>
                  <SelectItem value="custom_priority">自定义优先级</SelectItem>
                  <SelectItem value="custom_concurrency">自定义并发</SelectItem>
                  <SelectItem value="custom_rpm">自定义 RPM</SelectItem>
                  <SelectItem value="error">有错误</SelectItem>
                  <SelectItem value="unknown_subscription">未知订阅</SelectItem>
                </SelectContent>
              </Select>
              <Select value={authFilter} onValueChange={setAuthFilter}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部认证</SelectItem>
                  <SelectItem value="social">Social</SelectItem>
                  <SelectItem value="idc">IdC</SelectItem>
                  <SelectItem value="external_idp">External IdP</SelectItem>
                  <SelectItem value="api_key">API Key</SelectItem>
                </SelectContent>
              </Select>
              <Select value={subscriptionFilter} onValueChange={setSubscriptionFilter}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部订阅</SelectItem>
                  <SelectItem value="pro_plus">Pro+</SelectItem>
                  <SelectItem value="pro">Pro</SelectItem>
                  <SelectItem value="trial">试用</SelectItem>
                  <SelectItem value="free">Free</SelectItem>
                  <SelectItem value="unknown">未知</SelectItem>
                </SelectContent>
              </Select>
              <Select value={proxyFilter} onValueChange={setProxyFilter}>
                <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">全部代理</SelectItem>
                  {(proxyResources.data?.resources || []).map((r) => (
                    <SelectItem key={r.id} value={String(r.id)}>{r.name}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            {hasActiveFilters && (
              <div className="mt-2 flex justify-end">
                <Button variant="ghost" size="xs" onClick={clearFilters}>
                  <X className="h-3.5 w-3.5" />清除筛选
                </Button>
              </div>
            )}
          </div>
        )}

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
              onClick={batchDelete}
              disabled={selectedDisabledCount === 0}
            >
              <Trash2 className="h-3.5 w-3.5" />删除已禁用 ({selectedDisabledCount})
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
            description={hasActiveFilters || queryText ? '没有匹配当前筛选条件的账号' : '点击添加按钮创建第一个账号'}
            action={
              hasActiveFilters || queryText ? (
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
            />
          </div>
        )}
      </SectionCard>

      {/* Modals */}
      <ModalShell open={creditDetailsOpen} title="剩余可用积分明细" width="max-w-4xl" onClose={() => setCreditDetailsOpen(false)}>
        {creditDetailsLoading ? (
          <LoadingState text="加载积分明细..." />
        ) : creditDetailRows.length > 0 ? (
          <div className="overflow-hidden rounded-lg bg-card shadow-sm">
            <div className="max-h-[60vh] overflow-auto">
              <table className="w-full text-sm">
                <thead className="sticky top-0 z-10 bg-card">
                  <tr>
                    <th className="px-3 py-2 text-left font-semibold text-muted-foreground w-20">ID</th>
                    <th className="px-3 py-2 text-left font-semibold text-muted-foreground">账号</th>
                    <th className="px-3 py-2 text-left font-semibold text-muted-foreground w-28">订阅</th>
                    <th className="px-3 py-2 text-right font-semibold text-muted-foreground w-28">剩余</th>
                    <th className="px-3 py-2 text-right font-semibold text-muted-foreground w-28">总额</th>
                    <th className="px-3 py-2 text-left font-semibold text-muted-foreground w-44">最近查询</th>
                  </tr>
                </thead>
                <tbody>
                  {creditDetailRows.map((row, i) => (
                    <tr key={row.id} className={i % 2 === 0 ? 'bg-muted/20' : ''}>
                      <td className="px-3 py-2 font-mono text-xs text-muted-foreground">#{row.id}</td>
                      <td className="px-3 py-2 max-w-[240px] truncate font-medium">{row.email || `账号 #${row.id}`}</td>
                      <td className="px-3 py-2 text-muted-foreground">{row.subscriptionTitle || '未知'}</td>
                      <td className="px-3 py-2 text-right font-semibold text-success">{formatCredits(row.creditRemaining)}</td>
                      <td className="px-3 py-2 text-right text-muted-foreground">{formatCredits(row.creditLimit)}</td>
                      <td className="px-3 py-2 text-xs text-muted-foreground">{row.checkedAt ? formatFullDate(row.checkedAt) : '未查询'}</td>
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
      <BatchEditCredentialsModal open={batchEditOpen} ids={Array.from(selectedIds)} onClose={() => setBatchEditOpen(false)} onDone={() => { invalidate(); setSelectedIds(new Set()) }} />
      <BatchImportModal open={batchOpen} onClose={() => setBatchOpen(false)} existingCredentials={importDuplicateCheckCredentials} onDone={invalidate} />
      <KamImportModal open={kamOpen} onClose={() => setKamOpen(false)} existingCredentials={importDuplicateCheckCredentials} onDone={invalidate} />
      <CredentialExportModal open={exportOpen} onClose={() => setExportOpen(false)} />
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
