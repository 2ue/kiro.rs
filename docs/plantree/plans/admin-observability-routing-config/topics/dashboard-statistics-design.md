# Dashboard 运营统计设计

状态：Planning  
范围：新版 React `overview` dashboard、usage 聚合接口、账号运行态与统计健康  
关联：[`overview-page.tsx`](../../../../../ui/src/features/overview/overview-page.tsx)、[`usage.rs`](../../../../../src/anthropic/usage.rs)、[`postgres.rs`](../../../../../src/storage/postgres.rs)

## 1. 目标与问题定义

当前 dashboard 既承担运营总览，也承担实时调度监控、费用对账、账号质量和错误排障。信息没有按统计范围分层，导致以下问题：

1. **账号数据被 Top 10 截断，Top 数量被误当成全量。**
   - 后端 `dashboard_top_aggregates` 和 `dashboard_top_aggregates_for_window` 都固定 `LIMIT 10`。
   - 前端 `DimensionRankPanel` 再次 `slice(0, 10)`。
   - `AccountQualityPanel` 先读取 `top.credentials`，再按这些 ID 请求账号累计摘要，因此账号质量表最多只能覆盖 Top 10 个账号。
   - 这会把“20 多个已配置账号”错误地展示成“只存在或只参与了 10 个账号”。

2. **同一指标在多个 tab 或多个卡片重复出现。**
   - `账号请求` 实际上等于 `成功请求 + 错误请求`，与当前窗口的 `总请求` 重复。
   - `未计价请求` 同时出现在费用和异常 tab。
   - `估算费用`、`原始计费`、`Kiro 积分` 同时出现在窗口摘要、费用 tab 和成本关系面板。
   - `Top 账号数` 展示的是返回的排行行数，不是窗口内实际活跃账号数。
   - `实时负载`、`当前窗口摘要`、`流量` tab 使用不同范围，但当前视觉层级没有始终强调这一点。

3. **核心运营指标缺失或不完整。**
   - 可以看到部分请求、Token、费用和错误，但不能同时回答“有多少账号、多少账号真正有流量、多少账号不可调度、哪个账号正在失败、哪些账号没有统计数据”。
   - 本地账号与外部池虽然在 usage 记录中可区分，但 dashboard 的账号质量视图没有统一的来源分组和覆盖信息。
   - 聚合或降级失败时，页面缺少明确的“统计覆盖范围”和“数据是否完整”信号，容易把部分结果误认为全量结果。

## 2. 设计原则

### 2.1 统计范围必须显式

每个指标都必须带有明确范围：

| 范围 | 含义 | 示例 |
| --- | --- | --- |
| `realtime` | 固定滚动窗口，默认最近 60 秒；不受顶部日期窗口切换影响 | 实时 RPM、实时错误 RPM、实时 TPM |
| `window` | 顶部选择的业务时间窗口 | 今天、昨天、最近 7 天、最近 30 天、本月 |
| `retained` | 当前保留数据内的累计值，不宣称永久历史 | 累计请求、累计费用 |
| `live` | 当前账号池和调度器状态 | 已启用、可调度、冷却、限流、在途、队列 |
| `freshness` | 结果生成时间、数据截止时间和是否部分结果 | 统计生成于、运行态更新时间、覆盖状态 |

页面上的指标标题或副标题必须能让操作员知道它属于哪个范围。不能把实时 RPM 和“今天请求量”放在同一张无范围说明的卡片里。

### 2.2 排行不是全量

排行接口可以继续提供 Top N，但必须：

- 返回 `total`、`returned`、`truncated`；
- 前端把它明确标为“排行”，不能用于全量账号数；
- 账号质量和账号覆盖必须使用独立的、可分页的全量接口；
- 任何“总账号数、窗口活跃账号数、健康账号数”都必须来自后端全量聚合或实时状态汇总，不能由当前数组长度推导。

### 2.3 零用量账号不能被静默过滤

账号运营页面需要同时展示：

- 已配置但本窗口没有请求的账号；
- 有请求且成功的账号；
- 有请求但错误率较高的账号；
- 当前禁用、冷却、限流或不可调度的账号。

用量为零的账号应由账号表通过 `LEFT JOIN` 或等价的零填充返回，而不是因为没有 usage rollup 行就消失。默认可以按“有请求优先”排序，但必须能切换到“全部账号”。

### 2.4 一项指标只保留一个权威展示位置

同一个数值可以在详情表中再次出现，但不应在多个 KPI 卡片中重复占位。建议约束：

- 运营 tab：实时负载和账号池；
- 流量 tab：请求、错误、Token、延迟、模型/入口分布和趋势；
- 费用 tab：估算、原始、Kiro、外部池成本与利润；
- 账号 tab：全量账号质量与 Top 排行；
- 异常 tab：错误率、错误分类、来源、受影响账号和恢复情况；
- 统计健康：只放 writer、缓存、丢弃和新鲜度，不重复业务费用指标。

## 3. 页面信息架构

### 3.1 页面头部

保留当前时间窗口切换和自动刷新，但增加两个只读状态：

- `数据新鲜度`：窗口聚合生成时间、实时统计更新时间、账号运行态更新时间；
- `覆盖范围`：已配置本地账号数、窗口内活跃本地账号数、当前表格筛选后的账号数。

当任一查询发生降级、超时或只返回部分数据时，头部显示可点击的警示状态，并说明“哪些数据不完整”，不能只显示空表。

### 3.2 运营总览（默认页）

默认只保留操作员最需要的四组指标：

1. **实时负载**：实时 RPM、错误 RPM、实时 TPM、实时请求成功率。
2. **账号池容量**：总账号、已启用、可调度、冷却/限流、禁用、在途/队列。
3. **窗口健康**：窗口请求、成功率、P95、窗口错误数。
4. **告警摘要**：高错误账号数、无可调度账号、统计丢弃、外部池异常。

费用、Token 细分、缓存、Sticky 等技术或财务指标不在默认页重复铺开，进入对应 tab。

### 3.3 流量 tab

- 最近 24 小时按小时：请求、成功、错误；
- 最近 7 天按天：请求、错误、估算费用；
- 模型分布：请求、Token、错误率、估算费用；
- 入口分布：请求、错误率、平均/P95 延迟；
- 可选 Top N 排行，默认 Top 10，提供 20/50/100/200 选项；
- 排行接口返回完整数量元数据，页面显示“Top 10 / 共 X 个维度”。

### 3.4 费用 tab

只展示当前窗口：

- 本地估算费用；
- 原始/实际费用；
- Kiro 积分；
- 计价覆盖率和未计价请求；
- 外部池原始成本、可计费成本、成本底价差额、利润；
- 本地账号与外部池的费用拆分。

`估算费用` 和 `原始计费` 的定义必须通过 tooltip 说明，不能用两个名称相近但口径不明的卡片重复表达。

### 3.5 账号质量 tab

核心组件改为**全量账号表**，旁边或上方放小型排行，不再由排行驱动账号表。

账号表默认页大小 50，提供 20/50/100/200；支持服务端分页、排序和筛选。页面必须显示：

`显示 1-50 / 共 24 个本地账号；窗口活跃 18；当前筛选 24`

建议筛选：

- 来源：本地账号 / 外部池；
- 运行态：可调度、禁用、冷却、限流、观察期、无请求；
- 订阅/企业个人标识；
- endpoint、provider、region；
- 账号 ID、邮箱或显示名；
- 只看有错误、只看未计价、只看高 RPM/并发。

### 3.6 异常诊断 tab

错误必须按以下维度分开：

- 状态：成功、客户端错误、上游错误、超时、流错误、客户端断开；
- 阶段：请求解析、模型解析、账号选择、Token 刷新、本地上游、外部池、下游发送；
- 来源：本地账号 / 外部池；
- 是否经过重试；
- 是否通过换号或切换来源恢复；
- 受影响账号数和受影响请求数。

错误排行仍可以只展示 Top N，但必须提供“共 X 类错误”和跳转到 usage 明细筛选的入口。

## 4. 指标定义与字段设计

### 4.1 运营核心指标

| 字段 | 范围 | 计算口径 | 展示 |
| --- | --- | --- | --- |
| `totalRequests` | `window` | 当前窗口所有 usage 记录数 | 主数值，带完整格式化 tooltip |
| `successRequests` | `window` | `status = success` | 次级值 |
| `errorRequests` | `window` | 非成功状态数 | 次级值，非零时警示色 |
| `successRate` | `window` | `successRequests / totalRequests` | 百分比进度条 |
| `errorRate` | `window` | `errorRequests / totalRequests` | 百分比；按 5%、20% 分级 |
| `p95DurationMs` | `window` | 有效耗时样本 P95 | 主延迟指标 |
| `averageDurationMs` | `window` | 有效耗时平均值 | P95 下方辅助值 |
| `realtimeRpm` | `realtime` | 最近 60 秒请求数折算为每分钟 | 实时负载卡 |
| `realtimeErrorRpm` | `realtime` | 最近 60 秒错误数折算为每分钟 | 实时负载卡 |
| `realtimeTpm` | `realtime` | 最近 60 秒 Token 折算为每分钟 | 实时负载卡 |
| `realtimeSuccessRate` | `realtime` | 最近 60 秒成功率 | 实时负载卡 |

### 4.2 账号池和调度指标

| 字段 | 范围 | 计算口径 | 展示 |
| --- | --- | --- | --- |
| `configuredLocalAccounts` | `live` | credentials 配置总数 | 账号池总数 |
| `enabledLocalAccounts` | `live` | 未禁用账号数 | 账号池 |
| `availableLocalAccounts` | `live` | 当前可调度账号数 | 绿色/警示 |
| `disabledLocalAccounts` | `live` | 当前禁用账号数 | 红色 |
| `coolingLocalAccounts` | `live` | cooldown > 0 的账号数 | 黄色 |
| `rateLimitedLocalAccounts` | `live` | rate limited 的账号数 | 黄色 |
| `windowActiveLocalAccounts` | `window` | 当前窗口请求数 > 0 的本地账号数 | 账号质量 KPI |
| `windowIdleLocalAccounts` | `window` | 已配置但窗口请求数 = 0 的本地账号数 | 账号质量 KPI |
| `inFlightRequests` | `live` | 当前在途请求数 | 并发占用 `used / limit` |
| `queuedRequests` | `live` | 当前排队请求数 | 仅非零时展示 |
| `highRiskLocalAccounts` | `window + live` | 有请求且错误率 >= 10%，或当前不可调度且有近期错误 | 风险数，不由 Top N 推导 |

### 4.3 用量与费用指标

| 字段 | 范围 | 计算口径 | 展示 |
| --- | --- | --- | --- |
| `totalInputTokens` | `window` | 输入 Token 总量 | 流量/费用详情 |
| `totalOutputTokens` | `window` | 输出 Token 总量 | 流量/费用详情 |
| `totalCacheReadInputTokens` | `window` | 缓存读取 Token | 缓存详情 |
| `totalCacheCreationInputTokens` | `window` | 缓存写入 Token | 缓存详情 |
| `cacheReadRatio` | `window` | 缓存读取量 / 可比较的缓存输入量 | 进度条 |
| `streamRatio` | `window` | 流式请求 / 总请求 | 流量详情 |
| `totalEstimatedCostUsd` | `window` | 当前价格表对最终 usage 的估算 | 费用 tab 权威卡 |
| `totalOriginalCostUsd` | `window` | 上游原始 usage 或原始成本口径 | 费用对账卡 |
| `totalKiroMeteringUsage` | `window` | Kiro metering usage 汇总 | 费用 tab 权威卡 |
| `pricedRequests` | `window` | 有可靠价格覆盖的请求数 | 计价覆盖 |
| `unpricedRequests` | `window` | 无价格覆盖的请求数 | 计价覆盖，非零警示 |
| `externalPoolRequests` | `window` | route 为 external pool 的请求数 | 外部池卡 |
| `externalPoolBillableCostUsd` | `window` | 外部池最终 billable 成本 | 外部池计费 |
| `externalPoolProfitUsd` | `window` | 对外计费与成本之间的利润 | 外部池计费 |

### 4.4 运行与统计健康指标

这些字段只放在“统计健康”或“诊断”区域：

- `usageWriterQueueUsed / usageWriterQueueCapacity`；
- `redisWriterQueueUsed / redisWriterQueueCapacity`；
- `inMemoryRecords / inMemoryLimit`；
- `droppedPersistRecords`、`droppedRedisRecords`；
- `postgresEnabled`、`redisEnabled`、`redisQueueEnabled`；
- `generatedAt`、`usageDataThrough`、`runtimeStateThrough`；
- `coverage.complete`、`coverage.reason`。

这些指标不能与业务请求量、费用、Token 再做一组重复 KPI。

## 5. 全量账号表字段

账号表返回一个账号一行，所有字段都允许为零或未知，但不能静默缺行。

### 5.1 身份字段

- `kind`: `local_credential` 或 `external_pool`；
- `id`、`label`、`email`；
- `provider`、`endpoint`、`region`；
- `subscriptionTitle`、`accountType`（个人/企业/未知）；
- `isEnabled`、`disabledReason`。

### 5.2 当前运行态字段

- `dispatchStatus`: 可调度、冷却、限流、观察期、禁用、异常；
- `inFlightRequests`、`maxConcurrentRequests`；
- `currentRpm`、`rpmLimit`，展示为 `当前/限制`，无限制使用 `∞`；
- `failureCount`、`refreshFailureCount`、`transientFailureStreak`；
- `lastErrorKind`、`lastErrorReason`、`lastErrorAt`；
- `lastUsedAt`、`runtimeUpdatedAt`。

### 5.3 当前窗口用量字段

- `windowRequests`；
- `windowSuccessRequests`、`windowErrorRequests`、`windowErrorRate`；
- `windowInputTokens`、`windowOutputTokens`；
- `windowEstimatedCostUsd`、`windowOriginalCostUsd`；
- `windowKiroMeteringUsage`；
- `windowPricedRequests`、`windowUnpricedRequests`；
- `windowLastRequestAt`。

### 5.4 展示规则

- 账号名列显示 `#ID + 邮箱/显示名`，长文本截断但 tooltip 保留完整值；
- 运行态使用统一 Badge，不在表格里重复输出同一状态的长文案；
- 错误率按 `0`、`>0`、`>=10%` 分级；
- 当前 RPM 使用 `current/limit`，并发使用 `inFlight/max`；
- 无限制统一显示 `∞`，未知显示 `-`，不能把未知当成零；
- 零请求账号显示在“全部账号”中，默认排序放在有请求账号之后；
- 默认排序建议：不可调度/高错误优先，其次错误率，再其次窗口请求数；
- 表格上方显示分页和覆盖统计，禁止只显示“Top 10”而没有总数；
- 点击账号行跳转到账号管理或带账号过滤的 usage 明细，不能触发 Token 刷新或余额查询。

## 6. 后端接口建议

### 6.1 新增全量账号聚合接口

建议新增：

`GET /api/admin/usage-dashboard/accounts`

参数：

- `timezone`
- `windowKey`
- `page`
- `pageSize`，默认 50，允许 20/50/100/200
- `sortBy`、`sortOrder`
- `kind`
- `status`
- `q`
- `subscription`
- `provider`
- `endpoint`

响应：

```json
{
  "generatedAt": "...",
  "timezone": "Asia/Shanghai",
  "windowKey": "today",
  "coverage": {
    "configuredLocalAccounts": 24,
    "enabledLocalAccounts": 22,
    "disabledLocalAccounts": 2,
    "windowActiveLocalAccounts": 18,
    "windowIdleLocalAccounts": 6,
    "total": 24,
    "filteredTotal": 24,
    "page": 1,
    "pageSize": 50,
    "totalPages": 1,
    "complete": true,
    "reason": null
  },
  "summary": {
    "windowRequests": 1234,
    "windowErrorRequests": 12,
    "windowEstimatedCostUsd": 1.23,
    "windowKiroMeteringUsage": 456.0
  },
  "items": []
}
```

实现要求：

1. 账号表必须以 credentials/external pool 为主表，对窗口 usage 做 `LEFT JOIN` 或等价零填充；
2. 不允许按账号逐个请求 usage，避免 N+1；
3. 运行态和窗口用量要在同一响应中带回，避免前端把两个接口结果拼成不完整列表；
4. 后端负责 `total`、`filteredTotal`、`totalPages` 和风险计数；
5. 如果只拿到降级数据，返回 `complete=false` 和原因；
6. 不自动触发账号余额查询、Token 刷新或上游探针。

当前实现已落地为：

- `GET /api/admin/usage-dashboard/accounts`，支持 `timezone`、`windowKey`、`page`、`pageSize`、`q`、`status`、`sortBy`、`sortOrder`；
- `pageSize` 服务端限制在 20-200，默认 50，避免单次响应无限膨胀；
- usage 查询通过 `usage_rollup_totals`、`usage_rollup_time_buckets` 和 partial-hour `usage_records` 边界聚合完成，并限制为当前本地账号 ID；
- 服务层把 rollup 与轻量运行态快照按账号 ID 合并，零用量账号补零，返回 `configuredLocalAccounts`、`windowActiveLocalAccounts`、`windowIdleLocalAccounts`、`complete`、`reason`；
- `ui` 和 `admin-ui` 的账号统计均改为该接口分页展示，不再由 Top 10 排行驱动，也不再对每个账号发起 usage N+1 请求；
- 查询复用独立 dashboard gate、只读事务和 15 秒账号统计超时，不读取余额、不刷新 Token、不调用调度器。

### 6.2 扩展排行接口

现有 `/usage-dashboard/top` 可以保持兼容，但增加：

- `limit` 参数；
- `total`；
- `returned`；
- `truncated`；
- `orderBy`，允许请求量、错误率、费用、Token。

排行数据只用于排行组件，不再驱动账号质量表。

### 6.3 错误聚合字段

错误聚合建议增加：

- `source`: local / external；
- `phase`；
- `statusCode`；
- `retryable`；
- `recovered`；
- `affectedCredentials`；
- `terminalRequests`。

这样 dashboard 可以区分“本地账号本身报错”和“切换到外部池后恢复”，避免把所有错误只按字符串堆在一起。

## 7. 与参考项目的借鉴和边界

### 7.1 `sub2api`

保留并吸收：

- 总请求、输入/输出/缓存 Token、实际/标准费用、平均响应时间等基础统计卡；
- 模型、分组、平台拆分；
- 用户/账号排行的可选 Top 20/50/100/200；
- 服务端筛选、排序和分页；
- 统计更新时间和 stale 标记。

不直接照搬其“用户/租户”概念。当前 kiro.rs 是单运营者模型，本项目里的本地账号和外部池是上游容量资源，不是产品用户。

### 7.2 `codex2api`

借鉴其“用量快照”和“探针状态”的分离思路：

- 账号用量快照属于账号健康/容量信息；
- 探针、Token 刷新、429、未授权不能混入普通 usage 费用；
- 探针失败应显示为账号健康事件，不应制造一条伪造的业务请求。

本项目 dashboard 不应因为打开页面就主动刷新账号 Token 或发起全量探针。

## 8. 实施顺序

### Phase 1：先修复完整性

- 新增全量账号聚合接口；
- 前端账号质量表改为服务端分页；
- 返回覆盖统计、零用量账号和 `complete` 标记；
- 删除 `AccountQualityPanel` 对 Top 10 的依赖；
- 删除旧版账号统计对 Top 10 的依赖；
- 完成 Rust `cargo check --all-targets`、`ui` 构建和 `admin-ui` 构建验证。

20+ 账号真实数据库回归仍需在隔离 PostgreSQL 数据集上执行；当前工作区没有可安全复用的生产数据连接。

### Phase 2：收敛重复指标

- 移除 `Top 账号数` 和重复的 `账号请求`；
- 将未计价、估算费用、原始计费、Kiro 积分各自固定到唯一权威区域；
- 统一实时、窗口、累计、运行态标签；
- 将技术性的 writer/Redis 指标收敛到统计健康。

### Phase 3：补齐运营诊断

- 增加本地/外部来源、错误阶段、重试恢复和受影响账号字段；
- 增加高风险账号、无可用容量、统计不完整告警；
- 增加按账号、来源和错误阶段跳转 usage 明细的快捷筛选。

### Phase 4：验证与发布

- 后端聚合单测：24 个账号、0 用量账号、窗口边界、分页排序；
- 前端组件测试：总数、页码、零用量、状态筛选、错误覆盖提示；
- 性能测试：确认不会产生账号级 N+1 查询；
- Redis/PgSQL 降级测试：降级时必须显示 `complete=false`；
- `pnpm --dir ui build`、Rust 相关测试和 dashboard 接口回归。

## 9. 验收标准

1. 生产环境有 20 个以上本地账号时，dashboard 能显示配置总数，并通过分页查看全部账号；
2. 账号质量表不再受 Top 10 限制，零请求账号不会丢失；
3. `Top 账号数` 不再作为全量账号数，窗口活跃账号数由后端准确返回；
4. 实时、当前窗口、保留期累计和 live 运行态均有清晰范围；
5. 费用、Token、未计价、Kiro 积分和统计健康没有重复 KPI；
6. 本地账号和外部池在统计和错误视图中可分别筛选、分别汇总；
7. 任一聚合降级、部分失败或数据过期都会显示覆盖状态和原因；
8. 打开 dashboard、切换 tab、分页或筛选不会触发 Token 刷新、余额查询或全量上游探针；
9. 所有排行组件都显示 `returned / total`，不能把返回行数解释为系统总量。

## 10. 待确认事项

- “累计”是否按当前 usage 保留期命名为“保留期累计”，避免清理历史后产生误解；
- 账号运行态的权威新鲜度来自 Redis、进程内存还是组合快照；
- 外部池是否与本地账号共用一张表，还是在 UI 上采用两张并列表；
- 是否需要在第一阶段加入 CSV 导出全部账号，还是先完成服务端分页和筛选。
