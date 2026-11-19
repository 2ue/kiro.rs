// 账号卡片：勾选、运行状态、可点改的数字、带悬停说明的 usage 摘要，常用操作直接放出，其余在 ⋯ 菜单里。
import { Fragment, type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import {
  Activity,
  BarChart3,
  Boxes,
  CopyPlus,
  Ellipsis,
  Eye,
  FlaskConical,
  ListPlus,
  Pencil,
  Power,
  RotateCcw,
  Route,
  Router,
  Sparkles,
  Trash2,
} from 'lucide-react'
import { ProgressRing } from '@/components/charts'
import {
  Badge,
  Button,
  Checkbox,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Tooltip,
} from '@/components/ui'
import { cn } from '@/lib/utils'
import type { Account, AccountQualityView, AccountsConfig, AccountStatus } from '@/types/api'
import { isAutoDisabledNow, skippedReasonLabel } from './account-list'
import type { QuickEditField } from './account-quick-edit'
import { accountUsageSummaryParts, type UsageSummaryPart } from './account-usage'
import {
  accountHeaderProfileSummary,
  accountModelMappingSummary,
  accountRouteSummary,
  accountSupportedModelsSummary,
  authLabel,
  streamRetrySummary,
} from './account-utils'

function qualitySummary(quality?: AccountQualityView): string | null {
  if (!quality) return null
  const parts = [
    `质量错误率 ${Math.round(quality.recentErrorRate * 100)}%`,
    quality.ttftEwmaMs != null ? `首字 ${Math.round(quality.ttftEwmaMs)}ms` : null,
    quality.latencyEwmaMs != null ? `总耗时 ${Math.round(quality.latencyEwmaMs)}ms` : null,
    `样本 ${quality.sampleCount}`,
  ].filter(Boolean) as string[]
  if (!quality.scoringActive) parts.push('冷启动')
  if (quality.inProbation) parts.push(`避让 ${quality.probationRemainingSecs}s`)
  else if (quality.recoveryProgress < 1) parts.push(`恢复 ${Math.round(quality.recoveryProgress * 100)}%`)
  return parts.join(' · ')
}

/** 卡片上可以直接点改的数值。 */
function QuickValue({ title, onClick, children }: { title: string; onClick: () => void; children: ReactNode }) {
  return (
    <Button
      variant="ghost"
      size="xs"
      title={title}
      aria-label={title}
      onClick={onClick}
      className="h-auto rounded px-1 py-0 text-sm font-medium tabular-nums text-foreground underline decoration-dotted decoration-muted-foreground/50 underline-offset-4 hover:bg-muted"
    >
      {children}
    </Button>
  )
}

/** usage 摘要：每段悬停说明含义、当前值来源和在哪里改；被单账号覆盖的段落标出来。 */
function UsageSummary({ parts }: { parts: UsageSummaryPart[] }) {
  return (
    <>
      {parts.map((part, index) => (
        <Fragment key={part.key}>
          {index > 0 && ' · '}
          <Tooltip
            label={(
              <span className="block max-w-sm space-y-1 whitespace-normal font-normal leading-5">
                <span className="block">{part.detail}</span>
                {part.global && (
                  <span className="block">
                    全局：
                    <Link to={part.global.to} className="underline underline-offset-2 hover:opacity-80">{part.global.label}</Link>
                  </span>
                )}
                <span className="block">单账号：{part.account}</span>
              </span>
            )}
          >
            <span tabIndex={0} className="cursor-help underline decoration-dotted decoration-muted-foreground/40 underline-offset-2 focus:outline-none focus-visible:ring-1 focus-visible:ring-ring">
              {part.text}
              {part.overridden && <span className="ml-0.5 text-info">（单账号覆盖）</span>}
            </span>
          </Tooltip>
        </Fragment>
      ))}
    </>
  )
}

export interface AccountCardProps {
  account: Account
  runtime?: AccountStatus
  config: AccountsConfig
  selected: boolean
  busy: boolean
  promoting: boolean
  onToggleSelect: () => void
  onEdit: () => void
  onCopy: () => void
  onTest: () => void
  onDiagnose: () => void
  onOpenUsage: () => void
  onShowEffective: () => void
  onToggleEnabled: () => void
  onQuickEdit: (field: QuickEditField) => void
  onDiscoverModels: () => void
  onPromoteModels: () => void
  onClearCooldown: () => void
  onClearAutoDisabled: () => void
  onDelete: () => void
}

export function AccountCard({
  account,
  runtime,
  config,
  selected,
  busy,
  promoting,
  onToggleSelect,
  onEdit,
  onCopy,
  onTest,
  onDiagnose,
  onOpenUsage,
  onShowEffective,
  onToggleEnabled,
  onQuickEdit,
  onDiscoverModels,
  onPromoteModels,
  onClearCooldown,
  onClearAutoDisabled,
  onDelete,
}: AccountCardProps) {
  const flying = runtime?.inFlight ?? 0
  const cap = account.maxConcurrentRequests
  const usePct = cap > 0 ? Math.round((flying / cap) * 100) : 0
  const autoDisabledNow = isAutoDisabledNow(account)
  const healthy = account.enabled && !autoDisabledNow
  const quality = qualitySummary(runtime?.quality)
  const cooling = (runtime?.cooldownRemainingSecs ?? 0) > 0
  const supportedCount = (account.supportedModels ?? []).length
  const routeLimited = Boolean(account.routeMode && account.routeMode !== 'allow_all')
  const usageParts = accountUsageSummaryParts(account, config)

  return (
    <div
      className={cn(
        'rounded-lg border border-border/70 bg-card p-4 shadow-sm transition hover:shadow-md',
        selected && 'border-primary/50 ring-1 ring-primary/30',
      )}
    >
      <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
        <div className="flex min-w-0 items-start gap-3">
          <Checkbox className="mt-1" checked={selected} onCheckedChange={onToggleSelect} aria-label={`选择账号 #${account.id}`} />
          <ProgressRing
            value={usePct}
            size={44}
            strokeWidth={4}
            color={healthy ? 'hsl(var(--success))' : 'hsl(var(--destructive))'}
            label={`${usePct}%`}
            className="mt-0.5 shrink-0"
          />
          <div className="min-w-0 space-y-1.5">
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-semibold">#{account.id} {account.name}</span>
              <Badge tone={account.enabled ? 'success' : 'neutral'}>{account.enabled ? '启用' : '停用'}</Badge>
              {autoDisabledNow && <Badge tone="error">自动禁用</Badge>}
              {account.autoDisabled && !autoDisabledNow && <Badge tone="neutral" title="自动禁用已到期，可清除标记">自动禁用已到期</Badge>}
              <Badge tone={runtime?.dispatchable ? 'info' : 'neutral'}>
                {runtime ? (runtime.dispatchable ? '可调度' : skippedReasonLabel(runtime.skippedReason)) : '状态未知'}
              </Badge>
              {(account.tags ?? []).map((tag) => <Badge key={tag} size="xs" tone="secondary">{tag}</Badge>)}
            </div>
            <div className="flex flex-wrap items-center gap-x-1 text-sm text-muted-foreground">
              <span className="break-all">{account.baseUrl}</span>
              <span>· {account.maskedApiKey || '未显示 Key'} · 并发</span>
              <QuickValue title="修改单账号最大并发" onClick={() => onQuickEdit('maxConcurrentRequests')}>{flying}/{cap}</QuickValue>
              <span>· 优先级</span>
              <QuickValue title="修改优先级" onClick={() => onQuickEdit('priority')}>{account.priority}</QuickValue>
              <span>· 成本</span>
              <QuickValue title="修改成本倍率" onClick={() => onQuickEdit('costMultiplier')}>×{account.costMultiplier}</QuickValue>
              {account.proxyUrl && <span>· 走代理</span>}
            </div>
            <div className="text-xs leading-5 text-muted-foreground">
              <UsageSummary parts={usageParts} /> · {streamRetrySummary(account, config)} · {accountRouteSummary(account)} · {accountHeaderProfileSummary(account)} · 认证：{authLabel(account.authType)} · 模型：{accountModelMappingSummary(account)} · {accountSupportedModelsSummary(account)}
              {cooling ? ` · 冷却 ${runtime!.cooldownRemainingSecs}s${runtime!.cooldownReason ? `（${runtime!.cooldownReason}）` : ''}` : ''}
              {runtime?.transientFailureStreak ? ` · 失败窗口 ${runtime.transientFailureStreak} 次/${runtime.transientFailureTtlSecs}s` : ''}
            </div>
            {quality && <div className="text-xs text-muted-foreground">{quality}</div>}
            {account.autoDisabledLastError && <div className="text-xs text-destructive">{account.autoDisabledLastError}</div>}
          </div>
        </div>
        <div className="flex flex-wrap gap-1.5 lg:shrink-0 lg:justify-end">
          <Button variant="ghost" size="xs" onClick={onEdit}><Pencil />编辑</Button>
          <Button variant="ghost" size="xs" title="复制全部配置（含密钥）为一个新账号，默认停用，复制后打开编辑" disabled={busy} onClick={onCopy}>
            <CopyPlus />复制
          </Button>
          <Button variant="ghost" size="xs" onClick={onTest}><FlaskConical />测试</Button>
          <Button variant="ghost" size="xs" onClick={onDiagnose} title="在账号风控页查看这个账号的缓存、成本、质量与异动"><Activity />诊断</Button>
          <Button variant="ghost" size="xs" onClick={onOpenUsage} title="查看这个账号的用量明细"><BarChart3 />用量</Button>
          <Button variant="ghost" size="xs" disabled={busy} onClick={onToggleEnabled}>
            <Power />{account.enabled ? '停用' : '启用'}
          </Button>
          {cooling && (
            <Button variant="ghost" size="xs" disabled={busy} onClick={onClearCooldown}><RotateCcw />清除冷却</Button>
          )}
          {account.autoDisabled && (
            <Button variant="ghost" size="xs" disabled={busy} onClick={onClearAutoDisabled}><RotateCcw />清除禁用</Button>
          )}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon-xs" title="更多操作" aria-label="更多操作"><Ellipsis /></Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-64">
              <DropdownMenuGroup>
                <DropdownMenuLabel className="text-xs text-muted-foreground">调度</DropdownMenuLabel>
                <DropdownMenuItem onSelect={() => onQuickEdit('route')}>
                  <Route />路由限制<span className="ml-auto text-xs text-muted-foreground">{routeLimited ? (account.routeMode === 'allow_list' ? '只允许' : '排除') : '全部入口'}</span>
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => onQuickEdit('supportedModels')}>
                  <Boxes />支持模型<span className="ml-auto text-xs text-muted-foreground">{supportedCount ? `${supportedCount} 个` : '不限制'}</span>
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => onQuickEdit('proxy')}>
                  <Router />代理<span className="ml-auto text-xs text-muted-foreground">{account.proxyUrl ? '已配置' : '未配置'}</span>
                </DropdownMenuItem>
              </DropdownMenuGroup>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuLabel className="text-xs text-muted-foreground">模型</DropdownMenuLabel>
                <DropdownMenuItem disabled={busy} onSelect={onDiscoverModels}><Sparkles />从上游发现模型</DropdownMenuItem>
                <DropdownMenuItem disabled={promoting} onSelect={onPromoteModels}><ListPlus />加入模型目录</DropdownMenuItem>
              </DropdownMenuGroup>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuLabel className="text-xs text-muted-foreground">查看与维护</DropdownMenuLabel>
                <DropdownMenuItem onSelect={onShowEffective}><Eye />生效参数</DropdownMenuItem>
                <DropdownMenuItem disabled={busy || !cooling} onSelect={onClearCooldown}><RotateCcw />清除冷却</DropdownMenuItem>
                <DropdownMenuItem disabled={busy || !account.autoDisabled} onSelect={onClearAutoDisabled}><RotateCcw />清除自动禁用</DropdownMenuItem>
              </DropdownMenuGroup>
              <DropdownMenuSeparator />
              <DropdownMenuItem destructive disabled={busy} onSelect={onDelete}><Trash2 />删除</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
    </div>
  )
}
