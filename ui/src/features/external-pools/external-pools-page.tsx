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
  clearExternalPoolAutoDisabled,
  createExternalPool,
  deleteExternalPool,
  discoverExternalPoolSupportedModels,
  discoverStoredExternalPoolSupportedModels,
  getExternalPools,
  getExternalPoolsStatus,
  setExternalPoolEnabled,
  updateExternalPool,
  updateRuntimeConfig,
} from '@/api/credentials'
import { defaultExternalPoolsConfig } from '@/lib/runtime-config-defaults'
import { useRuntimeConfig } from '@/hooks/use-credentials'
import { extractErrorMessage, cn } from '@/lib/utils'
import type { ExternalPool, ExternalPoolsConfig, UpdateExternalPoolRequest } from '@/types/api'
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
  type ExternalPoolFormDraft,
  authLabel,
  defaultPoolForm,
  joinRules,
  parseModelMappingRules,
  parseSupportedModelsText,
  poolFormFromPool,
  poolBodyModeSummary,
  poolModelMappingSummary,
  poolSupportedModelsSummary,
  poolUsageSummary,
  splitRules,
  whole,
} from './external-pool-utils'
import { ExternalPoolFormModal } from './external-pool-form-modal'
import { ExternalPoolTestModal } from './external-pool-test-modal'
import {
  FormSection,
  NumberBox,
  SelectBox,
  TextAreaBox,
  ToggleRow,
} from './external-pool-components'

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
// ExternalPoolsPage
// ============================================================================

export function ExternalPoolsPage() {
  const queryClient = useQueryClient()
  const confirmDialog = useConfirm()
  const runtimeConfig = useRuntimeConfig()
  const pools = useQuery({ queryKey: ['external-pools'], queryFn: getExternalPools })
  const status = useQuery({ queryKey: ['external-pools-status'], queryFn: getExternalPoolsStatus, refetchInterval: 5000 })

  const [savingConfig, setSavingConfig] = useState(false)
  const [configDraft, setConfigDraft] = useState<ExternalPoolsConfig>(defaultExternalPoolsConfig())
  const [modelRulesText, setModelRulesText] = useState('')
  const [pathRulesText, setPathRulesText] = useState('')
  const [createOpen, setCreateOpen] = useState(false)
  const [editingPool, setEditingPool] = useState<ExternalPool | null>(null)
  const [testingPool, setTestingPool] = useState<ExternalPool | null>(null)
  const [savingPool, setSavingPool] = useState(false)
  const [createForm, setCreateForm] = useState<ExternalPoolFormDraft>(() => defaultPoolForm())
  const [editForm, setEditForm] = useState<ExternalPoolFormDraft>(() => defaultPoolForm())

  useEffect(() => {
    const externalPools = { ...defaultExternalPoolsConfig(), ...runtimeConfig.data?.externalPools }
    setConfigDraft(externalPools)
    setModelRulesText(joinRules(externalPools.directExternalModelRules))
    setPathRulesText(joinRules(externalPools.directExternalPathRules))
  }, [runtimeConfig.data?.externalPools])

  const statusMap = useMemo(() => {
    const map = new Map<number, NonNullable<typeof status.data>['pools'][number]>()
    status.data?.pools.forEach((item) => map.set(item.pool.id, item))
    return map
  }, [status.data])

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ['external-pools'] })
    queryClient.invalidateQueries({ queryKey: ['external-pools-status'] })
    queryClient.invalidateQueries({ queryKey: ['runtime-config'] })
    queryClient.invalidateQueries({ queryKey: ['usage-records'] })
  }

  const saveConfig = async () => {
    if (!runtimeConfig.data) return toast.error('运行配置尚未加载')
    setSavingConfig(true)
    try {
      await updateRuntimeConfig({
        ...runtimeConfig.data,
        externalPools: {
          ...configDraft,
          directExternalModelRules: splitRules(modelRulesText),
          directExternalPathRules: splitRules(pathRulesText),
          externalPoolGlobalMaxConcurrentRequests: whole(configDraft.externalPoolGlobalMaxConcurrentRequests),
          externalPoolMaxQueuedRequests: whole(configDraft.externalPoolMaxQueuedRequests),
          externalPoolMaxInputTokens: whole(configDraft.externalPoolMaxInputTokens),
          externalPoolDispatchMaxWaitSecs: whole(configDraft.externalPoolDispatchMaxWaitSecs, 1),
          externalPoolRetryMaxAttempts: whole(configDraft.externalPoolRetryMaxAttempts),
          externalPoolLocalRescueMaxWaitSecs: whole(configDraft.externalPoolLocalRescueMaxWaitSecs),
          localPoolCircuitWindowSecs: whole(configDraft.localPoolCircuitWindowSecs, 1),
          localPoolCircuitOpenAfterFailures: whole(configDraft.localPoolCircuitOpenAfterFailures, 1),
          localPoolCircuitRequireDistinctCredentials: whole(configDraft.localPoolCircuitRequireDistinctCredentials),
          localPoolCircuitOpenSecs: whole(configDraft.localPoolCircuitOpenSecs, 1),
          externalPoolAutoDisableFailureThreshold: whole(configDraft.externalPoolAutoDisableFailureThreshold, 1),
          externalPoolAutoDisableWindowSecs: whole(configDraft.externalPoolAutoDisableWindowSecs, 1),
          externalPoolAutoDisableDurationSecs: whole(configDraft.externalPoolAutoDisableDurationSecs),
          externalPoolRateLimitCooldownSecs: whole(configDraft.externalPoolRateLimitCooldownSecs, 1),
          externalPoolServerErrorCooldownSecs: whole(configDraft.externalPoolServerErrorCooldownSecs, 1),
          externalPoolNetworkErrorCooldownSecs: whole(configDraft.externalPoolNetworkErrorCooldownSecs, 1),
          externalPoolProtocolErrorCooldownSecs: whole(configDraft.externalPoolProtocolErrorCooldownSecs, 1),
          externalPoolModelUnavailableCooldownMode: configDraft.externalPoolModelUnavailableCooldownMode,
          externalPoolModelUnavailableCooldownSecs: whole(configDraft.externalPoolModelUnavailableCooldownSecs, 1),
          externalPoolRequestTimeoutSecs: whole(configDraft.externalPoolRequestTimeoutSecs),
          externalPoolStreamRequestTimeoutSecs: whole(configDraft.externalPoolStreamRequestTimeoutSecs),
          externalPoolStreamIdleTimeoutSecs: whole(configDraft.externalPoolStreamIdleTimeoutSecs),
          externalPoolUsageProjectionUpliftPercent: whole(configDraft.externalPoolUsageProjectionUpliftPercent),
          externalPoolUsageProjectionOutputUpliftMinTokens: whole(configDraft.externalPoolUsageProjectionOutputUpliftMinTokens),
          externalPoolUsageProjectionOutputUpliftPercent: whole(configDraft.externalPoolUsageProjectionOutputUpliftPercent),
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
      const { modelMappingRulesText, supportedModelsText, ...form } = createForm
      await createExternalPool({
        ...form,
        name: createForm.name.trim(),
        baseUrl: createForm.baseUrl.trim(),
        apiKey: createForm.apiKey.trim(),
        streamResponseMode: createForm.streamResponseMode === 'inherit' ? null : createForm.streamResponseMode,
        priority: whole(createForm.priority ?? 100),
        maxConcurrentRequests: whole(createForm.maxConcurrentRequests ?? 10, 1),
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

  const startEdit = (pool: ExternalPool) => {
    setEditingPool(pool)
    setEditForm(poolFormFromPool(pool))
  }

  const savePoolEdit = async () => {
    if (!editingPool || savingPool) return
    if (!editForm.name?.trim() || !editForm.baseUrl?.trim()) return toast.error('名称和 Base URL 必填')
    setSavingPool(true)
    try {
      const { modelMappingRulesText, supportedModelsText, ...form } = editForm
      const payload: UpdateExternalPoolRequest = {
        ...form,
        name: editForm.name.trim(),
        baseUrl: editForm.baseUrl.trim(),
        apiKey: editForm.apiKey?.trim() ? editForm.apiKey.trim() : undefined,
        streamResponseMode: editForm.streamResponseMode === 'inherit' ? null : editForm.streamResponseMode,
        priority: whole(editForm.priority ?? 100),
        maxConcurrentRequests: whole(editForm.maxConcurrentRequests ?? 10, 1),
        modelMappingRules: parseModelMappingRules(modelMappingRulesText),
        supportedModels: parseSupportedModelsText(supportedModelsText),
      }
      await updateExternalPool(editingPool.id, payload)
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
  const externalEnabled = configDraft.externalPoolsEnabled
  const directPolicyActive = externalEnabled && configDraft.externalDirectPolicyEnabled
  const fallbackActive = externalEnabled && !directPolicyActive && (
    configDraft.localPoolPreflightEnabled || configDraft.fallbackOnLocalCapacityExhausted ||
    configDraft.fallbackOnSchedulerRedisDegraded ||
    configDraft.fallbackOnNoAvailableCredentials || configDraft.fallbackOnLocalTransientExhausted ||
    configDraft.fallbackOnUnsupportedModel
  )
  const autoDisableActive = externalEnabled && configDraft.externalPoolAutoDisableEnabled
  const waitModeActive = externalEnabled && configDraft.externalPoolCapacityMode === 'wait'
  const localRescueActive = externalEnabled && !directPolicyActive && configDraft.externalPoolLocalRescueEnabled
  const cacheUpliftActive = externalEnabled && configDraft.externalPoolUsageProjectionUpliftPercent > 0
  const outputUpliftActive = externalEnabled
    && configDraft.externalPoolUsageProjectionOutputUpliftMinTokens > 0
    && configDraft.externalPoolUsageProjectionOutputUpliftPercent > 0
  const usageCompensationActive = cacheUpliftActive || outputUpliftActive

  const poolStatuses = status.data?.pools ?? []
  const totalPools = pools.data?.pools.length ?? poolStatuses.length
  const dispatchablePools = poolStatuses.filter((item) => item.dispatchable).length
  const totalInFlight = poolStatuses.reduce((sum, item) => sum + item.inFlight, 0)
  const totalCapacity = poolStatuses.reduce((sum, item) => sum + item.pool.maxConcurrentRequests, 0)
  const currentPathPoolCount = pools.data?.pools.filter((pool) => pool.usageProjectionMode === 'current_path_policy').length ?? 0
  const concurrencyPct = totalCapacity > 0 ? Math.round((totalInFlight / totalCapacity) * 100) : 0

  const setCacheUpliftEnabled = (enabled: boolean) =>
    setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionUpliftPercent: enabled ? (prev.externalPoolUsageProjectionUpliftPercent || 25) : 0 }))

  const setOutputUpliftEnabled = (enabled: boolean) =>
    setConfigDraft((prev) => ({
      ...prev,
      externalPoolUsageProjectionOutputUpliftMinTokens: enabled ? (prev.externalPoolUsageProjectionOutputUpliftMinTokens || 1000) : 0,
      externalPoolUsageProjectionOutputUpliftPercent: enabled ? (prev.externalPoolUsageProjectionOutputUpliftPercent || 25) : 0,
    }))

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.external.title}
        subtitle={pageMeta.external.subtitle}
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
          value={fallbackActive || directPolicyActive ? '已配置' : '未配置'}
          tone={fallbackActive || directPolicyActive ? 'success' : 'warning'}
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
              <ToggleRow label="启用外部账号" checked={configDraft.externalPoolsEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolsEnabled: v }))} />
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
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="失败阈值" suffix="次" value={configDraft.localPoolCircuitOpenAfterFailures} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitOpenAfterFailures: v }))} />
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="涉及账号" suffix="个" value={configDraft.localPoolCircuitRequireDistinctCredentials} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitRequireDistinctCredentials: v }))} />
                  <NumberBox disabled={!configDraft.localPoolCircuitEnabled} label="暂停时长" suffix="秒" value={configDraft.localPoolCircuitOpenSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, localPoolCircuitOpenSecs: v }))} />
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
                  <SelectBox disabled={!externalEnabled} label="满并发处理" value={configDraft.externalPoolCapacityMode} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolCapacityMode: v as ExternalPoolsConfig['externalPoolCapacityMode'] }))}>
                    <SelectItem value="fail_fast">立即失败</SelectItem>
                    <SelectItem value="wait">等待容量</SelectItem>
                  </SelectBox>
                  <NumberBox disabled={!externalEnabled} label="全局并发上限" description="限制同时进行的外部账号请求数；不是 RPM。0 表示不限。" suffix="并发" value={configDraft.externalPoolGlobalMaxConcurrentRequests} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolGlobalMaxConcurrentRequests: v }))} />
                  <NumberBox disabled={!waitModeActive} label="外部池排队上限" description="externalPoolMaxQueuedRequests；只限制外部池 wait 队列，不是本地账号 dispatch 队列。" suffix="请求" value={configDraft.externalPoolMaxQueuedRequests} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolMaxQueuedRequests: v }))} />
                  <NumberBox disabled={!externalEnabled} label="输入上限预检" suffix="Token" value={configDraft.externalPoolMaxInputTokens} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolMaxInputTokens: v }))} />
                  <NumberBox disabled={!waitModeActive} label="最大等待" description="必须大于 0；旧配置中的 0 按安全默认值 30 秒处理。" suffix="秒" min={1} value={configDraft.externalPoolDispatchMaxWaitSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolDispatchMaxWaitSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="最大重试" suffix="次" value={configDraft.externalPoolRetryMaxAttempts} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolRetryMaxAttempts: v }))} />
                </div>
              </FormSection>
              <FormSection title="冷却与超时">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <NumberBox disabled={!externalEnabled} label="429 冷却" suffix="秒" value={configDraft.externalPoolRateLimitCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolRateLimitCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="5xx 冷却" suffix="秒" value={configDraft.externalPoolServerErrorCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolServerErrorCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="网络错误冷却" suffix="秒" value={configDraft.externalPoolNetworkErrorCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolNetworkErrorCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="协议/认证冷却" suffix="秒" value={configDraft.externalPoolProtocolErrorCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolProtocolErrorCooldownSecs: v }))} />
                  <SelectBox disabled={!externalEnabled} label="模型不可用冷却范围" value={configDraft.externalPoolModelUnavailableCooldownMode} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolModelUnavailableCooldownMode: v as ExternalPoolsConfig['externalPoolModelUnavailableCooldownMode'] }))}>
                    <SelectItem value="model">仅当前模型</SelectItem>
                    <SelectItem value="pool">整个外部账号</SelectItem>
                    <SelectItem value="disabled">不写冷却</SelectItem>
                  </SelectBox>
                  <NumberBox disabled={!externalEnabled || configDraft.externalPoolModelUnavailableCooldownMode === 'disabled'} label="模型不可用冷却" suffix="秒" value={configDraft.externalPoolModelUnavailableCooldownSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolModelUnavailableCooldownSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="非流式总超时" suffix="秒" value={configDraft.externalPoolRequestTimeoutSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolRequestTimeoutSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="流式总超时" suffix="秒" value={configDraft.externalPoolStreamRequestTimeoutSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolStreamRequestTimeoutSecs: v }))} />
                  <NumberBox disabled={!externalEnabled} label="流式空闲超时" suffix="秒" value={configDraft.externalPoolStreamIdleTimeoutSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolStreamIdleTimeoutSecs: v }))} />
                </div>
              </FormSection>
              <FormSection title="流式 SSE 默认转发" description="作为外部账号默认值；单个外部账号仍可在编辑弹窗中覆盖。">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <SelectBox
                    disabled={!externalEnabled}
                    label="SSE 默认转发"
                    value={configDraft.externalPoolStreamResponseMode}
                    onChange={(v) => setConfigDraft((p) => ({
                      ...p,
                      externalPoolStreamResponseMode: v as ExternalPoolsConfig['externalPoolStreamResponseMode'],
                    }))}
                  >
                    <SelectItem value="event_passthrough">SSE 事件级透传</SelectItem>
                  </SelectBox>
                </div>
              </FormSection>
              <FormSection title="外部账号失败后回本地" description="仅对本地优先 fallback 到外部账号的请求生效；显式直连开启时不会回本地。">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <ToggleRow disabled={!externalEnabled || directPolicyActive} label="启用回本地" checked={configDraft.externalPoolLocalRescueEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolLocalRescueEnabled: v }))} />
                  <ToggleRow disabled={!localRescueActive} label="429 时回本地" checked={configDraft.externalPoolLocalRescueOnRateLimit} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolLocalRescueOnRateLimit: v }))} />
                  <ToggleRow disabled={!localRescueActive} label="超时时回本地" checked={configDraft.externalPoolLocalRescueOnTimeout} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolLocalRescueOnTimeout: v }))} />
                  <ToggleRow disabled={!localRescueActive} label="容量失败回本地" checked={configDraft.externalPoolLocalRescueOnCapacity} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolLocalRescueOnCapacity: v }))} />
                  <NumberBox disabled={!localRescueActive} label="回本地最多等待" suffix="秒" value={configDraft.externalPoolLocalRescueMaxWaitSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolLocalRescueMaxWaitSecs: v }))} />
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
              <ToggleRow disabled={!externalEnabled} label="启用自动禁用" checked={configDraft.externalPoolAutoDisableEnabled} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableEnabled: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="认证错误" checked={configDraft.externalPoolAutoDisableOnAuthError} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableOnAuthError: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="安全锁定" checked={configDraft.externalPoolAutoDisableOnSecurityLock} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableOnSecurityLock: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="额度耗尽" checked={configDraft.externalPoolAutoDisableOnQuotaExhausted} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableOnQuotaExhausted: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="配置错误" checked={configDraft.externalPoolAutoDisableOnMisconfiguredEndpoint} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableOnMisconfiguredEndpoint: v }))} />
              <ToggleRow disabled={!autoDisableActive} label="通道禁用" checked={configDraft.externalPoolAutoDisableOnChannelDisabled} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableOnChannelDisabled: v }))} />
              <NumberBox disabled={!autoDisableActive} label="触发阈值" suffix="次" value={configDraft.externalPoolAutoDisableFailureThreshold} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableFailureThreshold: v }))} />
              <NumberBox disabled={!autoDisableActive} label="统计窗口" suffix="秒" value={configDraft.externalPoolAutoDisableWindowSecs} min={1} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableWindowSecs: v }))} />
              <NumberBox disabled={!autoDisableActive} label="禁用时长" suffix="秒" value={configDraft.externalPoolAutoDisableDurationSecs} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolAutoDisableDurationSecs: v }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="用量补偿"
            titleSuffix={!externalEnabled ? '需先启用外部账号' : undefined}
            active={externalEnabled && usageCompensationActive}
            description={'仅对下游 usage 口径为“按当前入口路径整理 usage”的外部账号生效；选择“透传上游 usage”的账号不受影响。'}
          >
            <div className="space-y-4">
              <div className="grid gap-4 lg:grid-cols-2">
                <FormSection title="缓存读写补偿">
                  <div className="grid gap-3 sm:grid-cols-2">
                    <ToggleRow disabled={!externalEnabled} label="启用缓存补偿" checked={cacheUpliftActive} onChange={setCacheUpliftEnabled} />
                    <NumberBox disabled={!cacheUpliftActive} label="放大百分比" suffix="%" value={configDraft.externalPoolUsageProjectionUpliftPercent} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolUsageProjectionUpliftPercent: v }))} />
                  </div>
                </FormSection>
                <FormSection title="输出用量补偿">
                  <div className="grid gap-3 sm:grid-cols-3">
                    <ToggleRow disabled={!externalEnabled} label="启用输出补偿" checked={outputUpliftActive} onChange={setOutputUpliftEnabled} />
                    <NumberBox disabled={!outputUpliftActive} label="输出阈值" suffix="Token" value={configDraft.externalPoolUsageProjectionOutputUpliftMinTokens} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolUsageProjectionOutputUpliftMinTokens: v }))} />
                    <NumberBox disabled={!outputUpliftActive} label="放大百分比" suffix="%" value={configDraft.externalPoolUsageProjectionOutputUpliftPercent} onChange={(v) => setConfigDraft((p) => ({ ...p, externalPoolUsageProjectionOutputUpliftPercent: v }))} />
                  </div>
                </FormSection>
              </div>
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
                        <div className="text-xs text-muted-foreground">{poolUsageSummary(pool, configDraft)} · {poolBodyModeSummary(pool)} · 认证：{authLabel(pool.authType)} · 模型：{poolModelMappingSummary(pool)} · {poolSupportedModelsSummary(pool)}{runtime?.cooldownRemainingSecs ? ` · 冷却 ${runtime.cooldownRemainingSecs}s` : ''}</div>
                        {pool.autoDisabledLastError && <div className="text-xs text-destructive">{pool.autoDisabledLastError}</div>}
                      </div>
                    </div>
                    <div className="flex flex-wrap gap-1.5 lg:shrink-0">
                      <Button variant="ghost" size="xs" onClick={() => startEdit(pool)}><Pencil className="h-3.5 w-3.5" />编辑</Button>
                      <Button variant="ghost" size="xs" onClick={() => setTestingPool(pool)}><FlaskConical className="h-3.5 w-3.5" />测试</Button>
                      <Button variant="ghost" size="xs" onClick={() => mutatePool(() => setExternalPoolEnabled(pool.id, !pool.enabled), pool.enabled ? '已停用' : '已启用')}>
                        <Power className="h-3.5 w-3.5" />{pool.enabled ? '停用' : '启用'}
                      </Button>
                      <Button variant="ghost" size="xs" onClick={() => mutatePool(() => clearExternalPoolAutoDisabled(pool.id), '自动禁用状态已清除')}>
                        <RotateCcw className="h-3.5 w-3.5" />清除禁用
                      </Button>
                      <Button variant="ghost" size="xs" onClick={() => status.refetch()}><RefreshCw className="h-3.5 w-3.5" />刷新</Button>
                      <Button
                        variant="ghost" size="xs"
                        className="text-destructive hover:bg-destructive/10 hover:text-destructive"
                        onClick={async () => {
                          const confirmed = await confirmDialog({ title: '删除外部账号', message: `删除外部账号「${pool.name}」？此操作无法撤销。`, confirmText: '删除', tone: 'danger' })
                          if (confirmed) mutatePool(() => deleteExternalPool(pool.id), '外部账号已删除')
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

      <ExternalPoolFormModal
        mode="create"
        open={createOpen}
        draft={createForm}
        saving={savingPool}
        onDraftChange={setCreateForm}
        onDiscoverSupportedModels={async () => {
          if (!createForm.baseUrl.trim() || !createForm.apiKey.trim()) {
            throw new Error('请先填写外部账号 Base URL 和 Key')
          }
          const response = await discoverExternalPoolSupportedModels({
            baseUrl: createForm.baseUrl.trim(),
            apiKey: createForm.apiKey.trim(),
            authType: createForm.authType,
          })
          return response.supportedModels
        }}
        onClose={() => { if (savingPool) return; setCreateOpen(false); setCreateForm(defaultPoolForm()) }}
        onSubmit={submitPool}
      />
      <ExternalPoolFormModal
        mode="edit"
        pool={editingPool}
        open={Boolean(editingPool)}
        draft={editForm}
        saving={savingPool}
        onDraftChange={setEditForm}
        onDiscoverSupportedModels={async () => {
          if (!editingPool) return []
          const response = await discoverStoredExternalPoolSupportedModels(editingPool.id, {
            baseUrl: editForm.baseUrl.trim() || null,
            apiKey: editForm.apiKey.trim() || null,
            authType: editForm.authType,
          })
          return response.supportedModels
        }}
        onClose={() => { if (savingPool) return; setEditingPool(null); setEditForm(defaultPoolForm()) }}
        onSubmit={savePoolEdit}
      />
      <ExternalPoolTestModal
        pool={testingPool}
        open={Boolean(testingPool)}
        onClose={() => setTestingPool(null)}
        onDone={invalidate}
      />
    </PageContainer>
  )
}
