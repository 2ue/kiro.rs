import { useEffect, useMemo, useState } from 'react'
import {
  Check,
  Copy,
  CopyPlus,
  Database,
  Download,
  Edit3,
  Info,
  Link2,
  Plus,
  Route,
  Save,
  Trash2,
  Upload,
  WandSparkles,
  X,
  Zap,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  ErrorState,
  LoadingState,
  ModalShell,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  useConfirm,
} from '@/components/patterns'
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
} from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import {
  defaultReportedUsage,
  emptyRuntimeConfig,
  normalizeCachePolicy,
  normalizePromptSteering,
  normalizeReportedUsage,
} from '@/lib/runtime-config-defaults'
import { useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import type { CachePolicyConfig, CacheRoutePolicyPatch, IdentityPromptConfig, IdentityRouteOverride, RuntimeConfig } from '@/types/api'
import { identitySummary, RouteIdentityEditor } from './route-identity-editor'
import { RouteWebSearchEditor } from './route-websearch-editor'
import { RoutePolicyImportDialog } from './route-policy-import-dialog'
import { buildRoutePolicyFile, downloadRoutePolicyFile } from './route-policy-transfer'
import { routeBaseUrl, routeClaudeCodeEnv } from './route-copy'
import {
  buildDefinedCacheRoute,
  cachePolicyForStrategyTemplate,
  canonicalCachePolicyPath,
  deletePrefixAliases,
  normalizeDefinedCacheRoute,
  normalizeDefinedCacheRoutes,
  normalizeCachePolicyPath,
  PathStrategyBindingCard,
  reportedUsageForPrefix,
  routeOverrideForPrefix,
  sanitizeCacheRoutePolicy,
  StrategyTemplateEditor,
  type CacheStrategyType,
} from '@/features/runtime/runtime-sections'

const BUILT_IN_CACHE_PREFIXES = ['/v1', '/cc', '/ha', '/na'] as const

type TemplateKey = CacheStrategyType

type PathEditorState = {
  mode: 'new' | 'edit'
  originalPrefix: string | null
  prefix: string
  policy: CacheRoutePolicyPatch
  definedRoutes: string[]
  identity: IdentityRouteOverride
  /** Model-driven WebSearch override; undefined follows the runtime default. */
  webSearch?: boolean
  /** Set when the editor was prefilled by copying or importing a route policy. */
  origin?: { kind: 'copy' | 'import'; route: string | null }
  /** Register the new /dfcache route as soon as a valid name is typed (copy of a reachable route). */
  autoRegister?: boolean
}

type TemplateEditorState = {
  type: TemplateKey
  policy: CacheRoutePolicyPatch
}

const TEMPLATE_META: Array<{
  type: TemplateKey
  title: string
  shortTitle: string
  description: string
  tone: 'primary' | 'success' | 'warning' | 'info' | 'secondary'
}> = [
  {
    type: 'current_high_cache',
    title: '本地模拟缓存策略',
    shortTitle: '本地模拟缓存',
    description: '通过本地计算模拟缓存命中和缓存创建的展示用量。',
    tone: 'primary',
  },
  {
    type: 'kiro_rs_tool',
    title: 'Kiro-RS Tool',
    shortTitle: 'Kiro-RS Tool',
    description: '按会话和请求路径追踪稳定前缀，再生成缓存用量。',
    tone: 'success',
  },
  {
    type: 'stable_segment_cache',
    title: '稳定片段缓存',
    shortTitle: '稳定片段缓存',
    description: '从 tools、system 和历史消息中提取稳定片段作为缓存边界。',
    tone: 'warning',
  },
  {
    type: 'kiro_rs_tool_plus',
    title: 'Kiro-RS Tool Plus',
    shortTitle: 'Kiro-RS Tool Plus',
    description: '独立控制四类 usage 的 shaping、非流式策略、小请求和写入频次。',
    tone: 'info',
  },
  {
    type: 'bounded_high_cache',
    title: '上下限优先缓存策略',
    shortTitle: '上下限优先缓存',
    description: '参数同本地模拟缓存；各字段上下限是最终约束，计费只在范围内补，差额跨请求记账。',
    tone: 'secondary',
  },
]

function hydrateRuntimeConfig(config: RuntimeConfig): RuntimeConfig {
  return {
    ...emptyRuntimeConfig,
    ...config,
    reportedUsage: normalizeReportedUsage(config.reportedUsage ?? defaultReportedUsage()),
    cachePolicy: normalizeCachePolicy(config.cachePolicy),
    definedCacheRoutes: normalizeDefinedCacheRoutes(config.definedCacheRoutes ?? []),
    promptSteering: normalizePromptSteering(config.promptSteering),
  }
}

/** Identity override stored for a route prefix, accepting both "/x" and "/x/" keys. */
function identityOverrideForPrefix(identity: IdentityPromptConfig, prefix: string): IdentityRouteOverride {
  const canonical = canonicalCachePolicyPath(prefix)
  const entry = Object.entries(identity.pathOverrides ?? {}).find(([key]) => canonicalCachePolicyPath(key) === canonical)
  return entry ? { ...entry[1] } : {}
}

function withIdentityOverride(
  config: RuntimeConfig,
  oldPrefix: string | null,
  nextPrefix: string | null,
  override: IdentityRouteOverride | null,
): RuntimeConfig['promptSteering'] {
  const identity = config.promptSteering.identity
  const pathOverrides: Record<string, IdentityRouteOverride> = {}
  for (const [key, value] of Object.entries(identity.pathOverrides ?? {})) {
    const canonical = canonicalCachePolicyPath(key)
    if (canonical === oldPrefix || canonical === nextPrefix) continue
    pathOverrides[key] = value
  }
  if (nextPrefix && override && Object.keys(override).length > 0) pathOverrides[nextPrefix] = override
  return { ...config.promptSteering, identity: { ...identity, pathOverrides } }
}

/** Model-driven WebSearch override stored for a route prefix. */
function webSearchOverrideForPrefix(config: RuntimeConfig, prefix: string): boolean | undefined {
  const canonical = canonicalCachePolicyPath(prefix)
  const entry = Object.entries(config.bodyConversion.modelDrivenWebSearch?.pathOverrides ?? {})
    .find(([key]) => canonicalCachePolicyPath(key) === canonical)
  return entry?.[1]
}

function withWebSearchOverride(
  config: RuntimeConfig,
  oldPrefix: string | null,
  nextPrefix: string | null,
  value: boolean | undefined,
): RuntimeConfig['bodyConversion'] {
  const current = config.bodyConversion.modelDrivenWebSearch ?? { enabled: false, pathOverrides: {} }
  const pathOverrides: Record<string, boolean> = {}
  for (const [key, enabled] of Object.entries(current.pathOverrides ?? {})) {
    const canonical = canonicalCachePolicyPath(key)
    if (canonical === oldPrefix || canonical === nextPrefix) continue
    pathOverrides[key] = enabled
  }
  if (nextPrefix && value !== undefined) pathOverrides[nextPrefix] = value
  return { ...config.bodyConversion, modelDrivenWebSearch: { ...current, pathOverrides } }
}

function webSearchBadge(config: RuntimeConfig, prefix: string): string {
  const value = webSearchOverrideForPrefix(config, prefix) ?? config.bodyConversion.modelDrivenWebSearch?.enabled ?? false
  return value ? 'WebSearch 模型驱动' : 'WebSearch 模板'
}

function stripReadOnlyRuntimeFields(config: RuntimeConfig): RuntimeConfig {
  const editable = { ...config }
  delete editable.proxyUrl
  delete editable.proxyUsername
  delete editable.proxyPassword
  return editable
}

function strategyLabel(type?: CacheStrategyType): string {
  switch (type) {
    case 'current_high_cache':
      return '本地模拟缓存'
    case 'kiro_rs_tool':
      return 'Kiro-RS Tool'
    case 'stable_segment_cache':
      return '稳定片段缓存'
    case 'kiro_rs_tool_plus':
      return 'Kiro-RS Tool Plus'
    case 'bounded_high_cache':
      return '上下限优先缓存'
    default:
      return '无缓存'
  }
}

function templatePolicy(config: RuntimeConfig, type: TemplateKey): CacheRoutePolicyPatch {
  const cachePolicy = config.cachePolicy
  if (type === 'current_high_cache') {
    return cachePolicyForStrategyTemplate(
      { ...(cachePolicy.default ?? {}), ...(cachePolicy.currentHighCache ?? {}) },
      type,
    )
  }
  if (type === 'kiro_rs_tool') return cachePolicyForStrategyTemplate(cachePolicy.kiroRsTool ?? {}, type)
  if (type === 'stable_segment_cache') return cachePolicyForStrategyTemplate(cachePolicy.stableSegmentCache ?? {}, type)
  if (type === 'bounded_high_cache') return cachePolicyForStrategyTemplate(cachePolicy.boundedHighCache ?? {}, type)
  return cachePolicyForStrategyTemplate(cachePolicy.kiroRsToolPlus ?? {}, type)
}

function routeOrder(a: string, b: string): number {
  const aIndex = BUILT_IN_CACHE_PREFIXES.indexOf(a as (typeof BUILT_IN_CACHE_PREFIXES)[number])
  const bIndex = BUILT_IN_CACHE_PREFIXES.indexOf(b as (typeof BUILT_IN_CACHE_PREFIXES)[number])
  if (aIndex >= 0 || bIndex >= 0) {
    if (aIndex < 0) return 1
    if (bIndex < 0) return -1
    return aIndex - bIndex
  }
  return a.localeCompare(b)
}

function isBuiltInPrefix(prefix: string): boolean {
  return BUILT_IN_CACHE_PREFIXES.includes(prefix as (typeof BUILT_IN_CACHE_PREFIXES)[number])
}

function builtinDefaultType(prefix: string): CacheStrategyType {
  return prefix === '/na' ? 'no_cache' : 'current_high_cache'
}

function defaultPathPolicy(prefix: string, cachePolicy: CachePolicyConfig, reportedUsage: RuntimeConfig['reportedUsage'], definedRoutes: string[]): CacheRoutePolicyPatch {
  const existing = routeOverrideForPrefix(cachePolicy.pathOverrides, prefix)
  const legacyReportedUsage = reportedUsageForPrefix(reportedUsage.pathOverrides, prefix)
  if (existing) {
    if (
      existing.cacheType === 'no_cache' ||
      existing.cacheType === 'kiro_rs_tool' ||
      existing.cacheType === 'kiro_rs_tool_plus' ||
      existing.cacheType === 'stable_segment_cache' ||
      existing.cacheType === 'bounded_high_cache'
    ) {
      return existing
    }
    return legacyReportedUsage ? { ...existing, reportedUsage: existing.reportedUsage ?? legacyReportedUsage } : existing
  }
  if (legacyReportedUsage) return { cacheType: 'current_high_cache', reportedUsage: legacyReportedUsage }
  if (isBuiltInPrefix(prefix)) return { cacheType: builtinDefaultType(prefix) }
  const normalizedRoute = normalizeDefinedCacheRoute(prefix)
  if (normalizedRoute && definedRoutes.includes(normalizedRoute)) return { cacheType: 'current_high_cache' }
  return { cacheType: 'no_cache' }
}

function formatRatio(value?: number): string {
  if (value == null) return '—'
  return `${Math.round(value * 100)}%`
}

function formatTokens(value?: number): string {
  if (value == null || value === 0) return '不限制'
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`
  if (value >= 1000) return `${Math.round(value / 1000)}K`
  return String(value)
}

function templateHighlights(type: TemplateKey, policy: CacheRoutePolicyPatch): string[] {
  if (type === 'bounded_high_cache') {
    return [
      `命中读取 ${formatRatio(policy.simulation?.targetReadRatio)}`,
      `输入上限 ${formatTokens(policy.reportedUsage?.input?.maxTokens)}`,
      `计费下限 ${policy.hitShaping?.enabled ? `${policy.hitShaping.minCostRatio ?? 1}×` : '关闭'}`,
    ]
  }
  if (type === 'current_high_cache') {
    return [
      `命中读取 ${formatRatio(policy.simulation?.targetReadRatio)}`,
      `输入放大 ${policy.simulation?.tokenScale ?? 1}×`,
      `写入间隔 ${policy.creationControl?.minCreationIntervalSecs ?? 0}s`,
    ]
  }
  if (type === 'kiro_rs_tool') {
    return [
      `覆盖率 ${formatRatio(policy.kiroRsTool?.coverageRatio)}`,
      `增量写入 ${policy.kiroRsTool?.incrementalCreateEnabled === false ? '关闭' : '开启'}`,
      `稳定前缀 ${policy.kiroRsTool?.cacheCurrentUserStablePrefix ? '开启' : '关闭'}`,
    ]
  }
  if (type === 'stable_segment_cache') {
    return [
      `覆盖率 ${formatRatio(policy.stableSegment?.coverageRatio)}`,
      `最小片段 ${formatTokens(policy.stableSegment?.minSegmentTokens)}`,
      `工具 / 系统 / 历史 ${[
        policy.stableSegment?.includeTools,
        policy.stableSegment?.includeSystem,
        policy.stableSegment?.includeHistory,
      ].filter(Boolean).length}/3`,
    ]
  }
  const plus = policy.kiroRsToolPlus
  return [
    `覆盖率 ${formatRatio(plus?.coverageRatio)}`,
    `输入 ${plus?.shaping?.input?.multiplier ?? 1}× · 输出 ${plus?.shaping?.output?.multiplier ?? 1}×`,
    `小请求 ${plus?.smallRequest?.enabled ? `${formatTokens(plus.smallRequest.maxInputTokens)} 内` : '关闭'}`,
  ]
}

function pathSummary(policy: CacheRoutePolicyPatch): string[] {
  if (!policy.cacheType || policy.cacheType === 'no_cache') return ['请求不进入缓存计算']
  if (policy.cacheType === 'bounded_high_cache') {
    return [
      `输入上限 ${formatTokens(policy.reportedUsage?.input?.maxTokens)}`,
      `路径空间 ${policy.routeNamespace ? '独立' : '共享'}`,
    ]
  }
  if (policy.cacheType === 'current_high_cache') {
    return [
      `命中读取 ${formatRatio(policy.simulation?.targetReadRatio)}`,
      `路径空间 ${policy.routeNamespace ? '独立' : '共享'}`,
    ]
  }
  if (policy.cacheType === 'kiro_rs_tool') {
    return [
      `覆盖率 ${formatRatio(policy.kiroRsTool?.coverageRatio)}`,
      `增量写入 ${policy.kiroRsTool?.incrementalCreateEnabled === false ? '关闭' : '开启'}`,
    ]
  }
  if (policy.cacheType === 'stable_segment_cache') {
    return [
      `覆盖率 ${formatRatio(policy.stableSegment?.coverageRatio)}`,
      `路径空间 ${policy.routeNamespace ? '独立' : '共享'}`,
    ]
  }
  return [
    `覆盖率 ${formatRatio(policy.kiroRsToolPlus?.coverageRatio)}`,
    `输入 ${policy.kiroRsToolPlus?.shaping?.input?.multiplier ?? 1}×`,
  ]
}

function TemplateCard({
  type,
  policy,
  onEdit,
}: {
  type: TemplateKey
  policy: CacheRoutePolicyPatch
  onEdit: () => void
}) {
  const meta = TEMPLATE_META.find((item) => item.type === type) ?? TEMPLATE_META[0]
  const toneClass = {
    primary: 'bg-primary/10 text-primary',
    success: 'bg-success/10 text-success',
    warning: 'bg-warning/10 text-warning',
    info: 'bg-info/10 text-info',
    secondary: 'bg-secondary/10 text-secondary',
  }[meta.tone]
  return (
    <Card className="flex h-full flex-col border border-border/70 transition hover:-translate-y-0.5 hover:shadow-md">
      <CardHeader className="items-start gap-3 pb-3">
        <div className="flex min-w-0 items-start gap-3">
          <span className={`flex size-9 shrink-0 items-center justify-center rounded-lg ${toneClass}`}>
            <Zap className="size-4" />
          </span>
          <div className="min-w-0">
            <CardTitle>{meta.title}</CardTitle>
            <CardDescription>{meta.description}</CardDescription>
          </div>
        </div>
        <Button variant="outline" size="xs" onClick={onEdit}>
          <Edit3 />
          编辑
        </Button>
      </CardHeader>
      <CardContent className="mt-auto pt-1">
        <div className="flex flex-wrap gap-2">
          {templateHighlights(type, policy).map((item) => (
            <Badge key={item} tone={meta.tone} size="xs">{item}</Badge>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}

async function copyRouteText(label: string, value: string, reachable: boolean) {
  try {
    await navigator.clipboard.writeText(value)
    if (reachable) {
      toast.success(`${label}已复制`)
    } else {
      toast.warning(`${label}已复制，但该路由还没注册 /dfcache，请求会返回 404`)
    }
  } catch (error) {
    toast.error(`复制失败: ${extractErrorMessage(error)}`)
  }
}

function PathCard({
  prefix,
  policy,
  identityBadges,
  builtIn,
  registered,
  onEdit,
  onCopy,
  onExport,
  onDelete,
}: {
  prefix: string
  policy: CacheRoutePolicyPatch
  identityBadges: string[]
  builtIn: boolean
  registered: boolean
  onEdit: () => void
  onCopy: () => void
  onExport: () => void
  onDelete: () => void
}) {
  const reachable = builtIn || registered
  const baseUrl = routeBaseUrl(prefix)
  return (
    <Card className="flex h-full flex-col border border-border/70 transition hover:shadow-md">
      <CardHeader className="items-start gap-3 pb-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <CardTitle className="font-mono text-sm">{prefix}</CardTitle>
            {builtIn && <Badge size="xs" tone="secondary">内置</Badge>}
            {!builtIn && registered && <Badge size="xs" tone="success" dot>/dfcache 已注册</Badge>}
            {!reachable && <Badge size="xs" tone="warning">未注册，暂不可访问</Badge>}
          </div>
          <CardDescription>{strategyLabel(policy.cacheType)}</CardDescription>
        </div>
        <div className="flex shrink-0 gap-1">
          <Button variant="ghost" size="icon-xs" title="编辑" aria-label={`编辑 ${prefix}`} onClick={onEdit}><Edit3 /></Button>
          <Button variant="ghost" size="icon-xs" title="复制为新路由" aria-label={`以 ${prefix} 的策略新建路由`} onClick={onCopy}><CopyPlus /></Button>
          <Button variant="ghost" size="icon-xs" title="导出 JSON" aria-label={`导出 ${prefix} 的策略`} onClick={onExport}><Download /></Button>
          {!builtIn && <Button variant="ghost" size="icon-xs" className="text-destructive hover:text-destructive" title="删除" onClick={onDelete}><Trash2 /></Button>}
        </div>
      </CardHeader>
      <CardContent className="mt-auto space-y-3 pt-1">
        <div className="flex min-w-0 items-center gap-1 rounded-md border border-border/70 bg-muted/40 py-1 pl-2.5 pr-1">
          <Link2 className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
          <span className="min-w-0 flex-1 truncate font-mono text-xs" title={baseUrl}>{baseUrl}</span>
          <Button
            variant="ghost"
            size="icon-xs"
            title="复制完整路由地址"
            aria-label={`复制 ${prefix} 的完整路由地址`}
            onClick={() => void copyRouteText('路由地址', baseUrl, reachable)}
          >
            <Copy />
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="xs" title="更多复制方式" aria-label={`${prefix} 的更多复制方式`}>
                更多
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuLabel>复制接入信息</DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => void copyRouteText('路由地址', baseUrl, reachable)}>
                Base URL
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => void copyRouteText('Messages 接口地址', `${baseUrl}/v1/messages`, reachable)}>
                Messages 接口地址
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => void copyRouteText('Claude Code 环境变量', routeClaudeCodeEnv(prefix), reachable)}>
                Claude Code 环境变量
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        <div className="flex flex-wrap gap-2">
          {pathSummary(policy).map((item) => <Badge key={item} size="xs" tone="neutral">{item}</Badge>)}
          {identityBadges.map((item) => (
            <Badge key={item} size="xs" tone={item === '身份隐藏关' || item === 'WebSearch 模板' ? 'neutral' : 'info'}>{item}</Badge>
          ))}
        </div>
      </CardContent>
    </Card>
  )
}

export function CachePoliciesPage() {
  const runtimeConfig = useRuntimeConfig()
  const updateConfig = useUpdateRuntimeConfig()
  const confirm = useConfirm()
  const [workingConfig, setWorkingConfig] = useState<RuntimeConfig | null>(null)
  const [templateEditor, setTemplateEditor] = useState<TemplateEditorState | null>(null)
  const [pathEditor, setPathEditor] = useState<PathEditorState | null>(null)
  const [importOpen, setImportOpen] = useState(false)
  const [threshold, setThreshold] = useState(10000)

  useEffect(() => {
    if (!runtimeConfig.data) return
    const hydrated = hydrateRuntimeConfig(runtimeConfig.data)
    setWorkingConfig(hydrated)
    setThreshold(hydrated.highCacheThreshold)
  }, [runtimeConfig.data])

  const cachePolicy = workingConfig?.cachePolicy ?? emptyRuntimeConfig.cachePolicy
  const definedRoutes = workingConfig?.definedCacheRoutes ?? []
  const paths = useMemo(() => {
    if (!workingConfig) return []
    return Array.from(new Set([
      ...BUILT_IN_CACHE_PREFIXES,
      ...Object.keys(cachePolicy.pathOverrides ?? {}).map(canonicalCachePolicyPath),
      ...Object.keys(workingConfig.reportedUsage.pathOverrides ?? {}).map(canonicalCachePolicyPath),
      ...definedRoutes.map(canonicalCachePolicyPath),
      ...Object.keys(workingConfig.promptSteering.identity.pathOverrides ?? {}).map(canonicalCachePolicyPath),
    ])).sort(routeOrder)
  }, [cachePolicy.pathOverrides, definedRoutes, workingConfig])

  const persist = async (nextConfig: RuntimeConfig, successMessage = '路由策略已保存，新请求立即生效') => {
    const normalized = hydrateRuntimeConfig({
      ...nextConfig,
      highCacheThreshold: Math.max(0, Math.floor(nextConfig.highCacheThreshold || 0)),
    })
    try {
      const response = await updateConfig.mutateAsync(stripReadOnlyRuntimeFields(normalized))
      setWorkingConfig(hydrateRuntimeConfig(response ?? normalized))
      toast.success(successMessage)
      return true
    } catch (error) {
      toast.error(`保存失败: ${extractErrorMessage(error)}`)
      return false
    }
  }

  const openTemplateEditor = (type: TemplateKey) => {
    if (!workingConfig) return
    setTemplateEditor({ type, policy: templatePolicy(workingConfig, type) })
  }

  const saveTemplate = async () => {
    if (!workingConfig || !templateEditor) return
    const nextCachePolicy: CachePolicyConfig = { ...workingConfig.cachePolicy }
    if (templateEditor.type === 'current_high_cache') {
      nextCachePolicy.currentHighCache = templateEditor.policy
      nextCachePolicy.default = { ...nextCachePolicy.default, ...templateEditor.policy }
    } else if (templateEditor.type === 'kiro_rs_tool') {
      nextCachePolicy.kiroRsTool = templateEditor.policy
    } else if (templateEditor.type === 'stable_segment_cache') {
      nextCachePolicy.stableSegmentCache = templateEditor.policy
    } else if (templateEditor.type === 'bounded_high_cache') {
      nextCachePolicy.boundedHighCache = templateEditor.policy
    } else {
      nextCachePolicy.kiroRsToolPlus = templateEditor.policy
    }
    const saved = await persist({ ...workingConfig, cachePolicy: normalizeCachePolicy(nextCachePolicy) })
    if (saved) setTemplateEditor(null)
  }

  const mergedPathPolicy = (prefix: string): CacheRoutePolicyPatch => {
    if (!workingConfig) return { cacheType: 'no_cache' }
    return defaultPathPolicy(prefix, workingConfig.cachePolicy, workingConfig.reportedUsage, workingConfig.definedCacheRoutes)
  }

  const openPathEditor = (prefix: string) => {
    if (!workingConfig) return
    setPathEditor({
      mode: 'edit',
      originalPrefix: prefix,
      prefix,
      policy: mergedPathPolicy(prefix),
      definedRoutes: [...workingConfig.definedCacheRoutes],
      identity: identityOverrideForPrefix(workingConfig.promptSteering.identity, prefix),
      webSearch: webSearchOverrideForPrefix(workingConfig, prefix),
    })
  }

  const openNewPathEditor = () => {
    setPathEditor({
      mode: 'new',
      originalPrefix: null,
      prefix: '/dfcache/',
      policy: { cacheType: 'no_cache' },
      definedRoutes: [...definedRoutes],
      identity: {},
    })
  }

  /** Opens the new-route editor prefilled with another route's settings; the route itself is not copied. */
  const openCopyPathEditor = (prefix: string) => {
    if (!workingConfig) return
    const sourceRoute = normalizeDefinedCacheRoute(prefix)
    setPathEditor({
      mode: 'new',
      originalPrefix: null,
      prefix: '/dfcache/',
      // Deep copies so editing the copy never touches the source route's draft.
      policy: structuredClone(mergedPathPolicy(prefix)),
      definedRoutes: [...definedRoutes],
      identity: structuredClone(identityOverrideForPrefix(workingConfig.promptSteering.identity, prefix)),
      webSearch: webSearchOverrideForPrefix(workingConfig, prefix),
      origin: { kind: 'copy', route: prefix },
      // Built-in routes and registered /dfcache routes are reachable, so the copy is too.
      autoRegister: isBuiltInPrefix(prefix) || Boolean(sourceRoute && definedRoutes.includes(sourceRoute)),
    })
  }

  const exportPath = (prefix: string) => {
    if (!workingConfig) return
    downloadRoutePolicyFile(buildRoutePolicyFile(
      prefix,
      sanitizeCacheRoutePolicy(mergedPathPolicy(prefix)),
      identityOverrideForPrefix(workingConfig.promptSteering.identity, prefix),
      webSearchOverrideForPrefix(workingConfig, prefix),
    ))
    toast.success(`已导出 ${prefix} 的路由策略`)
  }

  /** Imported policies open the normal new-route editor, prefilled; the route is never imported. */
  const openImportedPathEditor = (imported: { sourceRoute: string | null; cachePolicy: CacheRoutePolicyPatch; identity: IdentityRouteOverride; webSearch?: boolean }) => {
    setImportOpen(false)
    setPathEditor({
      mode: 'new',
      originalPrefix: null,
      prefix: '/dfcache/',
      policy: imported.cachePolicy,
      definedRoutes: [...definedRoutes],
      identity: imported.identity,
      webSearch: imported.webSearch,
      origin: { kind: 'import', route: imported.sourceRoute },
    })
  }

  const savePath = async () => {
    if (!workingConfig || !pathEditor) return
    const normalizedPrefix = normalizeCachePolicyPath(pathEditor.prefix)
    if (!normalizedPrefix || normalizedPrefix === '/') {
      toast.error('请输入有效的路径前缀')
      return
    }
    if (pathEditor.mode === 'new' && !buildDefinedCacheRoute(pathEditor.prefix)) {
      toast.error('自定义路径必须使用 /dfcache/{name}，name 只能包含字母、数字、点、下划线或短横线')
      return
    }
    const oldPrefix = pathEditor.originalPrefix ? canonicalCachePolicyPath(pathEditor.originalPrefix) : null
    const nextPrefix = canonicalCachePolicyPath(normalizedPrefix)
    const otherPaths = paths.filter((path) => path !== oldPrefix)
    if (otherPaths.includes(nextPrefix)) {
      toast.error(`${nextPrefix} 已存在，请换一个路径`)
      return
    }
    const nextCachePolicy = { ...workingConfig.cachePolicy, pathOverrides: { ...workingConfig.cachePolicy.pathOverrides } }
    const nextReportedUsage = { ...workingConfig.reportedUsage, pathOverrides: { ...workingConfig.reportedUsage.pathOverrides } }
    if (oldPrefix) {
      deletePrefixAliases(nextCachePolicy.pathOverrides, oldPrefix)
      deletePrefixAliases(nextReportedUsage.pathOverrides, oldPrefix)
    }
    nextCachePolicy.pathOverrides[nextPrefix] = sanitizeCacheRoutePolicy(pathEditor.policy)
    let nextDefinedRoutes = normalizeDefinedCacheRoutes(pathEditor.definedRoutes)
    if (oldPrefix && oldPrefix !== nextPrefix) {
      const oldDefined = normalizeDefinedCacheRoute(oldPrefix)
      const nextDefined = normalizeDefinedCacheRoute(nextPrefix)
      if (oldDefined) nextDefinedRoutes = nextDefinedRoutes.filter((route) => route !== oldDefined)
      if (oldDefined && nextDefined && pathEditor.definedRoutes.includes(oldDefined)) nextDefinedRoutes.push(nextDefined)
    }
    nextDefinedRoutes = normalizeDefinedCacheRoutes(nextDefinedRoutes)
    const saved = await persist({
      ...workingConfig,
      cachePolicy: normalizeCachePolicy(nextCachePolicy),
      reportedUsage: normalizeReportedUsage(nextReportedUsage),
      definedCacheRoutes: nextDefinedRoutes,
      promptSteering: withIdentityOverride(workingConfig, oldPrefix, nextPrefix, pathEditor.identity),
      bodyConversion: withWebSearchOverride(workingConfig, oldPrefix, nextPrefix, pathEditor.webSearch),
    })
    if (saved) setPathEditor(null)
  }

  const deletePath = async (prefix: string) => {
    if (!workingConfig) return
    const confirmed = await confirm({
      title: '删除路由策略？',
      message: `确认删除路径 ${prefix} 的路由策略吗？对应的缓存覆盖、Usage 覆盖、身份隐藏覆盖、WebSearch 设置和 /dfcache 注册都会被清理。`,
      confirmText: '确认删除',
      cancelText: '取消',
      tone: 'danger',
    })
    if (!confirmed) return
    const nextCachePolicy = { ...workingConfig.cachePolicy, pathOverrides: { ...workingConfig.cachePolicy.pathOverrides } }
    const nextReportedUsage = { ...workingConfig.reportedUsage, pathOverrides: { ...workingConfig.reportedUsage.pathOverrides } }
    const normalizedPrefix = canonicalCachePolicyPath(prefix)
    deletePrefixAliases(nextCachePolicy.pathOverrides, normalizedPrefix)
    deletePrefixAliases(nextReportedUsage.pathOverrides, normalizedPrefix)
    const normalizedRoute = normalizeDefinedCacheRoute(normalizedPrefix)
    const nextDefinedRoutes = normalizedRoute
      ? workingConfig.definedCacheRoutes.filter((route) => route !== normalizedRoute)
      : workingConfig.definedCacheRoutes
    const saved = await persist({
      ...workingConfig,
      cachePolicy: normalizeCachePolicy(nextCachePolicy),
      reportedUsage: normalizeReportedUsage(nextReportedUsage),
      definedCacheRoutes: normalizeDefinedCacheRoutes(nextDefinedRoutes),
      promptSteering: withIdentityOverride(workingConfig, normalizedPrefix, null, null),
      bodyConversion: withWebSearchOverride(workingConfig, normalizedPrefix, null, undefined),
    }, '路由策略已删除')
    if (saved && pathEditor?.originalPrefix === prefix) setPathEditor(null)
  }

  const saveThreshold = async () => {
    if (!workingConfig) return
    await persist({ ...workingConfig, highCacheThreshold: threshold }, '缓存命中阈值已保存')
  }

  if (runtimeConfig.isLoading || !workingConfig) return <LoadingState text="加载路由策略..." />
  if (runtimeConfig.error) return <ErrorState message={extractErrorMessage(runtimeConfig.error)} />

  return (
    <PageContainer className="pb-24">
      <PageHeader
        title="路由策略"
        subtitle="按路由分别管理缓存策略和身份隐藏，默认模板、路径覆盖和 /dfcache 路由集中在一处"
        actions={
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => setImportOpen(true)}>
              <Upload />
              导入
            </Button>
            <Button onClick={openNewPathEditor}>
              <Plus />
              新增路由
            </Button>
          </div>
        }
      />

      <StatGrid min="12rem">
        <StatCard title="默认策略模板" value={TEMPLATE_META.length} desc="每种策略分别维护自己的参数" icon={<WandSparkles />} tone="primary" />
        <StatCard title="路由" value={paths.length} desc="包含内置路由和自定义覆盖" icon={<Route />} tone="info" />
        <StatCard title="/dfcache 路由" value={definedRoutes.length} desc="已注册的自定义入口" icon={<Database />} tone="success" />
        <StatCard title="命中判定阈值" value={formatTokens(workingConfig.highCacheThreshold)} desc="后台统计高缓存命中的 Token 阈值" icon={<Info />} tone="warning" />
      </StatGrid>

      <SectionCard
        title="默认策略模板"
        description="路由选择缓存策略后，会先读取对应模板；路由弹窗里可以再配置独立覆盖。"
        icon={<WandSparkles />}
      >
        <div className="grid gap-4 xl:grid-cols-2">
          {TEMPLATE_META.map((meta) => (
            <TemplateCard key={meta.type} type={meta.type} policy={templatePolicy(workingConfig, meta.type)} onEdit={() => openTemplateEditor(meta.type)} />
          ))}
        </div>
      </SectionCard>

      <SectionCard
        title="路由"
        description="每个路由独立配置缓存策略和身份隐藏。内置路由不能删除；自定义路由通过 /dfcache/{name} 管理。"
        icon={<Route />}
        actions={
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}><Upload />导入</Button>
            <Button variant="outline" size="sm" onClick={openNewPathEditor}><Plus />新增路径</Button>
          </div>
        }
      >
        <div className="grid gap-4 xl:grid-cols-2">
          {paths.map((prefix) => {
            const normalizedRoute = normalizeDefinedCacheRoute(prefix)
            return (
              <PathCard
                key={prefix}
                prefix={prefix}
                policy={mergedPathPolicy(prefix)}
                identityBadges={[
                  ...identitySummary(
                    workingConfig.promptSteering.identity,
                    prefix,
                    identityOverrideForPrefix(workingConfig.promptSteering.identity, prefix),
                  ),
                  webSearchBadge(workingConfig, prefix),
                ]}
                builtIn={isBuiltInPrefix(prefix)}
                registered={Boolean(normalizedRoute && definedRoutes.includes(normalizedRoute))}
                onEdit={() => openPathEditor(prefix)}
                onCopy={() => openCopyPathEditor(prefix)}
                onExport={() => exportPath(prefix)}
                onDelete={() => void deletePath(prefix)}
              />
            )
          })}
        </div>
      </SectionCard>

      <SectionCard
        title="统计展示"
        description="只影响后台列表里的高缓存命中标记，不改变请求处理和客户端 Usage。"
        icon={<Info />}
      >
        <div className="flex flex-col gap-3 md:flex-row md:items-end">
          <div className="min-w-0 flex-1 space-y-1.5">
            <label className="text-sm font-semibold" htmlFor="high-cache-threshold">缓存命中判定阈值</label>
            <p className="text-xs leading-5 text-muted-foreground">缓存读取达到这个 Token 数后，后台统计会把请求标记为高缓存命中。</p>
            <div className="flex max-w-xs items-center gap-2">
              <Input id="high-cache-threshold" type="number" min={0} value={threshold} onChange={(event) => setThreshold(Math.max(0, Number(event.target.value) || 0))} />
              <span className="shrink-0 text-sm text-muted-foreground">Token</span>
            </div>
          </div>
          <Button variant="outline" onClick={() => void saveThreshold()} disabled={updateConfig.isPending}>
            <Save />
            保存阈值
          </Button>
        </div>
      </SectionCard>

      {importOpen && (
        <RoutePolicyImportDialog onClose={() => setImportOpen(false)} onImport={openImportedPathEditor} />
      )}

      {templateEditor && (
        <ModalShell
          open
          title={`编辑${TEMPLATE_META.find((item) => item.type === templateEditor.type)?.shortTitle ?? '缓存'}模板`}
          description="参数只会在点击保存后写入运行配置；取消不会改变当前服务配置。"
          width="max-w-5xl"
          noBodyPadding
          onClose={() => { if (!updateConfig.isPending) setTemplateEditor(null) }}
          footer={
            <>
              <Button variant="outline" onClick={() => setTemplateEditor(null)} disabled={updateConfig.isPending}><X />取消</Button>
              <Button onClick={() => void saveTemplate()} disabled={updateConfig.isPending}><Save />保存模板</Button>
            </>
          }
        >
          <div className="px-5 py-6 sm:px-7 sm:py-7">
            <StrategyTemplateEditor
              title="策略参数"
              desc={TEMPLATE_META.find((item) => item.type === templateEditor.type)?.description ?? ''}
              showHeader
              cacheType={templateEditor.type}
              policy={templateEditor.policy}
              onChange={(policy) => setTemplateEditor((current) => current ? { ...current, policy } : current)}
            />
          </div>
        </ModalShell>
      )}

      {pathEditor && (
        <ModalShell
          open
          title={pathEditor.origin?.kind === 'copy'
            ? `复制路由策略 · 来自 ${pathEditor.origin.route}`
            : pathEditor.origin?.kind === 'import'
              ? `导入路由策略${pathEditor.origin.route ? ` · 来自 ${pathEditor.origin.route}` : ''}`
              : pathEditor.mode === 'new' ? '新增路由策略' : `编辑路由策略 · ${pathEditor.originalPrefix}`}
          description={pathEditor.origin
            ? `已填入${pathEditor.origin.kind === 'copy' ? '来源路由' : '导入'}的缓存策略和身份隐藏设置；路由不会${pathEditor.origin.kind === 'copy' ? '复制' : '导入'}，请填写新的 /dfcache 路由名后保存。`
            : '路径、缓存策略和身份隐藏会一起保存；关闭弹窗不会提交草稿。'}
          width="max-w-5xl"
          noBodyPadding
          onClose={() => { if (!updateConfig.isPending) setPathEditor(null) }}
          footer={
            <>
              <Button variant="outline" onClick={() => setPathEditor(null)} disabled={updateConfig.isPending}><X />取消</Button>
              <Button onClick={() => void savePath()} disabled={updateConfig.isPending}><Check />保存路由策略</Button>
            </>
          }
        >
          <div className="px-5 py-6 sm:px-7 sm:py-7">
            <PathStrategyBindingCard
              prefix={pathEditor.prefix}
              policy={pathEditor.policy}
              cachePolicy={workingConfig.cachePolicy}
              definedRoutes={pathEditor.definedRoutes}
              builtIn={isBuiltInPrefix(pathEditor.prefix)}
              showDelete={false}
              onPrefixChange={(nextPrefix) => setPathEditor((current) => {
                if (!current) return current
                const oldRoute = normalizeDefinedCacheRoute(current.prefix)
                const nextRoute = normalizeDefinedCacheRoute(nextPrefix)
                let definedRoutes = current.definedRoutes
                if (oldRoute && nextRoute && definedRoutes.includes(oldRoute)) {
                  definedRoutes = definedRoutes.filter((route) => route !== oldRoute)
                  definedRoutes = normalizeDefinedCacheRoutes([...definedRoutes, nextRoute])
                } else if (current.autoRegister && nextRoute) {
                  // A copied reachable route stays reachable once it has a valid name.
                  definedRoutes = normalizeDefinedCacheRoutes([...definedRoutes, nextRoute])
                }
                return { ...current, prefix: nextPrefix, definedRoutes }
              })}
              onDelete={() => undefined}
              onChange={(policy) => setPathEditor((current) => current ? { ...current, policy } : current)}
              onDefinedRouteChange={(enabled) => setPathEditor((current) => {
                if (!current) return current
                const route = normalizeDefinedCacheRoute(current.prefix)
                if (!route) return current
                const routes = enabled
                  ? normalizeDefinedCacheRoutes([...current.definedRoutes, route])
                  : current.definedRoutes.filter((item) => item !== route)
                // An explicit choice replaces the copy's automatic registration.
                return { ...current, definedRoutes: routes, autoRegister: enabled }
              })}
            />
            <div className="mt-8 border-t pt-6">
              <RouteIdentityEditor
                prefix={canonicalCachePolicyPath(normalizeCachePolicyPath(pathEditor.prefix) ?? pathEditor.prefix)}
                identity={workingConfig.promptSteering.identity}
                value={pathEditor.identity}
                onChange={(identity) => setPathEditor((current) => current ? { ...current, identity } : current)}
              />
            </div>
            <div className="mt-8 border-t pt-6">
              <RouteWebSearchEditor
                defaultEnabled={workingConfig.bodyConversion.modelDrivenWebSearch?.enabled ?? false}
                value={pathEditor.webSearch}
                onChange={(webSearch) => setPathEditor((current) => current ? { ...current, webSearch } : current)}
              />
            </div>
          </div>
        </ModalShell>
      )}
    </PageContainer>
  )
}
