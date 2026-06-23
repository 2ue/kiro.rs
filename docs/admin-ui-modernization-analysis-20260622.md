# Admin UI 现代化重构分析

日期：2026-06-22

## 结论

当前实际改造目标是 `/console` 对应的 `admin-ui-daisy`。这套前端不是常规路由页面体系，而是一个 `Dashboard` 壳层根据 pathname 维护 tab 状态，再把各业务面板直接渲染到同一个 `main` 容器里。

问题不只是颜色不对，也不是某两个页面不好看。现有实现的根问题是：视觉 token、页面骨架、资源列表、表格、表单分组、弹层和批量操作没有形成统一的后台设计系统。每个页面都在自己实现工具栏、卡片、说明块、表格容器和弹层结构，导致整体像多个局部页面拼在一起。

本次方向是中性浅底后台系统，黑金只做品牌点缀和高亮，不做大面积背景。页面应以白色和浅灰为主，黑色用于结构文字和少量品牌标识，金色只用于主操作、选中态、焦点和关键强调。

## 当前结构诊断

### 应用壳层

- `admin-ui-daisy/src/App.tsx` 只负责登录态分流。
- `admin-ui-daisy/src/components/Dashboard.tsx` 维护 `activeTab`，用 `window.history.pushState` 同步 `/console/...` 路径。
- `admin-ui-daisy/src/types/ui.ts` 定义 `TabKey`、路径片段和页面标题。
- 所有页面共用一个 `max-width` 和一套 padding，缺少 dashboard、data table、resource grid、settings 等页面类型。

这会带来一个明显问题：总览页、凭据资源页、表格页和配置中心被同一个内容容器约束，但它们的信息密度和交互模型完全不同。

### 页面类型

现有页面大致分为四类：

- 看板型：`UsageDashboardPanel`，包含指标、图表、排行和状态摘要。
- 数据查询型：`UsagePanel`、`PricingPanel`、`AuditPanel`，以筛选、表格、详情弹层为主。
- 资源管理型：`CredentialsPanel`、`ProxyPanel`、`ExternalPoolsPanel`，以资源卡片、批量操作、编辑弹层为主。
- 配置中心型：`ConfigPanel`，大量设置项、说明、保存条和分类切换。

现在这些类型没有共享页面骨架。比如 `UsageDashboardPanel` 自己定义 `Panel`，其他页面使用 `SectionCard`；`ExternalPoolsPanel` 自己定义 `PolicyBlock/FormSection/ToggleRow`；`ConfigPanel` 又定义 `ConfigGroup/ToggleField/NumberField`。结果是页面层级、边距、标题和操作区都不一致。

### 组件体量

前端页面文件普遍过大：

- `CredentialDialogs.tsx` 超过 1600 行。
- `ConfigPanel.tsx` 超过 1400 行。
- `ExternalPoolsPanel.tsx` 超过 1200 行。
- `UsagePanel.tsx`、`CredentialCard.tsx` 都超过 1000 行。
- `CredentialsPanel.tsx` 接近 1000 行。

这说明很多页面已经变成单文件子应用。后续完整重构应该拆出 page shell、summary、toolbar、resource card、detail drawer/modal、settings section 等稳定层，而不是继续在页面文件内部加局部组件。

## 视觉问题

当前 CSS 仍然把暖色和金色作为基础视觉语言：

- `--page-bg: #f4efe6`
- `--page-bg-2: #ebe2d2`
- `--surface-solid: #fffdf8`
- DaisyUI `base-100/base-200/base-300` 也是奶白、米黄、砂色。
- 侧栏、顶栏、卡片头、配置分组、表格头、登录页视觉区都存在暖色渐变。

这不符合“黑金不要黑色背景，黑金只是点缀”的要求。新的色系应先建立中性系统色，再在有限位置使用金色。

目标色系：

- 页面背景：`#F6F7F9`
- 主面板：`#FFFFFF`
- 次级面板：`#FAFAFB`
- 悬浮/弱背景：`#F3F4F6`
- 边框：`#E5E7EB`
- 强边框：`#D1D5DB`
- 主文字：`#111827`
- 次级文字：`#374151`
- 弱文字：`#6B7280`
- 品牌黑：`#111111`
- 金色强调：`#B88A2E`
- 金色 hover：`#9A6A16`
- 金色弱背景：`rgba(184, 138, 46, 0.08)`
- 成功：`#16A34A`
- 警告：`#D97706`
- 错误：`#DC2626`
- 信息：`#2563EB`

## 交互问题

### 凭据展开

`CredentialCard` 当前在 grid 内部展开详情。展开后单张卡片变高，但同一行其他卡片仍是摘要高度，视觉上像网格被撑坏。更合理的交互有两种：

- 首选：固定高度摘要卡片，点击后右侧 detail drawer 展示详情和编辑动作。
- 过渡方案：展开卡片跨整行显示，避免同一行出现一高多矮。

第一阶段采用过渡方案，先解决网格塌陷和默认信息不足；后续再把详情编辑完全迁移到 drawer。

### 配置中心

`ConfigPanel` 现在是一个 `SectionCard` 里套访问 Key、只读代理、保存条、tab、多个 `ConfigGroup`。每个 `ToggleField` 又是独立 `Card bordered`，内部再有边框和背景。用户看到的是一堆盒子嵌套盒子。

目标结构：

- 页面本身是 settings layout。
- 左侧是分类导航，右侧是当前分类内容。
- 分组内部用行和分隔线，不再每个字段都是小卡片。
- 说明保留，但写成“作用、影响范围、什么时候生效”，避免晦涩内部实现词。

### 原生控件

源码里已经没有 `<select>/<option>/<optgroup>`，也没有 `confirm()/alert()/prompt()`。现有自定义 `Select` 和 `ConfirmProvider` 可以继续保留，但需要样式扁平化，并把弹层、下拉菜单视觉统一到新的中性系统。

### 菜单切换

左侧菜单应继续是真实链接，点击后更新路由并切换 tab。实现上 `Sidebar` 已用 `<a href>` 加 `preventDefault`，问题更可能来自覆盖层、z-index 或 dev 端口不一致造成访问的不是最新构建。后续验证必须只使用 9022。

## 重构策略

第一阶段先处理全局基础层：

- 调整 `package.json`，开发和预览端口统一为 9022。
- 重建 DaisyUI 主题和 CSS token，去掉大面积米黄、暖色渐变、浮雕阴影。
- 重构侧栏、顶栏、按钮、输入框、表格、弹层、卡片、badge、toolbar、settings group 的基础样式。
- 给页面配置增加 layout 类型，让 `Dashboard` 根据页面类型决定内容宽度和页面 class。
- 凭据 grid 默认桌面三列，超宽才四列。
- 凭据卡片默认展示更多摘要信息，展开时跨整行。

第二阶段迁移页面结构：

- 看板页统一到共享 dashboard panel / metric tile。
- 数据页统一到 shared table workspace。
- 资源页统一到 resource grid + selected detail pattern。
- 设置页改为真正的 settings layout，减少嵌套卡片。

第三阶段拆文件：

- `CredentialsPanel` 拆出 summary、toolbar、grid、batch bar。
- `CredentialCard` 拆成 summary card、detail content、edit dialogs。
- `ConfigPanel` 拆成 access、startup proxy、dispatch、cache、usage、compat。
- `UsagePanel` 拆成 summary、records workspace、detail modals。
- `ExternalPoolsPanel` 拆成 policy workspace、pool list、pool dialogs。

## 验证要求

- 不启动 9026。
- 只访问 `http://127.0.0.1:9022/console`。
- 登录 Key：`admin123`。
- 前端构建后必须重新构建 Rust release，因为后端嵌入 `admin-ui-daisy/dist`。
- 检查 `/console/dashboard`、`/console/credentials`、`/console/config`、`/console/usage`、`/console/proxies`、`/console/external-pools`。
- 检查左侧菜单点击是否切换页面并同步路由。
- 扫描确认没有原生 select 和浏览器原生弹层调用。
