import { type ReactNode, useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircle2, FlaskConical, Loader2, Pencil, Play, Plus, Power, RefreshCw, RotateCcw, RotateCw, Save, Trash2, X, XCircle } from 'lucide-react'
import { toast } from 'sonner'
import { Button, Card, Input, Select, Toggle, Textarea } from 'react-daisyui'
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
          localPoolCircuitWindowSecs: whole(configDraft.localPoolCircuitWindowSecs, 1),
          localPoolCircuitOpenAfterFailures: whole(configDraft.localPoolCircuitOpenAfterFailures, 1),
          localPoolCircuitRequireDistinctCredentials: whole(configDraft.localPoolCircuitRequireDistinctCredentials),
          localPoolCircuitOpenSecs: whole(configDraft.localPoolCircuitOpenSecs, 1),
          localPoolCircuitHalfOpenMaxProbes: whole(configDraft.localPoolCircuitHalfOpenMaxProbes, 1),
          externalPoolAutoDisableFailureThreshold: whole(configDraft.externalPoolAutoDisableFailureThreshold, 1),
          externalPoolAutoDisableWindowSecs: whole(configDraft.externalPoolAutoDisableWindowSecs, 1),
          externalPoolAutoDisableDurationSecs: whole(configDraft.externalPoolAutoDisableDurationSecs),
          externalPoolRateLimitCooldownSecs: whole(configDraft.externalPoolRateLimitCooldownSecs, 1),
          externalPoolServerErrorCooldownSecs: whole(configDraft.externalPoolServerErrorCooldownSecs, 1),
          externalPoolNetworkErrorCooldownSecs: whole(configDraft.externalPoolNetworkErrorCooldownSecs, 1),
          externalPoolProtocolErrorCooldownSecs: whole(configDraft.externalPoolProtocolErrorCooldownSecs, 1),
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
  const fallbackActive = externalEnabled
  const directPolicyActive = externalEnabled && configDraft.externalDirectPolicyEnabled
  const autoDisableActive = externalEnabled && configDraft.externalPoolAutoDisableEnabled
  const waitModeActive = externalEnabled && configDraft.externalPoolCapacityMode === 'wait'

  return (
    <div className="space-y-5">
      <SectionCard title="备用号池调度策略" actions={<Button size="sm" color="primary" loading={savingConfig} onClick={saveConfig}><Save className="h-4 w-4" />保存策略</Button>}>
        <div className="space-y-4">
          <PolicyBlock
            title="总开关"
            active={externalEnabled}
            description="关闭后所有备用池调度、直连、fallback、自动禁用配置都不生效，请求只走本地凭证。"
          >
            <div className="grid gap-3 md:grid-cols-2">
              <ToggleRow label="启用备用号池" checked={configDraft.externalPoolsEnabled} onChange={(externalPoolsEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolsEnabled }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="外部池调度参数"
            active={externalEnabled}
            description="只在备用号池启用后生效。容量模式为等待时，会使用外部池独立排队；0 表示对应上限不限制。"
          >
            <div className="grid gap-3 md:grid-cols-4">
              <SelectBox disabled={!externalEnabled} label="满并发处理" value={configDraft.externalPoolCapacityMode} onChange={(externalPoolCapacityMode) => setConfigDraft((prev) => ({ ...prev, externalPoolCapacityMode: externalPoolCapacityMode as ExternalPoolsConfig['externalPoolCapacityMode'] }))}>
                <option value="fail_fast">立即失败</option>
                <option value="wait">等待容量</option>
              </SelectBox>
              <NumberBox disabled={!externalEnabled} label="外部池全局并发" value={configDraft.externalPoolGlobalMaxConcurrentRequests} onChange={(externalPoolGlobalMaxConcurrentRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolGlobalMaxConcurrentRequests }))} />
              <NumberBox disabled={!waitModeActive} label="外部池排队上限（0 不限制）" value={configDraft.externalPoolMaxQueuedRequests} onChange={(externalPoolMaxQueuedRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolMaxQueuedRequests }))} />
              <NumberBox disabled={!waitModeActive} label="最大等待秒数（0 不超时）" value={configDraft.externalPoolDispatchMaxWaitSecs} onChange={(externalPoolDispatchMaxWaitSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolDispatchMaxWaitSecs }))} />
              <NumberBox disabled={!externalEnabled} label="外部池最大重试" value={configDraft.externalPoolRetryMaxAttempts} onChange={(externalPoolRetryMaxAttempts) => setConfigDraft((prev) => ({ ...prev, externalPoolRetryMaxAttempts }))} />
              <NumberBox disabled={!externalEnabled} label="429 冷却秒数" value={configDraft.externalPoolRateLimitCooldownSecs} min={1} onChange={(externalPoolRateLimitCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolRateLimitCooldownSecs }))} />
              <NumberBox disabled={!externalEnabled} label="5xx 冷却秒数" value={configDraft.externalPoolServerErrorCooldownSecs} min={1} onChange={(externalPoolServerErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolServerErrorCooldownSecs }))} />
              <NumberBox disabled={!externalEnabled} label="网络错误冷却秒数" value={configDraft.externalPoolNetworkErrorCooldownSecs} min={1} onChange={(externalPoolNetworkErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolNetworkErrorCooldownSecs }))} />
              <NumberBox disabled={!externalEnabled} label="协议/认证冷却秒数" value={configDraft.externalPoolProtocolErrorCooldownSecs} min={1} onChange={(externalPoolProtocolErrorCooldownSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolProtocolErrorCooldownSecs }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="本地优先 fallback"
            active={fallbackActive}
            description="备用号池启用后才生效；正常情况下先调度本地凭证，只有本地容量或可用性不足时才转到外部池。"
          >
            <div className="grid gap-3 md:grid-cols-4">
              <ToggleRow disabled={!externalEnabled} label="本地预检 fail-fast" checked={configDraft.localPoolPreflightEnabled} onChange={(localPoolPreflightEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolPreflightEnabled }))} />
              <ToggleRow disabled={!externalEnabled} label="容量不足 fallback" checked={configDraft.fallbackOnLocalCapacityExhausted} onChange={(fallbackOnLocalCapacityExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalCapacityExhausted }))} />
              <ToggleRow disabled={!externalEnabled} label="无可用凭据 fallback" checked={configDraft.fallbackOnNoAvailableCredentials} onChange={(fallbackOnNoAvailableCredentials) => setConfigDraft((prev) => ({ ...prev, fallbackOnNoAvailableCredentials }))} />
              <ToggleRow disabled={!externalEnabled} label="瞬态耗尽 fallback" checked={configDraft.fallbackOnLocalTransientExhausted} onChange={(fallbackOnLocalTransientExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalTransientExhausted }))} />
              <ToggleRow disabled={!externalEnabled} label="不支持模型 fallback" checked={configDraft.fallbackOnUnsupportedModel} onChange={(fallbackOnUnsupportedModel) => setConfigDraft((prev) => ({ ...prev, fallbackOnUnsupportedModel }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="显式直连策略"
            active={directPolicyActive}
            description="这是主动绕过本地凭证的策略；关闭后维护态直连、模型规则和路径规则都不生效。"
          >
            <div className="grid gap-3 md:grid-cols-2">
              <ToggleRow disabled={!externalEnabled} label="显式直连策略" checked={configDraft.externalDirectPolicyEnabled} onChange={(externalDirectPolicyEnabled) => setConfigDraft((prev) => ({ ...prev, externalDirectPolicyEnabled }))} />
              <ToggleRow disabled={!directPolicyActive} label="本地维护直连外部池" checked={configDraft.directExternalOnLocalMaintenance} onChange={(directExternalOnLocalMaintenance) => setConfigDraft((prev) => ({ ...prev, directExternalOnLocalMaintenance }))} />
            </div>
            <div className="mt-3 grid gap-3 md:grid-cols-2">
              <Textarea bordered disabled={!directPolicyActive} className="min-h-24" placeholder="直连模型规则，每行一条" value={modelRulesText} onChange={(event) => setModelRulesText(event.target.value)} />
              <Textarea bordered disabled={!directPolicyActive} className="min-h-24" placeholder="直连路径规则，每行一条" value={pathRulesText} onChange={(event) => setPathRulesText(event.target.value)} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="自动禁用外部池"
            active={autoDisableActive}
            description="只控制外部池自身的自动禁用。单个外部池可在列表里继承、强制启用或禁用该全局策略。"
          >
            <div className="grid gap-3 md:grid-cols-4">
              <ToggleRow disabled={!externalEnabled} label="自动禁用外部池" checked={configDraft.externalPoolAutoDisableEnabled} onChange={(externalPoolAutoDisableEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableEnabled }))} />
              <ToggleRow disabled={!autoDisableActive} label="认证错误自动禁用" checked={configDraft.externalPoolAutoDisableOnAuthError} onChange={(externalPoolAutoDisableOnAuthError) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnAuthError }))} />
              <ToggleRow disabled={!autoDisableActive} label="安全锁定自动禁用" checked={configDraft.externalPoolAutoDisableOnSecurityLock} onChange={(externalPoolAutoDisableOnSecurityLock) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnSecurityLock }))} />
              <ToggleRow disabled={!autoDisableActive} label="额度耗尽自动禁用" checked={configDraft.externalPoolAutoDisableOnQuotaExhausted} onChange={(externalPoolAutoDisableOnQuotaExhausted) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnQuotaExhausted }))} />
              <ToggleRow disabled={!autoDisableActive} label="配置错误自动禁用" checked={configDraft.externalPoolAutoDisableOnMisconfiguredEndpoint} onChange={(externalPoolAutoDisableOnMisconfiguredEndpoint) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnMisconfiguredEndpoint }))} />
              <NumberBox disabled={!autoDisableActive} label="自动禁用阈值" value={configDraft.externalPoolAutoDisableFailureThreshold} min={1} onChange={(externalPoolAutoDisableFailureThreshold) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableFailureThreshold }))} />
              <NumberBox disabled={!autoDisableActive} label="自动禁用统计窗口秒数" value={configDraft.externalPoolAutoDisableWindowSecs} min={1} onChange={(externalPoolAutoDisableWindowSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableWindowSecs }))} />
              <NumberBox disabled={!autoDisableActive} label="自动禁用秒数（0 手动恢复）" value={configDraft.externalPoolAutoDisableDurationSecs} onChange={(externalPoolAutoDisableDurationSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableDurationSecs }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="保留字段"
            active={false}
            description="当前版本不会使用这些字段。页面保留展示但禁止编辑，避免误以为会影响调度。"
          >
            <div className="grid gap-3 md:grid-cols-4">
              <ToggleRow disabled label="本地池熔断字段（保留）" checked={configDraft.localPoolCircuitEnabled} onChange={(localPoolCircuitEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitEnabled }))} />
              <NumberBox disabled label="熔断窗口秒数（保留）" value={configDraft.localPoolCircuitWindowSecs} min={1} onChange={(localPoolCircuitWindowSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitWindowSecs }))} />
              <NumberBox disabled label="熔断失败阈值（保留）" value={configDraft.localPoolCircuitOpenAfterFailures} min={1} onChange={(localPoolCircuitOpenAfterFailures) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenAfterFailures }))} />
              <NumberBox disabled label="熔断账号数（保留）" value={configDraft.localPoolCircuitRequireDistinctCredentials} onChange={(localPoolCircuitRequireDistinctCredentials) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitRequireDistinctCredentials }))} />
              <NumberBox disabled label="熔断开启秒数（保留）" value={configDraft.localPoolCircuitOpenSecs} min={1} onChange={(localPoolCircuitOpenSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenSecs }))} />
              <NumberBox disabled label="半开探测数（保留）" value={configDraft.localPoolCircuitHalfOpenMaxProbes} min={1} onChange={(localPoolCircuitHalfOpenMaxProbes) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitHalfOpenMaxProbes }))} />
            </div>
          </PolicyBlock>
        </div>
      </SectionCard>

      <SectionCard title="添加外部池">
        <div className="grid gap-3 md:grid-cols-6">
          <Input bordered placeholder="名称" value={form.name} onChange={(event) => setForm((prev) => ({ ...prev, name: event.target.value }))} />
          <Input bordered className="md:col-span-2" placeholder="Base URL" value={form.baseUrl} onChange={(event) => setForm((prev) => ({ ...prev, baseUrl: event.target.value }))} />
          <Input bordered placeholder="请求 Key" value={form.apiKey} onChange={(event) => setForm((prev) => ({ ...prev, apiKey: event.target.value }))} />
          <Input bordered type="number" placeholder="单池最大并发" value={form.maxConcurrentRequests} onChange={(event) => setForm((prev) => ({ ...prev, maxConcurrentRequests: Number(event.target.value) }))} />
          <Button color="primary" onClick={submitPool}><Plus className="h-4 w-4" />添加</Button>
          <select className="select select-bordered select-sm" value={form.usageProjectionMode} onChange={(event) => setForm((prev) => ({ ...prev, usageProjectionMode: event.target.value as CreateExternalPoolRequest['usageProjectionMode'] }))}>
            <option value="pass_through">严格透传 usage</option>
            <option value="current_path_policy">按原请求路径整形缓存上报</option>
          </select>
          <select className="select select-bordered select-sm" value={form.authType} onChange={(event) => setForm((prev) => ({ ...prev, authType: event.target.value as CreateExternalPoolRequest['authType'] }))}>
            <option value="bearer">Authorization Bearer</option>
            <option value="x_api_key">x-api-key</option>
          </select>
          <select className="select select-bordered select-sm" value={form.autoDisablePolicy} onChange={(event) => setForm((prev) => ({ ...prev, autoDisablePolicy: event.target.value as CreateExternalPoolRequest['autoDisablePolicy'] }))}>
            <option value="inherit">继承全局自动禁用</option>
            <option value="enabled">强制允许自动禁用</option>
            <option value="disabled">禁用自动禁用</option>
          </select>
          <div className="rounded-box border border-base-300 bg-base-200/60 px-3 py-2 text-xs text-base-content/60">
            备用池请求固定走自身 /v1/messages；原请求路径只用于缓存上报整形
          </div>
          <Input bordered className="md:col-span-3" placeholder="备注" value={form.notes} onChange={(event) => setForm((prev) => ({ ...prev, notes: event.target.value }))} />
        </div>
      </SectionCard>

      <div className="space-y-3">
        {pools.data?.pools.map((pool) => {
          const runtime = statusMap.get(pool.id)
          const editing = editingPoolId === pool.id
          return (
            <SectionCard key={pool.id} title={`#${pool.id} ${pool.name}`}>
              {editing ? (
                <div className="grid gap-3 md:grid-cols-6">
                  <Input bordered placeholder="名称" value={editForm.name || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, name: event.target.value }))} />
                  <Input bordered className="md:col-span-2" placeholder="Base URL" value={editForm.baseUrl || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, baseUrl: event.target.value }))} />
                  <Input bordered placeholder="新 Key（留空不改）" value={editForm.apiKey || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, apiKey: event.target.value }))} />
                  <Input bordered type="number" placeholder="单池最大并发" value={editForm.maxConcurrentRequests ?? 10} onChange={(event) => setEditForm((prev) => ({ ...prev, maxConcurrentRequests: Number(event.target.value) }))} />
                  <Input bordered type="number" placeholder="优先级" value={editForm.priority ?? 100} onChange={(event) => setEditForm((prev) => ({ ...prev, priority: Number(event.target.value) }))} />
                  <select className="select select-bordered select-sm" value={editForm.usageProjectionMode} onChange={(event) => setEditForm((prev) => ({ ...prev, usageProjectionMode: event.target.value as UpdateExternalPoolRequest['usageProjectionMode'] }))}>
                    <option value="pass_through">严格透传 usage</option>
                    <option value="current_path_policy">按原请求路径整形缓存上报</option>
                  </select>
                  <select className="select select-bordered select-sm" value={editForm.authType} onChange={(event) => setEditForm((prev) => ({ ...prev, authType: event.target.value as UpdateExternalPoolRequest['authType'] }))}>
                    <option value="bearer">Authorization Bearer</option>
                    <option value="x_api_key">x-api-key</option>
                  </select>
                  <select className="select select-bordered select-sm" value={editForm.autoDisablePolicy} onChange={(event) => setEditForm((prev) => ({ ...prev, autoDisablePolicy: event.target.value as UpdateExternalPoolRequest['autoDisablePolicy'] }))}>
                    <option value="inherit">继承全局自动禁用</option>
                    <option value="enabled">强制允许自动禁用</option>
                    <option value="disabled">禁用自动禁用</option>
                  </select>
                  <label className="flex items-center gap-2 text-sm"><Toggle checked={Boolean(editForm.enabled)} onChange={(event) => setEditForm((prev) => ({ ...prev, enabled: event.target.checked }))} />启用</label>
                  <div className="rounded-box border border-base-300 bg-base-200/60 px-3 py-2 text-xs text-base-content/60">
                    请求固定走备用池 /v1/messages
                  </div>
                  <Input bordered className="md:col-span-2" placeholder="备注" value={editForm.notes || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, notes: event.target.value }))} />
                  <div className="flex gap-2 md:col-span-6">
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
                  <div className="text-xs text-base-content/50">usage: {pool.usageProjectionMode} · auth: {pool.authType} · request: /v1/messages {runtime?.cooldownRemainingSecs ? `· 冷却 ${runtime.cooldownRemainingSecs}s` : ''}</div>
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

function ToggleRow({ label, checked, disabled = false, onChange }: { label: string; checked: boolean; disabled?: boolean; onChange: (value: boolean) => void }) {
  return (
    <label className={`flex items-center justify-between gap-3 rounded-box border border-base-300 p-3 text-sm ${disabled ? 'cursor-not-allowed bg-base-200 opacity-60' : ''}`}>
      <span>{label}</span>
      <Toggle checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
    </label>
  )
}

function NumberBox({ label, value, min = 0, disabled = false, onChange }: { label: string; value: number; min?: number; disabled?: boolean; onChange: (value: number) => void }) {
  return (
    <label className={`space-y-1 text-sm ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <span className="text-base-content/60">{label}</span>
      <Input bordered type="number" min={min} value={value} disabled={disabled} onChange={(event) => onChange(Number(event.target.value))} />
    </label>
  )
}

function SelectBox({ label, value, disabled = false, onChange, children }: { label: string; value: string; disabled?: boolean; onChange: (value: string) => void; children: ReactNode }) {
  return (
    <label className={`space-y-1 text-sm ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <span className="text-base-content/60">{label}</span>
      <select className="select select-bordered w-full" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)}>
        {children}
      </select>
    </label>
  )
}
