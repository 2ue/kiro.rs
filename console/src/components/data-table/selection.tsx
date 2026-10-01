import type { ReactNode } from 'react'
import { X } from 'lucide-react'
import type { ColumnDef } from '@tanstack/react-table'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'

export function selectionColumn<T>(): ColumnDef<T, unknown> {
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
    cell: ({ row }) => (
      <span onClick={(e) => e.stopPropagation()} className="flex">
        <Checkbox aria-label="选择该行" checked={row.getIsSelected()} onCheckedChange={(v) => row.toggleSelected(v === true)} />
      </span>
    ),
  }
}

/** 选中行后浮出的批量操作栏 */
export function BulkBar({ count, onClear, children }: { count: number; onClear: () => void; children: ReactNode }) {
  if (count === 0) return null
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-4 z-30 flex justify-center px-4">
      <div
        role="toolbar"
        aria-label="批量操作"
        className="pointer-events-auto flex max-w-full items-center gap-1 overflow-x-auto rounded-xl border bg-popover p-1.5 shadow-lg animate-in fade-in-0 slide-in-from-bottom-2"
      >
        <span className="num px-2 text-sm font-medium whitespace-nowrap">已选 {count} 项</span>
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
