import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import type { ColumnDef } from '@tanstack/react-table'
import { FileUp, FlaskConical, Loader2, MoreHorizontal, Pencil, Plus, Trash2 } from 'lucide-react'
import { proxiesApi } from '@/api/endpoints/proxies'
import type { ProxyResource, ProxyResourceTestResponse } from '@/api/types'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Switch } from '@/components/ui/switch'
import { DataTable } from '@/components/data-table/data-table'
import { EmptyState, ErrorState } from '@/components/patterns/data-state'
import { Page, PageHeader } from '@/components/patterns/page-header'
import { useConfirm } from '@/components/patterns/confirm'
import { ToneBadge } from '@/components/status/tone-badge'
import { fmtMs, fmtRelative } from '@/lib/format'
import { qk } from '@/queries/keys'
import { useProxies } from '@/queries/shared'
import { ImportDialog, ProxyDialog } from './proxy-dialogs'

/** 代理 URL 去掉内嵌凭据后展示 */
function displayUrl(url: string): string {
  try {
    const u = new URL(url)
    u.username = ''
    u.password = ''
    return u.toString().replace(/\/$/, '')
  } catch {
    return url
  }
}

export function ProxiesPage() {
  const proxies = useProxies()
  const queryClient = useQueryClient()
  const confirm = useConfirm()
  const [editing, setEditing] = useState<ProxyResource | 'new' | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const [results, setResults] = useState<Record<number, ProxyResourceTestResponse | 'pending'>>({})
  const invalidate = () => queryClient.invalidateQueries({ queryKey: qk.proxies.all })

  const test = async (id: number) => {
    setResults((r) => ({ ...r, [id]: 'pending' }))
    try {
      const res = await proxiesApi.test(id)
      setResults((r) => ({ ...r, [id]: res }))
    } catch (e) {
      setResults((r) => ({ ...r, [id]: { success: false, message: (e as Error).message, proxyUrl: '', testUrl: '', durationMs: 0 } }))
    }
  }
  const testAll = () => (proxies.data?.resources ?? []).filter((p) => p.enabled).forEach((p) => void test(p.id))

  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) => proxiesApi.update(id, { enabled }),
    onSettled: invalidate,
    meta: { error: '更新失败' },
  })
  const remove = useMutation({
    mutationFn: (id: number) => proxiesApi.remove(id),
    onSettled: invalidate,
    meta: { success: '代理已删除', error: '删除失败' },
  })

  const columns = useMemo<ColumnDef<ProxyResource, unknown>[]>(
    () => [
      {
        id: 'name',
        header: '名称',
        size: 180,
        meta: { mobile: 'title' },
        cell: ({ row: { original: p } }) => (
          <div className="min-w-0 leading-tight">
            <div className="truncate font-medium">{p.name}</div>
            {p.notes && <div className="truncate text-xs text-muted-foreground">{p.notes}</div>}
          </div>
        ),
      },
      {
        id: 'url',
        header: '地址',
        size: 260,
        meta: { mobile: true, grow: true },
        cell: ({ row: { original: p } }) => (
          <span className="truncate font-mono text-xs">
            {displayUrl(p.proxyUrl)}
            {(p.proxyUsername || p.hasPassword) && <span className="ml-1.5 text-muted-foreground">（含认证）</span>}
          </span>
        ),
      },
      {
        id: 'enabled',
        header: '启用',
        size: 72,
        meta: { mobile: 'badge' },
        cell: ({ row: { original: p } }) => (
          <span onClick={(e) => e.stopPropagation()}>
            <Switch
              checked={p.enabled}
              aria-label={`启用 ${p.name}`}
              onCheckedChange={async (enabled) => {
                if (!enabled && p.credentialCount > 0) {
                  const ok = await confirm({
                    title: `停用代理「${p.name}」？`,
                    description: `${p.credentialCount} 个绑定账号会因代理不可用而无法调度，直到重新启用或改绑。`,
                    destructive: true,
                    confirmText: '停用',
                  })
                  if (!ok) return
                }
                toggle.mutate({ id: p.id, enabled })
              }}
            />
          </span>
        ),
      },
      {
        id: 'bound',
        header: '绑定账号',
        size: 96,
        meta: { mobile: true, align: 'right' },
        cell: ({ row: { original: p } }) =>
          p.credentialCount > 0 ? (
            <Link to="/accounts" search={{ proxy: p.id }} className="text-primary hover:underline" onClick={(e) => e.stopPropagation()}>
              {p.credentialCount}
            </Link>
          ) : (
            <span className="text-muted-foreground">0</span>
          ),
      },
      {
        id: 'test',
        header: '最近测试',
        size: 200,
        meta: { mobile: true },
        cell: ({ row: { original: p } }) => {
          const r = results[p.id]
          if (r === 'pending') return <Loader2 className="size-4 animate-spin text-muted-foreground" />
          if (!r) return <span className="text-xs text-muted-foreground">未测试</span>
          return (
            <span title={r.message} className="flex items-center gap-1.5">
              <ToneBadge tone={r.success ? 'success' : 'danger'}>{r.success ? '可用' : '失败'}</ToneBadge>
              <span className="num truncate text-xs text-muted-foreground">{r.success ? fmtMs(r.durationMs) : r.message}</span>
            </span>
          )
        },
      },
      {
        id: 'updated',
        header: '更新',
        size: 90,
        meta: { align: 'right' },
        cell: ({ row: { original: p } }) => <span className="text-xs text-muted-foreground">{fmtRelative(p.updatedAt)}</span>,
      },
      {
        id: 'actions',
        header: '',
        size: 48,
        cell: ({ row: { original: p } }) => (
          <span onClick={(e) => e.stopPropagation()}>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon-xs" aria-label="更多操作">
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => void test(p.id)}>
                  <FlaskConical /> 测试
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => setEditing(p)}>
                  <Pencil /> 编辑
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  variant="destructive"
                  onSelect={async () => {
                    const ok = await confirm({
                      title: `删除代理「${p.name}」？`,
                      description:
                        p.credentialCount > 0 ? `${p.credentialCount} 个账号绑定了该代理，删除后这些账号将无法调度。` : '删除后不可恢复。',
                      destructive: true,
                      confirmText: '删除',
                    })
                    if (ok) remove.mutate(p.id)
                  }}
                >
                  <Trash2 /> 删除
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </span>
        ),
      },
    ],
    [results], // eslint-disable-line react-hooks/exhaustive-deps
  )

  const list = proxies.data?.resources ?? []
  return (
    <Page>
      <PageHeader
        title="代理"
        description="出站代理资源；账号绑定后通过该代理访问 Kiro 上游"
        actions={
          <>
            <Button variant="outline" size="sm" onClick={testAll} disabled={!list.length}>
              <FlaskConical /> 测试全部
            </Button>
            <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
              <FileUp /> 批量导入
            </Button>
            <Button size="sm" onClick={() => setEditing('new')}>
              <Plus /> 添加代理
            </Button>
          </>
        }
      />
      {proxies.error && !proxies.data ? (
        <ErrorState error={proxies.error} onRetry={() => proxies.refetch()} />
      ) : (
        <DataTable
          data={list}
          columns={columns}
          getRowId={(p) => String(p.id)}
          onRowClick={(p) => setEditing(p)}
          rowClassName={(p) => (p.enabled ? undefined : 'text-muted-foreground')}
          empty={
            proxies.isLoading ? (
              <div className="p-6 text-center text-sm text-muted-foreground">加载中…</div>
            ) : (
              <EmptyState title="还没有代理资源" />
            )
          }
        />
      )}
      <ProxyDialog
        proxy={editing === 'new' ? undefined : (editing ?? undefined)}
        open={editing !== null}
        onOpenChange={(o) => !o && setEditing(null)}
      />
      <ImportDialog open={importOpen} onOpenChange={setImportOpen} />
    </Page>
  )
}
