import { useEffect, useState } from 'react'
import { Download } from 'lucide-react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { exportCredentials } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { CredentialExportFormat } from '@/types/api'

interface CredentialExportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  selectedIds?: number[]
  totalCount: number
  disabledCount: number
}

type ExportScope = 'selected' | 'all' | 'disabled'

const formats: Array<{ value: CredentialExportFormat; label: string; description: string }> = [
  {
    value: 'json',
    label: 'JSON 数组',
    description: '导出为可直接批量导入的凭据数组。',
  },
  {
    value: 'backup-json',
    label: '备份 JSON',
    description: '带导出时间和格式标识，适合归档。',
  },
  {
    value: 'jsonl',
    label: 'JSONL',
    description: '每行一个凭据，便于脚本处理。',
  },
]

function exportFilename(format: CredentialExportFormat): string {
  const stamp = new Date().toISOString().replace(/[:.]/g, '-')
  return `kiro-credentials-${stamp}.${format === 'jsonl' ? 'jsonl' : 'json'}`
}

function downloadBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  document.body.appendChild(anchor)
  anchor.click()
  anchor.remove()
  URL.revokeObjectURL(url)
}

export function CredentialExportDialog({
  open,
  onOpenChange,
  selectedIds = [],
  totalCount,
  disabledCount,
}: CredentialExportDialogProps) {
  const [format, setFormat] = useState<CredentialExportFormat>('json')
  const [scope, setScope] = useState<ExportScope>('all')
  const [exporting, setExporting] = useState(false)
  const selectedCount = selectedIds.length
  const scopes: Array<{ value: ExportScope; label: string; count: number; disabled: boolean }> = [
    { value: 'selected', label: '选中凭据', count: selectedCount, disabled: selectedCount === 0 },
    { value: 'all', label: '全部凭据', count: totalCount, disabled: false },
    { value: 'disabled', label: '全部已禁用', count: disabledCount, disabled: disabledCount === 0 },
  ]

  useEffect(() => {
    if (open) setScope(selectedCount > 0 ? 'selected' : 'all')
  }, [open, selectedCount])

  const handleExport = async () => {
    setExporting(true)
    try {
      const blob = await exportCredentials(format, scope === 'selected' ? selectedIds : undefined, scope === 'disabled')
      downloadBlob(blob, exportFilename(format))
      toast.success(scope === 'selected' ? `已导出选中凭据 ${selectedCount} 个` : scope === 'disabled' ? '已导出全部已禁用凭据' : '凭据已导出')
      onOpenChange(false)
    } catch (error) {
      toast.error(`导出失败: ${extractErrorMessage(error)}`)
    } finally {
      setExporting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>导出凭据</DialogTitle>
          <DialogDescription>
            导出内容包含完整 refreshToken、kiroApiKey、代理等敏感字段。
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          {formats.map((item) => (
            <button
              key={item.value}
              type="button"
              className={`w-full rounded-md border p-3 text-left transition-colors ${
                format === item.value ? 'border-primary bg-primary/5' : 'hover:bg-muted'
              }`}
              onClick={() => setFormat(item.value)}
            >
              <div className="font-medium">{item.label}</div>
              <div className="text-sm text-muted-foreground">{item.description}</div>
            </button>
          ))}
          <div className="space-y-2 pt-2">
            <div className="text-sm font-medium">导出范围</div>
            <div className="grid gap-2 sm:grid-cols-3">
              {scopes.map((item) => (
                <button
                  key={item.value}
                  type="button"
                  disabled={item.disabled}
                  className={`rounded-md border p-3 text-left text-sm transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${
                    scope === item.value ? 'border-primary bg-primary/5' : 'hover:bg-muted'
                  }`}
                  onClick={() => setScope(item.value)}
                >
                  <div className="font-medium">{item.label}</div>
                  <div className="text-muted-foreground">{item.count} 个</div>
                </button>
              ))}
            </div>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={exporting}>
            取消
          </Button>
          <Button onClick={handleExport} disabled={exporting}>
            <Download className="h-4 w-4" />
            {exporting ? '导出中...' : '导出'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
