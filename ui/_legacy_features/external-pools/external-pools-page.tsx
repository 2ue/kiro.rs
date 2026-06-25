import { type ReactNode, useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { FlaskConical, Pencil, Plus, Power, RefreshCw, RotateCcw, Save, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import {
  clearExternalPoolAutoDisabled,
  createExternalPool,
  deleteExternalPool,
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
import { Badge, Button, Input, Label, Switch, Textarea } from '@/components/ui'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui'
import {
  type ExternalPoolFormDraft,
  authLabel,
  defaultPoolForm,
  joinRules,
  parseModelMappingRules,
  poolFormFromPool,
  poolModelMappingSummary,
  poolUsageSummary,
  splitRules,
  whole,
} from './external-pool-utils'
import { ExternalPoolFormModal } from './external-pool-form-modal'
import { ExternalPoolTestModal } from './external-pool-test-modal'

// --- 局部小组件 ---

function PolicyBlock({ title, description, active, children }: {
  title: string; description: string; active: boolean; children: ReactNode
}) {
  return (
    <section className={cn('rounded-lg border border-border p-3', active ? 'bg-card' : 'bg-muted/40')}>
      <div className="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div>
          <div className="text-sm font-semibold">{title}</div>
          <p className="mt-1 text-xs text-muted-foreground">{description}</p>
        </div>
        <Badge tone={active ? 'success' : 'neutral'}>{active ? '生效中' : '未生效'}</Badge>
      </div>
      {children}
    </section>
  )
}

function SummaryItem({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="rounded-lg border border-border bg-muted/40 p-3">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-1 text-sm font-semibold">{value}</div>
    </div>
  )
}

function FormSection({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return (
    <section className="rounded-lg border border-border bg-card p-3">
      <div className="mb-3">
        <div className="text-sm font-semibold">{title}</div>
        {description && <p className="mt-1 text-xs leading-5 text-muted-foreground">{description}</p>}
      </div>
      {children}
    </section>
  )
}

function HintBox({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-lg border border-border bg-muted/50 px-3 py-2 text-xs leading-5 text-muted-foreground">
      {children}
    </div>
  )
}

function ToggleRow({ label, checked, disabled = false, onChange }: {
  label: string; checked: boolean; disabled?: boolean; onChange: (value: boolean) => void
}) {
  return (
    <label className={cn('flex min-h-12 items-center justify-between gap-3 rounded-lg border border-border bg-card px-3 py-2 text-sm', disabled && 'cursor-not-allowed bg-muted opacity-60')}>
      <span className="min-w-0 font-medium text-muted-foreground">{label}</span>
      <Switch className="shrink-0" checked={checked} disabled={disabled} onCheckedChange={onChange} />
    </label>
  )
}

function NumberBox({ label, description, value, min = 0, disabled = false, suffix, onChange }: {
  label: string; description?: string; value: number; min?: number; disabled?: boolean; suffix?: string; onChange: (value: number) => void
}) {
  return (
    <div>
      <div className="mb-1">
        <Label>{label}</Label>
        {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      </div>
      <div className="flex items-center gap-1">
        <Input type="number" min={min} inputMode="numeric" className="h-9 w-full text-sm" value={value} disabled={disabled} onChange={(e) => onChange(Number(e.target.value))} />
        {suffix && <span className="shrink-0 text-xs text-muted-foreground">{suffix}</span>}
      </div>
    </div>
  )
}

function TextAreaBox({ label, disabled = false, value, onChange }: {
  label: string; disabled?: boolean; value: string; onChange: (v: string) => void
}) {
  return (
    <div>
      <div className="mb-1"><Label>{label}</Label></div>
      <Textarea className="min-h-20 w-full font-mono text-xs" value={value} disabled={disabled} onChange={(e) => onChange(e.target.value)} />
    </div>
  )
}

function SelectBox({ label, value, disabled = false, onChange, children }: {
  label: string; value: string; disabled?: boolean; onChange: (v: string) => void; children: ReactNode
}) {
  return (
    <div>
      <div className="mb-1"><Label>{label}</Label></div>
      <Select value={value} onValueChange={onChange} disabled={disabled}>
        <SelectTrigger size="sm" className="w-full"><SelectValue /></SelectTrigger>
        <SelectContent>{children}</SelectContent>
      </Select>
    </div>
  )
}

// --- 主页面 ---

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
    queryClient.invalidateQueries({ queryKey: ['runtimeConfig'] })
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
          externalPoolDispatchMaxWaitSecs: whole(configDraft.externalPoolDispatchMaxWaitSecs),
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
      const { modelMappingRulesText, ...form } = createForm
      await createExternalPool({
        ...form,
        name: createForm.name.trim(),
        baseUrl: createForm.baseUrl.trim(),
        apiKey: createForm.apiKey.trim(),
        priority: whole(createForm.priority ?? 100),
        maxConcurrentRequests: whole(createForm.maxConcurrentRequests ?? 10, 1),
        modelMappingRules: parseModelMappingRules(modelMappingRulesText),
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
      const { modelMappingRulesText, ...form } = editForm
      const payload: UpdateExternalPoolRequest = {
        ...form,
        name: editForm.name.trim(),
        baseUrl: editForm.baseUrl.trim(),
        apiKey: editForm.apiKey?.trim() ? editForm.apiKey.trim() : undefined,
        priority: whole(editForm.priority ?? 100),
        maxConcurrentRequests: whole(editForm.maxConcurrentRequests ?? 10, 1),
        modelMappingRules: parseModelMappingRules(modelMappingRulesText),
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
    try {
      await action()
      toast.success(success)
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    }
  }

  const externalEnabled = configDraft.externalPoolsEnabled
  const fallbackActive = externalEnabled && (
    configDraft.localPoolPreflightEnabled
    || configDraft.fallbackOnLocalCapacityExhausted
    || configDraft.fallbackOnNoAvailableCredentials
    || configDraft.fallbackOnLocalTransientExhausted
    || configDraft.fallbackOnUnsupportedModel
  )
  const directPolicyActive = externalEnabled && configDraft.externalDirectPolicyEnabled
  const autoDisableActive = externalEnabled && configDraft.externalPoolAutoDisableEnabled
  const waitModeActive = externalEnabled && configDraft.externalPoolCapacityMode === 'wait'
  const localRescueActive = externalEnabled && configDraft.externalPoolLocalRescueEnabled
  const cacheUpliftActive = externalEnabled && configDraft.externalPoolUsageProjectionUpliftPercent > 0
  const outputUpliftActive = externalEnabled
    && configDraft.externalPoolUsageProjectionOutputUpliftMinTokens > 0
    && configDraft.externalPoolUsageProjectionOutputUpliftPercent > 0
  const usageCompensationActive = cacheUpliftActive || outputUpliftActive
  const currentPathPoolCount = pools.data?.pools.filter((pool) => pool.usageProjectionMode === 'current_path_policy').length ?? 0
  const poolStatuses = status.data?.pools ?? []
  const totalPools = pools.data?.pools.length ?? poolStatuses.length
  const dispatchablePools = poolStatuses.filter((item) => item.dispatchable).length
  const totalInFlight = poolStatuses.reduce((sum, item) => sum + item.inFlight, 0)
  const totalCapacity = poolStatuses.reduce((sum, item) => sum + item.pool.maxConcurrentRequests, 0)

  const setCacheUpliftEnabled = (enabled: boolean) => {
    setConfigDraft((prev) => ({
      ...prev,
      externalPoolUsageProjectionUpliftPercent: enabled ? (prev.externalPoolUsageProjectionUpliftPercent || 25) : 0,
    }))
  }

  const setOutputUpliftEnabled = (enabled: boolean) => {
    setConfigDraft((prev) => ({
      ...prev,
      externalPoolUsageProjectionOutputUpliftMinTokens: enabled ? (prev.externalPoolUsageProjectionOutputUpliftMinTokens || 1000) : 0,
      externalPoolUsageProjectionOutputUpliftPercent: enabled ? (prev.externalPoolUsageProjectionOutputUpliftPercent || 25) : 0,
    }))
  }

  return (
    <PageContainer>
      <PageHeader
        title={pageMeta.external.title}
        subtitle={pageMeta.external.subtitle}
      />

      <StatGrid>
        <StatCard title="外部账号" value={totalPools} tone="info" />
        <StatCard title="可调度" value={dispatchablePools} tone={dispatchablePools > 0 ? 'success' : 'warning'} />
        <StatCard title="外部并发" value={`${totalInFlight}/${totalCapacity || 0}`} />
        <StatCard title="按入口规则" value={`${currentPathPoolCount} 个`} />
      </StatGrid>

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
          <div className="grid gap-3 md:grid-cols-5">
            <SummaryItem label="外部账号" value={externalEnabled ? '已启用' : '已关闭'} />
            <SummaryItem label="入口策略" value={fallbackActive || directPolicyActive ? '已配置' : '未配置'} />
            <SummaryItem label="可用外部账号" value={`${dispatchablePools}/${totalPools}`} />
            <SummaryItem label="外部账号并发" value={`${totalInFlight}/${totalCapacity || 0}`} />
            <SummaryItem label="按入口规则" value={`${currentPathPoolCount} 个`} />
          </div>

          <PolicyBlock title="1. 是否启用外部账号" active={externalEnabled} description="关闭后不会进入任何外部账号，请求只走本地账号。">
            <div className="grid gap-3 md:grid-cols-2">
              <ToggleRow label="启用外部账号" checked={configDraft.externalPoolsEnabled} onChange={(externalPoolsEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolsEnabled }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock title="2. 什么时候进入外部账号" active={externalEnabled} description="默认先使用本地账号；本地不可用或命中指定规则时，再使用外部账号。">
            <div className="grid gap-4 lg:grid-cols-2">
              <FormSection title="本地优先" description="先调度本地账号，只有下面情况出现时才转入外部账号。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <ToggleRow disabled={!externalEnabled} label="本地容量预检" checked={configDraft.localPoolPreflightEnabled} onChange={(localPoolPreflightEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolPreflightEnabled }))} />
                  <ToggleRow disabled={!externalEnabled} label="容量不足时使用外部账号" checked={configDraft.fallbackOnLocalCapacityExhausted} onChange={(fallbackOnLocalCapacityExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalCapacityExhausted }))} />
                  <ToggleRow disabled={!externalEnabled} label="没有可用账号时使用外部账号" checked={configDraft.fallbackOnNoAvailableCredentials} onChange={(fallbackOnNoAvailableCredentials) => setConfigDraft((prev) => ({ ...prev, fallbackOnNoAvailableCredentials }))} />
                  <ToggleRow disabled={!externalEnabled} label="本地临时错误过多时使用外部账号" checked={configDraft.fallbackOnLocalTransientExhausted} onChange={(fallbackOnLocalTransientExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalTransientExhausted }))} />
                  <ToggleRow disabled={!externalEnabled} label="模型不支持时使用外部账号" checked={configDraft.fallbackOnUnsupportedModel} onChange={(fallbackOnUnsupportedModel) => setConfigDraft((prev) => ({ ...prev, fallbackOnUnsupportedModel }))} />
                </div>
              </FormSection>
              <FormSection title="规则直达外部账号" description="命中规则后跳过本地账号，直接进入外部账号。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <ToggleRow disabled={!externalEnabled} label="启用规则直达" checked={configDraft.externalDirectPolicyEnabled} onChange={(externalDirectPolicyEnabled) => setConfigDraft((prev) => ({ ...prev, externalDirectPolicyEnabled }))} />
                  <ToggleRow disabled={!directPolicyActive} label="本地保护暂停时直达外部账号" checked={configDraft.directExternalOnLocalMaintenance} onChange={(directExternalOnLocalMaintenance) => setConfigDraft((prev) => ({ ...prev, directExternalOnLocalMaintenance }))} />
                </div>
                <div className="mt-3 grid gap-3 sm:grid-cols-2">
                  <TextAreaBox disabled={!directPolicyActive} label="直达模型规则" value={modelRulesText} onChange={setModelRulesText} />
                  <TextAreaBox disabled={!directPolicyActive} label="直达路径规则" value={pathRulesText} onChange={setPathRulesText} />
                </div>
                <div className="mt-3 grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <ToggleRow disabled={!directPolicyActive} label="启用本地保护暂停" checked={configDraft.localPoolCircuitEnabled} onChange={(localPoolCircuitEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitEnabled }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="统计窗口" suffix="秒" value={configDraft.localPoolCircuitWindowSecs} min={1} onChange={(localPoolCircuitWindowSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitWindowSecs }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="失败阈值" suffix="次" value={configDraft.localPoolCircuitOpenAfterFailures} min={1} onChange={(localPoolCircuitOpenAfterFailures) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenAfterFailures }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="涉及账号" suffix="个" value={configDraft.localPoolCircuitRequireDistinctCredentials} min={1} onChange={(localPoolCircuitRequireDistinctCredentials) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitRequireDistinctCredentials }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="暂停时长" suffix="秒" value={configDraft.localPoolCircuitOpenSecs} min={1} onChange={(localPoolCircuitOpenSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenSecs }))} />
                </div>
              </FormSection>
            </div>
          </PolicyBlock>

          <PolicyBlock title="3. 进入外部账号后怎么调度" active={externalEnabled} description="控制外部账号自己的并发、排队、重试和超时。单个外部账号还可以单独设置并发。">
            <div className="space-y-4">
              <FormSection title="容量与排队" description={waitModeActive ? '外部账号满并发时会等待容量；从本地转入外部账号的请求，等待失败后可再尝试回到本地。' : '外部账号满并发时不会排队；从本地转入外部账号的请求，可按回本地策略再尝试本地。'}>
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <SelectBox disabled={!externalEnabled} label="满并发处理" value={configDraft.externalPoolCapacityMode} onChange={(externalPoolCapacityMode) => setConfigDraft((prev) => ({ ...prev, externalPoolCapacityMode: externalPoolCapacityMode as ExternalPoolsConfig['externalPoolCapacityMode'] }))}>
                    <SelectItem value="fail_fast">立即失败</SelectItem>
                    <SelectItem value="wait">等待容量</SelectItem>
                  </SelectBox>
                  <NumberBox disabled={!externalEnabled} label="全局并发上限" suffix="并发" value={configDraft.externalPoolGlobalMaxConcurrentRequests} onChange={(externalPoolGlobalMaxConcurrentRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolGlobalMaxConcurrentRequests }))} />
                  <NumberBox disabled={!waitModeActive} label="排队上限" suffix="请求" value={configDraft.externalPoolMaxQueuedRequests} onChange={(externalPoolMaxQueuedRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolMaxQueuedRequests }))} />
                  <NumberBox disabled={!waitModeActive} label="最大等待" suffix="秒" value={configDraft.externalPoolDispatchMaxWaitSecs} onChange={(externalPoolDispatchMaxWaitSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolDispatchMaxWaitSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="最大重试" suffix="次" value={configDraft.externalPoolRetryMaxAttempts} onChange={(externalPoolRetryMaxAttempts) => setConfigDraft((prev) => ({ ...prev, externalPoolRetryMaxAttempts }))} />
                </div>
              </FormSection>
              <FormSection title="冷却与超时" description="冷却用于临时避开出错外部账号；流式空闲超时用于防止长时间无输出。">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <NumberBox disabled={!externalEnabled} label="429 冷却" suffix="秒" value={configDraft.externalPoolRateLimitCooldownSecs} min={1} onChange={(externalPoolRateLimitCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolRateLimitCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="5xx 冷却" suffix="秒" value={configDraft.externalPoolServerErrorCooldownSecs} min={1} onChange={(externalPoolServerErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolServerErrorCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="网络错误冷却" suffix="秒" value={configDraft.externalPoolNetworkErrorCooldownSecs} min={1} onChange={(externalPoolNetworkErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolNetworkErrorCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="协议/认证冷却" suffix="秒" value={configDraft.externalPoolProtocolErrorCooldownSecs} min={1} onChange={(externalPoolProtocolErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolProtocolErrorCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="非流式总超时" suffix="秒" value={configDraft.externalPoolRequestTimeoutSecs} onChange={(externalPoolRequestTimeoutSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolRequestTimeoutSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="流式总超时" suffix="秒" value={configDraft.externalPoolStreamRequestTimeoutSecs} onChange={(externalPoolStreamRequestTimeoutSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolStreamRequestTimeoutSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="流式空闲超时" suffix="秒" value={configDraft.externalPoolStreamIdleTimeoutSecs} onChange={(externalPoolStreamIdleTimeoutSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolStreamIdleTimeoutSecs }))} />
                </div>
              </FormSection>
              <FormSection title="外部账号失败后回本地" description="仅对先从本地转入外部账号的请求生效。命中后只回本地尝试一次，并禁止再次进入外部账号。">
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <ToggleRow disabled={!externalEnabled} label="启用回本地" checked={configDraft.externalPoolLocalRescueEnabled} onChange={(externalPoolLocalRescueEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueEnabled }))} />
                  <ToggleRow disabled={!localRescueActive} label="429 时回本地" checked={configDraft.externalPoolLocalRescueOnRateLimit} onChange={(externalPoolLocalRescueOnRateLimit) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueOnRateLimit }))} />
                  <ToggleRow disabled={!localRescueActive} label="超时时回本地" checked={configDraft.externalPoolLocalRescueOnTimeout} onChange={(externalPoolLocalRescueOnTimeout) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueOnTimeout }))} />
                  <ToggleRow disabled={!localRescueActive} label="容量失败回本地" checked={configDraft.externalPoolLocalRescueOnCapacity} onChange={(externalPoolLocalRescueOnCapacity) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueOnCapacity }))} />
                  <NumberBox disabled={!localRescueActive} label="回本地最多等待" suffix="秒" description="0 表示只立刻探测可用本地槽位；默认 15 秒。" value={configDraft.externalPoolLocalRescueMaxWaitSecs} onChange={(externalPoolLocalRescueMaxWaitSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueMaxWaitSecs }))} />
                </div>
              </FormSection>
            </div>
          </PolicyBlock>

          <PolicyBlock title="4. 外部账号异常后怎么处理" active={autoDisableActive} description="自动禁用只作用于外部账号本身；单个外部账号可选择继承、强制启用或关闭。">
            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
              <ToggleRow disabled={!externalEnabled} label="启用自动禁用" checked={configDraft.externalPoolAutoDisableEnabled} onChange={(externalPoolAutoDisableEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableEnabled }))} />
              <ToggleRow disabled={!autoDisableActive} label="认证错误" checked={configDraft.externalPoolAutoDisableOnAuthError} onChange={(externalPoolAutoDisableOnAuthError) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnAuthError }))} />
              <ToggleRow disabled={!autoDisableActive} label="安全锁定" checked={configDraft.externalPoolAutoDisableOnSecurityLock} onChange={(externalPoolAutoDisableOnSecurityLock) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnSecurityLock }))} />
              <ToggleRow disabled={!autoDisableActive} label="额度耗尽" checked={configDraft.externalPoolAutoDisableOnQuotaExhausted} onChange={(externalPoolAutoDisableOnQuotaExhausted) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnQuotaExhausted }))} />
              <ToggleRow disabled={!autoDisableActive} label="配置错误" checked={configDraft.externalPoolAutoDisableOnMisconfiguredEndpoint} onChange={(externalPoolAutoDisableOnMisconfiguredEndpoint) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnMisconfiguredEndpoint }))} />
              <ToggleRow disabled={!autoDisableActive} label="通道禁用" checked={configDraft.externalPoolAutoDisableOnChannelDisabled} onChange={(externalPoolAutoDisableOnChannelDisabled) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnChannelDisabled }))} />
              <NumberBox disabled={!autoDisableActive} label="触发阈值" suffix="次" value={configDraft.externalPoolAutoDisableFailureThreshold} min={1} onChange={(externalPoolAutoDisableFailureThreshold) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableFailureThreshold }))} />
              <NumberBox disabled={!autoDisableActive} label="统计窗口" suffix="秒" value={configDraft.externalPoolAutoDisableWindowSecs} min={1} onChange={(externalPoolAutoDisableWindowSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableWindowSecs }))} />
              <NumberBox disabled={!autoDisableActive} label="禁用时长" suffix="秒" value={configDraft.externalPoolAutoDisableDurationSecs} onChange={(externalPoolAutoDisableDurationSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableDurationSecs }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock title="5. 返回给客户端的用量" active={externalEnabled && usageCompensationActive} description={'只影响选择"按入口规则展示"的外部账号。本地账号和"保持原样"的外部账号不会受影响。'}>
            <div className="space-y-4">
              <HintBox>
                生效条件：请求进入外部账号，并且该外部账号的用量模式为"按入口规则展示"。如果外部账号选择"保持原样"，下面配置不会改动用量展示。
              </HintBox>
              <div className="grid gap-4 lg:grid-cols-2">
                <FormSection title="缓存读写补偿" description="按入口规则展示时，对缓存读写用量做补偿。">
                  <div className="grid gap-3 sm:grid-cols-2">
                    <ToggleRow disabled={!externalEnabled} label="启用缓存补偿" checked={cacheUpliftActive} onChange={setCacheUpliftEnabled} />
                    <NumberBox disabled={!cacheUpliftActive} label="放大百分比" suffix="%" value={configDraft.externalPoolUsageProjectionUpliftPercent} onChange={(externalPoolUsageProjectionUpliftPercent) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionUpliftPercent }))} />
                  </div>
                </FormSection>
                <FormSection title="输出用量补偿" description="当输出达到阈值后，放大最终展示给客户端的输出用量。">
                  <div className="grid gap-3 sm:grid-cols-3">
                    <ToggleRow disabled={!externalEnabled} label="启用输出补偿" checked={outputUpliftActive} onChange={setOutputUpliftEnabled} />
                    <NumberBox disabled={!outputUpliftActive} label="输出阈值" suffix="Token" value={configDraft.externalPoolUsageProjectionOutputUpliftMinTokens} onChange={(externalPoolUsageProjectionOutputUpliftMinTokens) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionOutputUpliftMinTokens }))} />
                    <NumberBox disabled={!outputUpliftActive} label="放大百分比" suffix="%" value={configDraft.externalPoolUsageProjectionOutputUpliftPercent} onChange={(externalPoolUsageProjectionOutputUpliftPercent) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionOutputUpliftPercent }))} />
                  </div>
                </FormSection>
              </div>
            </div>
          </PolicyBlock>
        </div>
      </SectionCard>

      <SectionCard
        title="外部账号列表"
        description="单个外部账号配置只影响自身；全局调度、冷却、补偿策略在上方统一保存。"
        actions={
          <Button size="sm" onClick={() => { setCreateForm(defaultPoolForm()); setCreateOpen(true) }}>
            <Plus className="h-4 w-4" />
            添加外部账号
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
              return (
                <div key={pool.id} className="rounded-lg border border-border bg-card p-4">
                  <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
                    <div className="space-y-2">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="font-semibold">#{pool.id} {pool.name}</span>
                        <Badge tone={pool.enabled ? 'success' : 'neutral'}>{pool.enabled ? '启用' : '停用'}</Badge>
                        {pool.autoDisabled && <Badge tone="error">自动禁用</Badge>}
                        <Badge tone={runtime?.dispatchable ? 'info' : 'neutral'}>{runtime?.dispatchable ? '可调度' : runtime?.skippedReason || '不可调度'}</Badge>
                      </div>
                      <div className="text-sm text-muted-foreground">{pool.baseUrl} · {pool.maskedApiKey || '未显示 Key'} · 并发 {runtime?.inFlight ?? 0}/{pool.maxConcurrentRequests} · 优先级 {pool.priority}</div>
                      <div className="text-xs text-muted-foreground">{poolUsageSummary(pool, configDraft)} · 认证：{authLabel(pool.authType)} · 模型：{poolModelMappingSummary(pool)}{runtime?.cooldownRemainingSecs ? ` · 冷却 ${runtime.cooldownRemainingSecs}s` : ''}</div>
                      {pool.autoDisabledLastError && <div className="text-xs text-destructive">{pool.autoDisabledLastError}</div>}
                    </div>
                    <div className="flex flex-wrap gap-2">
                      <Button variant="ghost" size="sm" onClick={() => startEdit(pool)}><Pencil className="h-4 w-4" />编辑</Button>
                      <Button variant="ghost" size="sm" onClick={() => setTestingPool(pool)}><FlaskConical className="h-4 w-4" />测试</Button>
                      <Button variant="ghost" size="sm" onClick={() => mutatePool(() => setExternalPoolEnabled(pool.id, !pool.enabled), pool.enabled ? '已停用' : '已启用')}><Power className="h-4 w-4" />{pool.enabled ? '停用' : '启用'}</Button>
                      <Button variant="ghost" size="sm" onClick={() => mutatePool(() => clearExternalPoolAutoDisabled(pool.id), '自动禁用状态已清除')}><RotateCcw className="h-4 w-4" />清除禁用</Button>
                      <Button variant="ghost" size="sm" onClick={() => status.refetch()}><RefreshCw className="h-4 w-4" />刷新</Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="text-destructive hover:bg-destructive/10 hover:text-destructive"
                        onClick={async () => {
                          const confirmed = await confirmDialog({
                            title: '删除外部账号',
                            message: `删除外部账号 ${pool.name}？`,
                            confirmText: '删除',
                            tone: 'danger',
                          })
                          if (confirmed) mutatePool(() => deleteExternalPool(pool.id), '外部账号已删除')
                        }}
                      >
                        <Trash2 className="h-4 w-4" />删除
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
