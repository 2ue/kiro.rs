# UI 功能对齐审计（ui vs admin-ui / admin-ui-daisy）

> 本文档目的：记录新版前端 `ui/` 相对两套老版前端的**逐页功能差异**，作为后续代码优化（回补缺失、修复回退、保留有意重构）的唯一依据。
>
> **本文档力求自包含**：即使不读源码、不看对话记录，也能据此准确定位问题并实施修复，不产生误解。

审计时间：2026-06-30。审计方式：9 个功能域逐页逐字段对比三套源码。

---

## 0. 阅读前必须了解的背景（避免误判）

### 0.1 三套 UI 是什么关系

仓库根目录下有三套并存的前端，是**同一个 Kiro 控制台的三代实现**，后端 API 完全相同：

| 目录 | 代号 | 技术栈 | 定位 |
|---|---|---|---|
| `admin-ui-daisy/` | **daisy 老版** | React + react-daisyui + Tailwind v3 | 功能最全的老版，**本审计的功能基线** |
| `admin-ui/` | **中间版** | React + Radix + CVA + Tailwind v3 | 过渡版，个别能力仅它独有（会特别标注） |
| `ui/` | **新版（被审计对象）** | React 18 + shadcn/Radix + Tailwind v4 + react-router v6 + recharts | 最新完全重构版，四域 IA |

### 0.2 基线定义（判断"缺失"的标准）

- **以 `admin-ui-daisy` 为功能基线**。某能力在 daisy 老版有、在 `ui` 没有 → 记为「缺失/回退」。
- 三套都没有的能力（如列表导出、代理列表搜索）→ **不算缺失**，不要在本文档之外臆造需求。
- 仅 `admin-ui` 中间版独有、daisy 没有的能力 → 单独标注为「中间版独有」，是否回补取决于是否要做"功能并集"，默认**不算 ui 缺失**。

### 0.3 关键约束（修复时必须遵守）

- **后端 API 与业务逻辑一律不动**，本文档所有问题都是纯前端问题。
- 前端构建/类型检查必须用 node 22：
  - 类型检查：`/Users/yuanfeijie/.volta/bin/volta run --node 22.23.0 pnpm check`
  - 构建：`/Users/yuanfeijie/.volta/bin/volta run --node 22.23.0 pnpm build`
  - 默认 shell 的 node 是 16，会失败。工作目录 `ui/`。
- 复用层不要改动：`@/api/*`、`@/hooks/*`、`@/types/api`、`@/lib/*`。`@/` 映射到 `ui/src/`。
- 设计系统规则见 `ui/DESIGN_SYSTEM_BRIEF.md`：只用语义色 token、弹窗用 `ModalShell`、表格用 `<Table>`、危险操作用 `useConfirm()`。

### 0.4 行号说明

文中行号基于审计时点的代码快照。修复前请以实际文件内容为准（用关键代码片段定位，行号仅作辅助）。

---

## 1. 总体结论

**`ui` 未完整覆盖老版能力。** 9 个域中 5 个域存在实质缺失/回退，其中 1 项为确定的代码 bug。核心 CRUD / 查询 / 明细链路齐全；缺失集中在"细节能力"与"保存期校验"。

### 1.1 逐页覆盖矩阵

| 域 | ui 页面文件 | 覆盖结论 | 实质缺失 |
|---|---|---|---|
| 校验 | `features/validation/validation-page.tsx` | ✅ 完整（有增强） | 0 |
| 代理 | `features/proxies/` | ✅ 完整（新增卡片展开绑定） | 0（2 处待确认后端语义） |
| 外部池 | `features/external-pools/` | ⚠️ 近完整 | 1（预设缩水） |
| 价格/模型 | `features/models/models-page.tsx` | ⚠️ 近完整 | 1（告警降级） |
| 总览 | `features/overview/overview-page.tsx` | ⚠️ 有缺失 | 3 |
| 审计 | `features/audit/audit-page.tsx` | ⚠️ 有缺失 | 2 + 1 bug |
| 用量 | `features/usage/` | ⚠️ 有缺失 | 3 |
| 配置 | `features/runtime/` + `features/security/` | ⚠️ 字段全，校验缺 | 2（保存逻辑） |
| 账号 | `features/credentials/` | ❌ 有 bug + 退化 | 1 bug + 4 |

### 1.2 修复优先级（按"正确性"而非"展示"排序）

正确性/数据问题（高）：
- **P0** 账号卡「查询」额度按钮 no-op（§9.1）
- **P0** 配置保存缺 2 条冷却交叉校验 + 漏 normalizePayloadShaping（§8.1、§8.2）
- **P1** 用量清理丢失「清理至当前时刻全部」能力（§7.1）
- **P1** 单条添加账号弹窗丢 4 组输入（§9.2）

展示/交互缺失（中，可打包成一轮"细节回补"）：
- 总览 3 项（§5）、审计 2 项 + 1 bug（§6）、外部池预设缩水（§4.1）、模型告警降级（§4.2）

---

## 2. 校验域 — ✅ 完整

**ui 文件**：`features/validation/validation-page.tsx`
**基线**：`admin-ui-daisy/src/components/AccountValidationPanel.tsx`

**结论**：0 缺失。老版全部能力（校验现有凭据 all/enabled/disabled + force、校验外部凭据粘贴/文件、批量校验、结果分组、额度/订阅字段、错误聚合、checkedAt）均已覆盖，并有增强：统计卡补回"升级/无变化"（6 张 vs 老版 4 张）、解析失败红字提示、existing 限制说明 Callout、两 section 独立 pending。

**唯一交互形态变更（非回退，不需改）**：老版"系统复查"是三个直接触发按钮（全部/仅启用/仅禁用）；ui 改为"范围按钮组 + 单个执行按钮"，scope 与 force:true 完全一致。

---

## 3. 代理域 — ✅ 完整

**ui 文件**：`features/proxies/proxies-page.tsx`、`features/proxies/proxy-components.tsx`
**基线**：`admin-ui-daisy/src/components/ProxyPanel.tsx`

**结论**：0 功能缺失。列表字段、增删改、测试连通性、绑定账号批量管理（syncBindings + Promise.allSettled）全部保留。**增强**：ui 卡片新增"账号绑定"展开态，可不进编辑弹窗就批量绑定/解绑（老版只能在编辑弹窗内做）。

**2 处需向后端确认语义（不是 bug，是文案/传参可能与后端不一致）**：

1. **卡片"测试"按钮传参**（`proxy-components.tsx` 约 411-413）：老版/中间版传空 `request: {}`，由后端按 `resource.id` 用已存配置测试；ui 改为显式传 `{ proxyUrl, proxyUsername, proxyPassword }`（取自 resource 对象）。
   - 风险：若某些部署下 list 接口**不回传明文密码**，带密码代理的卡片测试会缺密码导致误判。
   - 待确认：后端 list 是否始终回传 `proxyPassword`。若不保证，应改回传空 request。

2. **删除确认文案语义**（`proxy-components.tsx` 约 425-431）：
   - 老版文案：「如果仍有账号绑定，后端会拒绝删除」（= 后端**拒绝**）。
   - ui 文案：`credentialCount > 0` 时提示「删除后这些账号会回退到全局代理或直连。确认删除?」（= 后端**允许删除并解绑**）。
   - 两者描述的后端行为**相反**。待确认后端真实语义后，保留正确的一条，避免误导操作者。

**注**：「代理作为账号筛选维度」不在本域文件内（属账号域），账号域已确认筛选面板含"代理"动态维度。

---

## 4. 外部池域 / 价格模型域

### 4.1 外部池 — ⚠️ 模型映射快捷预设缩水（1 项实质退化）

**ui 文件**：`features/external-pools/external-pool-utils.ts`（预设表）、`external-pools-page.tsx`、`external-pool-form-modal.tsx`
**基线**：`admin-ui-daisy/src/components/ExternalPoolsPanel.tsx`（中间版 `admin-ui/src/components/external-pools-panel.tsx` 两表均完整，可作回补参照）

**缺失（退化，非彻底丢失——规则仍可手填或用"快捷导入"补回）**：
- `DIRECT_MODEL_MAPPING_PRESETS`：**27 条 → 17 条**。丢失：Sonnet 4.5/4.7/4.8 点号、Opus 4.5 点号、Opus 4.7/4.8 点号、Haiku 4.5 点号，以及全部 thinking 预设（Opus 4-5 thinking→4.5、Opus 4.6 thinking、Opus 4.8 thinking）。
- `PROCESSED_MODEL_MAPPING_PRESETS`：**20 条 → 16 条**。丢失：Opus 的 4 条 thinking 预设（4.5 thinking→4-5、4-5 thinking、4.6 thinking→4-6、4.8 thinking）。

**修复指引**：以 `admin-ui` 中间版的 `external-pools-panel.tsx` 两张完整预设表为准，把缺失条目补回 `ui/src/features/external-pools/external-pool-utils.ts` 的两个常量。补回后用 `pnpm check` 验证类型。

**次要（纯引导文案，可选）**：用量补偿块（"用量补偿" PolicyBlock）删掉了老版 HintBox（"生效条件：请求进入外部账号…"），两个 FormSection（缓存读写补偿、输出用量补偿）没带 description。功能等价，仅少说明文字。

**有意变更（不要改）**：
- 新增 `preservePath`（保留请求路径）开关 —— ui 增强，老版无。
- Base URL 文案、概览区改 StatCard+ProgressRing、策略分区改短标题 —— 字段一一对应，无增减。

### 4.2 价格/模型 — ⚠️ 同步失败告警降级（1 项实质退化）

**ui 文件**：`features/models/models-page.tsx`
**基线**：`admin-ui-daisy/src/components/PricingPanel.tsx`

**缺失/退化**：
1. **`lastError` 告警可见性下降**（约 337-339、350-352）：老版用独立 `WarningAlert`/`WarningBox`（AlertTriangle + 整块黄色 alert）；ui 压成状态卡里一行 `text-xs text-destructive truncate max-w-xs` 截断小字。长错误信息会被截断，同步失败不易察觉。
   - **修复指引**：把 lastError 恢复为醒目告警块（可用 `Callout tone="error"`），不要截断完整错误。
2. **来源标签映射丢失**（约 69-82）：老版 `sourceLabel` 把 `kiro`→"服务同步"、`seed`→"Seed"；ui 去掉这两条映射，`kiro`/`seed` 源会显示成原始裸字符串。
   - **修复指引**：补回 `kiro`→"服务同步"、`seed`→"Seed" 映射。

**有意变更（不要改）**：表结构重组（价格目录表 6 列 + 模型清单表 12 列，能力列与价格列合并）、新增模型名模糊匹配（`pricingIndex`/`findPricing`/`modelKeyAliases`，处理 `/` 前缀与 4.5↔4-5 点号横线互转，是增强）、盈亏分析迁往「成本」页、Modal 价格提示文案合并、保存时 model 名 `.toLowerCase()` 规范化 + 空值校验。

---

## 5. 总览域 — ⚠️ 3 项缺失

**ui 文件**：`features/overview/overview-page.tsx`
**基线**：`admin-ui-daisy/src/components/UsageDashboardPanel.tsx`（中间版 `admin-ui/src/components/usage-dashboard-panel.tsx`）

**缺失**：
1. **趋势面板的聚合汇总丢失（实质数据）**：老版 `SeriesChart` 副标题为 `${formatNumber(totalRequests)} 请求 · ${formatUsd(totalCost)}`，把该趋势窗口自身的总请求量与总费用直接显示。ui 的 `TrendSection`（约 131-181）副标题改为静态文案（"按小时聚合的请求量与错误"），actions 只剩错误数 Badge。**24h/7d 两条趋势的总请求数与总费用都不再展示**。7 天趋势的费用合计与顶部"估算费用"卡（选中窗口）不是同一值，属真实信息丢失。
   - **修复指引**：在 `TrendSection` 两个 `SectionCard` 的 description 或 actions 里补回该窗口的"总请求 · 总费用"汇总（对 hourly/daily 数组做 reduce）。
2. **趋势图单点费用 hover 丢失**：老版每根柱子 `title` 含"请求/错误/费用"三项；ui 用 recharts，series 只配了 `requests`/`errors` 两条，`valueFormatter` 只格式化数字（约 148-154、172-178），tooltip 不含每个时间点的费用。
   - **修复指引**：给图表 series 增加 cost 维度或在 tooltip formatter 中带出 `cost`（`seriesPointToChartRow` 已含 `cost` 字段，只是未在图表用上）。
3. **Sticky 回退告警文案丢失**：老版底部状态栏在 `fallbackFromStickyRequests > 0` 时显示"检测到 Sticky 回退，说明粘度命中的账号不可用或并发不可用"。ui 底部栏（约 999-1019）无此条件告警（运行信号面板只有 Sticky 回退比例条，无这句显式提示）。
   - **修复指引**：在底部状态栏按 `summary.fallbackFromStickyRequests > 0` 条件补回该告警文案。

**有意变更（不要改）**：耗时卡主值由"均值"改为"P95"（标题同步改为"P95 耗时"，均值降为副标题）、趋势图改 recharts 面积/柱状图且 requests/errors 拆两条 series、状态分布+用量来源合并为 Tab 切换面板、维度排行截断前 10 + 新增占比列、时区/生成时间移到底部栏、异常摘要前 5 条 + 错误率 Callout、普遍改 `formatCompact`（完整数字进 title）。**新增（老版没有）**：账号池状态面板、请求量/缓存卡内嵌 Sparkline、isFetching 刷新动画。

---

## 6. 审计域 — ⚠️ 2 项缺失 + 1 bug

**ui 文件**：`features/audit/audit-page.tsx`
**基线**：`admin-ui-daisy/src/components/AuditPanel.tsx`

**缺失**：
1. **手动刷新按钮被删**：老版工具栏有独立「刷新」按钮（`logs.refetch()`）；ui 的 ToolbarActions（约 274）只剩 pending 旋转图标，无可点击刷新入口。本域无自动刷新，等于用户无法主动触发重拉。
   - **修复指引**：在 ToolbarActions 补回「刷新」按钮，onClick 调 `logs.refetch()`。
2. **搜索覆盖维度被砍**：老版 haystack 含 `objectLabel(record.objectType)`（对象中文名）和 `record.errorMessage`；ui 搜索过滤（约 224-233）只匹配 action/actionLabel/actor/objectType/objectId。按错误信息或对象中文名搜不到。
   - **修复指引**：把 `objectLabel(objectType)` 和 `errorMessage` 加回搜索 haystack。

**附带 bug + 行为变更**：
- **搜索大小写 bug**（约 228）：`r.action.includes(lower)` 用原始大小写的 `action` 去匹配已 `toLowerCase` 的关键词；老版统一把整个 haystack 转小写再比。含大写字母的 action 名按小写搜会漏。修复：`r.action.toLowerCase().includes(lower)`。
- **清除筛选不重置分页**（约 248）：老版 `clearFilters` 会 `setPage(1)`；ui 只清筛选条件不重置页码，可能停在空页。修复：`clearFilters` 内补 `setPage(1)`。
- **文案变更（不要改）**：`pricing` 分类 label 由"模型价格"改为"价格同步"，映射 key 未变。

**增强（不要改）**：详情弹窗新增"结果"成功/失败 Badge、整行可点击打开详情。

**澄清**：老版本域本就没有"时间范围过滤""导出""自动定时刷新"，这三项不构成 ui 缺失。

---

## 7. 用量域 — ⚠️ 3 项缺失

**ui 文件**：`features/usage/usage-page.tsx`、`usage-detail-modal.tsx`、`usage-cleanup-modal.tsx`
**基线**：`admin-ui-daisy/src/components/UsagePanel.tsx`（含内嵌 UsageBillingModal/UsageDetailModal/UsageCleanupModal）

**缺失**：
1. **【P1 功能回退】清理丢失"清理至当前时刻全部"能力**（`usage-cleanup-modal.tsx:30`）：
   - 现状：`olderThanDays: Math.max(1, Math.min(3650, Math.floor(Number(olderThanDays)) || 30))` —— 最小钳到 1 天。
   - 老版：`parseCleanupInteger(olderThanDays, 7, 0)` 允许填 `0`，语义为"以任务启动时刻为 cutoff，清理当时之前全部匹配记录"，UI 有明确提示文案。
   - 影响：**无法清理"全部至当前时刻"**，实际运维能力回退。
   - **修复指引**：把下限从 1 改回 0（`Math.max(0, ...)`），并补回 0 值语义的提示文案；注意第 51 行确认文案 `${olderThanDays} 天前` 在 0 时需特殊处理为"全部"。
2. **卡片/列表视图切换被删**：老版有 `recordView: 'cards' | 'table'` 切换（LayoutGrid/List 按钮）+ 完整卡片视图（usage-record-card，含账号/会话块、六指标网格、错误块）。ui 只留表格视图。
   - **修复指引**：如需回补，重建卡片视图组件并加视图切换；优先级低于 #1。
3. **清理预估"预计批次数"丢失**：老版 preview 区展示 `预计 N 批`（`ceil(matchedRows / batchSize)`）。ui `previewResult` 类型只有 matchedRows/cutoffAt/oldest/newestCreatedAt，未计算也未展示 estimatedBatches。
   - **修复指引**：在 preview 结果展示处按 `Math.ceil(matchedRows / batchSize)` 补回。

**弱缺失（取决于基线口径）**：
- 清理任务状态块缺 `模式(mode)` 显示（其余 jobId/processedRows/... 均覆盖）。
- "模型计价同步卡 + 同步价格按钮"**仅中间版 `admin-ui` 有**，daisy 基线与 ui 均无。以 daisy 为基准不算回退；若要功能并集则需补。

**有意变更（不要改）**：实时 TPM 卡并入 RPM 卡（TPM 降为 desc）+ 新增错误率/Token 卡、独立计费弹窗 UsageBillingModal 并入统一 UsageDetailModal（"总输入"tile + 盈亏 Badge 改为"净盈亏"字段）、筛选区改可折叠 + 文本防抖 + 激活计数 Badge、费用列 `!pricingAvailable` 时显示 `—`、清理须先预览才能执行（`!previewResult` 时禁用）、默认保留天数 30（老版 7）。**详情弹窗为老版超集**：补全了 publicError、cache5m/1h、responseLatency、上报·可计费、净盈亏。

---

## 8. 配置域（runtime + security）— ⚠️ 字段全，保存期校验缺 2 处

**ui 文件**：`features/runtime/runtime-page.tsx`、`features/runtime/runtime-sections.tsx`、`features/security/security-page.tsx`、`lib/runtime-config-defaults.ts`
**基线**：`admin-ui-daisy/src/components/ConfigPanel.tsx`（老版单页 11 个 tab：access/limits/cooldown/scheduler/warmup/payload/payloadHistory/payloadFallback/cachePolicy/compat/stats，全部映射到 ui 的 runtime + security 两页）

**字段层面无整块缺失**：所有运行配置字段、reportedUsage/cachePolicy/payloadShaping/promptCacheCreationControl/modelMapping 全部子字段都已落地。部分还是**超集**（payloadHistory 比老版多 4 字段：historicalToolResultHeadLines/TailLines/toolDescriptionMaxChars/toolSchemaAnnotationMaxChars；负载均衡模式选择器为新增）。

**问题集中在保存逻辑（2 项 P0，影响数据正确性）**：

### 8.1 缺 2 条冷却交叉校验

- **现状**：`runtime-page.tsx` 的 `save()` 只有 3 条校验：触顶扣减下限≤上限、处理阈值（payloadGuardMaxBytes）为 0 或 ≥65536、安全余量（阈值-安全余量≥65536）。
- **老版**：`ConfigPanel.tsx` 的 `save()`（约 1505-1606）额外有 2 条：
  1. `credentialTransientCooldownSecs > credentialMaxCooldownSecs` → 拦截（临时冷却不能大于最大冷却）。
  2. 各错误类型基础冷却（rateLimit/server/network/stream/protocol/auth）任一 `> credentialMaxCooldownSecs` → 拦截。
- **影响**：用户把某项错误冷却填得比最大冷却还大时，老版报错拦截，**ui 静默放行**，非法配置会写入后端。
- **修复指引**：在 `runtime-page.tsx` 的 `save()` 提交前补回这 2 条校验，用 `toast.error(...)` + `return` 拦截（与现有 3 条同样式）。

### 8.2 normalizeConfig 漏调用 normalizePayloadShaping

- **现状**：`runtime-page.tsx` 的 `normalizeConfig`（约 140-194）调用了 normalizeReportedUsage / normalizePromptCacheCreationControl / normalizeCachePolicy / normalizeDefinedCacheRoutes，但**没有调用 `normalizePayloadShaping`**。payloadShaping 直接透传 draft 原始值（加载时仅 `{ ...defaultPayloadShaping(), ...config.data.payloadShaping }` 做了 spread，保存路径未归一化）。
- **关键事实**：`normalizePayloadShaping` 函数**已在 `lib/runtime-config-defaults.ts:382` 导出，但全项目无人 import**。它的作用是对 payloadShaping 的 9 个 char/byte 字段做 `toWhole` 取整/钳制。
- **影响**：payloadShaping 的 9 个数值字段不再 floor/clamp，可能写入非整数/越界值。
- **修复指引**：在 `runtime-page.tsx` import 该函数，并在 `normalizeConfig` 返回对象里补 `payloadShaping: normalizePayloadShaping(draft.payloadShaping)`。

### 8.3 一项校验触发条件偏宽（行为变更，非缺失）

- payloadGuard 的两条数值校验**已覆盖**，但老版前缀是 `next.payloadGuardEnabled && ...`（仅启用时校验），ui 只判 `payloadGuardMaxBytes > 0`。结果：即便关闭大小保护，只要填了 maxBytes，ui 仍可能报错拦截。建议按 `payloadGuardEnabled` gate（优先级低，属体验问题非正确性问题）。

**有意变更/澄清（不要改）**：
- reportedUsage 各字段的 `reportedUsageModeDescription` 详细说明文案、大量 NumField/TogField 的 `desc` 在 ui 被精简 —— 四种模式功能都在（FIELD_MODE_LABELS：原始返回/保留口径/采样封顶/采样目标），只是解释性文案省略。
- 模型映射「填充默认规则」语义：老版整体替换 `rules`；ui 改为去重追加合并。两者都启用映射+autoGenerate，结果集不同（设计选择，非 bug）。
- modelMapping 编辑：ui 用 JSON textarea + 填充默认（与老版一致；中间版曾用结构化表格，ui 回归 JSON）。
- 新增 Key 一次性明文：老版自动复制剪贴板+设为可见；ui 改为顶部 `Callout` 明文卡片（带复制/关闭，提示"关闭后无法再次查看"），能力已覆盖且更显式。后台登录 Key 修改加了高危二次确认（增强）。

---

## 9. 账号域 — ❌ 1 bug + 4 项退化（最严重的域）

**ui 文件**：`features/credentials/credentials-page.tsx`、`credential-card.tsx`、`credential-dialogs.tsx`、`credential-inputs.tsx`、`credential-utils.ts`
**基线**：`admin-ui-daisy/src/components/CredentialsPanel.tsx` + `CredentialDialogs.tsx`（单条添加输入项以 daisy 的 CredentialDialogs 和中间版 `admin-ui/src/components/add-credential-dialog.tsx` 为参照）

### 9.1 【P0 确定 BUG】单卡「查询」额度按钮是空函数

- **现状**：`credentials-page.tsx:804-806` 写死 `onQueryBalance={() => {}}`、`loadingBalance={false}`。点击卡片「查询」无任何反应。
- **老版**：daisy 用 `queryCredentialBalance` → `getCredentialInfo` 回填 balanceMap；中间版用 `handleQueryCredentialBalance`。
- **缓解事实（避免误判严重度）**：ui 整页已通过 `useCredentialAccountInfo(currentIds)` **自动拉取**额度并合并进卡片，所以额度数值本身**仍可见**。这是"死按钮交互失灵"，不是"额度功能完全丢失"。
- **修复指引**：要么接上真正的单条查询（调用对应 hook 触发该卡 refetch + loading 态），要么如果自动拉取已完全覆盖需求，则**移除这个无效按钮**避免误导。二选一，需确认产品意图。

### 9.2 【P1】单条「添加账号」弹窗丢失 4 组输入

`credential-dialogs.tsx` 的 AddCredentialModal **提交体里带了这些字段，但渲染区没有对应输入框**，导致值恒为空/默认：

| 字段 | 提交体 | 渲染输入框 | 老版位置 |
|---|---|---|---|
| `rpm`（账号 RPM 覆盖） | ❌ 连 state 键都没有 | ❌ 无 | admin-ui add-credential-dialog 602-611 |
| `machineId` | ✅ 提交 `form.machineId.trim()`（约 377） | ❌ 渲染区只到"端点"为止 | daisy 547-548 / admin-ui 621-628 |
| `proxyUrl/proxyUsername/proxyPassword`（直连代理） | ✅ 提交（约 379-381） | ❌ 只有"代理资源"下拉（约 437），无直连代理输入 | daisy 564 / admin-ui 677 |
| `region`（Region 兼容字段） | ✅ 提交（约 366） | ❌ 只有 Auth Region/API Region（约 435-436），无兼容字段输入 | daisy 34 |

- **缓解事实**：这些能力在「批量导入默认参数面板」和「批量修改」里**仍然存在**，只是"快速单条添加"这条路径退化了。
- **修复指引**：在 AddCredentialModal 渲染区补回 RPM 输入（含 state 键）、Machine ID 输入、直连代理 URL/用户名/密码输入、Region 兼容字段输入。参照 daisy `CredentialDialogs.tsx` 与中间版 `add-credential-dialog.tsx`。补 RPM 时注意 form state 要先加 `rpm` 键。

### 9.3 有意变更（不要改）

- 单卡额度/积分从"按需点击查询"改为"整页自动拉取"（`useCredentialAccountInfo`），配套去掉工具栏"查询信息（当前页）"按钮，替换为"更新积分"。
- 单卡低频/危险操作（开关预热、清理并发、删除）从平铺按钮收进"更多…" DropdownMenu（`credential-card.tsx` 约 425-469）。能力不变，入口变了。
- 积分统计从独立面板改为顶部 StatGrid 里一张可点 StatCard（→明细弹窗，含"最近查询"时间、`updateAllCreditInfo`）。
- 卡片"计价请求"位置从"额度与费用"段合并到"调度/运行态"段（呈现一次，数据等价）。

### 9.4 已覆盖（确认齐全，避免重复造）

列表工具栏（搜索、排序维度比老版还多 scheduler_score/id、13 项状态筛选、认证/订阅/代理筛选、清除筛选、展开收起全部、分页 PAGE_SIZE 15）、负载均衡 4 模式切换、批量操作（全选/验活带进度弹窗+后台运行/批量改/重置优先级/清并发/清 RPM/刷新 Token/查积分/恢复异常/删已禁用/清全部已禁用）、积分统计（更新/汇总/最近查询/明细弹窗）、卡片展开全部字段（并发含 ProgressRing、RPM、错误率、延迟 EWMA、调度评分、三窗口调度、调度压力、Lease、预热、观察期、计价覆盖、per-model cooldowns chips、额度积分、估算成本、Region、代理、最近错误）、单卡操作（禁用/优先级/并发/RPM/Region/代理弹窗/测试/刷新/恢复/清并发/预热/删除）、四类弹窗（添加 social/idc/external_idp/api_key + 文件填充、批量编辑、批量导入含默认参数+去重+验活模式+回滚、KAM 导入、导出 JSON/备份/JSONL、测试）。

---

## 10. 附录：有意重构清单（明确「不要当成缺失去改」）

下列差异是 `ui` 重构时的**有意设计决策**，能力等价或更优，**不要回退**：

1. **用量页**：卡片/列表视图只留表格（§7#2 列为缺失但优先级最低，回补与否取决于产品）、计费弹窗并入统一详情弹窗、实时 TPM 卡并入 RPM 卡、筛选区改折叠+防抖、清理须先预览。
2. **总览页**：耗时卡主值改 P95、趋势图改 recharts、状态分布+用量来源合并 Tab、维度排行截断前 10+占比列、新增账号池面板/Sparkline。
3. **配置页**：说明文案精简、模型映射填充改去重追加、新增 Key 改 Callout 明文卡片、后台 Key 修改加二次确认。
4. **账号页**：额度改自动拉取、低频操作收进 DropdownMenu、积分统计改可点 StatCard。
5. **审计页**：详情加结果 Badge、整行可点击、pricing 分类文案改"价格同步"。
6. **代理页**：新增卡片展开式账号绑定。
7. **外部池页**：新增 preservePath 开关、概览改 StatCard+ProgressRing。
8. **校验页**：系统复查改"范围按钮组+执行按钮"、统计卡补升级/无变化。
9. **模型页**：表结构重组、新增模型名模糊匹配、盈亏分析迁往「成本」页。

### 不构成 ui 缺失的项（三套都无 / 仅中间版有）

- 三套都无：列表级导出、代理列表搜索、审计时间范围过滤/导出/自动刷新。
- 仅中间版 `admin-ui` 有、daisy 基线无：用量页"模型计价同步卡+同步价格按钮"。是否补取决于是否做功能并集。

---

## 11. 修复批次建议

| 批次 | 内容 | 风险 | 验证 |
|---|---|---|---|
| 批次 1（正确性，先做） | §9.1 死按钮、§8.1 冷却校验、§8.2 normalizePayloadShaping | 低，改动小 | `pnpm check` + 手动触发保存校验 |
| 批次 2（功能回退） | §7.1 清理 0 值、§9.2 单条添加 4 组输入 | 中，涉及表单 | `pnpm check` + 手动走添加/清理流程 |
| 批次 3（展示回补） | §5 总览 3 项、§6 审计 2 项+bug、§4.1 预设、§4.2 告警 | 低 | `pnpm check` + 目视 |
| 待确认（需后端语义） | §3 代理 2 处（测试传参、删除文案） | — | 确认后端后再定 |

每批次完成后用 `/Users/yuanfeijie/.volta/bin/volta run --node 22.23.0 pnpm check` 验证类型，全部完成后 `pnpm build` 验证构建。修改时只改本文档指向的 feature 文件，不动 `@/api`、`@/hooks`、`@/types`、`@/lib`（§8.2 的 import 除外）。
