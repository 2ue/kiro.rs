# 04 · 横切能力（健康层与分析层共用）

> Archive status: Historical feature proposal; unfinished items require R8 entry-audit triage.
>
> Original path: `ui/docs/dashboard-enhancement/04-cross-cutting.md`. Current disposition and authority: [operator UI planning archive](../README.md).

---

## 1. 数据新鲜度指示

**为什么**：运维必须知道看到的是不是陈旧数据，尤其健康层。

**实现**：每个实时面板标注"X 秒前更新"。数据来源：
- 账号 summary：`CredentialSummaryResponse.runtimeFresh`（布尔，运行态是否新鲜）+ `updatedAt`。
- dashboard：`UsageDashboardResponse.generatedAt`。

`runtimeFresh === false` 时显式提示"运行态数据可能陈旧"。

---

## 2. 健康阈值配置

**为什么**：健康总评条（[02-overview-health.md](./02-overview-health.md) 模块 1）的红/黄线（错误率、可用率、队列水位）不应硬编码。

**实现**：
- 第一期：前端 localStorage 持久化（参考现有 `@/lib/storage` 模式）。
- 后续：若要服务端持久化，需后端配合，见 [05-roadmap.md](./05-roadmap.md)，**不要擅自实现后端**。

**默认阈值建议**（可调）：错误率 > 20% 红 / > 5% 黄；可用率 < 30% 黄 / = 0 红；队列水位 > 80% 黄。

---

## 3. 自动刷新分层

**为什么**：健康层和分析层的刷新诉求不同，统一高频轮询会浪费请求。

**实现**：
- 健康层：短间隔（建议 10–15s）。
- 分析层：长间隔或手动。
- 复用现有 `useAutoRefreshPreference`（`@/hooks/use-auto-refresh`），各页独立 localStorage key（参考总览页现有 `OVERVIEW_AUTO_REFRESH_KEY` 写法）。

---

## 4. 时间窗口统一

沿用现有 `data.windows` 切换机制（总览页已实现 windows 按钮组）。分析层各模块共享当前选中窗口，避免每个面板各自选时间。

---

## 5. 跳转联动约定

下钻跳转统一走 react-router，目标页通过 URL query 接收筛选条件：
- 跳账号页：带状态筛选（如 `?status=disabled`）。
- 跳用量页：带维度筛选（如 `?conversationId=xxx`、`?status=error`）——用量页筛选项已支持这些维度。
- 跳外部池：带 pool 定位。

实现前确认目标页是否已支持从 URL 读取筛选（当前多数页面筛选是组件内 state，可能需要补 URL 同步——属目标页的小改动，非后端改动）。
