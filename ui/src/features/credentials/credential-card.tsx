import {
  Activity,
  Boxes,
  ChevronRight,
  CircleAlert,
  Coins,
  Ellipsis,
  Eraser,
  Flame,
  FlaskConical,
  Gauge,
  KeyRound,
  Pencil,
  RefreshCw,
  RotateCcw,
  Route,
  Router,
  ShieldOff,
  Snowflake,
  Timer,
  Trash2,
  Wallet,
  Globe,
  Zap,
  type LucideIcon,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  Badge,
  Button,
  Checkbox,
  SelectItem,
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Sheet,
  SheetBody,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
  Skeleton,
  Spinner,
  Switch,
  Tooltip,
} from '@/components/ui'
import { EmptyState, Field, LoadingState, MiddleTruncate, ModalShell, useConfirm } from '@/components/patterns'
import {
  formatCompact,
  formatFullDate,
  formatDateTime,
  formatMeteringUsage,
  formatNumber,
  formatPercent,
  formatQuotaFixed,
  formatUsd,
  formatUsdFixed2,
} from '@/lib/format'
import { cn, extractErrorMessage } from '@/lib/utils'
import {
  useClearInFlight,
  useCredentialDiagnostics,
  useDeleteCredential,
  useForceRefreshToken,
  useResetFailure,
  useRuntimeConfig,
  useSetCredentialConcurrency,
  useSetCredentialOverage,
  useSetCredentialRegions,
  useSetCredentialRpm,
  useSetCredentialRateLimitAutoDisable,
  useSetCredentialSupportedModels,
  useSetDisabled,
  useDiscoverCredentialSupportedModels,
  useSetPriority,
  useSetCredentialFast,
  useSetCredentialRoutePolicy,
  useSetWarmup,
} from '@/hooks/use-credentials'
import type { BalanceResponse, CredentialStatusItem, ExternalPoolRouteMode, UsageRecord } from '@/types/api'
import {
  accountInfoValue,
  authLabel,
  credentialFlags,
  credentialLabel,
  credentialRouteSummary,
  credentialStatusMeta,
  formatRemaining,
  endpointLabel,
  loadTone,
  numberOrZero,
  proxySummary,
  quotaBarModel,
  quotaReading,
  subscriptionBadgeMeta,
  successRate,
  type CredentialFlag,
  type CredentialStateKind,
  type CredentialStatusMeta,
  type CredentialSubscriptionKey,
} from './credential-utils'
import { CredentialProxyDialog } from './credential-proxy-dialog'
import { SelectBox } from '@/features/external-pools/external-pool-components'
import { SupportedModelTagsEditor, mergeSupportedModels } from '@/features/shared/supported-model-tags'
import { RouteSelector } from '@/features/route-scenarios/route-selector'
import { getRouteScenarioOptions } from '@/api/credentials'

// ============================================================================
// Sub-components
// ============================================================================

const QUOTA_TONE_CLASS = { success: 'bg-success', warning: 'bg-warning', error: 'bg-destructive' } as const

/** Plan usage, followed on the same track by overage in a separate colour once past the limit. */
export function QuotaBar({ model }: { model: ReturnType<typeof quotaBarModel> }) {
  return (
    <div
      role="progressbar"
      aria-label="额度使用进度"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(model.basePercent)}
      aria-valuetext={model.inOverage ? '基础额度已用完，正在使用超额' : `已用 ${Math.round(model.basePercent)}%`}
      className="mt-2 flex h-1.5 w-full overflow-hidden rounded-full bg-muted"
    >
      <div
        className={`h-full transition-[width] duration-300 ${QUOTA_TONE_CLASS[model.baseTone]} ${model.inOverage ? 'rounded-l-full' : 'rounded-full'}`}
        style={{ width: `${model.baseWidth}%` }}
      />
      {model.inOverage && (
        <div
          className="h-full rounded-r-full border-l border-card bg-violet-500 transition-[width] duration-300"
          style={{ width: `${model.overageWidth}%` }}
          title="超额部分"
        />
      )}
    </div>
  )
}

type MeterTone = 'warning' | 'full' | 'error'

// A full account is busy, not broken: it keeps the amber of "nearly full" instead of turning red.
const METER_TEXT_CLASS: Record<MeterTone, string> = { warning: 'text-warning', full: 'text-warning', error: 'text-destructive' }
const METER_FILL_CLASS: Record<MeterTone, string> = { warning: 'bg-warning', full: 'bg-warning', error: 'bg-destructive' }

/**
 * A stat cell: label above, value below; the value turns amber when it nears or reaches its limit
 * and red for a low success rate. Cells given `percent` also draw a thin bar of how full they are.
 * `percent: null` means the value has no limit: the bar's room is kept so cells in a row stay
 * aligned, but nothing is drawn.
 */
function Meter({
  label,
  value,
  tone,
  onClick,
  title,
  wrap,
  percent,
}: {
  label: string
  value: React.ReactNode
  tone?: MeterTone
  onClick?: () => void
  title?: string
  /** Let a long value break onto a second line instead of being cut off. */
  wrap?: boolean
  percent?: number | null
}) {
  const body = (
    <>
      <span className="block text-[0.7rem] leading-4 text-muted-foreground">{label}</span>
      <span className={cn('tabular mt-0.5 block text-xs font-semibold leading-4', wrap ? 'break-words' : 'truncate', tone ? METER_TEXT_CLASS[tone] : 'text-foreground')}>
        {value}
      </span>
      {percent !== undefined && (
        <span
          className={cn('mt-1.5 block h-1 overflow-hidden rounded-full', percent === null ? 'invisible' : 'bg-muted')}
          role={percent === null ? undefined : 'progressbar'}
          aria-label={percent === null ? undefined : `${label}占用`}
          aria-valuemin={percent === null ? undefined : 0}
          aria-valuemax={percent === null ? undefined : 100}
          aria-valuenow={percent === null ? undefined : Math.round(Math.min(100, Math.max(0, percent)))}
        >
          <span
            className={cn('block h-full rounded-full transition-[width] duration-300', tone ? METER_FILL_CLASS[tone] : 'bg-primary/60')}
            style={{ width: `${Math.min(100, Math.max(0, percent ?? 0))}%` }}
          />
        </span>
      )}
    </>
  )
  if (!onClick) return <div className="min-w-0" title={title}>{body}</div>
  return (
    <button type="button" onClick={onClick} title={title} aria-label={title}
      className="-m-1.5 block min-w-0 rounded-md p-1.5 text-left transition-colors hover:bg-muted/70 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
      {body}
    </button>
  )
}

/** Detail rows colour a full account like a nearly full one. */
const rowTone = (tone: ReturnType<typeof loadTone>) => (tone ? 'warning' : undefined)

/** One row in the detail sheet; editable rows show a pencil and open the matching dialog. */
function DetailRow({ label, children, onEdit, editLabel, tone }: {
  label: string
  children: React.ReactNode
  onEdit?: () => void
  editLabel?: string
  tone?: 'error' | 'warning'
}) {
  return (
    <div className="flex min-h-9 items-center justify-between gap-4 py-1.5">
      <dt className="shrink-0 text-xs text-muted-foreground">{label}</dt>
      <dd className={cn('flex min-w-0 items-center justify-end gap-1 text-right text-sm font-medium', tone === 'error' ? 'text-destructive' : tone === 'warning' ? 'text-warning' : 'text-foreground')}>
        <span className="min-w-0 break-words">{children}</span>
        {onEdit && (
          <Button variant="ghost" size="icon-xs" onClick={onEdit} aria-label={editLabel ?? `修改${label}`} title={editLabel ?? `修改${label}`} className="-mr-1.5 text-muted-foreground">
            <Pencil />
          </Button>
        )}
      </dd>
    </div>
  )
}

function DetailSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="py-3 first:pt-0">
      <h3 className="mb-1 text-[0.7rem] font-semibold uppercase tracking-wide text-muted-foreground">{title}</h3>
      <dl className="divide-y divide-border/60">{children}</dl>
    </section>
  )
}

/**
 * How each leading state is drawn. Errors are red with an alert icon; a cooldown is sky blue with a
 * snowflake; a full account (concurrency or RPM) is amber. Each of them also colours the card's top
 * edge, so a page of cards shows at a glance which accounts are broken and which are only waiting.
 */
const STATE_STYLE: Record<CredentialStateKind, {
  tone: 'neutral' | 'success' | 'warning' | 'error' | 'info'
  badgeClass?: string
  icon?: LucideIcon
  edgeClass?: string
}> = {
  normal: { tone: 'success' },
  probation: { tone: 'info' },
  disabled: { tone: 'neutral' },
  error: { tone: 'error', icon: CircleAlert, edgeClass: 'bg-destructive' },
  cooldown: {
    tone: 'info',
    badgeClass: 'bg-sky-500/15 text-sky-700 dark:text-sky-300',
    icon: Snowflake,
    edgeClass: 'bg-sky-500',
  },
  rate_limited: { tone: 'warning', badgeClass: 'bg-warning/15', icon: Timer, edgeClass: 'bg-warning' },
  concurrency_full: { tone: 'warning', badgeClass: 'bg-warning/15', icon: Gauge, edgeClass: 'bg-warning' },
}

/** Counts a snapshot's remaining seconds down every second until the next refresh replaces it. */
function useCountdown(secs: number | undefined): number | undefined {
  const [anchor, setAnchor] = useState(() => ({ secs, at: Date.now() }))
  const [, setTick] = useState(0)
  if (anchor.secs !== secs) setAnchor({ secs, at: Date.now() })
  const remaining = secs === undefined ? undefined : Math.max(0, secs - Math.floor((Date.now() - anchor.at) / 1000))
  const ticking = remaining !== undefined && remaining > 0
  useEffect(() => {
    if (!ticking) return
    const timer = window.setInterval(() => setTick((n) => n + 1), 1000)
    return () => window.clearInterval(timer)
  }, [ticking])
  return remaining
}

function StatusBadge({ status, remaining }: { status: CredentialStatusMeta; remaining?: number }) {
  const style = STATE_STYLE[status.kind]
  const Icon = style.icon
  const suffix = status.usage ?? (remaining !== undefined ? (remaining > 0 ? formatRemaining(remaining) : undefined) : undefined)
  return (
    <Badge tone={style.tone} dot={!Icon} className={style.badgeClass} title={status.title}>
      {Icon && <Icon className="size-3" aria-hidden="true" />}
      {status.label}
      {suffix && <span className="tabular">{suffix}</span>}
    </Badge>
  )
}

/** Each plan gets its own colour so a page of cards can be read by tier at a glance. */
const SUBSCRIPTION_CLASS: Record<CredentialSubscriptionKey, string> = {
  free: 'bg-muted text-muted-foreground',
  students: 'bg-sky-500/10 text-sky-700 dark:text-sky-300',
  pro: 'bg-primary/10 text-primary',
  pro_plus: 'bg-violet-500/10 text-violet-700 dark:text-violet-300',
  pro_max: 'bg-fuchsia-500/10 text-fuchsia-700 dark:text-fuchsia-300',
  power: 'bg-amber-500/15 text-amber-700 dark:text-amber-300',
  unknown: 'bg-muted text-muted-foreground',
}

const AUTH_CLASS: Record<string, string> = {
  social: 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300',
  idc: 'bg-blue-500/10 text-blue-700 dark:text-blue-300',
  external_idp: 'bg-cyan-500/10 text-cyan-700 dark:text-cyan-300',
  api_key: 'bg-orange-500/10 text-orange-700 dark:text-orange-300',
}

/** Icon and a one-word label for standing settings; the tooltip carries the full sentence. */
const CONFIG_FLAG: Partial<Record<CredentialFlag['key'], { icon: React.ComponentType<{ className?: string }>; short: string }>> = {
  route: { icon: Route, short: '路由' },
  proxy: { icon: Router, short: '代理' },
  models: { icon: Boxes, short: '模型' },
  'rate-limit': { icon: ShieldOff, short: '429仅冷却' },
  'profile-arn': { icon: KeyRound, short: 'ARN' },
}

function MetaItem({ label, value, detail, error }: {
  label: string; value: React.ReactNode; detail?: React.ReactNode; error?: boolean
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[0.68rem] font-medium text-muted-foreground">{label}</span>
      <span className={`text-sm font-semibold truncate ${error ? 'text-destructive' : ''}`}>{value}</span>
      {detail && <span className="text-xs text-muted-foreground truncate">{detail}</span>}
    </div>
  )
}

function supportedModelsSummary(models?: string[]) {
  if (!models || models.length === 0) return '不限制'
  if (models.length <= 2) return models.join(', ')
  return `${models[0]}, ${models[1]} 等 ${models.length} 个`
}

const EMPTY_SUPPORTED_MODELS: string[] = []

// ============================================================================
// CredentialCard exports
// ============================================================================

/** The responsive grid credential cards sit in; shared by the account page and the preview. */
export const CREDENTIAL_CARD_GRID = 'grid gap-3 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4'

export interface CredentialCardProps {
  credential: CredentialStatusItem
  selected: boolean
  onToggleSelect: () => void
  onQueryBalance: (id: number) => void
  onTest: (credential: CredentialStatusItem) => void
  balance?: BalanceResponse
  loadingBalance: boolean
  expanded: boolean
  onToggleExpanded: () => void
  showEmailSuffix: boolean
}

export function CredentialCard({
  credential, selected, onToggleSelect, onQueryBalance, onTest, balance, loadingBalance, expanded, onToggleExpanded, showEmailSuffix,
}: CredentialCardProps) {
  const supportedModels = credential.supportedModels ?? EMPTY_SUPPORTED_MODELS
  const [editingPriority, setEditingPriority] = useState(false)
  const [editingFast, setEditingFast] = useState(false)
  const [editingRoutePolicy, setEditingRoutePolicy] = useState(false)
  const scenarioOptions = useQuery({ queryKey: ['route-scenario-options'], queryFn: getRouteScenarioOptions, enabled: editingRoutePolicy })
  const routeOptions = scenarioOptions.data?.routes ?? []
  const [routeModeDraft, setRouteModeDraft] = useState<ExternalPoolRouteMode>(credential.routeMode ?? 'allow_all')
  const [routeRulesDraft, setRouteRulesDraft] = useState<string[]>(credential.routeRules ?? [])
  const [editingProxy, setEditingProxy] = useState(false)
  const [editingConcurrency, setEditingConcurrency] = useState(false)
  const [editingRpm, setEditingRpm] = useState(false)
  const [editingRegions, setEditingRegions] = useState(false)
  const [editingSupportedModels, setEditingSupportedModels] = useState(false)
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)
  const [showDiagnostics, setShowDiagnostics] = useState(false)
  const [diagnosticsPage, setDiagnosticsPage] = useState(1)
  const [diagnosticRecords, setDiagnosticRecords] = useState<UsageRecord[]>([])

  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [fastValue, setFastValue] = useState(String(credential.fast ?? 0))
  const [concurrencyValue, setConcurrencyValue] = useState(
    typeof credential.maxConcurrentRequestsOverride === 'number' ? String(credential.maxConcurrentRequestsOverride) : ''
  )
  const [rpmValue, setRpmValue] = useState(
    typeof credential.rpmOverride === 'number' ? String(credential.rpmOverride) : ''
  )
  const [regionValue, setRegionValue] = useState(credential.region || '')
  const [authRegionValue, setAuthRegionValue] = useState(credential.authRegion || '')
  const [apiRegionValue, setApiRegionValue] = useState(credential.apiRegion || '')
  const [supportedModelsDraft, setSupportedModelsDraft] = useState<string[]>(supportedModels)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const setFast = useSetCredentialFast()
  const setRoutePolicy = useSetCredentialRoutePolicy()
  const setCredentialConcurrency = useSetCredentialConcurrency()
  const setCredentialRpm = useSetCredentialRpm()
  const setCredentialRateLimitAutoDisable = useSetCredentialRateLimitAutoDisable()
  const setCredentialOverage = useSetCredentialOverage()
  const setCredentialRegions = useSetCredentialRegions()
  const setCredentialSupportedModels = useSetCredentialSupportedModels()
  const discoverCredentialSupportedModels = useDiscoverCredentialSupportedModels()
  const resetFailure = useResetFailure()
  const deleteCredential = useDeleteCredential()
  const setWarmup = useSetWarmup()
  const clearInFlight = useClearInFlight()
  const forceRefresh = useForceRefreshToken()
  const runtimeConfig = useRuntimeConfig()
  const diagnostics = useCredentialDiagnostics(credential.id, diagnosticsPage, showDiagnostics)
  const confirmDialog = useConfirm()

  const warmupTarget = Math.max(0, runtimeConfig.data?.credentialWarmupRequests ?? 3)
  const accountInfo = accountInfoValue(credential, balance)
  const probationRemainingSecs = numberOrZero(credential.probationRemainingSecs)
  const recentErrorRate = numberOrZero(credential.recentErrorRate)
  const schedulerScore = numberOrZero(credential.schedulerScore)
  const schedulerSelectionCount = numberOrZero(credential.schedulerSelectionCount)
  const recentSelection60s = numberOrZero(credential.recentSchedulerSelectionCount60s)
  const schedulerSelectionPressure = numberOrZero(credential.schedulerSelectionPressure)
  const subMeta = subscriptionBadgeMeta(credential, balance)
  const epLabel = endpointLabel(credential.endpoint)
  const hasFailures = credential.failureCount > 0 || credential.refreshFailureCount > 0
  const canClearInFlight = credential.inFlightRequests > 0
  const quotaBar = quotaBarModel(accountInfo)
  const overageStatus = (accountInfo?.overageStatus || '').toUpperCase()
  const overageCapability = (accountInfo?.overageCapability || '').toUpperCase()
  const overageEnabled = overageStatus === 'ENABLED'
  const overageCapable = overageCapability === '' || overageCapability === 'OVERAGE_CAPABLE'
  const overageLabel = accountInfo
    ? !overageCapable
      ? '不支持'
      : overageStatus === 'ENABLED'
        ? '已开启'
        : overageStatus === 'DISABLED'
          ? '已关闭'
          : '未知'
    : '未查询'
  const overageDetail = accountInfo
    ? [
        accountInfo.overageRate > 0 ? `单价 ${formatUsd(accountInfo.overageRate)}` : null,
        accountInfo.currentOverages > 0 ? `已用 ${formatQuotaFixed(accountInfo.currentOverages)}` : null,
      ].filter(Boolean).join(' · ') || undefined
    : '查询额度后同步状态'
  const displayCredentialLabel = credentialLabel(credential, showEmailSuffix)

  useEffect(() => { setPriorityValue(String(credential.priority)) }, [credential.priority])
  useEffect(() => { setFastValue(String(credential.fast ?? 0)) }, [credential.fast])
  useEffect(() => {
    setConcurrencyValue(typeof credential.maxConcurrentRequestsOverride === 'number' ? String(credential.maxConcurrentRequestsOverride) : '')
  }, [credential.id, credential.maxConcurrentRequestsOverride])
  useEffect(() => {
    setRpmValue(typeof credential.rpmOverride === 'number' ? String(credential.rpmOverride) : '')
  }, [credential.id, credential.rpmOverride])
  useEffect(() => {
    setRegionValue(credential.region || '')
    setAuthRegionValue(credential.authRegion || '')
    setApiRegionValue(credential.apiRegion || '')
  }, [credential.id, credential.region, credential.authRegion, credential.apiRegion])
  useEffect(() => {
    setSupportedModelsDraft(supportedModels)
  }, [credential.id, supportedModels])
  useEffect(() => {
    if (!showDiagnostics || !diagnostics.data) return
    setDiagnosticRecords((current) => diagnosticsPage === 1
      ? diagnostics.data.records
      : [...current, ...diagnostics.data.records.filter((record) => !current.some((item) => item.id === record.id))])
  }, [diagnostics.data, diagnosticsPage, showDiagnostics])
  useEffect(() => {
    if (!showDiagnostics) {
      setDiagnosticsPage(1)
      setDiagnosticRecords([])
    }
  }, [showDiagnostics])

  const savePriority = () => {
    const p = Number(priorityValue)
    if (!Number.isInteger(p) || p < 0) { toast.error('优先级必须是非负整数'); return }
    setPriority.mutate({ id: credential.id, priority: p }, {
      onSuccess: (res) => { toast.success(res.message); setEditingPriority(false) },
      onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
    })
  }

  const resetRoutePolicyDraft = () => {
    setRouteModeDraft(credential.routeMode ?? 'allow_all')
    setRouteRulesDraft(credential.routeRules ?? [])
  }

  const saveRoutePolicy = () => {
    const routeRules = routeModeDraft === 'allow_all' ? [] : routeRulesDraft
    if (routeModeDraft === 'allow_list' && routeRules.length === 0) {
      toast.error('只允许模式至少需要一条路由规则，否则这个账号不会承接任何请求')
      return
    }
    setRoutePolicy.mutate({ id: credential.id, request: { routeMode: routeModeDraft, routeRules } }, {
      onSuccess: (res) => { toast.success(res.message); setEditingRoutePolicy(false) },
      onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveFast = (value: string) => {
    const v = value.trim()
    const n = v ? Number(v) : 0
    if (!Number.isInteger(n) || n < 0) { toast.error('Fast 值必须是非负整数'); return }
    setFast.mutate({ id: credential.id, fast: n }, {
      onSuccess: (res) => { toast.success(res.message); setEditingFast(false) },
      onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveConcurrency = () => {
    const trimmed = concurrencyValue.trim()
    let maxConcurrentRequests: number | null = null
    if (trimmed) {
      const parsed = Number(trimmed)
      if (!Number.isInteger(parsed) || parsed < 0) { toast.error('并发限制必须是非负整数'); return }
      maxConcurrentRequests = parsed
    }
    setCredentialConcurrency.mutate({ id: credential.id, request: { maxConcurrentRequests } }, {
      onSuccess: (res) => { toast.success(res.message); setEditingConcurrency(false) },
      onError: (e) => toast.error(`并发设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveRpm = () => {
    const trimmed = rpmValue.trim()
    let rpm: number | null = null
    if (trimmed) {
      const parsed = Number(trimmed)
      if (!Number.isInteger(parsed) || parsed < 0) { toast.error('RPM 限制必须是非负整数'); return }
      rpm = parsed
    }
    setCredentialRpm.mutate({ id: credential.id, request: { rpm } }, {
      onSuccess: (res) => { toast.success(res.message); setEditingRpm(false) },
      onError: (e) => toast.error(`RPM 设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const toggleRateLimitAutoDisable = (enabled: boolean) => {
    setCredentialRateLimitAutoDisable.mutate({ id: credential.id, request: { enabled } }, {
      onSuccess: (res) => toast.success(res.message),
      onError: (e) => toast.error(`429 自动禁用设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const toggleOverage = (enabled: boolean) => {
    setCredentialOverage.mutate({ id: credential.id, enabled }, {
      onSuccess: () => toast.success(`超额调用已${enabled ? '开启' : '关闭'}`),
      onError: (e) => toast.error(`超额开关设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveRegions = () => {
    setCredentialRegions.mutate({
      id: credential.id,
      request: { region: regionValue.trim() || null, authRegion: authRegionValue.trim() || null, apiRegion: apiRegionValue.trim() || null },
    }, {
      onSuccess: (res) => { toast.success(res.message); setEditingRegions(false) },
      onError: (e) => toast.error(`Region 设置失败: ${extractErrorMessage(e)}`),
    })
  }

  const saveSupportedModels = () => {
    const supportedModels = mergeSupportedModels([], supportedModelsDraft)
    setCredentialSupportedModels.mutate({ id: credential.id, request: { supportedModels } }, {
      onSuccess: (res) => {
        toast.success(`已保存 ${res.count} 个支持模型`)
        setSupportedModelsDraft(res.supportedModels)
        setEditingSupportedModels(false)
      },
      onError: (e) => toast.error(`支持模型保存失败: ${extractErrorMessage(e)}`),
    })
  }

  const discoverSupportedModels = () => {
    discoverCredentialSupportedModels.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(`已生成 ${res.count} 个模型标签`)
        setSupportedModelsDraft(res.supportedModels)
      },
      onError: (e) => toast.error(`模型发现失败: ${extractErrorMessage(e)}`),
    })
  }

  const handleDelete = () => {
    if (!credential.disabled) { toast.error('请先禁用账号再删除'); return }
    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => { toast.success(res.message); setShowDeleteConfirm(false) },
      onError: (e) => toast.error(`删除失败: ${extractErrorMessage(e)}`),
    })
  }

  // --------------------------------------------------------------------------
  // Render
  // --------------------------------------------------------------------------
  const fast = credential.fast ?? 0
  const fastTier = fast > 0 ? String(Math.min(fast, 3)) : undefined
  const status = credentialStatusMeta(credential)
  const stateStyle = STATE_STYLE[status.kind]
  const remaining = useCountdown(status.remainingSecs)
  const flags = credentialFlags(credential)
  const attentionFlags = flags.filter((flag) => flag.kind === 'attention')
  const configFlags = flags.filter((flag) => flag.kind === 'config')
  const routeSummary = credentialRouteSummary(credential.routeMode, credential.routeRules)
  // A disabled account's numbers are history; fade them but keep the switch and actions at full strength.
  const dim = credential.disabled ? 'opacity-60' : undefined

  const concurrencyLimit = credential.maxConcurrentRequests
  const concurrencyText = `${credential.inFlightRequests}/${concurrencyLimit > 0 ? concurrencyLimit : '∞'}`
  const rpmLimit = credential.rpm
  const rpmText = `${formatCompact(recentSelection60s)}/${rpmLimit > 0 ? formatCompact(rpmLimit) : '∞'}`
  const quota = quotaReading(accountInfo, quotaBar)
  const success = successRate(credential.successCount, schedulerSelectionCount)
  const quotaToneClass = quotaBar.inOverage
    ? 'text-violet-600 dark:text-violet-400'
    : quotaBar.baseTone === 'error'
      ? 'text-destructive'
      : quotaBar.baseTone === 'warning'
        ? 'text-warning'
        : 'text-foreground'

  const toggleDisabled = () => setDisabled.mutate(
    { id: credential.id, disabled: !credential.disabled },
    {
      onSuccess: (res) => toast.success(res.message),
      onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
    }
  )
  const runForceRefresh = () => forceRefresh.mutate(credential.id, {
    onSuccess: (res) => toast.success(res.message),
    onError: (e) => toast.error(`刷新失败: ${extractErrorMessage(e)}`),
  })
  const runResetFailure = () => resetFailure.mutate(credential.id, {
    onSuccess: (res) => toast.success(res.message),
    onError: (e) => toast.error(`操作失败: ${extractErrorMessage(e)}`),
  })
  const toggleWarmup = () => setWarmup.mutate(
    { id: credential.id, warmupRemaining: credential.warmupRemaining > 0 ? 0 : Math.max(1, warmupTarget) },
    {
      onSuccess: () => toast.success(credential.warmupRemaining > 0 ? '已关闭预热' : '已开启预热'),
      onError: (e) => toast.error(`预热设置失败: ${extractErrorMessage(e)}`),
    }
  )
  const runClearInFlight = async () => {
    if (!canClearInFlight) return
    const ok = await confirmDialog({ title: '清理并发', message: `清理账号 #${credential.id} 当前并发占用？`, confirmText: '清理', tone: 'danger' })
    if (!ok) return
    clearInFlight.mutate({ id: credential.id }, {
      onSuccess: (res) => toast.success(res.message),
      onError: (e) => toast.error(`清理失败: ${extractErrorMessage(e)}`),
    })
  }
  const openDiagnostics = () => { setDiagnosticsPage(1); setDiagnosticRecords([]); setShowDiagnostics(true) }
  const openFlag = (key: CredentialFlag['key']) => {
    if (key === 'route') { resetRoutePolicyDraft(); setEditingRoutePolicy(true) }
    else if (key === 'proxy') { setEditingProxy(true) }
    else if (key === 'models') setEditingSupportedModels(true)
    else if (key === 'errors') openDiagnostics()
    else onToggleExpanded()
  }

  return (
    <article
      data-fast-tier={fastTier}
      aria-label={`账号 ${displayCredentialLabel}`}
      data-selected={selected || undefined}
      className={cn(
        'group/card relative flex h-full flex-col overflow-hidden rounded-xl bg-card shadow-sm transition-shadow hover:shadow-md',
        // Exactly one edge: Fast cards draw a gradient rim, the rest a plain border.
        fast > 0 ? 'fast-card' : cn('border', selected ? 'border-primary ring-1 ring-primary' : 'border-border/70'),
      )}>
      {stateStyle.edgeClass && <span aria-hidden="true" className={cn('pointer-events-none absolute inset-x-0 top-0 h-1', stateStyle.edgeClass)} />}
      {/* ── Header: selection, state and the one switch that matters ── */}
      <div className="flex items-center gap-2 px-4 pt-4">
        <Checkbox checked={selected} onCheckedChange={onToggleSelect} aria-label={`选择 ${displayCredentialLabel}`} />
        <StatusBadge status={status} remaining={remaining} />
        {fast > 0 && (
          <button type="button" className="fast-badge" onClick={() => setEditingFast(true)}
            title={`Fast ${fast}：优先打满这个账号；点击修改`} aria-label={`Fast ${fast}，点击修改`}>
            <Zap className="size-3" aria-hidden="true" fill="currentColor" />FAST {fast}
          </button>
        )}
        <span className="tabular ml-auto font-mono text-xs text-muted-foreground">#{credential.id}</span>
        <Switch checked={!credential.disabled} disabled={setDisabled.isPending} onCheckedChange={toggleDisabled}
          aria-label={credential.disabled ? '启用账号' : '禁用账号'} title={credential.disabled ? '启用账号' : '禁用账号'} />
      </div>

      {/* ── Identity: one line, the middle elided with *** when it does not fit; plan and login as coloured chips ── */}
      <div className={cn('px-4 pt-2', dim)}>
        <button type="button" onClick={onToggleExpanded} title={`${displayCredentialLabel}：查看详情`}
          className="-mx-1.5 block w-[calc(100%+0.75rem)] rounded-md px-1.5 py-0.5 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
          <MiddleTruncate text={displayCredentialLabel} className="text-[0.95rem] font-semibold leading-6 text-foreground" />
        </button>
        <div className="mt-1.5 flex flex-wrap items-center gap-1">
          <span className={cn('inline-flex h-5 items-center rounded-full px-2 text-[0.68rem] font-semibold', SUBSCRIPTION_CLASS[subMeta.key])} title={subMeta.title}>
            {loadingBalance ? '…' : subMeta.label}
          </span>
          <span className={cn('inline-flex h-5 items-center rounded-full px-2 text-[0.68rem] font-semibold', AUTH_CLASS[credential.authMethod ?? ''] ?? 'bg-muted text-muted-foreground')}>
            {authLabel(credential.authMethod)}
          </span>
          {epLabel && (
            <span className="inline-flex h-5 items-center rounded-full border border-border px-2 text-[0.68rem] font-medium text-muted-foreground" title={credential.endpoint}>
              {epLabel}
            </span>
          )}
        </div>
      </div>

      {/* ── Quota: used / limit with the share beside it; overage spelled out when it happens ── */}
      <div className={cn('px-4 pt-3', dim)}>
        <div className="flex flex-wrap items-baseline gap-x-1.5 text-xs leading-5">
          <span className="mr-auto shrink-0 text-muted-foreground">额度</span>
          {loadingBalance ? (
            <Skeleton className="h-4 w-28" />
          ) : accountInfo && accountInfo.usageLimit > 0 && quota ? (
            // Percent and amount wrap as separate pieces, so a long overage reading never spills out of the card.
            <span className="tabular ml-auto flex flex-wrap items-baseline justify-end gap-x-1.5 text-right">
              <span className={cn('whitespace-nowrap font-semibold', quotaToneClass)}>{quota.percent}</span>
              <span className="whitespace-nowrap font-medium text-foreground">{quota.amount}</span>
            </span>
          ) : (
            <button type="button" onClick={() => onQueryBalance(credential.id)} className="font-medium text-primary hover:underline">
              未查询，点击查询
            </button>
          )}
        </div>
        <QuotaBar model={quotaBar} />
      </div>

      {/* ── Billing: credits used, estimated and original cost, always visible ── */}
      <dl className={cn('grid grid-cols-3 gap-3 px-4 pt-3', dim)}>
        <div className="min-w-0">
          <dt className="text-[0.7rem] leading-4 text-muted-foreground">使用积分</dt>
          <dd className="tabular mt-0.5 truncate text-xs font-semibold leading-4 text-foreground" title={formatMeteringUsage(credential.kiroMeteringUsage)}>
            {formatMeteringUsage(credential.kiroMeteringUsage)}
          </dd>
        </div>
        <div className="min-w-0">
          <dt className="text-[0.7rem] leading-4 text-muted-foreground">估算费用</dt>
          <dd className="tabular mt-0.5 truncate text-xs font-semibold leading-4 text-foreground">{formatUsdFixed2(credential.estimatedCostUsd)}</dd>
        </div>
        <div className="min-w-0">
          <dt className="text-[0.7rem] leading-4 text-muted-foreground">原始费用</dt>
          <dd className="tabular mt-0.5 truncate text-xs font-semibold leading-4 text-foreground">{formatUsdFixed2(credential.originalCostUsd)}</dd>
        </div>
      </dl>

      {/* ── Load: what the scheduler is doing with it right now ── */}
      {/* Priority is a short number; success/dispatch needs the room for two counts. */}
      <div className={cn('grid grid-cols-[1fr_1fr_0.7fr_1.6fr] items-start gap-x-2 px-4 pt-3.5', dim)}>
        <Meter label="并发" value={concurrencyText}
          percent={concurrencyLimit > 0 ? (credential.inFlightRequests / concurrencyLimit) * 100 : null}
          tone={loadTone(credential.inFlightRequests, concurrencyLimit)}
          onClick={() => setEditingConcurrency(true)} title="修改并发限制" />
        <Meter label="RPM" value={rpmText}
          percent={rpmLimit > 0 ? (recentSelection60s / rpmLimit) * 100 : null}
          tone={loadTone(recentSelection60s, rpmLimit)}
          onClick={() => setEditingRpm(true)} title="修改 RPM 限制" />
        <Meter label="优先级" value={credential.priority} percent={null} onClick={() => setEditingPriority(true)} title="修改优先级" />
        <Meter label="成功/调度"
          wrap
          percent={null}
          value={<>{formatCompact(credential.successCount)}/<wbr />{formatCompact(success.dispatched)}</>}
          // Only a low rate is worth colouring; a healthy one stays neutral like the other values.
          tone={success.tone === 'success' ? undefined : success.tone}
          title={`成功 ${formatNumber(credential.successCount)} / 调度 ${formatNumber(success.dispatched)}${success.percent !== undefined ? `，成功率 ${formatPercent(success.percent / 100)}` : ''}`} />
      </div>

      {/* ── Flags: attention first in words, standing settings as icons; absent when there are none ── */}
      {(attentionFlags.length > 0 || configFlags.length > 0 || overageEnabled) && (
      <div className="flex flex-wrap items-center gap-1.5 px-4 pt-3">
        {attentionFlags.map((flag) => (
          <button key={flag.key} type="button" onClick={() => openFlag(flag.key)} title={flag.title}>
            <Badge tone={flag.tone} className="cursor-pointer">{flag.label}</Badge>
          </button>
        ))}
        {overageEnabled && (
          <Tooltip label={overageDetail ? `超额调用已开启：${overageDetail}` : '超额调用已开启'}>
            <span tabIndex={0} className="inline-flex h-5 items-center gap-1 rounded-full bg-violet-500/10 px-2 text-[0.68rem] font-medium text-violet-700 dark:text-violet-300">
              <Coins className="size-3" aria-hidden="true" />超额
            </span>
          </Tooltip>
        )}
        {configFlags.map((flag) => {
          const meta = CONFIG_FLAG[flag.key]
          const Icon = meta?.icon
          return (
            <Tooltip key={flag.key} label={flag.title ?? flag.label}>
              <button type="button" onClick={() => openFlag(flag.key)} aria-label={flag.title ?? flag.label}
                className={cn(
                  'inline-flex h-5 items-center gap-1 rounded-full px-2 text-[0.68rem] font-medium transition-colors hover:brightness-95',
                  flag.tone === 'warning' ? 'bg-warning/10 text-warning' : flag.tone === 'info' ? 'bg-info/10 text-info' : 'bg-muted text-muted-foreground',
                )}>
                {Icon && <Icon className="size-3" aria-hidden="true" />}
                <span>{meta?.short ?? flag.label}</span>
                {flag.count !== undefined && <span className="tabular">{flag.count}</span>}
              </button>
            </Tooltip>
          )
        })}
      </div>
      )}

      {/* ── Times: when the account was added and when it last served a request successfully ── */}
      {/* Pinned with the action bar, so the times of cards in a row line up whether or not they have flags. */}
      <div className="tabular mt-auto grid grid-cols-2 gap-x-2 px-4 pb-3 pt-2.5 text-[0.7rem] leading-4 text-muted-foreground">
        {/* Label above value keeps both on fixed lines, so a date with a year never pushes the row taller. */}
        <span className="min-w-0" title={credential.createdAt ? `创建时间：${formatFullDate(credential.createdAt)}` : undefined}>
          <span className="block">创建</span>
          <span className="block truncate text-foreground/80">{formatDateTime(credential.createdAt)}</span>
        </span>
        <span className="min-w-0 text-right"
          // `lastUsedAt` moves only when a request on this account succeeds.
          title={credential.lastUsedAt ? `最近一次成功请求：${formatFullDate(credential.lastUsedAt)}` : '还没有成功请求'}>
          <span className="block">最近成功</span>
          <span className="block truncate text-foreground/80">{credential.lastUsedAt ? formatDateTime(credential.lastUsedAt) : '未成功过'}</span>
        </span>
      </div>

      {/* ── Actions: pinned to the bottom so every card in a row lines up ── */}
      <div className="flex items-center gap-0.5 border-t border-border/60 px-2 py-1.5">
        <Button variant="ghost" size="xs" onClick={() => onTest(credential)}>
          <FlaskConical />测试
        </Button>
        <Button variant="ghost" size="xs" onClick={() => onQueryBalance(credential.id)} disabled={loadingBalance}>
          {loadingBalance ? <Spinner size="sm" /> : <Wallet />}额度
        </Button>
        <Button variant="ghost" size="xs" onClick={openDiagnostics} title="查看账号调度与错误诊断">
          <Activity />诊断
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon-xs" title="更多操作" aria-label="更多操作">
              <Ellipsis />
            </Button>
          </DropdownMenuTrigger>
          {/* Only what the card has no direct entry for: concurrency, RPM and priority are the stat cells,
              test / quota / diagnostics the buttons, and Fast, route, models and proxy their chips once set. */}
          <DropdownMenuContent align="start" className="w-60">
            <DropdownMenuGroup>
              <DropdownMenuLabel className="text-xs text-muted-foreground">调度</DropdownMenuLabel>
              {fast === 0 && (
                <DropdownMenuItem onSelect={() => setEditingFast(true)}>
                  <Zap /> 开启 Fast
                </DropdownMenuItem>
              )}
              {!routeSummary && (
                <DropdownMenuItem onSelect={() => { resetRoutePolicyDraft(); setEditingRoutePolicy(true) }}>
                  <Route /> 路由限制<span className="ml-auto text-xs text-muted-foreground">全部入口</span>
                </DropdownMenuItem>
              )}
              {supportedModels.length === 0 && (
                <DropdownMenuItem onSelect={() => setEditingSupportedModels(true)}>
                  <Boxes /> 支持模型<span className="ml-auto text-xs text-muted-foreground">不限制</span>
                </DropdownMenuItem>
              )}
              <DropdownMenuItem disabled={setWarmup.isPending} onSelect={toggleWarmup}>
                <Flame /> {credential.warmupRemaining > 0 ? '关闭预热' : '开启预热'}
              </DropdownMenuItem>
              <DropdownMenuCheckboxItem
                checked={credential.rateLimitAutoDisableEnabled !== false}
                disabled={setCredentialRateLimitAutoDisable.isPending}
                onCheckedChange={(checked) => toggleRateLimitAutoDisable(checked === true)}>
                429 自动禁用
              </DropdownMenuCheckboxItem>
              <DropdownMenuCheckboxItem
                checked={overageEnabled}
                disabled={!accountInfo || !overageCapable || setCredentialOverage.isPending}
                onCheckedChange={(checked) => toggleOverage(checked === true)}>
                超额调用<span className="ml-auto pl-2 text-xs text-muted-foreground">{overageLabel}</span>
              </DropdownMenuCheckboxItem>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel className="text-xs text-muted-foreground">网络</DropdownMenuLabel>
              {!credential.hasProxy && (
                <DropdownMenuItem onSelect={() => { setEditingProxy(true) }}>
                  <Router /> 代理<span className="ml-auto max-w-24 truncate text-xs text-muted-foreground">{proxySummary(credential)}</span>
                </DropdownMenuItem>
              )}
              <DropdownMenuItem onSelect={() => setEditingRegions(true)}>
                <Globe /> Region<span className="ml-auto font-mono text-xs text-muted-foreground">{credential.effectiveApiRegion || '-'}</span>
              </DropdownMenuItem>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel className="text-xs text-muted-foreground">维护</DropdownMenuLabel>
              <DropdownMenuItem disabled={forceRefresh.isPending || credential.authMethod === 'api_key'} onSelect={runForceRefresh}>
                <RefreshCw /> {credential.authMethod === 'api_key' ? '刷新 Token（API Key 无需）' : '刷新 Token'}
              </DropdownMenuItem>
              <DropdownMenuItem disabled={resetFailure.isPending || !hasFailures} onSelect={runResetFailure}>
                <RotateCcw /> 恢复异常
              </DropdownMenuItem>
              <DropdownMenuItem disabled={clearInFlight.isPending || !canClearInFlight} onSelect={runClearInFlight}>
                <Eraser /> 清理并发
              </DropdownMenuItem>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuItem destructive disabled={!credential.disabled} onSelect={() => { if (credential.disabled) setShowDeleteConfirm(true) }}>
              <Trash2 /> {credential.disabled ? '删除' : '删除（需先禁用）'}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <Button variant="ghost" size="xs" className="ml-auto text-muted-foreground" onClick={onToggleExpanded} aria-haspopup="dialog" title="查看全部信息">
          详情<ChevronRight />
        </Button>
      </div>

      {/* ── Detail sheet: everything else, grouped, with edit entry points ── */}
      <Sheet open={expanded} onOpenChange={(open) => { if (open !== expanded) onToggleExpanded() }}>
        <SheetContent width="sm:max-w-md">
          <SheetHeader>
            <div className="flex items-center gap-2">
              <StatusBadge status={status} remaining={remaining} />
              <span className="tabular font-mono text-xs text-muted-foreground">#{credential.id}</span>
            </div>
            <SheetTitle className="break-all">{displayCredentialLabel}</SheetTitle>
            <SheetDescription className="flex flex-wrap items-center gap-1">
              <span className={cn('inline-flex h-5 items-center rounded-full px-2 text-[0.68rem] font-semibold', SUBSCRIPTION_CLASS[subMeta.key])}>{subMeta.label}</span>
              <span className={cn('inline-flex h-5 items-center rounded-full px-2 text-[0.68rem] font-semibold', AUTH_CLASS[credential.authMethod ?? ''] ?? 'bg-muted text-muted-foreground')}>{authLabel(credential.authMethod)}</span>
              {epLabel && <span className="inline-flex h-5 items-center rounded-full border border-border px-2 text-[0.68rem] font-medium">{epLabel}</span>}
            </SheetDescription>
          </SheetHeader>
          <SheetBody className="divide-y divide-border/70">
            {credential.lastErrorReason && (
              <div className="mb-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs">
                <div className="font-semibold text-destructive">最近错误{credential.lastErrorAtMs ? `（${formatDateTime(credential.lastErrorAtMs)}）` : ''}</div>
                <div className="mt-1 break-words text-muted-foreground">{credential.lastErrorKind}: {credential.lastErrorReason}</div>
              </div>
            )}

            <DetailSection title="调度">
              <DetailRow label="优先级" onEdit={() => setEditingPriority(true)}>{credential.priority}</DetailRow>
              <DetailRow label="Fast" onEdit={() => setEditingFast(true)}>{fast > 0 ? fast : '未开启'}</DetailRow>
              <DetailRow label="并发" onEdit={() => setEditingConcurrency(true)} tone={rowTone(loadTone(credential.inFlightRequests, concurrencyLimit))}>
                {concurrencyText}
                {credential.inFlightRequests > 0 && (
                  <span className="ml-1 text-xs font-normal text-muted-foreground">最老 {credential.oldestInFlightAgeSecs}s · 闲置 {credential.newestInFlightIdleSecs}s</span>
                )}
              </DetailRow>
              <DetailRow label="RPM（近 60 秒）" onEdit={() => setEditingRpm(true)} tone={rowTone(loadTone(recentSelection60s, rpmLimit))}>{rpmText}</DetailRow>
              <DetailRow label="路由限制" onEdit={() => { resetRoutePolicyDraft(); setEditingRoutePolicy(true) }} editLabel="修改路由限制">
                {routeSummary ?? '全部入口'}
              </DetailRow>
              <DetailRow label="支持模型" onEdit={() => setEditingSupportedModels(true)}>
                <span title={supportedModels.join('\n') || undefined}>{supportedModelsSummary(supportedModels)}</span>
              </DetailRow>
              <DetailRow label="429 自动禁用">
                <span className="inline-flex items-center gap-2">
                  <span className="text-xs font-normal text-muted-foreground">{credential.rateLimitAutoDisableEnabled === false ? '仅冷却' : '开启'}</span>
                  <Switch checked={credential.rateLimitAutoDisableEnabled !== false} disabled={setCredentialRateLimitAutoDisable.isPending}
                    onCheckedChange={toggleRateLimitAutoDisable} aria-label="429 自动禁用" />
                </span>
              </DetailRow>
              {credential.warmupRemaining > 0 && <DetailRow label="预热剩余" tone="warning">{credential.warmupRemaining} 次</DetailRow>}
              {credential.inProbation && <DetailRow label="观察期">{probationRemainingSecs}s</DetailRow>}
            </DetailSection>

            <DetailSection title="运行">
              <DetailRow label="成功 / 调度" tone={success.tone === 'success' ? undefined : success.tone}>
                {formatNumber(credential.successCount)} / {formatNumber(success.dispatched)}
                {success.percent !== undefined && <span className="ml-1 text-xs font-normal text-muted-foreground">（{formatPercent(success.percent / 100)}）</span>}
              </DetailRow>
              <DetailRow label="连续失败 / 刷新失败" tone={hasFailures ? 'error' : undefined}>
                {credential.failureCount} / {credential.refreshFailureCount}
              </DetailRow>
              <DetailRow label="近期错误率" tone={recentErrorRate > 0 ? 'error' : undefined}>{formatPercent(recentErrorRate)}</DetailRow>
              <DetailRow label="调度评分（压力）" tone={schedulerSelectionPressure > 1 ? 'error' : undefined}>
                {schedulerScore.toFixed(2)}（{schedulerSelectionPressure.toFixed(2)}）
              </DetailRow>
              <DetailRow label="最近成功">{credential.lastUsedAt ? formatDateTime(credential.lastUsedAt) : '未成功过'}</DetailRow>
              {credential.cooldownReason && <DetailRow label="冷却原因">{credential.cooldownReason}</DetailRow>}
              {credential.cooldowns && credential.cooldowns.length > 0 && (
                <div className="flex flex-wrap gap-1.5 py-2">
                  {credential.cooldowns.map((cd) => (
                    <Badge key={`${cd.global ? 'global' : cd.model}-${cd.remainingSecs}`} className={STATE_STYLE.cooldown.badgeClass} title={cd.reason || undefined}>
                      <Snowflake className="size-3" aria-hidden="true" />
                      {cd.global ? '全部模型' : cd.model || '-'} · {formatRemaining(cd.remainingSecs)}
                    </Badge>
                  ))}
                </div>
              )}
            </DetailSection>

            <DetailSection title="额度与计费">
              <DetailRow label="已用 / 上限" tone={quotaBar.inOverage ? 'error' : undefined}>
                {quota ? `${quota.percent}  ${quota.amount}` : '未查询'}
              </DetailRow>
              <DetailRow label="超额调用">
                <span className="inline-flex items-center gap-2">
                  <span className={cn('text-xs font-normal', overageEnabled ? 'text-warning' : 'text-muted-foreground')}>{overageLabel}</span>
                  {setCredentialOverage.isPending ? <Spinner size="sm" /> : (
                    <Switch checked={overageEnabled} disabled={!accountInfo || !overageCapable} onCheckedChange={toggleOverage} aria-label="切换超额调用" />
                  )}
                </span>
              </DetailRow>
              {overageDetail && <DetailRow label="超额说明">{overageDetail}</DetailRow>}
              <DetailRow label="估算 / 原始计费">{formatUsdFixed2(credential.estimatedCostUsd)} / {formatUsdFixed2(credential.originalCostUsd)}</DetailRow>
              <DetailRow label="使用积分">{formatMeteringUsage(credential.kiroMeteringUsage)}</DetailRow>
              {(credential.pricedRequests > 0 || credential.unpricedRequests > 0) && (
                <DetailRow label="计价请求覆盖" tone={credential.unpricedRequests > 0 ? 'warning' : undefined}>
                  {formatNumber(credential.pricedRequests)} / {formatNumber(credential.pricedRequests + credential.unpricedRequests)}
                </DetailRow>
              )}
            </DetailSection>

            <DetailSection title="网络">
              <DetailRow label="Auth Region" onEdit={() => setEditingRegions(true)} editLabel="修改 Region">
                <span className="font-mono">{credential.effectiveAuthRegion || '-'}</span>
              </DetailRow>
              <DetailRow label="API Region" onEdit={() => setEditingRegions(true)} editLabel="修改 Region">
                <span className="font-mono">{credential.effectiveApiRegion || '-'}</span>
              </DetailRow>
              <DetailRow label="代理" onEdit={() => { setEditingProxy(true) }} editLabel="修改代理">{proxySummary(credential)}</DetailRow>
            </DetailSection>

            <DetailSection title="账号">
              {credential.email && credential.maskedApiKey && (
                <DetailRow label="API Key"><span className="font-mono">{credential.maskedApiKey}</span></DetailRow>
              )}
              {credential.hasProfileArn && <DetailRow label="Profile ARN">已配置</DetailRow>}
              <DetailRow label="创建时间">{formatDateTime(credential.createdAt)}</DetailRow>
              <DetailRow label="更新时间">{formatDateTime(credential.updatedAt)}</DetailRow>
            </DetailSection>
          </SheetBody>
          <SheetFooter>
            <Button variant="outline" size="sm" onClick={() => onTest(credential)}><FlaskConical />测试</Button>
            <Button variant="outline" size="sm" onClick={() => onQueryBalance(credential.id)} disabled={loadingBalance}>
              {loadingBalance ? <Spinner size="sm" /> : <Wallet />}查询额度
            </Button>
            <Button variant="outline" size="sm" onClick={openDiagnostics}><Activity />诊断</Button>
            <Button variant="outline" size="sm" className="ml-auto" onClick={runForceRefresh}
              disabled={forceRefresh.isPending || credential.authMethod === 'api_key'}>
              <RefreshCw className={forceRefresh.isPending ? 'animate-spin' : undefined} />刷新 Token
            </Button>
          </SheetFooter>
        </SheetContent>
      </Sheet>

      {/* ── Route Policy Modal ── */}
      <ModalShell open={editingRoutePolicy} title={`路由限制：${displayCredentialLabel}`} width="max-w-lg"
        onClose={() => { setEditingRoutePolicy(false); resetRoutePolicyDraft() }}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
            控制哪些入口路由可以调度到这个账号。被排除的路由不会用这个账号，先于 Fast 和优先级生效。
            如果某个路由所有本地账号都被排除，按外部池的路由规则决定是否转外部池。要整套切换多个账号的规则，用路由场景。
          </div>
          <div className="grid gap-3 @lg:grid-cols-[200px_1fr]">
            <SelectBox
              label="路由模式"
              value={routeModeDraft}
              disabled={setRoutePolicy.isPending}
              onChange={(v) => setRouteModeDraft(v as ExternalPoolRouteMode)}
            >
              <SelectItem value="allow_all">全部入口允许</SelectItem>
              <SelectItem value="allow_list">只允许下列入口</SelectItem>
              <SelectItem value="deny_list">禁止下列入口</SelectItem>
            </SelectBox>
            <div className="min-w-0 space-y-1.5">
              <div className="text-sm font-medium">路由</div>
              <RouteSelector
                routes={routeOptions}
                value={routeRulesDraft}
                disabled={setRoutePolicy.isPending || routeModeDraft === 'allow_all'}
                onChange={setRouteRulesDraft}
              />
            </div>
          </div>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => { setEditingRoutePolicy(false); resetRoutePolicyDraft() }} disabled={setRoutePolicy.isPending}>取消</Button>
            <Button size="sm" onClick={saveRoutePolicy} disabled={setRoutePolicy.isPending}>
              {setRoutePolicy.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Fast Modal ── */}
      <ModalShell open={editingFast} title={`Fast：${displayCredentialLabel}`} width="max-w-md"
        onClose={() => { setEditingFast(false); setFastValue(String(fast)) }}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前 Fast</span>
              <span className="font-semibold">{fast > 0 ? fast : '未开启'}</span>
            </div>
            <div className="mt-1 text-xs text-muted-foreground">
              开启后，请求会先集中到这个账号，并发用满后再分给其他账号。数值越大越先打满（和优先级相反），0 表示关闭。
              已有会话会逐步切过来，不会强行中断；冷却、限流、并发上限等安全规则照常生效。
            </div>
          </div>
          <Field label="Fast 值" description="0 或留空表示关闭">
            <Input type="number" min={0} value={fastValue} disabled={setFast.isPending}
              onChange={(e) => setFastValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            {fast > 0 && (
              <Button variant="ghost" size="sm" onClick={() => saveFast('0')} disabled={setFast.isPending}>关闭 Fast</Button>
            )}
            <Button size="sm" onClick={() => saveFast(fastValue)} disabled={setFast.isPending}>
              {setFast.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Priority Modal ── */}
        <ModalShell open={editingPriority} title={`优先级：${displayCredentialLabel}`} width="max-w-md"
        onClose={() => { setEditingPriority(false); setPriorityValue(String(credential.priority)) }}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前优先级</span>
              <span className="font-semibold">{credential.priority}</span>
            </div>
            <div className="mt-1 text-xs text-muted-foreground">数值越小优先级越高；不能小于 0。</div>
          </div>
          <Field label="新优先级">
            <Input type="number" min={0} value={priorityValue} disabled={setPriority.isPending}
              onChange={(e) => setPriorityValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => { setEditingPriority(false); setPriorityValue(String(credential.priority)) }} disabled={setPriority.isPending}>取消</Button>
            <Button size="sm" onClick={savePriority} disabled={setPriority.isPending}>
              {setPriority.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Concurrency Modal ── */}
      <ModalShell open={editingConcurrency} title={`并发限制：${displayCredentialLabel}`} width="max-w-lg"
        onClose={() => setEditingConcurrency(false)}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前生效</span>
              <span className="font-semibold">{credential.maxConcurrentRequests > 0 ? `${credential.maxConcurrentRequests} 并发` : '不限'}</span>
            </div>
          </div>
          <Field label="账号级最大并发" description="留空继承全局；0 表示该账号不限；正整数设置上限。">
            <Input type="number" min={0} value={concurrencyValue} placeholder="留空继承全局，0 表示不限"
              disabled={setCredentialConcurrency.isPending} onChange={(e) => setConcurrencyValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setEditingConcurrency(false)} disabled={setCredentialConcurrency.isPending}>取消</Button>
            <Button variant="ghost" size="sm"
              disabled={setCredentialConcurrency.isPending || typeof credential.maxConcurrentRequestsOverride !== 'number'}
              onClick={() => {
                setConcurrencyValue('')
                setCredentialConcurrency.mutate({ id: credential.id, request: { maxConcurrentRequests: null } }, {
                  onSuccess: (res) => { toast.success(res.message); setEditingConcurrency(false) },
                  onError: (e) => toast.error(`设置失败: ${extractErrorMessage(e)}`),
                })
              }}>继承全局</Button>
            <Button size="sm" onClick={saveConcurrency} disabled={setCredentialConcurrency.isPending}>
              {setCredentialConcurrency.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── RPM Modal ── */}
      <ModalShell open={editingRpm} title={`RPM 限制：${displayCredentialLabel}`} width="max-w-lg"
        onClose={() => setEditingRpm(false)}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前生效</span>
              <span className="font-semibold">
                {credential.rpm != null
                  ? credential.rpm === 0 ? '不限制' : `${credential.rpm} 次/分钟`
                  : '-'}
              </span>
            </div>
          </div>
          <Field label="账号级 RPM 限制" description="留空继承全局；0 表示该账号不限；正整数设置每分钟最大请求数。">
            <Input type="number" min={0} value={rpmValue} placeholder="留空继承全局，0 表示不限"
              disabled={setCredentialRpm.isPending} onChange={(e) => setRpmValue(e.target.value)} />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setEditingRpm(false)} disabled={setCredentialRpm.isPending}>取消</Button>
            <Button variant="ghost" size="sm"
              disabled={setCredentialRpm.isPending || typeof credential.rpmOverride !== 'number'}
              onClick={() => {
                setRpmValue('')
                setCredentialRpm.mutate({ id: credential.id, request: { rpm: null } }, {
                  onSuccess: (res) => { toast.success(res.message); setEditingRpm(false) },
                  onError: (e) => toast.error(`设置失败: ${extractErrorMessage(e)}`),
                })
              }}>继承全局</Button>
            <Button size="sm" onClick={saveRpm} disabled={setCredentialRpm.isPending}>
              {setCredentialRpm.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Supported Models Modal ── */}
      <ModalShell open={editingSupportedModels} title={`支持模型：${displayCredentialLabel}`} width="max-w-xl"
        onClose={() => {
          if (setCredentialSupportedModels.isPending || discoverCredentialSupportedModels.isPending) return
          setSupportedModelsDraft(supportedModels)
          setEditingSupportedModels(false)
        }}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 px-3 py-2 text-sm">
            <div className="flex justify-between gap-2">
              <span className="text-muted-foreground">当前限制</span>
              <span className="font-semibold">{supportedModelsDraft.length > 0 ? `${supportedModelsDraft.length} 个模型` : '不限制'}</span>
            </div>
            <div className="mt-1 text-xs text-muted-foreground">保存空列表表示该账号可参与任意模型调度；非空时只允许精确匹配模型调用该账号。</div>
          </div>
          <Field label="支持模型" description="点击自动生成会先拉取当前 Kiro 账号支持模型，再生成可靠的 Claude/Claude Code 常用请求名；也可以手工添加或删除标签。">
            <SupportedModelTagsEditor
              value={supportedModelsDraft}
              disabled={setCredentialSupportedModels.isPending || discoverCredentialSupportedModels.isPending}
              onChange={setSupportedModelsDraft}
            />
          </Field>
          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => {
              setSupportedModelsDraft([])
              setCredentialSupportedModels.mutate({ id: credential.id, request: { supportedModels: [] } }, {
                onSuccess: (res) => {
                  toast.success('已清空模型限制')
                  setSupportedModelsDraft(res.supportedModels)
                  setEditingSupportedModels(false)
                },
                onError: (e) => toast.error(`清空失败: ${extractErrorMessage(e)}`),
              })
            }} disabled={setCredentialSupportedModels.isPending || discoverCredentialSupportedModels.isPending || supportedModels.length === 0}>
              不限制
            </Button>
            <Button variant="outline" size="sm" onClick={discoverSupportedModels} disabled={setCredentialSupportedModels.isPending || discoverCredentialSupportedModels.isPending}>
              {discoverCredentialSupportedModels.isPending && <Spinner size="sm" />}拉取并生成官方写法
            </Button>
            <Button size="sm" onClick={saveSupportedModels} disabled={setCredentialSupportedModels.isPending || discoverCredentialSupportedModels.isPending}>
              {setCredentialSupportedModels.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Regions Modal ── */}
      <ModalShell open={editingRegions} title={`Region：${displayCredentialLabel}`} width="max-w-lg"
        onClose={() => { if (setCredentialRegions.isPending) return; setEditingRegions(false) }}>
        <div className="space-y-3">
          <div className="rounded-lg bg-muted/30 p-3 text-sm grid gap-2 sm:grid-cols-2">
            <div>
              <div className="text-xs text-muted-foreground">当前 Auth Region</div>
              <div className="font-mono font-semibold">{credential.effectiveAuthRegion || '-'}</div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">当前 API Region</div>
              <div className="font-mono font-semibold">{credential.effectiveApiRegion || '-'}</div>
            </div>
          </div>
          <Field label="Region 兼容字段" description="未设置 Auth Region 时作为 token 刷新回退字段。">
            <Input className="font-mono" value={regionValue} disabled={setCredentialRegions.isPending}
              onChange={(e) => setRegionValue(e.target.value)} placeholder="留空继承全局" />
          </Field>
          <Field label="Auth Region">
            <Input className="font-mono" value={authRegionValue} disabled={setCredentialRegions.isPending}
              onChange={(e) => setAuthRegionValue(e.target.value)} placeholder="如 us-east-1，留空继承" />
          </Field>
          <Field label="API Region">
            <Input className="font-mono" value={apiRegionValue} disabled={setCredentialRegions.isPending}
              onChange={(e) => setApiRegionValue(e.target.value)} placeholder="如 us-east-1，留空继承" />
          </Field>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setEditingRegions(false)} disabled={setCredentialRegions.isPending}>取消</Button>
            <Button variant="ghost" size="sm"
              disabled={setCredentialRegions.isPending || (!credential.region && !credential.authRegion && !credential.apiRegion)}
              onClick={() => {
                setRegionValue(''); setAuthRegionValue(''); setApiRegionValue('')
                setCredentialRegions.mutate({ id: credential.id, request: { region: null, authRegion: null, apiRegion: null } }, {
                  onSuccess: (res) => { toast.success(res.message); setEditingRegions(false) },
                  onError: (e) => toast.error(`设置失败: ${extractErrorMessage(e)}`),
                })
              }}>清空覆盖</Button>
            <Button size="sm" onClick={saveRegions} disabled={setCredentialRegions.isPending}>
              {setCredentialRegions.isPending && <Spinner size="sm" />}保存
            </Button>
          </div>
        </div>
      </ModalShell>

      {/* ── Proxy Modal ── */}
      <CredentialProxyDialog open={editingProxy} onClose={() => setEditingProxy(false)}
        credential={credential} title={`绑定代理：${displayCredentialLabel}`} />

      {/* ── Diagnostics ── */}
      <ModalShell
        open={showDiagnostics}
        title={`账号诊断：${displayCredentialLabel}`}
        width="max-w-5xl"
        onClose={() => setShowDiagnostics(false)}
      >
        <div className="space-y-4">
          {diagnostics.isLoading && !diagnostics.data ? (
            <LoadingState text="加载账号调度与错误记录..." />
          ) : diagnostics.error ? (
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
              {extractErrorMessage(diagnostics.error)}
            </div>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                <MetaItem
                  label="冷却"
                  value={diagnostics.data?.runtime?.cooledDown ? formatRemaining(diagnostics.data.runtime.cooldownRemainingSecs) : '无'}
                />
                <MetaItem
                  label="并发"
                  value={`${diagnostics.data?.runtime?.inFlightRequests ?? credential.inFlightRequests}/${diagnostics.data?.runtime?.maxConcurrentRequests || credential.maxConcurrentRequests || '∞'}`}
                />
                <MetaItem
                  label="最近错误"
                  value={diagnostics.data?.runtime?.lastErrorKind || credential.lastErrorKind || '无'}
                  detail={diagnostics.data?.runtime?.lastErrorReason || credential.lastErrorReason}
                  error={Boolean(diagnostics.data?.runtime?.lastErrorReason || credential.lastErrorReason)}
                />
              </div>
              <div className="rounded-lg border border-border">
                <div className="border-b border-border px-3 py-2 text-sm font-semibold">错误记录</div>
                {diagnosticRecords.length === 0 ? (
                  <EmptyState title="暂无错误记录" description="该账号尚未留下可展示的请求级错误记录。" />
                ) : (
                  <div className="max-h-[55vh] space-y-2 overflow-y-auto p-3">
                    {diagnosticRecords.map((record) => (
                      <div key={record.id} className="rounded-lg border border-border bg-muted/20 p-3 text-xs">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="font-mono text-muted-foreground">{record.createdAt}</span>
                          <Badge tone="error">{record.errorType || record.status}</Badge>
                          {record.errorStatusCode && <Badge>{record.errorStatusCode}</Badge>}
                          <span className="font-mono text-muted-foreground">{record.id}</span>
                        </div>
                        {record.errorMessage && <div className="mt-2 text-sm text-foreground">{record.errorMessage}</div>}
                        {record.rawUpstreamError && (
                          <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap rounded bg-background p-2 font-mono text-[0.7rem] text-muted-foreground">
                            {record.rawUpstreamError.body}
                          </pre>
                        )}
                        {record.credentialAttempts && record.credentialAttempts.length > 0 && (
                          <div className="mt-2 space-y-1 border-t border-border pt-2">
                            {record.credentialAttempts.map((attempt) => (
                              <div key={`${record.id}-${attempt.attempt}-${attempt.credentialId}`} className="rounded bg-background px-2 py-1">
                                <span className="font-semibold">#{attempt.credentialId}</span>
                                <span className="ml-2 text-muted-foreground">{attempt.action}</span>
                                {attempt.errorType && <span className="ml-2 text-destructive">{attempt.errorType}</span>}
                                {attempt.errorMessage && <span className="ml-2 text-muted-foreground">{attempt.errorMessage}</span>}
                                {attempt.rawUpstreamError && (
                                  <pre className="mt-1 whitespace-pre-wrap font-mono text-[0.68rem] text-muted-foreground">
                                    {attempt.rawUpstreamError.body}
                                  </pre>
                                )}
                              </div>
                            ))}
                          </div>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
              <div className="flex justify-between gap-2">
                <Button variant="ghost" size="sm" onClick={() => diagnostics.refetch()} disabled={diagnostics.isFetching}>
                  {diagnostics.isFetching && <Spinner size="sm" />}刷新诊断
                </Button>
                <div className="flex gap-2">
                  {diagnostics.data?.hasNext && (
                    <Button size="sm" onClick={() => setDiagnosticsPage((page) => page + 1)} disabled={diagnostics.isFetching}>
                      加载更早记录
                    </Button>
                  )}
                  <Button variant="outline" size="sm" onClick={() => setShowDiagnostics(false)}>关闭</Button>
                </div>
              </div>
            </>
          )}
        </div>
      </ModalShell>

      {/* ── Delete Confirm ── */}
      <ModalShell open={showDeleteConfirm} title={`确认删除账号 #${credential.id}`} width="max-w-md"
        onClose={() => setShowDeleteConfirm(false)}>
        <div className="space-y-3 text-sm">
          <p>此操作会永久删除该账号，无法撤销。</p>
          <div className="rounded-lg bg-muted/30 px-3 py-2">
            <div className="font-semibold">{displayCredentialLabel}</div>
            <div className="mt-1 text-xs text-muted-foreground">只有已禁用账号允许删除。</div>
          </div>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setShowDeleteConfirm(false)} disabled={deleteCredential.isPending}>取消</Button>
            <Button variant="destructive" size="sm" onClick={handleDelete} disabled={deleteCredential.isPending || !credential.disabled}>
              {deleteCredential.isPending && <Spinner size="sm" />}确认删除
            </Button>
          </div>
        </div>
      </ModalShell>
    </article>
  )
}
