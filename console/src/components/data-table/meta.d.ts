import '@tanstack/react-table'

declare module '@tanstack/react-table' {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  interface ColumnMeta<TData, TValue> {
    align?: 'left' | 'right'
    /** 该列占用剩余宽度 */
    grow?: boolean
    className?: string
    /** 列显隐菜单中的名称 */
    label?: string
  }
}
