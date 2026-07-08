# 00 · 背景与数据契约家底盘点

> 实施前必读。本册回答两个问题：**当前 UI 的定位**、**后端到底能提供哪些数据、当前 ui 用了多少**。判断"哪些能力现在就能做"的依据全在这里。

---

## 1. UI 定位

仓库只维护 `ui/` 这一套管理后台前端，后端 API 集中在 `src/admin/`：

| 目录 | 代号 | 技术栈 | 定位 |
|---|---|---|---|
| `ui/` | 管理后台 | React 18 + shadcn/Radix + Tailwind v4 + react-router v6 + recharts | 当前维护版，四域 IA |

dashboard 相关代码集中在 `ui/src/features/overview/overview-page.tsx`。

---

## 2. 数据契约家底盘点（核心）

逐字段核查后端 `src/admin/` 路由与 `ui/src/types/api.ts` 的结论：**当前总览页只消费了可用数据的一部分。**

### 2.1 端点与使用情况总表

| 数据源 | 端点 | 关键字段 | 当前 ui 使用情况 |
|---|---|---|---|
| 用量仪表盘 | `/usage-dashboard` | `windows` / `series` / `top` | ✅ 已用，但 series 的 `totalEstimatedCostUsd`、`billableInputTokens`、`successRequests` 未充分利用 |
| 用量汇总 | `/usage-summary` | `realtime`（rpm/tpm）、`topConversations`、`topCredentials` | ⚠️ 部分（realtime 在用量页用；`topConversations` 总览未用） |
| 账号实时态 | `/credentials/summary` | `queuedRequests`、`maxQueuedRequests`、`runtimeFresh` | ⚠️ 部分（queued 用了；`maxQueuedRequests`、`runtimeFresh` 没用） |
| 外部池状态 | `/external-pools/status` | `cooldownRemainingSecs`、`dispatchable`、`skippedReason`、`inFlight` | ❌ 总览完全没用 |
| **存储写入器健康** | **`/usage-writer-stats`** | 队列容量/余量、内存记录数、`dropped_persist_records` | ❌ **前端连 api 封装都没有，完全闲置** |

### 2.2 最大发现：`/usage-writer-stats` 完全闲置

后端 `src/admin/router.rs` 注册了 `/usage-writer-stats`（handler `get_usage_writer_stats`），返回 `UsageRecorderStats`：

```rust
pub struct UsageRecorderStats {
    pub in_memory_limit: usize,        // 内存记录上限
    pub in_memory_records: usize,      // 当前内存记录数
    pub postgres_enabled: bool,        // 是否启用 Postgres 持久化
    pub writer_queue_enabled: bool,    // 写入队列是否启用
    pub writer_queue_capacity: usize,  // 写入队列容量
    pub writer_queue_available: usize, // 写入队列剩余余量
    pub dropped_persist_records: u64,  // 因队列溢出被丢弃的记录数
}
```

`ui/src` 全局搜索 `writer` / `writerStats` 无任何结果——新版前端没有这个端点的 api 封装、hook、type。

**为什么重要**：`dropped_persist_records > 0` 表示**用量记录正在丢失**（写入队列溢出），是数据完整性的硬告警。当前没有任何页面会暴露它，运维无从知晓数据是否可信。这是零后端改动、价值最高的增量。

### 2.3 已就绪但 dashboard 未用的数据

- **外部池实时态**（`ExternalPoolStatus`）：`inFlight`、`cooldownRemainingSecs`、`cooldownReason`、`dispatchable`、`skippedReason`。总览只显示了本地账号池，没有外部池实时状态。
- **会话热点**（`UsageSummary.topConversations`，类型 `UsageAggregate[]`）：含 requests、cacheRead/Creation tokens、estimatedCostUsd。后端已返回，前端没展示。
- **趋势序列的成本/可计费 Token**（`UsageSeriesPoint.totalEstimatedCostUsd`、`billableInputTokens`、`successRequests`）：趋势图当前只画了 requests/errors，成本维度没用上。
- **队列水位**（`CredentialSummaryResponse.maxQueuedRequests`）：有 `queuedRequests` 没有上限对比，无法体现"队列快满了"。
- **数据新鲜度**（`CredentialSummaryResponse.runtimeFresh`、`UsageDashboardResponse.generatedAt`）：可用于标注实时面板的数据时效。

---

## 3. 关键数据结构速查（实现时对照字段名）

```ts
// /usage-dashboard 的窗口汇总（features 已在用，列出未用字段）
interface UsageDashboardSummary {
  totalRequests; successRequests; errorRequests; errorRate
  streamRequests; nonStreamRequests; highCacheRequests
  totalInputTokens; billableInputTokens; totalOutputTokens
  totalCacheReadInputTokens; totalCacheCreationInputTokens; cacheReadRatio
  totalEstimatedCostUsd; pricedRequests; unpricedRequests
  averageDurationMs; p95DurationMs
  stickyBoundRequests; fallbackFromStickyRequests
  simulatedRequests; upstreamMetadataRequests
  externalPoolBilling?; externalPoolBillingByPool?   // 4 层成本：raw/shaped/uplifted/profit
  statusBreakdown; usageSourceBreakdown
}

// 趋势单点：注意 cost 字段已存在，趋势图却没画
interface UsageSeriesPoint {
  key; label; from; to
  requests; successRequests; errorRequests
  totalInputTokens; billableInputTokens; totalOutputTokens
  totalEstimatedCostUsd   // ← 未被趋势图使用
}

// 实时吞吐（用量页在用，总览可复用）
interface UsageRealtimeStats {
  windowSeconds; requests; rpm
  inputTpm; outputTpm; totalTpm; billableTpm
}

// 账号池实时态
interface CredentialSummaryResponse {
  total; available; disabled; currentId
  globalInFlightRequests; queuedRequests
  globalMaxConcurrentRequests; maxQueuedRequests   // ← maxQueued 未用
  updatedAt; runtimeFresh                          // ← runtimeFresh 未用
}

// 外部池实时态（总览未用）
interface ExternalPoolStatus {
  pool; inFlight; cooldownRemainingSecs
  cooldownReason?; dispatchable; skippedReason?
}
```

---

## 4. 需要新增的封装（唯一允许动复用层的地方）

实现存储健康卡需要补一个端点封装。这是"补齐缺失封装"，不是改动现有逻辑：

1. `ui/src/api/usage.ts` 新增 `getUsageWriterStats(): Promise<UsageRecorderStats>`（GET `/usage-writer-stats`）。
2. `ui/src/types/api.ts` 新增 `UsageRecorderStats` type（字段同上 §2.2，注意 TS 用 camelCase，需确认后端序列化命名——核查后端是否 `#[serde(rename_all = "camelCase")]`）。
3. `ui/src/hooks/use-usage.ts` 新增 `useUsageWriterStats()`（带短轮询）。

> ⚠️ 实施第一步必须先确认 `/usage-writer-stats` 返回 JSON 的字段命名（camelCase 还是 snake_case），以正确定义 TS type。其余复用层文件不要改。
