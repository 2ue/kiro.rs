import { type ReactNode, useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { FlaskConical, Pencil, Plus, Power, RefreshCw, RotateCcw, Save, Trash2, X } from 'lucide-react'
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
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import type { CreateExternalPoolRequest, ExternalPool, ExternalPoolsConfig, UpdateExternalPoolRequest } from '@/types/api'
import { defaultExternalPoolsConfig } from '@/components/runtime-config-panel'

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
    preservePath: true,
    notes: '',
  })

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
          externalPoolRetryMaxAttempts: whole(configDraft.externalPoolRetryMaxAttempts),
          localPoolCircuitWindowSecs: whole(configDraft.localPoolCircuitWindowSecs, 1),
          localPoolCircuitOpenAfterFailures: whole(configDraft.localPoolCircuitOpenAfterFailures, 1),
          localPoolCircuitRequireDistinctCredentials: whole(configDraft.localPoolCircuitRequireDistinctCredentials),
          localPoolCircuitOpenSecs: whole(configDraft.localPoolCircuitOpenSecs, 1),
          localPoolCircuitHalfOpenMaxProbes: whole(configDraft.localPoolCircuitHalfOpenMaxProbes, 1),
          externalPoolAutoDisableFailureThreshold: whole(configDraft.externalPoolAutoDisableFailureThreshold, 1),
          externalPoolAutoDisableDurationSecs: whole(configDraft.externalPoolAutoDisableDurationSecs),
          externalPoolRateLimitCooldownSecs: whole(configDraft.externalPoolRateLimitCooldownSecs, 1),
          externalPoolServerErrorCooldownSecs: whole(configDraft.externalPoolServerErrorCooldownSecs, 1),
          externalPoolNetworkErrorCooldownSecs: whole(configDraft.externalPoolNetworkErrorCooldownSecs, 1),
          externalPoolProtocolErrorCooldownSecs: whole(configDraft.externalPoolProtocolErrorCooldownSecs, 1),
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
    if (!form.name.trim() || !form.baseUrl.trim() || !form.apiKey.trim()) {
      toast.error('名称、Base URL 和 Key 必填')
      return
    }
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
    if (!editForm.name?.trim() || !editForm.baseUrl?.trim()) {
      toast.error('名称和 Base URL 必填')
      return
    }
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

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Power className="h-5 w-5" />
            备用号池调度策略
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-5">
          <PolicyBlock
            title="总开关"
            active={externalEnabled}
            description="关闭后所有备用池调度、直连、fallback、自动禁用配置都不生效，请求只走本地凭证。"
          >
            <div className="grid gap-4 md:grid-cols-2">
              <Toggle label="启用备用号池" checked={configDraft.externalPoolsEnabled} onChange={(externalPoolsEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolsEnabled }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="外部池调度参数"
            active={externalEnabled}
            description="只在备用号池启用后生效，用于限制外部池自身的并发、重试和错误冷却。"
          >
            <div className="grid gap-4 md:grid-cols-4">
              <NumberBox disabled={!externalEnabled} label="外部池全局并发" value={configDraft.externalPoolGlobalMaxConcurrentRequests} onChange={(externalPoolGlobalMaxConcurrentRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolGlobalMaxConcurrentRequests }))} />
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
            <div className="grid gap-4 md:grid-cols-4">
              <Toggle disabled={!externalEnabled} label="本地容量预检 fail-fast" checked={configDraft.localPoolPreflightEnabled} onChange={(localPoolPreflightEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolPreflightEnabled }))} />
              <Toggle disabled={!externalEnabled} label="本地容量不足 fallback" checked={configDraft.fallbackOnLocalCapacityExhausted} onChange={(fallbackOnLocalCapacityExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalCapacityExhausted }))} />
              <Toggle disabled={!externalEnabled} label="无可用凭据 fallback" checked={configDraft.fallbackOnNoAvailableCredentials} onChange={(fallbackOnNoAvailableCredentials) => setConfigDraft((prev) => ({ ...prev, fallbackOnNoAvailableCredentials }))} />
              <Toggle disabled={!externalEnabled} label="本地瞬态耗尽 fallback" checked={configDraft.fallbackOnLocalTransientExhausted} onChange={(fallbackOnLocalTransientExhausted) => setConfigDraft((prev) => ({ ...prev, fallbackOnLocalTransientExhausted }))} />
              <Toggle disabled={!externalEnabled} label="不支持模型 fallback" checked={configDraft.fallbackOnUnsupportedModel} onChange={(fallbackOnUnsupportedModel) => setConfigDraft((prev) => ({ ...prev, fallbackOnUnsupportedModel }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="显式直连策略"
            active={directPolicyActive}
            description="这是主动绕过本地凭证的策略；关闭后维护态直连、模型规则和路径规则都不生效。"
          >
            <div className="grid gap-4 md:grid-cols-2">
              <Toggle disabled={!externalEnabled} label="显式直连策略" checked={configDraft.externalDirectPolicyEnabled} onChange={(externalDirectPolicyEnabled) => setConfigDraft((prev) => ({ ...prev, externalDirectPolicyEnabled }))} />
              <Toggle disabled={!directPolicyActive} label="本地维护直连外部池" checked={configDraft.directExternalOnLocalMaintenance} onChange={(directExternalOnLocalMaintenance) => setConfigDraft((prev) => ({ ...prev, directExternalOnLocalMaintenance }))} />
            </div>
            <div className="mt-4 grid gap-4 md:grid-cols-2">
              <TextArea disabled={!directPolicyActive} label="直连模型规则，每行一条" value={modelRulesText} onChange={setModelRulesText} />
              <TextArea disabled={!directPolicyActive} label="直连路径规则，每行一条" value={pathRulesText} onChange={setPathRulesText} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="自动禁用外部池"
            active={autoDisableActive}
            description="只控制外部池自身的自动禁用。单个外部池可在列表里继承、强制启用或禁用该全局策略。"
          >
            <div className="grid gap-4 md:grid-cols-4">
              <Toggle disabled={!externalEnabled} label="自动禁用外部池" checked={configDraft.externalPoolAutoDisableEnabled} onChange={(externalPoolAutoDisableEnabled) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableEnabled }))} />
              <Toggle disabled={!autoDisableActive} label="认证错误自动禁用" checked={configDraft.externalPoolAutoDisableOnAuthError} onChange={(externalPoolAutoDisableOnAuthError) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnAuthError }))} />
              <Toggle disabled={!autoDisableActive} label="安全锁定自动禁用" checked={configDraft.externalPoolAutoDisableOnSecurityLock} onChange={(externalPoolAutoDisableOnSecurityLock) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnSecurityLock }))} />
              <Toggle disabled={!autoDisableActive} label="额度耗尽自动禁用" checked={configDraft.externalPoolAutoDisableOnQuotaExhausted} onChange={(externalPoolAutoDisableOnQuotaExhausted) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnQuotaExhausted }))} />
              <Toggle disabled={!autoDisableActive} label="配置错误自动禁用" checked={configDraft.externalPoolAutoDisableOnMisconfiguredEndpoint} onChange={(externalPoolAutoDisableOnMisconfiguredEndpoint) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableOnMisconfiguredEndpoint }))} />
              <NumberBox disabled={!autoDisableActive} label="自动禁用阈值" value={configDraft.externalPoolAutoDisableFailureThreshold} min={1} onChange={(externalPoolAutoDisableFailureThreshold) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableFailureThreshold }))} />
              <NumberBox disabled={!autoDisableActive} label="自动禁用秒数（0 手动恢复）" value={configDraft.externalPoolAutoDisableDurationSecs} onChange={(externalPoolAutoDisableDurationSecs) => setConfigDraft((prev) => ({ ...prev, externalPoolAutoDisableDurationSecs }))} />
            </div>
          </PolicyBlock>

          <PolicyBlock
            title="保留字段"
            active={false}
            description="当前版本不会使用这些字段。页面保留展示但禁止编辑，避免误以为会影响调度。"
          >
            <div className="grid gap-4 md:grid-cols-4">
              <NumberBox disabled label="外部池排队上限（保留）" value={configDraft.externalPoolMaxQueuedRequests} onChange={(externalPoolMaxQueuedRequests) => setConfigDraft((prev) => ({ ...prev, externalPoolMaxQueuedRequests }))} />
              <Toggle disabled label="本地池熔断字段（保留）" checked={configDraft.localPoolCircuitEnabled} onChange={(localPoolCircuitEnabled) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitEnabled }))} />
              <NumberBox disabled label="熔断窗口秒数（保留）" value={configDraft.localPoolCircuitWindowSecs} min={1} onChange={(localPoolCircuitWindowSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitWindowSecs }))} />
              <NumberBox disabled label="熔断失败阈值（保留）" value={configDraft.localPoolCircuitOpenAfterFailures} min={1} onChange={(localPoolCircuitOpenAfterFailures) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenAfterFailures }))} />
              <NumberBox disabled label="熔断账号数（保留）" value={configDraft.localPoolCircuitRequireDistinctCredentials} onChange={(localPoolCircuitRequireDistinctCredentials) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitRequireDistinctCredentials }))} />
              <NumberBox disabled label="熔断开启秒数（保留）" value={configDraft.localPoolCircuitOpenSecs} min={1} onChange={(localPoolCircuitOpenSecs) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitOpenSecs }))} />
              <NumberBox disabled label="半开探测数（保留）" value={configDraft.localPoolCircuitHalfOpenMaxProbes} min={1} onChange={(localPoolCircuitHalfOpenMaxProbes) => setConfigDraft((prev) => ({ ...prev, localPoolCircuitHalfOpenMaxProbes }))} />
            </div>
          </PolicyBlock>
          <Button onClick={saveConfig} disabled={savingConfig || runtimeConfig.isLoading}>
            <Save className="mr-2 h-4 w-4" />
            保存策略
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Plus className="h-5 w-5" />
            添加外部池
          </CardTitle>
        </CardHeader>
        <CardContent className="grid gap-3 md:grid-cols-6">
          <Input placeholder="名称" value={form.name} onChange={(event) => setForm((prev) => ({ ...prev, name: event.target.value }))} />
          <Input className="md:col-span-2" placeholder="Base URL，例如 https://pool.example.com" value={form.baseUrl} onChange={(event) => setForm((prev) => ({ ...prev, baseUrl: event.target.value }))} />
          <Input placeholder="请求 Key" value={form.apiKey} onChange={(event) => setForm((prev) => ({ ...prev, apiKey: event.target.value }))} />
          <Input type="number" placeholder="并发" value={form.maxConcurrentRequests} onChange={(event) => setForm((prev) => ({ ...prev, maxConcurrentRequests: Number(event.target.value) }))} />
          <Button onClick={submitPool}>
            <Plus className="mr-2 h-4 w-4" />
            添加
          </Button>
          <select className="h-10 rounded-md border bg-background px-3 text-sm" value={form.usageProjectionMode} onChange={(event) => setForm((prev) => ({ ...prev, usageProjectionMode: event.target.value as CreateExternalPoolRequest['usageProjectionMode'] }))}>
            <option value="pass_through">严格透传 usage</option>
            <option value="current_path_policy">按当前路径整形 usage</option>
          </select>
          <select className="h-10 rounded-md border bg-background px-3 text-sm" value={form.authType} onChange={(event) => setForm((prev) => ({ ...prev, authType: event.target.value as CreateExternalPoolRequest['authType'] }))}>
            <option value="bearer">Authorization Bearer</option>
            <option value="x_api_key">x-api-key</option>
          </select>
          <select className="h-10 rounded-md border bg-background px-3 text-sm" value={form.autoDisablePolicy} onChange={(event) => setForm((prev) => ({ ...prev, autoDisablePolicy: event.target.value as CreateExternalPoolRequest['autoDisablePolicy'] }))}>
            <option value="inherit">继承全局自动禁用</option>
            <option value="enabled">强制允许自动禁用</option>
            <option value="disabled">禁用自动禁用</option>
          </select>
          <label className="flex items-center gap-2 text-sm">
            <Switch checked={Boolean(form.preservePath)} onCheckedChange={(preservePath) => setForm((prev) => ({ ...prev, preservePath }))} />
            保留请求路径
          </label>
          <Input className="md:col-span-2" placeholder="备注" value={form.notes} onChange={(event) => setForm((prev) => ({ ...prev, notes: event.target.value }))} />
        </CardContent>
      </Card>

      <div className="grid gap-4">
        {pools.data?.pools.map((pool) => {
          const runtime = statusMap.get(pool.id)
          const editing = editingPoolId === pool.id
          return (
            <Card key={pool.id}>
              <CardContent className="space-y-4 p-5">
                {editing ? (
                  <div className="grid gap-3 md:grid-cols-6">
                    <Input placeholder="名称" value={editForm.name || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, name: event.target.value }))} />
                    <Input className="md:col-span-2" placeholder="Base URL" value={editForm.baseUrl || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, baseUrl: event.target.value }))} />
                    <Input placeholder="新 Key（留空不改）" value={editForm.apiKey || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, apiKey: event.target.value }))} />
                    <Input type="number" placeholder="并发" value={editForm.maxConcurrentRequests ?? 10} onChange={(event) => setEditForm((prev) => ({ ...prev, maxConcurrentRequests: Number(event.target.value) }))} />
                    <Input type="number" placeholder="优先级" value={editForm.priority ?? 100} onChange={(event) => setEditForm((prev) => ({ ...prev, priority: Number(event.target.value) }))} />
                    <select className="h-10 rounded-md border bg-background px-3 text-sm" value={editForm.usageProjectionMode} onChange={(event) => setEditForm((prev) => ({ ...prev, usageProjectionMode: event.target.value as UpdateExternalPoolRequest['usageProjectionMode'] }))}>
                      <option value="pass_through">严格透传 usage</option>
                      <option value="current_path_policy">按当前路径整形 usage</option>
                    </select>
                    <select className="h-10 rounded-md border bg-background px-3 text-sm" value={editForm.authType} onChange={(event) => setEditForm((prev) => ({ ...prev, authType: event.target.value as UpdateExternalPoolRequest['authType'] }))}>
                      <option value="bearer">Authorization Bearer</option>
                      <option value="x_api_key">x-api-key</option>
                    </select>
                    <select className="h-10 rounded-md border bg-background px-3 text-sm" value={editForm.autoDisablePolicy} onChange={(event) => setEditForm((prev) => ({ ...prev, autoDisablePolicy: event.target.value as UpdateExternalPoolRequest['autoDisablePolicy'] }))}>
                      <option value="inherit">继承全局自动禁用</option>
                      <option value="enabled">强制允许自动禁用</option>
                      <option value="disabled">禁用自动禁用</option>
                    </select>
                    <label className="flex items-center gap-2 text-sm">
                      <Switch checked={Boolean(editForm.enabled)} onCheckedChange={(enabled) => setEditForm((prev) => ({ ...prev, enabled }))} />
                      启用
                    </label>
                    <label className="flex items-center gap-2 text-sm">
                      <Switch checked={Boolean(editForm.preservePath)} onCheckedChange={(preservePath) => setEditForm((prev) => ({ ...prev, preservePath }))} />
                      保留路径
                    </label>
                    <Input className="md:col-span-2" placeholder="备注" value={editForm.notes || ''} onChange={(event) => setEditForm((prev) => ({ ...prev, notes: event.target.value }))} />
                    <div className="flex gap-2 md:col-span-6">
                      <Button size="sm" onClick={savePoolEdit}><Save className="mr-2 h-4 w-4" />保存</Button>
                      <Button variant="outline" size="sm" onClick={() => { setEditingPoolId(null); setEditForm({}) }}><X className="mr-2 h-4 w-4" />取消</Button>
                    </div>
                  </div>
                ) : (
                <div className="flex flex-col gap-4 lg:flex-row lg:items-center lg:justify-between">
                  <div className="space-y-2">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-semibold">#{pool.id} {pool.name}</span>
                    <Badge variant={pool.enabled ? 'default' : 'secondary'}>{pool.enabled ? '启用' : '停用'}</Badge>
                    {pool.autoDisabled && <Badge variant="destructive">自动禁用</Badge>}
                    <Badge variant={runtime?.dispatchable ? 'outline' : 'secondary'}>{runtime?.dispatchable ? '可调度' : runtime?.skippedReason || '不可调度'}</Badge>
                  </div>
                  <div className="text-sm text-muted-foreground">{pool.baseUrl} · {pool.maskedApiKey || '未显示 Key'} · 并发 {runtime?.inFlight ?? 0}/{pool.maxConcurrentRequests} · 优先级 {pool.priority}</div>
                  <div className="text-xs text-muted-foreground">usage: {pool.usageProjectionMode} · auth: {pool.authType} · path: {pool.preservePath ? '保留' : '转 /v1/messages'} {runtime?.cooldownRemainingSecs ? `· 冷却 ${runtime.cooldownRemainingSecs}s` : ''}</div>
                  {pool.autoDisabledLastError && <div className="text-xs text-destructive">{pool.autoDisabledLastError}</div>}
                  </div>
                  <div className="flex flex-wrap gap-2">
                  <Button variant="outline" size="sm" onClick={() => startEdit(pool)}>
                    <Pencil className="mr-2 h-4 w-4" />编辑
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => mutatePool(() => testExternalPool(pool.id), '外部池测试完成')}>
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
                )}
              </CardContent>
            </Card>
          )
        })}
        {!pools.isLoading && !pools.data?.pools.length && (
          <Card><CardContent className="p-8 text-center text-muted-foreground">暂无外部备用号池</CardContent></Card>
        )}
      </div>
    </div>
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

function Toggle({ label, checked, onChange, disabled = false }: { label: string; checked: boolean; onChange: (value: boolean) => void; disabled?: boolean }) {
  return (
    <label className={`flex items-center justify-between gap-3 rounded-md border p-3 text-sm ${disabled ? 'cursor-not-allowed bg-muted/40 opacity-60' : ''}`}>
      <span>{label}</span>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onChange} />
    </label>
  )
}

function NumberBox({ label, value, min = 0, disabled = false, onChange }: { label: string; value: number; min?: number; disabled?: boolean; onChange: (value: number) => void }) {
  return (
    <label className={`space-y-1 text-sm ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <span className="text-muted-foreground">{label}</span>
      <Input type="number" min={min} value={value} disabled={disabled} onChange={(event) => onChange(Number(event.target.value))} />
    </label>
  )
}

function TextArea({ label, value, disabled = false, onChange }: { label: string; value: string; disabled?: boolean; onChange: (value: string) => void }) {
  return (
    <label className={`space-y-1 text-sm ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}>
      <span className="text-muted-foreground">{label}</span>
      <textarea className="min-h-24 w-full rounded-md border bg-background px-3 py-2 text-sm" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)} />
    </label>
  )
}
