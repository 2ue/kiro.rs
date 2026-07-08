import { useEffect, useMemo, useState } from 'react'
import { Loader2, Plus, Save } from 'lucide-react'
import { toast } from 'sonner'
import type { CredentialListItem, ExternalPool } from '@/types/api'
import { ModalShell } from '@/components/patterns'
import { Button } from '@/components/ui'
import { SelectItem } from '@/components/ui'
import { cn } from '@/lib/utils'
import {
  type ExternalPoolFormDraft,
  type ExternalPoolModelMappingPreset,
  modelMappingDescription,
  modelMappingPresetsForMode,
  modelMappingPresetClass,
  appendModelMappingPreset,
  appendModelMappingPresets,
  appendModelMappingRules,
  parseModelMappingRules,
  requestBodyModeDescription,
  streamResponseDescription,
  usageProjectionDescription,
} from './external-pool-utils'
import {
  FormSection,
  HintBox,
  NumberBox,
  SelectBox,
  TextAreaBox,
  TextBox,
  ToggleRow,
} from './external-pool-components'

const NO_SYNC_CREDENTIAL = '__select_credential__'

function ModelMappingPresetTags({ presets, disabled, onSelect }: {
  presets: ExternalPoolModelMappingPreset[]; disabled?: boolean; onSelect: (p: ExternalPoolModelMappingPreset) => void
}) {
  if (!presets.length) return null
  return (
    <div className="flex flex-wrap gap-2">
      {presets.map((preset) => (
        <button
          key={`${preset.source}->${preset.target}`}
          type="button"
          className={cn('rounded-lg px-3 py-1 text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-50', modelMappingPresetClass(preset.tone))}
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

// --- Main modal ---

export function ExternalPoolFormModal({
  mode, pool, open, draft, saving, credentialOptions = [], onDraftChange, onClose, onSubmit, onSyncSupportedModels,
}: {
  mode: 'create' | 'edit'
  pool?: ExternalPool | null
  open: boolean
  draft: ExternalPoolFormDraft
  saving: boolean
  credentialOptions?: CredentialListItem[]
  onDraftChange: (value: ExternalPoolFormDraft | ((prev: ExternalPoolFormDraft) => ExternalPoolFormDraft)) => void
  onClose: () => void
  onSubmit: () => void
  onSyncSupportedModels?: (credentialId: number) => Promise<string[]>
}) {
  const isEdit = mode === 'edit'
  const title = isEdit ? `编辑外部账号${pool ? ` #${pool.id}` : ''}` : '添加外部账号'
  const keyLabel = isEdit ? '新请求 Key' : '请求 Key'
  const keyDescription = isEdit
    ? `留空表示不修改当前 Key。当前：${pool?.maskedApiKey || '未显示 Key'}`
    : '外部账号的请求密钥，保存后只显示脱敏值。'
  const [quickImportText, setQuickImportText] = useState('')
  const [syncCredentialId, setSyncCredentialId] = useState(NO_SYNC_CREDENTIAL)
  const [syncingModels, setSyncingModels] = useState(false)
  const mappingPresets = useMemo(() => modelMappingPresetsForMode(draft.modelMappingMode), [draft.modelMappingMode])

  useEffect(() => {
    if (!open) {
      setQuickImportText('')
      setSyncCredentialId(NO_SYNC_CREDENTIAL)
    }
  }, [open])

  const set = <K extends keyof ExternalPoolFormDraft>(key: K, value: ExternalPoolFormDraft[K]) =>
    onDraftChange((prev) => ({ ...prev, [key]: value }))

  const addMappingPreset = (preset: ExternalPoolModelMappingPreset) => {
    const result = appendModelMappingPreset(draft.modelMappingRulesText, preset)
    onDraftChange((prev) => ({ ...prev, modelMappingRulesText: result.text }))
    if (result.added) toast.success('模型映射规则已添加')
    else toast.info('该模型映射规则已存在')
  }

  const addAllMappingPresets = () => {
    const result = appendModelMappingPresets(draft.modelMappingRulesText, mappingPresets)
    onDraftChange((prev) => ({ ...prev, modelMappingRulesText: result.text }))
    if (result.added > 0) toast.success(`已添加 ${result.added} 条模型映射规则`)
    else toast.info('快捷模型映射规则都已存在')
  }

  const importMappingRules = () => {
    const rules = parseModelMappingRules(quickImportText)
    if (!rules.length) { toast.error('没有可导入的模型映射规则'); return }
    const result = appendModelMappingRules(draft.modelMappingRulesText, rules)
    onDraftChange((prev) => ({ ...prev, modelMappingRulesText: result.text }))
    if (result.added > 0) { toast.success(`已导入 ${result.added} 条模型映射规则`); setQuickImportText('') }
    else toast.info('导入的模型映射规则都已存在')
  }

  const syncSupportedModels = async () => {
    if (!onSyncSupportedModels) return
    const credentialId = Number(syncCredentialId)
    if (!Number.isInteger(credentialId) || credentialId <= 0) {
      toast.error('请选择要同步的本地账号')
      return
    }
    setSyncingModels(true)
    try {
      const supportedModels = await onSyncSupportedModels(credentialId)
      onDraftChange((prev) => ({ ...prev, supportedModelsText: supportedModels.join('\n') }))
      toast.success(`已同步 ${supportedModels.length} 个支持模型`)
    } catch (error) {
      toast.error(error instanceof Error ? error.message : '同步失败')
    } finally {
      setSyncingModels(false)
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
          <Button type="button" variant="ghost" size="sm" onClick={onClose} disabled={saving}>取消</Button>
          <Button type="button" size="sm" onClick={onSubmit} disabled={saving}>
            {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : isEdit ? <Save className="h-4 w-4" /> : <Plus className="h-4 w-4" />}
            {isEdit ? '保存外部账号' : '添加外部账号'}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <FormSection title="连接信息" description="系统会使用这里的服务地址和 Key 连接外部账号。">
          <div className="grid gap-3 md:grid-cols-2">
            <TextBox label="名称" value={draft.name} disabled={saving} onChange={(v) => set('name', v)} />
            <SelectBox label="认证方式" value={draft.authType} disabled={saving} onChange={(v) => set('authType', v as ExternalPoolFormDraft['authType'])}>
              <SelectItem value="bearer">Authorization: Bearer &lt;key&gt;</SelectItem>
              <SelectItem value="x_api_key">x-api-key: &lt;key&gt;</SelectItem>
            </SelectBox>
            <TextBox className="md:col-span-2" label="服务地址" description="填写服务地址即可，通常不需要带具体请求路径。" value={draft.baseUrl} disabled={saving} onChange={(v) => set('baseUrl', v)} />
            <TextBox className="md:col-span-2" label={keyLabel} description={keyDescription} value={draft.apiKey} disabled={saving} onChange={(v) => set('apiKey', v)} />
          </div>
        </FormSection>

        <div className="grid gap-4 lg:grid-cols-2">
          <FormSection title="调度设置" description="这些设置只影响当前外部账号，不改变全局排队和冷却策略。">
            <div className="grid gap-3 sm:grid-cols-2">
              <NumberBox label="单账号最大并发" description="当前外部账号同时处理的最大请求数。" suffix="并发" value={draft.maxConcurrentRequests} min={1} disabled={saving} onChange={(v) => set('maxConcurrentRequests', v)} />
              <NumberBox label="优先级" description="数字越小越靠前；同优先级再按容量和状态分配。" suffix="值" value={draft.priority} disabled={saving} onChange={(v) => set('priority', v)} />
              <ToggleRow label={isEdit ? '启用外部账号' : '创建后立即启用'} checked={Boolean(draft.enabled)} disabled={saving} onChange={(v) => set('enabled', v)} />
              <ToggleRow label="保留请求路径" checked={Boolean(draft.preservePath)} disabled={saving} onChange={(v) => set('preservePath', v)} />
            </div>
            <div className="mt-2 text-xs leading-relaxed text-muted-foreground">
              保留请求路径：转发时保留原始请求路径（默认开启）。关闭后只转发到服务根地址，适合不需要路径透传的场景。
            </div>
          </FormSection>

          <FormSection title="Usage 投影" description="只决定当前外部账号的 usage 是否按入口缓存策略改写；非 usage 内容不受影响。">
            <div className="space-y-3">
              <SelectBox label="Usage 投影策略" value={draft.usageProjectionMode} disabled={saving} onChange={(v) => set('usageProjectionMode', v as ExternalPoolFormDraft['usageProjectionMode'])}>
                <SelectItem value="pass_through">上游原样：不改 usage</SelectItem>
                <SelectItem value="current_path_policy">按入口策略投影：应用全局补偿</SelectItem>
              </SelectBox>
              <HintBox>{usageProjectionDescription(draft.usageProjectionMode)}</HintBox>
              <SelectBox
                label="流式响应 Usage 返回"
                value={draft.streamResponseMode}
                disabled={saving}
                onChange={(v) => set('streamResponseMode', v as ExternalPoolFormDraft['streamResponseMode'])}
              >
                <SelectItem value="inherit">继承全局默认</SelectItem>
                <SelectItem value="event_passthrough_usage_rewrite">事件透传，usage 按入口投影</SelectItem>
                <SelectItem value="event_passthrough_capture">事件完全透传，仅内部计量</SelectItem>
              </SelectBox>
              <HintBox>{streamResponseDescription(draft.streamResponseMode)}</HintBox>
            </div>
          </FormSection>
        </div>

        <FormSection title="调度资格" description="只决定该外部账号是否允许承接某些模型；不改变请求体里的 model，也不影响模型映射规则。">
          <div className="grid gap-3 md:grid-cols-[1fr_220px]">
            <TextAreaBox
              label="支持模型"
              description="空列表表示不限制；非空时，请求模型必须命中这里的列表才会调度到该外部账号。"
              value={draft.supportedModelsText}
              disabled={saving || syncingModels}
              onChange={(v) => set('supportedModelsText', v)}
            />
            <div className="space-y-2">
              <SelectBox
                label="从本地账号同步"
                value={syncCredentialId}
                disabled={saving || syncingModels || !isEdit || !onSyncSupportedModels}
                onChange={setSyncCredentialId}
              >
                <SelectItem value={NO_SYNC_CREDENTIAL}>选择账号</SelectItem>
                {credentialOptions.map((credential) => (
                  <SelectItem key={credential.id} value={String(credential.id)}>
                    #{credential.id} {credential.email || credential.maskedApiKey || credential.authMethod || '账号'}
                  </SelectItem>
                ))}
              </SelectBox>
              <Button type="button" variant="outline" size="sm" className="w-full" disabled={saving || syncingModels || !isEdit || syncCredentialId === NO_SYNC_CREDENTIAL || !onSyncSupportedModels} onClick={syncSupportedModels}>
                {syncingModels && <Loader2 className="h-4 w-4 animate-spin" />}同步支持模型
              </Button>
              {!isEdit && <div className="text-xs text-muted-foreground">创建后编辑外部账号可从本地账号同步。</div>}
            </div>
          </div>
        </FormSection>

        <FormSection title="请求体处理" description="控制发往该外部账号前是否进入本系统 body 处理链路。">
          <div className="grid gap-3">
            <div className="space-y-3">
              <SelectBox
                label="Body 模式"
                value={draft.requestBodyMode}
                disabled={saving}
                onChange={(v) => set('requestBodyMode', v as ExternalPoolFormDraft['requestBodyMode'])}
              >
                <SelectItem value="normalized">标准处理</SelectItem>
                <SelectItem value="raw_passthrough">Raw 透传</SelectItem>
              </SelectBox>
              <HintBox>{requestBodyModeDescription(draft.requestBodyMode)}</HintBox>
            </div>
          </div>
        </FormSection>

        <FormSection title="模型处理" description="控制当前外部账号发出请求时的模型名称处理方式。">
          <div className="grid gap-3 md:grid-cols-[240px_1fr]">
            <div className="space-y-3">
              {draft.requestBodyMode === 'raw_passthrough' && (
                <>
                  <ToggleRow
                    label="写回顶层 model"
                    checked={draft.rawModelMode === 'rewrite_top_level'}
                    disabled={saving}
                    onChange={(v) => set('rawModelMode', v ? 'rewrite_top_level' : 'none')}
                  />
                  <HintBox>
                    开启后只扫描 raw JSON 顶层 model，按本区域模型处理规则得到目标模型并写回顶层 model；关闭则 body 和 model 都原样透传。
                  </HintBox>
                </>
              )}
              <SelectBox label="映射模式" value={draft.modelMappingMode} disabled={saving} onChange={(v) => set('modelMappingMode', v as ExternalPoolFormDraft['modelMappingMode'])}>
                <SelectItem value="passthrough">直接使用请求模型</SelectItem>
                <SelectItem value="passthrough_mapping">请求模型优先映射</SelectItem>
                <SelectItem value="direct_mapping">映射后内部处理</SelectItem>
                <SelectItem value="processed_mapping">内部处理后映射</SelectItem>
              </SelectBox>
              <HintBox>
                {modelMappingDescription(draft.modelMappingMode, draft.normalizeModelVersionDots)}
                {draft.normalizeModelVersionDots && draft.modelMappingMode !== 'passthrough' && (
                  <div className="mt-1">未命中映射规则时，自动将版本号点号替换为横杠。</div>
                )}
              </HintBox>
              <ToggleRow label="未命中时点号转横杠" checked={Boolean(draft.normalizeModelVersionDots)} disabled={saving || draft.modelMappingMode === 'passthrough' || draft.modelMappingRequireMatch} onChange={(v) => set('normalizeModelVersionDots', v)} />
              {draft.modelMappingMode !== 'passthrough' && (
                <ToggleRow label="必须命中映射" checked={Boolean(draft.modelMappingRequireMatch)} disabled={saving} onChange={(v) => set('modelMappingRequireMatch', v)} />
              )}
            </div>
            {draft.modelMappingMode !== 'passthrough' && (
              <div className="space-y-3">
                <TextAreaBox
                  label="映射规则"
                  description="每行一条：claude-sonnet-4-5-20250929 -> claude-sonnet-4.5"
                  value={draft.modelMappingRulesText}
                  disabled={saving}
                  action={<Button type="button" variant="ghost" size="xs" onClick={addAllMappingPresets} disabled={saving || !mappingPresets.length}>全部添加</Button>}
                  onChange={(v) => set('modelMappingRulesText', v)}
                />
                <ModelMappingPresetTags presets={mappingPresets} disabled={saving} onSelect={addMappingPreset} />
                <TextAreaBox
                  label="快捷导入"
                  description="粘贴多行 source -> target，点击解析导入后追加到上方规则。"
                  value={quickImportText}
                  disabled={saving}
                  action={<Button type="button" variant="ghost" size="xs" onClick={importMappingRules} disabled={saving || !quickImportText.trim()}>解析导入</Button>}
                  onChange={setQuickImportText}
                />
              </div>
            )}
          </div>
        </FormSection>

        <FormSection title="错误处理和备注" description="自动禁用策略只决定当前外部账号是否继承全局自动禁用规则。">
          <div className="grid gap-3 md:grid-cols-2">
            <SelectBox label="自动禁用策略" value={draft.autoDisablePolicy} disabled={saving} onChange={(v) => set('autoDisablePolicy', v as ExternalPoolFormDraft['autoDisablePolicy'])}>
              <SelectItem value="inherit">继承全局自动禁用</SelectItem>
              <SelectItem value="enabled">单独启用自动禁用</SelectItem>
              <SelectItem value="disabled">关闭自动禁用</SelectItem>
            </SelectBox>
            <TextBox label="备注" value={draft.notes} disabled={saving} onChange={(v) => set('notes', v)} />
          </div>
        </FormSection>

        {!isEdit && !draft.enabled && (
          <HintBox>当前选择为创建后不立即启用。保存后可以先在列表里测试连接，再手动启用参与调度。</HintBox>
        )}
      </div>
    </ModalShell>
  )
}
