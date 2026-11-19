// 批量修改：只改勾选的项。提交后由页面逐个 PUT /accounts/{id}，只带这个账号真正变化的字段；
// overrides 在后端是整体替换，页面会先取最新的账号列表再逐个合并；取列表期间弹窗保持打开，取失败时填写内容不丢。
import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { getRouteScenarioOptions } from '@/api/route-scenarios'
import { Field, ModalShell } from '@/components/patterns'
import { TagsField } from '@/components/patterns/tags-field'
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui'
import { RouteSelector } from '@/features/route-scenarios/route-selector'
import { SupportedModelTagsEditor } from '@/features/shared/supported-model-tags'
import { useRuntimeConfig } from '@/hooks/use-runtime-config'
import {
  AUTO_DISABLE_POLICY_OPTIONS,
  RULE_MODE_OPTIONS,
  STREAM_RETRY_MODE_OPTIONS,
  USAGE_PROJECTION_MODE_OPTIONS,
  type EnumOption,
} from '@/lib/enum-labels'
import type { AutoDisablePolicy, RouteRuleMode, StreamRetryMode, UsageProjectionMode } from '@/types/api'
import {
  BATCH_PROXY_MASK_ERROR,
  MASKED_CREDENTIAL,
  countBatchEditFields,
  validateBatchEditDraft,
  type BatchEditDraft,
  type OverrideKey,
  type SupportedModelsEditMode,
} from './account-batch'
import { BatchField, FormSection, NumberInput, SectionToggle, SwitchCell } from './account-form-parts'
import { ACCOUNT_OVERRIDE_FIELDS } from './account-utils'

type FieldKey =
  | 'priority'
  | 'maxConcurrentRequests'
  | 'costMultiplier'
  | 'enabled'
  | 'usageProjectionMode'
  | 'autoDisablePolicy'
  | 'preOutputStreamRetryMode'
  | 'tags'
  | 'route'
  | 'supportedModels'
  | 'proxy'

const NO_FIELDS: Record<FieldKey, boolean> = {
  priority: false,
  maxConcurrentRequests: false,
  costMultiplier: false,
  enabled: false,
  usageProjectionMode: false,
  autoDisablePolicy: false,
  preOutputStreamRetryMode: false,
  tags: false,
  route: false,
  supportedModels: false,
  proxy: false,
}

const initialValues = () => ({
  priority: '',
  maxConcurrentRequests: '',
  costMultiplier: '',
  enabled: true,
  usageProjectionMode: 'current_path_policy' as UsageProjectionMode,
  autoDisablePolicy: 'inherit' as AutoDisablePolicy,
  preOutputStreamRetryMode: 'inherit' as StreamRetryMode,
  tagsAdd: [] as string[],
  tagsRemove: [] as string[],
  routeMode: 'allow_all' as RouteRuleMode,
  routeRules: [] as string[],
  modelsMode: 'replace' as SupportedModelsEditMode,
  models: [] as string[],
  proxyMode: 'set' as 'set' | 'clear',
  proxyUrl: '',
})

const MODELS_MODE_OPTIONS: Array<{ value: SupportedModelsEditMode; label: string }> = [
  { value: 'replace', label: '替换为下列模型' },
  { value: 'append', label: '追加下列模型' },
  { value: 'remove', label: '移除下列模型' },
  { value: 'clear', label: '清空（不限制）' },
]

function EnumSelect<T extends string>({ id, value, options, onChange }: { id: string; value: T; options: EnumOption[]; onChange: (value: T) => void }) {
  return (
    <Select value={value} onValueChange={(v) => onChange(v as T)}>
      <SelectTrigger id={id}><SelectValue /></SelectTrigger>
      <SelectContent>
        {options.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
      </SelectContent>
    </Select>
  )
}

export function AccountBatchEditModal({ open, count, tagOptions, submitting = false, onClose, onSubmit }: {
  open: boolean
  count: number
  tagOptions: string[]
  /** 页面正在取最新账号列表：禁用应用和关闭，避免关掉后又被执行。 */
  submitting?: boolean
  onClose: () => void
  onSubmit: (draft: BatchEditDraft) => void
}) {
  const runtime = useRuntimeConfig()
  const routes = useQuery({ queryKey: ['route-scenario-options'], queryFn: getRouteScenarioOptions, enabled: open })
  const [fields, setFields] = useState(NO_FIELDS)
  const [values, setValues] = useState(initialValues)
  const [overrideFields, setOverrideFields] = useState<Partial<Record<OverrideKey, boolean>>>({})
  const [overrideValues, setOverrideValues] = useState<Partial<Record<OverrideKey, string>>>({})

  useEffect(() => {
    if (!open) {
      setFields(NO_FIELDS)
      setValues(initialValues())
      setOverrideFields({})
      setOverrideValues({})
    }
  }, [open])

  const toggle = (key: FieldKey) => (enabled: boolean) => setFields((prev) => ({ ...prev, [key]: enabled }))
  const set = <K extends keyof ReturnType<typeof initialValues>>(key: K, value: ReturnType<typeof initialValues>[K]) =>
    setValues((prev) => ({ ...prev, [key]: value }))

  const build = (): BatchEditDraft | string => {
    const draft: BatchEditDraft = {}
    const number = (text: string, label: string) => {
      if (!text.trim()) return `请填写${label}`
      const value = Number(text)
      return Number.isFinite(value) ? value : `${label}不是数字`
    }
    for (const [key, label] of [['priority', '优先级'], ['maxConcurrentRequests', '单账号最大并发'], ['costMultiplier', '成本倍率']] as const) {
      if (!fields[key]) continue
      const value = number(values[key], label)
      if (typeof value === 'string') return value
      draft[key] = value
    }
    if (fields.enabled) draft.enabled = values.enabled
    if (fields.usageProjectionMode) draft.usageProjectionMode = values.usageProjectionMode
    if (fields.autoDisablePolicy) draft.autoDisablePolicy = values.autoDisablePolicy
    if (fields.preOutputStreamRetryMode) draft.preOutputStreamRetryMode = values.preOutputStreamRetryMode
    if (fields.tags) draft.tags = { add: values.tagsAdd, remove: values.tagsRemove }
    if (fields.route) draft.route = { routeMode: values.routeMode, routeRules: values.routeMode === 'allow_all' ? [] : values.routeRules }
    if (fields.supportedModels) draft.supportedModels = { mode: values.modelsMode, models: values.modelsMode === 'clear' ? [] : values.models }
    if (fields.proxy) draft.proxy = { mode: values.proxyMode, url: values.proxyUrl }
    const overrides: NonNullable<BatchEditDraft['overrides']> = {}
    for (const [key, label] of ACCOUNT_OVERRIDE_FIELDS) {
      if (!overrideFields[key]) continue
      const text = (overrideValues[key] ?? '').trim()
      if (key === 'usageProjectionCostFloorEnabled') {
        overrides[key] = text === 'true' ? true : text === 'false' ? false : null
        continue
      }
      if (!text) overrides[key] = null
      else {
        const value = Number(text)
        if (!Number.isInteger(value) || value < 0) return `${label}必须是非负整数，留空表示恢复全局`
        overrides[key] = value
      }
    }
    if (Object.keys(overrides).length > 0) draft.overrides = overrides
    return draft
  }

  const built = build()
  const preview = typeof built === 'string' ? null : countBatchEditFields(built)
  const tickedCount = Object.values(fields).filter(Boolean).length + Object.values(overrideFields).filter(Boolean).length

  const submit = () => {
    if (submitting) return
    const draft = build()
    if (typeof draft === 'string') { toast.error(draft); return }
    const error = validateBatchEditDraft(draft)
    if (error) { toast.error(error); return }
    if (countBatchEditFields(draft) === 0) { toast.error('请先勾选要修改的项'); return }
    onSubmit(draft)
  }

  const global = runtime.data?.accounts
  const proxyMasked = fields.proxy && values.proxyMode === 'set' && values.proxyUrl.includes(MASKED_CREDENTIAL)
  const close = () => { if (!submitting) onClose() }

  return (
    <ModalShell
      open={open}
      title={`批量修改（${count} 个账号）`}
      description="只修改勾选的项，没勾选的保持原样；每个账号只提交真正变化的字段，没有变化的会跳过。"
      width="max-w-4xl"
      onClose={close}
      footer={(
        <>
          <span className="mr-auto self-center text-xs text-muted-foreground">
            {submitting ? '正在读取最新账号…' : tickedCount > 0 ? `将修改 ${preview ?? tickedCount} 项` : '勾选要修改的项'}
          </span>
          <Button variant="outline" size="sm" onClick={close} disabled={submitting}>取消</Button>
          <Button size="sm" onClick={submit} disabled={tickedCount === 0 || submitting}>
            {submitting && <Loader2 className="animate-spin" />}应用修改
          </Button>
        </>
      )}
    >
      <div className="space-y-4">
        <FormSection title="调度" description="勾选左侧的框才会修改该项。">
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <BatchField id="batch-priority" label="优先级" description="数字越小越靠前；同优先级再按容量和状态分配" enabled={fields.priority} onEnabledChange={toggle('priority')}>
              <NumberInput value={values.priority} onChange={(v) => set('priority', v)} placeholder="100" />
            </BatchField>
            <BatchField id="batch-concurrency" label="单账号最大并发" description="同时处理的最大请求数，不是 RPM；1 到 10000" enabled={fields.maxConcurrentRequests} onEnabledChange={toggle('maxConcurrentRequests')}>
              <NumberInput value={values.maxConcurrentRequests} onChange={(v) => set('maxConcurrentRequests', v)} min={1} unit="个请求" placeholder="10" />
            </BatchField>
            <BatchField id="batch-cost" label="成本倍率" description="账号成本 = 上游原始成本 × 倍率，必须大于 0" enabled={fields.costMultiplier} onEnabledChange={toggle('costMultiplier')}>
              <NumberInput value={values.costMultiplier} onChange={(v) => set('costMultiplier', v)} step="0.01" min={0} unit="×" placeholder="1" />
            </BatchField>
            <BatchField id="batch-enabled" label="启用状态" description="走启停接口，会记一条手动启用 / 停用异动" enabled={fields.enabled} onEnabledChange={toggle('enabled')}>
              <SwitchCell checked={values.enabled} onChange={(v) => set('enabled', v)} onText="启用" offText="停用" />
            </BatchField>
            <BatchField id="batch-usage" label="下游 usage" enabled={fields.usageProjectionMode} onEnabledChange={toggle('usageProjectionMode')}>
              <EnumSelect id="batch-usage-select" value={values.usageProjectionMode} options={USAGE_PROJECTION_MODE_OPTIONS} onChange={(v) => set('usageProjectionMode', v)} />
            </BatchField>
            <BatchField id="batch-auto-disable" label="自动禁用策略" enabled={fields.autoDisablePolicy} onEnabledChange={toggle('autoDisablePolicy')}>
              <EnumSelect id="batch-auto-disable-select" value={values.autoDisablePolicy} options={AUTO_DISABLE_POLICY_OPTIONS} onChange={(v) => set('autoDisablePolicy', v)} />
            </BatchField>
            <BatchField id="batch-stream-retry" label="首输出前流式恢复" enabled={fields.preOutputStreamRetryMode} onEnabledChange={toggle('preOutputStreamRetryMode')}>
              <EnumSelect id="batch-stream-retry-select" value={values.preOutputStreamRetryMode} options={STREAM_RETRY_MODE_OPTIONS} onChange={(v) => set('preOutputStreamRetryMode', v)} />
            </BatchField>
          </div>
        </FormSection>

        <FormSection
          title={<SectionToggle id="batch-tags" checked={fields.tags} onChange={toggle('tags')}>标签</SectionToggle>}
          description="在每个账号现有标签上增删，不影响没提到的标签。"
        >
          {fields.tags ? (
            <div className="grid gap-4 md:grid-cols-2">
              <Field label="添加标签">
                <TagsField value={values.tagsAdd} options={tagOptions} onChange={(v) => set('tagsAdd', v)} emptyText="不添加" />
              </Field>
              <Field label="移除标签">
                <TagsField value={values.tagsRemove} options={tagOptions} onChange={(v) => set('tagsRemove', v)} emptyText="不移除" />
              </Field>
            </div>
          ) : <p className="text-xs text-muted-foreground">不修改</p>}
        </FormSection>

        <FormSection
          title={<SectionToggle id="batch-route" checked={fields.route} onChange={toggle('route')}>路由限制</SectionToggle>}
          description="从路由页里已有的入口中选择；会整体替换这些账号的路由规则。"
        >
          {fields.route ? (
            <div className="grid gap-4 md:grid-cols-[16rem_1fr]">
              <Field label="路由模式">
                <EnumSelect id="batch-route-mode" value={values.routeMode} options={RULE_MODE_OPTIONS} onChange={(v) => set('routeMode', v)} />
              </Field>
              <Field label="路由" description={values.routeMode === 'allow_all' ? '不限制时无需选择' : undefined}>
                <RouteSelector routes={routes.data?.routes ?? []} value={values.routeRules} disabled={values.routeMode === 'allow_all'} onChange={(v) => set('routeRules', v)} />
              </Field>
            </div>
          ) : <p className="text-xs text-muted-foreground">不修改</p>}
        </FormSection>

        <FormSection
          title={<SectionToggle id="batch-models" checked={fields.supportedModels} onChange={toggle('supportedModels')}>支持模型</SectionToggle>}
          description="只决定账号能否承接某些模型；空列表表示不限制。"
        >
          {fields.supportedModels ? (
            <div className="grid gap-4 md:grid-cols-[16rem_1fr]">
              <Field label="操作">
                <Select value={values.modelsMode} onValueChange={(v) => set('modelsMode', v as SupportedModelsEditMode)}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {MODELS_MODE_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
                  </SelectContent>
                </Select>
              </Field>
              <Field label="模型">
                {values.modelsMode === 'clear'
                  ? <p className="flex min-h-9 items-center text-xs text-muted-foreground">保存后这些账号可承接任意模型</p>
                  : <SupportedModelTagsEditor value={values.models} onChange={(v) => set('models', v)} />}
              </Field>
            </div>
          ) : <p className="text-xs text-muted-foreground">不修改</p>}
        </FormSection>

        <FormSection
          title={<SectionToggle id="batch-proxy" checked={fields.proxy} onChange={toggle('proxy')}>代理</SectionToggle>}
          description="清空会去掉账号自己的代理，改走全局代理或直连。"
        >
          {fields.proxy ? (
            <div className="grid gap-4 md:grid-cols-[16rem_1fr]">
              <Field label="操作">
                <Select value={values.proxyMode} onValueChange={(v) => set('proxyMode', v as 'set' | 'clear')}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="set">设置为下面的地址</SelectItem>
                    <SelectItem value="clear">清空（走全局代理或直连）</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
              <Field
                label="代理地址"
                description="支持 http、https、socks5、socks5h。批量修改不能用 *** 占位：*** 是单个账号已保存凭据的打码，每个账号原来的用户名和密码不同，需要填写完整地址。"
              >
                <div className="space-y-1">
                  <Input value={values.proxyUrl} disabled={values.proxyMode === 'clear'} placeholder="http://user:pass@host:port" autoComplete="off" aria-invalid={proxyMasked || undefined} onChange={(e) => set('proxyUrl', e.target.value)} />
                  {proxyMasked && <p className="text-xs text-destructive">{BATCH_PROXY_MASK_ERROR}</p>}
                </div>
              </Field>
            </div>
          ) : <p className="text-xs text-muted-foreground">不修改</p>}
        </FormSection>

        <FormSection title="覆盖全局参数" description="勾选后留空表示恢复继承全局；与账号现有的覆盖值合并，没勾选的覆盖值保持不变。">
          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {ACCOUNT_OVERRIDE_FIELDS.map(([key, label, unit]) => {
              const enabled = Boolean(overrideFields[key])
              const onEnabledChange = (v: boolean) => setOverrideFields((prev) => ({ ...prev, [key]: v }))
              const globalValue = global?.[key]
              if (key === 'usageProjectionCostFloorEnabled') {
                return (
                  <BatchField key={key} id={`batch-override-${key}`} label={label} enabled={enabled} onEnabledChange={onEnabledChange}>
                    <Select value={overrideValues[key] || 'inherit'} onValueChange={(v) => setOverrideValues((prev) => ({ ...prev, [key]: v }))}>
                      <SelectTrigger><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="inherit">继承全局（{globalValue ? '开启' : '关闭'}）</SelectItem>
                        <SelectItem value="true">开启</SelectItem>
                        <SelectItem value="false">关闭</SelectItem>
                      </SelectContent>
                    </Select>
                  </BatchField>
                )
              }
              return (
                <BatchField key={key} id={`batch-override-${key}`} label={label} enabled={enabled} onEnabledChange={onEnabledChange} description={`留空恢复全局 ${globalValue ?? '—'}${unit ? ` ${unit}` : ''}`}>
                  <NumberInput value={overrideValues[key] ?? ''} min={0} unit={unit} placeholder={String(globalValue ?? '')} onChange={(v) => setOverrideValues((prev) => ({ ...prev, [key]: v }))} />
                </BatchField>
              )
            })}
          </div>
        </FormSection>
      </div>
    </ModalShell>
  )
}
