import * as React from 'react'
import { Activity, ArrowRightLeft, CheckCircle2, Edit3, Eye, EyeOff, Network, Plus, RefreshCw, Trash2, Unlink } from 'lucide-react'
import { toast } from 'sonner'
import {
  useBatchUpdateCredentials,
  useCredentials,
  useDeleteProxyResource,
  useProxyResources,
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

const PAGE_SIZE_OPTIONS = [10, 25, 50, 100]
const DIRECT_PROXY_VALUE = '__direct__'
type ReplaceProxyMode = 'single' | 'individual'

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

function latencyLabel(result: ProxyResourceTestResponse): string {
  if (!result.success) return result.status ? `失败 · HTTP ${result.status}` : '失败'
  return `${result.durationMs}ms`
}

export function ProxiesPage() {
  const resources = useProxyResources()
  const allCredentials = useCredentials({ enabled: false, refetchInterval: false })
  const removeProxy = useDeleteProxyResource()
  const testProxy = useTestProxyResource()
  const batchUpdateCredentials = useBatchUpdateCredentials()
  const confirm = useConfirm()

  const [page, setPage] = React.useState(1)
  const [pageSize, setPageSize] = React.useState(PAGE_SIZE_OPTIONS[0])
  const [selectedIds, setSelectedIds] = React.useState<Set<number>>(new Set())
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

  const list = resources.data?.resources || []
  const pageCount = Math.max(1, Math.ceil(list.length / pageSize))
  const pagedList = React.useMemo(
    () => list.slice((page - 1) * pageSize, page * pageSize),
    [list, page, pageSize],
  )
  const selectedResources = React.useMemo(
    () => list.filter((resource) => selectedIds.has(resource.id)),
    [list, selectedIds],
  )
  const allPageSelected = pagedList.length > 0 && pagedList.every((resource) => selectedIds.has(resource.id))
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

  React.useEffect(() => {
    const validIds = new Set(list.map((resource) => resource.id))
    setSelectedIds((prev) => new Set([...prev].filter((id) => validIds.has(id))))
  }, [list])

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
      if (allPageSelected) {
        pagedList.forEach((resource) => next.delete(resource.id))
      } else {
        pagedList.forEach((resource) => next.add(resource.id))
      }
      return next
    })
  }

  const loadCredentialSnapshot = async () => {
    const snapshot = allCredentials.data ?? (await allCredentials.refetch()).data
    return snapshot?.credentials || []
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

  const batchTestSelected = async () => {
    if (!selectedResources.length) return toast.error('请先选择要测活的代理')
    const pendingIds = new Set(selectedResources.map((resource) => resource.id))
    setTestingIds(pendingIds)
    let passed = 0
    let cursor = 0
    const workerCount = Math.min(6, selectedResources.length)
    await Promise.all(Array.from({ length: workerCount }, async () => {
      while (cursor < selectedResources.length) {
        const resource = selectedResources[cursor]
        cursor += 1
        try {
          const result = await testProxy.mutateAsync({ id: resource.id, request: {} })
          setTestResults((prev) => new Map(prev).set(resource.id, result))
          if (result.success) passed += 1
        } catch (error) {
          setTestResults((prev) => new Map(prev).set(resource.id, {
            success: false,
            message: `测活请求失败: ${extractErrorMessage(error)}`,
            proxyUrl: resource.proxyUrl,
            testUrl: '',
            status: null,
            durationMs: 0,
          }))
          toast.error(`代理「${resource.name}」测试失败: ${extractErrorMessage(error)}`)
        } finally {
          setTestingIds((prev) => {
            const next = new Set(prev)
            next.delete(resource.id)
            return next
          })
        }
      }
    }))
    toast.success(`批量测活完成：通过 ${passed}/${selectedResources.length}`)
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
    let credentials: CredentialStatusItem[]
    try {
      credentials = await loadCredentialSnapshot()
    } catch (error) {
      toast.error(`加载账号绑定失败: ${extractErrorMessage(error)}`)
      return
    }
    const bound = credentials.filter((credential) => credential.proxyResourceId === resource.id)
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
    try {
      await removeProxy.mutateAsync(resource.id)
      setSelectedIds((prev) => {
        const next = new Set(prev)
        next.delete(resource.id)
        return next
      })
      toast.success(disabledBound > 0 ? `代理已删除，已清除 ${disabledBound} 个已禁用账号关联` : '代理资源已删除')
    } catch (error) {
      toast.error(`删除失败: ${extractErrorMessage(error)}`)
    }
  }

  const requestReplaceBindings = async (resource: ProxyResource) => {
    let credentials: CredentialStatusItem[]
    try {
      credentials = await loadCredentialSnapshot()
    } catch (error) {
      toast.error(`加载账号绑定失败: ${extractErrorMessage(error)}`)
      return
    }
    const bound = credentials.filter((credential) => credential.proxyResourceId === resource.id)
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
    let credentials: CredentialStatusItem[]
    try {
      credentials = await loadCredentialSnapshot()
    } catch (error) {
      toast.error(`加载账号绑定失败: ${extractErrorMessage(error)}`)
      return
    }
    const bound = credentials.filter((credential) => credential.proxyResourceId === resource.id)
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
      setSelectedIds((prev) => {
        const next = new Set(prev)
        next.delete(deleteCandidate.id)
        return next
      })
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
            <Button variant="outline" size="sm" onClick={batchTestSelected} disabled={selectedResources.length === 0 || testingIds.size > 0}>
              {testingIds.size > 0 ? <Spinner size="sm" /> : <CheckCircle2 className="h-4 w-4" />}
              批量测活
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
              <label className="flex items-center gap-2 text-xs text-muted-foreground">
                <Checkbox checked={allPageSelected} onCheckedChange={togglePageSelected} />
                全选当前页
              </label>
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                {selectedIds.size > 0 && <Badge tone="primary">已选 {selectedIds.size}</Badge>}
                {resources.isFetching && <Spinner size="sm" />}
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
                    const result = testResults.get(resource.id)
                    const testing = testingIds.has(resource.id)
                    return (
                      <tr key={resource.id} className="bg-card align-top">
                        <td className="px-3 py-3">
                          <Checkbox checked={selectedIds.has(resource.id)} onCheckedChange={() => toggleSelected(resource.id)} />
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
                              disabled={resource.credentialCount === 0 || batchUpdateCredentials.isPending}
                            >
                              <ArrowRightLeft className="size-4" />
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              title="清除代理绑定"
                              onClick={() => clearBindings(resource)}
                              disabled={resource.credentialCount === 0 || batchUpdateCredentials.isPending}
                            >
                              <Unlink className="size-4" />
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
                              disabled={removeProxy.isPending || batchUpdateCredentials.isPending}
                            >
                              <Trash2 className="size-4" />
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
