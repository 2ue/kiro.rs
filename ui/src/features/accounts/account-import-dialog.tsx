// 批量导入账号：粘贴或选文件 → 识别格式 → 统一默认参数 → 预览（新建 / 跳过 / 格式错误）→ 逐条 POST /accounts。
// 交互照 main 的 credential-import-dialog；密钥在预览和结果里只显示打码值。
import { useEffect, useMemo, useRef, useState, type ChangeEvent } from 'react'
import { FileUp, Play, RotateCcw, Square, X } from 'lucide-react'
import { toast } from 'sonner'
import { createAccount } from '@/api/accounts'
import { Callout, Field, ModalShell } from '@/components/patterns'
import { TagsField } from '@/components/patterns/tags-field'
import {
  Badge,
  Button,
  Progress,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Textarea,
} from '@/components/ui'
import { SupportedModelTagsEditor } from '@/features/shared/supported-model-tags'
import { ACCOUNT_AUTH_TYPE_OPTIONS, USAGE_PROJECTION_MODE_OPTIONS } from '@/lib/enum-labels'
import { extractErrorMessage } from '@/lib/utils'
import type { Account, AccountAuthType, UsageProjectionMode } from '@/types/api'
import { BATCH_CONCURRENCY, runWithConcurrency, type BatchRowStatus } from './account-batch'
import { BatchStatusCell } from './account-batch-run'
import { FormSection, NumberInput, SwitchCell } from './account-form-parts'
import {
  IMPORT_FORMAT_LABELS,
  buildImportPlan,
  countImportRows,
  initialImportDefaults,
  parseImportText,
  validateImportDefaults,
  type ImportDefaults,
  type ImportPlanRow,
} from './account-import'

const PASTE_INPUT_NAME = '粘贴内容'
const PLACEHOLDER = [
  '两种格式任选：',
  '1. 每行一条：服务地址 密钥 [名称]，用空格、| 或逗号分隔，# 开头的行忽略',
  '   https://api.example.com sk-xxxx 主力账号',
  '2. JSON 数组（字段与创建账号接口一致），也可以直接粘贴配置迁移包或账号页导出的文件',
].join('\n')

interface RunRow extends ImportPlanRow {
  run?: BatchRowStatus
  createdId?: number
  error?: string
}

const STATE_VIEW: Record<ImportPlanRow['state'], { label: string; tone: 'success' | 'warning' | 'error' | 'neutral' }> = {
  new: { label: '将新建', tone: 'success' },
  duplicate: { label: '跳过：重复', tone: 'warning' },
  exists: { label: '跳过：已存在', tone: 'warning' },
  invalid: { label: '格式错误', tone: 'error' },
}

export function AccountImportModal({ open, onClose, existingAccounts, tagOptions, onDone }: {
  open: boolean
  onClose: () => void
  existingAccounts: Account[]
  tagOptions: string[]
  onDone: () => void
}) {
  const [text, setText] = useState('')
  const [files, setFiles] = useState<Array<{ name: string; text: string }>>([])
  const [defaults, setDefaults] = useState<ImportDefaults>(initialImportDefaults)
  const [rows, setRows] = useState<RunRow[] | null>(null)
  const [running, setRunning] = useState(false)
  const [stopping, setStopping] = useState(false)
  const [formError, setFormError] = useState('')
  const stopRef = useRef(false)

  useEffect(() => {
    if (!open) {
      setText('')
      setFiles([])
      setDefaults(initialImportDefaults())
      setRows(null)
      setRunning(false)
      setStopping(false)
      setFormError('')
    }
  }, [open])

  const inputs = useMemo(() => {
    const pasted = text.trim() ? [{ name: PASTE_INPUT_NAME, text }] : []
    return [...files, ...pasted].map((input) => ({ name: input.name, result: parseImportText(input.text) }))
  }, [files, text])
  const parsedCount = inputs.reduce((sum, input) => sum + input.result.entries.length, 0)

  const setDefault = <K extends keyof ImportDefaults>(key: K, value: ImportDefaults[K]) => setDefaults((prev) => ({ ...prev, [key]: value }))

  const handleFiles = async (event: ChangeEvent<HTMLInputElement>) => {
    const selected = Array.from(event.target.files ?? [])
    event.target.value = ''
    if (!selected.length) return
    const read: Array<{ name: string; text: string }> = []
    const failed: string[] = []
    for (const file of selected) {
      try {
        read.push({ name: file.name, text: await file.text() })
      } catch (error) {
        failed.push(`${file.name}：${extractErrorMessage(error)}`)
      }
    }
    setFiles((prev) => [...prev.filter((file) => !read.some((item) => item.name === file.name)), ...read])
    if (failed.length) toast.warning(`部分文件没读到：${failed.slice(0, 3).join('；')}`)
  }

  const preview = () => {
    setFormError('')
    const error = validateImportDefaults(defaults)
    if (error) { setFormError(error); return }
    const broken = inputs.filter((input) => input.result.error)
    if (broken.length) { setFormError(broken.map((input) => `${input.name}：${input.result.error}`).join('；')); return }
    if (!parsedCount) { setFormError('没有识别到账号，请检查内容格式'); return }
    setRows(buildImportPlan(inputs.map((input) => ({ origin: input.name, entries: input.result.entries })), defaults, existingAccounts))
  }

  const runImport = async (indices: number[]) => {
    if (!rows || !indices.length) return
    const snapshot = rows
    const update = (index: number, patch: Partial<RunRow>) => setRows((prev) => prev && prev.map((row, i) => (i === index ? { ...row, ...patch } : row)))
    const outcomes: BatchRowStatus[] = []
    stopRef.current = false
    setStopping(false)
    setRunning(true)
    const { stopped } = await runWithConcurrency(indices, async (index) => {
      const row = snapshot[index]
      if (!row.request) return
      update(index, { run: 'running', error: undefined })
      try {
        const created = await createAccount(row.request)
        update(index, { run: 'success', createdId: created.id })
        outcomes.push('success')
      } catch (error) {
        update(index, { run: 'failed', error: extractErrorMessage(error) })
        outcomes.push('failed')
      }
    }, { concurrency: BATCH_CONCURRENCY, shouldStop: () => stopRef.current })
    setRunning(false)
    setStopping(false)
    onDone()
    const success = outcomes.filter((item) => item === 'success').length
    const failed = outcomes.length - success
    const message = `导入${stopped ? '已停止' : '完成'}：成功 ${success}，失败 ${failed}`
    if (failed || stopped) toast.warning(message)
    else toast.success(message)
  }

  const counts = rows ? countImportRows(rows) : null
  const pendingIndices = rows?.flatMap((row, index) => (row.state === 'new' && !row.run ? [index] : [])) ?? []
  const failedIndices = rows?.flatMap((row, index) => (row.run === 'failed' ? [index] : [])) ?? []
  const toImport = rows?.filter((row) => row.state === 'new') ?? []
  const finished = toImport.filter((row) => row.run === 'success' || row.run === 'failed').length

  const footer = !rows ? (
    <>
      <Button variant="outline" size="sm" onClick={onClose}>取消</Button>
      <Button size="sm" onClick={preview} disabled={!parsedCount}>预览{parsedCount ? ` ${parsedCount} 条` : ''}</Button>
    </>
  ) : (
    <>
      {!running && <Button variant="ghost" size="sm" className="mr-auto" onClick={() => setRows(null)}>返回修改</Button>}
      {running ? (
        <Button variant="outline" size="sm" disabled={stopping} onClick={() => { stopRef.current = true; setStopping(true) }}>
          <Square />{stopping ? '正在停止…' : '停止'}
        </Button>
      ) : (
        <Button variant="outline" size="sm" onClick={onClose}>关闭</Button>
      )}
      {!running && failedIndices.length > 0 && (
        <Button variant="outline" size="sm" onClick={() => void runImport(failedIndices)}><RotateCcw />重试失败 ({failedIndices.length})</Button>
      )}
      {!running && pendingIndices.length > 0 && (
        <Button size="sm" onClick={() => void runImport(pendingIndices)}><Play />开始导入 ({pendingIndices.length})</Button>
      )}
      {!running && pendingIndices.length === 0 && failedIndices.length === 0 && <Button size="sm" onClick={onClose}>完成</Button>}
    </>
  )

  const pinned = rows && counts ? (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
        <span>共 <span className="font-semibold tabular-nums">{rows.length}</span> 条</span>
        <span className="text-muted-foreground tabular-nums">
          将新建 {counts.new} · 跳过 {counts.duplicate + counts.exists}（重复 {counts.duplicate}、已存在 {counts.exists}）· 格式错误 {counts.invalid}
        </span>
        <span className="ml-auto text-xs text-muted-foreground">
          {defaults.enabled ? '导入后启用' : '导入后停用'} · 同时导入 {BATCH_CONCURRENCY} 个
        </span>
      </div>
      {(running || finished > 0) && toImport.length > 0 && <Progress value={Math.round((finished / toImport.length) * 100)} className="h-1.5" />}
    </div>
  ) : undefined

  return (
    <ModalShell
      open={open}
      title="批量导入账号"
      description="粘贴内容或选择文件，自动识别格式；预览确认后再逐条创建。"
      width="max-w-6xl"
      onClose={() => { if (!running) onClose() }}
      pinned={pinned}
      footer={footer}
    >
      {rows ? (
        <div className="overflow-x-auto rounded-lg border border-border/70">
          <table className="w-full min-w-[56rem] text-sm">
            <thead className="bg-muted/40 text-xs text-muted-foreground">
              <tr>
                <th className="w-28 px-3 py-2 text-left font-medium">来源</th>
                <th className="px-3 py-2 text-left font-medium">名称</th>
                <th className="px-3 py-2 text-left font-medium">服务地址</th>
                <th className="w-36 px-3 py-2 text-left font-medium">密钥</th>
                <th className="w-28 px-3 py-2 text-left font-medium">预览</th>
                <th className="w-24 px-3 py-2 text-left font-medium">结果</th>
                <th className="px-3 py-2 text-left font-medium">说明</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row, index) => (
                <tr key={`${row.origin}-${row.position}-${index}`} className="border-t border-border/60 align-top">
                  <td className="px-3 py-2 text-xs text-muted-foreground">
                    <div className="max-w-[7rem] truncate" title={row.origin}>{row.origin}</div>
                    <div>第 {row.position} 条</div>
                  </td>
                  <td className="max-w-[12rem] break-words px-3 py-2">{row.name}</td>
                  <td className="max-w-[16rem] break-all px-3 py-2 text-xs">{row.baseUrl}</td>
                  <td className="px-3 py-2 font-mono text-xs">{row.maskedKey}</td>
                  <td className="px-3 py-2"><Badge tone={STATE_VIEW[row.state].tone}>{STATE_VIEW[row.state].label}</Badge></td>
                  <td className="px-3 py-2 text-xs">{row.run ? <BatchStatusCell status={row.run} /> : row.state === 'new' ? <span className="text-muted-foreground">待导入</span> : <span className="text-muted-foreground">-</span>}</td>
                  <td className="px-3 py-2 text-xs">
                    {row.createdId !== undefined && <div className="text-success">已创建 #{row.createdId}</div>}
                    {row.error && <div className="break-words text-destructive">{row.error}</div>}
                    {row.reason && <div className={row.state === 'invalid' ? 'text-destructive' : 'text-warning'}>{row.reason}</div>}
                    {row.warnings.map((warning) => <div key={warning} className="text-warning">{warning}</div>)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="space-y-4">
          <div className="grid gap-4 lg:grid-cols-[minmax(0,5fr)_minmax(0,6fr)]">
            <div className="space-y-4">
              <FormSection
                title="账号数据"
                description="可以多选文件，每个文件单独识别格式；粘贴的内容也会一起导入。"
                actions={(
                  <label className="cursor-pointer">
                    <input type="file" className="sr-only" accept=".json,.jsonl,.txt,.csv" multiple onChange={(e) => void handleFiles(e)} />
                    <Button type="button" variant="outline" size="sm" asChild><span><FileUp />选择文件</span></Button>
                  </label>
                )}
              >
                <Textarea className="min-h-[200px] font-mono text-xs" placeholder={PLACEHOLDER} value={text} onChange={(e) => setText(e.target.value)} />
                {files.length > 0 && (
                  <div className="mt-2 flex flex-wrap gap-1.5">
                    {files.map((file) => (
                      <span key={file.name} className="inline-flex items-center gap-1 rounded-md bg-muted py-0.5 pl-2 pr-0.5 text-xs">
                        {file.name}
                        <Button type="button" variant="ghost" size="icon-xs" className="size-5" aria-label={`移除 ${file.name}`} onClick={() => setFiles((prev) => prev.filter((item) => item.name !== file.name))}>
                          <X />
                        </Button>
                      </span>
                    ))}
                  </div>
                )}
              </FormSection>
              {inputs.length > 0 && (
                <FormSection title="识别结果">
                  <div className="space-y-1.5 text-xs">
                    {inputs.map((input) => {
                      const invalid = input.result.entries.filter((entry) => entry.error).length
                      return (
                        <div key={input.name} className="flex flex-wrap items-baseline gap-x-2">
                          <span className="font-medium">{input.name}</span>
                          {input.result.error ? (
                            <span className="text-destructive">{input.result.error}</span>
                          ) : (
                            <span className="text-muted-foreground">
                              {IMPORT_FORMAT_LABELS[input.result.format]} · {input.result.entries.length - invalid} 条可用
                              {invalid > 0 && <span className="text-destructive"> · {invalid} 条格式错误</span>}
                            </span>
                          )}
                        </div>
                      )
                    })}
                  </div>
                </FormSection>
              )}
            </div>
            <FormSection
              title="默认参数"
              description="只填充每条账号里缺失的字段，文件里写了的以文件为准；标签和支持模型在文件里为空时才用这里的值。"
              actions={<Button type="button" variant="ghost" size="xs" onClick={() => setDefaults(initialImportDefaults())}>恢复默认</Button>}
            >
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="导入后启用" description="关闭时全部以停用状态创建，检查无误后再启用；文件里的启用状态不生效">
                  <SwitchCell checked={defaults.enabled} onChange={(v) => setDefault('enabled', v)} onText="启用" offText="停用" />
                </Field>
                <Field label="优先级" description="留空用默认 100；数字越小越靠前">
                  <NumberInput value={defaults.priority} onChange={(v) => setDefault('priority', v)} placeholder="100" />
                </Field>
                <Field label="单账号最大并发" description="留空用默认 10">
                  <NumberInput value={defaults.maxConcurrentRequests} onChange={(v) => setDefault('maxConcurrentRequests', v)} min={1} unit="个请求" placeholder="10" />
                </Field>
                <Field label="成本倍率" description="留空用默认 1">
                  <NumberInput value={defaults.costMultiplier} onChange={(v) => setDefault('costMultiplier', v)} step="0.01" min={0} unit="×" placeholder="1" />
                </Field>
                <Field label="认证方式">
                  <Select value={defaults.authType} onValueChange={(v) => setDefault('authType', v as AccountAuthType)}>
                    <SelectTrigger><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {ACCOUNT_AUTH_TYPE_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
                    </SelectContent>
                  </Select>
                </Field>
                <Field label="下游 usage">
                  <Select value={defaults.usageProjectionMode} onValueChange={(v) => setDefault('usageProjectionMode', v as UsageProjectionMode)}>
                    <SelectTrigger><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {USAGE_PROJECTION_MODE_OPTIONS.map((option) => <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>)}
                    </SelectContent>
                  </Select>
                </Field>
                <Field label="标签" className="sm:col-span-2">
                  <TagsField value={defaults.tags} options={tagOptions} onChange={(v) => setDefault('tags', v)} />
                </Field>
                <Field label="支持模型" description="空表示不限制" className="sm:col-span-2">
                  <SupportedModelTagsEditor value={defaults.supportedModels} onChange={(v) => setDefault('supportedModels', v)} />
                </Field>
              </div>
            </FormSection>
          </div>
          {formError && <Callout tone="error">{formError}</Callout>}
        </div>
      )}
    </ModalShell>
  )
}
