import { useRef, useState } from 'react'
import { FileUp, Upload, X } from 'lucide-react'
import { ModalShell } from '@/components/patterns'
import { Button, Textarea } from '@/components/ui'
import { parseRoutePolicyImport, ROUTE_POLICY_MAX_BYTES, type RoutePolicyImport } from './route-policy-transfer'

type ImportedPolicy = Extract<RoutePolicyImport, { ok: true }>

export function RoutePolicyImportDialog({
  onClose,
  onImport,
}: {
  onClose: () => void
  onImport: (imported: ImportedPolicy) => void
}) {
  const [text, setText] = useState('')
  const [fileName, setFileName] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)

  const readFile = async (file: File) => {
    if (file.size > ROUTE_POLICY_MAX_BYTES) {
      setError('文件超过 1 MB，不是有效的路由策略文件')
      return
    }
    try {
      setText(await file.text())
      setFileName(file.name)
      setError(null)
    } catch (err) {
      setError(`读取文件失败：${err instanceof Error ? err.message : String(err)}`)
    }
  }

  const confirm = () => {
    const parsed = parseRoutePolicyImport(text)
    if (!parsed.ok) {
      setError(parsed.error)
      return
    }
    onImport(parsed)
  }

  return (
    <ModalShell
      open
      title="导入路由策略"
      description="粘贴“导出”得到的 JSON，或选择 JSON 文件。确认后会打开新增弹窗并填好导入的内容，路由需要重新填写。"
      width="max-w-2xl"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}><X />取消</Button>
          <Button onClick={confirm} disabled={!text.trim()}><Upload />下一步</Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <Button type="button" variant="outline" size="sm" onClick={() => fileInput.current?.click()}>
            <FileUp />
            选择 JSON 文件
          </Button>
          <span className="text-xs text-muted-foreground">{fileName ? `已读取：${fileName}` : '也可以直接把 JSON 粘贴到下面'}</span>
          <input
            ref={fileInput}
            type="file"
            accept=".json,application/json"
            className="hidden"
            aria-label="选择路由策略 JSON 文件"
            onChange={(event) => {
              const file = event.target.files?.[0]
              if (file) void readFile(file)
              event.target.value = ''
            }}
          />
        </div>
        <label className="block text-sm font-medium" htmlFor="route-policy-import-text">策略 JSON</label>
        <Textarea
          id="route-policy-import-text"
          className="min-h-72 font-mono text-xs"
          placeholder={'{\n  "format": "kiro-rs-route-policy",\n  "version": 2,\n  "cachePolicy": { "cacheType": "current_high_cache" },\n  "promptInjection": {},\n  "features": {}\n}'}
          value={text}
          aria-invalid={Boolean(error)}
          aria-describedby={error ? 'route-policy-import-error' : undefined}
          onChange={(event) => {
            setText(event.target.value)
            setFileName(null)
            setError(null)
          }}
        />
        {error && <p id="route-policy-import-error" role="alert" className="text-xs text-destructive">{error}</p>}
      </div>
    </ModalShell>
  )
}
