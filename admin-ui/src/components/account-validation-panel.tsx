import { AlertTriangle, FileSearch, RefreshCw } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { validateExistingCredentials, validateExternalCredentials } from '@/api/credentials'
import { parseCredentialImportText } from '@/lib/credential-import'
import { extractErrorMessage } from '@/lib/utils'
import type { CredentialValidationGroup, CredentialValidationItem, CredentialValidationResponse } from '@/types/api'

function formatNumber(value: number | null | undefined): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '0'
  return new Intl.NumberFormat('zh-CN').format(value as number)
}

function formatQuota(value: number | null | undefined): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 2 }).format(value as number)
}

function formatDate(value?: string | null): string {
  if (!value) return '-'
  return new Date(value).toLocaleString('zh-CN', { hour12: false })
}

function badgeVariant(key: string): 'default' | 'secondary' | 'destructive' | 'outline' {
  if (key === 'downgraded' || key === 'failed') return 'destructive'
  if (key === 'upgraded' || key === 'pro' || key === 'pro_plus') return 'default'
  return 'secondary'
}

function itemTitle(item: CredentialValidationItem) {
  if (item.id) return `#${item.id} ${item.email || ''}`.trim()
  return `#${item.index || '-'} ${item.email || ''}`.trim()
}

function quotaText(item: CredentialValidationItem) {
  if (!item.current) return '-'
  return `${formatQuota(item.current.currentUsage)}/${formatQuota(item.current.usageLimit)}`
}

function ResultGroup({ group }: { group: CredentialValidationGroup }) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-base">
          <Badge variant={badgeVariant(group.key)}>{group.title}</Badge>
          <span className="text-sm font-normal text-muted-foreground">{group.count} 个</span>
        </CardTitle>
      </CardHeader>
      <CardContent className="divide-y p-0">
        {group.items.map((item) => (
          <div key={`${item.id || 'external'}-${item.index || item.email || item.subscriptionTitle}`} className="grid gap-3 px-4 py-3 text-sm lg:grid-cols-[1.4fr_1fr_1fr_1.2fr]">
            <div className="min-w-0">
              <div className="truncate font-semibold" title={itemTitle(item)}>{itemTitle(item)}</div>
              <div className="mt-1 flex flex-wrap gap-1">
                {item.disabled !== null && item.disabled !== undefined && <Badge variant={item.disabled ? 'destructive' : 'secondary'}>{item.disabled ? '已禁用' : '启用'}</Badge>}
                {item.matchedExistingCredentialId && <Badge variant="outline">匹配系统 #{item.matchedExistingCredentialId}</Badge>}
                {item.existingDisabled && <Badge variant="destructive">系统内已禁用</Badge>}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">订阅</div>
              <div className="font-medium">{item.current?.subscriptionTitle || item.subscriptionTitle || '-'}</div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">额度</div>
              <div className="font-mono font-medium">{quotaText(item)}</div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">状态</div>
              <div className="break-words">{item.error ? <span className="text-destructive">{item.error}</span> : item.current ? formatDate(item.current.checkedAt) : '-'}</div>
            </div>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}

function Results({ result }: { result: CredentialValidationResponse | null }) {
  if (!result) {
    return <Card><CardContent className="py-8 text-center text-muted-foreground">暂无校验结果</CardContent></Card>
  }
  return (
    <div className="space-y-4">
      <div className="grid gap-4 md:grid-cols-4">
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">总数</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold">{formatNumber(result.total)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">成功</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-green-600">{formatNumber(result.success)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">失败</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-amber-600">{formatNumber(result.failed)}</div></CardContent></Card>
        <Card><CardHeader className="pb-2"><CardTitle className="text-sm text-muted-foreground">疑似掉级</CardTitle></CardHeader><CardContent><div className="text-2xl font-bold text-red-600">{formatNumber(result.downgraded)}</div></CardContent></Card>
      </div>
      {result.groups.length === 0 ? <Card><CardContent className="py-8 text-center text-muted-foreground">暂无分组结果</CardContent></Card> : result.groups.map(group => <ResultGroup key={group.key} group={group} />)}
    </div>
  )
}

export function AccountValidationPanel() {
  const [raw, setRaw] = useState('')
  const [result, setResult] = useState<CredentialValidationResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const parsedCount = useMemo(() => {
    try {
      return raw.trim() ? parseCredentialImportText(raw).length : 0
    } catch {
      return 0
    }
  }, [raw])

  const validateExisting = async (scope: 'all' | 'enabled' | 'disabled') => {
    setLoading(true)
    try {
      const data = await validateExistingCredentials({ scope, force: true })
      setResult(data)
      toast.success(`校验完成：成功 ${data.success}/${data.total}`)
    } catch (error) {
      toast.error(`校验失败: ${extractErrorMessage(error)}`)
    } finally {
      setLoading(false)
    }
  }

  const validateExternal = async () => {
    let credentials
    try {
      credentials = parseCredentialImportText(raw)
    } catch (error) {
      toast.error(`JSON 解析失败: ${extractErrorMessage(error)}`)
      return
    }
    if (credentials.length === 0) {
      toast.error('没有解析到可校验的凭据')
      return
    }
    setLoading(true)
    try {
      const data = await validateExternalCredentials({ credentials })
      setResult(data)
      toast.success(`校验完成：成功 ${data.success}/${data.total}`)
    } catch (error) {
      toast.error(`校验失败: ${extractErrorMessage(error)}`)
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <CardTitle>系统凭据复查</CardTitle>
              <CardDescription>强制查询系统内凭据的订阅、额度和用量，并和上次快照比较。禁用凭据也可以手动复查。</CardDescription>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => validateExisting('all')} disabled={loading}><RefreshCw className="h-4 w-4 mr-2" />全部复查</Button>
              <Button variant="outline" size="sm" onClick={() => validateExisting('enabled')} disabled={loading}>仅启用</Button>
              <Button variant="outline" size="sm" onClick={() => validateExisting('disabled')} disabled={loading}>仅禁用</Button>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="flex items-start gap-2 rounded-md border border-amber-200 bg-amber-50 p-3 text-sm text-amber-900 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
            <div>掉级判断基于上次保存的信息和本次强制查询结果；首次查询没有历史快照时会标记为未知。</div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <CardTitle>外部 JSON 校验</CardTitle>
              <CardDescription>粘贴 Kiro Account Manager 或批量导入格式 JSON，只查询订阅和额度，不导入系统、不改变调度。</CardDescription>
            </div>
            <Button size="sm" onClick={validateExternal} disabled={loading || parsedCount === 0}><FileSearch className="h-4 w-4 mr-2" />校验 JSON</Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-2">
          <textarea
            className="min-h-[220px] w-full rounded-md border bg-background px-3 py-2 font-mono text-xs"
            value={raw}
            onChange={event => setRaw(event.target.value)}
            placeholder="粘贴 KAM JSON、credentials 数组或 JSONL"
          />
          <div className="text-xs text-muted-foreground">已解析 {parsedCount} 条可校验凭据</div>
        </CardContent>
      </Card>

      <Results result={result} />
    </div>
  )
}
