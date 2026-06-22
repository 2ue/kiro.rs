# Admin UI 现代化分析与重构记录

日期：2026-06-22

## 结论

当前项目有两个管理后台：

- `/admin`：旧版 `admin-ui`。
- `/console`：新版 `admin-ui-daisy`，也是当前实际改造目标。

本次重构聚焦 `/console`。新版后台的业务功能已经比较完整，问题不在功能缺失，而在 UI 视觉系统不统一：默认组件痕迹重、层级弱、页面之间细节不一致、配置页说明偏生硬、登录页和控制台主体不在同一套视觉语言里。

最终方向是单一浅底黑金简约风格：浅暖灰页面底、黑色结构文字、金色强调线和主操作，不使用黑色背景，不保留其他主题，也不提供主题切换入口。

## 参考方向

本次参考现代后台项目的布局和信息层级，不直接复制模板代码。

1. shadcn/ui Dashboard Blocks  
   参考点：紧凑指标卡、清晰工具条、面板化数据区域。

2. Ant Design Pro  
   参考点：稳定的侧栏、顶栏、内容区分工，以及企业后台的信息密度。

3. Tremor  
   参考点：数据面板、状态信号、排行和图表的表达方式。

4. TailAdmin  
   参考点：Tailwind 后台模板的整体包装、卡片层级和操作栏。

5. Vue Vben Admin  
   参考点：长期维护型后台的菜单组织和配置页面结构。

## 当前 UI 问题

1. 主题不够明确  
   原始 UI 偏默认后台模板感，颜色和组件形态没有形成统一品牌记忆点。上一版深色黑金虽然有方向，但用户明确要求不要黑色背景，因此需要改为浅底黑金。

2. 页面表层统一，但组件细节分散  
   总览、凭据、配置、用量等页面都有各自的卡片、筛选区和小面板。只改 `body` 背景或登录页无法解决整体质感。

3. 配置页默认组件味明显  
   配置页大量使用默认卡片、折叠面板和 boxed tabs，说明文字虽然必要，但需要更像人能直接理解的配置说明，而不是实现细节说明。

4. 菜单交互必须保持可靠  
   左侧菜单应该是真实可点击链接，并在前端切换时同步路由；同时要避免装饰层遮挡菜单点击。

5. 页面文案要讲用途，不讲内部逻辑  
   配置页面保留必要说明，但说明重点是“这个设置影响什么、什么时候生效、怎么理解”，不是把内部处理链路直接写给用户。

## 重构原则

1. 单主题  
   只保留 `blackGold`。不保留黑白主题、不保留其他配色、不提供主题切换器。

2. 浅底黑金  
   背景使用浅暖灰和白色面板，黑色用于结构和文字，金色用于主按钮、焦点、导航激活、指标强调。

3. 不使用网格背景  
   页面背景改为干净的线性浅底，不使用网格、暗色底或大面积装饰光斑。

4. 先统一视觉系统，再处理页面细节  
   全局 token、按钮、表单、表格、弹窗、卡片、侧栏、顶栏先统一，业务页面继承同一套视觉语言。

5. 不写假版本和假状态  
   侧栏底部不显示写死版本号。没有后端真实状态数据时，不伪造“服务状态”。

6. 不使用浏览器原生交互控件  
   下拉选择使用页面内自定义 `button + listbox`；确认操作使用自定义 Modal，不调用浏览器原生 `confirm`。按钮保持平面样式，不做浮雕、抬起或重阴影效果。

## 本次代码修改

### 单主题与入口

文件：

- `admin-ui-daisy/tailwind.config.ts`
- `admin-ui-daisy/index.html`
- `admin-ui-daisy/src/types/ui.ts`
- `admin-ui-daisy/src/App.tsx`
- `admin-ui-daisy/src/components/Dashboard.tsx`

修改：

- DaisyUI 只保留 `blackGold` 主题。
- 默认主题固定为 `blackGold`。
- 移除控制台内主题切换入口。
- `/console` 根路径进入总览，子菜单路径按页面映射同步。
- 移动端和桌面端都使用同一套浅底黑金外壳。

### 全局视觉系统

文件：`admin-ui-daisy/src/styles.css`

修改：

- 重建浅底黑金 token：页面底色、surface、边框、文字、金色强调、阴影。
- 移除黑色背景、网格背景和大面积装饰光斑。
- 统一按钮、输入框、选择框、文本框、表格、弹窗、滚动条。
- 主按钮改为平面金色按钮，去掉浮雕渐变、悬浮抬起和按钮阴影。
- 新增自定义 `Select`，使用页面内 listbox，不渲染原生 `<select>`、`<option>`、`<optgroup>`。
- 新增 `ConfirmProvider/useConfirm`，危险操作使用自定义确认弹窗，不再调用浏览器 `confirm()`。
- 新增或重构公共样式：
  - `.app-shell`
  - `.auth-shell`
  - `.top-bar`
  - `.sidebar-shell`
  - `.section-card`
  - `.stat-card`
  - `.metric-tile`
  - `.dashboard-toolbar`
  - `.credential-card`
  - `.setting-card`
  - `.config-group`
  - `.toolbar-panel`
  - `.credit-summary-panel`

### 布局组件

文件：

- `admin-ui-daisy/src/components/layout/Sidebar.tsx`
- `admin-ui-daisy/src/components/layout/TopBar.tsx`
- `admin-ui-daisy/src/components/ui/index.tsx`

修改：

- 侧栏改为浅底黑金导航，激活菜单有金色提示线。
- 侧栏底部改为真实入口 `/console`，不显示假的版本或状态。
- 顶栏增加控制台识别和结构化标题区域。
- 通用 `StatCard`、`SectionCard`、`EmptyState`、`ModalShell` 统一面板质感。

### 登录页

文件：`admin-ui-daisy/src/components/LoginPage.tsx`

修改：

- 登录页改为浅底黑金视觉，不使用黑色背景。
- 文案只说明入口用途：查看状态、维护资源、调整设置。
- 不显示写死版本号。

### 凭据页

文件：

- `admin-ui-daisy/src/components/CredentialsPanel.tsx`
- `admin-ui-daisy/src/components/credentials/CredentialCard.tsx`

修改：

- 积分统计区域改为统一 summary panel。
- 搜索、筛选、排序和批量操作改为统一 toolbar panel。
- 凭据卡 header、展开详情和 meta 信息块统一视觉层级。
- 保留密集信息，但改善可扫描性和 hover 层级。

### 配置页

文件：`admin-ui-daisy/src/components/ConfigPanel.tsx`

修改：

- 配置分组从默认折叠面板改为固定 setting group。
- 分类 tabs 从 DaisyUI boxed tabs 改为自定义分段按钮。
- 保存条改为 sticky 浅底黑金操作条。
- 配置说明保留，但表达以“用途、影响范围、生效方式”为主。

### 原生控件替换范围

文件：

- `admin-ui-daisy/src/components/ui/index.tsx`
- `admin-ui-daisy/src/components/common.tsx`
- `admin-ui-daisy/src/main.tsx`
- `admin-ui-daisy/src/components/CredentialsPanel.tsx`
- `admin-ui-daisy/src/components/ConfigPanel.tsx`
- `admin-ui-daisy/src/components/CredentialDialogs.tsx`
- `admin-ui-daisy/src/components/AccountValidationPanel.tsx`
- `admin-ui-daisy/src/components/ExternalPoolsPanel.tsx`
- `admin-ui-daisy/src/components/UsagePanel.tsx`
- `admin-ui-daisy/src/components/ProxyPanel.tsx`
- `admin-ui-daisy/src/components/PricingPanel.tsx`
- `admin-ui-daisy/src/components/credentials/CredentialCard.tsx`

修改：

- 替换所有 `react-daisyui` 的 `Select` 使用。
- 替换手写 `<select>/<option>/<optgroup>`。
- 替换所有 `confirm()` 和 `window.confirm()`。
- 保留普通输入框、数字输入框和复选框，因为它们是表单输入本体，不是浏览器原生弹层或系统下拉。

## 构建与验证要求

前端构建需要使用 Node 22 直接运行 Vite，因为当前环境中 `pnpm exec vite` 可能落到 Node 16，导致 Vite 5 的 `crypto.getRandomValues` 报错。

推荐构建命令：

```bash
cd admin-ui-daisy
/Users/yuanfeijie/.volta/tools/image/node/22.22.3/bin/node node_modules/vite/bin/vite.js build
```

后端嵌入 `admin-ui-daisy/dist`，所以前端构建后还需要重新编译 Rust release，并只重启 9022：

```bash
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk \
PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin:/Users/yuanfeijie/.cargo/bin:$PATH \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang \
cargo build --release
```

```bash
pid=$(lsof -tiTCP:9022 -sTCP:LISTEN); if [ -n "$pid" ]; then kill $pid; fi
nohup ./target/release/kiro-rs -c config.json > /tmp/kiro-rs-9022.log 2>&1 &
```

验证目标：

- `http://127.0.0.1:9022/console`
- 登录 Key：`admin123`
- 9022 有新 UI。
- 9026 不启动、不占用。
- 左侧菜单可点击，并且路由随菜单变化。
- 配置页、凭据页、用量页、审计页至少做一次实际打开检查。
