import { useState } from 'react'
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
}

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

export function CredentialExportDialog({ open, onOpenChange }: CredentialExportDialogProps) {
  const [format, setFormat] = useState<CredentialExportFormat>('json')
  const [exporting, setExporting] = useState(false)

  const handleExport = async () => {
    setExporting(true)
    try {
      const blob = await exportCredentials(format)
      downloadBlob(blob, exportFilename(format))
      toast.success('凭据已导出')
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
