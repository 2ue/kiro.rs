# 管理端账号体验与缓存边界展示实施方案

## 适用范围

本方案处理管理端账号列表、账号详情、配置页面、弹层、顶部导航、侧边菜单、版本号、术语统一、缓存统计展示和高并发参数解释。

本方案不是视觉稿，但必须给后续 UI 重构明确约束，避免再次出现歧义。

## 来源项目与学习点

- `Kiro-account-manager`：账号管理体验、导入、测试、状态展示值得参考。
- `Kiro-Go`：账号状态字段更直观，适合管理端展示 account tier、quota、overage。
- 当前项目 UI：已经有账号、外部账号、模型、运行态、usage、配置等页面，但布局和术语需要统一。

## 当前项目现状

用户明确提出的问题包括：

- 不要把业务逻辑说明写到页面上。
- 配置说明要说人话。
- 不要假版本号。
- 顶部 nav 固定。
- 顶部 nav 和左侧菜单高度对齐。
- 弹层固定顶部标题区域。
- 不要使用原生 select、confirm 等组件。
- 黑金只是点缀和高亮，不是大面积黑色或米黄色背景。
- 设置页不要多层嵌套卡片。
- 保存按钮应该在每个 tab 顶部或顶部固定区域。
- 账号卡片默认展示更多信息，一行默认 3 个。
- 展开交互不能让同一行高度异常。
- 操作按钮不要放到弹窗内部才可见。

## 目标

- 管理端统一使用 account / 账号。
- 所有说明文案描述“这个设置有什么作用”，不得暴露内部业务模块细节。
- 页面布局减少嵌套卡片和无意义色块。
- 账号卡片信息密度更高但不拥挤。
- 配置页按任务重新分组。
- 缓存、调度、限流配置用人能理解的语言说明。
- 版本号来自真实构建信息。

## 非目标

- 不在本方案里重写业务 API。
- 不修改后端字段名，除非另有迁移方案。
- 不增加营销风格 landing page。
- 不使用大面积黑色背景或米黄色背景。

## 涉及文件

典型文件：

- `ui/src/features/credentials/*`
- `ui/src/features/external-pools/*`
- `ui/src/features/runtime/*`
- `ui/src/features/models/*`
- `ui/src/features/usage/*`
- `ui/src/features/settings/*`
- `ui/src/components/*`
- `ui/src/styles/*`
- 后端版本信息接口对应文件

## 设计 Token 约束

色系必须遵守：

```text
background: #F7F8FA
surface: #FFFFFF
surfaceAlt: #F2F4F7
border: #D8DEE8
textPrimary: #1F2933
textSecondary: #5D6675
textMuted: #8A94A6
accentGold: #B8872D
accentGoldHover: #9A6F22
accentDark: #111827
success: #1F8A5B
warning: #B7791F
danger: #C2413A
info: #2563A8
focus: #B8872D
```

规则：

- 黑金只用于高亮、选中、关键按钮、焦点态。
- 页面大背景必须是浅灰或白，不得大面积黑色。
- 不得大面积米黄色。
- 不使用粉色作为系统状态色。
- 按钮不得有浮雕效果。
- 卡片圆角建议 8px，不能因 hover 造成边框缺角。

## 账号卡片布局

列表默认：

- 桌面宽度大于 1200px：每行 3 个。
- 900px 到 1200px：每行 2 个。
- 小于 900px：每行 1 个。

默认展示字段：

- 账号名称。
- 账号 ID。
- tier：Free / Pro / Power / Unknown。
- 启用状态。
- 当前 in-flight / max concurrent。
- RPM usage。
- 最近成功时间。
- 最近错误摘要。
- 当前模型支持数量。
- sticky/session 状态只在详情里展示，不在卡片上用晦涩词。

操作按钮：

- 常用操作直接在卡片右上或底部 action bar：编辑、测试、启用/停用、更多。
- 不得把主要操作藏到详情弹窗内部。

展开交互：

- 不使用“单卡展开撑高同一行”的方式。
- 推荐使用右侧详情抽屉或独立详情页。
- 如果必须原地展开，必须整行使用 masonry-free layout，不能导致同一行卡片高度错乱。

## 配置页重新划分

Tab 建议：

1. General
2. Accounts
3. Dispatch
4. Rate Limits
5. Cache
6. Streaming
7. Errors & Logs
8. Advanced

每个 tab 顶部必须有固定 action bar：

- Save
- Reset changes
- Last saved time

说明文案示例：

- `Dispatch wait`：`How long a request may wait for an account to become ready before returning a retryable error.`
- `Max queued requests`：`The maximum number of requests that can wait at the same time. Higher values can absorb bursts but increase memory use and waiting time.`
- `Single-account RPM`：`The maximum number of requests one account may start per minute.`
- `Stream idle timeout`：`How long a stream may stay silent before it is treated as stalled.`

不得写：

- 备用账号来源的内部名称
- “内部调度链路”
- 账号底层存储快照
- “sticky 换号”
- 跨来源账号救援链路

## 弹层规范

所有弹层必须：

- 顶部标题区域固定。
- 底部 action 区固定或顶部 action 区固定。
- 内容区域独立滚动。
- 关闭、取消、确认按钮使用项目组件，不使用原生 confirm。
- 表单选择使用项目 Select 组件，不使用原生 select。
- 错误提示在字段附近展示，不使用 alert。

## 版本号显示

版本号必须来自真实构建信息：

- 后端从 `Cargo.toml` package version 或 build script 注入。
- 前端从后端 `/api/version` 或等价接口读取。
- 不得写死假版本。
- 如果版本未知，显示 `Version unavailable`，不得显示伪造值。

建议接口：

```json
{
  "version": "0.1.23",
  "gitCommit": "abc1234",
  "builtAt": "2026-06-26T00:00:00Z"
}
```

## 缓存边界展示

管理端 Cache 页面必须展示：

- 当前 cache entry 数。
- 最大 entry 数。
- 估算内存。
- 1 小时 hit/miss。
- 淘汰次数。
- 每个路由的 cache 状态。

文案必须说明作用：

```text
Cache entries help estimate repeated prompt reuse. Limits prevent memory growth during long sessions.
```

## 测试方案

必须使用浏览器自动化或截图检查：

- 登录页。
- 总览页。
- 账号列表 3/2/1 列。
- 账号详情抽屉。
- 配置每个 tab。
- 弹层标题固定。
- 顶部 nav 滚动后固定。
- 左侧菜单点击路由切换。
- 版本号来自接口。
- 无原生 select、confirm、alert。

建议测试：

- `ui_nav_stays_fixed_on_scroll`
- `ui_sidebar_routes_are_clickable`
- `ui_account_cards_have_no_missing_corners_on_hover`
- `ui_account_grid_defaults_to_three_columns`
- `ui_settings_tab_has_sticky_save_bar`
- `ui_modals_have_fixed_header`
- `ui_version_is_loaded_from_api`

## 验收标准

- 页面不出现内部晦涩概念。
- 所有可见“账号”文案统一。
- 配置说明能让非代码维护者理解作用。
- 账号卡片 hover 不缺边框。
- 账号详情不破坏列表网格高度。
- 顶部 nav 固定且与侧边菜单对齐。
- 版本号真实。

## 风险与回滚

风险：

- UI 重构影响现有操作路径。
- 术语替换误改 API 字段名。

规避：

- 只改显示文案，不改后端字段名。
- 每个页面单独截图验收。
- 保留旧接口适配层。

回滚：

- UI 改动按页面拆分提交。
- 发现某页面问题时只回滚该页面，不回滚后端。

## 不得做的事项

- 不得使用原生 confirm、alert、select。
- 不得写死假版本号。
- 不得把业务逻辑说明写到页面上。
- 不得大面积使用黑色或米黄色背景。
- 不得用嵌套卡片堆配置项。
- 不得把主要操作藏到弹窗深处。

## 后续可选扩展

可以增加账号批量操作和测试结果面板，但必须先完成基础布局、术语和组件规范。
