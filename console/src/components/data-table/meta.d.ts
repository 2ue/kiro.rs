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
    /**
     * 窄屏卡片布局中的角色：title 作为卡片标题，badge 放在标题右侧，
     * 其余列按 mobile=true 显示为键值行；未标记的列在窄屏隐藏。
     */
    mobile?: 'title' | 'badge' | boolean
  }
}
