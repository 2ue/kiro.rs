import { useState, useEffect, useMemo, useRef } from 'react'
import { LogOut, Moon, Sun, Server, Plus, Upload, FileUp, Trash2, RotateCcw, CheckCircle2, BarChart3, Settings, DollarSign, Download, FileClock, RefreshCw, Router } from 'lucide-react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { storage } from '@/lib/storage'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { CredentialCard } from '@/components/credential-card'
import { AddCredentialDialog } from '@/components/add-credential-dialog'
import { BatchImportDialog } from '@/components/batch-import-dialog'
import { KamImportDialog } from '@/components/kam-import-dialog'
import { BatchVerifyDialog, type VerifyResult } from '@/components/batch-verify-dialog'
import { CredentialTestDialog } from '@/components/credential-test-dialog'
import { UsageRecordsPanel } from '@/components/usage-records-panel'
import { RuntimeConfigPanel } from '@/components/runtime-config-panel'
import { ModelPricingPanel } from '@/components/model-pricing-panel'
import { AuditLogsPanel } from '@/components/audit-logs-panel'
import { CredentialExportDialog } from '@/components/credential-export-dialog'
import { ProxyResourcesPanel } from '@/components/proxy-resources-panel'
import { useCredentialsPage, useDeleteCredential, useResetFailure, useLoadBalancingMode, useSetLoadBalancingMode } from '@/hooks/use-credentials'
import { getCredentialBalance, forceRefreshToken, getCredentials, testCredential } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, testModelLabel } from '@/lib/test-models'
import type { BalanceResponse, CredentialStatusItem, LoadBalancingMode } from '@/types/api'

interface DashboardProps {
  onLogout: () => void
}

export function Dashboard({ onLogout }: DashboardProps) {
  const [testingCredential, setTestingCredential] = useState<CredentialStatusItem | null>(null)
  const [testDialogOpen, setTestDialogOpen] = useState(false)
  const [addDialogOpen, setAddDialogOpen] = useState(false)
  const [batchImportDialogOpen, setBatchImportDialogOpen] = useState(false)
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
  const [queryInfoProgress, setQueryInfoProgress] = useState({ current: 0, total: 0 })
  const [batchRefreshing, setBatchRefreshing] = useState(false)
  const [batchRefreshProgress, setBatchRefreshProgress] = useState({ current: 0, total: 0 })
  const [activeTab, setActiveTab] = useState<'credentials' | 'proxies' | 'usage' | 'pricing' | 'audit' | 'config'>('credentials')
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
  const { data, isLoading, error, refetch } = useCredentialsPage({
    page: currentPage,
    limit: itemsPerPage,
  })
  const { mutate: deleteCredential } = useDeleteCredential()
  const { mutate: resetFailure } = useResetFailure()
  const { data: loadBalancingData, isLoading: isLoadingMode } = useLoadBalancingMode()
  const { mutate: setLoadBalancingMode, isPending: isSettingMode } = useSetLoadBalancingMode()

  // 计算分页
  const totalPages = data?.totalPages || 0
  const currentCredentials = useMemo(() => data?.credentials || [], [data?.credentials])
  const disabledCredentialCount = Math.max((data?.total || 0) - (data?.available || 0), 0)
  const selectedDisabledCount = Array.from(selectedIds).filter(id => {
    const credential = currentCredentials.find(c => c.id === id)
    return Boolean(credential?.disabled)
  }).length

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

  // 只保留当前仍存在的凭据缓存，避免删除后残留旧数据
  useEffect(() => {
    if (!data?.credentials) {
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
  }, [currentCredentials, data?.credentials])

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
      return cred && !cred.disabled && cred.authMethod !== 'api_key'
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
    const skippedText = skippedCount > 0 ? `，跳过 ${skippedCount} 个禁用或 API Key 凭据` : ''

    if (failCount === 0) {
      toast.success(`成功刷新 ${successCount} 个凭据的 Token${skippedText}`)
    } else {
      toast.warning(`刷新 Token：成功 ${successCount} 个，失败 ${failCount} 个${skippedText}`)
    }

    deselectAll()
  }

  // 一键清除所有已禁用凭据
  const handleClearAll = async () => {
    if (!data || data.total === 0) {
      toast.error('没有可清除的凭据')
      return
    }

    let allCredentials
    try {
      allCredentials = await getCredentials()
    } catch (error) {
      toast.error(`加载凭据失败: ${extractErrorMessage(error)}`)
      return
    }

    const disabledCredentials = allCredentials.credentials.filter(credential => credential.disabled)

    if (disabledCredentials.length === 0) {
      toast.error('没有可清除的已禁用凭据')
      return
    }

    if (!confirm(`确定要清除所有 ${disabledCredentials.length} 个已禁用凭据吗？此操作无法撤销。`)) {
      return
    }

    let successCount = 0
    let failCount = 0

    for (const credential of disabledCredentials) {
      try {
        await new Promise<void>((resolve, reject) => {
          deleteCredential(credential.id, {
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
      toast.success(`成功清除所有 ${successCount} 个已禁用凭据`)
    } else {
      toast.warning(`清除已禁用凭据：成功 ${successCount} 个，失败 ${failCount} 个`)
    }

    deselectAll()
  }

  const fetchBalanceForCredential = async (id: number) => {
    setLoadingBalanceIds(prev => {
      const next = new Set(prev)
      next.add(id)
      return next
    })

    try {
      const balance = await getCredentialBalance(id)
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
      toast.success(`凭据 #${id} 额度已更新`)
    } else {
      toast.error(`查询额度失败: ${extractErrorMessage(result.error)}`)
    }
  }

  // 查询当前页额度（逐个查询，避免瞬时并发）
  const handleQueryCurrentPageInfo = async () => {
    if (currentCredentials.length === 0) {
      toast.error('当前页没有可查询的凭据')
      return
    }

    const ids = currentCredentials
      .filter(credential => !credential.disabled)
      .map(credential => credential.id)

    if (ids.length === 0) {
      toast.error('当前页没有可查询额度的启用凭据')
      return
    }

    setQueryingInfo(true)
    setQueryInfoProgress({ current: 0, total: ids.length })

    let successCount = 0
    let failCount = 0

    for (let i = 0; i < ids.length; i++) {
      const id = ids[i]

      const result = await fetchBalanceForCredential(id)
      if (result.ok) {
        successCount++
      } else {
        failCount++
      }

      setQueryInfoProgress({ current: i + 1, total: ids.length })
    }

    setQueryingInfo(false)
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    queryClient.invalidateQueries({ queryKey: ['credentials-page'] })

    if (failCount === 0) {
      toast.success(`查询完成：成功 ${successCount}/${ids.length}`)
    } else {
      toast.warning(`查询完成：成功 ${successCount} 个，失败 ${failCount} 个`)
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
            <span className="font-semibold">Kiro Admin</span>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant={activeTab === 'credentials' ? 'secondary' : 'ghost'}
              size="sm"
              onClick={() => setActiveTab('credentials')}
            >
              <Server className="h-4 w-4" />
              凭据
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
        {activeTab === 'usage' ? (
          <UsageRecordsPanel />
        ) : activeTab === 'proxies' ? (
          <ProxyResourcesPanel />
        ) : activeTab === 'pricing' ? (
          <ModelPricingPanel />
        ) : activeTab === 'audit' ? (
          <AuditLogsPanel />
        ) : activeTab === 'config' ? (
          <RuntimeConfigPanel />
        ) : (
          <>
        {/* 统计卡片 */}
        <div className="grid gap-4 md:grid-cols-4 mb-6">
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
              <div className="text-xs text-muted-foreground">排队 {data?.queuedRequests || 0}/{data?.maxQueuedRequests || '不限'}</div>
            </CardContent>
          </Card>
        </div>

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
                  <Button
                    onClick={handleBatchForceRefresh}
                    size="sm"
                    variant="outline"
                    disabled={batchRefreshing}
                  >
                    <RefreshCw className={`h-4 w-4 mr-2 ${batchRefreshing ? 'animate-spin' : ''}`} />
                    {batchRefreshing ? `刷新中... ${batchRefreshProgress.current}/${batchRefreshProgress.total}` : '批量刷新 Token'}
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
                  onClick={handleQueryCurrentPageInfo}
                  size="sm"
                  variant="outline"
                  disabled={queryingInfo}
                >
                  <RefreshCw className={`h-4 w-4 mr-2 ${queryingInfo ? 'animate-spin' : ''}`} />
                  {queryingInfo ? `查询中... ${queryInfoProgress.current}/${queryInfoProgress.total}` : '查询本页额度'}
                </Button>
              )}
              {(data?.total || 0) > 0 && (
                <Button
                  onClick={handleClearAll}
                  size="sm"
                  variant="outline"
                  className="text-destructive hover:text-destructive"
                  disabled={disabledCredentialCount === 0}
                  title={disabledCredentialCount === 0 ? '没有可清除的已禁用凭据' : undefined}
                >
                  <Trash2 className="h-4 w-4 mr-2" />
                  清除已禁用
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
          {currentCredentials.length === 0 ? (
            <Card>
              <CardContent className="py-8 text-center text-muted-foreground">
                {(data?.total || 0) === 0 ? '暂无凭据' : '当前页暂无凭据'}
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
                    disabled={currentPage === 1}
                  >
                    上一页
                  </Button>
                  <span className="text-sm text-muted-foreground">
                    第 {currentPage} / {totalPages} 页（共 {data?.total || 0} 个凭据）
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                    disabled={currentPage === totalPages}
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

      {/* KAM 账号导入对话框 */}
      <KamImportDialog
        open={kamImportDialogOpen}
        onOpenChange={setKamImportDialogOpen}
      />

      <CredentialExportDialog
        open={exportDialogOpen}
        onOpenChange={setExportDialogOpen}
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
    </div>
  )
}
