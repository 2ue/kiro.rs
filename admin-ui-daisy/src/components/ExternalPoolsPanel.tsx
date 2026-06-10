import { type ReactNode, useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircle2, FlaskConical, Loader2, Pencil, Play, Plus, Power, RefreshCw, RotateCcw, RotateCw, Save, Trash2, X, XCircle } from 'lucide-react'
import { toast } from 'sonner'
import { Button, Card, Input, Join, Select, Toggle, Textarea } from 'react-daisyui'
import { Badge, EmptyState, FieldLabel, ModalShell, SectionCard } from '@/components/common'
import {
  clearExternalPoolAutoDisabled,
  createExternalPool,
  deleteExternalPool,
  getExternalPools,
  getExternalPoolsStatus,
  setExternalPoolEnabled,
  testExternalPool,
  updateExternalPool,
  updateRuntimeConfig,
} from '@/api/credentials'
import { defaultExternalPoolsConfig } from '@/lib/runtime-config-defaults'
import { useRuntimeConfig } from '@/hooks/use-credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import { extractErrorMessage } from '@/lib/utils'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS } from '@/lib/test-models'
import type { CreateExternalPoolRequest, ExternalPool, ExternalPoolsConfig, ExternalPoolTestResponse, UpdateExternalPoolRequest } from '@/types/api'

const splitRules = (value: string) => value.split('\n').map((item) => item.trim()).filter(Boolean)
const joinRules = (value: string[] = []) => value.join('\n')
const whole = (value: number, min = 0) => Math.max(min, Math.floor(Number.isFinite(value) ? value : min))

export function ExternalPoolsPanel() {
  const queryClient = useQueryClient()
  const runtimeConfig = useRuntimeConfig()
  const pools = useQuery({ queryKey: ['external-pools'], queryFn: getExternalPools })
  const status = useQuery({ queryKey: ['external-pools-status'], queryFn: getExternalPoolsStatus, refetchInterval: 5000 })
  const [savingConfig, setSavingConfig] = useState(false)
  const [configDraft, setConfigDraft] = useState<ExternalPoolsConfig>(defaultExternalPoolsConfig())
  const [modelRulesText, setModelRulesText] = useState('')
  const [pathRulesText, setPathRulesText] = useState('')
  const [editingPoolId, setEditingPoolId] = useState<number | null>(null)
  const [testingPool, setTestingPool] = useState<ExternalPool | null>(null)
  const [editForm, setEditForm] = useState<UpdateExternalPoolRequest>({})
  const [form, setForm] = useState<CreateExternalPoolRequest>({
    name: '',
    baseUrl: '',
    apiKey: '',
    authType: 'bearer',
    enabled: true,
    priority: 100,
    maxConcurrentRequests: 10,
    usageProjectionMode: 'pass_through',
    autoDisablePolicy: 'inherit',
    preservePath: false,
    notes: '',
  })

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
      toast.success('备用号池策略已保存')
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    } finally {
      setSavingConfig(false)
    }
  }

  const submitPool = async () => {
    if (!form.name?.trim() || !form.baseUrl?.trim() || !form.apiKey?.trim()) return toast.error('名称、Base URL 和 Key 必填')
    try {
      await createExternalPool({
        ...form,
        priority: whole(form.priority ?? 100),
        maxConcurrentRequests: whole(form.maxConcurrentRequests ?? 10, 1),
      })
      toast.success('外部池已添加')
      setForm((prev) => ({ ...prev, name: '', baseUrl: '', apiKey: '', notes: '' }))
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    }
  }

  const startEdit = (pool: ExternalPool) => {
    setEditingPoolId(pool.id)
    setEditForm({
      name: pool.name,
      baseUrl: pool.baseUrl,
      apiKey: '',
      authType: pool.authType,
      enabled: pool.enabled,
      priority: pool.priority,
      maxConcurrentRequests: pool.maxConcurrentRequests,
      usageProjectionMode: pool.usageProjectionMode,
      autoDisablePolicy: pool.autoDisablePolicy,
      preservePath: pool.preservePath,
      notes: pool.notes || '',
    })
  }

  const savePoolEdit = async () => {
    if (!editingPoolId) return
    if (!editForm.name?.trim() || !editForm.baseUrl?.trim()) return toast.error('名称和 Base URL 必填')
    try {
      await updateExternalPool(editingPoolId, {
        ...editForm,
        apiKey: editForm.apiKey?.trim() ? editForm.apiKey.trim() : undefined,
        priority: whole(editForm.priority ?? 100),
        maxConcurrentRequests: whole(editForm.maxConcurrentRequests ?? 10, 1),
      })
      toast.success('外部池已更新')
      setEditingPoolId(null)
      setEditForm({})
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
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
      externalPoolUsageProjectionUpliftPercent: enabled
        ? (prev.externalPoolUsageProjectionUpliftPercent || 25)
        : 0,
    }))
  }

  const setOutputUpliftEnabled = (enabled: boolean) => {
    setConfigDraft((prev) => ({
      ...prev,
      externalPoolUsageProjectionOutputUpliftMinTokens: enabled
        ? (prev.externalPoolUsageProjectionOutputUpliftMinTokens || 1000)
        : 0,
      externalPoolUsageProjectionOutputUpliftPercent: enabled
        ? (prev.externalPoolUsageProjectionOutputUpliftPercent || 25)
        : 0,
    }))
  }

  return (
    <div className="space-y-5">
      <SectionCard title="备用池策略" actions={<Button size="sm" color="primary" loading={savingConfig} onClick={saveConfig}><Save className="h-4 w-4" />保存策略</Button>}>
        <div className="space-y-5">
          <div className="grid gap-3 md:grid-cols-5">
            <SummaryItem label="备用池" value={externalEnabled ? '已启用' : '已关闭'} />
            <SummaryItem label="入口策略" value={fallbackActive || directPolicyActive ? '已配置' : '未配置'} />
            <SummaryItem label="可调度外部池" value={`${dispatchablePools}/${totalPools}`} />
            <SummaryItem label="外部池并发" value={`${totalInFlight}/${totalCapacity || 0}`} />
            <SummaryItem label="按路径整形池" value={`${currentPathPoolCount} 个`} />
          </div>

          <PolicyBlock
            title="1. 是否启用备用池"
            active={externalEnabled}
            description="关闭后不会进入任何外部池，请求只走本地凭证。"
          >
            <div className="grid gap-3 md:grid-cols-2">
              <ToggleRow label="启用备用池" checked={configDraft.externalPoolsEnabled} onChange={(externalPoolsEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolsEnabled }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="2. 什么时候进入备用池"
            active={externalEnabled}
            description="fallback 是本地凭证不可用后再转外部池；显式直连是命中规则后直接走外部池。"
          >
            <div className="grid gap-4 lg:grid-cols-2">
              <FormSection title="本地优先 fallback" description="先调度本地凭证，只有下面情况出现时才转外部池。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <ToggleRow disabled={!externalEnabled} label="本地容量预检" checked={configDraft.localPoolPreflightEnabled} onChange={(localPoolPreflightEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolPreflightEnabled }))} />
                  <ToggleRow disabled={!externalEnabled} label="容量不足 fallback" checked={configDraft.fallbackOnLocalCapacityExhausted} onChange={(fallbackOnLocalCapacityExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalCapacityExhausted }))} />
                  <ToggleRow disabled={!externalEnabled} label="无可用凭据 fallback" checked={configDraft.fallbackOnNoAvailableCredentials} onChange={(fallbackOnNoAvailableCredentials) => setConfigDraft((prev) => ({ ...prev, fallbackOnNoAvailableCredentials }))} />
                  <ToggleRow disabled={!externalEnabled} label="瞬态错误耗尽 fallback" checked={configDraft.fallbackOnLocalTransientExhausted} onChange={(fallbackOnLocalTransientExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalTransientExhausted }))} />
                  <ToggleRow disabled={!externalEnabled} label="模型不支持 fallback" checked={configDraft.fallbackOnUnsupportedModel} onChange={(fallbackOnUnsupportedModel) => setConfigDraft((prev) => ({ ...prev, fallbackOnUnsupportedModel }))} />
                </div>
              </FormSection>

              <FormSection title="显式直连" description="命中规则后绕过本地凭证，直接进入外部池。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <ToggleRow disabled={!externalEnabled} label="启用显式直连" checked={configDraft.externalDirectPolicyEnabled} onChange={(externalDirectPolicyEnabled) => setConfigDraft((prev) => ({ ...prev, externalDirectPolicyEnabled }))} />
                  <ToggleRow disabled={!directPolicyActive} label="本地熔断时直连" checked={configDraft.directExternalOnLocalMaintenance} onChange={(directExternalOnLocalMaintenance) => setConfigDraft((prev) => ({ ...prev, directExternalOnLocalMaintenance }))} />
                </div>
                <div className="mt-3 grid gap-3 sm:grid-cols-2">
                  <TextAreaBox disabled={!directPolicyActive} label="直连模型规则" value={modelRulesText} onChange={setModelRulesText} />
                  <TextAreaBox disabled={!directPolicyActive} label="直连路径规则" value={pathRulesText} onChange={setPathRulesText} />
                </div>
                <div className="mt-3 grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <ToggleRow disabled={!directPolicyActive} label="启用本地熔断" checked={configDraft.localPoolCircuitEnabled} onChange={(localPoolCircuitEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitEnabled }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="熔断窗口" suffix="秒" value={configDraft.localPoolCircuitWindowSecs} min={1} onChange={(localPoolCircuitWindowSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitWindowSecs }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="失败阈值" suffix="次" value={configDraft.localPoolCircuitOpenAfterFailures} min={1} onChange={(localPoolCircuitOpenAfterFailures) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenAfterFailures }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="涉及凭证" suffix="个" value={configDraft.localPoolCircuitRequireDistinctCredentials} min={1} onChange={(localPoolCircuitRequireDistinctCredentials) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitRequireDistinctCredentials }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="熔断时长" suffix="秒" value={configDraft.localPoolCircuitOpenSecs} min={1} onChange={(localPoolCircuitOpenSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenSecs }))} />
                </div>
              </FormSection>
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="3. 进入备用池后怎么调度"
            active={externalEnabled}
            description="控制外部池自己的并发、排队、重试和超时。单个外部池还可以单独设置并发。"
          >
            <div className="space-y-4">
              <FormSection title="容量与排队" description={waitModeActive ? '外部池满并发时会等待容量；fallback 请求等待失败后可按回本地策略再探测本地。' : '外部池满并发时不会排队；fallback 请求可按回本地策略再探测本地。'}>
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <SelectBox disabled={!externalEnabled} label="满并发处理" value={configDraft.externalPoolCapacityMode} onChange={(externalPoolCapacityMode) => setConfigDraft((prev) => ({ ...prev, externalPoolCapacityMode: externalPoolCapacityMode as ExternalPoolsConfig['externalPoolCapacityMode'] }))}>
                    <option value="fail_fast">立即失败</option>
                    <option value="wait">等待容量</option>
                  </SelectBox>
                  <NumberBox disabled={!externalEnabled} label="全局并发上限" suffix="并发" value={configDraft.externalPoolGlobalMaxConcurrentRequests} onChange={(externalPoolGlobalMaxConcurrentRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolGlobalMaxConcurrentRequests }))} />
                  <NumberBox disabled={!waitModeActive} label="排队上限" suffix="请求" value={configDraft.externalPoolMaxQueuedRequests} onChange={(externalPoolMaxQueuedRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolMaxQueuedRequests }))} />
                  <NumberBox disabled={!waitModeActive} label="最大等待" suffix="秒" value={configDraft.externalPoolDispatchMaxWaitSecs} onChange={(externalPoolDispatchMaxWaitSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolDispatchMaxWaitSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="最大重试" suffix="次" value={configDraft.externalPoolRetryMaxAttempts} onChange={(externalPoolRetryMaxAttempts) => setConfigDraft((prev) => ({ ...prev, externalPoolRetryMaxAttempts }))} />
                </div>
              </FormSection>

              <FormSection title="冷却与超时" description="冷却用于临时避开出错外部池；流式空闲超时用于防止长时间无输出。">
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

              <FormSection title="备用池失败后回本地" description="仅对本地失败后 fallback 到备用池的请求生效。命中后只回本地尝试一次，并禁止再次进入备用池。">
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

          <PolicyBlock
            title="4. 外部池异常后怎么处理"
            active={autoDisableActive}
            description="自动禁用只作用于外部池本身；单个外部池可选择继承、强制启用或关闭。"
          >
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

          <PolicyBlock
            title="5. 返回给下游的 usage"
            active={externalEnabled && usageCompensationActive}
            description="只影响选择“按路径整形”的外部池。本地凭证和“严格透传”的外部池不会受影响。"
          >
            <div className="space-y-4">
              <HintBox>
                生效条件：请求进入外部池，并且该外部池的 Usage 模式为“按路径整形”。如果外部池是“严格透传”，下面配置不会改写 usage。
              </HintBox>
              <div className="grid gap-4 lg:grid-cols-2">
                <FormSection title="缓存读写补偿" description="按路径整形后，对上报的 cache read/write token 做补偿。">
                  <div className="grid gap-3 sm:grid-cols-2">
                    <ToggleRow disabled={!externalEnabled} label="启用缓存补偿" checked={cacheUpliftActive} onChange={setCacheUpliftEnabled} />
                    <NumberBox disabled={!cacheUpliftActive} label="放大百分比" suffix="%" value={configDraft.externalPoolUsageProjectionUpliftPercent} onChange={(externalPoolUsageProjectionUpliftPercent) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionUpliftPercent }))} />
                  </div>
                </FormSection>

                <FormSection title="输出 token 补偿" description="当输出达到阈值后，放大最终上报给下游的 output_tokens。">
                  <div className="grid gap-3 sm:grid-cols-3">
                    <ToggleRow disabled={!externalEnabled} label="启用输出补偿" checked={outputUpliftActive} onChange={setOutputUpliftEnabled} />
                    <NumberBox disabled={!outputUpliftActive} label="输出阈值" suffix="tokens" value={configDraft.externalPoolUsageProjectionOutputUpliftMinTokens} onChange={(externalPoolUsageProjectionOutputUpliftMinTokens) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionOutputUpliftMinTokens }))} />
                    <NumberBox disabled={!outputUpliftActive} label="放大百分比" suffix="%" value={configDraft.externalPoolUsageProjectionOutputUpliftPercent} onChange={(externalPoolUsageProjectionOutputUpliftPercent) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionOutputUpliftPercent }))} />
                  </div>
                </FormSection>
              </div>
            </div>
          </PolicyBlock>
        </div>
      </SectionCard>

      <SectionCard title="添加外部池">
        <div className="space-y-5">
          <FormSection title="基础信息" description="外部池请求会使用这里的 Base URL 和 Key。">
            <div className="grid gap-3 md:grid-cols-2">
              <TextBox label="名称" value={form.name || ''} onChange={(name) => setForm((prev) => ({ ...prev, name }))} />
              <SelectBox label="认证方式" value={form.authType || 'bearer'} onChange={(authType) => setForm((prev) => ({ ...prev, authType: authType as CreateExternalPoolRequest['authType'] }))}>
                <option value="bearer">Authorization Bearer</option>
                <option value="x_api_key">x-api-key</option>
              </SelectBox>
              <TextBox className="md:col-span-2" label="Base URL" description="填写到域名或 /v1 均可，系统会调用外部池的 /v1/messages。" value={form.baseUrl || ''} onChange={(baseUrl) => setForm((prev) => ({ ...prev, baseUrl }))} />
              <TextBox className="md:col-span-2" label="请求 Key" value={form.apiKey || ''} onChange={(apiKey) => setForm((prev) => ({ ...prev, apiKey }))} />
            </div>
          </FormSection>

          <div className="grid gap-4 lg:grid-cols-2">
            <FormSection title="调度能力" description="单池最大并发只限制这个外部池；优先级数字越小越靠前。">
              <div className="grid gap-3 sm:grid-cols-2">
                <NumberBox label="单池最大并发" suffix="并发" value={form.maxConcurrentRequests ?? 10} min={1} onChange={(maxConcurrentRequests) => setForm((prev) => ({ ...prev, maxConcurrentRequests }))} />
                <NumberBox label="优先级" suffix="值" value={form.priority ?? 100} onChange={(priority) => setForm((prev) => ({ ...prev, priority }))} />
                <ToggleRow label="启用外部池" checked={Boolean(form.enabled)} onChange={(enabled) => setForm((prev) => ({ ...prev, enabled }))} />
              </div>
            </FormSection>

            <FormSection title="Usage 上报" description="严格透传不会改 usage；按路径整形会应用全局 usage 补偿。">
              <div className="space-y-3">
                <SelectBox label="Usage 模式" value={form.usageProjectionMode || 'pass_through'} onChange={(usageProjectionMode) => setForm((prev) => ({ ...prev, usageProjectionMode: usageProjectionMode as CreateExternalPoolRequest['usageProjectionMode'] }))}>
                  <option value="pass_through">严格透传 usage</option>
                  <option value="current_path_policy">按路径整形 usage</option>
                </SelectBox>
                <HintBox>{usageProjectionDescription(form.usageProjectionMode)}</HintBox>
              </div>
            </FormSection>
          </div>

          <FormSection title="错误处理和备注" description="自动禁用策略只控制这个外部池是否继承全局自动禁用。">
            <div className="grid gap-3 md:grid-cols-2">
              <SelectBox label="自动禁用策略" value={form.autoDisablePolicy || 'inherit'} onChange={(autoDisablePolicy) => setForm((prev) => ({ ...prev, autoDisablePolicy: autoDisablePolicy as CreateExternalPoolRequest['autoDisablePolicy'] }))}>
                <option value="inherit">继承全局自动禁用</option>
                <option value="enabled">强制启用自动禁用</option>
                <option value="disabled">禁用自动禁用</option>
              </SelectBox>
              <TextBox label="备注" value={form.notes || ''} onChange={(notes) => setForm((prev) => ({ ...prev, notes }))} />
            </div>
          </FormSection>

          <div className="flex justify-end">
            <Button color="primary" onClick={submitPool}><Plus className="h-4 w-4" />添加外部池</Button>
          </div>
        </div>
      </SectionCard>

      <div className="space-y-3">
        {pools.data?.pools.map((pool) => {
          const runtime = statusMap.get(pool.id)
          const editing = editingPoolId === pool.id
          return (
            <SectionCard key={pool.id} title={`#${pool.id} ${pool.name}`}>
              {editing ? (
                <div className="space-y-4">
                  <FormSection title="基础信息">
                    <div className="grid gap-3 md:grid-cols-2">
                      <TextBox label="名称" value={editForm.name || ''} onChange={(name) => setEditForm((prev) => ({ ...prev, name }))} />
                      <SelectBox label="认证方式" value={editForm.authType || 'bearer'} onChange={(authType) => setEditForm((prev) => ({ ...prev, authType: authType as UpdateExternalPoolRequest['authType'] }))}>
                        <option value="bearer">Authorization Bearer</option>
                        <option value="x_api_key">x-api-key</option>
                      </SelectBox>
                      <TextBox className="md:col-span-2" label="Base URL" value={editForm.baseUrl || ''} onChange={(baseUrl) => setEditForm((prev) => ({ ...prev, baseUrl }))} />
                      <TextBox className="md:col-span-2" label="新 Key" description="留空表示不修改当前 Key。" value={editForm.apiKey || ''} onChange={(apiKey) => setEditForm((prev) => ({ ...prev, apiKey }))} />
                    </div>
                  </FormSection>

                  <div className="grid gap-4 lg:grid-cols-2">
                    <FormSection title="调度能力">
                      <div className="grid gap-3 sm:grid-cols-2">
                        <NumberBox label="单池最大并发" suffix="并发" value={editForm.maxConcurrentRequests ?? 10} min={1} onChange={(maxConcurrentRequests) => setEditForm((prev) => ({ ...prev, maxConcurrentRequests }))} />
                        <NumberBox label="优先级" suffix="值" value={editForm.priority ?? 100} onChange={(priority) => setEditForm((prev) => ({ ...prev, priority }))} />
                        <ToggleRow label="启用外部池" checked={Boolean(editForm.enabled)} onChange={(enabled) => setEditForm((prev) => ({ ...prev, enabled }))} />
                      </div>
                    </FormSection>

                    <FormSection title="Usage 上报">
                      <div className="space-y-3">
                        <SelectBox label="Usage 模式" value={editForm.usageProjectionMode || 'pass_through'} onChange={(usageProjectionMode) => setEditForm((prev) => ({ ...prev, usageProjectionMode: usageProjectionMode as UpdateExternalPoolRequest['usageProjectionMode'] }))}>
                          <option value="pass_through">严格透传 usage</option>
                          <option value="current_path_policy">按路径整形 usage</option>
                        </SelectBox>
                        <HintBox>{usageProjectionDescription(editForm.usageProjectionMode || 'pass_through')}</HintBox>
                      </div>
                    </FormSection>
                  </div>

                  <FormSection title="错误处理和备注">
                    <div className="grid gap-3 md:grid-cols-2">
                      <SelectBox label="自动禁用策略" value={editForm.autoDisablePolicy || 'inherit'} onChange={(autoDisablePolicy) => setEditForm((prev) => ({ ...prev, autoDisablePolicy: autoDisablePolicy as UpdateExternalPoolRequest['autoDisablePolicy'] }))}>
                        <option value="inherit">继承全局自动禁用</option>
                        <option value="enabled">强制启用自动禁用</option>
                        <option value="disabled">禁用自动禁用</option>
                      </SelectBox>
                      <TextBox label="备注" value={editForm.notes || ''} onChange={(notes) => setEditForm((prev) => ({ ...prev, notes }))} />
                    </div>
                  </FormSection>

                  <div className="flex gap-2">
                    <Button size="sm" color="primary" onClick={savePoolEdit}><Save className="h-4 w-4" />保存</Button>
                    <Button size="sm" color="ghost" onClick={() => { setEditingPoolId(null); setEditForm({}) }}><X className="h-4 w-4" />取消</Button>
                  </div>
                </div>
              ) : (
              <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
                <div className="space-y-2">
                  <div className="flex flex-wrap gap-2">
                    <Badge tone={pool.enabled ? 'success' : 'neutral'}>{pool.enabled ? '启用' : '停用'}</Badge>
                    {pool.autoDisabled && <Badge tone="error">自动禁用</Badge>}
                    <Badge tone={runtime?.dispatchable ? 'info' : 'neutral'}>{runtime?.dispatchable ? '可调度' : runtime?.skippedReason || '不可调度'}</Badge>
                  </div>
                  <div className="text-sm text-base-content/70">{pool.baseUrl} · {pool.maskedApiKey || '未显示 Key'} · 并发 {runtime?.inFlight ?? 0}/{pool.maxConcurrentRequests} · 优先级 {pool.priority}</div>
                  <div className="text-xs text-base-content/50">{poolUsageSummary(pool, configDraft)} · auth: {authLabel(pool.authType)} · request: /v1/messages {runtime?.cooldownRemainingSecs ? `· 冷却 ${runtime.cooldownRemainingSecs}s` : ''}</div>
                  {pool.autoDisabledLastError && <div className="text-xs text-error">{pool.autoDisabledLastError}</div>}
                </div>
                <div className="flex flex-wrap gap-2">
                  <Button size="sm" color="ghost" onClick={() => startEdit(pool)}><Pencil className="h-4 w-4" />编辑</Button>
                  <Button size="sm" color="ghost" onClick={() => setTestingPool(pool)}><FlaskConical className="h-4 w-4" />测试</Button>
                  <Button size="sm" color="ghost" onClick={() => mutatePool(() => setExternalPoolEnabled(pool.id, !pool.enabled), pool.enabled ? '已停用' : '已启用')}><Power className="h-4 w-4" />{pool.enabled ? '停用' : '启用'}</Button>
                  <Button size="sm" color="ghost" onClick={() => mutatePool(() => clearExternalPoolAutoDisabled(pool.id), '自动禁用状态已清除')}><RotateCcw className="h-4 w-4" />清除禁用</Button>
                  <Button size="sm" color="ghost" onClick={() => status.refetch()}><RefreshCw className="h-4 w-4" />刷新</Button>
                  <Button size="sm" color="error" onClick={() => confirm(`删除外部池 ${pool.name}？`) && mutatePool(() => deleteExternalPool(pool.id), '外部池已删除')}><Trash2 className="h-4 w-4" />删除</Button>
                </div>
              </div>
              )}
            </SectionCard>
          )
        })}
        {!pools.isLoading && !pools.data?.pools.length && <EmptyState text="暂无外部备用号池" />}
      </div>
      <ExternalPoolTestModal
        pool={testingPool}
        open={Boolean(testingPool)}
        onClose={() => setTestingPool(null)}
        onDone={invalidate}
      />
    </div>
  )
}

function ExternalPoolTestModal({
  pool,
  open,
  onClose,
  onDone,
}: {
  pool: ExternalPool | null
  open: boolean
  onClose: () => void
  onDone: () => void
}) {
  const modelCapabilities = useModelCapabilities()
  const [model, setModel] = useState(DEFAULT_TEST_MODEL)
  const [prompt, setPrompt] = useState(DEFAULT_TEST_PROMPT)
  const [result, setResult] = useState<ExternalPoolTestResponse | null>(null)
  const [error, setError] = useState('')
  const [running, setRunning] = useState(false)

  const modelOptions = useMemo(() => {
    const seen = new Set<string>()
    const options: { id: string; label: string }[] = []
    const push = (id: string, label: string) => {
      const key = id.trim()
      if (!key || seen.has(key)) return
      seen.add(key)
      options.push({ id: key, label })
    }
    TEST_MODELS.forEach((item) => push(item.id, item.label))
    ;[...(modelCapabilities.data?.models || [])]
      .sort((left, right) => left.model.localeCompare(right.model))
      .forEach((item) => push(item.model, item.displayName || item.model))
    return options
  }, [modelCapabilities.data?.models])
  const selectedModelLabel = useMemo(
    () => modelOptions.find((option) => option.id === model)?.label || model,
    [model, modelOptions]
  )

  useEffect(() => {
    if (!open) return
    setModel(DEFAULT_TEST_MODEL)
    setPrompt(DEFAULT_TEST_PROMPT)
    setResult(null)
    setError('')
    setRunning(false)
  }, [open, pool?.id])

  const run = async () => {
    if (!pool) return
    const trimmedModel = model.trim()
    const trimmedPrompt = prompt.trim() || DEFAULT_TEST_PROMPT
    if (!trimmedModel) {
      toast.error('请选择或输入测试模型')
      return
    }
    setRunning(true)
    setResult(null)
    setError('')
    try {
      const response = await testExternalPool(pool.id, {
        model: trimmedModel,
        prompt: trimmedPrompt,
      })
      setResult(response)
      if (response.ok) {
        toast.success(response.message || '外部池模型调用测试通过')
      } else {
        toast.error(response.message || '外部池模型调用测试失败')
      }
      onDone()
    } catch (err) {
      setError(extractErrorMessage(err))
    } finally {
      setRunning(false)
    }
  }

  return (
    <ModalShell
      open={open}
      title="测试外部备用池"
      width="max-w-3xl"
      onClose={onClose}
      footer={
        <>
          <Button type="button" size="sm" color="ghost" onClick={onClose} disabled={running}>
            关闭
          </Button>
          <Button type="button" size="sm" color="primary" onClick={run} disabled={!pool || running}>
            {running ? <Loader2 className="h-4 w-4 animate-spin" /> : result || error ? <RotateCw className="h-4 w-4" /> : <Play className="h-4 w-4" />}
            {result || error ? '重试' : '开始测试'}
          </Button>
        </>
      }
    >
      {pool && (
        <div className="space-y-4">
          <Card bordered className="bg-base-200">
            <Card.Body className="p-4">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-lg font-semibold">#{pool.id} {pool.name}</span>
                <Badge tone="neutral">{pool.authType}</Badge>
                <Badge tone={pool.enabled ? 'success' : 'error'}>{pool.enabled ? '启用' : '已禁用'}</Badge>
              </div>
              <div className="mt-2 break-all text-sm text-base-content/60">{pool.baseUrl}</div>
            </Card.Body>
          </Card>

          <div className="grid gap-3 sm:grid-cols-[1fr_220px]">
            <FieldLabel title="测试模型">
              <Select bordered size="sm" value={model} disabled={running} onChange={(event) => setModel(event.target.value)}>
                {modelOptions.map((option) => (
                  <Select.Option key={option.id} value={option.id}>
                    {option.label}
                  </Select.Option>
                ))}
              </Select>
            </FieldLabel>
            <FieldLabel title="测试消息">
              <Input bordered value={prompt} disabled={running} onChange={(event) => setPrompt(event.target.value)} />
            </FieldLabel>
          </div>

          <div className="rounded-box bg-neutral p-4 font-mono text-sm text-neutral-content">
            <div className="space-y-1">
              <div><span className="text-info">外部池：</span><span> #{pool.id} {pool.name}</span></div>
              <div><span className="text-info">使用模型：</span><span> {model}</span></div>
              <div><span className="text-neutral-content/60">发送测试消息：</span><span> "{prompt.trim() || DEFAULT_TEST_PROMPT}"</span></div>
            </div>
            <div className="mt-4 border-t border-neutral-content/20 pt-4">
              {running && (
                <div className="flex items-center gap-2 text-info">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  正在等待外部池模型响应...
                </div>
              )}
              {result && (
                <div className={result.ok ? 'space-y-3 text-success' : 'space-y-3 text-error'}>
                  <div>
                    {result.ok ? <CheckCircle2 className="mr-2 inline h-4 w-4" /> : <XCircle className="mr-2 inline h-4 w-4" />}
                    {result.message}
                  </div>
                  <div className="text-neutral-content/60">HTTP 状态：{result.status ?? '-'}</div>
                  {result.model && <div className="text-neutral-content/60">返回模型：{result.model}</div>}
                  {result.response && (
                    <div>
                      <div className="mb-1 text-warning">响应：</div>
                      <div className="whitespace-pre-wrap break-words">{result.response}</div>
                    </div>
                  )}
                </div>
              )}
              {error && (
                <div className="space-y-2 text-error">
                  <div><XCircle className="mr-2 inline h-4 w-4" />测试失败</div>
                  <div className="whitespace-pre-wrap break-words">{error}</div>
                </div>
              )}
              {!running && !result && !error && <div className="text-neutral-content/60">等待开始测试</div>}
            </div>
          </div>

          <div className="flex flex-wrap justify-between gap-3 text-sm text-base-content/60">
            <span>测试模型：{selectedModelLabel}</span>
            <span>提示词："{prompt.trim() || DEFAULT_TEST_PROMPT}"</span>
          </div>
        </div>
      )}
    </ModalShell>
  )
}

function PolicyBlock({
  title,
  description,
  active,
  children,
}: {
  title: string
  description: string
  active: boolean
  children: ReactNode
}) {
  return (
    <section className={`rounded-box border border-base-300 p-3 ${active ? 'bg-base-100' : 'bg-base-200/60'}`}>
      <div className="mb-3 flex flex-wrap items-start justify-between gap-2">
        <div>
          <div className="text-sm font-semibold">{title}</div>
          <p className="mt-1 text-xs text-base-content/60">{description}</p>
        </div>
        <Badge tone={active ? 'success' : 'neutral'}>{active ? '生效中' : '未生效'}</Badge>
      </div>
      {children}
    </section>
  )
}

function SummaryItem({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="rounded-box border border-base-300 bg-base-200/40 p-3">
      <div className="text-xs text-base-content/60">{label}</div>
      <div className="mt-1 text-sm font-semibold">{value}</div>
    </div>
  )
}

function FormSection({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return (
    <section className="rounded-box border border-base-300 bg-base-100 p-3">
      <div className="mb-3">
        <div className="text-sm font-semibold">{title}</div>
        {description && <p className="mt-1 text-xs leading-5 text-base-content/60">{description}</p>}
      </div>
      {children}
    </section>
  )
}

function HintBox({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-box border border-base-300 bg-base-200/50 px-3 py-2 text-xs leading-5 text-base-content/60">
      {children}
    </div>
  )
}

function ToggleRow({ label, checked, disabled = false, onChange }: { label: string; checked: boolean; disabled?: boolean; onChange: (value: boolean) => void }) {
  return (
    <label className={`flex min-h-12 items-center justify-between gap-3 rounded-box border border-base-300 bg-base-100 px-3 py-2 text-sm ${disabled ? 'cursor-not-allowed bg-base-200 opacity-60' : ''}`}>
      <span className="min-w-0 font-medium text-base-content/75">{label}</span>
      <Toggle color="primary" size="sm" className="shrink-0" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
    </label>
  )
}

function TextBox({
  label,
  description,
  value,
  disabled = false,
  className = '',
  onChange,
}: {
  label: string
  description?: string
  value: string
  disabled?: boolean
  className?: string
  onChange: (value: string) => void
}) {
  return (
    <div className={className}>
      <FieldLabel title={label} description={description}>
        <Input
          bordered
          size="sm"
          className="w-full"
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
        />
      </FieldLabel>
    </div>
  )
}

function NumberBox({
  label,
  description,
  value,
  min = 0,
  disabled = false,
  suffix,
  onChange,
}: {
  label: string
  description?: string
  value: number
  min?: number
  disabled?: boolean
  suffix?: string
  onChange: (value: number) => void
}) {
  return (
    <FieldLabel title={label} description={description}>
      <Join className="w-full">
        <Input
          bordered
          size="sm"
          type="number"
          min={min}
          inputMode="numeric"
          className="join-item w-full"
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(Number(event.target.value))}
        />
        {suffix && <span className="join-item unit-addon min-w-16">{suffix}</span>}
      </Join>
    </FieldLabel>
  )
}

function TextAreaBox({ label, value, disabled = false, onChange }: { label: string; value: string; disabled?: boolean; onChange: (value: string) => void }) {
  return (
    <FieldLabel title={label}>
      <Textarea
        bordered
        size="sm"
        className="min-h-24 w-full font-mono text-xs"
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
    </FieldLabel>
  )
}

function usageProjectionDescription(mode: ExternalPool['usageProjectionMode'] | undefined) {
  if (mode === 'current_path_policy') {
    return '按当前请求路径重新整理 usage，并应用全局 usage 补偿。适合希望外部池返回特征和本地路径一致的场景。'
  }
  return '严格透传外部池返回的 usage，不应用缓存补偿和输出补偿。适合只把外部池当作直接上游。'
}

function poolUsageSummary(pool: ExternalPool, config: ExternalPoolsConfig) {
  if (pool.usageProjectionMode !== 'current_path_policy') {
    return 'Usage: 严格透传'
  }
  const parts = ['Usage: 按路径整形']
  if (config.externalPoolUsageProjectionUpliftPercent > 0) {
    parts.push(`缓存 +${config.externalPoolUsageProjectionUpliftPercent}%`)
  }
  if (config.externalPoolUsageProjectionOutputUpliftMinTokens > 0 && config.externalPoolUsageProjectionOutputUpliftPercent > 0) {
    parts.push(`输出 >= ${config.externalPoolUsageProjectionOutputUpliftMinTokens} 后 +${config.externalPoolUsageProjectionOutputUpliftPercent}%`)
  }
  return parts.join(' · ')
}

function authLabel(authType: ExternalPool['authType']) {
  return authType === 'x_api_key' ? 'x-api-key' : 'Bearer'
}

function SelectBox({ label, value, disabled = false, onChange, children }: { label: string; value: string; disabled?: boolean; onChange: (value: string) => void; children: ReactNode }) {
  return (
    <FieldLabel title={label}>
      <select className="select select-bordered select-sm w-full" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)}>
        {children}
      </select>
    </FieldLabel>
  )
}
