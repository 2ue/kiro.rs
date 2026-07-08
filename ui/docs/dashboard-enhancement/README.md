# Dashboard 与运维分析能力增强方案

> 目标:基于 Kiro 控制台**现有后端能力**，增强 `ui/` 的 dashboard 与运维分析页，让运维"一眼看清健康、快速下钻排查、清晰复盘成本"。
>
> **本方案力求自包含**：即使不读源码、不看对话记录，也能据此准确理解每个模块的数据来源、实现边界与实施顺序，不产生误解。

方案时间：2026-06-30。基于对后端 `src/admin/` 路由与 `ui/src/types/api.ts` 数据契约的逐字段核查。

---

## 这份方案解决什么问题

当前 `ui/` 的总览页（`features/overview/overview-page.tsx`，1000+ 行）把"实时健康监控"和"历史趋势分析"混在一页，且**只消费了后端可用数据的一部分**。核查发现：

- 有一个端点 `/usage-writer-stats`（存储写入器健康，含**数据丢弃告警信号**）**前端完全没接**，连 api 封装都没有。
- 外部池实时状态 `/external-pools/status`、会话热点 `topConversations`、趋势的成本维度 `totalEstimatedCostUsd` 等**已就绪的数据没被 dashboard 用上**。

本方案不要求改后端业务逻辑：约 90% 的增强靠现有端点即可实现。

---

## 文档结构（分册阅读）

| 分册 | 内容 | 给谁看 |
|---|---|---|
| [00-background.md](./00-background.md) | 背景、当前 UI 定位、**数据契约家底盘点**（每个端点有什么、用了多少） | 实施前必读，判断"能做什么" |
| [01-architecture.md](./01-architecture.md) | 页面架构建议：总览页拆成"健康层 + 分析层"双层 | 定方向 |
| [02-overview-health.md](./02-overview-health.md) | 总览（健康层）5 个模块明细 | 实现健康层 |
| [03-analytics.md](./03-analytics.md) | 分析（历史层）5 个模块明细 | 实现分析层 |
| [04-cross-cutting.md](./04-cross-cutting.md) | 横切能力（数据新鲜度、阈值配置、刷新分层等） | 两页共用 |
| [05-roadmap.md](./05-roadmap.md) | 实施优先级、批次划分、后端依赖清单 | 排期 |

建议阅读顺序：00 →（01 定方向）→ 05（看批次）→ 按批次回到 02/03/04 看模块细节。

---

## 关键约束（实施时必须遵守）

- **后端 API 与业务逻辑一律不动**。所有模块基于现有端点。需要后端配合的项已在 [05-roadmap.md](./05-roadmap.md) 单独列出，不要擅自实现。
- 前端构建/类型检查用 node 22：
  - 类型检查：`/Users/yuanfeijie/.volta/bin/volta run --node 22.23.0 pnpm check`
  - 构建：`/Users/yuanfeijie/.volta/bin/volta run --node 22.23.0 pnpm build`
  - 默认 shell 的 node 是 16，会失败。工作目录 `ui/`。
- 复用层不要改：`@/api/*`、`@/hooks/*`、`@/types/api`、`@/lib/*`（`@/` = `ui/src/`）。
  - **例外**：本方案需要**新增** `getUsageWriterStats` 的 api 封装与对应 hook、type（见 00 与 02），属"补齐缺失封装"，不是改动现有逻辑。
- 设计系统规则见 `ui/DESIGN_SYSTEM_BRIEF.md`：只用语义色 token、弹窗用 `ModalShell`、表格用 `<Table>`、危险操作用 `useConfirm()`、图表用 `@/components/charts`。
- 行号基于审计时点快照，以实际代码为准（用关键代码片段定位）。

---

## 与功能审计文档的关系

本方案是"做加法"（增强能力）；同目录之外的 `ui/FEATURE_PARITY_AUDIT.md` 是"补欠账"（回补相对老版的功能缺失）。两者独立，但有 2 处交集，已在分册中标注：

- 趋势图缺"单点费用 hover""窗口费用汇总"——审计文档记为缺失，本方案在 [03-analytics.md](./03-analytics.md) 的"多指标趋势"模块中一并解决。
- 外部池盈亏分析——已存在于总览，本方案建议迁入分析页的"成本分析"模块。
