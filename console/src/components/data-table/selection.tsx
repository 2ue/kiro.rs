import type { ReactNode } from 'react'
import { X } from 'lucide-react'
import type { ColumnDef } from '@tanstack/react-table'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'

/**
 * 行选择列：支持 Shift 连续选择（从上一次点击的行到当前行）。
 * 锚点存在 table meta 之外的闭包里，按表实例隔离。
 */
export function selectionColumn<T>(): ColumnDef<T, unknown> {
  let anchor: number | null = null
  return {
    id: '_select',
    size: 40,
    header: ({ table }) => (
      <Checkbox
        aria-label="全选当前页"
        checked={table.getIsAllRowsSelected() ? true : table.getIsSomeRowsSelected() ? 'indeterminate' : false}
        onCheckedChange={(v) => table.toggleAllRowsSelected(v === true)}
      />
    ),
    cell: ({ row, table }) => (
      <span
        className="flex"
        onClick={(e) => {
          e.stopPropagation()
          const rows = table.getRowModel().rows
          const index = rows.findIndex((r) => r.id === row.id)
          if (e.shiftKey && anchor !== null && anchor !== index) {
            e.preventDefault()
            const [from, to] = anchor < index ? [anchor, index] : [index, anchor]
            const target = !row.getIsSelected()
            table.setRowSelection((prev) => {
              const next = { ...prev }
              for (const r of rows.slice(from, to + 1)) {
                if (target) next[r.id] = true
                else delete next[r.id]
              }
              return next
            })
          } else {
            row.toggleSelected(!row.getIsSelected())
          }
          anchor = index
        }}
      >
        <Checkbox aria-label="选择该行（按住 Shift 连选）" checked={row.getIsSelected()} tabIndex={-1} className="pointer-events-none" />
      </span>
    ),
  }
}

/** 选中行后浮出的批量操作栏 */
export function BulkBar({
  count,
  onClear,
  children,
  extra,
}: {
  count: number
  onClear: () => void
  children: ReactNode
  /** 选择范围提示，例如"选中全部 120 条筛选结果" */
  extra?: ReactNode
}) {
  if (count === 0) return null
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-4 z-30 flex justify-center px-4">
      <div
        role="toolbar"
        aria-label="批量操作"
        className="pointer-events-auto flex max-w-full items-center gap-1 overflow-x-auto rounded-xl border bg-popover p-1.5 shadow-lg animate-in fade-in-0 slide-in-from-bottom-2"
      >
        <span className="num px-2 text-sm font-medium whitespace-nowrap">已选 {count} 项</span>
        {extra}
        <div className="h-5 w-px bg-border" />
        {children}
        <div className="h-5 w-px bg-border" />
        <Button variant="ghost" size="icon-sm" onClick={onClear} aria-label="取消选择">
          <X />
        </Button>
      </div>
    </div>
  )
}
