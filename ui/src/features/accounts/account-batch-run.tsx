// 批量操作的执行与结果弹窗：逐个调用单账号接口、限并发、带进度、可停止、结束给结果表。
// 停止后没开始的行标为“跳过：已停止”，可以“继续剩余”只跑这些，也可以“重试失败”。
import { useCallback, useRef, useState, type ReactNode } from 'react'
import { AlertCircle, CheckCircle2, Loader2, Play, RotateCcw, Square, XCircle } from 'lucide-react'
import { toast } from 'sonner'
import { ModalShell } from '@/components/patterns'
import { Button, Progress } from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import {
  BATCH_CONCURRENCY,
  markNotStartedRows,
  notStartedIndices,
  resetRows,
  runWithConcurrency,
  summarizeBatchRows,
  type BatchRow,
  type BatchRowStatus,
  type BatchTaskResult,
} from './account-batch'

export interface BatchItem {
  id: number
  label: string
}

export type BatchTask = (item: BatchItem) => Promise<BatchTaskResult>

interface BatchRunState {
  open: boolean
  title: string
  description?: string
  rows: BatchRow[]
  running: boolean
  stopping: boolean
  stopped: boolean
}

export interface BatchRunController {
  state: BatchRunState | null
  start: (options: { title: string; description?: string; items: BatchItem[]; task: BatchTask; onFinished?: () => void }) => void
  retryFailed: () => void
  /** 只跑上次停止时还没开始的行。 */
  continueRemaining: () => void
  stop: () => void
  hide: () => void
  show: () => void
  close: () => void
}

export function useBatchRun(): BatchRunController {
  const [state, setState] = useState<BatchRunState | null>(null)
  const stopRef = useRef(false)
  // 是否正在执行用 ref 判断：调用方可能 await（比如先取最新账号）之后才 start，渲染时闭包里的 state 可能已过期。
  const runningRef = useRef(false)
  const taskRef = useRef<{ task: BatchTask; onFinished?: () => void; title: string } | null>(null)

  const execute = useCallback(async (rows: BatchRow[], indices: number[]) => {
    const current = taskRef.current
    if (!current || indices.length === 0) return
    runningRef.current = true
    stopRef.current = false
    setState((prev) => prev && { ...prev, running: true, stopping: false, stopped: false })
    const update = (index: number, patch: Partial<BatchRow>) =>
      setState((prev) => prev && { ...prev, rows: prev.rows.map((row, i) => (i === index ? { ...row, ...patch } : row)) })
    const outcomes: BatchRowStatus[] = []
    const started = new Set<number>()
    const { stopped } = await runWithConcurrency(
      indices,
      async (index) => {
        const row = rows[index]
        started.add(index)
        update(index, { status: 'running', message: undefined })
        let result: BatchTaskResult
        try {
          result = await current.task({ id: row.id, label: row.label })
        } catch (error) {
          result = { status: 'failed', message: extractErrorMessage(error) }
        }
        outcomes.push(result.status)
        update(index, { status: result.status, message: result.message })
      },
      { concurrency: BATCH_CONCURRENCY, shouldStop: () => stopRef.current },
    )
    runningRef.current = false
    const notStarted = indices.length - started.size
    setState((prev) => prev && { ...prev, rows: markNotStartedRows(prev.rows, indices, started), running: false, stopping: false, stopped })
    current.onFinished?.()
    const summary = summarizeBatchRows(outcomes.map((status) => ({ status })))
    const text = `${current.title}${stopped ? '已停止' : '完成'}：成功 ${summary.success}，失败 ${summary.failed}${summary.skipped ? `，跳过 ${summary.skipped}` : ''}${notStarted > 0 ? `，未开始 ${notStarted}` : ''}`
    if (summary.failed > 0 || stopped) toast.warning(text)
    else toast.success(text)
  }, [])

  const start: BatchRunController['start'] = useCallback(({ title, description, items, task, onFinished }) => {
    if (runningRef.current) {
      toast.error('上一个批量操作还在进行，请等它结束或先停止')
      return
    }
    const rows: BatchRow[] = items.map((item) => ({ id: item.id, label: item.label, status: 'pending' }))
    taskRef.current = { task, onFinished, title }
    setState({ open: true, title, description, rows, running: false, stopping: false, stopped: false })
    void execute(rows, rows.map((_, index) => index))
  }, [execute])

  const rerun = useCallback((pick: (rows: BatchRow[]) => number[]) => {
    if (!state || runningRef.current) return
    const indices = pick(state.rows)
    if (indices.length === 0) return
    const rows = resetRows(state.rows, indices)
    setState({ ...state, rows })
    void execute(rows, indices)
  }, [execute, state])

  const retryFailed = useCallback(
    () => rerun((rows) => rows.flatMap((row, index) => (row.status === 'failed' ? [index] : []))),
    [rerun],
  )
  const continueRemaining = useCallback(() => rerun(notStartedIndices), [rerun])

  return {
    state,
    start,
    retryFailed,
    continueRemaining,
    stop: () => {
      stopRef.current = true
      setState((prev) => prev && { ...prev, stopping: true })
    },
    hide: () => setState((prev) => prev && { ...prev, open: false }),
    show: () => setState((prev) => prev && { ...prev, open: true }),
    close: () => setState((prev) => (prev?.running ? { ...prev, open: false } : null)),
  }
}

const STATUS_VIEW: Record<BatchRowStatus, { label: string; className: string; icon: ReactNode }> = {
  pending: { label: '等待', className: 'text-muted-foreground', icon: <span className="inline-block size-3.5 rounded-full bg-muted" /> },
  running: { label: '进行中', className: 'text-primary', icon: <Loader2 className="size-3.5 animate-spin" /> },
  success: { label: '成功', className: 'text-success', icon: <CheckCircle2 className="size-3.5" /> },
  failed: { label: '失败', className: 'text-destructive', icon: <XCircle className="size-3.5" /> },
  skipped: { label: '跳过', className: 'text-warning', icon: <AlertCircle className="size-3.5" /> },
}

export function BatchStatusCell({ status }: { status: BatchRowStatus }) {
  const view = STATUS_VIEW[status]
  return (
    <span className={`inline-flex items-center gap-1.5 whitespace-nowrap ${view.className}`}>
      {view.icon}
      {view.label}
    </span>
  )
}

export function BatchRunModal({ batch }: { batch: BatchRunController }) {
  const state = batch.state
  if (!state) return null
  const summary = summarizeBatchRows(state.rows)
  const remaining = notStartedIndices(state.rows).length
  const pct = summary.total > 0 ? Math.round((summary.finished / summary.total) * 100) : 0
  return (
    <ModalShell
      open={state.open}
      title={state.title}
      description={state.description}
      width="max-w-3xl"
      onClose={() => (state.running ? batch.hide() : batch.close())}
      pinned={(
        <div className="space-y-2">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
            <span>共 <span className="font-semibold tabular-nums">{summary.total}</span> 个</span>
            <span className="text-muted-foreground tabular-nums">
              成功 {summary.success} · 失败 {summary.failed} · 跳过 {summary.skipped} · 待处理 {summary.pending + summary.running}
            </span>
            <span className="ml-auto text-xs text-muted-foreground">
              {state.running ? (state.stopping ? '正在停止，进行中的会做完…' : `同时处理 ${BATCH_CONCURRENCY} 个`) : state.stopped ? '已停止' : '已结束'}
            </span>
          </div>
          <Progress value={pct} className="h-1.5" tone={summary.failed > 0 ? 'warning' : 'primary'} />
        </div>
      )}
      footer={state.running ? (
        <>
          <Button variant="ghost" size="sm" onClick={batch.hide}>后台运行</Button>
          <Button variant="outline" size="sm" onClick={batch.stop} disabled={state.stopping}>
            <Square />{state.stopping ? '正在停止…' : '停止'}
          </Button>
        </>
      ) : (
        <>
          {remaining > 0 && (
            <Button variant="outline" size="sm" onClick={batch.continueRemaining}>
              <Play />继续剩余 ({remaining})
            </Button>
          )}
          {summary.failed > 0 && (
            <Button variant="outline" size="sm" onClick={batch.retryFailed}>
              <RotateCcw />重试失败 ({summary.failed})
            </Button>
          )}
          <Button size="sm" onClick={batch.close}>关闭</Button>
        </>
      )}
    >
      <div className="overflow-hidden rounded-lg border border-border/70">
        <table className="w-full text-sm">
          <thead className="bg-muted/40 text-xs text-muted-foreground">
            <tr>
              <th className="px-3 py-2 text-left font-medium">账号</th>
              <th className="w-24 px-3 py-2 text-left font-medium">结果</th>
              <th className="px-3 py-2 text-left font-medium">说明</th>
            </tr>
          </thead>
          <tbody>
            {state.rows.map((row) => (
              <tr key={row.id} className="border-t border-border/60 align-top">
                <td className="max-w-[16rem] px-3 py-2">
                  <div className="truncate font-medium" title={row.label}>{row.label}</div>
                  <div className="text-xs text-muted-foreground">#{row.id}</div>
                </td>
                <td className="px-3 py-2 text-xs"><BatchStatusCell status={row.status} /></td>
                <td className={`break-words px-3 py-2 text-xs ${row.status === 'failed' ? 'text-destructive' : 'text-muted-foreground'}`}>{row.message ?? ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </ModalShell>
  )
}
