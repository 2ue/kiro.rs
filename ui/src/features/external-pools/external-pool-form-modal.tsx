import { useEffect, useMemo, useState } from 'react'
import { Loader2, Plus, Save } from 'lucide-react'
import { toast } from 'sonner'
import type { ExternalPool } from '@/types/api'
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
  mode, pool, open, draft, saving, onDraftChange, onClose, onSubmit,
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
  const keyDescription = isEdit
    ? `留空表示不修改当前 Key。当前：${pool?.maskedApiKey || '未显示 Key'}`
    : '外部账号的请求密钥，保存后只显示脱敏值。'
  const [quickImportText, setQuickImportText] = useState('')
  const mappingPresets = useMemo(() => modelMappingPresetsForMode(draft.modelMappingMode), [draft.modelMappingMode])

  useEffect(() => { if (!open) setQuickImportText('') }, [open])

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
              <ToggleRow label="模型版本号格式转换（例 4.5 → 4-5）" checked={Boolean(draft.normalizeModelVersionDots)} disabled={saving || draft.modelMappingMode === 'passthrough' || draft.modelMappingRequireMatch} onChange={(v) => set('normalizeModelVersionDots', v)} />
            </div>
          </FormSection>

          <FormSection title="用量与成本" description="只控制当前外部账号返回给客户端的用量展示方式。">
            <div className="space-y-3">
              <SelectBox label="用量展示模式" value={draft.usageProjectionMode} disabled={saving} onChange={(v) => set('usageProjectionMode', v as ExternalPoolFormDraft['usageProjectionMode'])}>
                <SelectItem value="pass_through">保持原样：不改外部账号用量</SelectItem>
                <SelectItem value="current_path_policy">按入口规则展示：应用全局补偿</SelectItem>
              </SelectBox>
              <HintBox>{usageProjectionDescription(draft.usageProjectionMode)}</HintBox>
            </div>
          </FormSection>
        </div>

        <FormSection title="模型处理" description="控制当前外部账号发出请求时的模型名称处理方式。">
          <div className="grid gap-3 md:grid-cols-[240px_1fr]">
            <div className="space-y-3">
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
