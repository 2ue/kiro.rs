import { useState } from 'react'
import { ModalShell } from '@/components/patterns'
import { Button } from '@/components/ui'
import { updateExternalPool } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { ModelFilterMode } from '@/types/api'
import { SupportedModelTagsEditor } from '@/features/shared/supported-model-tags'

export function ExternalPoolModelBatchModal({ ids, onClose, onSaved }: {
  ids: number[]
  onClose: () => void
  onSaved: (succeeded: number[]) => void
}) {
  const [mode, setMode] = useState<ModelFilterMode>('allowlist')
  const [models, setModels] = useState<string[]>([])
  const [saving, setSaving] = useState(false)
  const [failures, setFailures] = useState<{ id: number; message: string }[]>([])
  const save = async () => {
    setSaving(true)
    setFailures([])
    const failed: { id: number; message: string }[] = []
    const succeeded: number[] = []
    for (let offset = 0; offset < ids.length; offset += 4) {
      await Promise.all(ids.slice(offset, offset + 4).map(async (id) => {
        try {
          await updateExternalPool(id, { modelFilterMode: mode, supportedModels: models })
          succeeded.push(id)
        } catch (error) {
          failed.push({ id, message: extractErrorMessage(error) })
        }
      }))
    }
    setSaving(false)
    setFailures(failed)
    onSaved(succeeded)
    if (!failed.length) onClose()
  }
  return (
    <ModalShell open title={`批量模型筛选（${ids.length} 个外部账号）`} onClose={() => { if (!saving) onClose() }} width="max-w-xl"
      footer={<><Button variant="ghost" disabled={saving} onClick={onClose}>取消</Button><Button disabled={saving || !ids.length} onClick={() => void save()}>{saving ? '保存中…' : '应用到选中账号'}</Button></>}>
      <div className="space-y-3">
        <p className="text-sm text-muted-foreground">用以下模式和名单替换选中账号的模型筛选。空白名单或空黑名单均不限制外部账号的模型。</p>
        <SupportedModelTagsEditor value={models} onChange={setModels} mode={mode} onModeChange={setMode} disabled={saving} />
        {failures.map((failure) => <p key={failure.id} className="text-sm text-destructive">#{failure.id}：{failure.message}</p>)}
      </div>
    </ModalShell>
  )
}
