import * as React from 'react'
import { Activity, ArrowRightLeft, CheckCircle2, Edit3, Eye, EyeOff, Network, Plus, RefreshCw, Trash2, Unlink, UserX } from 'lucide-react'
import { toast } from 'sonner'
import { getCredentialsMatching } from '@/api/credentials'
import {
  useBatchUpdateCredentials,
  useDeleteProxyResource,
  useProxyResources,
  useTestAllProxyResources,
  useTestProxyResource,
} from '@/hooks/use-credentials'
import { formatDate } from '@/lib/format'
import { extractErrorMessage, cn } from '@/lib/utils'
import type { CredentialStatusItem, ProxyResource, ProxyResourceTestResponse } from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  EmptyState,
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  Pagination,
  useConfirm,
} from '@/components/patterns'
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
import { ProxyEditorModal, ProxyImportModal } from './proxy-components'

const PAGE_SIZE_OPTIONS = [10, 20, 50, 100]
const DEFAULT_PAGE_SIZE = 20
const DIRECT_PROXY_VALUE = '__direct__'
type ReplaceProxyMode = 'single' | 'individual'
/** The row button whose account lookup is running. */
type RowAction = 'replace' | 'clear' | 'delete'

const rowKey = (id: number, action: RowAction) => `proxy-${id}-${action}`

function maskSecret(value?: string | null): string {
  if (!value) return '-'
  return '*'.repeat(Math.min(Math.max(value.length, 6), 16))
}

function credentialLabel(credential: CredentialStatusItem): string {
  return credential.email || credential.maskedApiKey || `账号 #${credential.id}`
}

function latencyTone(result: ProxyResourceTestResponse): 'success' | 'info' | 'warning' | 'error' {
  if (!result.success) return 'error'
  if (result.durationMs < 200) return 'success'
  if (result.durationMs <= 500) return 'info'
  if (result.durationMs <= 1000) return 'warning'
  return 'error'
}

/** The last test the server recorded, shown until this page runs a test of its own. */
function storedResult(resource: ProxyResource): ProxyResourceTestResponse | undefined {
  if (resource.lastTestSuccess === null || resource.lastTestSuccess === undefined) return undefined
  return {
    success: resource.lastTestSuccess,
    message: resource.lastTestMessage ?? '',
    proxyUrl: resource.proxyUrl,
    testUrl: resource.lastTestUrl ?? '',
    status: resource.lastTestStatus ?? null,
    durationMs: resource.lastTestDurationMs ?? 0,
  }
}

function latencyLabel(result: ProxyResourceTestResponse): string {
  if (!result.success) return result.status ? `失败 · HTTP ${result.status}` : '失败'
  return `${result.durationMs}ms`
}

export function ProxiesPage() {
  const resources = useProxyResources()
  const removeProxy = useDeleteProxyResource()
  const testProxy = useTestProxyResource()
  const testAll = useTestAllProxyResources()
  const batchUpdateCredentials = useBatchUpdateCredentials()
  const confirm = useConfirm()

  const [page, setPage] = React.useState(1)
  const [pageSize, setPageSize] = React.useState(DEFAULT_PAGE_SIZE)
  const [selectedIds, setSelectedIds] = React.useState<Set<number>>(new Set())
  const [batchDeleteOpen, setBatchDeleteOpen] = React.useState(false)
  const [batchDeleteBound, setBatchDeleteBound] = React.useState<CredentialStatusItem[]>([])
  const [batchReplacement, setBatchReplacement] = React.useState(DIRECT_PROXY_VALUE)
  const [batchDeleting, setBatchDeleting] = React.useState(false)
  const [showSecrets, setShowSecrets] = React.useState(false)
  const [editorOpen, setEditorOpen] = React.useState(false)
  const [editing, setEditing] = React.useState<ProxyResource | null>(null)
  const [importOpen, setImportOpen] = React.useState(false)
  const [testingIds, setTestingIds] = React.useState<Set<number>>(new Set())
  const [testResults, setTestResults] = React.useState<Map<number, ProxyResourceTestResponse>>(new Map())
  const [deleteCandidate, setDeleteCandidate] = React.useState<ProxyResource | null>(null)
  const [deleteBoundCredentials, setDeleteBoundCredentials] = React.useState<CredentialStatusItem[]>([])
  const [replacementProxy, setReplacementProxy] = React.useState(DIRECT_PROXY_VALUE)
  const [replaceCandidate, setReplaceCandidate] = React.useState<ProxyResource | null>(null)
  const [replaceBoundCredentials, setReplaceBoundCredentials] = React.useState<CredentialStatusItem[]>([])
  const [replaceSelectedIds, setReplaceSelectedIds] = React.useState<Set<number>>(new Set())
  const [replaceTargetProxy, setReplaceTargetProxy] = React.useState('')
  const [replaceMode, setReplaceMode] = React.useState<ReplaceProxyMode>('single')
  const [replaceTargetsByCredential, setReplaceTargetsByCredential] = React.useState<Map<number, string>>(new Map())
  // What is loading which accounts right now: a proxy row's action, the batch delete, or the
  // disabled-account unbind. Each shows its own spinner instead of sharing one.
  const [loadingBindings, setLoadingBindings] = React.useState<Set<string>>(new Set())
  const [deletingId, setDeletingId] = React.useState<number | null>(null)
  const [unbindingDisabled, setUnbindingDisabled] = React.useState(false)

  const list = resources.data?.resources || []
  const pageCount = Math.max(1, Math.ceil(list.length / pageSize))
  const pagedList = React.useMemo(
    () => list.slice((page - 1) * pageSize, page * pageSize),
    [list, page, pageSize],
  )
  const liveCount = list.filter((resource) => (testResults.get(resource.id) ?? storedResult(resource))?.success).length
  const selectedResources = React.useMemo(() => list.filter((resource) => selectedIds.has(resource.id)), [list, selectedIds])
  const allPageSelected = pagedList.length > 0 && pagedList.every((resource) => selectedIds.has(resource.id))
  const somePageSelected = pagedList.some((resource) => selectedIds.has(resource.id))
  const batchEnabledBound = batchDeleteBound.filter((credential) => !credential.disabled)
  const batchReplacementOptions = list.filter((resource) => resource.enabled && !selectedIds.has(resource.id))
  const deleteCandidateEnabledCredentials = deleteBoundCredentials.filter((credential) => !credential.disabled)
  const replacementOptions = list.filter((resource) => resource.id !== deleteCandidate?.id && resource.enabled)
  const replaceOptions = list.filter((resource) => resource.id !== replaceCandidate?.id && resource.enabled)
  const replaceSelectedCredentials = replaceBoundCredentials.filter((credential) => replaceSelectedIds.has(credential.id))
  const allReplaceSelected = replaceBoundCredentials.length > 0 && replaceBoundCredentials.every((credential) => replaceSelectedIds.has(credential.id))
  const replaceReady = replaceMode === 'single'
    ? Boolean(replaceTargetProxy)
    : replaceSelectedCredentials.every((credential) => Boolean(replaceTargetsByCredential.get(credential.id)))

  React.useEffect(() => {
    if (page > pageCount) setPage(pageCount)
  }, [page, pageCount])

  // Drop selections for proxies that no longer exist.
  React.useEffect(() => {
    const validIds = new Set(list.map((resource) => resource.id))
    setSelectedIds((prev) => {
      const next = new Set([...prev].filter((id) => validIds.has(id)))
      return next.size === prev.size ? prev : next
    })
  }, [list])

  const toggleSelected = (id: number) => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const togglePageSelected = () => {
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (allPageSelected) pagedList.forEach((resource) => next.delete(resource.id))
      else pagedList.forEach((resource) => next.add(resource.id))
      return next
    })
  }

  /** Selects every proxy the last test found unreachable, across all pages. */
  const selectFailed = () => {
    const failed = list.filter((resource) => (testResults.get(resource.id) ?? storedResult(resource))?.success === false)
    if (failed.length === 0) { toast.info('没有测活失败的代理'); return }
    setSelectedIds(new Set(failed.map((resource) => resource.id)))
    toast.success(`已选中 ${failed.length} 个测活失败的代理`)
  }

  const requestBatchDelete = async () => {
    if (selectedResources.length === 0) return
    const ids = new Set(selectedResources.map((resource) => resource.id))
    // Only proxies that still have accounts need their bindings looked up.
    const withBindings = selectedResources.filter((resource) => resource.credentialCount > 0)
    let bound: CredentialStatusItem[] = []
    if (withBindings.length > 0) {
      const loaded = await withBindingsLoading('batch-delete', async () => {
        const lists = await Promise.all(withBindings.map((resource) => getCredentialsMatching({ proxyResourceId: resource.id })))
        return lists.flat()
      })
      if (!loaded) return
      bound = loaded
    }
    const alternatives = list.filter((resource) => resource.enabled && !ids.has(resource.id))
    setBatchDeleteBound(bound)
    setBatchReplacement(alternatives.length > 0 ? String(alternatives[0].id) : DIRECT_PROXY_VALUE)
    setBatchDeleteOpen(true)
  }

  /**
   * Moves enabled accounts off the selected proxies first (the server refuses to delete a proxy that
   * still serves an enabled account), then deletes them one by one. Disabled accounts' bindings are
   * cleared by the delete itself.
   */
  const confirmBatchDelete = async () => {
    setBatchDeleting(true)
    try {
      if (batchEnabledBound.length > 0) {
        const replacementId = batchReplacement === DIRECT_PROXY_VALUE ? null : Number(batchReplacement)
        const enabledIds = batchEnabledBound.map((credential) => credential.id)
        for (let start = 0; start < enabledIds.length; start += 500) {
          const result = await batchUpdateCredentials.mutateAsync({
            ids: enabledIds.slice(start, start + 500),
            proxy: { proxyResourceId: replacementId },
          })
          if (result.failed > 0) {
            toast.warning(`改绑未全部成功：成功 ${result.success}，失败 ${result.failed}，已取消删除`)
            return
          }
        }
      }
      let deleted = 0
      const failures: string[] = []
      for (const resource of selectedResources) {
        try {
          await removeProxy.mutateAsync(resource.id)
          deleted += 1
        } catch (error) {
          failures.push(`${resource.name}: ${extractErrorMessage(error)}`)
        }
      }
      setSelectedIds(new Set())
      setBatchDeleteOpen(false)
      setBatchDeleteBound([])
      if (failures.length > 0) toast.warning(`已删除 ${deleted} 个，失败 ${failures.length} 个：${failures.slice(0, 2).join('；')}`)
      else toast.success(`已删除 ${deleted} 个代理${batchEnabledBound.length > 0 ? `，${batchEnabledBound.length} 个启用账号已改绑` : ''}`)
    } catch (error) {
      toast.error(`批量删除失败: ${extractErrorMessage(error)}`)
    } finally {
      setBatchDeleting(false)
    }
  }

  const openCreate = () => {
    setEditing(null)
    setEditorOpen(true)
  }
  const openEdit = (resource: ProxyResource) => {
    setEditing(resource)
    setEditorOpen(true)
  }
  const closeEditor = () => {
    setEditorOpen(false)
    setEditing(null)
  }

  const handlePageSizeChange = (nextPageSize: number) => {
    setPageSize(nextPageSize)
    setPage(1)
  }

  /**
   * Runs one account lookup under its own spinner key; on failure shows the error and returns
   * undefined. Lookups go through the server-side filter, so the page never downloads every account.
   */
  const withBindingsLoading = async <T,>(key: string, load: () => Promise<T>): Promise<T | undefined> => {
    setLoadingBindings((prev) => new Set(prev).add(key))
    try {
      return await load()
    } catch (error) {
      toast.error(`加载账号绑定失败: ${extractErrorMessage(error)}`)
      return undefined
    } finally {
      setLoadingBindings((prev) => {
        const next = new Set(prev)
        next.delete(key)
        return next
      })
    }
  }

  /** Accounts bound to one proxy; a proxy whose count is 0 has none, so nothing is fetched. */
  const loadBoundCredentials = (resource: ProxyResource, action: RowAction) => {
    if (resource.credentialCount === 0) return Promise.resolve<CredentialStatusItem[]>([])
    return withBindingsLoading(rowKey(resource.id, action), () => getCredentialsMatching({ proxyResourceId: resource.id }))
  }

  const testOne = async (resource: ProxyResource) => {
    setTestingIds((prev) => new Set(prev).add(resource.id))
    try {
      const result = await testProxy.mutateAsync({ id: resource.id, request: {} })
      setTestResults((prev) => new Map(prev).set(resource.id, result))
      if (result.success) toast.success(`代理「${resource.name}」测活通过`)
      else toast.error(`代理「${resource.name}」测活失败`)
    } catch (error) {
      setTestResults((prev) => new Map(prev).set(resource.id, {
        success: false,
        message: `测活请求失败: ${extractErrorMessage(error)}`,
        proxyUrl: resource.proxyUrl,
        testUrl: '',
        status: null,
        durationMs: 0,
      }))
      toast.error(`代理测试失败: ${extractErrorMessage(error)}`)
    } finally {
      setTestingIds((prev) => {
        const next = new Set(prev)
        next.delete(resource.id)
        return next
      })
    }
  }

  /** Tests every proxy at once on the server; nothing needs to be selected first. */
  const testAllProxies = async () => {
    if (!list.length) return toast.error('还没有代理资源')
    setTestingIds(new Set(list.map((resource) => resource.id)))
    try {
      const result = await testAll.mutateAsync({})
      setTestResults((prev) => {
        const next = new Map(prev)
        for (const item of result.items) next.set(item.resourceId, item)
        return next
      })
      if (result.failed > 0) toast.warning(`全部测活完成：通过 ${result.success}/${result.total}，${result.failed} 个不可用`)
      else toast.success(`全部测活完成：${result.total} 个代理全部可用`)
    } catch (error) {
      toast.error(`全部测活失败: ${extractErrorMessage(error)}`)
    } finally {
      setTestingIds(new Set())
    }
  }

  /** Disabled accounts keep their proxy binding by default; this frees those proxies for live accounts. */
  const unbindDisabledAccounts = async () => {
    const disabled = await withBindingsLoading('unbind-disabled', () => getCredentialsMatching({ status: 'disabled' }))
    if (!disabled) return
    const bound = disabled.filter((credential) => typeof credential.proxyResourceId === 'number')
    if (bound.length === 0) {
      toast.info('没有绑定代理资源的禁用账号')
      return
    }
    const proxyCount = new Set(bound.map((credential) => credential.proxyResourceId)).size
    const confirmed = await confirm({
      title: '解除禁用账号的代理',
      message: `将解除 ${bound.length} 个禁用账号与 ${proxyCount} 个代理资源的绑定。只清除绑定关系，不删除账号和代理；这些账号以后重新启用时会走全局代理或直连。`,
      confirmText: '解除绑定',
      tone: 'danger',
    })
    if (!confirmed) return
    // The batch endpoint takes a page of accounts at a time.
    const chunkSize = 500
    let success = 0
    let failed = 0
    setUnbindingDisabled(true)
    try {
      for (let start = 0; start < bound.length; start += chunkSize) {
        const result = await batchUpdateCredentials.mutateAsync({
          ids: bound.slice(start, start + chunkSize).map((credential) => credential.id),
          proxy: { proxyResourceId: null },
        })
        success += result.success
        failed += result.failed
      }
    } catch (error) {
      toast.error(`解除绑定失败（已处理 ${success} 个）: ${extractErrorMessage(error)}`)
      return
    } finally {
      setUnbindingDisabled(false)
    }
    if (failed > 0) toast.warning(`解除绑定未全部成功：成功 ${success}，失败 ${failed}`)
    else toast.success(`已解除 ${success} 个禁用账号的代理绑定`)
  }

  const handleImported = (items: Array<{ resourceId: number; result: ProxyResourceTestResponse }>) => {
    if (!items.length) return
    setTestResults((previous) => {
      const next = new Map(previous)
      items.forEach(({ resourceId, result }) => next.set(resourceId, result))
      return next
    })
  }

  const requestDelete = async (resource: ProxyResource) => {
    const bound = await loadBoundCredentials(resource, 'delete')
    if (!bound) return
    const enabledBound = bound.filter((credential) => !credential.disabled)
    const disabledBound = bound.length - enabledBound.length
    if (enabledBound.length > 0) {
      const alternatives = list.filter((candidate) => candidate.id !== resource.id && candidate.enabled)
      setDeleteBoundCredentials(bound)
      setDeleteCandidate(resource)
      setReplacementProxy(alternatives.length > 0 ? String(alternatives[0].id) : DIRECT_PROXY_VALUE)
      toast.warning(`代理仍绑定 ${enabledBound.length} 个启用账号，请先选择替换代理`)
      return
    }

    const confirmed = await confirm({
      title: '删除代理资源',
      message: disabledBound > 0
        ? `确认删除代理「${resource.name}」？将自动清除 ${disabledBound} 个已禁用账号的代理关联。`
        : `确认删除代理「${resource.name}」？`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!confirmed) return
    setDeletingId(resource.id)
    try {
      await removeProxy.mutateAsync(resource.id)
      toast.success(disabledBound > 0 ? `代理已删除，已清除 ${disabledBound} 个已禁用账号关联` : '代理资源已删除')
    } catch (error) {
      // The count on the page can lag behind: the server refuses when an enabled account was bound meanwhile.
      toast.error(`删除失败: ${extractErrorMessage(error)}`)
      void resources.refetch()
    } finally {
      setDeletingId(null)
    }
  }

  const requestReplaceBindings = async (resource: ProxyResource) => {
    const bound = await loadBoundCredentials(resource, 'replace')
    if (!bound) return
    if (bound.length === 0) {
      toast.info('当前代理没有绑定账号')
      return
    }
    const options = list.filter((candidate) => candidate.id !== resource.id && candidate.enabled)
    if (options.length === 0) {
      toast.error('没有可用的目标代理，请先新增或启用其他代理')
      return
    }
    setReplaceCandidate(resource)
    setReplaceBoundCredentials(bound)
    setReplaceSelectedIds(new Set(bound.map((credential) => credential.id)))
    const defaultTarget = String(options[0].id)
    setReplaceTargetProxy(defaultTarget)
    setReplaceMode('single')
    setReplaceTargetsByCredential(new Map(bound.map((credential) => [credential.id, defaultTarget])))
  }

  const toggleReplaceCredential = (id: number) => {
    setReplaceSelectedIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const toggleAllReplaceCredentials = () => {
    setReplaceSelectedIds((prev) => {
      if (allReplaceSelected) return new Set()
      const next = new Set(prev)
      replaceBoundCredentials.forEach((credential) => next.add(credential.id))
      return next
    })
  }

  const setReplaceCredentialTarget = (credentialId: number, proxyId: string) => {
    setReplaceTargetsByCredential((prev) => {
      const next = new Map(prev)
      next.set(credentialId, proxyId)
      return next
    })
  }

  const applyReplaceBindings = async () => {
    if (!replaceCandidate) return
    const ids = Array.from(replaceSelectedIds)
    if (ids.length === 0) {
      toast.error('请选择要更换代理的账号')
      return
    }
    const buildGroups = () => {
      if (replaceMode === 'single') {
        const targetId = Number(replaceTargetProxy)
        if (!Number.isFinite(targetId) || targetId <= 0) return null
        return new Map<number, number[]>([[targetId, ids]])
      }
      const groups = new Map<number, number[]>()
      for (const id of ids) {
        const targetValue = replaceTargetsByCredential.get(id)
        const targetId = Number(targetValue)
        if (!Number.isFinite(targetId) || targetId <= 0) return null
        const group = groups.get(targetId) || []
        group.push(id)
        groups.set(targetId, group)
      }
      return groups
    }
    const groups = buildGroups()
    if (!groups || groups.size === 0) {
      toast.error('请选择目标代理')
      return
    }
    try {
      let success = 0
      let failed = 0
      for (const [targetId, groupIds] of groups) {
        const result = await batchUpdateCredentials.mutateAsync({
          ids: groupIds,
          proxy: { proxyResourceId: targetId },
        })
        success += result.success
        failed += result.failed
      }
      if (failed > 0) {
        toast.warning(`更换代理未全部成功：成功 ${success}，失败 ${failed}`)
        return
      }
      toast.success(groups.size === 1
        ? `已更换 ${success} 个账号的代理`
        : `已按单独设置更换 ${success} 个账号的代理`)
      setReplaceCandidate(null)
      setReplaceBoundCredentials([])
      setReplaceSelectedIds(new Set())
      setReplaceTargetProxy('')
      setReplaceMode('single')
      setReplaceTargetsByCredential(new Map())
    } catch (error) {
      toast.error(`更换代理失败: ${extractErrorMessage(error)}`)
    }
  }

  const clearBindings = async (resource: ProxyResource) => {
    const bound = await loadBoundCredentials(resource, 'clear')
    if (!bound) return
    if (bound.length === 0) {
      toast.info('当前代理没有绑定账号')
      return
    }
    const confirmed = await confirm({
      title: '清除代理绑定',
      message: `确认清除代理「${resource.name}」绑定的 ${bound.length} 个账号？只会清除账号与代理资源的绑定关系，不会删除账号，也不会删除代理资源。`,
      confirmText: '清除绑定',
      tone: 'danger',
    })
    if (!confirmed) return
    try {
      const result = await batchUpdateCredentials.mutateAsync({
        ids: bound.map((credential) => credential.id),
        proxy: { proxyResourceId: null },
      })
      if (result.failed > 0) {
        toast.warning(`清除绑定未全部成功：成功 ${result.success}，失败 ${result.failed}`)
        return
      }
      toast.success(`已清除 ${result.success} 个账号的代理绑定`)
    } catch (error) {
      toast.error(`清除绑定失败: ${extractErrorMessage(error)}`)
    }
  }

  const replaceEnabledBindingsAndDelete = async () => {
    if (!deleteCandidate) return
    const enabledIds = deleteCandidateEnabledCredentials.map((credential) => credential.id)
    const replacementId = replacementProxy === DIRECT_PROXY_VALUE ? null : Number(replacementProxy)
    try {
      if (enabledIds.length > 0) {
        const result = await batchUpdateCredentials.mutateAsync({
          ids: enabledIds,
          proxy: { proxyResourceId: replacementId },
        })
        if (result.failed > 0) {
          toast.warning(`更换代理未全部成功：成功 ${result.success}，失败 ${result.failed}，已取消删除`)
          return
        }
      }
      await removeProxy.mutateAsync(deleteCandidate.id)
      toast.success(`已更换 ${enabledIds.length} 个启用账号的代理，并删除代理「${deleteCandidate.name}」`)
      setDeleteBoundCredentials([])
      setDeleteCandidate(null)
    } catch (error) {
      toast.error(`删除失败: ${extractErrorMessage(error)}`)
    }
  }

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.proxies.title}
        subtitle={pageMeta.proxies.subtitle}
        actions={
          <div className="flex flex-wrap items-center gap-1.5">
            <Button variant="outline" size="sm" onClick={() => resources.refetch()}>
              <RefreshCw className={cn('h-4 w-4', resources.isFetching && 'animate-spin')} />
            </Button>
            <Button variant="outline" size="sm" onClick={() => setShowSecrets((value) => !value)}>
              {showSecrets ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              <span className="hidden sm:inline">{showSecrets ? '隐藏密钥' : '显示密钥'}</span>
            </Button>
            <Button variant="outline" size="sm" onClick={() => void testAllProxies()} disabled={list.length === 0 || testingIds.size > 0} title="测试全部代理资源">
              {testAll.isPending ? <Spinner size="sm" /> : <CheckCircle2 className="h-4 w-4" />}
              全部测活
            </Button>
            <Button variant="outline" size="sm" onClick={() => void unbindDisabledAccounts()} disabled={unbindingDisabled || loadingBindings.has('unbind-disabled')} title="解除所有禁用账号与代理资源的绑定">
              {unbindingDisabled || loadingBindings.has('unbind-disabled') ? <Spinner size="sm" /> : <UserX className="h-4 w-4" />}
              解除禁用账号代理
            </Button>
            <Button size="sm" onClick={openCreate}>
              <Plus className="h-4 w-4" />新增代理
            </Button>
            <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
              批量导入
            </Button>
          </div>
        }
      />

      <div className="space-y-3">
        {resources.isLoading ? (
          <LoadingState text="加载代理资源..." />
        ) : resources.error ? (
          <ErrorState message={extractErrorMessage(resources.error)} />
        ) : list.length === 0 ? (
          <EmptyState
            icon={<Network className="h-12 w-12" />}
            title="暂无代理资源"
            description="点击右上角按钮创建第一个代理资源"
            action={
              <Button size="sm" onClick={openCreate}>
                <Plus className="h-4 w-4" />新增代理
              </Button>
            }
          />
        ) : (
          <div className="space-y-3">
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-muted/30 px-3 py-2">
              <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
                <label className="flex items-center gap-2">
                  <Checkbox
                    checked={allPageSelected ? true : somePageSelected ? 'indeterminate' : false}
                    onCheckedChange={togglePageSelected}
                    aria-label="全选当前页"
                  />
                  全选当前页
                </label>
                <span>共 {list.length} 个代理 · 最近测活可用 {liveCount} 个</span>
                <Button type="button" variant="ghost" size="xs" onClick={selectFailed}>选中测活失败的</Button>
              </div>
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                {resources.isFetching && <Spinner size="sm" />}
                {selectedIds.size > 0 && (
                  <>
                    <Badge tone="primary">已选 {selectedIds.size}</Badge>
                    <Button type="button" variant="ghost" size="xs" onClick={() => setSelectedIds(new Set())}>取消选择</Button>
                    <Button type="button" variant="destructive" size="xs" onClick={() => void requestBatchDelete()}
                      disabled={loadingBindings.has('batch-delete') || batchDeleting}>
                      {loadingBindings.has('batch-delete') ? <Spinner size="sm" /> : <Trash2 className="h-3.5 w-3.5" />}批量删除
                    </Button>
                  </>
                )}
              </div>
            </div>

            <div className="overflow-x-auto rounded-lg border border-border/60">
              <table className="w-full min-w-[980px] border-collapse text-left text-sm">
                <thead className="bg-muted/50 text-xs text-muted-foreground">
                  <tr>
                    <th className="w-10 px-3 py-2" />
                    <th className="px-3 py-2">代理</th>
                    <th className="px-3 py-2">认证</th>
                    <th className="px-3 py-2">绑定</th>
                    <th className="px-3 py-2">测活</th>
                    <th className="px-3 py-2">更新时间</th>
                    <th className="px-3 py-2 text-right">操作</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border/60">
                  {pagedList.map((resource) => {
                    const result = testResults.get(resource.id) ?? storedResult(resource)
                    const testing = testingIds.has(resource.id)
                    const rowAction = (['replace', 'clear', 'delete'] as const).find((action) => loadingBindings.has(rowKey(resource.id, action)))
                    const rowLoading = rowAction !== undefined
                    return (
                      <tr key={resource.id} className={cn('align-top', selectedIds.has(resource.id) ? 'bg-primary/5' : 'bg-card')}>
                        <td className="px-3 py-3">
                          <Checkbox checked={selectedIds.has(resource.id)} onCheckedChange={() => toggleSelected(resource.id)} aria-label={`选择代理 ${resource.name}`} />
                        </td>
                        <td className="min-w-0 px-3 py-3">
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span className="font-semibold">{resource.name}</span>
                            <Badge>#{resource.id}</Badge>
                            <Badge tone={resource.enabled ? 'success' : 'error'}>{resource.enabled ? '启用' : '已禁用'}</Badge>
                            {resource.hasPassword && <Badge tone="info">密码</Badge>}
                          </div>
                          <div className="mt-1 max-w-[32rem] truncate font-mono text-xs text-muted-foreground" title={resource.proxyUrl}>
                            {resource.proxyUrl}
                          </div>
                          {resource.notes && (
                            <div className="mt-1 max-w-[32rem] truncate text-xs text-muted-foreground" title={resource.notes}>
                              {resource.notes}
                            </div>
                          )}
                        </td>
                        <td className="px-3 py-3 text-xs">
                          <div>用户：<span className="font-mono">{showSecrets ? resource.proxyUsername || '-' : maskSecret(resource.proxyUsername)}</span></div>
                          <div className="mt-1">密码：<span className="font-mono">{showSecrets ? resource.proxyPassword || '-' : resource.hasPassword ? maskSecret(resource.proxyPassword || '******') : '-'}</span></div>
                        </td>
                        <td className="px-3 py-3">
                          <div className="font-semibold tabular-nums">{resource.credentialCount}</div>
                          <div className="text-xs text-muted-foreground">账号</div>
                        </td>
                        <td className="px-3 py-3">
                          {testing ? (
                            <Badge tone="info">测试中…</Badge>
                          ) : result ? (
                            <Badge tone={latencyTone(result)}>{latencyLabel(result)}</Badge>
                          ) : (
                            <Badge tone="neutral">未测试</Badge>
                          )}
                          {result && !result.success && (
                            <div className="mt-1 max-w-56 truncate text-xs text-muted-foreground" title={result.message}>
                              {result.message}
                            </div>
                          )}
                        </td>
                        <td className="px-3 py-3 text-xs text-muted-foreground">
                          <div>{formatDate(resource.updatedAt)}</div>
                          <div className="mt-1">创建 {formatDate(resource.createdAt)}</div>
                        </td>
                        <td className="px-3 py-3">
                          <div className="flex justify-end gap-1.5">
                            <Button variant="ghost" size="icon-sm" title="测活" onClick={() => testOne(resource)} disabled={testing}>
                              {testing ? <Spinner size="sm" /> : <Activity className="size-4" />}
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              title="更换代理绑定"
                              onClick={() => requestReplaceBindings(resource)}
                              disabled={resource.credentialCount === 0 || batchUpdateCredentials.isPending || rowLoading}
                            >
                              {rowLoading && rowAction === 'replace' ? <Spinner size="sm" /> : <ArrowRightLeft className="size-4" />}
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              title="清除代理绑定"
                              onClick={() => clearBindings(resource)}
                              disabled={resource.credentialCount === 0 || batchUpdateCredentials.isPending || rowLoading}
                            >
                              {rowLoading && rowAction === 'clear' ? <Spinner size="sm" /> : <Unlink className="size-4" />}
                            </Button>
                            <Button variant="ghost" size="icon-sm" title="编辑代理" onClick={() => openEdit(resource)}>
                              <Edit3 className="size-4" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              className="text-destructive hover:bg-destructive/10 hover:text-destructive"
                              title="删除代理"
                              onClick={() => requestDelete(resource)}
                              disabled={deletingId === resource.id || rowLoading}
                            >
                              {deletingId === resource.id || (rowLoading && rowAction === 'delete') ? <Spinner size="sm" /> : <Trash2 className="size-4" />}
                            </Button>
                          </div>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>

            <Pagination
              page={page}
              pageCount={pageCount}
              total={list.length}
              pageSize={pageSize}
              pageSizeOptions={PAGE_SIZE_OPTIONS}
              onPageChange={setPage}
              onPageSizeChange={handlePageSizeChange}
              pending={resources.isFetching}
            />
          </div>
        )}
      </div>

      <ModalShell
        open={batchDeleteOpen}
        onClose={() => { if (!batchDeleting) { setBatchDeleteOpen(false); setBatchDeleteBound([]) } }}
        title={`删除 ${selectedResources.length} 个代理`}
        description="删除后不可恢复。已禁用账号与这些代理的绑定会一起清除。"
        width="max-w-xl"
        footer={(
          <>
            <Button variant="outline" size="sm" onClick={() => setBatchDeleteOpen(false)} disabled={batchDeleting}>取消</Button>
            <Button variant="destructive" size="sm" onClick={() => void confirmBatchDelete()} disabled={batchDeleting || selectedResources.length === 0}>
              {batchDeleting && <Spinner size="sm" />}
              {batchEnabledBound.length > 0 ? '改绑后删除' : '确认删除'}
            </Button>
          </>
        )}
      >
        <div className="space-y-4 text-sm">
          <div className="max-h-40 overflow-y-auto rounded-lg border border-border/60 text-xs">
            {selectedResources.map((resource) => {
              const count = batchDeleteBound.filter((credential) => credential.proxyResourceId === resource.id)
              const enabled = count.filter((credential) => !credential.disabled).length
              return (
                <div key={resource.id} className="flex items-center justify-between gap-3 border-b border-border/60 px-3 py-1.5 last:border-b-0">
                  <span className="min-w-0 truncate">{resource.name} <span className="text-muted-foreground">#{resource.id}</span></span>
                  <span className={enabled > 0 ? 'text-warning' : 'text-muted-foreground'}>
                    {count.length === 0 ? '未绑定账号' : `绑定 ${count.length} 个（启用 ${enabled}）`}
                  </span>
                </div>
              )
            })}
          </div>
          {batchEnabledBound.length > 0 ? (
            <div className="space-y-2 rounded-lg border border-warning/40 bg-warning/5 p-3">
              <div className="font-medium">有 {batchEnabledBound.length} 个启用账号还在用这些代理，删除前先改绑到：</div>
              <Select value={batchReplacement} onValueChange={setBatchReplacement} disabled={batchDeleting}>
                <SelectTrigger size="sm" className="sm:w-80"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value={DIRECT_PROXY_VALUE}>不绑定代理（走全局代理或直连）</SelectItem>
                  {batchReplacementOptions.map((resource) => <SelectItem key={resource.id} value={String(resource.id)}>{resource.name}</SelectItem>)}
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">要把这些账号分散到多个代理，可以先在账号页用批量修改轮换代理，再回来删除。</p>
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">这些代理没有绑定启用账号，可以直接删除。</p>
          )}
        </div>
      </ModalShell>

      <ModalShell
        open={Boolean(deleteCandidate)}
        onClose={() => {
          if (!batchUpdateCredentials.isPending && !removeProxy.isPending) {
            setDeleteBoundCredentials([])
            setDeleteCandidate(null)
          }
        }}
        title={deleteCandidate ? `更换绑定账号代理：${deleteCandidate.name}` : '更换绑定账号代理'}
        description="该代理仍绑定启用账号。请选择新代理或清空代理关联后再删除。"
        width="max-w-lg"
        footer={(
          <>
            <Button
              variant="outline"
              onClick={() => {
                setDeleteBoundCredentials([])
                setDeleteCandidate(null)
              }}
              disabled={batchUpdateCredentials.isPending || removeProxy.isPending}
            >
              取消
            </Button>
            <Button
              onClick={replaceEnabledBindingsAndDelete}
              disabled={batchUpdateCredentials.isPending || removeProxy.isPending}
            >
              {batchUpdateCredentials.isPending || removeProxy.isPending ? <Spinner size="sm" /> : <Trash2 className="size-4" />}
              更换并删除
            </Button>
          </>
        )}
      >
        <div className="space-y-4">
          <div className="rounded-lg bg-muted/30 p-3 text-sm">
            <div className="font-medium">将处理 {deleteCandidateEnabledCredentials.length} 个启用账号</div>
            <div className="mt-1 text-xs text-muted-foreground">
              已禁用账号的关联会在删除代理时由后端自动清除。
            </div>
          </div>
          <Select value={replacementProxy} onValueChange={setReplacementProxy}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value={DIRECT_PROXY_VALUE}>清空代理关联</SelectItem>
              {replacementOptions.map((resource) => (
                <SelectItem key={resource.id} value={String(resource.id)}>
                  {resource.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <div className="max-h-48 space-y-1 overflow-y-auto rounded-lg border border-border/60 p-2">
            {deleteCandidateEnabledCredentials.map((credential) => (
              <div key={credential.id} className="flex items-center justify-between gap-2 rounded bg-muted/30 px-2 py-1 text-xs">
                <span className="min-w-0 truncate" title={credentialLabel(credential)}>
                  {credentialLabel(credential)}
                </span>
                <Badge>#{credential.id}</Badge>
              </div>
            ))}
          </div>
        </div>
      </ModalShell>

      <ModalShell
        open={Boolean(replaceCandidate)}
        onClose={() => {
          if (!batchUpdateCredentials.isPending) {
            setReplaceCandidate(null)
            setReplaceBoundCredentials([])
            setReplaceSelectedIds(new Set())
            setReplaceTargetProxy('')
            setReplaceMode('single')
            setReplaceTargetsByCredential(new Map())
          }
        }}
        title={replaceCandidate ? `更换绑定代理：${replaceCandidate.name}` : '更换绑定代理'}
        description="选择当前代理绑定的账号，可统一更换到一个代理，也可为每个账号单独选择目标代理。"
        width="max-w-2xl"
        footer={(
          <>
            <Button
              variant="outline"
              onClick={() => {
                setReplaceCandidate(null)
                setReplaceBoundCredentials([])
                setReplaceSelectedIds(new Set())
                setReplaceTargetProxy('')
                setReplaceMode('single')
                setReplaceTargetsByCredential(new Map())
              }}
              disabled={batchUpdateCredentials.isPending}
            >
              取消
            </Button>
            <Button
              onClick={applyReplaceBindings}
              disabled={batchUpdateCredentials.isPending || replaceSelectedIds.size === 0 || !replaceReady}
            >
              {batchUpdateCredentials.isPending ? <Spinner size="sm" /> : <ArrowRightLeft className="size-4" />}
              更换代理
            </Button>
          </>
        )}
      >
        <div className="space-y-4">
          <div className="grid gap-3 sm:grid-cols-[1fr_1.2fr]">
            <div className="rounded-lg bg-muted/30 p-3 text-sm">
              <div className="font-medium">当前绑定账号</div>
              <div className="mt-1 text-xs text-muted-foreground">
                已选 {replaceSelectedCredentials.length}/{replaceBoundCredentials.length} 个账号。
              </div>
            </div>
            <div className="space-y-1.5">
              <div className="text-xs font-medium text-muted-foreground">更换方式</div>
              <div className="grid grid-cols-2 gap-1 rounded-md bg-muted/40 p-1">
                <Button
                  type="button"
                  variant={replaceMode === 'single' ? 'secondary' : 'ghost'}
                  size="xs"
                  onClick={() => setReplaceMode('single')}
                >
                  统一目标
                </Button>
                <Button
                  type="button"
                  variant={replaceMode === 'individual' ? 'secondary' : 'ghost'}
                  size="xs"
                  onClick={() => setReplaceMode('individual')}
                >
                  单独设置
                </Button>
              </div>
            </div>
          </div>
          {replaceMode === 'single' && (
            <div className="space-y-1.5">
              <div className="text-xs font-medium text-muted-foreground">统一目标代理</div>
              <Select value={replaceTargetProxy} onValueChange={(value) => {
                setReplaceTargetProxy(value)
                setReplaceTargetsByCredential((prev) => {
                  const next = new Map(prev)
                  replaceBoundCredentials.forEach((credential) => next.set(credential.id, value))
                  return next
                })
              }}>
                <SelectTrigger><SelectValue placeholder="选择目标代理" /></SelectTrigger>
                <SelectContent>
                  {replaceOptions.map((resource) => (
                    <SelectItem key={resource.id} value={String(resource.id)}>
                      {resource.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          )}
          <label className="flex items-center gap-2 text-xs text-muted-foreground">
            <Checkbox checked={allReplaceSelected} onCheckedChange={toggleAllReplaceCredentials} />
            全选当前绑定账号
          </label>
          <div className="max-h-80 space-y-1 overflow-y-auto rounded-lg border border-border/60 p-2">
            {replaceBoundCredentials.map((credential) => {
              const selected = replaceSelectedIds.has(credential.id)
              return (
                <div
                  key={credential.id}
                  className={cn(
                    'grid gap-2 rounded px-2 py-1.5 text-xs sm:grid-cols-[minmax(0,1fr)_14rem]',
                    selected ? 'bg-primary/5 text-primary' : 'bg-muted/30 hover:bg-muted'
                  )}
                >
                  <span className="flex min-w-0 items-center justify-between gap-2">
                    <span className="flex min-w-0 items-center gap-2">
                      <Checkbox checked={selected} onCheckedChange={() => toggleReplaceCredential(credential.id)} />
                      <span className="truncate" title={credentialLabel(credential)}>
                        {credentialLabel(credential)}
                      </span>
                    </span>
                    <span className="flex shrink-0 items-center gap-1.5">
                      <Badge>#{credential.id}</Badge>
                      <Badge tone={credential.disabled ? 'error' : 'success'}>
                        {credential.disabled ? '已禁用' : '启用'}
                      </Badge>
                    </span>
                  </span>
                  {replaceMode === 'individual' && (
                    <Select
                      value={replaceTargetsByCredential.get(credential.id) || replaceTargetProxy}
                      onValueChange={(value) => setReplaceCredentialTarget(credential.id, value)}
                      disabled={!selected || batchUpdateCredentials.isPending}
                    >
                      <SelectTrigger size="sm" className="h-8"><SelectValue placeholder="目标代理" /></SelectTrigger>
                      <SelectContent>
                        {replaceOptions.map((resource) => (
                          <SelectItem key={resource.id} value={String(resource.id)}>
                            {resource.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}
                </div>
              )
            })}
          </div>
        </div>
      </ModalShell>

      <ProxyEditorModal open={editorOpen} resource={editing} onClose={closeEditor} />
      <ProxyImportModal
        open={importOpen}
        onClose={() => setImportOpen(false)}
        onImported={handleImported}
      />
    </PageContainer>
  )
}
