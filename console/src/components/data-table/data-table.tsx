import { useRef, type ReactNode } from 'react'
import {
  flexRender,
  getCoreRowModel,
  useReactTable,
  type ColumnDef,
  type OnChangeFn,
  type Row,
  type RowSelectionState,
  type VisibilityState,
} from '@tanstack/react-table'
import { useVirtualizer } from '@tanstack/react-virtual'
import { useIsMobile } from '@/hooks/use-mobile'
import { cn } from '@/lib/utils'

export interface DataTableProps<T> {
  data: T[]
  columns: ColumnDef<T, unknown>[]
  getRowId: (row: T) => string
  onRowClick?: (row: T) => void
  activeRowId?: string
  rowSelection?: RowSelectionState
  onRowSelectionChange?: OnChangeFn<RowSelectionState>
  columnVisibility?: VisibilityState
  onColumnVisibilityChange?: OnChangeFn<VisibilityState>
  /** 行数较多时启用虚拟滚动；需配合固定高度容器 */
  virtual?: boolean
  maxHeight?: string
  empty?: ReactNode
  loading?: boolean
  className?: string
  rowClassName?: (row: T) => string | undefined
}

/**
 * 统一数据表：粘性表头、行点击打开详情、行选择、列显隐、可选虚拟滚动。
 * 列宽通过 column.size 控制；meta.align = 'right' 用于数字列。
 */
export function DataTable<T>({
  data,
  columns,
  getRowId,
  onRowClick,
  activeRowId,
  rowSelection,
  onRowSelectionChange,
  columnVisibility,
  onColumnVisibilityChange,
  virtual,
  maxHeight = 'calc(100svh - 18rem)',
  empty,
  loading,
  className,
  rowClassName,
}: DataTableProps<T>) {
  const table = useReactTable({
    data,
    columns,
    getRowId,
    getCoreRowModel: getCoreRowModel(),
    state: {
      ...(rowSelection ? { rowSelection } : {}),
      ...(columnVisibility ? { columnVisibility } : {}),
    },
    enableRowSelection: !!onRowSelectionChange,
    onRowSelectionChange,
    onColumnVisibilityChange,
  })

  const scrollRef = useRef<HTMLDivElement>(null)
  const rows = table.getRowModel().rows
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 40,
    overscan: 12,
    enabled: !!virtual,
  })

  const isMobile = useIsMobile()
  const hasMobileLayout = columns.some((c) => c.meta?.mobile)

  const visibleColumns = table.getVisibleLeafColumns()
  const gridTemplate = visibleColumns.map((c) => (c.columnDef.meta?.grow ? `minmax(${c.getSize()}px, 1fr)` : `${c.getSize()}px`)).join(' ')

  const renderRow = (row: Row<T>, style?: React.CSSProperties, index?: number) => {
    const id = getRowId(row.original)
    return (
      <div
        role="row"
        key={row.id}
        data-index={index}
        ref={virtual ? virtualizer.measureElement : undefined}
        aria-selected={row.getIsSelected() || undefined}
        tabIndex={onRowClick ? 0 : undefined}
        onClick={onRowClick ? () => onRowClick(row.original) : undefined}
        onKeyDown={
          onRowClick
            ? (e) => {
                if (e.key === 'Enter' && e.target === e.currentTarget) onRowClick(row.original)
              }
            : undefined
        }
        style={{ gridTemplateColumns: gridTemplate, ...style }}
        className={cn(
          'row-h grid items-center border-b text-sm transition-colors last:border-b-0',
          onRowClick && 'cursor-pointer hover:bg-muted/50 focus-visible:bg-muted/60 focus-visible:outline-none',
          row.getIsSelected() && 'bg-accent/50',
          activeRowId === id && 'bg-accent/70 shadow-[inset_2px_0_0_var(--primary)]',
          rowClassName?.(row.original),
        )}
      >
        {row.getVisibleCells().map((cell) => (
          <div
            role="cell"
            key={cell.id}
            className={cn(
              'min-w-0 truncate px-3',
              cell.column.columnDef.meta?.align === 'right' && 'num text-right',
              cell.column.columnDef.meta?.className,
            )}
          >
            {flexRender(cell.column.columnDef.cell, cell.getContext())}
          </div>
        ))}
      </div>
    )
  }

  if (isMobile && hasMobileLayout) {
    return (
      <MobileList
        rows={rows}
        onRowClick={onRowClick}
        activeRowId={activeRowId}
        getRowId={getRowId}
        empty={empty}
        className={className}
        rowClassName={rowClassName}
      />
    )
  }

  return (
    <div className={cn('overflow-hidden rounded-xl border bg-card', className)}>
      <div ref={scrollRef} className="overflow-auto" style={{ maxHeight: virtual ? maxHeight : undefined }} role="table">
        <div className="min-w-fit">
          <div role="rowgroup" className="sticky top-0 z-10 bg-card">
            {table.getHeaderGroups().map((hg) => (
              <div
                role="row"
                key={hg.id}
                className="grid h-9 items-center border-b bg-muted/40 text-xs font-medium text-muted-foreground"
                style={{ gridTemplateColumns: gridTemplate }}
              >
                {hg.headers.map((header) => (
                  <div
                    role="columnheader"
                    key={header.id}
                    className={cn(
                      'min-w-0 truncate px-3',
                      header.column.columnDef.meta?.align === 'right' && 'text-right',
                      header.column.columnDef.meta?.className,
                    )}
                  >
                    {header.isPlaceholder ? null : flexRender(header.column.columnDef.header, header.getContext())}
                  </div>
                ))}
              </div>
            ))}
          </div>
          <div role="rowgroup" className={cn(loading && 'opacity-60 transition-opacity')}>
            {rows.length === 0 ? (
              <div className="py-2">{empty}</div>
            ) : virtual ? (
              <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
                {virtualizer.getVirtualItems().map((item) =>
                  renderRow(
                    rows[item.index]!,
                    {
                      position: 'absolute',
                      top: 0,
                      left: 0,
                      width: '100%',
                      transform: `translateY(${item.start}px)`,
                    },
                    item.index,
                  ),
                )}
              </div>
            ) : (
              rows.map((row) => renderRow(row))
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

/** 窄屏卡片列表：只渲染标记了 meta.mobile 的列 */
function MobileList<T>({
  rows,
  onRowClick,
  activeRowId,
  getRowId,
  empty,
  className,
  rowClassName,
}: {
  rows: Row<T>[]
  onRowClick?: (row: T) => void
  activeRowId?: string
  getRowId: (row: T) => string
  empty?: ReactNode
  className?: string
  rowClassName?: (row: T) => string | undefined
}) {
  if (rows.length === 0) return <div className={cn('rounded-xl border bg-card py-2', className)}>{empty}</div>
  return (
    <ul className={cn('divide-y overflow-hidden rounded-xl border bg-card', className)}>
      {rows.map((row) => {
        const cells = row.getVisibleCells()
        const title = cells.find((c) => c.column.columnDef.meta?.mobile === 'title')
        const badge = cells.find((c) => c.column.columnDef.meta?.mobile === 'badge')
        const select = cells.find((c) => c.column.id === '_select')
        const rest = cells.filter((c) => c.column.columnDef.meta?.mobile === true)
        const id = getRowId(row.original)
        const header = (c: (typeof cells)[number]) => {
          const h = c.column.columnDef.header
          return typeof h === 'string' ? h : (c.column.columnDef.meta?.label ?? c.column.id)
        }
        return (
          <li
            key={row.id}
            tabIndex={onRowClick ? 0 : undefined}
            onClick={onRowClick ? () => onRowClick(row.original) : undefined}
            onKeyDown={onRowClick ? (e) => e.key === 'Enter' && e.target === e.currentTarget && onRowClick(row.original) : undefined}
            className={cn(
              'space-y-2 px-3 py-3 text-sm',
              onRowClick && 'cursor-pointer active:bg-muted/60',
              row.getIsSelected() && 'bg-accent/50',
              activeRowId === id && 'bg-accent/70',
              rowClassName?.(row.original),
            )}
          >
            <div className="flex items-start gap-2">
              {select && <div className="pt-0.5">{flexRender(select.column.columnDef.cell, select.getContext())}</div>}
              <div className="min-w-0 flex-1">{title && flexRender(title.column.columnDef.cell, title.getContext())}</div>
              {badge && <div className="shrink-0">{flexRender(badge.column.columnDef.cell, badge.getContext())}</div>}
            </div>
            {rest.length > 0 && (
              <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-xs">
                {rest.map((c) => (
                  <div key={c.id} className="min-w-0">
                    <dt className="text-muted-foreground">{header(c)}</dt>
                    <dd className="num mt-0.5 truncate">{flexRender(c.column.columnDef.cell, c.getContext())}</dd>
                  </div>
                ))}
              </dl>
            )}
          </li>
        )
      })}
    </ul>
  )
}
