import { type ReactNode, useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  FlaskConical,
  Pencil,
  Plus,
  Power,
  RefreshCw,
  RotateCcw,
  Save,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  clearAccountAutoDisabled,
  clearAccountCooldown,
  createAccount,
  deleteAccount,
  discoverAccountSupportedModels,
  discoverStoredAccountSupportedModels,
  getAccounts,
  getAccountsStatus,
  setAccountEnabled,
  updateAccount,
  updateRuntimeConfig,
  getRouteScenarioOptions,
} from '@/api/credentials'
import { defaultAccountsConfig } from '@/lib/runtime-config-defaults'
import { useRuntimeConfig } from '@/hooks/use-credentials'
import { extractErrorMessage, cn } from '@/lib/utils'
import type {
  Account,
  AccountQualityView,
  AccountsConfig,
  UpdateAccountRequest,
} from '@/types/api'
import { pageMeta } from '@/types/ui'
import {
  EmptyState,
  LoadingState,
  PageContainer,
  PageHeader,
  SectionCard,
  StatCard,
  StatGrid,
  useConfirm,
} from '@/components/patterns'
import { Badge, Button } from '@/components/ui'
import { SelectItem } from '@/components/ui'
import { ProgressRing } from '@/components/charts'
import {
  type AccountFormDraft,
  authLabel,
  defaultPoolForm,
  joinRules,
  joinStatusCodeList,
  parseHeaderOverridesText,
  parseModelMappingRules,
  parseStatusCodeList,
  parseSupportedModelsText,
  poolFormFromPool,
  poolBodyModeSummary,
  poolHeaderProfileSummary,
  poolModelMappingSummary,
  poolRouteSummary,
  streamRetrySummary,
  poolSupportedModelsSummary,
  poolUsageSummary,
  splitRules,
  whole,
} from './account-utils'
import { AccountFormModal } from './account-form-modal'
import { RouteSelector } from '@/features/route-scenarios/route-selector'
import { AccountTestModal } from './account-test-modal'
import {
  FormSection,
  NumberBox,
  SelectBox,
  TextBox,
  TextAreaBox,
  ToggleRow,
} from './account-components'

function normalizeRouteRuleMode(value?: string): AccountsConfig['accountRouteMode'] {
  if (value === 'allow_list' || value === 'deny_list') return value
  return 'allow_all'
}

function accountQualitySummary(quality?: AccountQualityView) {
  if (!quality) return null
  const parts = [
    `质量错误率 ${Math.round(quality.recentErrorRate * 100)}%`,
    quality.ttftEwmaMs != null ? `首字 ${Math.round(quality.ttftEwmaMs)}ms` : null,
    quality.latencyEwmaMs != null ? `总耗时 ${Math.round(quality.latencyEwmaMs)}ms` : null,
    `样本 ${quality.sampleCount}`,
  ].filter(Boolean) as string[]
  if (!quality.scoringActive) parts.push('冷启动')
  if (quality.inProbation) {
    parts.push(`避让 ${quality.probationRemainingSecs}s`)
  } else if (quality.recoveryProgress < 1) {
    parts.push(`恢复 ${Math.round(quality.recoveryProgress * 100)}%`)
  }
  return parts.join(' · ')
}

// ============================================================================
// Local sub-components
// ============================================================================

function PolicyBlock({ title, titleSuffix, description, active, children }: {
  title: string; titleSuffix?: string; description: string; active: boolean; children: ReactNode
}) {
  return (
    <section className={cn('rounded-lg p-3 shadow-sm', active ? 'bg-card' : 'bg-muted/30')}>
      <div className="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div>
          <div className="flex items-center gap-2">
            <div className="text-sm font-semibold">{title}</div>
            {titleSuffix && <span className="text-xs text-muted-foreground">{titleSuffix}</span>}
          </div>
          <p className="mt-1 text-xs text-muted-foreground">{description}</p>
        </div>
        <Badge tone={active ? 'success' : 'neutral'}>{active ? '生效中' : '未生效'}</Badge>
      </div>
      {children}
    </section>
  )
}

// ============================================================================
// AccountsPage
// ============================================================================

export function AccountsPage() {
  const queryClient = useQueryClient()
  const confirmDialog = useConfirm()
  const runtimeConfig = useRuntimeConfig()
  const pools = useQuery({ queryKey: ['accounts'], queryFn: getAccounts })
  const scenarioOptions = useQuery({ queryKey: ['route-scenario-options'], queryFn: getRouteScenarioOptions })
  const routeOptions = scenarioOptions.data?.routes ?? []
  const status = useQuery({ queryKey: ['accounts-status'], queryFn: getAccountsStatus, refetchInterval: 5000 })

  const [savingConfig, setSavingConfig] = useState(false)
  const [configDraft, setConfigDraft] = useState<AccountsConfig>(defaultAccountsConfig())
  const [modelRulesText, setModelRulesText] = useState('')
  const [pathRulesText, setPathRulesText] = useState('')
  const [retryStatusCodesText, setRetryStatusCodesText] = useState('')
  const [samePoolRetryStatusCodesText, setSamePoolRetryStatusCodesText] = useState('')
  const [createOpen, setCreateOpen] = useState(false)
  const [editingPool, setEditingPool] = useState<Account | null>(null)
  const [testingPool, setTestingPool] = useState<Account | null>(null)
  const [savingPool, setSavingPool] = useState(false)
  const [createForm, setCreateForm] = useState<AccountFormDraft>(() => defaultPoolForm())
  const [editForm, setEditForm] = useState<AccountFormDraft>(() => defaultPoolForm())

  useEffect(() => {
    const accounts = { ...defaultAccountsConfig(), ...runtimeConfig.data?.accounts }
    setConfigDraft(accounts)
    setModelRulesText(joinRules(accounts.directExternalModelRules))
    setPathRulesText(joinRules(accounts.directExternalPathRules))
    setRetryStatusCodesText(joinStatusCodeList(accounts.retryStatusCodes))
    setSamePoolRetryStatusCodesText(joinStatusCodeList(accounts.samePoolRetryStatusCodes))
  }, [runtimeConfig.data?.accounts])

  const statusMap = useMemo(() => {
    const map = new Map<number, NonNullable<typeof status.data>['pools'][number]>()
    status.data?.pools.forEach((item) => map.set(item.pool.id, item))
    return map
  }, [status.data])

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['accounts'] })
    queryClient.invalidateQueries({ queryKey: ['accounts-status'] })
    queryClient.invalidateQueries({ queryKey: ['runtime-config'] })
    queryClient.invalidateQueries({ queryKey: ['usage-records'] })
  }

  const saveConfig = async () => {
    if (!runtimeConfig.data) return toast.error('运行配置尚未加载')
    setSavingConfig(true)
    try {
      await updateRuntimeConfig({
        ...runtimeConfig.data,
        accounts: {
          ...configDraft,
          directExternalModelRules: splitRules(modelRulesText),
          directExternalPathRules: splitRules(pathRulesText),
          globalMaxConcurrentRequests: whole(configDraft.globalMaxConcurrentRequests),
          maxQueuedRequests: whole(configDraft.maxQueuedRequests),
          accountMaxInputTokens: whole(configDraft.accountMaxInputTokens),
          dispatchMaxWaitSecs: whole(configDraft.dispatchMaxWaitSecs, 1),
          retryMaxAttempts: whole(configDraft.retryMaxAttempts),
          retryStatusCodes: parseStatusCodeList(retryStatusCodesText),
          samePoolRetryCount: whole(configDraft.samePoolRetryCount),
          samePoolRetryStatusCodes: parseStatusCodeList(samePoolRetryStatusCodesText),
          samePoolRetryDelayMs: whole(configDraft.samePoolRetryDelayMs),
          transientFailurePriorityPenalty: whole(configDraft.transientFailurePriorityPenalty),
          accountTransientFailureCooldownThreshold: whole(configDraft.accountTransientFailureCooldownThreshold),
          accountLocalRescueMaxWaitSecs: whole(configDraft.accountLocalRescueMaxWaitSecs),
          localPoolCircuitWindowSecs: whole(configDraft.localPoolCircuitWindowSecs, 1),
          localPoolCircuitOpenAfterFailures: whole(configDraft.localPoolCircuitOpenAfterFailures, 1),
          localPoolCircuitRequireDistinctCredentials: whole(configDraft.localPoolCircuitRequireDistinctCredentials),
          localPoolCircuitMinRatioPercent: whole(configDraft.localPoolCircuitMinRatioPercent),
          localPoolCircuitOpenSecs: whole(configDraft.localPoolCircuitOpenSecs, 1),
          localPoolCircuitHalfOpenProbes: whole(configDraft.localPoolCircuitHalfOpenProbes, 1),
          autoDisableFailureThreshold: whole(configDraft.autoDisableFailureThreshold, 1),
          autoDisableWindowSecs: whole(configDraft.autoDisableWindowSecs, 1),
          autoDisableDurationSecs: whole(configDraft.autoDisableDurationSecs),
          rateLimitCooldownSecs: whole(configDraft.rateLimitCooldownSecs, 1),
          serverErrorCooldownSecs: whole(configDraft.serverErrorCooldownSecs, 1),
          networkErrorCooldownSecs: whole(configDraft.networkErrorCooldownSecs, 1),
          protocolErrorCooldownSecs: whole(configDraft.protocolErrorCooldownSecs, 1),
          modelUnavailableCooldownMode: configDraft.modelUnavailableCooldownMode,
          modelUnavailableCooldownSecs: whole(configDraft.modelUnavailableCooldownSecs, 1),
          requestTimeoutSecs: whole(configDraft.requestTimeoutSecs),
          streamRequestTimeoutSecs: whole(configDraft.streamRequestTimeoutSecs),
          streamIdleTimeoutSecs: whole(configDraft.streamIdleTimeoutSecs),
          streamFirstOutputTimeoutSecs: whole(configDraft.streamFirstOutputTimeoutSecs),
          streamPreOutputRetryEnabled: Boolean(configDraft.streamPreOutputRetryEnabled),
          usageProjectionUpliftPercent: whole(configDraft.usageProjectionUpliftPercent),
          usageProjectionCostFloorEnabled: Boolean(configDraft.usageProjectionCostFloorEnabled),
          usageProjectionCostFloorMarginPercent: whole(configDraft.usageProjectionCostFloorMarginPercent),
          usageProjectionOutputUpliftMinTokens: whole(configDraft.usageProjectionOutputUpliftMinTokens),
          usageProjectionOutputUpliftPercent: whole(configDraft.usageProjectionOutputUpliftPercent),
          usageDebugEnabled: Boolean(configDraft.usageDebugEnabled),
          usageDebugDir: String(configDraft.usageDebugDir || '').trim(),
          usageDebugMaxBodyBytes: whole(configDraft.usageDebugMaxBodyBytes),
          usageDebugMaxFiles: whole(configDraft.usageDebugMaxFiles),
        },
      })
      toast.success('外部账号策略已保存')
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    } finally {
      setSavingConfig(false)
    }
  }

  const submitPool = async () => {
    if (savingPool) return
    if (!createForm.name?.trim() || !createForm.baseUrl?.trim() || !createForm.apiKey?.trim()) return toast.error('名称、Base URL 和 Key 必填')
    setSavingPool(true)
    try {
      const { modelMappingRulesText, supportedModelsText, headerOverridesText, ...form } = createForm
      await createAccount({
        ...form,
        name: createForm.name.trim(),
        baseUrl: createForm.baseUrl.trim(),
        apiKey: createForm.apiKey.trim(),
        headerOverrides: parseHeaderOverridesText(headerOverridesText),
        streamResponseMode: createForm.streamResponseMode === 'inherit' ? null : createForm.streamResponseMode,
        priority: whole(createForm.priority ?? 100),
        maxConcurrentRequests: whole(createForm.maxConcurrentRequests ?? 10, 1),
        routeRules: form.routeMode === 'allow_all' ? [] : form.routeRules,
        modelMappingRules: parseModelMappingRules(modelMappingRulesText),
        supportedModels: parseSupportedModelsText(supportedModelsText),
      })
      toast.success('外部账号已添加')
      setCreateOpen(false)
      setCreateForm(defaultPoolForm())
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    } finally {
      setSavingPool(false)
    }
  }

  const startEdit = (pool: Account) => {
    setEditingPool(pool)
    setEditForm(poolFormFromPool(pool))
  }

  const savePoolEdit = async () => {
    if (!editingPool || savingPool) return
    if (!editForm.name?.trim() || !editForm.baseUrl?.trim()) return toast.error('名称和 Base URL 必填')
    setSavingPool(true)
    try {
      const { modelMappingRulesText, supportedModelsText, headerOverridesText, ...form } = editForm
      const payload: UpdateAccountRequest = {
        ...form,
        name: editForm.name.trim(),
        baseUrl: editForm.baseUrl.trim(),
        apiKey: editForm.apiKey?.trim() ? editForm.apiKey.trim() : undefined,
        headerOverrides: parseHeaderOverridesText(headerOverridesText),
        streamResponseMode: editForm.streamResponseMode === 'inherit' ? null : editForm.streamResponseMode,
        priority: whole(editForm.priority ?? 100),
        maxConcurrentRequests: whole(editForm.maxConcurrentRequests ?? 10, 1),
        routeRules: form.routeMode === 'allow_all' ? [] : form.routeRules,
        modelMappingRules: parseModelMappingRules(modelMappingRulesText),
        supportedModels: parseSupportedModelsText(supportedModelsText),
      }
      await updateAccount(editingPool.id, payload)
      toast.success('外部账号已更新')
      setEditingPool(null)
      setEditForm(defaultPoolForm())
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    } finally {
      setSavingPool(false)
    }
  }

  const mutatePool = async (action: () => Promise<unknown>, success: string) => {
    try { await action(); toast.success(success); invalidate() }
    catch (error) { toast.error(extractErrorMessage(error)) }
  }

  // Derived policy flags
  const externalEnabled = configDraft.enabled
  const directPolicyActive = externalEnabled && configDraft.externalDirectPolicyEnabled
  const fallbackActive = externalEnabled && !directPolicyActive && (
    configDraft.localPoolPreflightEnabled || configDraft.fallbackOnLocalCapacityExhausted ||
    configDraft.fallbackOnSchedulerRedisDegraded ||
    configDraft.fallbackOnNoAvailableCredentials || configDraft.fallbackOnLocalTransientExhausted ||
    configDraft.fallbackOnUnsupportedModel
  )
  const autoDisableActive = externalEnabled && configDraft.autoDisableEnabled
  const waitModeActive = externalEnabled && configDraft.capacityMode === 'wait'
  const localRescueActive = externalEnabled && !directPolicyActive && configDraft.accountLocalRescueEnabled
  const cacheUpliftActive = externalEnabled && configDraft.usageProjectionUpliftPercent > 0
  const outputUpliftActive = externalEnabled
    && configDraft.usageProjectionOutputUpliftMinTokens > 0
    && configDraft.usageProjectionOutputUpliftPercent > 0
  const usageCompensationActive = cacheUpliftActive
    || outputUpliftActive
    || configDraft.usageProjectionCostFloorEnabled
  const usageDebugActive = externalEnabled && configDraft.usageDebugEnabled
  const routePolicyActive = externalEnabled && (
    configDraft.accountRouteMode !== 'allow_all'
    || configDraft.accountRouteRules.length > 0
    || configDraft.localPoolRouteMode !== 'allow_all'
    || configDraft.localPoolRouteRules.length > 0
  )

  const poolStatuses = status.data?.pools ?? []
  const totalPools = pools.data?.pools.length ?? poolStatuses.length
  const dispatchablePools = poolStatuses.filter((item) => item.dispatchable).length
  const totalInFlight = poolStatuses.reduce((sum, item) => sum + item.inFlight, 0)
  const totalCapacity = poolStatuses.reduce((sum, item) => sum + item.pool.maxConcurrentRequests, 0)
  const currentPathPoolCount = pools.data?.pools.filter((pool) => pool.usageProjectionMode === 'current_path_policy').length ?? 0
  const concurrencyPct = totalCapacity > 0 ? Math.round((totalInFlight / totalCapacity) * 100) : 0

  const setCacheUpliftEnabled = (enabled: boolean) =>
    setConfigDraft((prev) => ({ ...prev, usageProjectionUpliftPercent: enabled ? (prev.usageProjectionUpliftPercent || 25) : 0 }))

  const setOutputUpliftEnabled = (enabled: boolean) =>
    setConfigDraft((prev) => ({
      ...prev,
      usageProjectionOutputUpliftMinTokens: enabled ? (prev.usageProjectionOutputUpliftMinTokens || 1000) : 0,
      usageProjectionOutputUpliftPercent: enabled ? (prev.usageProjectionOutputUpliftPercent || 25) : 0,
    }))

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.accounts.title}
        subtitle={pageMeta.accounts.subtitle}
      />

      <StatGrid>
        <StatCard title="外部账号" value={totalPools} tone="info" icon={
          <ProgressRing value={concurrencyPct} size={40} strokeWidth={4} color="hsl(var(--info))" label={`${concurrencyPct}%`} />
        } />
        <StatCard title="可调度" value={dispatchablePools} tone={dispatchablePools > 0 ? 'success' : 'warning'} />
        <StatCard title="外部并发" value={`${totalInFlight}/${totalCapacity || 0}`} tone="default" />
        <StatCard title="按路径整理 usage" value={`${currentPathPoolCount} 个`} />
        <StatCard
          title="入口策略"
          value={fallbackActive || directPolicyActive || routePolicyActive ? '已配置' : '未配置'}
          tone={fallbackActive || directPolicyActive || routePolicyActive ? 'success' : 'warning'}
        />
      </StatGrid>

      {/* Policy config section */}
      <SectionCard
        title="外部账号策略"
        actions={
          <Button size="sm" disabled={savingConfig} onClick={saveConfig}>
            <Save className="h-4 w-4" />
            {savingConfig ? '保存中...' : '保存策略'}
          </Button>
        }
      >
        <div className="space-y-5">
          <PolicyBlock title="启用控制" active={externalEnabled} description="关闭后不会进入任何外部账号，请求只走本地账号。">
            <div className="grid gap-3 md:grid-cols-2">
              <ToggleRow label="启用外部账号" checked={configDraft.enabled} onChange={(v) => setConfigDraft((p) => ({ ...p, enabled: v }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="路由策略"
            titleSuffix={!externalEnabled ? '需先启用外部账号' : undefined}
            active={externalEnabled}
            description="控制哪些入口进入本地账号或外部账号。单个账号的规则和整套切换在路由场景页管理。"
          >
            <div className="grid gap-4 lg:grid-cols-2">
              <FormSection title="本地账号路由" description="决定哪些入口允许进入本地账号；其余入口会按账号策略处理。">
                <div className="grid gap-3 md:grid-cols-2">
                  <SelectBox
                    disabled={!externalEnabled}
                    label="路由模式"
                    value={configDraft.localPoolRouteMode}
                    onChange={(localPoolRouteMode) => setConfigDraft((prev) => ({
                      ...prev,
                      localPoolRouteMode: normalizeRouteRuleMode(localPoolRouteMode),
                    }))}
                  >
                    <SelectItem value="allow_all">全部入口允许进入本地账号</SelectItem>
                    <SelectItem value="allow_list">只允许下列入口进入本地账号</SelectItem>
                    <SelectItem value="deny_list">禁止下列入口进入本地账号</SelectItem>
                  </SelectBox>
                  <div className="min-w-0 space-y-1.5">
                    <div className="text-sm font-medium">路由</div>
                    <RouteSelector
                      routes={routeOptions}
                      value={configDraft.localPoolRouteRules}
                      disabled={!externalEnabled || configDraft.localPoolRouteMode === 'allow_all'}
                      onChange={(localPoolRouteRules) => setConfigDraft((prev) => ({ ...prev, localPoolRouteRules }))}
                    />
                  </div>
                </div>
              </FormSection>

              <FormSection title="外部账号路由" description="决定哪些入口允许进入外部账号；默认允许全部入口。">
                <div className="grid gap-3 md:grid-cols-2">
                  <SelectBox
                    disabled={!externalEnabled}
                    label="路由模式"
                    value={configDraft.accountRouteMode}
                    onChange={(accountRouteMode) => setConfigDraft((prev) => ({
                      ...prev,
                      accountRouteMode: normalizeRouteRuleMode(accountRouteMode),
                    }))}
                  >
                    <SelectItem value="allow_all">全部入口允许进入外部账号</SelectItem>
                    <SelectItem value="allow_list">只允许下列入口进入外部账号</SelectItem>
                    <SelectItem value="deny_list">禁止下列入口进入外部账号</SelectItem>
                  </SelectBox>
                  <div className="min-w-0 space-y-1.5">
                    <div className="text-sm font-medium">路由</div>
                    <RouteSelector
                      routes={routeOptions}
                      value={configDraft.accountRouteRules}
                      disabled={!externalEnabled || configDraft.accountRouteMode === 'allow_all'}
                      onChange={(accountRouteRules) => setConfigDraft((prev) => ({ ...prev, accountRouteRules }))}
                    />
                  </div>
                </div>
              </FormSection>
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="触发条件"
            titleSuffix={!externalEnabled ? '需先启用外部账号' : undefined}
            active={Boolean(fallbackActive || directPolicyActive)}
            description="启用显式直连后，所有请求跳过本地账号，只调度外部账号；关闭后才使用本地优先 fallback。"
          >
            <div className="grid gap-4 lg:grid-cols-2">
              <FormSection title="本地优先" description="先调度本地账号，只有下面情况出现时才转入外部账号。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="本地容量预检" checked={configDraft.localPoolPreflightEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolPreflightEnabled: v }))} />
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="容量不足时使用外部账号" checked={configDraft.fallbackOnLocalCapacityExhausted} onChange={(v) => setConfigDraft((p) => ({ ...p, fallbackOnLocalCapacityExhausted: v }))} />
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="调度 Redis 降级时使用外部账号" checked={configDraft.fallbackOnSchedulerRedisDegraded} onChange={(v) => setConfigDraft((p) => ({ ...p, fallbackOnSchedulerRedisDegraded: v }))} />
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="没有可用账号时使用外部账号" checked={configDraft.fallbackOnNoAvailableCredentials} onChange={(v) => setConfigDraft((p) => ({ ...p, fallbackOnNoAvailableCredentials: v }))} />
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="本地临时错误过多时使用外部账号" checked={configDraft.fallbackOnLocalTransientExhausted} onChange={(v) => setConfigDraft((p) => ({ ...p, fallbackOnLocalTransientExhausted: v }))} />
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="模型不支持时使用外部账号" checked={configDraft.fallbackOnUnsupportedModel} onChange={(v) => setConfigDraft((p) => ({ ...p, fallbackOnUnsupportedModel: v }))} />
                </div>
              </FormSection>
              <FormSection title="显式直连" description="开关打开即全量直连外部账号；模型和路径规则只用于细分记录的直连原因。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <ToggleRow disabled={!externalEnabled} label="启用显式直连" checked={configDraft.externalDirectPolicyEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, externalDirectPolicyEnabled: v }))} />
                  <ToggleRow disabled={!directPolicyActive} label="记录本地保护原因" checked={configDraft.directExternalOnLocalMaintenance} onChange={(v) => setConfigDraft((p) => ({ ...p, directExternalOnLocalMaintenance: v }))} />
                </div>
                <div className="mt-3 grid gap-3 sm:grid-cols-2">
                  <TextAreaBox disabled={!directPolicyActive} label="模型原因规则" value={modelRulesText} onChange={setModelRulesText} />
                  <TextAreaBox disabled={!directPolicyActive} label="路径原因规则" value={pathRulesText} onChange={setPathRulesText} />
                </div>
                <div className="mt-3 grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <ToggleRow label="启用本地保护统计" checked={configDraft.localPoolCircuitEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitEnabled: v }))} />
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="统计窗口" suffix="秒" value={configDraft.localPoolCircuitWindowSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitWindowSecs: v }))} />
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="至少被风控账号" suffix="个" value={configDraft.localPoolCircuitRequireDistinctCredentials} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitRequireDistinctCredentials: v, localPoolCircuitOpenAfterFailures: v }))} />
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="且占可用账号" suffix="%" value={configDraft.localPoolCircuitMinRatioPercent} min={0} max={100} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitMinRatioPercent: v }))} />
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="暂停时长" suffix="秒" value={configDraft.localPoolCircuitOpenSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitOpenSecs: v }))} />
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="恢复时探测请求" suffix="个" value={configDraft.localPoolCircuitHalfOpenProbes} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitHalfOpenProbes: v }))} />
                </div>
              </FormSection>
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="调度策略"
            titleSuffix={!externalEnabled ? '需先启用外部账号' : undefined}
            active={externalEnabled}
            description="控制外部账号自己的并发、排队、重试和超时。"
          >
            <div className="space-y-4">
              <FormSection title="容量与排队" description={waitModeActive ? '外部账号满并发时会等待容量。' : '外部账号满并发时不会排队。'}>
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <SelectBox disabled={!externalEnabled} label="满并发处理" value={configDraft.capacityMode} onChange={(v) => setConfigDraft((p) => ({ ...p, capacityMode: v as AccountsConfig['capacityMode'] }))}>
                    <SelectItem value="fail_fast">立即失败</SelectItem>
                    <SelectItem value="wait">等待容量</SelectItem>
                  </SelectBox>
                  <NumberBox disabled={!externalEnabled} label="全局并发上限" description="限制同时进行的外部账号请求数；不是 RPM。0 表示不限。" suffix="并发" value={configDraft.globalMaxConcurrentRequests} onChange={(v) => setConfigDraft((p) => ({ ...p, globalMaxConcurrentRequests: v }))} />
                  <NumberBox disabled={!waitModeActive} label="账号排队上限" description="maxQueuedRequests；只限制账号 wait 队列，不是本地账号 dispatch 队列。" suffix="请求" value={configDraft.maxQueuedRequests} onChange={(v) => setConfigDraft((p) => ({ ...p, maxQueuedRequests: v }))} />
                  <NumberBox disabled={!externalEnabled} label="估算输入上限（兼容）" description="保留历史配置；不再作为本地发送前拒绝条件，真实上下文超限以上游响应和请求大小保护为准。" suffix="Token" value={configDraft.accountMaxInputTokens} onChange={(v) => setConfigDraft((p) => ({ ...p, accountMaxInputTokens: v }))} />
                  <NumberBox disabled={!waitModeActive} label="最大等待" description="必须大于 0；旧配置中的 0 按安全默认值 30 秒处理。" suffix="秒" min={1} value={configDraft.dispatchMaxWaitSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, dispatchMaxWaitSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="账号最多尝试" description="控制同一请求最多尝试多少个外部账号；0 表示按候选账号自动尝试一轮。" suffix="池" value={configDraft.retryMaxAttempts} onChange={(v) => setConfigDraft((p) => ({ ...p, retryMaxAttempts: v }))} />
                  <ToggleRow label="网络错误跨池重试" description="连接、DNS、超时等没有 HTTP 状态码的错误，是否允许切换其他账号。" checked={configDraft.retryOnNetworkError} onChange={(v) => setConfigDraft((p) => ({ ...p, retryOnNetworkError: v }))} />
                  <ToggleRow label="协议错误跨池重试" description="成功状态码但返回错误信封或协议污染时，是否允许切换其他账号。" checked={configDraft.retryOnProtocolError} onChange={(v) => setConfigDraft((p) => ({ ...p, retryOnProtocolError: v }))} />
                  <TextAreaBox disabled={!externalEnabled} label="跨池重试状态码" value={retryStatusCodesText} onChange={setRetryStatusCodesText} />
                  <NumberBox disabled={!externalEnabled} label="同池重试次数" description="命中下面状态码时，先在同一个外部账号上重试；重试耗尽后才冷却并尝试其他外部账号。" suffix="次" value={configDraft.samePoolRetryCount} onChange={(v) => setConfigDraft((p) => ({ ...p, samePoolRetryCount: v }))} />
                  <NumberBox disabled={!externalEnabled || configDraft.samePoolRetryCount <= 0} label="同池重试间隔" suffix="毫秒" value={configDraft.samePoolRetryDelayMs} onChange={(v) => setConfigDraft((p) => ({ ...p, samePoolRetryDelayMs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="失败池临时降权" description="每个瞬态失败窗口内的失败次数都会临时增加有效优先级；默认 20，可让优先级 1 的故障池让位给 10/20 的健康池，0 表示关闭。" suffix="优先级" min={0} value={configDraft.transientFailurePriorityPenalty} onChange={(v) => setConfigDraft((p) => ({ ...p, transientFailurePriorityPenalty: v }))} />
                  <NumberBox disabled={!externalEnabled} label="连续失败计数（兼容）" description="保留旧配置字段；普通瞬态失败只降低调度优先级，不会触发池级冷却。池级冷却仅用于模型不可用、端点错误或自动禁用等极端状态。" suffix="次" min={0} value={configDraft.accountTransientFailureCooldownThreshold} onChange={(v) => setConfigDraft((p) => ({ ...p, accountTransientFailureCooldownThreshold: v }))} />
                  <TextAreaBox disabled={!externalEnabled || configDraft.samePoolRetryCount <= 0} label="同池重试状态码" value={samePoolRetryStatusCodesText} onChange={setSamePoolRetryStatusCodesText} />
                </div>
              </FormSection>
              <FormSection title="冷却与超时">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <NumberBox disabled={!externalEnabled} label="429 冷却" suffix="秒" value={configDraft.rateLimitCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, rateLimitCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="5xx 冷却" suffix="秒" value={configDraft.serverErrorCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, serverErrorCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="网络错误冷却" suffix="秒" value={configDraft.networkErrorCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, networkErrorCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="协议/认证冷却" suffix="秒" value={configDraft.protocolErrorCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, protocolErrorCooldownSecs: v }))} />
                  <SelectBox disabled={!externalEnabled} label="模型不可用冷却范围" value={configDraft.modelUnavailableCooldownMode} onChange={(v) => setConfigDraft((p) => ({ ...p, modelUnavailableCooldownMode: v as AccountsConfig['modelUnavailableCooldownMode'] }))}>
                    <SelectItem value="model">仅当前模型</SelectItem>
                    <SelectItem value="pool">整个外部账号</SelectItem>
                    <SelectItem value="disabled">不写冷却</SelectItem>
                  </SelectBox>
                  <NumberBox disabled={!externalEnabled || configDraft.modelUnavailableCooldownMode === 'disabled'} label="模型不可用冷却" suffix="秒" value={configDraft.modelUnavailableCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, modelUnavailableCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="非流式总超时" suffix="秒" value={configDraft.requestTimeoutSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, requestTimeoutSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="流式总超时" suffix="秒" value={configDraft.streamRequestTimeoutSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, streamRequestTimeoutSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="流式空闲超时" suffix="秒" value={configDraft.streamIdleTimeoutSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, streamIdleTimeoutSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="首个输出超时" suffix="秒" value={configDraft.streamFirstOutputTimeoutSecs} onChange={(v) => setConfigDraft((prev) => ({ ...prev, streamFirstOutputTimeoutSecs: v }))} />
                  <ToggleRow disabled={!externalEnabled} label="流式首输出前错误换池" description="stream 已返回 200 但还没提交 content、thinking 或 tool_use 前遇到 error、断流、EOF 或空闲超时时，允许在外部账号内换池。" checked={configDraft.streamPreOutputRetryEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, streamPreOutputRetryEnabled: v }))} />
                </div>
              </FormSection>
              <FormSection title="流式 SSE 默认转发" description="作为外部账号默认值；单个外部账号仍可在编辑弹窗中覆盖。">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <SelectBox
                    disabled={!externalEnabled}
                    label="SSE 默认转发"
                    value={configDraft.streamResponseMode}
                    onChange={(v) => setConfigDraft((p) => ({
                      ...p,
                      streamResponseMode: v as AccountsConfig['streamResponseMode'],
                    }))}
                  >
                    <SelectItem value="event_passthrough">SSE 事件级透传</SelectItem>
                  </SelectBox>
                </div>
              </FormSection>
              <FormSection title="外部账号失败后回本地" description="仅对本地优先 fallback 到外部账号的请求生效；显式直连开启时不会回本地。">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="启用回本地" checked={configDraft.accountLocalRescueEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, accountLocalRescueEnabled: v }))} />
                  <ToggleRow disabled={!localRescueActive} label="429 时回本地" checked={configDraft.accountLocalRescueOnRateLimit} onChange={(v) => setConfigDraft((p) => ({ ...p, accountLocalRescueOnRateLimit: v }))} />
                  <ToggleRow disabled={!localRescueActive} label="超时时回本地" checked={configDraft.accountLocalRescueOnTimeout} onChange={(v) => setConfigDraft((p) => ({ ...p, accountLocalRescueOnTimeout: v }))} />
                  <ToggleRow disabled={!localRescueActive} label="容量失败回本地" checked={configDraft.accountLocalRescueOnCapacity} onChange={(v) => setConfigDraft((p) => ({ ...p, accountLocalRescueOnCapacity: v }))} />
                  <NumberBox disabled={!localRescueActive} label="回本地最多等待" suffix="秒" value={configDraft.accountLocalRescueMaxWaitSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, accountLocalRescueMaxWaitSecs: v }))} />
                </div>
              </FormSection>
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="自动禁用"
            titleSuffix={!externalEnabled ? '需先启用外部账号' : undefined}
            active={autoDisableActive}
            description="自动禁用只作用于外部账号本身；单个外部账号可选择继承、强制启用或关闭。"
          >
            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
              <ToggleRow disabled={!externalEnabled} label="启用自动禁用" checked={configDraft.autoDisableEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableEnabled: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="认证错误" checked={configDraft.autoDisableOnAuthError} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableOnAuthError: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="安全锁定" checked={configDraft.autoDisableOnSecurityLock} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableOnSecurityLock: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="额度耗尽" checked={configDraft.autoDisableOnQuotaExhausted} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableOnQuotaExhausted: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="配置错误" checked={configDraft.autoDisableOnMisconfiguredEndpoint} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableOnMisconfiguredEndpoint: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="通道禁用" checked={configDraft.autoDisableOnChannelDisabled} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableOnChannelDisabled: v }))} />
              <NumberBox disabled={!autoDisableActive} label="触发阈值" suffix="次" value={configDraft.autoDisableFailureThreshold} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableFailureThreshold: v }))} />
              <NumberBox disabled={!autoDisableActive} label="统计窗口" suffix="秒" value={configDraft.autoDisableWindowSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableWindowSecs: v }))} />
              <NumberBox disabled={!autoDisableActive} label="禁用时长" suffix="秒" value={configDraft.autoDisableDurationSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, autoDisableDurationSecs: v }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="用量补偿"
            titleSuffix={!externalEnabled ? '需先启用外部账号' : undefined}
            active={externalEnabled && usageCompensationActive}
            description={'仅对下游 usage 口径为“按当前入口路径整理 usage”的外部账号生效；选择“透传上游 usage”的账号不受影响。'}
          >
            <div className="space-y-4">
              <FormSection title="成本底线" description="用最终返回给调用方的 usage 覆盖原始上游成本；有上游缓存证据时优先补缓存字段，并遵守当前路径上限。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <ToggleRow disabled={!externalEnabled} label="启用成本底线" checked={Boolean(configDraft.usageProjectionCostFloorEnabled)} onChange={(v) => setConfigDraft((p) => ({ ...p, usageProjectionCostFloorEnabled: v }))} />
                  <NumberBox disabled={!externalEnabled || !configDraft.usageProjectionCostFloorEnabled} label="成本补齐余量" suffix="%" min={0} value={configDraft.usageProjectionCostFloorMarginPercent} onChange={(v) => setConfigDraft((p) => ({ ...p, usageProjectionCostFloorMarginPercent: v }))} />
                </div>
              </FormSection>
              <div className="grid gap-4 lg:grid-cols-2">
                <FormSection title="缓存读写补偿">
                  <div className="grid gap-3 sm:grid-cols-2">
                    <ToggleRow disabled={!externalEnabled} label="启用缓存补偿" checked={cacheUpliftActive} onChange={setCacheUpliftEnabled} />
                    <NumberBox disabled={!cacheUpliftActive} label="放大百分比" suffix="%" value={configDraft.usageProjectionUpliftPercent} onChange={(v) => setConfigDraft((p) => ({ ...p, usageProjectionUpliftPercent: v }))} />
                  </div>
                </FormSection>
                <FormSection title="输出用量补偿">
                  <div className="grid gap-3 sm:grid-cols-3">
                    <ToggleRow disabled={!externalEnabled} label="启用输出补偿" checked={outputUpliftActive} onChange={setOutputUpliftEnabled} />
                    <NumberBox disabled={!outputUpliftActive} label="输出阈值" suffix="Token" value={configDraft.usageProjectionOutputUpliftMinTokens} onChange={(v) => setConfigDraft((p) => ({ ...p, usageProjectionOutputUpliftMinTokens: v }))} />
                    <NumberBox disabled={!outputUpliftActive} label="放大百分比" suffix="%" value={configDraft.usageProjectionOutputUpliftPercent} onChange={(v) => setConfigDraft((p) => ({ ...p, usageProjectionOutputUpliftPercent: v }))} />
                  </div>
                </FormSection>
              </div>
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="诊断记录"
            titleSuffix={!externalEnabled ? '需先启用外部账号' : undefined}
            active={usageDebugActive}
            description="临时保存账号上游原始响应/SSE usage 样本、请求关联信息和本系统解析结果；用于排查 output_tokens 为 0 等异常。"
          >
            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
              <ToggleRow disabled={!externalEnabled} label="启用 usage 原始数据诊断" checked={configDraft.usageDebugEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, usageDebugEnabled: v }))} />
              <TextBox className="xl:col-span-3" disabled={!usageDebugActive} label="诊断目录" description="容器内路径；记录失败只写服务日志，不影响请求。" value={configDraft.usageDebugDir} onChange={(v) => setConfigDraft((p) => ({ ...p, usageDebugDir: v }))} />
              <NumberBox disabled={!usageDebugActive} label="单条原始片段上限" suffix="Bytes" value={configDraft.usageDebugMaxBodyBytes} onChange={(v) => setConfigDraft((p) => ({ ...p, usageDebugMaxBodyBytes: v }))} />
              <NumberBox disabled={!usageDebugActive} label="最多诊断文件" suffix="个" value={configDraft.usageDebugMaxFiles} onChange={(v) => setConfigDraft((p) => ({ ...p, usageDebugMaxFiles: v }))} />
            </div>
          </PolicyBlock>
        </div>
      </SectionCard>

      {/* Pool list */}
      <SectionCard
        title="外部账号列表"
        description="单个外部账号配置只影响自身；全局调度、冷却、补偿策略在上方统一保存。"
        actions={
          <Button size="sm" onClick={() => { setCreateForm(defaultPoolForm()); setCreateOpen(true) }}>
            <Plus className="h-4 w-4" />添加外部账号
          </Button>
        }
      >
        {pools.isLoading ? (
          <LoadingState />
        ) : !pools.data?.pools.length ? (
          <EmptyState title="暂无外部账号" description="点击右上角按钮添加第一个外部账号。" />
        ) : (
          <div className="space-y-3">
            {pools.data.pools.map((pool) => {
              const runtime = statusMap.get(pool.id)
              const inFlight = runtime?.inFlight ?? 0
              const capacity = pool.maxConcurrentRequests
              const usePct = capacity > 0 ? Math.round((inFlight / capacity) * 100) : 0
              const qualitySummary = accountQualitySummary(runtime?.quality)
              return (
                <div key={pool.id} className="rounded-lg bg-card p-4 shadow-sm">
                  <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
                    <div className="flex items-start gap-3">
                      <ProgressRing
                        value={usePct}
                        size={44}
                        strokeWidth={4}
                        color={pool.enabled && !pool.autoDisabled ? 'hsl(var(--success))' : 'hsl(var(--destructive))'}
                        label={`${usePct}%`}
                        className="mt-0.5 shrink-0"
                      />
                      <div className="min-w-0 space-y-1.5">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="font-semibold">#{pool.id} {pool.name}</span>
                          <Badge tone={pool.enabled ? 'success' : 'neutral'}>{pool.enabled ? '启用' : '停用'}</Badge>
                          {pool.autoDisabled && <Badge tone="error">自动禁用</Badge>}
                          <Badge tone={runtime?.dispatchable ? 'info' : 'neutral'}>{runtime?.dispatchable ? '可调度' : runtime?.skippedReason || '不可调度'}</Badge>
                        </div>
                        <div className="text-sm text-muted-foreground">{pool.baseUrl} · {pool.maskedApiKey || '未显示 Key'} · 并发 {inFlight}/{capacity} · 优先级 {pool.priority}</div>
                        <div className="text-xs text-muted-foreground">{poolUsageSummary(pool, configDraft)} · {streamRetrySummary(pool, configDraft)} · {poolRouteSummary(pool)} · {poolBodyModeSummary(pool)} · {poolHeaderProfileSummary(pool)} · 认证：{authLabel(pool.authType)} · 模型：{poolModelMappingSummary(pool)} · {poolSupportedModelsSummary(pool)}{runtime?.cooldownRemainingSecs ? ` · 冷却 ${runtime.cooldownRemainingSecs}s` : ''}{runtime?.transientFailureStreak ? ` · 失败窗口 ${runtime.transientFailureStreak} 次/${runtime.transientFailureTtlSecs}s` : ''}</div>
                        {qualitySummary && <div className="text-xs text-muted-foreground">{qualitySummary}</div>}
                        {pool.autoDisabledLastError && <div className="text-xs text-destructive">{pool.autoDisabledLastError}</div>}
                      </div>
                    </div>
                    <div className="flex flex-wrap gap-1.5 lg:shrink-0">
                      <Button variant="ghost" size="xs" onClick={() => startEdit(pool)}><Pencil className="h-3.5 w-3.5" />编辑</Button>
                      <Button variant="ghost" size="xs" onClick={() => setTestingPool(pool)}><FlaskConical className="h-3.5 w-3.5" />测试</Button>
                      <Button variant="ghost" size="xs" onClick={() => mutatePool(() => setAccountEnabled(pool.id, !pool.enabled), pool.enabled ? '已停用' : '已启用')}>
                        <Power className="h-3.5 w-3.5" />{pool.enabled ? '停用' : '启用'}
                      </Button>
                      <Button variant="ghost" size="xs" onClick={() => mutatePool(() => clearAccountAutoDisabled(pool.id), '自动禁用状态已清除')}>
                        <RotateCcw className="h-3.5 w-3.5" />清除禁用
                      </Button>
                      <Button variant="ghost" size="xs" onClick={() => mutatePool(() => clearAccountCooldown(pool.id), '冷却状态已清除')}>
                        <RotateCcw className="h-3.5 w-3.5" />清除冷却
                      </Button>
                      <Button variant="ghost" size="xs" onClick={() => status.refetch()}><RefreshCw className="h-3.5 w-3.5" />刷新</Button>
                      <Button
                        variant="ghost" size="xs"
                        className="text-destructive hover:bg-destructive/10 hover:text-destructive"
                        onClick={async () => {
                          const confirmed = await confirmDialog({ title: '删除外部账号', message: `删除外部账号「${pool.name}」？此操作无法撤销。`, confirmText: '删除', tone: 'danger' })
                          if (confirmed) mutatePool(() => deleteAccount(pool.id), '外部账号已删除')
                        }}
                      >
                        <Trash2 className="h-3.5 w-3.5" />删除
                      </Button>
                    </div>
                  </div>
                </div>
              )
            })}
          </div>
        )}
      </SectionCard>

      <AccountFormModal
        mode="create"
        open={createOpen}
        draft={createForm}
        saving={savingPool}
        onDraftChange={setCreateForm}
        onDiscoverSupportedModels={async () => {
          if (!createForm.baseUrl.trim() || !createForm.apiKey.trim()) {
            throw new Error('请先填写外部账号 Base URL 和 Key')
          }
          const response = await discoverAccountSupportedModels({
            baseUrl: createForm.baseUrl.trim(),
            apiKey: createForm.apiKey.trim(),
            authType: createForm.authType,
          })
          return response.supportedModels
        }}
        onClose={() => { if (savingPool) return; setCreateOpen(false); setCreateForm(defaultPoolForm()) }}
        onSubmit={submitPool}
      />
      <AccountFormModal
        mode="edit"
        pool={editingPool}
        open={Boolean(editingPool)}
        draft={editForm}
        saving={savingPool}
        onDraftChange={setEditForm}
        onDiscoverSupportedModels={async () => {
          if (!editingPool) return []
          const response = await discoverStoredAccountSupportedModels(editingPool.id, {
            baseUrl: editForm.baseUrl.trim() || null,
            apiKey: editForm.apiKey.trim() || null,
            authType: editForm.authType,
          })
          return response.supportedModels
        }}
        onClose={() => { if (savingPool) return; setEditingPool(null); setEditForm(defaultPoolForm()) }}
        onSubmit={savePoolEdit}
      />
      <AccountTestModal
        pool={testingPool}
        open={Boolean(testingPool)}
        onClose={() => setTestingPool(null)}
        onDone={invalidate}
      />
    </PageContainer>
  )
}
