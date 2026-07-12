# 05 · 实施路线图（优先级、批次、后端依赖）

> Archive status: Historical unaccepted roadmap; no batch below is active work.
>
> Original path: `ui/docs/dashboard-enhancement/05-roadmap.md`. Current disposition and authority: [operator UI planning archive](../README.md).

---

## 批次划分原则

按"价值 / 成本比"和"是否依赖后端"排序。第一批全部是**零后端改动、独立不影响现有页面**的纯前端工作。

---

## 第一批 — 零后端改动，价值最高（建议先做）

| 模块 | 出处 | 为什么先做 | 主要工作量 |
|---|---|---|---|
| **存储健康卡** | [02](./02-overview-health.md) 模块 5 | 数据丢弃告警当前无任何页面暴露；独立卡片、不影响现有逻辑 | 新增 1 个 api 封装 + 1 个 hook + 1 个 type + 1 张卡 |
| **外部池实时态接入总览** | [02](./02-overview-health.md) 模块 4 | `/external-pools/status` 已有，总览却无外部池实时态 | 复用现有 hook + 1 个面板 |
| **趋势图补成本维度 + 双轴** | [03](./03-analytics.md) 模块 1 | 数据已在 series 里；顺带解决审计文档 2 个缺口 | 增强现有 `TrendSection` |
| **健康总评条 + 待办清单** | [02](./02-overview-health.md) 模块 1、2 | "一眼定生死"+ 可操作告警，运维核心诉求；纯前端聚合已有数据 | 2 个新组件 + 聚合逻辑 |

**推荐起步**：存储健康卡。理由——数据现成、价值高（数据完整性告警）、完全独立不碰现有页面、是验证"新增 api 封装"流程的最小切片。

### 存储健康卡的具体落地步骤（供起步参考）

1. `@/types/api`：新增 `UsageWriterStats` type（字段见 [00-background.md](./00-background.md) §2）。
2. `@/api/usage.ts`：新增 `getUsageWriterStats(): Promise<UsageWriterStats>`，GET `/usage-writer-stats`。
3. `@/hooks/use-usage.ts`：新增 `useUsageWriterStats(refetchInterval?)`，参考现有 `useUsageDashboard` 写法。
4. 总览页：新增"存储健康"`SectionCard`，含内存记录数/队列余量进度条、Postgres 状态 Badge、`dropped_persist_records > 0` 时 `Callout tone="error"`。
5. `pnpm check` + `pnpm build`（node 22）验证。

---

## 第二批 — 纯前端，中价值

| 模块 | 出处 |
|---|---|
| 成本分析模块（成本占比 + 外部池盈亏迁入 + 计价覆盖率趋势） | [03](./03-analytics.md) 模块 2 |
| 会话热点（`topConversations`） | [03](./03-analytics.md) 模块 4 |
| 维度下钻联动（排行 → 趋势/明细） | [03](./03-analytics.md) 模块 3 |
| 实时吞吐增强（队列水位 + RPM 迷你时序） | [02](./02-overview-health.md) 模块 3 |
| 错误下钻增强 | [03](./03-analytics.md) 模块 5 |

第二批可考虑配合 [01-architecture.md](./01-architecture.md) 的"总览/分析双层拆分"一起做——把历史类模块从总览迁到新分析页。

---

## 第三批 — 横切打磨

见 [04-cross-cutting.md](./04-cross-cutting.md)：数据新鲜度指示、阈值配置（localStorage 版）、刷新分层、跳转联动 URL 同步。

---

## 需要后端配合的项（列出供决策，不要擅自实现）

| 诉求 | 为什么需要后端 | 建议 |
|---|---|---|
| 健康阈值服务端持久化 | 跨设备/重启保留配置 | 可选；先用 localStorage 顶着（[04](./04-cross-cutting.md) §2） |
| 告警历史 / 环比对比（如同比上周） | 当前 `windows` 是独立快照，不含跨窗口环比数据 | 若要趋势对比需后端提供环比聚合 |
| 健康事件时间线（异常发生/恢复的历史记录） | 后端当前只暴露实时快照，无健康事件流 | 较大改动，按需评估 |

---

## 验收口径

每批完成后：
1. `pnpm check`（node 22）类型通过。
2. `pnpm build`（node 22）构建通过。
3. 新增模块在无数据/加载/错误三态下不崩（复用 `EmptyState`/`LoadingState`/`ErrorState`）。
4. 不破坏现有总览页其余模块。
5. 设计系统合规（语义色 token、`SectionCard`/`StatCard`/charts 原语）。
