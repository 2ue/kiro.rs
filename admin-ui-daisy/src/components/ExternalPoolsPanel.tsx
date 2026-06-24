import { type ReactNode, useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircle2, FlaskConical, Loader2, Pencil, Play, Plus, Power, RefreshCw, RotateCcw, RotateCw, Save, Trash2, XCircle } from 'lucide-react'
import { toast } from 'sonner'
import { Button, Card, Input, Join, Toggle, Textarea } from 'react-daisyui'
import { Badge, EmptyState, FieldLabel, ModalShell, SectionCard, Select, useConfirm } from '@/components/common'
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
import type { CreateExternalPoolRequest, ExternalPool, ExternalPoolModelMappingRule, ExternalPoolsConfig, ExternalPoolTestResponse, UpdateExternalPoolRequest } from '@/types/api'

const splitRules = (value: string) => value.split('\n').map((item) => item.trim()).filter(Boolean)
const joinRules = (value: string[] = []) => value.join('\n')
const whole = (value: number, min = 0) => Math.max(min, Math.floor(Number.isFinite(value) ? value : min))
const DEFAULT_POOL_MODEL_MAPPING_MODE: NonNullable<CreateExternalPoolRequest['modelMappingMode']> = 'processed_mapping'

const parseModelMappingRules = (value: string): ExternalPoolModelMappingRule[] => value
  .split('\n')
  .map((line) => line.trim())
  .filter((line) => line && !line.startsWith('#') && !line.startsWith('//'))
  .map((line) => line.split(/\s*(?:->|=>|→|=)\s*/, 2))
  .map(([source, target]) => ({
    enabled: true,
    source: source?.trim() || '',
    target: target?.trim() || '',
    kind: 'alias' as const,
  }))
  .filter((rule) => rule.source && rule.target)

const joinModelMappingRules = (rules: ExternalPoolModelMappingRule[] = []) => rules
  .filter((rule) => rule.source?.trim() && rule.target?.trim())
  .map((rule) => `${rule.source.trim()} -> ${rule.target.trim()}`)
  .join('\n')

type ExternalPoolModelMappingPreset = {
  label: string
  source: string
  target: string
  tone: 'blue' | 'cyan' | 'emerald' | 'purple' | 'amber' | 'rose'
}

const DIRECT_MODEL_MAPPING_PRESETS: ExternalPoolModelMappingPreset[] = [
  { label: 'Sonnet 4 完整ID→4', source: 'claude-sonnet-4-20250514', target: 'claude-sonnet-4', tone: 'blue' },
  { label: 'Sonnet 4 原样', source: 'claude-sonnet-4', target: 'claude-sonnet-4', tone: 'blue' },
  { label: 'Sonnet 4.5 完整ID→4.5', source: 'claude-sonnet-4-5-20250929', target: 'claude-sonnet-4.5', tone: 'blue' },
  { label: 'Sonnet 4.5→4.5', source: 'claude-sonnet-4-5', target: 'claude-sonnet-4.5', tone: 'blue' },
  { label: 'Sonnet 4.5 点号', source: 'claude-sonnet-4.5', target: 'claude-sonnet-4.5', tone: 'blue' },
  { label: 'Sonnet 4.6→4.6', source: 'claude-sonnet-4-6', target: 'claude-sonnet-4.6', tone: 'cyan' },
  { label: 'Sonnet 4.6 点号', source: 'claude-sonnet-4.6', target: 'claude-sonnet-4.6', tone: 'cyan' },
  { label: 'Sonnet 4.7→4.7', source: 'claude-sonnet-4-7', target: 'claude-sonnet-4.7', tone: 'cyan' },
  { label: 'Sonnet 4.7 点号', source: 'claude-sonnet-4.7', target: 'claude-sonnet-4.7', tone: 'cyan' },
  { label: 'Sonnet 4.8→4.8', source: 'claude-sonnet-4-8', target: 'claude-sonnet-4.8', tone: 'cyan' },
  { label: 'Sonnet 4.8 点号', source: 'claude-sonnet-4.8', target: 'claude-sonnet-4.8', tone: 'cyan' },
  { label: 'Opus 4.5 完整ID→4.5', source: 'claude-opus-4-5-20251101', target: 'claude-opus-4.5', tone: 'purple' },
  { label: 'Opus 4.5→4.5', source: 'claude-opus-4-5', target: 'claude-opus-4.5', tone: 'purple' },
  { label: 'Opus 4.5 点号', source: 'claude-opus-4.5', target: 'claude-opus-4.5', tone: 'purple' },
  { label: 'Opus 4-5 thinking→4.5', source: 'claude-opus-4-5-thinking', target: 'claude-opus-4.5-thinking', tone: 'purple' },
  { label: 'Opus 4.6→4.6', source: 'claude-opus-4-6', target: 'claude-opus-4.6', tone: 'purple' },
  { label: 'Opus 4.6 thinking', source: 'claude-opus-4-6-thinking', target: 'claude-opus-4.6-thinking', tone: 'purple' },
  { label: 'Opus 4.7→4.7', source: 'claude-opus-4-7', target: 'claude-opus-4.7', tone: 'purple' },
  { label: 'Opus 4.7 点号', source: 'claude-opus-4.7', target: 'claude-opus-4.7', tone: 'purple' },
  { label: 'Opus 4.8→4.8', source: 'claude-opus-4-8', target: 'claude-opus-4.8', tone: 'purple' },
  { label: 'Opus 4.8 点号', source: 'claude-opus-4.8', target: 'claude-opus-4.8', tone: 'purple' },
  { label: 'Opus 4.8 thinking', source: 'claude-opus-4-8-thinking', target: 'claude-opus-4.8-thinking', tone: 'purple' },
  { label: 'Haiku 4.5 完整ID→4.5', source: 'claude-haiku-4-5-20251001', target: 'claude-haiku-4.5', tone: 'emerald' },
  { label: 'Haiku 4.5→4.5', source: 'claude-haiku-4-5', target: 'claude-haiku-4.5', tone: 'emerald' },
  { label: 'Haiku 4.5 点号', source: 'claude-haiku-4.5', target: 'claude-haiku-4.5', tone: 'emerald' },
  { label: '3.5 Sonnet 完整ID', source: 'claude-3-5-sonnet-20241022', target: 'claude-3.5-sonnet', tone: 'amber' },
  { label: '3.5 Haiku 完整ID', source: 'claude-3-5-haiku-20241022', target: 'claude-3.5-haiku', tone: 'emerald' },
]

const PROCESSED_MODEL_MAPPING_PRESETS: ExternalPoolModelMappingPreset[] = [
  { label: 'Sonnet 4 原样', source: 'claude-sonnet-4', target: 'claude-sonnet-4', tone: 'blue' },
  { label: 'Sonnet 4.5→4-5', source: 'claude-sonnet-4.5', target: 'claude-sonnet-4-5', tone: 'cyan' },
  { label: 'Sonnet 4-5 原样', source: 'claude-sonnet-4-5', target: 'claude-sonnet-4-5', tone: 'cyan' },
  { label: 'Sonnet 4.6→4-6', source: 'claude-sonnet-4.6', target: 'claude-sonnet-4-6', tone: 'cyan' },
  { label: 'Sonnet 4-6 原样', source: 'claude-sonnet-4-6', target: 'claude-sonnet-4-6', tone: 'cyan' },
  { label: 'Sonnet 4.7→4-7', source: 'claude-sonnet-4.7', target: 'claude-sonnet-4-7', tone: 'cyan' },
  { label: 'Sonnet 4.8→4-8', source: 'claude-sonnet-4.8', target: 'claude-sonnet-4-8', tone: 'cyan' },
  { label: 'Opus 4.5→4-5', source: 'claude-opus-4.5', target: 'claude-opus-4-5', tone: 'purple' },
  { label: 'Opus 4-5 原样', source: 'claude-opus-4-5', target: 'claude-opus-4-5', tone: 'purple' },
  { label: 'Opus 4.5 thinking→4-5', source: 'claude-opus-4.5-thinking', target: 'claude-opus-4-5-thinking', tone: 'purple' },
  { label: 'Opus 4-5 thinking', source: 'claude-opus-4-5-thinking', target: 'claude-opus-4-5-thinking', tone: 'purple' },
  { label: 'Opus 4.6→4-6', source: 'claude-opus-4.6', target: 'claude-opus-4-6', tone: 'purple' },
  { label: 'Opus 4.6 thinking→4-6', source: 'claude-opus-4.6-thinking', target: 'claude-opus-4-6-thinking', tone: 'purple' },
  { label: 'Opus 4.7→4-7', source: 'claude-opus-4.7', target: 'claude-opus-4-7', tone: 'purple' },
  { label: 'Opus 4.8→4-8', source: 'claude-opus-4.8', target: 'claude-opus-4-8', tone: 'purple' },
  { label: 'Opus 4.8 thinking', source: 'claude-opus-4.8-thinking', target: 'claude-opus-4-8-thinking', tone: 'purple' },
  { label: 'Haiku 4.5→4-5', source: 'claude-haiku-4.5', target: 'claude-haiku-4-5', tone: 'emerald' },
  { label: 'Haiku 4-5 原样', source: 'claude-haiku-4-5', target: 'claude-haiku-4-5', tone: 'emerald' },
  { label: '3.5 Sonnet→3-5', source: 'claude-3.5-sonnet', target: 'claude-3-5-sonnet', tone: 'amber' },
  { label: '3.5 Haiku→3-5', source: 'claude-3.5-haiku', target: 'claude-3-5-haiku', tone: 'emerald' },
]

const modelMappingPresetsForMode = (mode: ExternalPoolFormDraft['modelMappingMode']) => {
  if (mode === 'passthrough_mapping') return DIRECT_MODEL_MAPPING_PRESETS
  if (mode === 'direct_mapping') return DIRECT_MODEL_MAPPING_PRESETS
  if (mode === 'processed_mapping') return PROCESSED_MODEL_MAPPING_PRESETS
  return []
}

const appendModelMappingPreset = (currentText: string, preset: ExternalPoolModelMappingPreset) => {
  const result = appendModelMappingRules(currentText, [{ enabled: true, source: preset.source, target: preset.target, kind: 'alias' }])
  return { text: result.text, added: result.added > 0 }
}

const appendModelMappingPresets = (currentText: string, presets: ExternalPoolModelMappingPreset[]) => {
  return appendModelMappingRules(currentText, presets.map((preset) => ({ enabled: true, source: preset.source, target: preset.target, kind: 'alias' })))
}

const appendModelMappingRules = (currentText: string, incomingRules: ExternalPoolModelMappingRule[]) => {
  const rules = parseModelMappingRules(currentText)
  const seen = new Set(rules.map((rule) => rule.source.trim().toLowerCase()))
  let added = 0
  incomingRules.forEach((rule) => {
    const source = rule.source?.trim() || ''
    const target = rule.target?.trim() || ''
    const key = source.toLowerCase()
    if (!source || !target || seen.has(key)) return
    seen.add(key)
    rules.push({ enabled: true, source, target, kind: 'alias' })
    added += 1
  })
  return { text: joinModelMappingRules(rules), added }
}

type ExternalPoolFormDraft = {
  name: string
  baseUrl: string
  apiKey: string
  authType: NonNullable<CreateExternalPoolRequest['authType']>
  enabled: boolean
  priority: number
  maxConcurrentRequests: number
  usageProjectionMode: NonNullable<CreateExternalPoolRequest['usageProjectionMode']>
  autoDisablePolicy: NonNullable<CreateExternalPoolRequest['autoDisablePolicy']>
  normalizeModelVersionDots: boolean
  modelMappingMode: NonNullable<CreateExternalPoolRequest['modelMappingMode']>
  modelMappingRequireMatch: boolean
  modelMappingRulesText: string
  notes: string
}

const defaultPoolForm = (): ExternalPoolFormDraft => ({
  name: '',
  baseUrl: '',
  apiKey: '',
  authType: 'bearer',
  enabled: false,
  priority: 100,
  maxConcurrentRequests: 10,
  usageProjectionMode: 'pass_through',
  autoDisablePolicy: 'inherit',
  normalizeModelVersionDots: false,
  modelMappingMode: DEFAULT_POOL_MODEL_MAPPING_MODE,
  modelMappingRequireMatch: false,
  modelMappingRulesText: '',
  notes: '',
})

const poolFormFromPool = (pool: ExternalPool): ExternalPoolFormDraft => ({
  name: pool.name,
  baseUrl: pool.baseUrl,
  apiKey: '',
  authType: pool.authType,
  enabled: pool.enabled,
  priority: pool.priority,
  maxConcurrentRequests: pool.maxConcurrentRequests,
  usageProjectionMode: pool.usageProjectionMode,
  autoDisablePolicy: pool.autoDisablePolicy,
  normalizeModelVersionDots: Boolean(pool.normalizeModelVersionDots),
  modelMappingMode: pool.modelMappingMode || DEFAULT_POOL_MODEL_MAPPING_MODE,
  modelMappingRequireMatch: Boolean(pool.modelMappingRequireMatch),
  modelMappingRulesText: joinModelMappingRules(pool.modelMappingRules || []),
  notes: pool.notes || '',
})

export function ExternalPoolsPanel() {
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
      <SectionCard title="外部账号策略" actions={<Button size="sm" color="primary" loading={savingConfig} onClick={saveConfig}><Save className="h-4 w-4" />保存策略</Button>}>
        <div className="space-y-5">
          <div className="grid gap-3 md:grid-cols-5">
            <SummaryItem label="外部账号" value={externalEnabled ? '已启用' : '已关闭'} />
            <SummaryItem label="入口策略" value={fallbackActive || directPolicyActive ? '已配置' : '未配置'} />
            <SummaryItem label="可用外部账号" value={`${dispatchablePools}/${totalPools}`} />
            <SummaryItem label="外部账号并发" value={`${totalInFlight}/${totalCapacity || 0}`} />
            <SummaryItem label="按入口规则" value={`${currentPathPoolCount} 个`} />
          </div>

          <PolicyBlock
            title="1. 是否启用外部账号"
            active={externalEnabled}
            description="关闭后不会进入任何外部账号，请求只走本地账号。"
          >
            <div className="grid gap-3 md:grid-cols-2">
              <ToggleRow label="启用外部账号" checked={configDraft.externalPoolsEnabled} onChange={(externalPoolsEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolsEnabled }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="2. 什么时候进入外部账号"
            active={externalEnabled}
            description="默认先使用本地账号；本地不可用或命中指定规则时，再使用外部账号。"
          >
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

          <PolicyBlock
            title="3. 进入外部账号后怎么调度"
            active={externalEnabled}
            description="控制外部账号自己的并发、排队、重试和超时。单个外部账号还可以单独设置并发。"
          >
            <div className="space-y-4">
              <FormSection title="容量与排队" description={waitModeActive ? '外部账号满并发时会等待容量；从本地转入外部账号的请求，等待失败后可再尝试回到本地。' : '外部账号满并发时不会排队；从本地转入外部账号的请求，可按回本地策略再尝试本地。'}>
                <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
                  <SelectBox disabled={!externalEnabled} label="满并发处理" value={configDraft.externalPoolCapacityMode} onChange={(externalPoolCapacityMode) => setConfigDraft((prev) => ({ ...prev, externalPoolCapacityMode: externalPoolCapacityMode as ExternalPoolsConfig['externalPoolCapacityMode'] }))}>
                    <Select.Option value="fail_fast">立即失败</Select.Option>
                    <Select.Option value="wait">等待容量</Select.Option>
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

          <PolicyBlock
            title="4. 外部账号异常后怎么处理"
            active={autoDisableActive}
            description="自动禁用只作用于外部账号本身；单个外部账号可选择继承、强制启用或关闭。"
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
            title="5. 返回给客户端的用量"
            active={externalEnabled && usageCompensationActive}
            description="只影响选择“按入口规则展示”的外部账号。本地账号和“保持原样”的外部账号不会受影响。"
          >
            <div className="space-y-4">
              <HintBox>
                生效条件：请求进入外部账号，并且该外部账号的用量模式为“按入口规则展示”。如果外部账号选择“保持原样”，下面配置不会改动用量展示。
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

      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">外部账号列表</h2>
          <p className="text-sm text-base-content/60">单个外部账号配置只影响自身；全局调度、冷却、补偿策略在上方统一保存。</p>
        </div>
        <Button color="primary" onClick={() => { setCreateForm(defaultPoolForm()); setCreateOpen(true) }}>
          <Plus className="h-4 w-4" />
          添加外部账号
        </Button>
      </div>

      <div className="space-y-3">
        {pools.data?.pools.map((pool) => {
          const runtime = statusMap.get(pool.id)
          return (
            <SectionCard key={pool.id} title={`#${pool.id} ${pool.name}`}>
              <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
                <div className="space-y-2">
                  <div className="flex flex-wrap gap-2">
                    <Badge tone={pool.enabled ? 'success' : 'neutral'}>{pool.enabled ? '启用' : '停用'}</Badge>
                    {pool.autoDisabled && <Badge tone="error">自动禁用</Badge>}
                    <Badge tone={runtime?.dispatchable ? 'info' : 'neutral'}>{runtime?.dispatchable ? '可调度' : runtime?.skippedReason || '不可调度'}</Badge>
                  </div>
                  <div className="text-sm text-base-content/70">{pool.baseUrl} · {pool.maskedApiKey || '未显示 Key'} · 并发 {runtime?.inFlight ?? 0}/{pool.maxConcurrentRequests} · 优先级 {pool.priority}</div>
                  <div className="text-xs text-base-content/50">{poolUsageSummary(pool, configDraft)} · 认证：{authLabel(pool.authType)} · 模型：{poolModelMappingSummary(pool)} {runtime?.cooldownRemainingSecs ? `· 冷却 ${runtime.cooldownRemainingSecs}s` : ''}</div>
                  {pool.autoDisabledLastError && <div className="text-xs text-error">{pool.autoDisabledLastError}</div>}
                </div>
                <div className="flex flex-wrap gap-2">
                  <Button size="sm" color="ghost" onClick={() => startEdit(pool)}><Pencil className="h-4 w-4" />编辑</Button>
                  <Button size="sm" color="ghost" onClick={() => setTestingPool(pool)}><FlaskConical className="h-4 w-4" />测试</Button>
                  <Button size="sm" color="ghost" onClick={() => mutatePool(() => setExternalPoolEnabled(pool.id, !pool.enabled), pool.enabled ? '已停用' : '已启用')}><Power className="h-4 w-4" />{pool.enabled ? '停用' : '启用'}</Button>
                  <Button size="sm" color="ghost" onClick={() => mutatePool(() => clearExternalPoolAutoDisabled(pool.id), '自动禁用状态已清除')}><RotateCcw className="h-4 w-4" />清除禁用</Button>
                  <Button size="sm" color="ghost" onClick={() => status.refetch()}><RefreshCw className="h-4 w-4" />刷新</Button>
                  <Button
                    size="sm"
                    color="error"
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
            </SectionCard>
          )
        })}
        {!pools.isLoading && !pools.data?.pools.length && <EmptyState text="暂无外部账号" />}
      </div>
      <ExternalPoolFormModal
        mode="create"
        open={createOpen}
        draft={createForm}
        saving={savingPool}
        onDraftChange={setCreateForm}
        onClose={() => {
          if (savingPool) return
          setCreateOpen(false)
          setCreateForm(defaultPoolForm())
        }}
        onSubmit={submitPool}
      />
      <ExternalPoolFormModal
        mode="edit"
        pool={editingPool}
        open={Boolean(editingPool)}
        draft={editForm}
        saving={savingPool}
        onDraftChange={setEditForm}
        onClose={() => {
          if (savingPool) return
          setEditingPool(null)
          setEditForm(defaultPoolForm())
        }}
        onSubmit={savePoolEdit}
      />
      <ExternalPoolTestModal
        pool={testingPool}
        open={Boolean(testingPool)}
        onClose={() => setTestingPool(null)}
        onDone={invalidate}
      />
    </div>
  )
}

function ExternalPoolFormModal({
  mode,
  pool,
  open,
  draft,
  saving,
  onDraftChange,
  onClose,
  onSubmit,
}: {
  mode: 'create' | 'edit'
  pool?: ExternalPool | null
  open: boolean
  draft: ExternalPoolFormDraft
  saving: boolean
  onDraftChange: (value: ExternalPoolFormDraft | ((prev: ExternalPoolFormDraft) => ExternalPoolFormDraft)) => void
  onClose: () => void
  onSubmit: () => void
}) {
  const isEdit = mode === 'edit'
  const title = isEdit ? `编辑外部账号${pool ? ` #${pool.id}` : ''}` : '添加外部账号'
  const keyLabel = isEdit ? '新请求 Key' : '请求 Key'
  const keyDescription = isEdit ? `留空表示不修改当前 Key。当前：${pool?.maskedApiKey || '未显示 Key'}` : '外部账号的请求密钥，保存后只显示脱敏值。'
  const [quickImportText, setQuickImportText] = useState('')
  const mappingPresets = useMemo(() => modelMappingPresetsForMode(draft.modelMappingMode), [draft.modelMappingMode])
  useEffect(() => {
    if (!open) setQuickImportText('')
  }, [open])
  const addMappingPreset = (preset: ExternalPoolModelMappingPreset) => {
    const result = appendModelMappingPreset(draft.modelMappingRulesText, preset)
    onDraftChange((prev) => ({ ...prev, modelMappingRulesText: result.text }))
    if (result.added) {
      toast.success('模型映射规则已添加')
    } else {
      toast.info('该模型映射规则已存在')
    }
  }
  const addAllMappingPresets = () => {
    const result = appendModelMappingPresets(draft.modelMappingRulesText, mappingPresets)
    onDraftChange((prev) => ({ ...prev, modelMappingRulesText: result.text }))
    if (result.added > 0) {
      toast.success(`已添加 ${result.added} 条模型映射规则`)
    } else {
      toast.info('快捷模型映射规则都已存在')
    }
  }
  const importMappingRules = () => {
    const rules = parseModelMappingRules(quickImportText)
    if (rules.length === 0) {
      toast.error('没有可导入的模型映射规则')
      return
    }
    const result = appendModelMappingRules(draft.modelMappingRulesText, rules)
    onDraftChange((prev) => ({ ...prev, modelMappingRulesText: result.text }))
    if (result.added > 0) {
      toast.success(`已导入 ${result.added} 条模型映射规则`)
      setQuickImportText('')
    } else {
      toast.info('导入的模型映射规则都已存在')
    }
  }

  return (
    <ModalShell
      open={open}
      title={title}
      width="max-w-3xl"
      onClose={onClose}
      footer={
        <>
          <Button type="button" size="sm" color="ghost" onClick={onClose} disabled={saving}>
            取消
          </Button>
          <Button type="button" size="sm" color="primary" onClick={onSubmit} disabled={saving}>
            {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : isEdit ? <Save className="h-4 w-4" /> : <Plus className="h-4 w-4" />}
            {isEdit ? '保存外部账号' : '添加外部账号'}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <FormSection title="连接信息" description="系统会使用这里的服务地址和 Key 连接外部账号。">
          <div className="grid gap-3 md:grid-cols-2">
            <TextBox label="名称" value={draft.name} disabled={saving} onChange={(name) => onDraftChange((prev) => ({ ...prev, name }))} />
            <SelectBox label="认证方式" value={draft.authType} disabled={saving} onChange={(authType) => onDraftChange((prev) => ({ ...prev, authType: authType as ExternalPoolFormDraft['authType'] }))}>
              <Select.Option value="bearer">Authorization: Bearer &lt;key&gt;</Select.Option>
              <Select.Option value="x_api_key">x-api-key: &lt;key&gt;</Select.Option>
            </SelectBox>
            <TextBox className="md:col-span-2" label="服务地址" description="填写服务地址即可，通常不需要带具体请求路径。" value={draft.baseUrl} disabled={saving} onChange={(baseUrl) => onDraftChange((prev) => ({ ...prev, baseUrl }))} />
            <TextBox className="md:col-span-2" label={keyLabel} description={keyDescription} value={draft.apiKey} disabled={saving} onChange={(apiKey) => onDraftChange((prev) => ({ ...prev, apiKey }))} />
          </div>
        </FormSection>

        <div className="grid gap-4 lg:grid-cols-2">
          <FormSection title="调度设置" description="这些设置只影响当前外部账号，不改变全局排队和冷却策略。">
            <div className="grid gap-3 sm:grid-cols-2">
              <NumberBox label="单账号最大并发" description="当前外部账号同时处理的最大请求数。" suffix="并发" value={draft.maxConcurrentRequests} min={1} disabled={saving} onChange={(maxConcurrentRequests) => onDraftChange((prev) => ({ ...prev, maxConcurrentRequests }))} />
              <NumberBox label="优先级" description="数字越小越靠前；同优先级再按容量和状态分配。" suffix="值" value={draft.priority} disabled={saving} onChange={(priority) => onDraftChange((prev) => ({ ...prev, priority }))} />
              <ToggleRow label={isEdit ? '启用外部账号' : '创建后立即启用'} checked={Boolean(draft.enabled)} disabled={saving} onChange={(enabled) => onDraftChange((prev) => ({ ...prev, enabled }))} />
              <ToggleRow label="未命中时点号转横杠" checked={Boolean(draft.normalizeModelVersionDots)} disabled={saving || draft.modelMappingMode === 'passthrough' || draft.modelMappingRequireMatch} onChange={(normalizeModelVersionDots) => onDraftChange((prev) => ({ ...prev, normalizeModelVersionDots }))} />
            </div>
          </FormSection>

          <FormSection title="用量与成本" description="只控制当前外部账号返回给客户端的用量展示方式。">
            <div className="space-y-3">
              <SelectBox label="用量展示模式" value={draft.usageProjectionMode} disabled={saving} onChange={(usageProjectionMode) => onDraftChange((prev) => ({ ...prev, usageProjectionMode: usageProjectionMode as ExternalPoolFormDraft['usageProjectionMode'] }))}>
                <Select.Option value="pass_through">保持原样：不改外部账号用量</Select.Option>
                <Select.Option value="current_path_policy">按入口规则展示：应用全局补偿</Select.Option>
              </SelectBox>
              <HintBox>{usageProjectionDescription(draft.usageProjectionMode)}</HintBox>
            </div>
          </FormSection>
        </div>

        <FormSection title="模型处理" description="控制当前外部账号发出请求时的模型名称处理方式。">
          <div className="grid gap-3 md:grid-cols-[240px_1fr]">
            <div className="space-y-3">
              <SelectBox label="映射模式" value={draft.modelMappingMode} disabled={saving} onChange={(modelMappingMode) => onDraftChange((prev) => ({ ...prev, modelMappingMode: modelMappingMode as ExternalPoolFormDraft['modelMappingMode'] }))}>
                <Select.Option value="passthrough">直接使用请求模型</Select.Option>
                <Select.Option value="passthrough_mapping">请求模型优先映射</Select.Option>
                <Select.Option value="direct_mapping">映射后内部处理</Select.Option>
                <Select.Option value="processed_mapping">内部处理后映射</Select.Option>
              </SelectBox>
              <HintBox>{modelMappingDescription(draft.modelMappingMode, draft.normalizeModelVersionDots)}</HintBox>
              {draft.modelMappingMode !== 'passthrough' && (
                <ToggleRow label="必须命中映射" checked={Boolean(draft.modelMappingRequireMatch)} disabled={saving} onChange={(modelMappingRequireMatch) => onDraftChange((prev) => ({ ...prev, modelMappingRequireMatch }))} />
              )}
            </div>
            {draft.modelMappingMode !== 'passthrough' && (
              <div className="space-y-3">
                <TextAreaBox
                  label="映射规则"
                  description="每行一条：claude-sonnet-4-5-20250929 -> claude-sonnet-4.5"
                  value={draft.modelMappingRulesText}
                  disabled={saving}
                  action={<Button type="button" size="xs" color="ghost" onClick={addAllMappingPresets} disabled={saving || mappingPresets.length === 0}>全部添加</Button>}
                  onChange={(modelMappingRulesText) => onDraftChange((prev) => ({ ...prev, modelMappingRulesText }))}
                />
                <ModelMappingPresetTags presets={mappingPresets} disabled={saving} onSelect={addMappingPreset} />
                <TextAreaBox
                  label="快捷导入"
                  description="粘贴多行 source -> target，点击解析导入后追加到上方规则。"
                  value={quickImportText}
                  disabled={saving}
                  action={<Button type="button" size="xs" color="ghost" onClick={importMappingRules} disabled={saving || !quickImportText.trim()}>解析导入</Button>}
                  onChange={setQuickImportText}
                />
              </div>
            )}
          </div>
        </FormSection>

        <FormSection title="错误处理和备注" description="自动禁用策略只决定当前外部账号是否继承全局自动禁用规则。">
          <div className="grid gap-3 md:grid-cols-2">
            <SelectBox label="自动禁用策略" value={draft.autoDisablePolicy} disabled={saving} onChange={(autoDisablePolicy) => onDraftChange((prev) => ({ ...prev, autoDisablePolicy: autoDisablePolicy as ExternalPoolFormDraft['autoDisablePolicy'] }))}>
              <Select.Option value="inherit">继承全局自动禁用</Select.Option>
              <Select.Option value="enabled">单独启用自动禁用</Select.Option>
              <Select.Option value="disabled">关闭自动禁用</Select.Option>
            </SelectBox>
            <TextBox label="备注" value={draft.notes} disabled={saving} onChange={(notes) => onDraftChange((prev) => ({ ...prev, notes }))} />
          </div>
        </FormSection>

        {!isEdit && !draft.enabled && (
          <HintBox>
            当前选择为创建后不立即启用。保存后可以先在列表里测试连接，再手动启用参与调度。
          </HintBox>
        )}
      </div>
    </ModalShell>
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
        toast.success(response.message || '外部账号模型调用测试通过')
      } else {
        toast.error(response.message || '外部账号模型调用测试失败')
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
      title="测试外部账号"
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

          <div className="rounded-box border border-base-300 bg-base-200 p-4 font-mono text-sm text-base-content">
            <div className="space-y-1">
              <div><span className="text-info">外部账号：</span><span> #{pool.id} {pool.name}</span></div>
              <div><span className="text-info">使用模型：</span><span> {model}</span></div>
              <div><span className="text-base-content/55">发送测试消息：</span><span> "{prompt.trim() || DEFAULT_TEST_PROMPT}"</span></div>
            </div>
            <div className="mt-4 border-t border-base-300 pt-4">
              {running && (
                <div className="flex items-center gap-2 text-info">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  正在等待外部账号模型响应...
                </div>
              )}
              {result && (
                <div className={result.ok ? 'space-y-3 text-success' : 'space-y-3 text-error'}>
                  <div>
                    {result.ok ? <CheckCircle2 className="mr-2 inline h-4 w-4" /> : <XCircle className="mr-2 inline h-4 w-4" />}
                    {result.message}
                  </div>
                  <div className="text-base-content/55">HTTP 状态：{result.status ?? '-'}</div>
                  {result.model && <div className="text-base-content/55">返回模型：{result.model}</div>}
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
              {!running && !result && !error && <div className="text-base-content/55">等待开始测试</div>}
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

function TextAreaBox({
  label,
  description,
  value,
  disabled = false,
  action,
  onChange,
}: {
  label: string
  description?: string
  value: string
  disabled?: boolean
  action?: ReactNode
  onChange: (value: string) => void
}) {
  return (
    <div>
      <div className="mb-1 flex items-start justify-between gap-3">
        <div>
          <div className="text-sm font-medium">{label}</div>
          {description && <div className="mt-1 text-xs leading-4 text-base-content/50">{description}</div>}
        </div>
        {action}
      </div>
      <Textarea
        bordered
        size="sm"
        className="min-h-24 w-full font-mono text-xs"
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
    </div>
  )
}

function ModelMappingPresetTags({
  presets,
  disabled,
  onSelect,
}: {
  presets: ExternalPoolModelMappingPreset[]
  disabled?: boolean
  onSelect: (preset: ExternalPoolModelMappingPreset) => void
}) {
  if (presets.length === 0) return null
  return (
    <div className="flex flex-wrap gap-2">
      {presets.map((preset) => (
        <button
          key={`${preset.source}->${preset.target}`}
          type="button"
          className={`rounded-lg px-3 py-1 text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${modelMappingPresetClass(preset.tone)}`}
          title={`${preset.source} -> ${preset.target}`}
          disabled={disabled}
          onClick={() => onSelect(preset)}
        >
          + {preset.label}
        </button>
      ))}
    </div>
  )
}

function modelMappingPresetClass(tone: ExternalPoolModelMappingPreset['tone']) {
  switch (tone) {
    case 'cyan':
      return 'bg-cyan-100 text-cyan-700 hover:bg-cyan-200'
    case 'emerald':
      return 'bg-emerald-100 text-emerald-700 hover:bg-emerald-200'
    case 'purple':
      return 'bg-purple-100 text-purple-700 hover:bg-purple-200'
    case 'amber':
      return 'bg-amber-100 text-amber-700 hover:bg-amber-200'
    case 'rose':
      return 'bg-rose-100 text-rose-700 hover:bg-rose-200'
    case 'blue':
    default:
      return 'bg-blue-100 text-blue-700 hover:bg-blue-200'
  }
}

function modelMappingDescription(mode: ExternalPool['modelMappingMode'] | undefined, normalizeFallback: boolean) {
  const processedFallback = normalizeFallback ? '未命中后使用内部处理模型，并把数字点号转横杠。' : '未命中后使用内部处理模型。'
  if (mode === 'passthrough') return '直接使用客户端请求里的模型，不应用映射规则和兜底转换。'
  if (mode === 'passthrough_mapping') return '先用客户端请求模型匹配规则；未命中时仍使用原请求模型。'
  if (mode === 'direct_mapping') return `用客户端请求模型匹配规则；${processedFallback}`
  return `先使用本系统解析后的模型匹配规则；${processedFallback}`
}

function poolModelMappingSummary(pool: ExternalPool) {
  if (pool.modelMappingMode === 'passthrough') return '原样'
  const count = pool.modelMappingRules?.length || 0
  const mode = pool.modelMappingMode === 'passthrough_mapping'
    ? '原样+映射'
    : pool.modelMappingMode === 'direct_mapping'
      ? '映射+内部'
      : '内部+映射'
  const fallback = pool.modelMappingRequireMatch ? '必须命中' : pool.normalizeModelVersionDots ? '未命中4.8->4-8' : '允许未命中'
  return `${mode}${count ? ` ${count}条` : ''} · ${fallback}`
}

function usageProjectionDescription(mode: ExternalPool['usageProjectionMode'] | undefined) {
  if (mode === 'current_path_policy') {
    return '按当前入口规则整理用量，并应用全局用量补偿。适合希望外部账号展示方式和本地入口一致的场景。'
  }
  return '保持外部账号返回的用量，不应用缓存补偿和输出补偿。适合只做外部连接的场景。'
}

function poolUsageSummary(pool: ExternalPool, config: ExternalPoolsConfig) {
  if (pool.usageProjectionMode !== 'current_path_policy') {
    return '用量：保持原样'
  }
  const parts = ['用量：按入口规则']
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
      <Select size="sm" className="w-full" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)}>
        {children}
      </Select>
    </FieldLabel>
  )
}
