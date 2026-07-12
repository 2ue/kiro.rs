# 03 · 分析（历史层）模块明细

> Archive status: Historical feature proposal; unfinished items require R8 entry-audit triage.
>
> Original path: `ui/docs/dashboard-enhancement/03-analytics.md`. Current disposition and authority: [operator UI planning archive](../README.md).

> 定位：**排查问题、成本复盘、容量规划时的下钻分析**。可滚动、信息密集，长轮询或手动刷新。
>
> 数据来源端点详见 [00-background.md](./00-background.md) 的家底盘点表。

---

## 模块 1 — 多指标趋势（增强现有 TrendSection）

**现状**：总览趋势图只画了 requests/errors 两条 series。

**可用但未用的字段**（`UsageSeriesPoint` 已含）：`totalEstimatedCostUsd`、`billableInputTokens`、`successRequests`。

**增强**：
- 指标切换：请求量 / 错误率 / **成本** / Token。
- 双轴叠加：请求量（柱）+ 成本（线），直接看"请求涨了成本是否同步涨"。
- 复用 `@/components/charts` 的 `TrendAreaChart` / `TrendBarChart`。

**与已删除历史审计 companion 的交集**（见 [archive provenance](../README.md#deleted-companions)）：本模块曾计划一并解决两个缺口——
1. 趋势单点费用 hover 丢失（tooltip 带出 `cost`，`seriesPointToChartRow` 已含 `cost` 字段，只是没在图表用上）。
2. 趋势窗口费用汇总丢失（在图表标题/副标题对数组 reduce 出 `总请求 · 总费用`）。

---

## 模块 2 — 成本分析（新增，运维核心诉求）

**目的**：统一回答"钱花在哪、外部池划不划算"。当前成本数据散在各处，无统一视图。

**展示**：
- 成本按维度占比：模型 / 账号 / 入口（数据：`top.models/credentials/endpoints` 的 `totalEstimatedCostUsd`）。
- **外部池盈亏分析**：从总览**迁入**这里，并做成趋势。数据：`externalPoolBillingByPool` 的四层成本链路（`rawCostUsd` 原始 / `shapedCostUsd` 展示 / `upliftedCostUsd` 补偿后 / `profitUsd` 盈亏）。
- 计价覆盖率：`pricedRequests / totalRequests` 趋势。覆盖率低 = 成本估算不准，是预警信号。

**注意**：外部池盈亏从总览迁来后，总览不再保留该面板（避免两处重复，见 [01-architecture.md](./01-architecture.md)）。

---

## 模块 3 — 维度下钻（增强现有维度排行）

**现状**：维度排行是静态 Top 10 表（模型/账号/入口/错误）。

**增强**：点击某行（某模型/账号）→ 联动筛选上方趋势图 + 提供"查看明细"跳用量页（带该维度筛选）。把"看排行"升级成"从排行进入排查"。

**数据**：现有 `top` 聚合 + 跳转用量页时通过 query 参数带筛选。

---

## 模块 4 — 会话热点（数据闲置，新增）

**目的**：发现"某个会话异常烧 token / 异常高频"。

**数据**：`/usage-summary` 的 `topConversations`（`UsageAggregate[]`，含 requests / cacheReadInputTokens / cacheCreationInputTokens / estimatedCostUsd）——**后端已返回，前端从未展示**。

**展示**：Top 会话表，按请求数/成本排序，点击跳用量页（按 conversationId 筛选，用量页已支持该筛选项）。

---

## 模块 5 — 错误下钻（增强现有异常摘要）

**现状**：异常摘要只列 top errors 静态清单。

**增强**：
- 错误按类型分布（饼/条）。
- 错误率时序（来自 series 的 `errorRequests / requests`）。
- 一键跳用量页错误筛选。

**数据**：`top.errors` + series。无需后端改动。
