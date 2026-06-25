# UI 重构 - 设计系统约定(给功能页移植用)

新前端在 `kiro.rs/ui/`,技术栈:React 18 + shadcn/Radix + Tailwind v4 + react-router v6。
任务:把 `kiro.rs/admin-ui-daisy/src/components/<X>Panel.tsx`(react-daisyui)移植成新设计系统下的 feature 页面。

## 环境

- 前端构建必须用 node 22:`/Users/yuanfeijie/.volta/bin/volta run --node 22.23.0 pnpm check`(类型检查)和 `... pnpm build`。默认 shell 的 node 是 16,会失败。
- 工作目录 `kiro.rs/ui/`。`@/` 映射到 `ui/src/`。
- 复用层已就位且**不要改动**:`@/api/*`、`@/hooks/*`、`@/types/api`、`@/lib/*`(format、utils、storage、credential-import、test-models)。直接 import 用。
- `@/types/ui` 导出 `pageMeta`(每个 tab 的 title/subtitle)、`navItems`、`TabKey`。

## 绝对规则

- **不要用 react-daisyui**,不要用 daisy 的 class(`btn`、`card`、`badge`、`base-content`、`base-100/200/300`、`rounded-box`、`text-error/success/warning/info` 作为 daisy 语义色等)。
- 颜色只用语义 token(下面列),不要硬编码十六进制。
- 不要写动态拼接的 Tailwind class(如 `bg-${color}`),Tailwind v4 JIT 不识别。
- 用 `cn()`(来自 `@/lib/utils`)合并 class。
- 中文文案、注释保持和原面板一致。

## 颜色 token(Tailwind 工具类直接可用)

- 背景:`bg-background`(页面)、`bg-card`(卡片)、`bg-muted`(浅灰块)、`bg-muted/40`(更淡)
- 文字:`text-foreground`、`text-muted-foreground`(次要)、`text-primary`
- 边框:`border-border`、`border-input`
- 语义色:`text-success bg-success`、`text-warning bg-warning`、`text-info bg-info`、`text-destructive bg-destructive`(error)。每个都有 `-foreground` 变体。primary 是金棕色。
- 圆角统一 `rounded-lg`/`rounded-xl`;卡片阴影 `shadow-sm`。
- 滚动容器加 `scrollbar-thin`。

## 可用组件

### `@/components/ui`(shadcn 原子)
- `Button` — props:`variant`(default|secondary|destructive|outline|ghost|link)、`size`(default|sm|xs|lg|icon|icon-sm|icon-xs)、`asChild`。默认 type=button。
- `Badge` — props:`tone`(neutral|primary|secondary|success|warning|error|info)。**用 tone,不是 color**。导出 `BadgeProps`。
- `Card, CardHeader, CardTitle, CardDescription, CardContent, CardFooter` — 朴素容器(无内边距预设,自己加 p-*)。
- `Input, Textarea` — 原生 props。
- `Label, Separator, Switch, Checkbox, Progress, Spinner, Skeleton`。
  - `Switch`/`Checkbox` 用 `checked` + `onCheckedChange(bool)`(Radix,不是 onChange)。`Spinner` 有 `size`(sm|md|lg)。
- `Dialog, DialogContent, DialogHeader, DialogBody, DialogFooter, DialogTitle, DialogDescription`。`DialogContent` 有 `width` prop(如 "max-w-2xl")、`hideClose`。
- `Select, SelectTrigger, SelectValue, SelectContent, SelectItem` — Radix Select,受控用 `value` + `onValueChange(string)`。`SelectTrigger` 有 `size`(默认|sm)。**注意 Radix Select 的 item value 不能是空字符串**——用 'all' 之类哨兵值代替 "全部" 选项,并在 onValueChange 里转回 ''。
- `Tabs, TabsList, TabsTrigger, TabsContent`、`Tooltip`(简易:`<Tooltip label=".." side="..">{child}</Tooltip>`)、`DropdownMenu*`、`Popover*`。
- `Table, TableHeader, TableBody, TableRow, TableHead, TableCell` — 语义化表格。表头放 `<TableHeader><TableRow><TableHead>..`。宽表格外面包 `<div className="scrollbar-thin overflow-x-auto">` 并给 Table 加 `min-w-[..]`。

### `@/components/patterns`(复合布局原语,优先用这些)
- `PageContainer` — 每个页面最外层(max-width + 纵向 gap)。
- `PageHeader` — props:`title`、`subtitle`、`actions`。放页面顶部。
- `SectionCard` — props:`title`、`description`、`icon`、`actions`、`noPadding`、`children`。卡片化的区块容器。
- `StatCard` — props:`title`、`value`、`desc`、`tone`(default|success|warning|info|error)、`icon`。`StatGrid` 自适应包裹多个 StatCard。
- `EmptyState`(props:`title`、`description`、`icon`)、`LoadingState`(props:`text`)、`ErrorState`(props:`message`)、`Callout`(props:`tone`)。
- `Pagination` — props:`page`、`pageCount`、`total?`、`pageSize?`、`onPageChange`。
- `Field` — 表单字段包装:`label`、`htmlFor`、`required`、`description`、`error`、`inline`。`FieldGrid`(props:`min` 最小列宽,默认 15rem)自适应多列。
- `Toolbar, ToolbarSearch, ToolbarActions`。
- `ModalShell` — 受控弹窗:`open`、`onClose`、`title`、`description?`、`footer?`、`width?`、`noBodyPadding?`、`children`。**弹窗一律用 ModalShell,不要直接用 Dialog**。
- `useConfirm()` — 返回 `confirm({title, message, confirmText?, cancelText?, tone?})` => Promise<boolean>。删除等危险操作用它。Provider 已在 app 根挂好。

### `@/components/charts`
- `MiniBarChart`(data:number[]、color:'primary'|'success'|'info'|'warning'|'error')、`Sparkline`(data、color CSS 值如 'hsl(var(--primary))')、`ProgressRing`(value、max、color)。

## 页面结构范式

```tsx
import { PageContainer, PageHeader, SectionCard, StatCard, StatGrid, EmptyState, LoadingState, ErrorState, useConfirm } from '@/components/patterns'
import { Button, Badge, ... } from '@/components/ui'
import { pageMeta } from '@/types/ui'

export function XPage() {
  return (
    <PageContainer>
      <PageHeader title={pageMeta.<key>.title} subtitle={pageMeta.<key>.subtitle} actions={...} />
      <StatGrid>...</StatGrid>
      <SectionCard title=".." actions={..}>...</SectionCard>
    </PageContainer>
  )
}
```

## 参考已完成的页面(照着写,风格对齐)

- `ui/src/features/usage/`(usage-page.tsx + usage-modals.tsx + usage-helpers.tsx)— 最全面:筛选、双视图表格/卡片、分页、多弹窗、自动刷新。
- `ui/src/features/pricing/`、`ui/src/features/proxies/`、`ui/src/features/validation/`、`ui/src/features/audit/`。

## toast

用 `import { toast } from 'sonner'`,`toast.success/error/warning/info(...)`。

## 完成标准

- 你的 feature 目录下文件齐全,导出 `export function <Comp>()`(名字和 `ui/src/app/router.tsx` 里 import 的一致,已存在占位)。
- `pnpm check`(node 22)通过——**只看你负责的文件有没有新报错**(其他 agent 也在并行改别的 feature,忽略不属于你的文件的错误)。
- 不改 router.tsx、不改 `@/components`、`@/api`、`@/hooks`、`@/types`、`@/lib`、不改其他 feature 目录。
- 功能、字段、文案与原 daisy 面板对齐,不要漏功能。
