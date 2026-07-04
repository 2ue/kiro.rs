import { type ReactNode, useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircle2, FlaskConical, Loader2, Pencil, Play, Plus, Power, RefreshCw, RotateCcw, RotateCw, Save, Trash2, XCircle } from 'lucide-react'
import { toast } from 'sonner'
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
import { extractErrorMessage } from '@/lib/utils'
import { useRuntimeConfig } from '@/hooks/use-credentials'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { useModelCapabilities } from '@/hooks/use-usage'
import { DEFAULT_TEST_MODEL, DEFAULT_TEST_PROMPT, TEST_MODELS } from '@/lib/test-models'
import type { CreateExternalPoolRequest, ExternalPool, ExternalPoolModelMappingRule, ExternalPoolsConfig, ExternalPoolTestResponse, UpdateExternalPoolRequest } from '@/types/api'
import { defaultExternalPoolsConfig } from '@/components/runtime-config-panel'

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
  { label: 'Sonnet 4透传', source: 'claude-sonnet-4', target: 'claude-sonnet-4', tone: 'blue' },
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
  { label: 'Sonnet 4透传', source: 'claude-sonnet-4', target: 'claude-sonnet-4', tone: 'blue' },
  { label: 'Sonnet 4.5→4-5', source: 'claude-sonnet-4.5', target: 'claude-sonnet-4-5', tone: 'cyan' },
  { label: 'Sonnet 4-5透传', source: 'claude-sonnet-4-5', target: 'claude-sonnet-4-5', tone: 'cyan' },
  { label: 'Sonnet 4.6→4-6', source: 'claude-sonnet-4.6', target: 'claude-sonnet-4-6', tone: 'cyan' },
  { label: 'Sonnet 4-6透传', source: 'claude-sonnet-4-6', target: 'claude-sonnet-4-6', tone: 'cyan' },
  { label: 'Sonnet 4.7→4-7', source: 'claude-sonnet-4.7', target: 'claude-sonnet-4-7', tone: 'cyan' },
  { label: 'Sonnet 4.8→4-8', source: 'claude-sonnet-4.8', target: 'claude-sonnet-4-8', tone: 'cyan' },
  { label: 'Opus 4.5→4-5', source: 'claude-opus-4.5', target: 'claude-opus-4-5', tone: 'purple' },
  { label: 'Opus 4-5透传', source: 'claude-opus-4-5', target: 'claude-opus-4-5', tone: 'purple' },
  { label: 'Opus 4.5 thinking→4-5', source: 'claude-opus-4.5-thinking', target: 'claude-opus-4-5-thinking', tone: 'purple' },
  { label: 'Opus 4-5 thinking', source: 'claude-opus-4-5-thinking', target: 'claude-opus-4-5-thinking', tone: 'purple' },
  { label: 'Opus 4.6→4-6', source: 'claude-opus-4.6', target: 'claude-opus-4-6', tone: 'purple' },
  { label: 'Opus 4.6 thinking→4-6', source: 'claude-opus-4.6-thinking', target: 'claude-opus-4-6-thinking', tone: 'purple' },
  { label: 'Opus 4.7→4-7', source: 'claude-opus-4.7', target: 'claude-opus-4-7', tone: 'purple' },
  { label: 'Opus 4.8→4-8', source: 'claude-opus-4.8', target: 'claude-opus-4-8', tone: 'purple' },
  { label: 'Opus 4.8 thinking', source: 'claude-opus-4.8-thinking', target: 'claude-opus-4-8-thinking', tone: 'purple' },
  { label: 'Haiku 4.5→4-5', source: 'claude-haiku-4.5', target: 'claude-haiku-4-5', tone: 'emerald' },
  { label: 'Haiku 4-5透传', source: 'claude-haiku-4-5', target: 'claude-haiku-4-5', tone: 'emerald' },
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
  skipNonStreamUsageProjection: boolean
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
  skipNonStreamUsageProjection: false,
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
  skipNonStreamUsageProjection: Boolean(pool.skipNonStreamUsageProjection),
  autoDisablePolicy: pool.autoDisablePolicy,
  normalizeModelVersionDots: Boolean(pool.normalizeModelVersionDots),
  modelMappingMode: pool.modelMappingMode || DEFAULT_POOL_MODEL_MAPPING_MODE,
  modelMappingRequireMatch: Boolean(pool.modelMappingRequireMatch),
  modelMappingRulesText: joinModelMappingRules(pool.modelMappingRules || []),
  notes: pool.notes || '',
})

export function ExternalPoolsPanel() {
  const queryClient = useQueryClient()
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
    const externalPools = {
      ...defaultExternalPoolsConfig(),
      ...runtimeConfig.data?.externalPools,
    }
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
    queryClient.invalidateQueries({ queryKey: ['usage-records'] })
  }

  const saveConfig = async () => {
    if (!runtimeConfig.data) {
      toast.error('运行配置尚未加载')
      return
    }
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
      queryClient.invalidateQueries({ queryKey: ['runtimeConfig'] })
      invalidate()
    } catch (error) {
      toast.error(extractErrorMessage(error))
    } finally {
      setSavingConfig(false)
    }
  }

  const submitPool = async () => {
    if (savingPool) return
    if (!createForm.name.trim() || !createForm.baseUrl.trim() || !createForm.apiKey.trim()) {
      toast.error('名称、Base URL 和 Key 必填')
      return
    }
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
      toast.success('外部池已添加')
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
    if (!editForm.name?.trim() || !editForm.baseUrl?.trim()) {
      toast.error('名称和 Base URL 必填')
      return
    }
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
      toast.success('外部池已更新')
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
  const directPolicyActive = externalEnabled && configDraft.externalDirectPolicyEnabled
  const fallbackActive = externalEnabled && !directPolicyActive && (
    configDraft.localPoolPreflightEnabled
    || configDraft.fallbackOnLocalCapacityExhausted
    || configDraft.fallbackOnNoAvailableCredentials
    || configDraft.fallbackOnLocalTransientExhausted
    || configDraft.fallbackOnUnsupportedModel
  )
  const autoDisableActive = externalEnabled && configDraft.externalPoolAutoDisableEnabled
  const waitModeActive = externalEnabled && configDraft.externalPoolCapacityMode === 'wait'
  const localRescueActive = externalEnabled && !directPolicyActive && configDraft.externalPoolLocalRescueEnabled
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
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Power className="h-5 w-5" />
            备用号池配置
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-5">
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
            <div className="grid gap-4 md:grid-cols-2">
              <Toggle label="启用备用号池" checked={configDraft.externalPoolsEnabled} onChange={(externalPoolsEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolsEnabled }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="2. 什么时候进入备用池"
            active={externalEnabled}
            description="启用显式直连后，所有请求跳过本地凭证，只调度外部池；关闭后才使用本地优先 fallback。"
          >
            <div className="grid gap-4 lg:grid-cols-2">
              <FormSection title="本地优先 fallback" description="先调度本地凭证，只有下面情况出现时才转外部池。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <Toggle disabled={!externalEnabled || directPolicyActive} label="本地容量预检" checked={configDraft.localPoolPreflightEnabled} onChange={(localPoolPreflightEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolPreflightEnabled }))} />
                  <Toggle disabled={!externalEnabled || directPolicyActive} label="容量不足 fallback" checked={configDraft.fallbackOnLocalCapacityExhausted} onChange={(fallbackOnLocalCapacityExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalCapacityExhausted }))} />
                  <Toggle disabled={!externalEnabled || directPolicyActive} label="无可用凭据 fallback" checked={configDraft.fallbackOnNoAvailableCredentials} onChange={(fallbackOnNoAvailableCredentials) => setConfigDraft((prev) => ({ ...prev, fallbackOnNoAvailableCredentials }))} />
                  <Toggle disabled={!externalEnabled || directPolicyActive} label="瞬态错误耗尽 fallback" checked={configDraft.fallbackOnLocalTransientExhausted} onChange={(fallbackOnLocalTransientExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalTransientExhausted }))} />
                  <Toggle disabled={!externalEnabled || directPolicyActive} label="模型不支持 fallback" checked={configDraft.fallbackOnUnsupportedModel} onChange={(fallbackOnUnsupportedModel) => setConfigDraft((prev) => ({ ...prev, fallbackOnUnsupportedModel }))} />
                </div>
              </FormSection>

              <FormSection title="显式直连" description="开关打开即全量直连外部池；模型和路径规则只用于细分记录的直连原因。">
                <div className="grid gap-3 sm:grid-cols-2">
                  <Toggle disabled={!externalEnabled} label="启用显式直连" checked={configDraft.externalDirectPolicyEnabled} onChange={(externalDirectPolicyEnabled) => setConfigDraft((prev) => ({ ...prev, externalDirectPolicyEnabled }))} />
                  <Toggle disabled={!directPolicyActive} label="记录本地熔断原因" checked={configDraft.directExternalOnLocalMaintenance} onChange={(directExternalOnLocalMaintenance) => setConfigDraft((prev) => ({ ...prev, directExternalOnLocalMaintenance }))} />
                </div>
                <div className="mt-3 grid gap-3 sm:grid-cols-2">
                  <TextArea disabled={!directPolicyActive} label="模型原因规则" value={modelRulesText} onChange={setModelRulesText} />
                  <TextArea disabled={!directPolicyActive} label="路径原因规则" value={pathRulesText} onChange={setPathRulesText} />
                </div>
                <div className="mt-3 grid gap-3 md:grid-cols-5">
                  <Toggle disabled={!directPolicyActive} label="启用本地熔断统计" checked={configDraft.localPoolCircuitEnabled} onChange={(localPoolCircuitEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitEnabled }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="熔断窗口秒数" value={configDraft.localPoolCircuitWindowSecs} min={1} onChange={(localPoolCircuitWindowSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitWindowSecs }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="失败次数阈值" value={configDraft.localPoolCircuitOpenAfterFailures} min={1} onChange={(localPoolCircuitOpenAfterFailures) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenAfterFailures }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="涉及凭证数" value={configDraft.localPoolCircuitRequireDistinctCredentials} min={1} onChange={(localPoolCircuitRequireDistinctCredentials) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitRequireDistinctCredentials }))} />
                  <NumberBox disabled={!directPolicyActive || !configDraft.localPoolCircuitEnabled} label="熔断秒数" value={configDraft.localPoolCircuitOpenSecs} min={1} onChange={(localPoolCircuitOpenSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenSecs }))} />
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
                <div className="grid gap-3 md:grid-cols-4">
                  <SelectBox disabled={!externalEnabled} label="满并发处理" value={configDraft.externalPoolCapacityMode} onChange={(externalPoolCapacityMode) => setConfigDraft((prev) => ({ ...prev, externalPoolCapacityMode: externalPoolCapacityMode as ExternalPoolsConfig['externalPoolCapacityMode'] }))}>
                    <option value="fail_fast">立即失败</option>
                    <option value="wait">等待容量</option>
                  </SelectBox>
                  <NumberBox disabled={!externalEnabled} label="全局并发上限" value={configDraft.externalPoolGlobalMaxConcurrentRequests} onChange={(externalPoolGlobalMaxConcurrentRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolGlobalMaxConcurrentRequests }))} />
                  <NumberBox disabled={!waitModeActive} label="排队上限" value={configDraft.externalPoolMaxQueuedRequests} onChange={(externalPoolMaxQueuedRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolMaxQueuedRequests }))} />
                  <NumberBox disabled={!waitModeActive} label="最大等待秒数" value={configDraft.externalPoolDispatchMaxWaitSecs} onChange={(externalPoolDispatchMaxWaitSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolDispatchMaxWaitSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="最大重试次数" value={configDraft.externalPoolRetryMaxAttempts} onChange={(externalPoolRetryMaxAttempts) => setConfigDraft((prev) => ({ ...prev, externalPoolRetryMaxAttempts }))} />
                </div>
              </FormSection>

              <FormSection title="冷却与超时" description="冷却用于临时避开出错外部池；流式空闲超时用于防止长时间无输出。">
                <div className="grid gap-3 md:grid-cols-4">
                  <NumberBox disabled={!externalEnabled} label="429 冷却秒数" value={configDraft.externalPoolRateLimitCooldownSecs} min={1} onChange={(externalPoolRateLimitCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolRateLimitCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="5xx 冷却秒数" value={configDraft.externalPoolServerErrorCooldownSecs} min={1} onChange={(externalPoolServerErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolServerErrorCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="网络错误冷却秒数" value={configDraft.externalPoolNetworkErrorCooldownSecs} min={1} onChange={(externalPoolNetworkErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolNetworkErrorCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="协议/认证冷却秒数" value={configDraft.externalPoolProtocolErrorCooldownSecs} min={1} onChange={(externalPoolProtocolErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolProtocolErrorCooldownSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="非流式总超时" value={configDraft.externalPoolRequestTimeoutSecs} onChange={(externalPoolRequestTimeoutSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolRequestTimeoutSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="流式总超时" value={configDraft.externalPoolStreamRequestTimeoutSecs} onChange={(externalPoolStreamRequestTimeoutSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolStreamRequestTimeoutSecs }))} />
                  <NumberBox disabled={!externalEnabled} label="流式空闲超时" value={configDraft.externalPoolStreamIdleTimeoutSecs} onChange={(externalPoolStreamIdleTimeoutSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolStreamIdleTimeoutSecs }))} />
                </div>
              </FormSection>

              <FormSection title="备用池失败后回本地" description="仅对本地优先 fallback 到备用池的请求生效；显式直连开启时不会回本地。">
                <div className="grid gap-3 md:grid-cols-4">
                  <Toggle disabled={!externalEnabled || directPolicyActive} label="启用回本地" checked={configDraft.externalPoolLocalRescueEnabled} onChange={(externalPoolLocalRescueEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueEnabled }))} />
                  <Toggle disabled={!localRescueActive} label="429 时回本地" checked={configDraft.externalPoolLocalRescueOnRateLimit} onChange={(externalPoolLocalRescueOnRateLimit) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueOnRateLimit }))} />
                  <Toggle disabled={!localRescueActive} label="超时时回本地" checked={configDraft.externalPoolLocalRescueOnTimeout} onChange={(externalPoolLocalRescueOnTimeout) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueOnTimeout }))} />
                  <Toggle disabled={!localRescueActive} label="容量失败回本地" checked={configDraft.externalPoolLocalRescueOnCapacity} onChange={(externalPoolLocalRescueOnCapacity) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueOnCapacity }))} />
                  <NumberBox disabled={!localRescueActive} label="回本地最多等待" description="0 表示只立刻探测可用本地槽位；默认 15 秒。" value={configDraft.externalPoolLocalRescueMaxWaitSecs} onChange={(externalPoolLocalRescueMaxWaitSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolLocalRescueMaxWaitSecs }))} />
                </div>
              </FormSection>
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="4. 外部池异常后怎么处理"
            active={autoDisableActive}
            description="自动禁用只作用于外部池本身；单个外部池可选择继承、强制启用或关闭。"
          >
            <div className="grid gap-3 md:grid-cols-4">
              <Toggle disabled={!externalEnabled} label="启用自动禁用" checked={configDraft.externalPoolAutoDisableEnabled} onChange={(externalPoolAutoDisableEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableEnabled }))} />
              <Toggle disabled={!autoDisableActive} label="认证错误" checked={configDraft.externalPoolAutoDisableOnAuthError} onChange={(externalPoolAutoDisableOnAuthError) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnAuthError }))} />
              <Toggle disabled={!autoDisableActive} label="安全锁定" checked={configDraft.externalPoolAutoDisableOnSecurityLock} onChange={(externalPoolAutoDisableOnSecurityLock) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnSecurityLock }))} />
              <Toggle disabled={!autoDisableActive} label="额度耗尽" checked={configDraft.externalPoolAutoDisableOnQuotaExhausted} onChange={(externalPoolAutoDisableOnQuotaExhausted) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnQuotaExhausted }))} />
              <Toggle disabled={!autoDisableActive} label="配置错误" checked={configDraft.externalPoolAutoDisableOnMisconfiguredEndpoint} onChange={(externalPoolAutoDisableOnMisconfiguredEndpoint) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnMisconfiguredEndpoint }))} />
              <Toggle disabled={!autoDisableActive} label="通道禁用" checked={configDraft.externalPoolAutoDisableOnChannelDisabled} onChange={(externalPoolAutoDisableOnChannelDisabled) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnChannelDisabled }))} />
              <NumberBox disabled={!autoDisableActive} label="触发阈值" value={configDraft.externalPoolAutoDisableFailureThreshold} min={1} onChange={(externalPoolAutoDisableFailureThreshold) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableFailureThreshold }))} />
              <NumberBox disabled={!autoDisableActive} label="统计窗口秒数" value={configDraft.externalPoolAutoDisableWindowSecs} min={1} onChange={(externalPoolAutoDisableWindowSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableWindowSecs }))} />
              <NumberBox disabled={!autoDisableActive} label="禁用秒数" value={configDraft.externalPoolAutoDisableDurationSecs} onChange={(externalPoolAutoDisableDurationSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableDurationSecs }))} />
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
                    <Toggle disabled={!externalEnabled} label="启用缓存补偿" checked={cacheUpliftActive} onChange={setCacheUpliftEnabled} />
                    <NumberBox disabled={!cacheUpliftActive} label="放大百分比" value={configDraft.externalPoolUsageProjectionUpliftPercent} onChange={(externalPoolUsageProjectionUpliftPercent) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionUpliftPercent }))} />
                  </div>
                </FormSection>

                <FormSection title="输出 token 补偿" description="当输出达到阈值后，放大最终上报给下游的 output_tokens。">
                  <div className="grid gap-3 sm:grid-cols-3">
                    <Toggle disabled={!externalEnabled} label="启用输出补偿" checked={outputUpliftActive} onChange={setOutputUpliftEnabled} />
                    <NumberBox disabled={!outputUpliftActive} label="输出阈值" value={configDraft.externalPoolUsageProjectionOutputUpliftMinTokens} onChange={(externalPoolUsageProjectionOutputUpliftMinTokens) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionOutputUpliftMinTokens }))} />
                    <NumberBox disabled={!outputUpliftActive} label="放大百分比" value={configDraft.externalPoolUsageProjectionOutputUpliftPercent} onChange={(externalPoolUsageProjectionOutputUpliftPercent) => setConfigDraft((prev) => ({ ...prev, externalPoolUsageProjectionOutputUpliftPercent }))} />
                  </div>
                </FormSection>
              </div>
            </div>
          </PolicyBlock>

          <Button onClick={saveConfig} disabled={savingConfig || runtimeConfig.isLoading}>
            <Save className="mr-2 h-4 w-4" />
            保存策略
          </Button>
        </CardContent>
      </Card>

      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">外部池列表</h2>
          <p className="text-sm text-muted-foreground">单池配置只影响对应外部池；全局调度、冷却、补偿策略在上方统一保存。</p>
        </div>
        <Button onClick={() => { setCreateForm(defaultPoolForm()); setCreateOpen(true) }}>
          <Plus className="mr-2 h-4 w-4" />
          添加外部池
        </Button>
      </div>

      <div className="grid gap-4">
        {pools.data?.pools.map((pool) => {
          const runtime = statusMap.get(pool.id)
          return (
            <Card key={pool.id}>
              <CardContent className="space-y-4 p-5">
                <div className="flex flex-col gap-4 lg:flex-row lg:items-center lg:justify-between">
                  <div className="space-y-2">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-semibold">#{pool.id} {pool.name}</span>
                    <Badge variant={pool.enabled ? 'default' : 'secondary'}>{pool.enabled ? '启用' : '停用'}</Badge>
                    {pool.autoDisabled && <Badge variant="destructive">自动禁用</Badge>}
                    <Badge variant={runtime?.dispatchable ? 'outline' : 'secondary'}>{runtime?.dispatchable ? '可调度' : runtime?.skippedReason || '不可调度'}</Badge>
                  </div>
                  <div className="text-sm text-muted-foreground">{pool.baseUrl} · {pool.maskedApiKey || '未显示 Key'} · 并发 {runtime?.inFlight ?? 0}/{pool.maxConcurrentRequests} · 优先级 {pool.priority}</div>
                  <div className="text-xs text-muted-foreground">{poolUsageSummary(pool, configDraft)} · auth: {authLabel(pool.authType)} · model: {poolModelMappingSummary(pool)} · request: /v1/messages {runtime?.cooldownRemainingSecs ? `· 冷却 ${runtime.cooldownRemainingSecs}s` : ''}</div>
                  {pool.autoDisabledLastError && <div className="text-xs text-destructive">{pool.autoDisabledLastError}</div>}
                  </div>
                  <div className="flex flex-wrap gap-2">
                  <Button variant="outline" size="sm" onClick={() => startEdit(pool)}>
                    <Pencil className="mr-2 h-4 w-4" />编辑
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => setTestingPool(pool)}>
                    <FlaskConical className="mr-2 h-4 w-4" />测试
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => mutatePool(() => setExternalPoolEnabled(pool.id, !pool.enabled), pool.enabled ? '已停用' : '已启用')}>
                    <Power className="mr-2 h-4 w-4" />{pool.enabled ? '停用' : '启用'}
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => mutatePool(() => clearExternalPoolAutoDisabled(pool.id), '自动禁用状态已清除')}>
                    <RotateCcw className="mr-2 h-4 w-4" />清除禁用
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => status.refetch()}>
                    <RefreshCw className="mr-2 h-4 w-4" />刷新
                  </Button>
                  <Button variant="destructive" size="sm" onClick={() => confirm(`删除外部池 ${pool.name}？`) && mutatePool(() => deleteExternalPool(pool.id), '外部池已删除')}>
                    <Trash2 className="mr-2 h-4 w-4" />删除
                  </Button>
                  </div>
                </div>
              </CardContent>
            </Card>
          )
        })}
        {!pools.isLoading && !pools.data?.pools.length && (
          <Card><CardContent className="p-8 text-center text-muted-foreground">暂无外部备用号池</CardContent></Card>
        )}
      </div>
      <ExternalPoolFormDialog
        mode="create"
        open={createOpen}
        draft={createForm}
        saving={savingPool}
        onDraftChange={setCreateForm}
        onOpenChange={(open) => {
          if (savingPool) return
          setCreateOpen(open)
          if (!open) setCreateForm(defaultPoolForm())
        }}
        onSubmit={submitPool}
      />
      <ExternalPoolFormDialog
        mode="edit"
        pool={editingPool}
        open={Boolean(editingPool)}
        draft={editForm}
        saving={savingPool}
        onDraftChange={setEditForm}
        onOpenChange={(open) => {
          if (savingPool) return
          if (!open) {
            setEditingPool(null)
            setEditForm(defaultPoolForm())
          }
        }}
        onSubmit={savePoolEdit}
      />
      <ExternalPoolTestDialog
        pool={testingPool}
        open={Boolean(testingPool)}
        onOpenChange={(open) => {
          if (!open) setTestingPool(null)
        }}
        onDone={invalidate}
      />
    </div>
  )
}

function ExternalPoolFormDialog({
  mode,
  pool,
  open,
  draft,
  saving,
  onDraftChange,
  onOpenChange,
  onSubmit,
}: {
  mode: 'create' | 'edit'
  pool?: ExternalPool | null
  open: boolean
  draft: ExternalPoolFormDraft
  saving: boolean
  onDraftChange: (value: ExternalPoolFormDraft | ((prev: ExternalPoolFormDraft) => ExternalPoolFormDraft)) => void
  onOpenChange: (open: boolean) => void
  onSubmit: () => void
}) {
  const isEdit = mode === 'edit'
  const title = isEdit ? `编辑外部池${pool ? ` #${pool.id}` : ''}` : '添加外部池'
  const keyLabel = isEdit ? '新请求 Key' : '请求 Key'
  const keyDescription = isEdit ? `留空表示不修改当前 Key。当前：${pool?.maskedApiKey || '未显示 Key'}` : '外部池的请求密钥，保存后只显示脱敏值。'
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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[88vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <FormSection title="连接信息" description="系统会使用这里的 Base URL 和 Key 调用外部池自己的 /v1/messages。">
            <div className="grid gap-3 md:grid-cols-2">
              <TextBox label="名称" value={draft.name} disabled={saving} onChange={(name) => onDraftChange((prev) => ({ ...prev, name }))} />
              <SelectBox label="认证方式" value={draft.authType} disabled={saving} onChange={(authType) => onDraftChange((prev) => ({ ...prev, authType: authType as ExternalPoolFormDraft['authType'] }))}>
                <option value="bearer">Authorization: Bearer &lt;key&gt;</option>
                <option value="x_api_key">x-api-key: &lt;key&gt;</option>
              </SelectBox>
              <TextBox className="md:col-span-2" label="Base URL" description="填写到域名或 /v1 均可；不要填写 /cc，外部池请求路径固定为 /v1/messages。" value={draft.baseUrl} disabled={saving} onChange={(baseUrl) => onDraftChange((prev) => ({ ...prev, baseUrl }))} />
              <TextBox className="md:col-span-2" label={keyLabel} description={keyDescription} value={draft.apiKey} disabled={saving} onChange={(apiKey) => onDraftChange((prev) => ({ ...prev, apiKey }))} />
            </div>
          </FormSection>

          <div className="grid gap-4 lg:grid-cols-2">
            <FormSection title="调度设置" description="这些设置只影响当前外部池，不改变备用池全局排队和冷却策略。">
              <div className="grid gap-3 sm:grid-cols-2">
                <NumberBox label="单池最大并发" description="当前外部池同时处理的最大请求数。" value={draft.maxConcurrentRequests} min={1} disabled={saving} onChange={(maxConcurrentRequests) => onDraftChange((prev) => ({ ...prev, maxConcurrentRequests }))} />
                <NumberBox label="优先级" description="数字越小越靠前；同优先级再按容量和状态分配。" value={draft.priority} disabled={saving} onChange={(priority) => onDraftChange((prev) => ({ ...prev, priority }))} />
                <Toggle label={isEdit ? '启用外部池' : '创建后立即启用'} checked={Boolean(draft.enabled)} disabled={saving} onChange={(enabled) => onDraftChange((prev) => ({ ...prev, enabled }))} />
                <Toggle label="未命中时点号转横杠" checked={Boolean(draft.normalizeModelVersionDots)} disabled={saving || draft.modelMappingMode === 'passthrough' || draft.modelMappingRequireMatch} onChange={(normalizeModelVersionDots) => onDraftChange((prev) => ({ ...prev, normalizeModelVersionDots }))} />
              </div>
            </FormSection>

            <FormSection title="Usage 与成本" description="只控制当前外部池返回给下游的 usage 口径。">
              <div className="space-y-3">
                <SelectBox label="Usage 上报模式" value={draft.usageProjectionMode} disabled={saving} onChange={(usageProjectionMode) => onDraftChange((prev) => ({ ...prev, usageProjectionMode: usageProjectionMode as ExternalPoolFormDraft['usageProjectionMode'] }))}>
                  <option value="pass_through">严格透传：不改外部池 usage</option>
                  <option value="current_path_policy">按当前路径整形：重写 usage 并应用全局补偿</option>
                </SelectBox>
                <Toggle
                  label="同步请求不整形"
                  checked={Boolean(draft.skipNonStreamUsageProjection)}
                  disabled={saving || draft.usageProjectionMode !== 'current_path_policy'}
                  onChange={(skipNonStreamUsageProjection) => onDraftChange((prev) => ({ ...prev, skipNonStreamUsageProjection }))}
                />
                <HintBox>{usageProjectionDescription(draft.usageProjectionMode)}</HintBox>
              </div>
            </FormSection>
          </div>

          <FormSection title="模型处理" description="控制当前外部池出站 model 字段的处理顺序和未命中策略。">
            <div className="grid gap-3 md:grid-cols-[240px_1fr]">
              <div className="space-y-3">
                <SelectBox label="映射模式" value={draft.modelMappingMode} disabled={saving} onChange={(modelMappingMode) => onDraftChange((prev) => ({ ...prev, modelMappingMode: modelMappingMode as ExternalPoolFormDraft['modelMappingMode'] }))}>
                  <option value="passthrough">直接透传请求模型</option>
                  <option value="passthrough_mapping">透传模型优先映射</option>
                  <option value="direct_mapping">映射后内部处理</option>
                  <option value="processed_mapping">内部处理后映射</option>
                </SelectBox>
                <HintBox>{modelMappingDescription(draft.modelMappingMode, draft.normalizeModelVersionDots)}</HintBox>
                {draft.modelMappingMode !== 'passthrough' && (
                  <Toggle label="必须命中映射" checked={Boolean(draft.modelMappingRequireMatch)} disabled={saving} onChange={(modelMappingRequireMatch) => onDraftChange((prev) => ({ ...prev, modelMappingRequireMatch }))} />
                )}
              </div>
              {draft.modelMappingMode !== 'passthrough' && (
                <div className="space-y-3">
                  <TextArea
                    label="映射规则"
                    description="每行一条：claude-sonnet-4-5-20250929 -> claude-sonnet-4.5"
                    value={draft.modelMappingRulesText}
                    disabled={saving}
                    action={<Button type="button" variant="outline" size="sm" onClick={addAllMappingPresets} disabled={saving || mappingPresets.length === 0}>全部添加</Button>}
                    onChange={(modelMappingRulesText) => onDraftChange((prev) => ({ ...prev, modelMappingRulesText }))}
                  />
                  <ModelMappingPresetTags presets={mappingPresets} disabled={saving} onSelect={addMappingPreset} />
                  <TextArea
                    label="快捷导入"
                    description="粘贴多行 source -> target，点击解析导入后追加到上方规则。"
                    value={quickImportText}
                    disabled={saving}
                    action={<Button type="button" variant="outline" size="sm" onClick={importMappingRules} disabled={saving || !quickImportText.trim()}>解析导入</Button>}
                    onChange={setQuickImportText}
                  />
                </div>
              )}
            </div>
          </FormSection>

          <FormSection title="错误处理和备注" description="自动禁用策略只决定当前外部池是否继承全局自动禁用规则。">
            <div className="grid gap-3 md:grid-cols-2">
              <SelectBox label="自动禁用策略" value={draft.autoDisablePolicy} disabled={saving} onChange={(autoDisablePolicy) => onDraftChange((prev) => ({ ...prev, autoDisablePolicy: autoDisablePolicy as ExternalPoolFormDraft['autoDisablePolicy'] }))}>
                <option value="inherit">继承全局自动禁用</option>
                <option value="enabled">单独启用自动禁用</option>
                <option value="disabled">关闭自动禁用</option>
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
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={saving}>
            取消
          </Button>
          <Button onClick={onSubmit} disabled={saving}>
            {saving ? <Loader2 className="mr-2 h-4 w-4 animate-spin" /> : isEdit ? <Save className="mr-2 h-4 w-4" /> : <Plus className="mr-2 h-4 w-4" />}
            {isEdit ? '保存外部池' : '添加外部池'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function ExternalPoolTestDialog({
  pool,
  open,
  onOpenChange,
  onDone,
}: {
  pool: ExternalPool | null
  open: boolean
  onOpenChange: (open: boolean) => void
  onDone: () => void
}) {
  const modelCapabilities = useModelCapabilities()
  const [model, setModel] = useState(DEFAULT_TEST_MODEL)
  const [prompt, setPrompt] = useState(DEFAULT_TEST_PROMPT)
  const [result, setResult] = useState<ExternalPoolTestResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
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
    setError(null)
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
    setError(null)
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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>测试外部备用池</DialogTitle>
        </DialogHeader>
        {pool && (
          <div className="space-y-4">
            <div className="flex items-center justify-between gap-3 rounded-lg border bg-muted/30 p-4">
              <div className="flex min-w-0 items-center gap-3">
                <div className="flex h-12 w-12 shrink-0 items-center justify-center rounded-lg bg-teal-600 text-white">
                  <Play className="h-6 w-6" />
                </div>
                <div className="min-w-0">
                  <div className="truncate text-lg font-semibold">#{pool.id} {pool.name}</div>
                  <div className="mt-1 flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
                    <Badge variant="secondary">{pool.authType}</Badge>
                    <span className="break-all">{pool.baseUrl}</span>
                  </div>
                </div>
              </div>
              <Badge variant={pool.enabled ? 'success' : 'secondary'}>
                {pool.enabled ? 'active' : 'disabled'}
              </Badge>
            </div>

            <div className="grid gap-3 sm:grid-cols-[1fr_220px]">
              <label className="space-y-2">
                <span className="text-sm font-medium">选择测试模型</span>
                <select
                  value={model}
                  onChange={(event) => setModel(event.target.value)}
                  disabled={running}
                  className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {modelOptions.map((option) => (
                    <option key={option.id} value={option.id}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="space-y-2">
                <span className="text-sm font-medium">测试消息</span>
                <Input value={prompt} disabled={running} onChange={(event) => setPrompt(event.target.value)} />
              </label>
            </div>

            <div className="rounded-lg border bg-slate-950 p-4 font-mono text-sm text-slate-200">
              <div className="space-y-1">
                <div><span className="text-blue-400">外部池：</span><span className="text-blue-300"> #{pool.id} {pool.name}</span></div>
                <div><span className="text-cyan-300">使用模型：</span><span className="text-cyan-200"> {model}</span></div>
                <div><span className="text-slate-400">发送测试消息：</span><span className="text-slate-300"> "{prompt.trim() || DEFAULT_TEST_PROMPT}"</span></div>
              </div>
              <div className="mt-4 border-t border-slate-700 pt-4">
                {running && (
                  <div className="flex items-center gap-2 text-blue-300">
                    <Loader2 className="h-4 w-4 animate-spin" />
                    正在等待外部池模型响应...
                  </div>
                )}
                {result && (
                  <div className={result.ok ? 'space-y-3 text-emerald-200' : 'space-y-3 text-red-200'}>
                    <div>
                      {result.ok ? <CheckCircle2 className="mr-2 inline h-4 w-4" /> : <XCircle className="mr-2 inline h-4 w-4" />}
                      {result.message}
                    </div>
                    <div className="text-slate-400">HTTP 状态：{result.status ?? '-'}</div>
                    {result.model && <div className="text-slate-400">返回模型：{result.model}</div>}
                    {result.response && (
                      <div>
                        <div className="mb-1 text-yellow-300">响应：</div>
                        <div className="whitespace-pre-wrap break-words">{result.response}</div>
                      </div>
                    )}
                  </div>
                )}
                {error && (
                  <div className="space-y-2 text-red-300">
                    <div><XCircle className="mr-2 inline h-4 w-4" />测试失败</div>
                    <div className="whitespace-pre-wrap break-words text-red-200">{error}</div>
                  </div>
                )}
                {!running && !result && !error && <div className="text-slate-400">等待开始测试</div>}
              </div>
            </div>

            <div className="flex flex-wrap justify-between gap-3 text-sm text-muted-foreground">
              <span>测试模型：{selectedModelLabel}</span>
              <span>提示词："{prompt.trim() || DEFAULT_TEST_PROMPT}"</span>
            </div>
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={running}>
            关闭
          </Button>
          <Button onClick={run} disabled={!pool || running}>
            {running ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : result || error ? (
              <RotateCw className="mr-2 h-4 w-4" />
            ) : (
              <Play className="mr-2 h-4 w-4" />
            )}
            {result || error ? '重试' : '开始测试'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
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
    <section className={`rounded-lg border p-4 ${active ? 'bg-background' : 'bg-muted/30'}`}>
      <div className="mb-3 flex flex-wrap items-start justify-between gap-3">
        <div>
          <div className="font-medium">{title}</div>
          <p className="mt-1 text-xs text-muted-foreground">{description}</p>
        </div>
        <Badge variant={active ? 'default' : 'secondary'}>{active ? '生效中' : '未生效'}</Badge>
      </div>
      {children}
    </section>
  )
}

function SummaryItem({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="rounded-lg border bg-muted/20 p-3">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-1 text-sm font-semibold">{value}</div>
    </div>
  )
}

function FormSection({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return (
    <section className="rounded-lg border bg-muted/10 p-3">
      <div className="mb-3">
        <div className="text-sm font-medium">{title}</div>
        {description && <p className="mt-1 text-xs leading-5 text-muted-foreground">{description}</p>}
      </div>
      {children}
    </section>
  )
}

function HintBox({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-md border bg-muted/30 px-3 py-2 text-xs leading-5 text-muted-foreground">
      {children}
    </div>
  )
}

function Toggle({ label, checked, onChange, disabled = false }: { label: string; checked: boolean; onChange: (value: boolean) => void; disabled?: boolean }) {
  return (
    <label className={`flex items-center justify-between gap-3 rounded-md border p-3 text-sm ${disabled ? 'cursor-not-allowed bg-muted/40 opacity-60' : ''}`}>
      <span>{label}</span>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onChange} />
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
    <label className={`space-y-1 text-sm ${className} ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <span className="text-muted-foreground">{label}</span>
      <Input value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)} />
      {description && <span className="block text-xs leading-4 text-muted-foreground">{description}</span>}
    </label>
  )
}

function NumberBox({ label, description, value, min = 0, disabled = false, onChange }: { label: string; description?: string; value: number; min?: number; disabled?: boolean; onChange: (value: number) => void }) {
  return (
    <label className={`space-y-1 text-sm ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <span className="text-muted-foreground">{label}</span>
      <Input type="number" min={min} value={value} disabled={disabled} onChange={(event) => onChange(Number(event.target.value))} />
      {description && <span className="block text-xs leading-4 text-muted-foreground">{description}</span>}
    </label>
  )
}

function SelectBox({ label, value, disabled = false, onChange, children }: { label: string; value: string; disabled?: boolean; onChange: (value: string) => void; children: ReactNode }) {
  return (
    <label className={`space-y-1 text-sm ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <span className="text-muted-foreground">{label}</span>
      <select className="h-10 w-full rounded-md border bg-background px-3 text-sm" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)}>
        {children}
      </select>
    </label>
  )
}

function TextArea({
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
    <div className={`space-y-1 text-sm ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <div className="flex items-start justify-between gap-3">
        <div>
          <span className="text-muted-foreground">{label}</span>
          {description && <span className="mt-1 block text-xs leading-4 text-muted-foreground">{description}</span>}
        </div>
        {action}
      </div>
      <textarea className="min-h-24 w-full rounded-md border bg-background px-3 py-2 font-mono text-xs" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)} />
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
  if (mode === 'passthrough') return '直接发送下游请求里的原始模型，不应用映射规则和兜底转换。'
  if (mode === 'passthrough_mapping') return '用下游原始请求模型匹配规则；未命中时仍原样透传请求模型。'
  if (mode === 'direct_mapping') return `用下游原始请求模型匹配规则；${processedFallback}`
  return `先使用本系统解析后的模型匹配规则；${processedFallback}`
}

function poolModelMappingSummary(pool: ExternalPool) {
  if (pool.modelMappingMode === 'passthrough') return '透传'
  const count = pool.modelMappingRules?.length || 0
  const mode = pool.modelMappingMode === 'passthrough_mapping'
    ? '透传+映射'
    : pool.modelMappingMode === 'direct_mapping'
      ? '映射+内部'
      : '内部+映射'
  const fallback = pool.modelMappingRequireMatch ? '必须命中' : pool.normalizeModelVersionDots ? '未命中4.8->4-8' : '允许未命中'
  return `${mode}${count ? ` ${count}条` : ''} · ${fallback}`
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
  if (pool.skipNonStreamUsageProjection) {
    parts.push('同步原样')
  }
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
