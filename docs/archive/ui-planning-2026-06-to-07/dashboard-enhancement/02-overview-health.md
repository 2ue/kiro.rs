# 02 · 总览（健康层）模块明细

> Archive status: Historical feature proposal; unfinished items require R8 entry-audit triage.
>
> Original path: `ui/docs/dashboard-enhancement/02-overview-health.md`. Current disposition and authority: [operator UI planning archive](../README.md).

> 定位：**5 秒内回答"系统现在健不健康、有没有要我立刻处理的事"**。首屏不滚动看全，短轮询（10-15s）。
>
> 数据来源端点详见 [00-background.md](./00-background.md) 的家底盘点表。

---

## 模块 1 — 系统健康总评条（新增，顶部）

**目的**：聚合多个子系统状态成单一红/黄/绿信号灯 + 异常计数，置于页面最顶部。

**聚合输入**（全部来自已有数据）：
| 维度 | 数据来源 | 越线条件（建议默认，可配置） |
|---|---|---|
| 账号池可用率 | `/credentials/summary` `available/total` | < 30% 黄、= 0 红 |
| 错误率 | `/usage-dashboard` 当前窗口 `errorRate` | > 5% 黄、> 20% 红 |
| 外部池可调度 | `/external-pools/status` `dispatchable` | 全部不可调度 红 |
| 存储丢记录 | `/usage-writer-stats` `dropped_persist_records` | > 0 红 |
| 队列水位 | `/credentials/summary` `queuedRequests/maxQueuedRequests` | > 80% 黄 |

**交互**：任一项越线 → 整条变色 + 展开列出具体越线项（带跳转到对应页）。全绿时显示"系统正常运行"。

**当前缺失**：这是"一眼定生死"的能力，现在完全没有。

---

## 模块 2 — 待办 / 告警清单（新增）

**目的**：把分散的异常聚合成**可操作清单**，每条带一键跳转到处理页。

**清单项来源**：
- "N 个账号已禁用 / 冷却" → 跳账号页（带 disabled/cooldown 筛选）。数据：`summary.disabled` + dashboard。
- "外部池 X 已自动禁用，原因：Y" → 跳外部池页。数据：`/external-pools/status` 的 `skippedReason` / `cooldownReason`。
- "错误率 X% 超阈值" → 跳用量页（错误筛选）。数据：dashboard `errorRate`。
- "写入队列余量不足 / 已丢弃 N 条记录" → 展开存储健康卡（模块 5）。数据：`/usage-writer-stats`。

**空态**：无待办时显示"当前无需处理的异常"。

---

## 模块 3 — 实时吞吐（增强现有）

**目的**：实时请求压力一目了然。

**展示**：
- RPM / TPM 实时值（数据：`/usage-summary` 的 `realtime`，含 `rpm`/`inputTpm`/`outputTpm`/`totalTpm`/`billableTpm`）。
- 并发占用：`globalInFlightRequests / globalMaxConcurrentRequests`（已有）。
- **队列水位条**（新增）：`queuedRequests / maxQueuedRequests` —— `maxQueuedRequests` 当前没用上。
- 迷你时序：最近 N 次轮询的 RPM 走势（前端自存轮询历史，体现"在涨还是在跌"）。

**复用**：`@/components/charts` 的 `Sparkline`、现有 `ProgressRing`。

---

## 模块 4 — 资源池实时态（增强现有账号池面板）

**目的**：账号池 + 外部池的实时可用状态并排。

**展示**：
- 账号池环形图（**保留现有** `CredentialPoolPanel`：可用/禁用/冷却/合计 + 并发占用）。
- **新增外部池实时态**：每个池的 `inFlight`、`cooldownRemainingSecs`、`dispatchable`、`skippedReason`（数据：`/external-pools/status`）。当前总览**完全没有**外部池实时态。

**注意**：外部池盈亏分析（成本向）不放这里，迁去分析页成本模块（见 [03-analytics.md](./03-analytics.md)）。这里只放"可用性"实时态。

---

## 模块 5 — 存储健康卡（全新，数据已就绪但前端未接）

**目的**：暴露用量数据的持久化健康，**这是当前最大的盲区**。

**实现前置（需新增封装，README 已说明属"补缺失封装"）**：
1. `ui/src/types/api.ts` 加 `UsageRecorderStats` 类型（字段见 00 册）。
2. `ui/src/api/usage.ts` 加 `getUsageWriterStats()`，GET `/usage-writer-stats`。
3. `ui/src/hooks/use-usage.ts` 加 `useUsageWriterStats()`（短轮询）。

**展示**：
- 内存记录数 / 上限：`in_memory_records / in_memory_limit`（进度条）。
- 写入队列余量 / 容量：`writer_queue_available / writer_queue_capacity`（进度条，余量低→黄）。
- Postgres 持久化：`postgres_enabled` 开关状态徽章。
- **丢弃记录数**：`dropped_persist_records` —— **非 0 时红色告警**，这是数据完整性受损的直接信号（用量记录因队列溢出被丢，统计与成本会偏低）。

**价值**：零业务逻辑改动，纯前端补封装 + 一张卡，但补上了"用量数据是否可信"这一核心运维问题。
