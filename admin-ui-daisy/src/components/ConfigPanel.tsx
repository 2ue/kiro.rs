import { BadgeInfo, Copy, Edit3, Eye, EyeOff, Gauge, KeyRound, Plus, Router, Save, Shield, Sparkles, Trash2, Wand2, X, Zap } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Alert, Button, Card, Collapse, Input, Join, Loading, Select, Tabs, Toggle } from 'react-daisyui'
import { ErrorState, FieldLabel, SectionCard } from '@/components/common'
import {
  defaultModelMappingConfig,
  defaultPayloadShaping,
  defaultExternalPoolsConfig,
  defaultPromptCacheCreationControl,
  emptyRuntimeConfig,
  fieldNeedsMax,
  fieldNeedsTarget,
  normalizePayloadShaping,
  normalizePromptCacheCreationControl,
  normalizeReportedUsage,
  pathPolicy,
  reportedUsageModeDescription,
  toRatio,
  toScale,
  toWhole,
} from '@/lib/runtime-config-defaults'
import { extractErrorMessage } from '@/lib/utils'
import { createRequestApiKey, deleteRequestApiKey, getAccessKeys, updateAdminApiKey, updateRequestApiKey } from '@/api/credentials'
import { useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import { storage } from '@/lib/storage'
import type {
  AccessKeysResponse,
  CompatProfile,
  KiroAgentModeStrategy,
  ModelCapabilitiesStatus,
  ModelMappingConfig,
  ModelMappingRule,
  ModelResolutionMode,
  PayloadGuardMode,
  ReportedUsageFieldMode,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RequestApiKeyItem,
  RuntimeConfig,
} from '@/types/api'

type ConfigTab = 'dispatch' | 'cache' | 'usage' | 'compat'

const configTabs: Array<{ key: ConfigTab; label: string; description: string }> = [
  { key: 'dispatch', label: '调度', description: '限速、冷却、并发、预热、请求压缩' },
  { key: 'cache', label: '高缓存', description: '缓存模拟比例、放大、触顶扣减' },
  { key: 'usage', label: '路径上报', description: '按路径改写 input、output、cache read/write' },
  { key: 'compat', label: '兼容诊断', description: '协议兼容、调试头、后台统计' },
]

function normalizeModelMapping(config?: Partial<ModelMappingConfig> | null): ModelMappingConfig {
  return {
    ...defaultModelMappingConfig(),
    ...(config || {}),
    rules: (config?.rules || [])
      .map((rule) => ({
        enabled: rule.enabled !== false,
        source: rule.source.trim().toLowerCase(),
        target: rule.target.trim().toLowerCase(),
        kind: rule.kind || 'alias',
        note: rule.note?.trim() || null,
      }))
      .filter((rule) => rule.source && rule.target),
  }
}

function versionEquivalentSource(model: string): string | null {
  const match = model.match(/^claude-(opus|sonnet|haiku)-(\d+)([.-])(\d{1,3})(-\d{6,})?(-thinking)?$/)
  if (!match) return null
  const [, family, major, separator, minor, , thinking = ''] = match
  return separator === '.'
    ? `claude-${family}-${major}-${minor}${thinking}`
    : `claude-${family}-${major}.${minor}${thinking}`
}

function modelVersionNumbers(model: string): number[] {
  return (model.match(/\d+/g) || []).map((part) => Number(part))
}

function compareModelId(a: string, b: string): number {
  const av = modelVersionNumbers(a)
  const bv = modelVersionNumbers(b)
  const len = Math.max(av.length, bv.length)
  for (let index = 0; index < len; index += 1) {
    const delta = (av[index] || 0) - (bv[index] || 0)
    if (delta !== 0) return delta
  }
  if (a.endsWith('-thinking') !== b.endsWith('-thinking')) return a.endsWith('-thinking') ? -1 : 1
  return a.localeCompare(b)
}

function addModelRule(rules: ModelMappingRule[], rule: ModelMappingRule) {
  const source = rule.source.trim().toLowerCase()
  const target = rule.target.trim().toLowerCase()
  if (!source || !target || source === target) return
  if (rules.some((item) => item.source === source && item.target === target && item.kind === rule.kind)) return
  rules.push({ ...rule, source, target, enabled: rule.enabled !== false })
}

function generateDefaultModelMappingRules(status?: ModelCapabilitiesStatus): ModelMappingRule[] {
  const models = (status?.models || []).map((item) => item.model.trim().toLowerCase()).filter(Boolean)
  const rules: ModelMappingRule[] = []
  for (const model of models) {
    const source = versionEquivalentSource(model)
    if (source) {
      addModelRule(rules, {
        enabled: true,
        source,
        target: model,
        kind: 'version_equivalent',
        note: '由当前上游模型列表生成的 dash/dot 小版本等价映射',
      })
    }
  }
  const pickFamily = (family: 'opus' | 'sonnet' | 'haiku') => {
    const sorted = models
      .filter((model) => model === family || model.startsWith(`claude-${family}`))
      .sort(compareModelId)
    return sorted[sorted.length - 1]
  }
  const opus = pickFamily('opus')
  const sonnet = pickFamily('sonnet')
  const haiku = pickFamily('haiku')
  for (const source of ['opus', 'opusplan', 'best', 'default', 'auto']) {
    if (opus) addModelRule(rules, { enabled: true, source, target: opus, kind: 'alias', note: '由当前上游 Opus 模型生成的默认别名' })
  }
  if (sonnet) addModelRule(rules, { enabled: true, source: 'sonnet', target: sonnet, kind: 'alias', note: '由当前上游 Sonnet 模型生成的默认别名' })
  if (haiku) addModelRule(rules, { enabled: true, source: 'haiku', target: haiku, kind: 'alias', note: '由当前上游 Haiku 模型生成的默认别名' })
  return rules
}

function numberValue(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

function NumberField({
  title,
  description,
  value,
  disabled,
  min,
  max,
  step,
  suffix,
  onChange,
}: {
  title: string
  description: string
  value: number
  disabled?: boolean
  min?: number
  max?: number
  step?: number
  suffix: string
  onChange: (value: number) => void
}) {
  return (
    <FieldLabel title={title} description={description}>
      <Join className="w-full">
        <Input
          bordered
          size="sm"
          type="number"
          className="join-item w-full"
          value={value}
          min={min}
          max={max}
          step={step}
          inputMode={step ? 'decimal' : 'numeric'}
          disabled={disabled}
          onChange={(event) => onChange(numberValue(event.target.value, min ?? 0))}
        />
        <span className="join-item unit-addon min-w-20">{suffix}</span>
      </Join>
    </FieldLabel>
  )
}

function ToggleField({
  title,
  description,
  checked,
  disabled,
  onChange,
}: {
  title: string
  description: string
  checked: boolean
  disabled?: boolean
  onChange: (value: boolean) => void
}) {
  return (
    <Card bordered className="bg-base-100 shadow-none">
      <Card.Body className="flex-row items-center justify-between gap-3 p-3">
      <div className="min-w-0">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</div>
      </div>
      <Toggle color="primary" size="sm" className="shrink-0" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
      </Card.Body>
    </Card>
  )
}

function ConfigGroup({
  icon,
  title,
  description,
  children,
}: {
  icon: React.ReactNode
  title: string
  description: string
  children: React.ReactNode
}) {
  return (
    <Collapse icon="arrow" open className="rounded-box border border-base-300 bg-base-100 shadow-none">
      <Collapse.Title className="flex items-start gap-2.5 px-3 py-2.5">
        <span className="rounded-lg border border-base-300 bg-base-200 p-1.5 text-primary">{icon}</span>
        <span>
          <span className="block text-sm font-semibold">{title}</span>
          <span className="mt-0.5 block text-xs leading-4 text-base-content/60">{description}</span>
        </span>
      </Collapse.Title>
      <Collapse.Content>
        <div className="grid gap-3 border-t border-base-300/70 pt-3 md:grid-cols-2">{children}</div>
      </Collapse.Content>
    </Collapse>
  )
}

function maskSecret(value?: string | null): string {
  if (!value) return '-'
  return '*'.repeat(Math.min(Math.max(value.length, 6), 16))
}

function ReadOnlySecretField({
  label,
  value,
  visible,
  onToggle,
}: {
  label: string
  value?: string | null
  visible: boolean
  onToggle: () => void
}) {
  return (
    <div>
      <div className="mb-2 text-sm font-semibold">{label}</div>
      <div className="flex gap-2">
        <Input
          bordered
          readOnly
          size="sm"
          className="min-w-0 font-mono text-xs"
          value={visible ? value || '-' : maskSecret(value)}
        />
        <Button
          type="button"
          size="sm"
          className="shrink-0"
          onClick={onToggle}
          title={visible ? `隐藏${label}` : `显示${label}`}
        >
          {visible ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
          {visible ? '隐藏' : '显示'}
        </Button>
      </div>
    </div>
  )
}

function StartupProxyPanel({ config }: { config: RuntimeConfig }) {
  const [showProxyUsername, setShowProxyUsername] = useState(false)
  const [showProxyPassword, setShowProxyPassword] = useState(false)
  const hasGlobalProxy = Boolean(config.proxyUrl)

  return (
    <ConfigGroup
      icon={<Router className="h-4 w-4" />}
      title="全局代理（启动期配置，只读）"
      description="这里展示启动配置里的全局代理。它会作为未配置凭据直连代理、也未绑定代理资源时的默认代理；修改需要改启动配置并重启服务。"
    >
      <div className="rounded-box border border-base-300 bg-base-100 p-3 md:col-span-2">
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <span className="text-sm font-semibold">当前状态</span>
          <span className={`rounded border px-2 py-0.5 text-[0.68rem] font-semibold ${hasGlobalProxy ? 'border-base-300 bg-base-100 text-success' : 'border-base-300 bg-base-200 text-base-content/60'}`}>
            {hasGlobalProxy ? '已配置全局代理' : '未配置全局代理'}
          </span>
          <span className="rounded border border-base-300 bg-base-200 px-2 py-0.5 text-[0.68rem] font-semibold text-base-content/60">
            只读
          </span>
        </div>
        <div className="grid gap-3 md:grid-cols-2">
          <div className="md:col-span-2">
            <div className="mb-2 text-sm font-semibold">代理 URL</div>
            <Input bordered readOnly size="sm" className="font-mono text-xs" value={config.proxyUrl || '-'} />
          </div>
          <ReadOnlySecretField
            label="代理用户名"
            value={config.proxyUsername}
            visible={showProxyUsername}
            onToggle={() => setShowProxyUsername((value) => !value)}
          />
          <ReadOnlySecretField
            label="代理密码"
            value={config.proxyPassword}
            visible={showProxyPassword}
            onToggle={() => setShowProxyPassword((value) => !value)}
          />
        </div>
      </div>
    </ConfigGroup>
  )
}

function ImpactGroupHeader({
  label,
  title,
  description,
  muted = false,
}: {
  label: string
  title: string
  description: string
  muted?: boolean
}) {
  return (
    <div className={`rounded-box border px-3 py-2.5 md:col-span-2 ${muted ? 'bg-base-200/70 text-base-content/55' : 'bg-base-100'}`}>
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <span className="rounded border border-base-300 bg-base-200 px-2 py-0.5 text-[0.68rem] font-semibold text-base-content/60">
          {label}
        </span>
        <span className="text-sm font-semibold">{title}</span>
      </div>
      <p className="text-xs leading-5 text-base-content/60">{description}</p>
    </div>
  )
}

function accessKeyItems(response: AccessKeysResponse | null): RequestApiKeyItem[] {
  if (!response) return []
  if (response.requestApiKeys?.length) return response.requestApiKeys
  if (!response.requestApiKey) return []
  return [{ id: 'legacy-primary', apiKey: response.requestApiKey, maskedApiKey: response.maskedRequestApiKey, primary: true }]
}

const REQUEST_API_KEY_PREFIX = 'sk-kiro-rs-'

function generateLocalRequestApiKey(): string {
  const bytes = new Uint8Array(32)
  const cryptoApi = globalThis.crypto
  if (cryptoApi?.getRandomValues) {
    cryptoApi.getRandomValues(bytes)
  } else {
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = Math.floor(Math.random() * 256)
    }
  }
  const binary = Array.from(bytes, (byte) => String.fromCharCode(byte)).join('')
  return `${REQUEST_API_KEY_PREFIX}${btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}`
}

function AccessKeysPanel() {
  const [keys, setKeys] = useState<AccessKeysResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [showAdminKey, setShowAdminKey] = useState(false)
  const [creating, setCreating] = useState(false)
  const [processingKeyId, setProcessingKeyId] = useState<string | null>(null)
  const [manualRequestApiKey, setManualRequestApiKey] = useState('')
  const [visibleRequestKeyIds, setVisibleRequestKeyIds] = useState<Set<string>>(new Set())
  const [editingRequestKeyId, setEditingRequestKeyId] = useState<string | null>(null)
  const [requestKeyDraft, setRequestKeyDraft] = useState('')
  const [nextAdminApiKey, setNextAdminApiKey] = useState('')

  const loadKeys = async () => {
    setLoading(true)
    try {
      setKeys(await getAccessKeys())
    } catch (error) {
      toast.error(`读取访问密钥失败: ${extractErrorMessage(error)}`)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void loadKeys()
  }, [])

  const copy = async (label: string, value?: string) => {
    if (!value) {
      toast.error(`${label} 为空，无法复制`)
      return
    }
    try {
      await navigator.clipboard.writeText(value)
      toast.success(`${label} 已复制`)
    } catch (error) {
      toast.error(`复制 ${label} 失败: ${extractErrorMessage(error)}`)
    }
  }

  const saveAdminApiKey = async () => {
    const adminApiKey = nextAdminApiKey.trim()
    if (!adminApiKey) {
      toast.error('请输入新的登录 Key（adminApiKey）')
      return
    }
    if (adminApiKey.length < 8) {
      toast.error('登录 Key 至少需要 8 个字符')
      return
    }
    setSaving(true)
    try {
      const response = await updateAdminApiKey({ adminApiKey })
      storage.setApiKey(response.adminApiKey)
      window.dispatchEvent(new CustomEvent('kiro-admin-key-updated'))
      setKeysAndResetDrafts(response)
      setNextAdminApiKey('')
      toast.success('登录 Key 已更新，后续后台请求会使用新 Key')
    } catch (error) {
      toast.error(`更新登录 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setSaving(false)
    }
  }

  const requestKeys = accessKeyItems(keys)
  const adminApiKeyValue = showAdminKey ? keys?.adminApiKey : keys?.maskedAdminApiKey

  const setKeysAndResetDrafts = (response: AccessKeysResponse) => {
    setKeys(response)
    setEditingRequestKeyId(null)
    setRequestKeyDraft('')
    setVisibleRequestKeyIds((prev) => {
      const valid = new Set(accessKeyItems(response).map((item) => item.id))
      return new Set(Array.from(prev).filter((id) => valid.has(id)))
    })
  }

  const generateRequestKey = async () => {
    setCreating(true)
    try {
      const before = new Set(requestKeys.map((item) => item.id))
      const response = await createRequestApiKey({})
      setKeysAndResetDrafts(response)
      const created = accessKeyItems(response).find((item) => !before.has(item.id))
      if (created) {
        setVisibleRequestKeyIds((prev) => new Set(prev).add(created.id))
        await copy('新请求 Key', created.apiKey)
      }
      toast.success('请求 Key 已生成并立即生效')
    } catch (error) {
      toast.error(`生成请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setCreating(false)
    }
  }

  const addManualRequestKey = async () => {
    const apiKey = manualRequestApiKey.trim()
    if (!apiKey) return toast.error('请输入要新增的请求 Key')
    if (apiKey.length < 8) return toast.error('请求 Key 至少需要 8 个字符')
    setCreating(true)
    try {
      const response = await createRequestApiKey({ apiKey })
      setKeysAndResetDrafts(response)
      setManualRequestApiKey('')
      toast.success('请求 Key 已新增并立即生效')
    } catch (error) {
      toast.error(`新增请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setCreating(false)
    }
  }

  const startEditRequestKey = (item: RequestApiKeyItem) => {
    setEditingRequestKeyId(item.id)
    setRequestKeyDraft(item.apiKey)
  }

  const cancelEditRequestKey = () => {
    setEditingRequestKeyId(null)
    setRequestKeyDraft('')
  }

  const saveEditedRequestKey = async (item: RequestApiKeyItem) => {
    const apiKey = requestKeyDraft.trim()
    if (!apiKey) return toast.error('请输入新的请求 Key')
    if (apiKey.length < 8) return toast.error('请求 Key 至少需要 8 个字符')
    if (apiKey === item.apiKey) {
      cancelEditRequestKey()
      return
    }
    setProcessingKeyId(item.id)
    try {
      const response = await updateRequestApiKey(item.id, { apiKey })
      setKeysAndResetDrafts(response)
      toast.success('请求 Key 已保存，旧 Key 立即失效')
    } catch (error) {
      toast.error(`保存请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setProcessingKeyId(null)
    }
  }

  const removeRequestKey = async (item: RequestApiKeyItem) => {
    if (requestKeys.length <= 1) return toast.error('至少需要保留一个请求 Key')
    if (!window.confirm(`确认删除 ${item.maskedApiKey}？删除后使用该 Key 的客户端会立即 401。`)) return
    setProcessingKeyId(item.id)
    try {
      const response = await deleteRequestApiKey(item.id)
      setKeysAndResetDrafts(response)
      toast.success('请求 Key 已删除')
    } catch (error) {
      toast.error(`删除请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setProcessingKeyId(null)
    }
  }

  const toggleRequestKeyVisible = (id: string) => {
    setVisibleRequestKeyIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  return (
    <ConfigGroup
      icon={<KeyRound className="h-4 w-4" />}
      title="接入与登录 Key"
      description="请求 Key 可配置多个，供客户端调用模型接口；登录 Key 仍只有一个，用于进入管理后台。"
    >
      <div className="rounded-box border border-base-300 bg-base-100 p-3 md:col-span-2">
        <div className="mb-3">
          <div className="flex flex-col gap-2 md:flex-row md:items-start md:justify-between">
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-2">
                <div className="text-sm font-semibold">请求调用 Key</div>
                <span className="rounded border border-base-300 bg-base-200 px-2 py-0.5 text-[0.68rem] font-semibold text-base-content/60">
                  apiKey / apiKeys
                </span>
                <span className="rounded border border-base-300 bg-base-100 px-2 py-0.5 text-[0.68rem] font-semibold text-success">
                  {requestKeys.length} 个可用
                </span>
              </div>
              <div className="mt-1 text-xs leading-4 text-base-content/60">
                用于调用 /v1/messages、/cc/v1/messages 等模型接口，可复制到 x-api-key 或 Authorization: Bearer。新增、编辑、删除后立即生效。
              </div>
            </div>
            <Button type="button" color="primary" size="sm" className="shrink-0" disabled={loading || creating} onClick={generateRequestKey}>
              {creating ? <Loading size="xs" /> : <Wand2 className="h-4 w-4" />}
              随机生成并新增
            </Button>
          </div>
        </div>

        <div className="mb-3 grid gap-2 md:grid-cols-[minmax(0,1fr)_auto_auto]">
          <Input
            bordered
            size="sm"
            className="w-full min-w-0 font-mono text-xs"
            value={manualRequestApiKey}
            placeholder="手动输入要新增的请求 Key"
            disabled={loading || creating}
            onChange={(event) => setManualRequestApiKey(event.target.value)}
          />
          <Button type="button" variant="outline" size="sm" className="shrink-0" disabled={loading || creating} onClick={() => setManualRequestApiKey(generateLocalRequestApiKey())}>
            <Wand2 className="h-4 w-4" />
            随机生成
          </Button>
          <Button type="button" size="sm" className="shrink-0" disabled={loading || creating || !manualRequestApiKey.trim()} onClick={addManualRequestKey}>
            <Plus className="h-4 w-4" />
            新增 Key
          </Button>
        </div>

        <div className="space-y-2">
          {loading && <div className="rounded-box border border-base-300 bg-base-200/60 p-3 text-sm text-base-content/60">加载中...</div>}
          {!loading && requestKeys.length === 0 && <ErrorState title="未配置请求 Key" message="请先生成或手动新增一个请求 Key。" />}
          {!loading && requestKeys.map((item) => {
            const visible = visibleRequestKeyIds.has(item.id)
            const busy = processingKeyId === item.id
            const editing = editingRequestKeyId === item.id
            return (
              <div key={item.id} className="rounded-box border border-base-300 bg-base-200/45 p-3">
                <div className="mb-2 flex flex-col gap-2 md:flex-row md:items-center md:justify-between">
                  <div className="flex min-w-0 flex-wrap items-center gap-2">
                    <span className="text-sm font-semibold">请求 Key</span>
                    {item.primary && <span className="rounded border border-primary/25 bg-primary/10 px-2 py-0.5 text-[0.68rem] font-semibold text-primary">主 Key</span>}
                    <span className="font-mono text-[0.68rem] text-base-content/50">{item.id.slice(0, 12)}</span>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    <Button type="button" size="xs" disabled={busy || editing} onClick={() => toggleRequestKeyVisible(item.id)} title={visible ? '隐藏请求 Key' : '显示完整请求 Key'}>
                      {visible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                      {visible ? '隐藏' : '显示'}
                    </Button>
                    <Button type="button" size="xs" disabled={busy || editing} onClick={() => copy('请求 Key', item.apiKey)}>
                      <Copy className="h-3.5 w-3.5" />
                      复制
                    </Button>
                    {!editing && (
                      <Button type="button" color="ghost" size="xs" disabled={busy || Boolean(editingRequestKeyId)} onClick={() => startEditRequestKey(item)}>
                        <Edit3 className="h-3.5 w-3.5" />
                        编辑
                      </Button>
                    )}
                    <Button type="button" color="error" variant="outline" size="xs" disabled={busy || editing || requestKeys.length <= 1} onClick={() => removeRequestKey(item)}>
                      <Trash2 className="h-3.5 w-3.5" />
                      删除
                    </Button>
                  </div>
                </div>
                <div className="space-y-2">
                  <Input
                    bordered
                    readOnly={!editing}
                    size="sm"
                    aria-label="请求调用 Key"
                    className="w-full min-w-0 font-mono text-xs"
                    value={editing ? requestKeyDraft : visible ? item.apiKey : item.maskedApiKey}
                    disabled={busy}
                    onChange={(event) => setRequestKeyDraft(event.target.value)}
                  />
                  {editing && (
                    <div className="flex flex-wrap justify-end gap-2">
                      <Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => setRequestKeyDraft(generateLocalRequestApiKey())}>
                        <Wand2 className="h-4 w-4" />
                        随机生成
                      </Button>
                      <Button type="button" color="primary" size="sm" disabled={busy || !requestKeyDraft.trim()} onClick={() => saveEditedRequestKey(item)}>
                        {busy ? <Loading size="xs" /> : <Save className="h-4 w-4" />}
                        保存
                      </Button>
                      <Button type="button" variant="outline" size="sm" disabled={busy} onClick={cancelEditRequestKey}>
                        <X className="h-4 w-4" />
                        取消
                      </Button>
                    </div>
                  )}
                </div>
              </div>
            )
          })}
        </div>
      </div>

      <div className="rounded-box border border-base-300 bg-base-100 p-3 md:col-span-2">
        <div className="mb-3">
          <div className="flex flex-wrap items-center gap-2">
            <div className="text-sm font-semibold">后台登录 Key</div>
            <span className="rounded border border-base-300 bg-base-200 px-2 py-0.5 text-[0.68rem] font-semibold text-base-content/60">
              adminApiKey
            </span>
            <span className="rounded border border-info/25 bg-info/10 px-2 py-0.5 text-[0.68rem] font-semibold text-info">
              登录密码
            </span>
          </div>
          <div className="mt-1 text-xs leading-4 text-base-content/60">
            这是登录页输入的密码，也用于所有 /api/admin 后台接口。修改成功后，当前浏览器会自动切换到新 Key。
          </div>
        </div>

        <div className="flex flex-col gap-2 sm:flex-row">
          <Input
            bordered
            readOnly
            size="sm"
            aria-label="当前后台登录 Key"
            className="w-full min-w-0 flex-1 font-mono text-xs"
            value={loading ? '加载中...' : adminApiKeyValue || '未配置'}
          />
          <div className="flex gap-2 sm:shrink-0">
            <Button
              type="button"
              size="sm"
              className="flex-1 sm:flex-none"
              onClick={() => setShowAdminKey((value) => !value)}
              title={showAdminKey ? '隐藏登录 Key' : '显示完整登录 Key'}
            >
              {showAdminKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              {showAdminKey ? '隐藏' : '显示'}
            </Button>
            <Button type="button" size="sm" className="flex-1 sm:flex-none" onClick={() => copy('登录 Key', keys?.adminApiKey)}>
              <Copy className="h-4 w-4" />
              复制登录 Key
            </Button>
          </div>
        </div>

        <div className="mt-4 border-t border-base-300 pt-3">
          <div className="mb-2">
            <div className="text-sm font-semibold">修改登录 Key</div>
            <div className="mt-1 text-xs leading-4 text-base-content/60">
              保存后旧登录 Key 立即失效；当前页面会自动写入新 Key，不需要重新登录。
            </div>
          </div>
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input
              bordered
              type="password"
              size="sm"
              className="w-full min-w-0 flex-1"
              value={nextAdminApiKey}
              placeholder="输入新的登录 Key（至少 8 个字符）"
              disabled={saving}
              onChange={(event) => setNextAdminApiKey(event.target.value)}
            />
            <Button type="button" color="primary" size="sm" className="shrink-0" disabled={saving || !nextAdminApiKey.trim()} onClick={saveAdminApiKey}>
              {saving ? '保存中...' : '保存登录 Key'}
            </Button>
          </div>
        </div>
      </div>
    </ConfigGroup>
  )
}

function ModeSelect({ value, disabled, onChange }: { value: ReportedUsageFieldMode; disabled?: boolean; onChange: (value: ReportedUsageFieldMode) => void }) {
  return (
    <Select bordered size="sm" className="w-full" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value as ReportedUsageFieldMode)}>
      <Select.Option value="raw">原始值（不经过缓存计算）</Select.Option>
      <Select.Option value="preserve">保留计算值（不改写）</Select.Option>
      <Select.Option value="sample-max">按上限采样改写</Select.Option>
      <Select.Option value="sample-target">按目标采样改写</Select.Option>
    </Select>
  )
}

function PolicyNumberInput({
  title,
  description,
  value,
  min,
  step,
  suffix,
  disabled,
  onChange,
}: {
  title: string
  description: string
  value: number
  min?: number
  step?: number
  suffix: string
  disabled?: boolean
  onChange: (value: number) => void
}) {
  return (
    <FieldLabel title={title} description={description}>
      <Join className="w-full">
        <Input
          bordered
          size="sm"
          className="join-item w-full"
          type="number"
          value={value}
          min={min}
          step={step}
          inputMode={step ? 'decimal' : 'numeric'}
          disabled={disabled}
          onChange={(event) => onChange(numberValue(event.target.value, min ?? 0))}
        />
        <span className="join-item unit-addon min-w-16">{suffix}</span>
      </Join>
    </FieldLabel>
  )
}

function ReportedUsageFieldEditor({
  title,
  description,
  value,
  allowMoveDelta,
  disabled,
  onChange,
}: {
  title: string
  description: string
  value: ReportedUsageFieldPolicy
  allowMoveDelta?: boolean
  disabled?: boolean
  onChange: (value: ReportedUsageFieldPolicy) => void
}) {
  return (
    <Card bordered className="bg-base-100 shadow-none">
      <Card.Body className="p-3">
      <div className="mb-2">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</div>
      </div>
      <div className="space-y-2.5">
        <ModeSelect value={value.mode} disabled={disabled} onChange={(mode) => onChange({ ...value, mode })} />
        <div className="rounded-box bg-base-200 px-2.5 py-1.5 text-xs leading-4 text-base-content/65">{reportedUsageModeDescription(value.mode)}</div>
        {fieldNeedsMax(value) && (
          <PolicyNumberInput
            title="采样上限"
            description="控制改写后的最大 token 数。实际值会在 1 到这个上限之间自然浮动。"
            value={value.maxTokens}
            min={0}
            suffix="tokens"
            disabled={disabled}
            onChange={(maxTokens) => onChange({ ...value, maxTokens })}
          />
        )}
        {fieldNeedsTarget(value) && (
          <div className="grid gap-3 md:grid-cols-2">
            <PolicyNumberInput
              title="目标值"
              description="控制采样分布的目标 token 数。比如 writer 设置 3000，表示常规结果围绕 3000 附近自然浮动。"
              value={value.targetTokens}
              min={0}
              suffix="tokens"
              disabled={disabled}
              onChange={(targetTokens) => onChange({ ...value, targetTokens })}
            />
            <PolicyNumberInput
              title="常规最大倍率"
              description="控制正常随机范围的上限，常规最大值 = 目标值 × 倍率。"
              value={value.normalMaxMultiplier}
              min={1}
              step={0.1}
              suffix="倍"
              disabled={disabled}
              onChange={(normalMaxMultiplier) => onChange({ ...value, normalMaxMultiplier })}
            />
          </div>
        )}
        {allowMoveDelta && (
          <ToggleField
            title="差值计入缓存读取"
            description="开启后，input_tokens 被压低的差值会加到 cache_read_input_tokens，只改变下游上报外观。"
            checked={value.moveDeltaToCacheRead}
            disabled={disabled || value.mode === 'preserve' || value.mode === 'raw'}
            onChange={(moveDeltaToCacheRead) => onChange({ ...value, moveDeltaToCacheRead })}
          />
        )}
      </div>
      </Card.Body>
    </Card>
  )
}

function ReportedUsagePathEditor({
  title,
  description,
  value,
  onDelete,
  onChange,
}: {
  title: string
  description: string
  value: ReportedUsagePathPolicy
  onDelete?: () => void
  onChange: (value: ReportedUsagePathPolicy) => void
}) {
  return (
    <Card bordered className="bg-base-200/55 shadow-none">
      <Card.Body className="p-3">
      <div className="mb-3 flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h4 className="text-sm font-semibold">{title}</h4>
          <p className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</p>
        </div>
        <div className="flex shrink-0 items-center justify-between gap-2 sm:justify-end">
          {onDelete && (
            <Button type="button" color="error" variant="outline" size="xs" onClick={onDelete} title="删除这条路径覆盖">
              <Trash2 className="h-3.5 w-3.5" />
              删除覆盖
            </Button>
          )}
          <Toggle color="primary" size="sm" className="shrink-0" checked={value.enabled} onChange={(event) => onChange({ ...value, enabled: event.target.checked })} />
        </div>
      </div>
      {!value.enabled && (
        <Alert status="warning" className="mb-3 py-2 text-xs leading-5">
          当前路径已关闭本地模拟缓存上报：下游响应和后台 usage 记录会隐藏模拟 cache read/write，并把 input 展示为完整输入。字段改写配置已隐藏，重新开启后才会显示并生效。
        </Alert>
      )}
      {value.enabled && (
        <>
          <div className="grid gap-3 xl:grid-cols-2">
            <ReportedUsageFieldEditor
              title="输入字段改写（input_tokens）"
              description="控制给下游和后台记录的 input_tokens。原始值表示请求输入是多少就报多少；保留计算值表示使用 high-cache 计算后的 input；采样可把 input 压到几十以内并把差值计入缓存读取。"
              value={value.input}
              allowMoveDelta
              onChange={(input) => onChange({ ...value, input })}
            />
            <ReportedUsageFieldEditor
              title="输出字段改写（output_tokens）"
              description="控制给下游和后台记录的 output_tokens。默认建议使用原始值，避免本地模拟影响客户端对输出量的判断。"
              value={value.output}
              onChange={(output) => onChange({ ...value, output })}
            />
            <ReportedUsageFieldEditor
              title="缓存读取字段改写（cache_read_input_tokens）"
              description="控制计算完成后给下游和后台记录的 cache_read_input_tokens。保留计算值表示保留 high-cache/上游 metadata/估算后的读缓存值。"
              value={value.cacheRead}
              onChange={(cacheRead) => onChange({ ...value, cacheRead })}
            />
            <ReportedUsageFieldEditor
              title="缓存写入字段改写（cache_creation_input_tokens）"
              description="控制计算完成后给下游和后台记录的 cache_creation_input_tokens。/cc 可设置目标值 3000，实际会自然浮动。"
              value={value.cacheCreation}
              onChange={(cacheCreation) => onChange({ ...value, cacheCreation })}
            />
          </div>
          <div className="mt-3 grid gap-3 xl:grid-cols-3">
            <PolicyNumberInput
              title="读取缓存最终上限"
              description="在 input 差值转入 cache_read_input_tokens 后执行，超出时只向下裁剪。填 0 表示关闭最终守护。"
              value={value.finalCacheReadMaxTokens ?? 700000}
              min={0}
              suffix="tokens"
              onChange={(finalCacheReadMaxTokens) =>
                onChange({ ...value, finalCacheReadMaxTokens })
              }
            />
            <PolicyNumberInput
              title="最终上限扣减下限"
              description="达到最终上限时，从上限扣减的最小 token 数。默认 0 表示不做波动。"
              value={value.finalCacheReadJitterMinTokens ?? 0}
              min={0}
              suffix="tokens"
              onChange={(finalCacheReadJitterMinTokens) =>
                onChange({ ...value, finalCacheReadJitterMinTokens })
              }
            />
            <PolicyNumberInput
              title="最终上限扣减上限"
              description="达到最终上限时，从上限扣减的最大 token 数；不会超过读取缓存最终上限。"
              value={value.finalCacheReadJitterMaxTokens ?? 0}
              min={0}
              suffix="tokens"
              onChange={(finalCacheReadJitterMaxTokens) =>
                onChange({ ...value, finalCacheReadJitterMaxTokens })
              }
            />
          </div>
        </>
      )}
      </Card.Body>
    </Card>
  )
}

export function ConfigPanel() {
  const config = useRuntimeConfig()
  const updateConfig = useUpdateRuntimeConfig()
  const modelCapabilities = useModelCapabilities()
  const [draft, setDraft] = useState<RuntimeConfig>(emptyRuntimeConfig)
  const [activeTab, setActiveTab] = useState<ConfigTab>('dispatch')

  useEffect(() => {
    if (config.data) {
      setDraft({
        ...emptyRuntimeConfig,
        ...config.data,
        payloadShaping: {
          ...defaultPayloadShaping(),
          ...config.data.payloadShaping,
        },
        externalPools: {
          ...defaultExternalPoolsConfig(),
          ...config.data.externalPools,
        },
        promptCacheCreationControl: {
          ...defaultPromptCacheCreationControl(),
          ...config.data.promptCacheCreationControl,
        },
        modelMapping: normalizeModelMapping(config.data.modelMapping),
      })
    }
  }, [config.data])

  const save = () => {
    const next: RuntimeConfig = {
      ...draft,
      credentialRpm: toWhole(draft.credentialRpm),
      credentialMaxConcurrentRequests: toWhole(draft.credentialMaxConcurrentRequests),
      credentialTransientCooldownSecs: toWhole(draft.credentialTransientCooldownSecs, 1),
      credentialRateLimitCooldownSecs: toWhole(draft.credentialRateLimitCooldownSecs, 1),
      credentialServerErrorCooldownSecs: toWhole(draft.credentialServerErrorCooldownSecs, 1),
      credentialNetworkErrorCooldownSecs: toWhole(draft.credentialNetworkErrorCooldownSecs, 1),
      credentialStreamErrorCooldownSecs: toWhole(draft.credentialStreamErrorCooldownSecs, 1),
      credentialProtocolErrorCooldownSecs: toWhole(draft.credentialProtocolErrorCooldownSecs, 1),
      credentialAuthErrorCooldownSecs: toWhole(draft.credentialAuthErrorCooldownSecs, 1),
      credentialCooldownBackoffMultiplier: Math.max(1, Number(draft.credentialCooldownBackoffMultiplier.toFixed(2))),
      credentialCooldownJitterPercent: toWhole(draft.credentialCooldownJitterPercent, 0, 100),
      credentialProbationSecs: toWhole(draft.credentialProbationSecs),
      credentialMaxCooldownSecs: toWhole(draft.credentialMaxCooldownSecs, 1),
      credentialDispatchMaxWaitSecs: toWhole(draft.credentialDispatchMaxWaitSecs),
      kiroUpstreamResponseTimeoutSecs: toWhole(draft.kiroUpstreamResponseTimeoutSecs),
      credentialRetryMaxAttempts: toWhole(draft.credentialRetryMaxAttempts),
      credentialInFlightLeaseMaxSecs: toWhole(draft.credentialInFlightLeaseMaxSecs),
      dispatchGlobalMaxConcurrentRequests: toWhole(draft.dispatchGlobalMaxConcurrentRequests),
      dispatchMaxQueuedRequests: toWhole(draft.dispatchMaxQueuedRequests),
      credentialWarmupRequests: toWhole(draft.credentialWarmupRequests),
      credentialWarmupSelectionPercent: toWhole(draft.credentialWarmupSelectionPercent, 0, 100),
      credentialWarmupMaxSelectionPercent: toWhole(draft.credentialWarmupMaxSelectionPercent, 0, 100),
      schedulerErrorEwmaAlpha: Math.min(1, Math.max(0.01, Number(draft.schedulerErrorEwmaAlpha.toFixed(2)))),
      schedulerPriorityWeight: Math.max(0, Number(draft.schedulerPriorityWeight.toFixed(2))),
      schedulerLoadWeight: Math.max(0, Number(draft.schedulerLoadWeight.toFixed(2))),
      schedulerErrorWeight: Math.max(0, Number(draft.schedulerErrorWeight.toFixed(2))),
      schedulerLatencyWeight: Math.max(0, Number(draft.schedulerLatencyWeight.toFixed(4))),
      schedulerProbationWeight: Math.max(0, Number(draft.schedulerProbationWeight.toFixed(2))),
      schedulerSelectionPressureWeight: Math.max(0, Number(draft.schedulerSelectionPressureWeight.toFixed(2))),
      schedulerTotalSelectionWeight: Math.max(0, Number(draft.schedulerTotalSelectionWeight.toFixed(4))),
      schedulerTopK: toWhole(draft.schedulerTopK, 1, 100),
      payloadGuardMaxBytes: toWhole(draft.payloadGuardMaxBytes),
      payloadGuardSafetyMarginBytes: toWhole(draft.payloadGuardSafetyMarginBytes),
      payloadShaping: normalizePayloadShaping(draft.payloadShaping),
      promptCacheTargetReadRatio: toRatio(draft.promptCacheTargetReadRatio),
      promptCacheTokenScale: toScale(draft.promptCacheTokenScale),
      promptCacheMaxSimulatedInputTokens: toWhole(draft.promptCacheMaxSimulatedInputTokens),
      promptCacheCapJitterMinTokens: toWhole(draft.promptCacheCapJitterMinTokens),
      promptCacheCapJitterMaxTokens: toWhole(draft.promptCacheCapJitterMaxTokens),
      promptCacheScaleMinInputTokens: toWhole(draft.promptCacheScaleMinInputTokens),
      promptCacheCreationControl: normalizePromptCacheCreationControl(draft.promptCacheCreationControl),
      reportedUsage: normalizeReportedUsage(draft.reportedUsage),
      modelMapping: normalizeModelMapping(draft.modelMapping),
      externalPools: {
        ...defaultExternalPoolsConfig(),
        ...draft.externalPools,
        externalPoolGlobalMaxConcurrentRequests: toWhole(draft.externalPools.externalPoolGlobalMaxConcurrentRequests),
        externalPoolMaxQueuedRequests: toWhole(draft.externalPools.externalPoolMaxQueuedRequests),
        externalPoolDispatchMaxWaitSecs: toWhole(draft.externalPools.externalPoolDispatchMaxWaitSecs),
        externalPoolRetryMaxAttempts: toWhole(draft.externalPools.externalPoolRetryMaxAttempts),
        externalPoolLocalRescueMaxWaitSecs: toWhole(draft.externalPools.externalPoolLocalRescueMaxWaitSecs),
        localPoolCircuitWindowSecs: toWhole(draft.externalPools.localPoolCircuitWindowSecs, 1),
        localPoolCircuitOpenAfterFailures: toWhole(draft.externalPools.localPoolCircuitOpenAfterFailures, 1),
        localPoolCircuitRequireDistinctCredentials: toWhole(draft.externalPools.localPoolCircuitRequireDistinctCredentials),
        localPoolCircuitOpenSecs: toWhole(draft.externalPools.localPoolCircuitOpenSecs, 1),
        externalPoolAutoDisableFailureThreshold: toWhole(draft.externalPools.externalPoolAutoDisableFailureThreshold, 1),
        externalPoolAutoDisableWindowSecs: toWhole(draft.externalPools.externalPoolAutoDisableWindowSecs, 1),
        externalPoolAutoDisableDurationSecs: toWhole(draft.externalPools.externalPoolAutoDisableDurationSecs),
        externalPoolRateLimitCooldownSecs: toWhole(draft.externalPools.externalPoolRateLimitCooldownSecs, 1),
        externalPoolServerErrorCooldownSecs: toWhole(draft.externalPools.externalPoolServerErrorCooldownSecs, 1),
        externalPoolNetworkErrorCooldownSecs: toWhole(draft.externalPools.externalPoolNetworkErrorCooldownSecs, 1),
        externalPoolProtocolErrorCooldownSecs: toWhole(draft.externalPools.externalPoolProtocolErrorCooldownSecs, 1),
        externalPoolRequestTimeoutSecs: toWhole(draft.externalPools.externalPoolRequestTimeoutSecs),
        externalPoolStreamRequestTimeoutSecs: toWhole(draft.externalPools.externalPoolStreamRequestTimeoutSecs),
        externalPoolStreamIdleTimeoutSecs: toWhole(draft.externalPools.externalPoolStreamIdleTimeoutSecs),
        externalPoolUsageProjectionUpliftPercent: toWhole(draft.externalPools.externalPoolUsageProjectionUpliftPercent),
        externalPoolUsageProjectionOutputUpliftMinTokens: toWhole(draft.externalPools.externalPoolUsageProjectionOutputUpliftMinTokens),
        externalPoolUsageProjectionOutputUpliftPercent: toWhole(draft.externalPools.externalPoolUsageProjectionOutputUpliftPercent),
      },
      highCacheThreshold: toWhole(draft.highCacheThreshold),
    }
    if (next.credentialTransientCooldownSecs > next.credentialMaxCooldownSecs) return toast.error('临时冷却秒数不能大于最大冷却秒数')
    if ([next.credentialRateLimitCooldownSecs, next.credentialServerErrorCooldownSecs, next.credentialNetworkErrorCooldownSecs, next.credentialStreamErrorCooldownSecs, next.credentialProtocolErrorCooldownSecs, next.credentialAuthErrorCooldownSecs].some((value) => value > next.credentialMaxCooldownSecs)) return toast.error('错误类型基础冷却秒数不能大于最大冷却秒数')
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens) return toast.error('触顶扣减下限不能大于上限')
    if (next.payloadGuardEnabled && next.payloadGuardMaxBytes > 0 && next.payloadGuardMaxBytes < 65536) return toast.error('Kiro Payload 最大字节数必须为 0 或不小于 65536')
    if (next.payloadGuardEnabled && next.payloadGuardMaxBytes > 0 && next.payloadGuardMaxBytes - next.payloadGuardSafetyMarginBytes < 65536) return toast.error('Payload 安全余量不能让实际裁剪目标小于 65536')
    const editableConfig = { ...next }
    delete editableConfig.proxyUrl
    delete editableConfig.proxyUsername
    delete editableConfig.proxyPassword
    updateConfig.mutate(editableConfig, {
      onSuccess: () => toast.success('配置已更新'),
      onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
    })
  }

  const payloadSizeLimitEnabled = draft.payloadGuardEnabled && draft.payloadGuardMaxBytes > 0
  const payloadShapingBranchEnabled = payloadSizeLimitEnabled && draft.payloadShaping.enabled
  const payloadGuardMode = draft.payloadGuardMode ?? 'preemptive'
  const payloadGuardRetryMode = payloadGuardMode === 'on_too_long'
  const defaultModelMappingRules = generateDefaultModelMappingRules(modelCapabilities.data)
  const payloadConditionTitle = payloadGuardRetryMode
    ? '仅在上游返回输入过长后重试时执行'
    : '仅当发送前请求体超过上方阈值时执行'
  const payloadConditionDescription = payloadSizeLimitEnabled
    ? payloadGuardRetryMode
      ? '第一次上游请求只做协议修复和字节统计；只有返回输入过长类错误时，才按 payloadGuardMaxBytes 裁剪并重试一次。'
      : '这些配置会在发送上游前判断最终 Kiro JSON body 是否大于 payloadGuardMaxBytes；小请求不会被截断或整形。'
    : '当前 payloadGuardMaxBytes 为 0 或 Payload 防护关闭，因此这些按大小触发的历史整形、历史裁剪和错误后裁剪重试都不会运行。'

  if (config.isLoading) return <div className="py-10 text-center text-base-content/60">加载中...</div>
  if (config.error) return <ErrorState text={extractErrorMessage(config.error)} />

  return (
    <SectionCard
      title="运行时配置"
      description="这些配置会写入 PgSQL 并对后续新请求热加载生效；监听地址、密钥、数据库连接和代理客户端等启动期配置仍需要改启动配置后重启。"
    >
      <div className="space-y-4">
        <AccessKeysPanel />
        <StartupProxyPanel config={draft} />

        <div className="sticky top-[4.25rem] z-30 flex flex-col gap-2 rounded-box border border-base-300 bg-base-100/95 p-2 shadow-sm backdrop-blur sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0 text-xs leading-5 text-base-content/60">
            修改运行时配置后点击保存，新请求会热加载生效。
          </div>
          <Button type="button" color="primary" size="sm" className="shrink-0" onClick={save} disabled={updateConfig.isPending}>
            {updateConfig.isPending ? <Loading size="xs" /> : <Save className="h-4 w-4" />}
            保存
          </Button>
        </div>

        <Tabs variant="boxed" size="sm" className="config-tabs">
          {configTabs.map((tab) => (
            <Tabs.Tab
              key={tab.key}
              href="#"
              active={activeTab === tab.key}
              className="config-tab"
              onClick={(event) => {
                event.preventDefault()
                setActiveTab(tab.key)
              }}
            >
              <span className="font-semibold">{tab.label}</span>
              <span className="hidden text-[0.68rem] text-base-content/55 md:block">{tab.description}</span>
            </Tabs.Tab>
          ))}
        </Tabs>

        {activeTab === 'dispatch' && (
          <>
            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="凭据限速与冷却" description="控制单个账号被调用的频率，以及上游临时错误后多久再尝试使用该账号。">
              <NumberField title="单凭据每分钟请求上限" description="控制每个凭据每分钟最多承接多少请求。填 0 表示关闭本地限速。" value={draft.credentialRpm} min={0} suffix="次/分钟" onChange={(credentialRpm) => setDraft((prev) => ({ ...prev, credentialRpm }))} />
              <NumberField title="单凭据最大并发请求数" description="控制同一个凭据同时处理多少个请求。填 0 表示不限制。" value={draft.credentialMaxConcurrentRequests} min={0} suffix="并发" onChange={(credentialMaxConcurrentRequests) => setDraft((prev) => ({ ...prev, credentialMaxConcurrentRequests }))} />
              <NumberField title="兼容默认冷却秒数" description="供旧调用路径使用的默认冷却值。明确分类的错误使用下方独立设置。" value={draft.credentialTransientCooldownSecs} min={1} suffix="秒" onChange={(credentialTransientCooldownSecs) => setDraft((prev) => ({ ...prev, credentialTransientCooldownSecs }))} />
              <NumberField title="429 基础冷却" description="上游没有返回 Retry-After 时，限流错误首次触发的冷却时长。" value={draft.credentialRateLimitCooldownSecs} min={1} suffix="秒" onChange={(credentialRateLimitCooldownSecs) => setDraft((prev) => ({ ...prev, credentialRateLimitCooldownSecs }))} />
              <NumberField title="5xx / 408 基础冷却" description="上游过载或超时响应首次触发的冷却时长。" value={draft.credentialServerErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialServerErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialServerErrorCooldownSecs }))} />
              <NumberField title="网络错误基础冷却" description="发送失败、连接中断等网络错误首次触发的冷却时长。" value={draft.credentialNetworkErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialNetworkErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialNetworkErrorCooldownSecs }))} />
              <NumberField title="流读取错误基础冷却" description="流读取错误或上游 idle timeout 首次触发的冷却时长。" value={draft.credentialStreamErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialStreamErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialStreamErrorCooldownSecs }))} />
              <NumberField title="协议异常基础冷却" description="可重试协议不匹配和未分类瞬态错误首次触发的冷却时长。" value={draft.credentialProtocolErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialProtocolErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialProtocolErrorCooldownSecs }))} />
              <NumberField title="认证判定基础冷却" description="401/403 触发刷新或失败判定期间暂停继续调度该账号的时长。" value={draft.credentialAuthErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialAuthErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialAuthErrorCooldownSecs }))} />
              <NumberField title="连续失败退避倍率" description="同一凭据连续发生瞬态错误时冷却倍增倍率。" value={draft.credentialCooldownBackoffMultiplier} min={1} max={10} step={0.1} suffix="倍" onChange={(credentialCooldownBackoffMultiplier) => setDraft((prev) => ({ ...prev, credentialCooldownBackoffMultiplier }))} />
              <NumberField title="冷却随机抖动" description="对没有 Retry-After 的退避增加随机偏移，降低并发同时恢复。" value={draft.credentialCooldownJitterPercent} min={0} max={100} suffix="%" onChange={(credentialCooldownJitterPercent) => setDraft((prev) => ({ ...prev, credentialCooldownJitterPercent }))} />
              <NumberField title="恢复观察窗口" description="冷却结束后仍降低该凭据的调度权重，成功后逐步恢复。" value={draft.credentialProbationSecs} min={0} suffix="秒" onChange={(credentialProbationSecs) => setDraft((prev) => ({ ...prev, credentialProbationSecs }))} />
              <NumberField title="最大冷却秒数" description="控制单个凭据最长冷却时间。" value={draft.credentialMaxCooldownSecs} min={1} suffix="秒" onChange={(credentialMaxCooldownSecs) => setDraft((prev) => ({ ...prev, credentialMaxCooldownSecs }))} />
              <NumberField title="单请求最长排队等待" description="所有可用凭据都处于冷却、限速或并发占满时最多等待多久。填 0 表示不限制。" value={draft.credentialDispatchMaxWaitSecs} min={0} suffix="秒" onChange={(credentialDispatchMaxWaitSecs) => setDraft((prev) => ({ ...prev, credentialDispatchMaxWaitSecs }))} />
              <NumberField title="Kiro 上游响应头超时" description="请求发出后等待 Kiro 上游返回响应头的最长时间，不影响后续流式输出。填 0 表示只用底层 HTTP client 超时。" value={draft.kiroUpstreamResponseTimeoutSecs} min={0} suffix="秒" onChange={(kiroUpstreamResponseTimeoutSecs) => setDraft((prev) => ({ ...prev, kiroUpstreamResponseTimeoutSecs }))} />
              <NumberField title="单请求最大重试次数" description="一次上游调用最多尝试多少个凭据/轮次。填 0 表示自动：小账号池最多 9 次，大账号池至少覆盖一轮账号。" value={draft.credentialRetryMaxAttempts} min={0} suffix="次" onChange={(credentialRetryMaxAttempts) => setDraft((prev) => ({ ...prev, credentialRetryMaxAttempts }))} />
              <NumberField title="异常并发自动回收" description="单个并发占用超过多久未活跃时自动释放。填 0 表示关闭。" value={draft.credentialInFlightLeaseMaxSecs} min={0} suffix="秒" onChange={(credentialInFlightLeaseMaxSecs) => setDraft((prev) => ({ ...prev, credentialInFlightLeaseMaxSecs }))} />
              <NumberField title="全局最大并发请求数" description="控制所有凭据合计可同时处理的请求数。填 0 表示不限制。" value={draft.dispatchGlobalMaxConcurrentRequests} min={0} suffix="并发" onChange={(dispatchGlobalMaxConcurrentRequests) => setDraft((prev) => ({ ...prev, dispatchGlobalMaxConcurrentRequests }))} />
              <NumberField title="最大等待队列请求数" description="调度容量已满时允许排队等待的请求数量。填 0 表示不限制。" value={draft.dispatchMaxQueuedRequests} min={0} suffix="请求" onChange={(dispatchMaxQueuedRequests) => setDraft((prev) => ({ ...prev, dispatchMaxQueuedRequests }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="健康评分调度" description="均衡/健康均衡模式使用共享错误率、延迟与实时并发为候选排序，并在最佳候选中分散请求。">
              <NumberField title="错误 EWMA 新样本权重" description="越高越快响应近期故障，范围 0.01 到 1。" value={draft.schedulerErrorEwmaAlpha} min={0.01} max={1} step={0.01} suffix="系数" onChange={(schedulerErrorEwmaAlpha) => setDraft((prev) => ({ ...prev, schedulerErrorEwmaAlpha }))} />
              <NumberField title="优先级权重" description="配置优先级对健康得分的影响。" value={draft.schedulerPriorityWeight} min={0} step={0.1} suffix="权重" onChange={(schedulerPriorityWeight) => setDraft((prev) => ({ ...prev, schedulerPriorityWeight }))} />
              <NumberField title="实时负载权重" description="当前在途并发对健康得分的影响。" value={draft.schedulerLoadWeight} min={0} step={1} suffix="权重" onChange={(schedulerLoadWeight) => setDraft((prev) => ({ ...prev, schedulerLoadWeight }))} />
              <NumberField title="近期错误率权重" description="近期上游错误率对健康得分的影响。" value={draft.schedulerErrorWeight} min={0} step={1} suffix="权重" onChange={(schedulerErrorWeight) => setDraft((prev) => ({ ...prev, schedulerErrorWeight }))} />
              <NumberField title="耗时权重" description="每毫秒成功耗时 EWMA 对健康得分的影响。" value={draft.schedulerLatencyWeight} min={0} step={0.001} suffix="权重" onChange={(schedulerLatencyWeight) => setDraft((prev) => ({ ...prev, schedulerLatencyWeight }))} />
              <NumberField title="恢复观察惩罚" description="处于观察窗口时额外增加的健康得分。" value={draft.schedulerProbationWeight} min={0} step={1} suffix="权重" onChange={(schedulerProbationWeight) => setDraft((prev) => ({ ...prev, schedulerProbationWeight }))} />
              <NumberField title="近期调度压力权重" description="最近 60 秒被选中比例高于平均值时增加的降权，避免短时间集中打同一账号。" value={draft.schedulerSelectionPressureWeight} min={0} step={1} suffix="权重" onChange={(schedulerSelectionPressureWeight) => setDraft((prev) => ({ ...prev, schedulerSelectionPressureWeight }))} />
              <NumberField title="总调度次数权重" description="总调度次数对健康得分的影响。默认 0，只建议作为很弱的长期均衡信号。" value={draft.schedulerTotalSelectionWeight} min={0} step={0.001} suffix="权重" onChange={(schedulerTotalSelectionWeight) => setDraft((prev) => ({ ...prev, schedulerTotalSelectionWeight }))} />
              <NumberField title="最佳候选抽样数量" description="从得分最佳的前 N 个账号按权重选择，降低请求集中。" value={draft.schedulerTopK} min={1} max={100} suffix="个" onChange={(schedulerTopK) => setDraft((prev) => ({ ...prev, schedulerTopK }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Sparkles className="h-4 w-4" />} title="新凭据预热" description="预热不会伪造成功次数；批量导入时按预热账号数量分配目标流量，避免新账号长期吃不到请求。">
              <NumberField title="预热剩余请求数" description="新添加凭据默认进入预热状态的请求次数。填 0 表示不预热。" value={draft.credentialWarmupRequests} min={0} suffix="次" onChange={(credentialWarmupRequests) => setDraft((prev) => ({ ...prev, credentialWarmupRequests }))} />
              <NumberField title="预热凭据参与概率" description="每个预热凭据的目标参与比例。批量导入时会按预热账号数放大。" value={draft.credentialWarmupSelectionPercent} min={0} max={100} suffix="%" onChange={(credentialWarmupSelectionPercent) => setDraft((prev) => ({ ...prev, credentialWarmupSelectionPercent }))} />
              <NumberField title="预热总流量上限" description="已有非预热账号可用时，所有预热账号合计最多承接的真实请求比例。" value={draft.credentialWarmupMaxSelectionPercent} min={0} max={100} suffix="%" onChange={(credentialWarmupMaxSelectionPercent) => setDraft((prev) => ({ ...prev, credentialWarmupMaxSelectionPercent }))} />
            </ConfigGroup>

            <ConfigGroup
              icon={<Wand2 className="h-4 w-4" />}
              title="请求压缩与 Payload 防护"
              description="区分每次请求都会执行的全局处理，以及按配置触发的大小裁剪、历史裁剪和兜底处理。"
            >
              <ImpactGroupHeader
                label="全局影响"
                title="每次请求发送上游前都会检查"
                description="这些配置不等待上游 400，也不依赖超预算判断。请求压缩开启后每次生效；Payload 防护开启后每次都会做协议修复和 body 字节统计。"
              />
              <ToggleField title="启用请求压缩" description="控制是否对上游请求做压缩处理。关闭时不会改变请求内容。" checked={draft.compressionEnabled} onChange={(compressionEnabled) => setDraft((prev) => ({ ...prev, compressionEnabled }))} />
              <ToggleField title="仅压缩空白字符" description="控制压缩时是否只处理多余空白。这是当前推荐的低风险压缩方式。" checked={draft.whitespaceCompression} disabled={!draft.compressionEnabled} onChange={(whitespaceCompression) => setDraft((prev) => ({ ...prev, whitespaceCompression }))} />
              <ToggleField title="启用 Kiro Payload 防护" description="按真实 Kiro JSON 字节数统计请求，并修复空 toolUses、孤立 tool_result 等 Kiro 容易拒绝的形态。" checked={draft.payloadGuardEnabled} onChange={(payloadGuardEnabled) => setDraft((prev) => ({ ...prev, payloadGuardEnabled }))} />
              <ToggleField title="备用池也应用 Payload 整形" description="开启后，备用池请求会复用本页同一套阈值、模式和内容整形规则；关闭时备用池保持原始 Anthropic 请求体透传。" checked={draft.payloadGuardExternalEnabled} disabled={!draft.payloadGuardEnabled} onChange={(payloadGuardExternalEnabled) => setDraft((prev) => ({ ...prev, payloadGuardExternalEnabled }))} />
              <FieldLabel title="大小裁剪触发模式" description="发送前预裁剪保持当前行为；上游过长后裁剪重试会先原样请求，只在输入过长类 400 后按阈值裁剪并重试一次。">
                <Select bordered size="sm" className="w-full" value={payloadGuardMode} disabled={!draft.payloadGuardEnabled} onChange={(event) => setDraft((prev) => ({ ...prev, payloadGuardMode: event.target.value as PayloadGuardMode }))}>
                  <Select.Option value="preemptive">发送前预裁剪</Select.Option>
                  <Select.Option value="on_too_long">上游过长后裁剪重试</Select.Option>
                </Select>
              </FieldLabel>
              <ImpactGroupHeader
                label="条件阈值"
                title="控制后续条件分支是否有机会触发"
                description="payloadGuardMaxBytes 是本地裁剪目标阈值，不是模型上下文窗口。填 0 表示关闭所有按大小触发的内容整形、历史裁剪、当前内容兜底裁剪和错误后裁剪重试，但仍保留上面的协议修复。"
              />
              <NumberField title="Kiro Payload 裁剪目标阈值" description="按最终发送到 Kiro 的 JSON body 字节数计算。默认 460800 bytes；填 0 时下方所有“条件分支”和“兜底分支”配置都不会触发。" value={draft.payloadGuardMaxBytes} min={0} suffix="bytes" onChange={(payloadGuardMaxBytes) => setDraft((prev) => ({ ...prev, payloadGuardMaxBytes }))} />
              <NumberField title="Payload 安全余量" description="实际裁剪目标会从上面的阈值中扣除该余量。默认 32768 bytes；用于避免 provider 层追加字段后贴近 Kiro 请求体上限。" value={draft.payloadGuardSafetyMarginBytes} min={0} suffix="bytes" disabled={!payloadSizeLimitEnabled} onChange={(payloadGuardSafetyMarginBytes) => setDraft((prev) => ({ ...prev, payloadGuardSafetyMarginBytes }))} />
              <ImpactGroupHeader
                label="条件分支"
                title={payloadConditionTitle}
                description={payloadConditionDescription}
                muted={!payloadSizeLimitEnabled}
              />
              <ToggleField title="超限裁剪旧历史" description="按当前模式触发大小裁剪时，优先裁剪最旧历史；关闭后不会裁 history，仍超限会继续透传给 Kiro。" checked={draft.payloadGuardTrimHistory} disabled={!payloadSizeLimitEnabled} onChange={(payloadGuardTrimHistory) => setDraft((prev) => ({ ...prev, payloadGuardTrimHistory }))} />
              <ToggleField title="启用 Payload 内容整形" description="按当前模式触发大小裁剪时生效，默认只处理旧历史、历史 thinking、历史 WebFetch 和工具定义描述。" checked={draft.payloadShaping.enabled} disabled={!payloadSizeLimitEnabled} onChange={(enabled) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, enabled } }))} />
              <ToggleField title="截断历史工具结果" description="只截断历史 tool_result，保留头尾和省略说明；当前合法 tool_result 默认不截断。" checked={draft.payloadShaping.truncateHistoricalToolResults} disabled={!payloadShapingBranchEnabled} onChange={(truncateHistoricalToolResults) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateHistoricalToolResults } }))} />
              <NumberField title="历史工具结果保留字符" description="单个历史 tool_result 的通用头尾保留预算。默认 8000 字符；WebFetch 会先走专项去噪。" value={draft.payloadShaping.historicalToolResultMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="chars" onChange={(historicalToolResultMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, historicalToolResultMaxChars } }))} />
              <ToggleField title="移除历史 thinking" description="只移除旧 assistant 历史里的 thinking 标签内容，不处理当前请求内容。" checked={draft.payloadShaping.discardHistoricalThinking} disabled={!payloadShapingBranchEnabled} onChange={(discardHistoricalThinking) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, discardHistoricalThinking } }))} />
              <ToggleField title="压缩工具定义描述" description="压缩当前请求 tools 的 description 和 JSON Schema 注释字段，不删除 type、properties、required、enum 等语义字段。" checked={draft.payloadShaping.compressToolDefinitions} disabled={!payloadShapingBranchEnabled} onChange={(compressToolDefinitions) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, compressToolDefinitions } }))} />
              <NumberField title="工具定义预算" description="当前请求 tools 的 JSON 字节预算。超过后压缩描述和 schema 注释；默认 20000 bytes，填 0 表示关闭该预算压缩。" value={draft.payloadShaping.toolDefinitionsBudgetBytes} disabled={!payloadShapingBranchEnabled} min={0} suffix="bytes" onChange={(toolDefinitionsBudgetBytes) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, toolDefinitionsBudgetBytes } }))} />
              <ToggleField title="WebFetch 历史去噪" description="对历史 WebFetch 工具结果移除 data image、重复行和明显噪声，默认正文预算 12000 字符。" checked={draft.payloadShaping.webFetchTrimEnabled} disabled={!payloadShapingBranchEnabled} onChange={(webFetchTrimEnabled) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, webFetchTrimEnabled } }))} />
              <NumberField title="WebFetch 正文预算" description="历史 WebFetch 正文去噪后的字符预算。填 0 表示关闭该项正文裁剪。" value={draft.payloadShaping.webFetchBodyMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="chars" onChange={(webFetchBodyMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, webFetchBodyMaxChars } }))} />
              <ImpactGroupHeader
                label="兜底分支"
                title="历史处理后仍超预算时才可能执行"
                description={
                  payloadShapingBranchEnabled
                    ? '这些配置属于最后兜底：只有历史整形和历史裁剪之后，body 仍然大于 payloadGuardMaxBytes 时才会处理当前消息、当前 tool_result、当前 document 或当前图片。'
                    : '当前超预算条件或 Payload 内容整形未启用，因此这些当前内容兜底配置不会运行。'
                }
                muted={!payloadShapingBranchEnabled}
              />
              <ToggleField title="自动适配当前内容预算" description="开启后，历史裁剪后仍超出 Kiro Payload 最大字节数时，会按下方预算裁剪当前 tool_result、当前文本、当前 document，并按体积丢弃当前图片；默认关闭。" checked={draft.payloadShaping.fitCurrentPayloadToBudget} disabled={!payloadShapingBranchEnabled} onChange={(fitCurrentPayloadToBudget) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, fitCurrentPayloadToBudget } }))} />
              <ToggleField title="截断当前工具结果" description="当前合法 tool_result 也可能非常大。开启后仅在历史裁剪后仍超预算时按头尾保留截断；自动适配当前内容预算打开时也会启用。" checked={draft.payloadShaping.truncateCurrentToolResults} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentToolResults) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentToolResults } }))} />
              <NumberField title="当前工具结果保留字符" description="单个当前 tool_result 的头尾保留预算。开启当前工具结果截断后使用；默认 80000 字符。" value={draft.payloadShaping.currentToolResultMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="chars" onChange={(currentToolResultMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentToolResultMaxChars } }))} />
              <ToggleField title="截断当前用户文本" description="开启后仅在仍超预算时截断当前 user content；包含 document 标签时会保留文档块结构，并只裁剪文档外侧文本。" checked={draft.payloadShaping.truncateCurrentUserContent} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentUserContent) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentUserContent } }))} />
              <NumberField title="当前用户文本保留字符" description="当前纯文本 user content 的头尾保留预算。开启当前用户文本截断后使用；默认 120000 字符。" value={draft.payloadShaping.currentUserContentMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="chars" onChange={(currentUserContentMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentUserContentMaxChars } }))} />
              <ToggleField title="截断当前文档" description="开启后仅在仍超预算时截断当前 document 块正文，并保留 document 开闭标签；适合 PDF 文本过大场景。" checked={draft.payloadShaping.truncateCurrentDocuments} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentDocuments) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentDocuments } }))} />
              <NumberField title="当前文档保留字符" description="单个当前 document 正文的头尾保留预算。开启当前文档截断后使用；默认 80000 字符。" value={draft.payloadShaping.currentDocumentMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="chars" onChange={(currentDocumentMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentDocumentMaxChars } }))} />
              <ToggleField title="丢弃当前图片" description="图片不会本地重编码压缩。开启后仅在仍超预算时按体积从大到小丢弃，并在文本中追加代理省略说明，默认关闭。" checked={draft.payloadShaping.truncateCurrentImages} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentImages) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentImages } }))} />
              <NumberField title="当前图片 JSON 预算" description="当前 images 数组允许保留的 JSON 字节数。开启当前图片丢弃后使用；默认 180000 bytes。" value={draft.payloadShaping.currentImagesMaxBytes} disabled={!payloadShapingBranchEnabled} min={0} suffix="bytes" onChange={(currentImagesMaxBytes) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentImagesMaxBytes } }))} />
            </ConfigGroup>
          </>
        )}

        {activeTab === 'cache' && (
          <>
            <ConfigGroup icon={<Zap className="h-4 w-4" />} title="高缓存模拟" description="控制 /v1/messages 和 /cc/v1/messages 的本地高缓存 usage 模拟。只影响下游看到的统计和后台记录，不影响 count_tokens 计算接口。">
              <NumberField title="缓存读取目标比例" description="cache_read_input_tokens 大致占输入的目标比例。常用值 0.95 到 0.99。" value={draft.promptCacheTargetReadRatio} min={0} max={0.99} step={0.01} suffix="比例" onChange={(promptCacheTargetReadRatio) => setDraft((prev) => ({ ...prev, promptCacheTargetReadRatio }))} />
              <NumberField title="高缓存输入放大倍数" description="控制高缓存模拟时 total input 的放大程度。只影响缓存计算，不代表 input 上报一定放大。" value={draft.promptCacheTokenScale} min={1} max={3} step={0.1} suffix="倍" onChange={(promptCacheTokenScale) => setDraft((prev) => ({ ...prev, promptCacheTokenScale }))} />
              <NumberField title="模拟输入上限" description="高缓存模拟后 total input 的最高值。填 0 表示不设置上限。" value={draft.promptCacheMaxSimulatedInputTokens} min={0} suffix="tokens" onChange={(promptCacheMaxSimulatedInputTokens) => setDraft((prev) => ({ ...prev, promptCacheMaxSimulatedInputTokens }))} />
              <NumberField title="放大启用门槛" description="基础输入达到多少 tokens 后才启用输入放大。" value={draft.promptCacheScaleMinInputTokens} min={0} suffix="tokens" onChange={(promptCacheScaleMinInputTokens) => setDraft((prev) => ({ ...prev, promptCacheScaleMinInputTokens }))} />
              <NumberField title="触顶扣减下限" description="模拟输入达到上限时，最少从上限扣掉多少 tokens。" value={draft.promptCacheCapJitterMinTokens} min={0} suffix="tokens" onChange={(promptCacheCapJitterMinTokens) => setDraft((prev) => ({ ...prev, promptCacheCapJitterMinTokens }))} />
              <NumberField title="触顶扣减上限" description="模拟输入达到上限时，最多从上限扣掉多少 tokens。" value={draft.promptCacheCapJitterMaxTokens} min={0} suffix="tokens" onChange={(promptCacheCapJitterMaxTokens) => setDraft((prev) => ({ ...prev, promptCacheCapJitterMaxTokens }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="缓存创建频次控制" description="只限制最终上报的 cache_creation_input_tokens 出现频次；不改变本地缓存命中计算、上游请求或 cache read 字段策略。">
              <ToggleField title="启用缓存创建频次控制" description="关闭时完全保持旧行为。开启后仅对本地 high-cache 模拟 usage 生效，真实上游 metadata 不受影响。" checked={draft.promptCacheCreationControl.enabled} onChange={(enabled) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, enabled } }))} />
              <FieldLabel title="控制维度" description="会话 + 模型会跨凭据共享频次状态，默认更适合减少调度换号后的重复 creation 上报；凭据 + 会话 + 模型更贴近真实账号缓存隔离。">
                <Select bordered size="sm" className="w-full" value={draft.promptCacheCreationControl.scopeMode} disabled={!draft.promptCacheCreationControl.enabled} onChange={(event) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, scopeMode: event.target.value as 'credential_conversation_model' | 'conversation_model' } }))}>
                  <Select.Option value="credential_conversation_model">凭据 + 会话 + 模型</Select.Option>
                  <Select.Option value="conversation_model">会话 + 模型</Select.Option>
                </Select>
              </FieldLabel>
              <NumberField title="最小成功请求间隔" description="同一控制维度下，两次 cache creation 之间至少间隔多少次成功请求。填 0 表示不按请求次数限制。" value={draft.promptCacheCreationControl.minSuccessfulRequestsBetweenCreation} disabled={!draft.promptCacheCreationControl.enabled} min={0} suffix="次" onChange={(minSuccessfulRequestsBetweenCreation) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, minSuccessfulRequestsBetweenCreation } }))} />
              <NumberField title="最小时间间隔" description="同一控制维度下，两次 cache creation 之间至少间隔多少秒。填 0 表示不按时间限制。" value={draft.promptCacheCreationControl.minCreationIntervalSecs} disabled={!draft.promptCacheCreationControl.enabled} min={0} suffix="秒" onChange={(minCreationIntervalSecs) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, minCreationIntervalSecs } }))} />
              <NumberField title="最小累计增量" description="被抑制的 creation 累计到多少 tokens 后才允许下一次创建上报。填 0 表示不按增量限制。" value={draft.promptCacheCreationControl.minCreationDeltaTokens} disabled={!draft.promptCacheCreationControl.enabled} min={0} suffix="tokens" onChange={(minCreationDeltaTokens) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, minCreationDeltaTokens } }))} />
              <NumberField title="单次创建上限" description="一次响应最多上报多少 cache creation tokens。超出部分会回到 input_tokens，填 0 表示不限制。" value={draft.promptCacheCreationControl.maxCreationTokensPerEvent} disabled={!draft.promptCacheCreationControl.enabled} min={0} suffix="tokens" onChange={(maxCreationTokensPerEvent) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, maxCreationTokensPerEvent } }))} />
              <NumberField title="额度窗口长度" description="在这个时间窗口内累计控制 cache creation 额度。填 0 表示关闭窗口额度控制。" value={draft.promptCacheCreationControl.creationBudgetWindowSecs} disabled={!draft.promptCacheCreationControl.enabled} min={0} suffix="秒" onChange={(creationBudgetWindowSecs) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, creationBudgetWindowSecs } }))} />
              <NumberField title="窗口创建额度" description="单个额度窗口内最多允许上报多少 cache creation tokens。填 0 表示不限制。" value={draft.promptCacheCreationControl.maxCreationTokensPerWindow} disabled={!draft.promptCacheCreationControl.enabled} min={0} suffix="tokens" onChange={(maxCreationTokensPerWindow) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, maxCreationTokensPerWindow } }))} />
              <NumberField title="状态空闲过期" description="同一控制维度长时间没有请求后清理控制器状态。填 0 表示不按空闲时间清理。" value={draft.promptCacheCreationControl.expireAfterIdleSecs} disabled={!draft.promptCacheCreationControl.enabled} min={0} suffix="秒" onChange={(expireAfterIdleSecs) => setDraft((prev) => ({ ...prev, promptCacheCreationControl: { ...prev.promptCacheCreationControl, expireAfterIdleSecs } }))} />
            </ConfigGroup>
          </>
        )}

        {activeTab === 'usage' && (
          <ConfigGroup icon={<BadgeInfo className="h-4 w-4" />} title="路径级 Usage 上报改写" description="每个路径前缀都是独立覆盖项：先使用未匹配路径默认策略，再按最长匹配路径前缀覆盖。只改变下游响应和后台 usage 记录，不影响本地 reader 计算、缓存 tracker 或上游请求。">
            <div className="space-y-3 md:col-span-2">
              <ReportedUsagePathEditor
                title="未匹配路径默认上报改写"
                description="没有命中 /cc、/ha、/na 等路径覆盖时使用。默认适合 /v1：input/output 使用原始值，cache read/write 保留 high-cache 计算值。"
                value={draft.reportedUsage.default}
                onChange={(defaultPolicy) => setDraft((prev) => ({ ...prev, reportedUsage: { ...prev.reportedUsage, default: defaultPolicy } }))}
              />
              {Object.entries(draft.reportedUsage.pathOverrides).map(([prefix, policy]) => (
                <div key={prefix} className="space-y-3">
                  <FieldLabel title="路径前缀" description="当前前缀只控制它自己匹配到的路径。例如 /cc、/ha、/na 互相独立，后续可以分别改 input、output、cache read、cache write。">
                    <Input
                      bordered
                      size="sm"
                      value={prefix}
                      onChange={(event) => {
                        const nextPrefix = event.target.value
                        setDraft((prev) => {
                          const pathOverrides = { ...prev.reportedUsage.pathOverrides }
                          delete pathOverrides[prefix]
                          pathOverrides[nextPrefix] = policy
                          return { ...prev, reportedUsage: { ...prev.reportedUsage, pathOverrides } }
                        })
                      }}
                    />
                  </FieldLabel>
                  <ReportedUsagePathEditor
                    title={`${prefix || '/'} 覆盖策略`}
                    description="只覆盖这个路径前缀匹配到的请求。关闭后不会把本地模拟 cache usage 展示给下游或后台记录；如果请求本身带有真实上游 metadata usage，仍按真实值处理。"
                    value={policy}
                    onDelete={() =>
                      setDraft((prev) => {
                        const pathOverrides = { ...prev.reportedUsage.pathOverrides }
                        delete pathOverrides[prefix]
                        return { ...prev, reportedUsage: { ...prev.reportedUsage, pathOverrides } }
                      })
                    }
                    onChange={(nextPolicy) =>
                      setDraft((prev) => ({
                        ...prev,
                        reportedUsage: {
                          ...prev.reportedUsage,
                          pathOverrides: { ...prev.reportedUsage.pathOverrides, [prefix]: nextPolicy },
                        },
                      }))
                    }
                  />
                </div>
              ))}
              <div className="flex justify-end">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    setDraft((prev) => {
                      let index = 1
                      let prefix = '/new'
                      while (prev.reportedUsage.pathOverrides[prefix]) {
                        index += 1
                        prefix = `/new-${index}`
                      }
                      return {
                        ...prev,
                        reportedUsage: {
                          ...prev.reportedUsage,
                          pathOverrides: { ...prev.reportedUsage.pathOverrides, [prefix]: pathPolicy() },
                        },
                      }
                    })
                  }
                >
                  添加路径覆盖
                </Button>
              </div>
            </div>
          </ConfigGroup>
        )}

        {activeTab === 'compat' && (
          <>
            <ConfigGroup icon={<Shield className="h-4 w-4" />} title="兼容与诊断" description="控制协议兼容细节和调试信息展示。调试信息只影响响应头或非流式 thinking 解析，不改变凭据调度。">
              <FieldLabel title="兼容模式" description="Claude Code 兼容适合日常 CLI 使用；Anthropic 严格模式会减少代理侧改写；调试模式会默认暴露代理改写告警头。">
                <Select bordered size="sm" value={draft.compatProfile} onChange={(event) => setDraft((prev) => ({ ...prev, compatProfile: event.target.value as CompatProfile }))}>
                  <Select.Option value="claude-code">Claude Code 兼容</Select.Option>
                  <Select.Option value="anthropic-strict">Anthropic 严格模式</Select.Option>
                  <Select.Option value="debug">调试模式</Select.Option>
                </Select>
              </FieldLabel>
              <FieldLabel title="Kiro Agent Mode" description="控制发往 Kiro IDE 上游的 x-amzn-kiro-agent-mode。vibe 保持当前 Claude Code 成功链路；spec 强制规格模式；auto 会按账号协议自动选择。">
                <Select bordered size="sm" value={draft.kiroAgentModeStrategy} onChange={(event) => setDraft((prev) => ({ ...prev, kiroAgentModeStrategy: event.target.value as KiroAgentModeStrategy }))}>
                  <Select.Option value="vibe">vibe（默认兼容）</Select.Option>
                  <Select.Option value="spec">spec（强制规格模式）</Select.Option>
                  <Select.Option value="auto">auto（按账号协议自动）</Select.Option>
                </Select>
              </FieldLabel>
              <FieldLabel title="模型解析策略" description="默认兼容解析会保留 sonnet、opus、default 等短模型名和同族自动归一化；更严格模式只影响请求发上游前的模型名解析，不改变凭据调度。">
                <Select bordered size="sm" value={draft.modelResolutionMode} onChange={(event) => setDraft((prev) => ({ ...prev, modelResolutionMode: event.target.value as ModelResolutionMode }))}>
                  <Select.Option value="compatible">默认兼容解析</Select.Option>
                  <Select.Option value="alias_only">仅精确与显式别名</Select.Option>
                  <Select.Option value="exact_only">仅模型目录精确 ID</Select.Option>
                </Select>
              </FieldLabel>
              <FieldLabel title="模型映射与兜底规则" description="精确匹配后按版本等价、别名、兜底规则解析；关闭映射或关闭自动生成并清空规则时，未命中的模型直接透传给上游。">
                <div className="space-y-3">
                  <div className="grid gap-3 lg:grid-cols-2">
                    <ToggleField title="启用模型映射" description="关闭后不做本地映射或兜底。" checked={draft.modelMapping.enabled} onChange={(enabled) => setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, enabled } }))} />
                    <ToggleField title="自动生成规则" description="按当前上游模型列表启用 dash/dot 小版本等价和常用别名。" checked={draft.modelMapping.autoGenerateRules} onChange={(autoGenerateRules) => setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, autoGenerateRules } }))} />
                  </div>
                  <div className="flex flex-wrap gap-2">
                    <Button size="sm" variant="outline" disabled={modelCapabilities.isLoading} onClick={() => {
                      if (!defaultModelMappingRules.length) {
                        toast.error('当前模型能力列表为空，无法生成默认规则')
                        return
                      }
                      setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, enabled: true, autoGenerateRules: true, rules: defaultModelMappingRules } }))
                      toast.success(`已填充 ${defaultModelMappingRules.length} 条默认模型映射规则`)
                    }}>
                      <Wand2 className="h-4 w-4" />
                      填充默认规则
                    </Button>
                  </div>
                  <textarea
                    className="textarea textarea-bordered min-h-40 w-full font-mono text-xs"
                    value={JSON.stringify(draft.modelMapping.rules, null, 2)}
                    onChange={(event) => {
                      try {
                        const rules = JSON.parse(event.target.value)
                        if (Array.isArray(rules)) {
                          setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, rules } }))
                        }
                      } catch {
                        // 保持输入态，保存前不会应用非法 JSON。
                      }
                    }}
                  />
                  <div className="text-xs text-base-content/60">当前规则 {draft.modelMapping.rules.length} 条；可生成默认规则 {defaultModelMappingRules.length} 条。</div>
                </div>
              </FieldLabel>
              <ToggleField title="提取 Thinking 内容块" description="非流式响应里是否把 <thinking> 标签解析成独立 thinking 内容块。" checked={draft.extractThinking} onChange={(extractThinking) => setDraft((prev) => ({ ...prev, extractThinking }))} />
              <ToggleField title="暴露代理改写告警" description="是否通过 x-kiro-rs-warnings 响应头展示代理侧动作，方便排查兼容问题。" checked={draft.exposeProxyWarnings} onChange={(exposeProxyWarnings) => setDraft((prev) => ({ ...prev, exposeProxyWarnings }))} />
            </ConfigGroup>

            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="后台统计" description="控制后台 usage 汇总的判断口径，只影响页面统计，不影响真实请求、缓存计算和费用估算。">
              <NumberField title="高缓存判定阈值" description="后台把一次请求统计为高缓存请求的 cache_read_input_tokens 门槛。" value={draft.highCacheThreshold} min={0} suffix="tokens" onChange={(highCacheThreshold) => setDraft((prev) => ({ ...prev, highCacheThreshold }))} />
            </ConfigGroup>
          </>
        )}

        <Alert status="info" className="py-2 text-sm">
          <Shield className="h-4 w-4" />
          <span>保存前会校验冷却、预热、缓存比例、放大倍数和触顶扣减范围；保存后新请求热加载生效。</span>
        </Alert>
      </div>
    </SectionCard>
  )
}
