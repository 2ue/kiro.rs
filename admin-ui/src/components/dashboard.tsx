import { useState, useEffect, useMemo, useRef } from 'react'
import { LogOut, Moon, Sun, Server, Plus, Upload, FileUp, Trash2, RotateCcw, CheckCircle2, BarChart3, Settings, DollarSign, Download, FileClock, RefreshCw, Router, Search, FileCheck2, LayoutDashboard, SlidersHorizontal, Wallet, X } from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { storage } from '@/lib/storage'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table'
import { ScrollArea } from '@/components/ui/scroll-area'
import { CredentialCard } from '@/components/credential-card'
import { AddCredentialDialog } from '@/components/add-credential-dialog'
import { BatchImportDialog } from '@/components/batch-import-dialog'
import { BatchEditCredentialsDialog } from '@/components/batch-edit-credentials-dialog'
import { KamImportDialog } from '@/components/kam-import-dialog'
import { BatchVerifyDialog, type VerifyResult } from '@/components/batch-verify-dialog'
import { CredentialTestDialog } from '@/components/credential-test-dialog'
import { UsageRecordsPanel } from '@/components/usage-records-panel'
import { UsageDashboardPanel } from '@/components/usage-dashboard-panel'
import { RuntimeConfigPanel } from '@/components/runtime-config-panel'
import { ModelPricingPanel } from '@/components/model-pricing-panel'
import { AuditLogsPanel } from '@/components/audit-logs-panel'
import { CredentialExportDialog } from '@/components/credential-export-dialog'
import { ProxyResourcesPanel } from '@/components/proxy-resources-panel'
import { AccountValidationPanel } from '@/components/account-validation-panel'
import { ExternalPoolsPanel } from '@/components/external-pools-panel'
import {
  useCredentials,
  useCredentialsAccountInfo,
  useCredentialsList,
  useCredentialsRuntime,
  useCredentialsSummary,
  useCredentialsUsageSummary,
  useBatchUpdateCredentials,
  useCredentialCreditSummary,
  useDeleteCredential,
  useDeleteDisabledCredentials,
  useLoadBalancingMode,
  useProxyResources,
  useResetFailure,
  useRuntimeConfig,
  useSetLoadBalancingMode,
} from '@/hooks/use-credentials'
import {
  forceRefreshToken,
  getCredentialInfo,
  getCredentialsAccountInfo,
  getCredentialsUsageSummary,
  refreshCredentialInfo,
  testCredential,
} from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, testModelLabel } from '@/lib/test-models'
import {
  buildCredentialRefreshReport,
  credentialRefreshSourceLabel,
  refreshCredentialInfoInBatches,
  type CredentialRefreshReport,
} from '@/lib/credential-refresh'
import type {
  BalanceResponse,
  CredentialListItem,
  CredentialSortBy,
  CredentialSortOrder,
  CredentialStatusItem,
  LoadBalancingMode,
} from '@/types/api'

const CREDIT_INFO_DETAIL_BATCH_SIZE = 500

interface CreditDetailRow {
  id: number
  email?: string | null
  subscriptionTitle?: string | null
  creditRemaining?: number | null
  creditLimit?: number | null
  checkedAt?: string | null
  estimatedCostUsd: number
  originalCostUsd: number
  disabled: boolean
}

function compareCreditDetailRows(left: CreditDetailRow, right: CreditDetailRow): number {
  if (left.disabled !== right.disabled) {
    return Number(left.disabled) - Number(right.disabled)
  }
  return left.id - right.id
}

const credentialSortOptions: Array<{ value: CredentialSortBy; label: string }> = [
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

function numericQueryValue(value: string): number | undefined {
  const trimmed = value.trim().replace(/^#/, '')
  if (!trimmed) return undefined
  const parsed = Number(trimmed)
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : undefined
}

function formatCredits(value?: number | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 }).format(value as number)
}

function formatDateTime(value?: string | null): string {
  if (!value) return '未查询'
  return new Date(value).toLocaleString('zh-CN', { hour12: false })
}

function formatUsdFixed2(value?: number | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(value as number)
}

function credentialFromListItem(item: CredentialListItem): CredentialStatusItem {
  return {
    ...item,
    failureCount: 0,
    isCurrent: false,
    expiresAt: null,
    accountInfo: undefined,
    successCount: 0,
    lastUsedAt: null,
    refreshFailureCount: 0,
    cooledDown: false,
    cooldownRemainingSecs: 0,
    cooldowns: [],
    rateLimited: false,
    rateLimitRemainingSecs: 0,
    inFlightRequests: 0,
    oldestInFlightAgeSecs: 0,
    newestInFlightIdleSecs: 0,
    maxConcurrentRequests: item.maxConcurrentRequests,
    inFlightLeaseMaxSecs: 0,
    transientFailureStreak: 0,
    recentErrorRate: 0,
    latencyEwmaMs: null,
    lastErrorAtMs: null,
    inProbation: false,
    probationRemainingSecs: 0,
    schedulerSelectionCount: 0,
    recentSchedulerSelectionCount10s: 0,
    recentSchedulerSelectionCount60s: 0,
    recentSchedulerSelectionCount5m: 0,
    schedulerSelectionPressure: 0,
    schedulerScore: 0,
    estimatedCostUsd: 0,
    originalCostUsd: 0,
    kiroMeteringUsage: 0,
    pricedRequests: 0,
    unpricedRequests: 0,
  }
}

function CreditRefreshReportPanel({
  report,
  onClear,
}: {
  report: CredentialRefreshReport
  onClear: () => void
}) {
  return (
    <Card className="border-amber-500/30 bg-amber-500/5">
      <CardHeader className="flex flex-row items-center justify-between gap-3 pb-3">
        <div className="min-w-0">
          <CardTitle className="text-base">最近一次积分查询结果</CardTitle>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <Badge variant="success">成功 {report.success}</Badge>
            <Badge variant={report.failed > 0 ? 'warning' : 'secondary'}>失败 {report.failed}</Badge>
            <span>总数 {report.total}</span>
          </div>
        </div>
        <Button variant="ghost" size="sm" onClick={onClear} title="清除查询结果">
          <X className="h-4 w-4" />
          清除
        </Button>
      </CardHeader>
      <CardContent className="space-y-2">
        {report.failed === 0 ? (
          <p className="text-sm text-muted-foreground">本次查询没有失败账号。</p>
        ) : (
          report.groups.map((group) => (
            <div key={group.key} className="rounded-md border bg-background/70 p-3">
              <div className="flex flex-wrap items-center gap-2">
                <Badge variant={group.source === 'external_pool' ? 'secondary' : group.source === 'unknown' ? 'outline' : 'destructive'}>
                  {credentialRefreshSourceLabel(group.source)}
                </Badge>
                <Badge variant="outline">{group.count} 个</Badge>
                <code className="text-xs font-semibold">{group.fingerprint}</code>
              </div>
              <p className="mt-1 break-words text-xs text-muted-foreground">{group.message}</p>
              {group.items.length > 0 && (
                <div className="mt-2 max-h-36 overflow-auto rounded border bg-muted/20 p-2 text-xs">
                  <div className="grid gap-1 md:grid-cols-2">
                    {group.items.map((item) => (
                      <div key={`${group.key}-${item.id}`} className="min-w-0">
                        <span className="font-mono">#{item.id}</span>
                        <span className="ml-1 text-muted-foreground">{item.email || '未返回账号'}</span>
                        <span className={`ml-1 ${item.disabled ? 'text-destructive' : 'text-green-600'}`}>
                          {item.disabled ? '已禁用' : '启用'}
                        </span>
                        {item.error && <div className="break-words text-[0.68rem] text-muted-foreground">{item.error}</div>}
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ))
        )}
      </CardContent>
    </Card>
  )
}

interface DashboardProps {
  onLogout: () => void
}

export function Dashboard({ onLogout }: DashboardProps) {
  const [testingCredential, setTestingCredential] = useState<CredentialStatusItem | null>(null)
  const [testDialogOpen, setTestDialogOpen] = useState(false)
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [batchImportDialogOpen, setBatchImportDialogOpen] = useState(false)
  const [batchEditDialogOpen, setBatchEditDialogOpen] = useState(false)
  const [kamImportDialogOpen, setKamImportDialogOpen] = useState(false)
  const [exportDialogOpen, setExportDialogOpen] = useState(false)
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())
  const [verifyDialogOpen, setVerifyDialogOpen] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [verifyResults, setVerifyResults] = useState<Map<number, VerifyResult>>(new Map())
  const [balanceMap, setBalanceMap] = useState<Map<number, BalanceResponse>>(new Map())
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(new Set())
  const [queryingInfo, setQueryingInfo] = useState(false)
  const [lastCreditRefresh, setLastCreditRefresh] = useState<CredentialRefreshReport | null>(null)
  const [queryText, setQueryText] = useState('')
  const [statusFilter, setStatusFilter] = useState('all')
  const [authFilter, setAuthFilter] = useState('all')
  const [subscriptionFilter, setSubscriptionFilter] = useState('all')
  const [proxyFilter, setProxyFilter] = useState('all')
  const [sortBy, setSortBy] = useState<CredentialSortBy>('default')
  const [sortOrder, setSortOrder] = useState<CredentialSortOrder>('desc')
  const [batchRefreshing, setBatchRefreshing] = useState(false)
  const [batchRefreshProgress, setBatchRefreshProgress] = useState({ current: 0, total: 0 })
  const [credentialIdQuery, setCredentialIdQuery] = useState('')
  const [accountQuery, setAccountQuery] = useState('')
  const [regionQuery, setRegionQuery] = useState('')
  const [modelQuery, setModelQuery] = useState('')
  const [endpointQuery, setEndpointQuery] = useState('')
  const [priorityQuery, setPriorityQuery] = useState('')
  const [rpmQuery, setRpmQuery] = useState('')
  const [concurrencyQuery, setConcurrencyQuery] = useState('')
  const [activeTab, setActiveTab] = useState<'dashboard' | 'credentials' | 'validation' | 'proxies' | 'external' | 'usage' | 'pricing' | 'audit' | 'config'>('credentials')
  const [creditDetailDialogOpen, setCreditDetailDialogOpen] = useState(false)
  const [creditDetailsLoading, setCreditDetailsLoading] = useState(false)
  const [creditDetailRows, setCreditDetailRows] = useState<CreditDetailRow[]>([])
  const cancelVerifyRef = useRef(false)
  const [currentPage, setCurrentPage] = useState(1)
  const itemsPerPage = 12
  const [darkMode, setDarkMode] = useState(() => {
    if (typeof window !== 'undefined') {
      return document.documentElement.classList.contains('dark')
    }
    return false
  })

  const queryClient = useQueryClient()
  const credentialsQuery = useMemo(
    () => ({
      page: currentPage,
      limit: itemsPerPage,
      q: queryText.trim() || undefined,
      credentialId: numericQueryValue(credentialIdQuery),
      account: accountQuery.trim() || undefined,
      region: regionQuery.trim() || undefined,
      model: modelQuery.trim() || undefined,
      endpoint: endpointQuery.trim() || undefined,
      priority: numericQueryValue(priorityQuery),
      rpm: numericQueryValue(rpmQuery),
      concurrency: numericQueryValue(concurrencyQuery),
      status: statusFilter !== 'all' ? statusFilter : undefined,
      authMethod: authFilter !== 'all' ? authFilter : undefined,
      subscription: subscriptionFilter !== 'all' ? subscriptionFilter : undefined,
      proxyResourceId: proxyFilter !== 'all' ? Number(proxyFilter) : undefined,
      sortBy: sortBy !== 'default' ? sortBy : undefined,
      sortOrder: sortBy !== 'default' ? sortOrder : undefined,
    }),
    [accountQuery, authFilter, concurrencyQuery, credentialIdQuery, currentPage, endpointQuery, modelQuery, priorityQuery, proxyFilter, queryText, regionQuery, rpmQuery, sortBy, sortOrder, statusFilter, subscriptionFilter]
  )
  const {
    data: listData,
    isLoading: isListLoading,
    error: listError,
    refetch: refetchList,
    isFetching: isListFetching,
    isPlaceholderData: isListPlaceholderData,
  } = useCredentialsList(credentialsQuery)
  const {
    data: summaryData,
    isLoading: isSummaryLoading,
    error: summaryError,
    refetch: refetchSummary,
  } = useCredentialsSummary()
  const visibleCredentialIds = useMemo(
    () => listData?.items.map((credential) => credential.id) || [],
    [listData?.items]
  )
  const runtimeQuery = useCredentialsRuntime(visibleCredentialIds)
  const accountInfoQuery = useCredentialsAccountInfo(visibleCredentialIds)
  const usageSummaryQuery = useCredentialsUsageSummary(visibleCredentialIds)
  const allCredentialsQuery = useCredentials({ enabled: false, refetchInterval: false })

  const refinedCreditStats = useMemo(() => {
    const enabled = creditDetailRows.filter((row) => !row.disabled)
    return {
      enabledCreditRemaining: enabled.reduce((sum, row) => sum + (row.creditRemaining ?? 0), 0),
      totalCreditLimit: creditDetailRows.reduce((sum, row) => sum + (row.creditLimit ?? 0), 0),
      totalEstimatedCostUsd: creditDetailRows.reduce((sum, row) => sum + row.estimatedCostUsd, 0),
      totalOriginalCostUsd: creditDetailRows.reduce((sum, row) => sum + row.originalCostUsd, 0),
    }
  }, [creditDetailRows])
  const orderedCreditDetailRows = useMemo(
    () => [...creditDetailRows].sort(compareCreditDetailRows),
    [creditDetailRows],
  )

  const { mutate: deleteCredential } = useDeleteCredential()
  const deleteDisabled = useDeleteDisabledCredentials()
  const batchUpdateCredentials = useBatchUpdateCredentials()
  const { mutate: resetFailure } = useResetFailure()
  const { data: loadBalancingData, isLoading: isLoadingMode } = useLoadBalancingMode()
  const { data: proxyResourcesData } = useProxyResources()
  const { mutate: setLoadBalancingMode, isPending: isSettingMode } = useSetLoadBalancingMode()
  const runtimeConfig = useRuntimeConfig()
  const creditSummary = useCredentialCreditSummary()
  const refetch = () => {
    refetchList()
    refetchSummary()
    runtimeQuery.refetch()
    accountInfoQuery.refetch()
    usageSummaryQuery.refetch()
    creditSummary.refetch()
  }

  const loadCreditDetails = async () => {
    setCreditDetailsLoading(true)
    try {
      const snapshot = (await allCredentialsQuery.refetch()).data
      const credentials = [...(snapshot?.credentials ?? [])].sort((left, right) => left.id - right.id)
      const ids = credentials.map((credential) => credential.id)
      const batches: number[][] = []
      for (let start = 0; start < ids.length; start += CREDIT_INFO_DETAIL_BATCH_SIZE) {
        batches.push(ids.slice(start, start + CREDIT_INFO_DETAIL_BATCH_SIZE))
      }

      const [accountInfoResponses, usageSummaryResponses] = await Promise.all([
        Promise.all(batches.map((batch) => getCredentialsAccountInfo(batch))),
        Promise.all(batches.map((batch) => getCredentialsUsageSummary(batch))),
      ])
      const accountInfoById = new Map(
        accountInfoResponses.flatMap((response) => response.items).map((item) => [item.id, item]),
      )
      const usageById = new Map(
        usageSummaryResponses.flatMap((response) => response.items).map((item) => [item.id, item]),
      )

      setCreditDetailRows(credentials.map((credential) => {
        const accountInfo = accountInfoById.get(credential.id)
        const usage = usageById.get(credential.id)
        return {
          id: credential.id,
          email: credential.email,
          subscriptionTitle: accountInfo?.subscriptionTitle ?? credential.subscriptionTitle,
          creditRemaining: accountInfo?.creditRemaining,
          creditLimit: accountInfo?.creditLimit,
          checkedAt: accountInfo?.checkedAt,
          estimatedCostUsd: usage?.estimatedCostUsd ?? 0,
          originalCostUsd: usage?.originalCostUsd ?? 0,
          disabled: credential.disabled,
        }
      }))
    } catch (error) {
      toast.error(`加载所有账号积分明细失败: ${extractErrorMessage(error)}`)
    } finally {
      setCreditDetailsLoading(false)
    }
  }

  const openCreditDetails = () => {
    setCreditDetailDialogOpen(true)
    void loadCreditDetails()
  }

  // 计算分页
  const totalPages = listData?.totalPages || 0
  const credentialsPage = listData?.page
  const isLoading = isListLoading || isSummaryLoading
  const error = listError || summaryError
  const pageTransitionPending = credentialsPage !== undefined && (isListPlaceholderData || (isListFetching && credentialsPage !== currentPage))
  const data = useMemo(() => {
    if (!listData && !summaryData) {
      return undefined
    }
    return {
      total: summaryData?.total ?? listData?.total ?? 0,
      available: summaryData?.available ?? listData?.available ?? 0,
      currentId: summaryData?.currentId || 0,
      globalInFlightRequests: summaryData?.globalInFlightRequests ?? 0,
      queuedRequests: summaryData?.queuedRequests ?? 0,
      globalMaxConcurrentRequests: summaryData?.globalMaxConcurrentRequests ?? 0,
      maxQueuedRequests: summaryData?.maxQueuedRequests ?? 0,
      page: listData?.page ?? currentPage,
      limit: listData?.limit ?? itemsPerPage,
      totalPages: listData?.totalPages ?? 0,
      filteredTotal: listData?.filteredTotal ?? 0,
      filteredAvailable: listData?.filteredAvailable ?? 0,
    }
  }, [currentPage, listData, summaryData])
  const currentCredentials = useMemo(() => {
    const runtimeById = new Map((runtimeQuery.data?.items || []).map((item) => [item.id, item]))
    const accountById = new Map((accountInfoQuery.data?.items || []).map((item) => [item.id, item]))
    const usageById = new Map((usageSummaryQuery.data?.items || []).map((item) => [item.id, item]))
    return (listData?.items || []).map((item) => {
      const runtimeItem = runtimeById.get(item.id)
      const usageItem = usageById.get(item.id)
      return {
        ...credentialFromListItem(item),
        ...runtimeItem,
        ...item,
        accountInfo: accountById.get(item.id),
        estimatedCostUsd: usageItem?.estimatedCostUsd ?? 0,
        originalCostUsd: usageItem?.originalCostUsd ?? 0,
        kiroMeteringUsage: usageItem?.kiroMeteringUsage ?? 0,
        pricedRequests: usageItem?.pricedRequests ?? 0,
        unpricedRequests: usageItem?.unpricedRequests ?? 0,
      }
    })
  }, [accountInfoQuery.data?.items, listData?.items, runtimeQuery.data?.items, usageSummaryQuery.data?.items])
  const disabledCredentialCount = summaryData?.disabled ?? Math.max((data?.total || 0) - (data?.available || 0), 0)
  const selectedDisabledCount = Array.from(selectedIds).filter(id => {
    const credential = currentCredentials.find(c => c.id === id)
    return Boolean(credential?.disabled)
  }).length
  const selectedCredentials = currentCredentials.filter((credential) => selectedIds.has(credential.id))
  const selectedPriorityOverrideCount = selectedCredentials.filter((credential) => credential.priority !== 0).length
  const selectedConcurrencyOverrideCount = selectedCredentials.filter((credential) => typeof credential.maxConcurrentRequestsOverride === 'number').length
  const selectedRpmOverrideCount = selectedCredentials.filter((credential) => typeof credential.rpmOverride === 'number').length
  const hasCredentialFilters = Boolean(
    queryText.trim() ||
    credentialIdQuery.trim() ||
    accountQuery.trim() ||
    regionQuery.trim() ||
    modelQuery.trim() ||
    endpointQuery.trim() ||
    priorityQuery.trim() ||
    rpmQuery.trim() ||
    concurrencyQuery.trim() ||
    statusFilter !== 'all' ||
    authFilter !== 'all' ||
    subscriptionFilter !== 'all' ||
    proxyFilter !== 'all' ||
    sortBy !== 'default'
  )

  const clearCredentialFilters = () => {
    setQueryText('')
    setCredentialIdQuery('')
    setAccountQuery('')
    setRegionQuery('')
    setModelQuery('')
    setEndpointQuery('')
    setPriorityQuery('')
    setRpmQuery('')
    setConcurrencyQuery('')
    setStatusFilter('all')
    setAuthFilter('all')
    setSubscriptionFilter('all')
    setProxyFilter('all')
    setSortBy('default')
    setSortOrder('desc')
  }

  // 后台分页总数变化时，避免停留在不存在的页码。
  useEffect(() => {
    if (!data) {
      return
    }

    const nextPage = data.totalPages > 0 ? Math.min(currentPage, data.totalPages) : 1
    if (currentPage !== nextPage) {
      setCurrentPage(nextPage)
    }
  }, [currentPage, data])

  useEffect(() => {
    setSelectedIds(prev => prev.size === 0 ? prev : new Set())
  }, [currentPage])

  useEffect(() => {
    setCurrentPage(1)
    setSelectedIds(new Set())
  }, [accountQuery, authFilter, concurrencyQuery, credentialIdQuery, endpointQuery, modelQuery, priorityQuery, proxyFilter, queryText, regionQuery, rpmQuery, sortBy, sortOrder, statusFilter, subscriptionFilter])

  // 只保留当前仍存在的凭据缓存，避免删除后残留旧数据
  useEffect(() => {
    if (!listData?.items) {
      setBalanceMap(new Map())
      setLoadingBalanceIds(new Set())
      return
    }

    const validIds = new Set(currentCredentials.map(credential => credential.id))

    setBalanceMap(prev => {
      const next = new Map<number, BalanceResponse>()
      prev.forEach((value, id) => {
        if (validIds.has(id)) {
          next.set(id, value)
        }
      })
      return next.size === prev.size ? prev : next
    })

    setLoadingBalanceIds(prev => {
      if (prev.size === 0) {
        return prev
      }
      const next = new Set<number>()
      prev.forEach(id => {
        if (validIds.has(id)) {
          next.add(id)
        }
      })
      return next.size === prev.size ? prev : next
    })
  }, [currentCredentials, listData?.items])

  const toggleDarkMode = () => {
    setDarkMode(!darkMode)
    document.documentElement.classList.toggle('dark')
  }

  const handleTestCredential = (credential: CredentialStatusItem) => {
    setTestingCredential(credential)
    setTestDialogOpen(true)
  }

  const handleLogout = () => {
    storage.removeApiKey()
    queryClient.clear()
    onLogout()
  }

  // 选择管理
  const toggleSelect = (id: number) => {
    const newSelected = new Set(selectedIds)
    if (newSelected.has(id)) {
      newSelected.delete(id)
    } else {
      newSelected.add(id)
    }
    setSelectedIds(newSelected)
  }

  const deselectAll = () => {
    setSelectedIds(new Set())
  }

  // 批量删除（仅删除已禁用项）
  const handleBatchDelete = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要删除的凭据')
      return
    }

    const disabledIds = Array.from(selectedIds).filter(id => {
      const credential = currentCredentials.find(c => c.id === id)
      return Boolean(credential?.disabled)
    })

    if (disabledIds.length === 0) {
      toast.error('选中的凭据中没有已禁用项')
      return
    }

    const skippedCount = selectedIds.size - disabledIds.length
    const skippedText = skippedCount > 0 ? `（将跳过 ${skippedCount} 个未禁用凭据）` : ''

    if (!confirm(`确定要删除 ${disabledIds.length} 个已禁用凭据吗？此操作无法撤销。${skippedText}`)) {
      return
    }

    let successCount = 0
    let failCount = 0

    for (const id of disabledIds) {
      try {
        await new Promise<void>((resolve, reject) => {
          deleteCredential(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    const skippedResultText = skippedCount > 0 ? `，已跳过 ${skippedCount} 个未禁用凭据` : ''

    if (failCount === 0) {
      toast.success(`成功删除 ${successCount} 个已禁用凭据${skippedResultText}`)
    } else {
      toast.warning(`删除已禁用凭据：成功 ${successCount} 个，失败 ${failCount} 个${skippedResultText}`)
    }

    deselectAll()
  }

  // 批量恢复异常
  const handleBatchResetFailure = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要恢复的凭据')
      return
    }

    const failedIds = Array.from(selectedIds).filter(id => {
      const cred = currentCredentials.find(c => c.id === id)
      return cred && cred.failureCount > 0
    })

    if (failedIds.length === 0) {
      toast.error('选中的凭据中没有失败的凭据')
      return
    }

    let successCount = 0
    let failCount = 0

    for (const id of failedIds) {
      try {
        await new Promise<void>((resolve, reject) => {
          resetFailure(id, {
            onSuccess: () => {
              successCount++
              resolve()
            },
            onError: (err) => {
              failCount++
              reject(err)
            }
          })
        })
      } catch (error) {
        // 错误已在 onError 中处理
      }
    }

    if (failCount === 0) {
      toast.success(`成功恢复 ${successCount} 个凭据`)
    } else {
      toast.warning(`成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  // 批量刷新 Token
  const handleBatchForceRefresh = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要刷新的凭据')
      return
    }

    const refreshableIds = Array.from(selectedIds).filter(id => {
      const cred = currentCredentials.find(c => c.id === id)
      return cred && cred.authMethod !== 'api_key'
    })
    const skippedCount = selectedIds.size - refreshableIds.length

    if (refreshableIds.length === 0) {
      toast.error('选中的凭据中没有可刷新 Token 的 OAuth 凭据')
      return
    }

    setBatchRefreshing(true)
    setBatchRefreshProgress({ current: 0, total: refreshableIds.length })

    let successCount = 0
    let failCount = 0

    for (let i = 0; i < refreshableIds.length; i++) {
      try {
        await forceRefreshToken(refreshableIds[i])
        successCount++
      } catch {
        failCount++
      }
      setBatchRefreshProgress({ current: i + 1, total: refreshableIds.length })
    }

    setBatchRefreshing(false)
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    queryClient.invalidateQueries({ queryKey: ['credential-balance'] })
    setBalanceMap(prev => {
      const next = new Map(prev)
      refreshableIds.forEach(id => next.delete(id))
      return next
    })
    const skippedText = skippedCount > 0 ? `，跳过 ${skippedCount} 个 API Key 凭据` : ''

    if (failCount === 0) {
      toast.success(`成功刷新 ${successCount} 个凭据的 Token${skippedText}`)
    } else {
      toast.warning(`刷新 Token：成功 ${successCount} 个，失败 ${failCount} 个${skippedText}`)
    }

    deselectAll()
  }

  const handleBatchQuerySelectedInfo = async () => {
    const ids = Array.from(selectedIds)
    if (ids.length === 0) {
      toast.error('请先选择要查询信息的账号')
      return
    }
    setQueryingInfo(true)
    setLoadingBalanceIds(prev => {
      const next = new Set(prev)
      ids.forEach(id => next.add(id))
      return next
    })
    try {
      const responses = await refreshCredentialInfoInBatches(
        ids,
        (batchIds) => refreshCredentialInfo(batchIds, true),
        {
          errorMessage: extractErrorMessage,
          onBatchCompleted: (batchIds, response) => {
            setBalanceMap(prev => {
              const next = new Map(prev)
              response.items.forEach(item => {
                if (item.ok && item.info) next.set(item.id, item.info)
              })
              return next
            })
            setLoadingBalanceIds(prev => {
              const next = new Set(prev)
              batchIds.forEach(id => next.delete(id))
              return next
            })
          },
        },
      )
      const report = buildCredentialRefreshReport(responses)
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
      queryClient.invalidateQueries({ queryKey: ['credential-credit-summary'] })
      setLastCreditRefresh(report)
      if (report.failed === 0) {
        toast.success(`查询完成：成功 ${report.success}/${report.total}`)
      } else {
        toast.warning(`查询完成：成功 ${report.success} 个，失败 ${report.failed} 个`)
      }
    } catch (error) {
      toast.error(`查询信息失败: ${extractErrorMessage(error)}`)
    } finally {
      setQueryingInfo(false)
      setLoadingBalanceIds(prev => {
        const next = new Set(prev)
        ids.forEach(id => next.delete(id))
        return next
      })
    }
  }

  const handleBatchResetPriority = () => {
    const ids = selectedCredentials.filter((credential) => credential.priority !== 0).map((credential) => credential.id)
    if (ids.length === 0) {
      toast.error('选中的账号没有自定义优先级')
      return
    }
    batchUpdateCredentials.mutate(
      { ids, priority: { priority: 0 } },
      {
        onSuccess: (response) => {
          refetch()
          if (response.failed === 0) toast.success(`已重置 ${response.success} 个账号优先级`)
          else toast.warning(`重置优先级：成功 ${response.success} 个，失败 ${response.failed} 个`)
        },
        onError: (error) => toast.error(`重置优先级失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const handleBatchClearConcurrency = () => {
    const ids = selectedCredentials.filter((credential) => typeof credential.maxConcurrentRequestsOverride === 'number').map((credential) => credential.id)
    if (ids.length === 0) {
      toast.error('选中的账号没有自定义并发')
      return
    }
    batchUpdateCredentials.mutate(
      { ids, concurrency: { maxConcurrentRequests: null } },
      {
        onSuccess: (response) => {
          refetch()
          if (response.failed === 0) toast.success(`已清除 ${response.success} 个账号并发覆盖`)
          else toast.warning(`清除并发覆盖：成功 ${response.success} 个，失败 ${response.failed} 个`)
        },
        onError: (error) => toast.error(`清除并发覆盖失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  const handleBatchClearRpm = () => {
    const ids = selectedCredentials.filter((credential) => typeof credential.rpmOverride === 'number').map((credential) => credential.id)
    if (ids.length === 0) {
      toast.error('选中的账号没有自定义 RPM')
      return
    }
    batchUpdateCredentials.mutate(
      { ids, rpm: { rpm: null } },
      {
        onSuccess: (response) => {
          refetch()
          if (response.failed === 0) toast.success(`已清除 ${response.success} 个账号 RPM 覆盖`)
          else toast.warning(`清除 RPM 覆盖：成功 ${response.success} 个，失败 ${response.failed} 个`)
        },
        onError: (error) => toast.error(`清除 RPM 覆盖失败: ${extractErrorMessage(error)}`),
      }
    )
  }

  // 一键清除所有已禁用凭据
  const handleClearAll = async () => {
    if (!data || data.total === 0) {
      toast.error('没有可清除的凭据')
      return
    }

    if (disabledCredentialCount === 0) {
      toast.error('没有可清除的已禁用凭据')
      return
    }

    if (!confirm(`确定要清除所有 ${disabledCredentialCount} 个已禁用凭据吗？此操作无法撤销。`)) {
      return
    }

    try {
      const response = await deleteDisabled.mutateAsync()
      if (response.failed === 0) {
        toast.success(`成功清除 ${response.success} 个已禁用凭据`)
      } else {
        toast.warning(`清除已禁用凭据：成功 ${response.success} 个，失败 ${response.failed} 个`)
      }
      deselectAll()
    } catch (error) {
      toast.error(`清除已禁用凭据失败: ${extractErrorMessage(error)}`)
    }
  }

  const fetchBalanceForCredential = async (id: number) => {
    setLoadingBalanceIds(prev => {
      const next = new Set(prev)
      next.add(id)
      return next
    })

    try {
      const balance = await getCredentialInfo(id, true)
      setBalanceMap(prev => {
        const next = new Map(prev)
        next.set(id, balance)
        return next
      })
      return { ok: true as const, balance }
    } catch (error) {
      return { ok: false as const, error }
    } finally {
      setLoadingBalanceIds(prev => {
        const next = new Set(prev)
        next.delete(id)
        return next
      })
    }
  }

  const handleQueryCredentialBalance = async (id: number) => {
    const result = await fetchBalanceForCredential(id)
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    queryClient.invalidateQueries({ queryKey: ['credentials-page'] })

    if (result.ok) {
      toast.success(`凭据 #${id} 信息已更新`)
    } else {
      toast.error(`查询信息失败: ${extractErrorMessage(result.error)}`)
    }
  }

  // 查询账号信息。后端批量接口会逐个查询并返回每个凭据的结果，避免前端制造请求风暴。
  const handleQueryCurrentPageInfo = async (enabledOnly = false) => {
    const ids = enabledOnly
      ? ((allCredentialsQuery.data ?? (await allCredentialsQuery.refetch()).data)?.credentials || [])
        .filter(credential => !credential.disabled)
        .map(credential => credential.id)
      : currentCredentials.map(credential => credential.id)

    if (ids.length === 0) {
      toast.error(enabledOnly ? '没有启用凭据可查询' : '当前页没有可查询信息的凭据')
      return
    }

    setQueryingInfo(true)
    setLoadingBalanceIds(prev => {
      const next = new Set(prev)
      ids.forEach(id => next.add(id))
      return next
    })

    const toastId = toast.loading(`正在查询账号信息... 0/${ids.length}`, {
      duration: Infinity,
    })

    try {
      const responses = await refreshCredentialInfoInBatches(
        ids,
        (batchIds) => refreshCredentialInfo(batchIds, true),
        {
          errorMessage: extractErrorMessage,
          onBatchCompleted: (batchIds, response) => {
            setBalanceMap(prev => {
              const next = new Map(prev)
              response.items.forEach(item => {
                if (item.ok && item.info) next.set(item.id, item.info)
              })
              return next
            })
            setLoadingBalanceIds(prev => {
              const next = new Set(prev)
              batchIds.forEach(id => next.delete(id))
              return next
            })
          },
        },
      )
      const report = buildCredentialRefreshReport(responses)
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['credentials-page'] })
      queryClient.invalidateQueries({ queryKey: ['credential-credit-summary'] })

      toast.dismiss(toastId)
      setLastCreditRefresh(report)
      if (report.failed === 0) {
        toast.success(`查询完成：成功 ${report.success}/${report.total}`)
      } else {
        toast.warning(`查询完成：成功 ${report.success} 个，失败 ${report.failed} 个`)
      }
    } catch (error) {
      toast.dismiss(toastId)
      toast.error(`查询信息失败: ${extractErrorMessage(error)}`)
    } finally {
      setQueryingInfo(false)
      setLoadingBalanceIds(prev => {
        const next = new Set(prev)
        ids.forEach(id => next.delete(id))
        return next
      })
    }
  }

  // 批量验活
  const handleBatchVerify = async () => {
    if (selectedIds.size === 0) {
      toast.error('请先选择要验活的凭据')
      return
    }

    // 初始化状态
    setVerifying(true)
    cancelVerifyRef.current = false
    const ids = Array.from(selectedIds)
    setVerifyProgress({ current: 0, total: ids.length })

    let successCount = 0

    // 初始化结果，所有凭据状态为 pending
    const initialResults = new Map<number, VerifyResult>()
    ids.forEach(id => {
      initialResults.set(id, { id, status: 'pending' })
    })
    setVerifyResults(initialResults)
    setVerifyDialogOpen(true)

    // 开始验活
    for (let i = 0; i < ids.length; i++) {
      // 检查是否取消
      if (cancelVerifyRef.current) {
        toast.info('已取消验活')
        break
      }

      const id = ids[i]

      // 更新当前凭据状态为 verifying
      setVerifyResults(prev => {
        const newResults = new Map(prev)
        newResults.set(id, { id, status: 'verifying' })
        return newResults
      })

      try {
        const response = await testCredential(id, {
          model: DEFAULT_TEST_MODEL,
          prompt: DEFAULT_TEST_PROMPT,
        })
        successCount++

        // 更新为成功状态
        setVerifyResults(prev => {
          const newResults = new Map(prev)
          newResults.set(id, {
            id,
            status: 'success',
            model: testModelLabel(response.model),
            response: response.response,
          })
          return newResults
        })
      } catch (error) {
        // 更新为失败状态
        setVerifyResults(prev => {
          const newResults = new Map(prev)
          newResults.set(id, {
            id,
            status: 'failed',
            error: extractErrorMessage(error)
          })
          return newResults
        })
      }

      // 更新进度
      setVerifyProgress({ current: i + 1, total: ids.length })

      // 添加延迟防止封号（最后一个不需要延迟）
      if (i < ids.length - 1 && !cancelVerifyRef.current) {
        await new Promise(resolve => setTimeout(resolve, 2000))
      }
    }

    setVerifying(false)

    if (!cancelVerifyRef.current) {
      toast.success(`验活完成：成功 ${successCount}/${ids.length}`)
    }
  }

  // 取消验活
  const handleCancelVerify = () => {
    cancelVerifyRef.current = true
    setVerifying(false)
  }

  const handleLoadBalancingChange = (newMode: LoadBalancingMode) => {
    setLoadBalancingMode(newMode, {
      onSuccess: () => {
        const modeName = newMode === 'priority' ? '优先级模式' : newMode === 'balanced' ? '均衡负载模式' : '健康均衡模式'
        toast.success(`已切换到${modeName}`)
      },
      onError: (error) => {
        toast.error(`切换失败: ${extractErrorMessage(error)}`)
      }
    })
  }

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto mb-4"></div>
          <p className="text-muted-foreground">加载中...</p>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background p-4">
        <Card className="w-full max-w-md">
          <CardContent className="pt-6 text-center">
            <div className="text-red-500 mb-4">加载失败</div>
            <p className="text-muted-foreground mb-4">{extractErrorMessage(error)}</p>
            <div className="space-x-2">
              <Button onClick={() => refetch()}>重试</Button>
              <Button variant="outline" onClick={handleLogout}>重新登录</Button>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-background">
      {/* 顶部导航 */}
      <header className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="container flex h-14 items-center justify-between px-4 md:px-8">
          <div className="flex items-center gap-2">
            <Server className="h-5 w-5" />
            <div className="leading-tight">
              <div className="font-semibold">Kiro Admin</div>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant={activeTab === 'dashboard' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('dashboard')}
            >
              <LayoutDashboard className="h-4 w-4" />
              总览
            </Button>
            <Button
              variant={activeTab === 'credentials' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('credentials')}
            >
              <Server className="h-4 w-4" />
              凭据
            </Button>
            <Button
              variant={activeTab === 'validation' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('validation')}
            >
              <FileCheck2 className="h-4 w-4" />
              校验
            </Button>
            <Button
              variant={activeTab === 'proxies' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('proxies')}
            >
              <Router className="h-4 w-4" />
              代理
            </Button>
            <Button
              variant={activeTab === 'external' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('external')}
            >
              <Router className="h-4 w-4" />
              备用池
            </Button>
            <Button
              variant={activeTab === 'usage' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('usage')}
            >
              <BarChart3 className="h-4 w-4" />
              Usage
            </Button>
            <Button
              variant={activeTab === 'pricing' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('pricing')}
            >
              <DollarSign className="h-4 w-4" />
              价格
            </Button>
            <Button
              variant={activeTab === 'audit' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('audit')}
            >
              <FileClock className="h-4 w-4" />
              审计
            </Button>
            <Button
              variant={activeTab === 'config' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('config')}
            >
              <Settings className="h-4 w-4" />
              配置
            </Button>
            <select
              className="h-9 rounded-md border bg-background px-3 text-sm"
              value={loadBalancingData?.mode || 'priority'}
              disabled={isLoadingMode || isSettingMode}
              title="负载均衡模式"
              onChange={(event) => handleLoadBalancingChange(event.target.value as LoadBalancingMode)}
            >
              <option value="priority">优先级模式</option>
              <option value="balanced">均衡负载模式</option>
              <option value="health_balanced">健康均衡模式</option>
              <option value="weighted_least_inflight">低负载优先模式</option>
            </select>
            <Button variant="ghost" size="icon" onClick={toggleDarkMode}>
              {darkMode ? <Sun className="h-5 w-5" /> : <Moon className="h-5 w-5" />}
            </Button>
            <Button variant="ghost" size="icon" onClick={handleLogout}>
              <LogOut className="h-5 w-5" />
            </Button>
          </div>
        </div>
      </header>

      {/* 主内容 */}
      <main className="container mx-auto px-4 md:px-8 py-6">
        {activeTab === 'dashboard' ? (
          <UsageDashboardPanel />
        ) : activeTab === 'usage' ? (
          <UsageRecordsPanel />
        ) : activeTab === 'validation' ? (
          <AccountValidationPanel />
        ) : activeTab === 'proxies' ? (
          <ProxyResourcesPanel />
        ) : activeTab === 'external' ? (
          <ExternalPoolsPanel />
        ) : activeTab === 'pricing' ? (
          <ModelPricingPanel />
        ) : activeTab === 'audit' ? (
          <AuditLogsPanel />
        ) : activeTab === 'config' ? (
          <RuntimeConfigPanel />
        ) : (
          <>
        {/* 统计卡片 */}
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-6 mb-6">
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                凭据总数
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{data?.total || 0}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                可用凭据
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold text-green-600">{data?.available || 0}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                当前活跃
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold flex items-center gap-2">
                #{data?.currentId || '-'}
                <Badge variant="success">活跃</Badge>
              </div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">调度容量</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">
                {data?.globalInFlightRequests || 0}/{data?.globalMaxConcurrentRequests || '不限'}
              </div>
              <div className="text-xs text-muted-foreground">全局并发 · 排队 {data?.queuedRequests || 0}/{data?.maxQueuedRequests || '不限'}</div>
            </CardContent>
          </Card>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">单凭据并发</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">
                {runtimeConfig.data?.credentialMaxConcurrentRequests || '不限'}
              </div>
              <div className="text-xs text-muted-foreground">每个凭据同时处理请求上限</div>
            </CardContent>
          </Card>
          <Card
            className="cursor-pointer hover:border-primary/50 transition-colors"
            onClick={openCreditDetails}
          >
            <CardHeader className="pb-2">
              <CardTitle className="flex items-center gap-1 text-sm font-medium text-muted-foreground">
                <Wallet className="h-3.5 w-3.5" />
                启用积分
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="text-2xl font-bold">{formatCredits(creditSummary.data?.enabledCreditRemaining)}</div>
              <div className="text-xs text-muted-foreground">
                总额 {formatCredits(creditSummary.data?.enabledCreditLimit)} · {formatDateTime(creditSummary.data?.lastCheckedAt)}
              </div>
              <div className="mt-1 text-xs text-muted-foreground">
                已记录 {formatUsdFixed2(creditSummary.data?.enabledEstimatedCostUsd)} · 原始 {formatUsdFixed2(creditSummary.data?.enabledOriginalCostUsd)}
              </div>
            </CardContent>
          </Card>
          </div>

          {lastCreditRefresh && (
            <div className="mb-6">
              <CreditRefreshReportPanel report={lastCreditRefresh} onClear={() => setLastCreditRefresh(null)} />
            </div>
          )}

        {/* 凭据列表 */}
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-4">
              <h2 className="text-xl font-semibold">凭据管理</h2>
              {selectedIds.size > 0 && (
                <div className="flex items-center gap-2">
                  <Badge variant="secondary">已选择 {selectedIds.size} 个</Badge>
                  <Button onClick={deselectAll} size="sm" variant="ghost">
                    取消选择
                  </Button>
                </div>
              )}
            </div>
            <div className="flex gap-2">
              <Button onClick={() => refetch()} size="sm" variant="outline">
                <RefreshCw className="h-4 w-4 mr-2" />
                刷新列表
              </Button>
              {selectedIds.size > 0 && (
                <>
                  <Button onClick={handleBatchVerify} size="sm" variant="outline">
                    <CheckCircle2 className="h-4 w-4 mr-2" />
                    批量验活
                  </Button>
                  <Button onClick={() => setBatchEditDialogOpen(true)} size="sm" variant="outline">
                    <SlidersHorizontal className="h-4 w-4 mr-2" />
                    批量修改
                  </Button>
                  <Button
                    onClick={handleBatchResetPriority}
                    size="sm"
                    variant="outline"
                    disabled={batchUpdateCredentials.isPending || selectedPriorityOverrideCount === 0}
                  >
                    <RotateCcw className="h-4 w-4 mr-2" />
                    重置优先级 ({selectedPriorityOverrideCount})
                  </Button>
                  <Button
                    onClick={handleBatchClearConcurrency}
                    size="sm"
                    variant="outline"
                    disabled={batchUpdateCredentials.isPending || selectedConcurrencyOverrideCount === 0}
                  >
                    <RotateCcw className="h-4 w-4 mr-2" />
                    清除并发 ({selectedConcurrencyOverrideCount})
                  </Button>
                  <Button
                    onClick={handleBatchClearRpm}
                    size="sm"
                    variant="outline"
                    disabled={batchUpdateCredentials.isPending || selectedRpmOverrideCount === 0}
                  >
                    <RotateCcw className="h-4 w-4 mr-2" />
                    清除 RPM ({selectedRpmOverrideCount})
                  </Button>
                  <Button
                    onClick={handleBatchForceRefresh}
                    size="sm"
                    variant="outline"
                    disabled={batchRefreshing}
                  >
                    <RefreshCw className={`h-4 w-4 mr-2 ${batchRefreshing ? 'animate-spin' : ''}`} />
                    {batchRefreshing ? `刷新中... ${batchRefreshProgress.current}/${batchRefreshProgress.total}` : '批量刷新 Token'}
                  </Button>
                  <Button
                    onClick={handleBatchQuerySelectedInfo}
                    size="sm"
                    variant="outline"
                    disabled={queryingInfo}
                  >
                    <RefreshCw className={`h-4 w-4 mr-2 ${queryingInfo ? 'animate-spin' : ''}`} />
                    查询信息
                  </Button>
                  <Button onClick={handleBatchResetFailure} size="sm" variant="outline">
                    <RotateCcw className="h-4 w-4 mr-2" />
                    恢复异常
                  </Button>
                  <Button
                    onClick={handleBatchDelete}
                    size="sm"
                    variant="destructive"
                    disabled={selectedDisabledCount === 0}
                    title={selectedDisabledCount === 0 ? '只能删除已禁用凭据' : undefined}
                  >
                    <Trash2 className="h-4 w-4 mr-2" />
                    批量删除
                  </Button>
                </>
              )}
              {verifying && !verifyDialogOpen && (
                <Button onClick={() => setVerifyDialogOpen(true)} size="sm" variant="secondary">
                  <CheckCircle2 className="h-4 w-4 mr-2 animate-spin" />
                  验活中... {verifyProgress.current}/{verifyProgress.total}
                </Button>
              )}
              {currentCredentials.length > 0 && (
                <Button
                  onClick={() => handleQueryCurrentPageInfo(false)}
                  size="sm"
                  variant="outline"
                  disabled={queryingInfo}
                >
                  <RefreshCw className={`h-4 w-4 mr-2 ${queryingInfo ? 'animate-spin' : ''}`} />
                  {queryingInfo ? '查询中...' : '查询本页信息'}
                </Button>
              )}
              {(summaryData?.available || 0) > 0 && (
                <Button
                  onClick={() => handleQueryCurrentPageInfo(true)}
                  size="sm"
                  variant="outline"
                  disabled={queryingInfo}
                >
                  <RefreshCw className={`h-4 w-4 mr-2 ${queryingInfo ? 'animate-spin' : ''}`} />
                  查询启用信息
                </Button>
              )}
              {(data?.total || 0) > 0 && (
                <Button
                  onClick={handleClearAll}
                  size="sm"
                  variant="outline"
                  className="text-destructive hover:text-destructive"
                  disabled={disabledCredentialCount === 0 || deleteDisabled.isPending}
                  title={disabledCredentialCount === 0 ? '没有可清除的已禁用凭据' : undefined}
                >
                  <Trash2 className={`h-4 w-4 mr-2 ${deleteDisabled.isPending ? 'animate-pulse' : ''}`} />
                  {deleteDisabled.isPending ? '清除中...' : '清除已禁用'}
                </Button>
              )}
              <Button onClick={() => setKamImportDialogOpen(true)} size="sm" variant="outline">
                <FileUp className="h-4 w-4 mr-2" />
                Kiro Account Manager 导入
              </Button>
              <Button onClick={() => setBatchImportDialogOpen(true)} size="sm" variant="outline">
                <Upload className="h-4 w-4 mr-2" />
                批量导入
              </Button>
              <Button onClick={() => setExportDialogOpen(true)} size="sm" variant="outline">
                <Download className="h-4 w-4 mr-2" />
                导出
              </Button>
              <Button onClick={() => setAddDialogOpen(true)} size="sm">
                <Plus className="h-4 w-4 mr-2" />
                添加凭据
              </Button>
            </div>
          </div>
          <div className="grid gap-2 md:grid-cols-2 xl:grid-cols-8">
            <div className="relative md:col-span-2">
              <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                className="pl-9"
                value={queryText}
                onChange={event => setQueryText(event.target.value)}
                placeholder="模糊搜索：订阅、代理、错误、priority:0、rpm:60..."
              />
            </div>
            <Input
              value={credentialIdQuery}
              onChange={event => setCredentialIdQuery(event.target.value)}
              placeholder="ID，如 #473"
              inputMode="numeric"
            />
            <Input
              value={accountQuery}
              onChange={event => setAccountQuery(event.target.value)}
              placeholder="邮箱 / Key"
            />
            <Input
              value={regionQuery}
              onChange={event => setRegionQuery(event.target.value)}
              placeholder="Region"
            />
            <Input
              value={modelQuery}
              onChange={event => setModelQuery(event.target.value)}
              placeholder="可用模型"
            />
            <Input
              value={endpointQuery}
              onChange={event => setEndpointQuery(event.target.value)}
              placeholder="Endpoint"
            />
            <select
              className="h-10 rounded-md border bg-background px-3 text-sm"
              value={sortBy}
              onChange={event => setSortBy(event.target.value as CredentialSortBy)}
              title="排序字段"
            >
              {credentialSortOptions.map(option => (
                <option key={option.value} value={option.value}>{option.label}</option>
              ))}
            </select>
            <select
              className="h-10 rounded-md border bg-background px-3 text-sm disabled:opacity-60"
              value={sortOrder}
              disabled={sortBy === 'default'}
              onChange={event => setSortOrder(event.target.value as CredentialSortOrder)}
              title="排序方向"
            >
              <option value="desc">降序</option>
              <option value="asc">升序</option>
            </select>
            <Input
              value={priorityQuery}
              onChange={event => setPriorityQuery(event.target.value)}
              placeholder="优先级 = 0"
              inputMode="numeric"
            />
            <Input
              value={rpmQuery}
              onChange={event => setRpmQuery(event.target.value)}
              placeholder="RPM = 60"
              inputMode="numeric"
            />
            <Input
              value={concurrencyQuery}
              onChange={event => setConcurrencyQuery(event.target.value)}
              placeholder="并发 = 3"
              inputMode="numeric"
            />
            <select
              className="h-10 rounded-md border bg-background px-3 text-sm"
              value={statusFilter}
              onChange={event => setStatusFilter(event.target.value)}
            >
              <option value="all">全部状态</option>
              <option value="enabled">启用</option>
              <option value="disabled">已禁用</option>
              <option value="current">当前活跃</option>
              <option value="cooldown">冷却中</option>
              <option value="rate_limited">限流中</option>
              <option value="proxy_blocked">代理不可用</option>
              <option value="custom_scheduling">有调度覆盖</option>
              <option value="custom_priority">自定义优先级</option>
              <option value="custom_concurrency">自定义并发</option>
              <option value="custom_rpm">自定义 RPM</option>
              <option value="error">有错误</option>
              <option value="unknown_subscription">未知订阅</option>
            </select>
            <select
              className="h-10 rounded-md border bg-background px-3 text-sm"
              value={authFilter}
              onChange={event => setAuthFilter(event.target.value)}
            >
              <option value="all">全部认证</option>
              <option value="social">Social</option>
              <option value="idc">IdC</option>
              <option value="external_idp">External IdP</option>
              <option value="api_key">API Key</option>
            </select>
            <select
              className="h-10 rounded-md border bg-background px-3 text-sm"
              value={subscriptionFilter}
              onChange={event => setSubscriptionFilter(event.target.value)}
            >
              <option value="all">全部订阅</option>
              <option value="pro_plus">Pro+</option>
              <option value="pro">Pro</option>
              <option value="trial">试用</option>
              <option value="free">Free</option>
              <option value="unknown">未知</option>
            </select>
            <select
              className="h-10 rounded-md border bg-background px-3 text-sm"
              value={proxyFilter}
              onChange={event => setProxyFilter(event.target.value)}
            >
              <option value="all">全部代理</option>
              {(proxyResourcesData?.resources || []).map(resource => (
                <option key={resource.id} value={resource.id}>
                  {resource.enabled ? '' : '已禁用 · '}{resource.name}
                </option>
              ))}
            </select>
            <Button
              size="sm"
              variant="outline"
              disabled={!hasCredentialFilters}
              onClick={clearCredentialFilters}
            >
              <X className="h-4 w-4 mr-2" />
              清除筛选
            </Button>
          </div>
          {currentCredentials.length === 0 ? (
            <Card>
              <CardContent className="py-8 text-center text-muted-foreground">
                {((data?.filteredTotal ?? data?.total) || 0) === 0 ? '暂无匹配凭据' : '当前页暂无凭据'}
              </CardContent>
            </Card>
          ) : (
            <>
              <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                {currentCredentials.map((credential) => (
                  <CredentialCard
                    key={credential.id}
                    credential={credential}
                    onQueryBalance={handleQueryCredentialBalance}
                    onTestCredential={handleTestCredential}
                    selected={selectedIds.has(credential.id)}
                    onToggleSelect={() => toggleSelect(credential.id)}
                    balance={balanceMap.get(credential.id) || null}
                    loadingBalance={loadingBalanceIds.has(credential.id)}
                  />
                ))}
              </div>

              {/* 分页控件 */}
              {totalPages > 1 && (
                <div className="flex justify-center items-center gap-4 mt-6">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                    disabled={currentPage === 1 || pageTransitionPending}
                  >
                    上一页
                  </Button>
                  <span className="text-sm text-muted-foreground">
                    第 {currentPage} / {totalPages} 页（共 {data?.filteredTotal ?? data?.total ?? 0} 个匹配凭据）
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                    disabled={currentPage === totalPages || pageTransitionPending}
                  >
                    下一页
                  </Button>
                </div>
              )}
            </>
          )}
        </div>
          </>
        )}
      </main>

      {/* 添加凭据对话框 */}
      <AddCredentialDialog
        open={addDialogOpen}
        onOpenChange={setAddDialogOpen}
      />

      {/* 测试账号连接对话框 */}
      <CredentialTestDialog
        credential={testingCredential}
        open={testDialogOpen}
        onOpenChange={setTestDialogOpen}
      />

      {/* 批量导入对话框 */}
      <BatchImportDialog
        open={batchImportDialogOpen}
        onOpenChange={setBatchImportDialogOpen}
      />

      <BatchEditCredentialsDialog
        open={batchEditDialogOpen}
        ids={Array.from(selectedIds)}
        onOpenChange={setBatchEditDialogOpen}
        onDone={() => {
          deselectAll()
          refetch()
        }}
      />

      {/* KAM 账号导入对话框 */}
      <KamImportDialog
        open={kamImportDialogOpen}
        onOpenChange={setKamImportDialogOpen}
      />

      <CredentialExportDialog
        open={exportDialogOpen}
        onOpenChange={setExportDialogOpen}
        selectedIds={Array.from(selectedIds)}
      />

      {/* 批量验活对话框 */}
      <BatchVerifyDialog
        open={verifyDialogOpen}
        onOpenChange={setVerifyDialogOpen}
        verifying={verifying}
        progress={verifyProgress}
        results={verifyResults}
        onCancel={handleCancelVerify}
      />

      {/* 积分详情弹层 */}
      <Dialog open={creditDetailDialogOpen} onOpenChange={setCreditDetailDialogOpen}>
        <DialogContent className="max-w-6xl max-h-[80vh]">
          <DialogHeader>
            <DialogTitle>所有账号积分详情</DialogTitle>
          </DialogHeader>

          {creditDetailsLoading ? (
            <div className="py-10 text-center text-sm text-muted-foreground">加载所有账号积分明细...</div>
          ) : creditDetailRows.length === 0 ? (
            <div className="py-10 text-center text-sm text-muted-foreground">暂无积分明细</div>
          ) : (
            <>
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-4">
                <Card>
                  <CardContent className="pt-4">
                    <div className="text-sm text-muted-foreground">可用剩余积分</div>
                    <div className="mt-1 text-xl font-bold">{formatCredits(refinedCreditStats.enabledCreditRemaining)}</div>
                    <div className="mt-1 text-xs text-muted-foreground">仅启用账号</div>
                  </CardContent>
                </Card>
                <Card>
                  <CardContent className="pt-4">
                    <div className="text-sm text-muted-foreground">总购买额度</div>
                    <div className="mt-1 text-xl font-bold">{formatCredits(refinedCreditStats.totalCreditLimit)}</div>
                    <div className="mt-1 text-xs text-muted-foreground">所有账号</div>
                  </CardContent>
                </Card>
                <Card>
                  <CardContent className="pt-4">
                    <div className="text-sm text-muted-foreground">已记录消耗</div>
                    <div className="mt-1 text-xl font-bold">{formatUsdFixed2(refinedCreditStats.totalEstimatedCostUsd)}</div>
                    <div className="mt-1 text-xs text-muted-foreground">所有账号</div>
                  </CardContent>
                </Card>
                <Card>
                  <CardContent className="pt-4">
                    <div className="text-sm text-muted-foreground">原始消耗</div>
                    <div className="mt-1 text-xl font-bold">{formatUsdFixed2(refinedCreditStats.totalOriginalCostUsd)}</div>
                    <div className="mt-1 text-xs text-muted-foreground">所有账号</div>
                  </CardContent>
                </Card>
              </div>

              <ScrollArea className="h-[400px]">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead className="w-16">序号</TableHead>
                      <TableHead className="w-20">ID</TableHead>
                      <TableHead>账号邮箱</TableHead>
                      <TableHead className="text-right">剩余积分</TableHead>
                      <TableHead className="text-right">总积分</TableHead>
                      <TableHead className="text-right">已记录消耗</TableHead>
                      <TableHead className="text-right">原始消耗</TableHead>
                      <TableHead>订阅类型</TableHead>
                      <TableHead>状态</TableHead>
                      <TableHead>最近查询</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {orderedCreditDetailRows.map((row, index) => (
                      <TableRow key={row.id}>
                        <TableCell className="font-medium">{index + 1}</TableCell>
                        <TableCell className="font-mono text-xs">#{row.id}</TableCell>
                        <TableCell className="font-mono text-sm">{row.email || '-'}</TableCell>
                        <TableCell className="text-right font-mono">{formatCredits(row.creditRemaining)}</TableCell>
                        <TableCell className="text-right font-mono">{formatCredits(row.creditLimit)}</TableCell>
                        <TableCell className="text-right font-mono">{formatUsdFixed2(row.estimatedCostUsd)}</TableCell>
                        <TableCell className="text-right font-mono">{formatUsdFixed2(row.originalCostUsd)}</TableCell>
                        <TableCell><Badge variant="outline">{row.subscriptionTitle || '-'}</Badge></TableCell>
                        <TableCell>
                          <Badge variant={row.disabled ? 'destructive' : 'default'}>
                            {row.disabled ? '已禁用' : '启用'}
                          </Badge>
                        </TableCell>
                        <TableCell className="text-xs text-muted-foreground">{formatDateTime(row.checkedAt)}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </ScrollArea>
            </>
          )}
        </DialogContent>
      </Dialog>
    </div>
  )
}
