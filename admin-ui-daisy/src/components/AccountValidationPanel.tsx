import { AlertTriangle, FileSearch, RefreshCw, Upload } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'
import { Button, Loading, Textarea } from 'react-daisyui'
import { validateExistingCredentials, validateExternalCredentials } from '@/api/credentials'
import { Badge, EmptyState, SectionCard, StatCard } from '@/components/common'
import { formatDate, formatNumber, formatQuota } from '@/lib/format'
import { parseCredentialImportText } from '@/lib/credential-import'
import { extractErrorMessage } from '@/lib/utils'
import type { CredentialValidationGroup, CredentialValidationItem, CredentialValidationResponse } from '@/types/api'

function changeTone(kind: string): 'neutral' | 'success' | 'warning' | 'error' | 'info' | 'secondary' {
  if (kind === 'downgraded' || kind === 'failed') return 'error'
  if (kind === 'upgraded') return 'success'
  if (kind === 'unknown') return 'warning'
  return 'secondary'
}

function itemTitle(item: CredentialValidationItem) {
  if (item.id) return `#${item.id} ${item.email || ''}`.trim()
  return `#${item.index || '-'} ${item.email || ''}`.trim()
}

function quotaText(item: CredentialValidationItem) {
  const info = item.current
  if (!info) return '-'
  return `${formatQuota(info.currentUsage)}/${formatQuota(info.usageLimit)}`
}

function ResultGroup({ group }: { group: CredentialValidationGroup }) {
  return (
    <div className="rounded-box border border-base-300 bg-base-100">
      <div className="flex items-center justify-between border-b border-base-300 px-3 py-2">
        <div className="flex items-center gap-2">
          <Badge tone={changeTone(group.key)}>{group.title}</Badge>
          <span className="text-sm text-base-content/60">{group.count} 个</span>
        </div>
      </div>
      <div className="divide-y divide-base-200">
        {group.items.map((item) => (
          <div key={`${item.id || 'external'}-${item.index || item.email || item.subscriptionTitle}`} className="grid gap-2 px-3 py-2 text-sm lg:grid-cols-[1.4fr_1fr_1fr_1.2fr]">
            <div className="min-w-0">
              <div className="truncate font-semibold" title={itemTitle(item)}>
                {itemTitle(item)}
              </div>
              <div className="mt-0.5 flex flex-wrap gap-1">
                {item.disabled !== null && item.disabled !== undefined && <Badge tone={item.disabled ? 'error' : 'success'}>{item.disabled ? '已禁用' : '启用'}</Badge>}
                {item.matchedExistingCredentialId && <Badge tone="info">匹配系统 #{item.matchedExistingCredentialId}</Badge>}
                {item.existingDisabled && <Badge tone="warning">系统内已禁用</Badge>}
              </div>
            </div>
            <div>
              <div className="text-xs text-base-content/50">订阅</div>
              <div className="font-medium">{item.current?.subscriptionTitle || item.subscriptionTitle || '-'}</div>
            </div>
            <div>
              <div className="text-xs text-base-content/50">额度</div>
              <div className="font-mono font-medium">{quotaText(item)}</div>
            </div>
            <div>
              <div className="text-xs text-base-content/50">状态</div>
              <div className="break-words">
                {item.error ? <span className="text-error">{item.error}</span> : item.current ? formatDate(item.current.checkedAt) : '-'}
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

function ValidationResults({ result }: { result: CredentialValidationResponse | null }) {
  if (!result) return <EmptyState text="暂无校验结果" />
  return (
    <div className="space-y-3">
      <div className="metric-grid">
        <StatCard title="总数" value={formatNumber(result.total)} />
        <StatCard title="成功" value={formatNumber(result.success)} tone="success" />
        <StatCard title="失败" value={formatNumber(result.failed)} tone={result.failed ? 'warning' : 'default'} />
        <StatCard title="疑似掉级" value={formatNumber(result.downgraded)} tone={result.downgraded ? 'warning' : 'default'} />
      </div>
      {result.groups.length === 0 ? <EmptyState text="暂无分组结果" /> : result.groups.map((group) => <ResultGroup key={group.key} group={group} />)}
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
    <div className="space-y-4">
      <SectionCard
        title="系统凭据复查"
        description="强制查询系统内凭据的订阅、额度和用量，并和上次快照比较。禁用凭据也可以手动复查，不会参与调度。"
        actions={
          <>
            <Button type="button" variant="outline" size="sm" onClick={() => validateExisting('all')} disabled={loading}>
              {loading ? <Loading size="xs" /> : <RefreshCw className="h-4 w-4" />}
              全部复查
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => validateExisting('enabled')} disabled={loading}>
              仅启用
            </Button>
            <Button type="button" variant="outline" size="sm" onClick={() => validateExisting('disabled')} disabled={loading}>
              仅禁用
            </Button>
          </>
        }
      >
        <div className="flex items-start gap-2 rounded-box border border-base-300 bg-base-100 p-3 text-sm text-base-content/70">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
          <div>掉级判断基于上次保存的信息和本次强制查询结果；首次查询没有历史快照时会标记为未知。</div>
        </div>
      </SectionCard>

      <SectionCard
        title="外部 JSON 校验"
        description="粘贴 Kiro Account Manager 或批量导入格式 JSON，只查询订阅和额度，不导入系统、不改变调度。"
        actions={
          <Button type="button" color="primary" size="sm" onClick={validateExternal} disabled={loading || parsedCount === 0}>
            {loading ? <Loading size="xs" /> : <FileSearch className="h-4 w-4" />}
            校验 JSON
          </Button>
        }
      >
        <div className="space-y-2">
          <Textarea bordered className="min-h-52 w-full font-mono text-xs" value={raw} onChange={(event) => setRaw(event.target.value)} placeholder="粘贴 KAM JSON、credentials 数组或 JSONL" />
          <div className="flex items-center justify-between text-xs text-base-content/60">
            <span>已解析 {parsedCount} 条可校验凭据</span>
            <span className="inline-flex items-center gap-1">
              <Upload className="h-3.5 w-3.5" />
              文件内容可直接粘贴到这里
            </span>
          </div>
        </div>
      </SectionCard>

      <SectionCard title="校验结果">
        <ValidationResults result={result} />
      </SectionCard>
    </div>
  )
}
