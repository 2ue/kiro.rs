// 导出账号：从 GET /config/export 里挑出选中或当前筛选结果的账号，下载能被批量导入识别的 JSON 数组。
import { useEffect, useState } from 'react'
import { Download, Loader2 } from 'lucide-react'
import { toast } from 'sonner'
import { exportConfig } from '@/api/config-transfer'
import { Callout, Field, ModalShell, useConfirm } from '@/components/patterns'
import { Button, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import { accountExportFilename, exportHasRedactedSecrets, pickExportAccounts, type ExportScope } from './account-export'
import { SwitchCell } from './account-form-parts'

function download(content: string, filename: string) {
  const url = URL.createObjectURL(new Blob([content], { type: 'application/json' }))
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  link.click()
  URL.revokeObjectURL(url)
}

export function AccountExportModal({ open, onClose, selectedIds, filteredIds, totalCount, filtered }: {
  open: boolean
  onClose: () => void
  selectedIds: number[]
  filteredIds: number[]
  totalCount: number
  /** 当前列表有筛选条件。 */
  filtered: boolean
}) {
  const confirm = useConfirm()
  const [scope, setScope] = useState<ExportScope>('all')
  const [includeSecrets, setIncludeSecrets] = useState(false)
  const [loading, setLoading] = useState(false)

  useEffect(() => {
    if (open) {
      setScope(selectedIds.length > 0 ? 'selected' : filtered ? 'filtered' : 'all')
      setIncludeSecrets(false)
    }
  }, [open])

  const ids = scope === 'selected' ? selectedIds : scope === 'filtered' ? filteredIds : null
  const count = ids ? ids.length : totalCount

  const handleExport = async () => {
    if (includeSecrets) {
      const ok = await confirm({
        title: '导出包含明文密钥？',
        message: (
          <div className="space-y-1">
            <div>文件里会带上这 {count} 个账号的上游密钥明文，拿到文件的人可以直接用这些 Key。</div>
            <div>导出会在审计日志里记一条 export_config（includeSecrets=true）。请妥善保管文件，用完删除。</div>
          </div>
        ),
        confirmText: '确认导出',
        tone: 'danger',
      })
      if (!ok) return
    }
    setLoading(true)
    try {
      const pkg = await exportConfig(includeSecrets)
      const accounts = pickExportAccounts(pkg.accounts, ids)
      if (accounts.length === 0) {
        toast.error('没有可导出的账号')
        return
      }
      download(JSON.stringify(accounts, null, 2), accountExportFilename())
      const redacted = exportHasRedactedSecrets(accounts)
      toast.success(`已导出 ${accounts.length} 个账号${redacted ? '（密钥已脱敏，这个文件不能直接导入）' : '（含密钥）'}`)
      onClose()
    } catch (error) {
      toast.error(`导出失败：${extractErrorMessage(error)}`)
    } finally {
      setLoading(false)
    }
  }

  return (
    <ModalShell
      open={open}
      title="导出账号"
      width="max-w-md"
      onClose={() => { if (!loading) onClose() }}
      footer={(
        <>
          <Button variant="ghost" size="sm" onClick={onClose} disabled={loading}>取消</Button>
          <Button size="sm" onClick={() => void handleExport()} disabled={loading || count === 0}>
            {loading ? <Loader2 className="animate-spin" /> : <Download />}导出 {count} 个
          </Button>
        </>
      )}
    >
      <div className="space-y-4">
        <Field label="导出范围" description="导出的是账号配置（JSON 数组），不含运行状态；可以直接用“导入”批量导回。">
          <Select value={scope} onValueChange={(v) => setScope(v as ExportScope)} disabled={loading}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>
              {selectedIds.length > 0 && <SelectItem value="selected">选中账号（{selectedIds.length} 个）</SelectItem>}
              {filtered && <SelectItem value="filtered" disabled={filteredIds.length === 0}>当前筛选结果（{filteredIds.length} 个）</SelectItem>}
              <SelectItem value="all">全部账号（{totalCount} 个）</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="包含密钥" description="打开后文件里是密钥明文，导出前会再确认一次，并写入审计日志。">
          <SwitchCell checked={includeSecrets} onChange={setIncludeSecrets} onText="包含明文密钥" offText="不包含（密钥写成 <redacted>）" disabled={loading} />
        </Field>
        {!includeSecrets && (
          <Callout tone="info">不带密钥导出的文件只能查看配置，不能再导入：导入时密钥为 &lt;redacted&gt; 的条目会标为格式错误。代理地址里的用户名密码同样会脱敏。</Callout>
        )}
      </div>
    </ModalShell>
  )
}
