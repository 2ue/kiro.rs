# 总览页重构方案：按 Tab 拆分整体、本地账号、外部池与排行

状态：Implemented And Locally Verified（2026-10-02，本地 demo 已启动并通过拆分接口、Tab 与排行验证）
日期：2026-10-02
基线提交：`c7f9edb`（main）
适用前端：`ui/`（新版 React 后台，入口 `/ui/overview`）。`admin-ui/` 和 `console/` 不在本次范围内。
取代关系：本文取代 [dashboard-statistics-design.md](dashboard-statistics-design.md) 里第 3.2–3.4 节（运营总览、流量、费用 tab）的信息架构。该文其余内容继续有效，包括账号全量分页、统计范围标注、排行不等于全量等原则。

## 1. 背景与目标

### 1.1 业务需求（来自运营方）

运营方要在一个页面里回答下面这些问题，而且每个问题都要能同时看到「整体」和「局部」：

1. 选一个时间范围或时间点，系统整体情况是什么：请求量、成功率、Token、费用。
2. 同一时间范围内，本地账号池消耗了多少：Kiro 积分、估算计费（美元）、原始计费（美元）、请求数、错误率。
3. 同一时间范围内，外部池消耗了多少：原始成本（美元）、可计费金额（美元）、利润（美元）、请求数、错误率，以及每个外部池各自的数字。
4. 后端数据仍然必须满足合计 = 本地 + 外部，保证统计事实一致；但页面不再展示三口径对账表，也不要求运营人员阅读一张对账表。
5. 能看到趋势，并点进某一个小时或某一天，查看那个时间点的拆分。
6. 排行需要按模型、账号、请求秘钥、请求路径、错误分别查看，并显示各项占请求或占错误的百分比。

### 1.2 现状问题（以 `c7f9edb` 为准）

页面文件是 `ui/src/features/overview/overview-page.tsx`（约 1370 行），分成「实时 / 流量 / 费用 / 账号质量 / 异常诊断」五个 tab，主要问题如下：

- **口径混在一起**：费用 tab 里的「窗口估算成本」是本地和外部的合计，页面上没有标注。
- **本地和外部只能靠前端相减**：工作区里有一处未提交的临时改动，用减法拆出本地值。它只能覆盖请求数和费用，Token、错误数、延迟都拆不了。
- **趋势不分来源**：`/usage-dashboard/series` 只返回 global 维度的 24 小时和 7 天序列，看不出本地和外部各自的变化。
- **时间范围固定**：只能选今天、最近24小时、昨天、最近7天、最近30天、本月，不能选任意时间段，也不能点进某个时间点。
- **内容冗余**：写入器健康、Sticky 回退、usage 来源分布、状态分布这类诊断数据和业务总览放在同一页；同一个费用指标在不同 tab 重复出现。
- **排行维度失真**：原排行只有模型和错误，无法回答哪个账号、秘钥或路径承担了流量，也没有百分比基数。
- **外部池没有形成整体视图**：逐池计费在费用 tab 的一个面板里，外部池的运行状态在另一个页面，二者没有并列展示。

### 1.3 非目标

- 不修改 usage 记录口径和任何费用计算逻辑（遵守「不改 usage 计算」约定）。本文只做读取和展示。
- 不修改既有 usage 字段和费用计算逻辑；为支持秘钥排行，新增 `request_api_key` rollup 维度，历史数据只从重新写入或后续产生的数据开始具备该维度，不做明细费用回填。
- 不重做账号管理、外部池管理、请求明细这几个页面。总览只负责链接过去。
- 不处理 `admin-ui/`。

## 2. 术语与指标口径

本节定义页面里出现的每个指标，后文直接使用这些名称。

### 2.1 三个统计口径

| 口径 | 含义 | 判定方式（usage 记录字段） |
| --- | --- | --- |
| **合计** | 时间范围内的全部 usage 记录 | 全部记录 |
| **外部池** | 由外部上游池处理的请求 | `routeKind = external_pool`，且 `externalPoolId` 不为空 |
| **本地账号池** | 合计中不属于外部池的部分 | 合计 − 外部池。其中包括 `routeKind = local_credential` 的记录，以及极少量 `routeKind` 为空的本地拒绝记录（例如入口校验失败） |

说明：

- 本地账号池用减法定义，原因有两个：一是保证「合计 = 本地 + 外部」恒等；二是现有 rollup 没有 `route_kind` 维度，减法是唯一不扫明细表的做法（见 3.2 节）。
- `routeKind = external_pool` 但 `externalPoolId` 为空的记录，会计入本地账号池。这类记录在入库路径里理论上不存在（外部池记录都会带 pool id）。实现时需要用测试锁定这一点（见 7.2 节）。
- 现网参考：最近 7 天 `routeKind` 为空的记录有 74 条，全部是 `error`，费用为 0，对费用指标没有影响。

### 2.2 指标定义

| 指标 | 合计 | 本地账号池 | 外部池 | 数据来源字段 |
| --- | --- | --- | --- | --- |
| 请求数 | 全部 | 合计 − 外部 | 外部池记录数 | rollup `requests` |
| 成功数 / 错误数 / 错误率 | 全部 | 合计 − 外部 | 外部池 | rollup `success_requests` / `error_requests` |
| 输入 Token / 输出 Token / 缓存读 / 缓存写 | 全部 | 合计 − 外部 | 外部池 | rollup `total_input_tokens` 等 |
| **估算计费（$）** | 全部 | 合计 − 外部 | 外部池可计费金额 | rollup `total_estimated_cost_usd`。外部池记录的 `estimatedCostUsd` 就是它的可计费金额（`src/external_pool.rs:9639`） |
| **原始计费（$）** | 全部 | 合计 − 外部 | 外部池原始成本 | rollup `total_original_cost_usd`。外部池记录的 `originalCostUsd` 就是 `rawCostUsd`（`src/external_pool.rs:9693`） |
| **Kiro 积分** | 等于本地 | 全部积分 | 恒为 0 | rollup `total_kiro_metering_usage`。外部池记录写入 0（`src/external_pool.rs` 记录构造处 `kiro_metering_usage: 0.0`） |
| 外部池可计费 / 原始成本 / 利润（$） | — | — | 仅外部池 | rollup `external_pool_billable_cost_usd`、`external_pool_raw_cost_usd`、`external_pool_profit_usd` |
| 已计价 / 未计价请求 | 全部 | 合计 − 外部 | 外部池 | rollup `priced_requests` / `unpriced_requests` |
| 平均耗时 | 全部 | 合计 − 外部，按加权计算 | 外部池 | rollup `duration_ms_sum` / `duration_ms_count` |

指标说明（页面 tooltip 使用同样的文字）：

- **估算计费**：最终返回给客户端的 usage 乘以价格表得到的金额，即「按对外口径算多少钱」。
- **原始计费**：上游原始 usage 乘以价格表。上游没有原始 usage 时，回退为估算计费，即「按真实消耗算多少钱」。
- **Kiro 积分**：Kiro 上游在每个请求里返回的 meteringEvent 累加值。它是请求级消耗，不是余额快照。
- **外部池利润**：可计费金额减原始成本。

### 2.3 不提供的拆分（明确告知）

- **P95 延迟**：分位数没法相减，所以只给合计的 P95。本地和外部只给平均耗时。
- **实时（最近 60 秒）RPM / TPM**：来自内存统计，只有合计。这部分放进「实时」小卡片，标注为合计口径。
- **按模型、按错误类型的本地 / 外部拆分**：rollup 里 model、error 维度和来源没有交叉。模型排行、错误排行只给合计，并在页面上标注；账号排行只覆盖本地账号维度，秘钥和路径排行为合计口径。

## 3. 数据基础与可行性

### 3.1 已有的数据（不需要改写入）

每条 usage 记录写入 PgSQL 时，会在同一个事务里更新小时级汇总表 `usage_rollup_time_buckets`。写入入口是 `PostgresUsageStore::record_batch`（`src/storage/postgres.rs:5726`）；维度定义是 `usage_rollup_dimensions`（`src/storage/postgres.rs:9182`）；指标定义是 `UsageRollupMetrics::from_record`（`src/storage/postgres.rs:8862`）。

- 桶粒度：按 UTC 整点切分（`usage_rollup_bucket_start`），主键是 `(bucket_start, dimension, dimension_key)`，并有索引 `(dimension, dimension_key, bucket_start)`。
- 本方案用到的维度，每小时都有一行：
  - `global / all`：全部记录。
  - `external_pool / <poolId>`：每个外部池一行，`dimension_label` 是池名称。
  - `credential / <credentialId>`：每个本地账号一行，`dimension_label` 是账号标识。
  - `model / <model>`、`error / <errorType>`：模型维度、错误维度，都是合计口径。
- 每行包含 2.2 节列出的全部指标列，以及 `external_pool_*` 系列列。外部池相关的列只会累加外部池记录。
- 软清理 usage 时（`src/storage/postgres.rs:5993` 附近）会同步扣减 rollup，所以 rollup 和明细表保持一致。

### 3.2 拆分方式

在任意小时桶 `h` 上：

```
合计(h)       = global(h)
外部池(h)     = SUM over pools p of external_pool(p, h)
本地账号池(h) = 合计(h) − 外部池(h)
单个外部池(h) = external_pool(p, h)
单个本地账号(h) = credential(c, h)
```

一次查询覆盖若干小时桶，然后按小时或按天聚合，就能得到「整体 + 局部」的趋势序列。整个过程不扫 `usage_records` 明细表。

### 3.3 时间精度约束

- 新接口只按**整点**取数，起止时间都对齐到小时（起点向下取整，终点向上取整），接口在响应里返回对齐后的实际范围。
- 现有窗口接口用「中间整小时走 rollup，两头不足一小时扫明细」的方式做到秒级精度（`push_dashboard_windows_cte`）。新接口默认不这样做（是否接受见 9.2 节 Q1），原因有三个：
  1. 拆分逻辑只要写一遍；
  2. 趋势图本来就以小时为最小单位；
  3. 运营场景里，「今天」这类窗口的末尾最多差几十分钟数据，影响可以接受。
- 当前这个未满的小时桶也会计入。它只是还在增长，数据本身没有缺失。页面在时间范围旁边显示「数据截至 HH:mm」。

### 3.4 性能估算

- 单次查询读取的行数 ≈ 小时数 × (1 + 外部池数 + 活跃本地账号数 + 模型数 + 错误类型数)。
- 以 30 天、20 个外部池、200 个账号、50 个模型、30 种错误为例，最多约 720 × 300 ≈ 21.6 万行，并且走 `(dimension, dimension_key, bucket_start)` 索引。实现阶段要用 `EXPLAIN ANALYZE` 确认。
- 可选范围上限设为 **90 天**，超出时返回 400。
- 复用现有的 dashboard 查询限流（`dashboard_query` gate，`src/anthropic/usage.rs:2680`）、只读事务（`configure_usage_dashboard_read_transaction`）和语句超时。

## 4. 后端设计

### 4.1 接口拆分

```
GET /api/admin/usage-dashboard/overview
GET /api/admin/usage-dashboard/overview/summary
GET /api/admin/usage-dashboard/overview/series
GET /api/admin/usage-dashboard/overview/local
GET /api/admin/usage-dashboard/overview/external
GET /api/admin/usage-dashboard/overview/rankings
```

鉴权方式和其他 `/api/admin/*` 接口一致，请求头带 `x-api-key` 或 `Authorization: Bearer`。

实现后的对外契约按页面职责拆分。`/overview` 返回完整结构，保留给兼容、调试和一次性读取；新版总览页默认消费下面五个轻量接口：

| 接口 | 用途 | 主要字段 |
| --- | --- | --- |
| `/overview/summary` | 汇总 Tab 的整体事实负载、少量总体指标、外部财务摘要和本地账号数 | `range`、`totals`、`external.rawCostUsd/billableCostUsd/profitUsd`、`local.activeAccounts/accountsTotal` |
| `/overview/series` | 趋势图和点击下钻 | `range`、`series[]` |
| `/overview/local` | 本地账号池分区 | `local.activeAccounts`、`local.accountsTotal`、`local.accounts[]`（时间范围内有流量的本地账号用量；前端再合并账号清单与运行态，展示全部已配置账号） |
| `/overview/external` | 外部池分区 | `external.rawCostUsd`、`external.billableCostUsd`、`external.profitUsd`、`external.pools[]` |
| `/overview/rankings` | 排行 Tab 的模型、账号、秘钥、路径和错误排行，以及百分比基数 | `totalRequests`、`totalLocalRequests`、`totalErrors`、`topModels[]`、`topAccounts[]`、`topKeys[]`、`topPaths[]`、`topErrors[]` |

这五个接口在 admin service 层共享同一个 2 秒短缓存的完整 `UsageOverviewResponse`，缓存 key 使用规范化后的 `timezone`、对齐后的 `from/to`、`granularity`、`topN` 和自定义范围身份。也就是说，接口拆分只改变传输和前端职责，不改变统计口径：同一轮请求看到的是同一份 rollup 快照。

请求参数（query）：

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `timezone` | 否 | 默认 `Asia/Shanghai`，解析规则沿用 `usage_dashboard_timezone` |
| `range` | 否 | 预设范围：`today`、`yesterday`、`last24h`、`last7d`、`last30d`、`thisMonth`，默认 `today` |
| `from` / `to` | 否 | RFC 3339 时间，同时给出时覆盖 `range`。要求 `from < to`，跨度不超过 90 天 |
| `granularity` | 否 | `hour` 或 `day`。默认跨度 ≤ 48 小时用 `hour`，否则用 `day`。`day` 按 `timezone` 的本地零点切分 |
| `topN` | 否 | 排行条数，默认 10，最大 50 |

错误：参数非法时返回 400 和中文原因；PgSQL 不可用时返回 503。overview 系列接口不提供 Redis 回退，依赖 PgSQL rollup。

### 4.2 完整响应结构

字段名采用 camelCase。下面用 TypeScript 写，Rust 侧按同样的结构定义。

```ts
interface UsageOverviewResponse {
  generatedAt: string            // 服务端生成时间
  timezone: string
  range: {
    key: string | null           // 预设 key；自定义范围为 null
    requestedFrom: string        // 调用方请求的起点（未对齐）
    requestedTo: string
    from: string                 // 对齐到整点后实际使用的起点
    to: string                   // 对齐到整点后实际使用的终点（不晚于下一个整点）
    dataThrough: string          // 实际数据截至时间，取 min(to, now)
    granularity: 'hour' | 'day'
  }
  totals: {
    all: OverviewMetrics
    local: OverviewMetrics       // all − external
    external: OverviewMetrics
  }
  external: {                    // 只有外部池才有的计费字段，取自 external_pool_* 列
    rawCostUsd: number
    billableCostUsd: number
    profitUsd: number
    costFloorAppliedRequests: number
    pools: OverviewPoolRow[]     // 时间范围内有请求的全部外部池，按 billableCostUsd 降序，不截断
  }
  local: {
    activeAccounts: number       // 时间范围内有请求的本地账号数（credential 维度去重）
    accounts: OverviewAccountRow[]      // 时间范围内有流量的本地账号用量，不按 topN 截断
    accountsTotal: number        // 有请求的账号总数
  }
  series: OverviewSeriesPoint[]  // 按 granularity 连续补齐，没有数据的桶补 0
  topModels: OverviewRankRow[]   // 合计口径
  topAccounts: OverviewRankRow[] // 本地账号口径
  topKeys: OverviewRankRow[]     // 合计口径，请求 API Key
  topPaths: OverviewRankRow[]    // 合计口径，请求 endpoint
  topErrors: OverviewRankRow[]   // 合计口径，按 errorRequests 降序
}

interface OverviewMetrics {
  requests: number
  successRequests: number
  errorRequests: number
  errorRate: number              // errorRequests / requests；requests = 0 时为 0
  inputTokens: number            // total_input_tokens
  outputTokens: number
  cacheReadInputTokens: number
  cacheCreationInputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage: number
  pricedRequests: number
  unpricedRequests: number
  averageDurationMs: number      // duration_ms_sum / duration_ms_count
}

interface OverviewSeriesPoint {
  bucketStart: string            // 桶起点（UTC ISO）
  label: string                  // 按 timezone 格式化，小时桶为 "MM-DD HH:00"，天桶为 "MM-DD"
  all: OverviewSeriesMetrics
  local: OverviewSeriesMetrics
  external: OverviewSeriesMetrics
}

interface OverviewSeriesMetrics {  // OverviewMetrics 的子集，控制响应体积
  requests: number
  errorRequests: number
  inputTokens: number
  outputTokens: number
  estimatedCostUsd: number
  originalCostUsd: number
  kiroMeteringUsage: number
}

interface OverviewPoolRow {
  poolId: number
  poolName: string               // 取 dimension_label；为空时显示 "#<poolId>"
  metrics: OverviewMetrics
  rawCostUsd: number
  billableCostUsd: number
  profitUsd: number
}

interface OverviewAccountRow {
  credentialId: number
  label: string                  // 取 dimension_label；为空时显示 "#<credentialId>"
  metrics: OverviewMetrics
}

interface OverviewRankRow {
  key: string
  label: string
  requests: number
  errorRequests: number
  estimatedCostUsd: number
}
```

拆分响应是上述完整结构的投影：

```ts
interface UsageOverviewSummaryResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  totals: UsageOverviewResponse['totals']
  external: Pick<UsageOverviewResponse['external'],
    'rawCostUsd' | 'billableCostUsd' | 'profitUsd' | 'costFloorAppliedRequests'>
  local: Pick<UsageOverviewResponse['local'], 'activeAccounts' | 'accountsTotal'>
}

interface UsageOverviewSeriesResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  series: UsageOverviewSeriesPoint[]
}

interface UsageOverviewLocalResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  local: UsageOverviewResponse['local']
}

interface UsageOverviewExternalResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  external: UsageOverviewResponse['external']
}

interface UsageOverviewRankingsResponse {
  generatedAt: string
  timezone: string
  range: UsageOverviewResponse['range']
  totalRequests: number
  totalLocalRequests: number
  totalErrors: number
  topModels: UsageOverviewRankRow[]
  topAccounts: UsageOverviewRankRow[]
  topKeys: UsageOverviewRankRow[]
  topPaths: UsageOverviewRankRow[]
  topErrors: UsageOverviewRankRow[]
}
```

不变式（实现阶段写成测试）：

1. 所有可加指标都满足 `totals.all = totals.local + totals.external`。
2. `totals.external.requests = SUM(external.pools[].metrics.requests)`，费用字段同理。
3. `SUM(series[].all.X) = totals.all.X`，`local` 和 `external` 同理。
4. `totals.external.kiroMeteringUsage = 0`，并且 `totals.local.kiroMeteringUsage = totals.all.kiroMeteringUsage`。
5. 减法得出的本地值出现负数时（只有数据不一致才会发生），截到 0，同时在服务端打 warn 日志。

### 4.3 查询实现要点

- 新增 `PostgresUsageStore::dashboard_overview(spec)`，放在 `src/storage/postgres.rs`，或拆到新文件 `src/storage/postgres/overview.rs` 以免主文件继续变大。实现时按仓库现有约定选择。
- 用一条 SQL 取出 `[from, to)` 内 `dimension IN ('global','external_pool','credential','model','endpoint','request_api_key','error')` 的全部桶行。然后在 Rust 里：
  1. 按 `granularity` 把每个 `bucket_start` 映射到展示桶；`day` 按时区零点映射。
  2. 分别累加 global、各外部池、各账号、各模型、各路径、各请求秘钥、各错误类型。
  3. 用 global 减去外部池之和，得到本地值。
- 只读事务、语句超时、查询限流都复用现有实现。
- 结果缓存沿用 admin service 现有的 2 秒短缓存做法（`src/admin/service.rs` 中 `admin_usage_*_cache_key`），缓存 key 包含规范化后的 `from`、`to`、`granularity`、`timezone`、`topN`。

### 4.4 改动文件清单

| 文件 | 改动 |
| --- | --- |
| `src/anthropic/usage.rs`（或新建 `src/anthropic/usage_overview.rs`） | 新增响应类型、范围解析函数 `resolve_overview_range`，以及 `UsageRecorder::dashboard_overview` |
| `src/storage/postgres.rs` | 新增 `dashboard_overview` 查询 |
| `src/admin/handlers.rs`、`src/admin/router.rs` | 新增 `/usage-dashboard/overview` 及 `summary`、`series`、`local`、`external`、`rankings` 五个拆分路由 |
| `src/admin/service.rs` | 新增完整 overview 短缓存；拆分接口只投影同一份缓存快照 |

现有接口全部保留，不改语义，也不删除：`/usage-dashboard/windows`、`series`、`top`、`accounts`、`breakdown`、`external-pool-billing`、`external-pool-risk`。旧版 `admin-ui` 和其他页面还在用这些接口。

## 5. 前端设计

### 5.1 设计参考

布局参考了三类成熟后台的共同做法：

- **Grafana / Datadog**：顶部放全局时间选择器，图表可以框选或点击下钻。
- **OpenRouter / 云厂商账单页**：按职责拆分汇总、来源池和排行，避免把所有信息压成一张超长页面。
- **sub2api 类网关后台**：账号池和上游池分开展示用量与健康状态。

据此定了两条原则：

1. 时间范围只有一个来源。页面上所有数字都跟随顶部选择的时间范围，只有「实时」小卡片例外，它单独标注为最近 60 秒。
2. 每个 Tab 只负责自己的数据域：汇总看事实负载和整体指标，本地账号看本地账号，外部池看外部财务与池明细，排行看单一维度比较。
3. 合计、本地、外部的一致性仍由后端不变式保证，但页面不把三口径对账表作为主要交互。
4. 排行必须展示百分比基数；模型、账号、秘钥、路径按请求占比，错误按错误占比。

### 5.2 页面结构（自上而下）

```
┌───────────────────────────────────────────────────────────────────────────┐
│ 总览          [今天][昨天][24h][7天][30天][本月][自定义…]  数据截至 14:05  ⟳ │
│               已下钻：2026-10-01 14:00–15:00  [返回整体范围]                │
├───────────────────────────────────────────────────────────────────────────┤
│ [汇总] [本地账号] [外部池] [排行]                                           │
├───────────────────────────────────────────────────────────────────────────┤
│ 汇总 Tab：实时负载（顶部） + 整体请求/成功率/Token/费用/积分 + 来源趋势       │
│ 本地账号 Tab：本地积分/估算计费/原始计费/错误率 + 账号运行态 + 账号排行       │
│ 外部池 Tab：原始成本/可计费金额/利润/保底 + 池运行态 + 全部池明细             │
│ 排行 Tab：[模型] [账号] [秘钥] [路径] [错误]                                │
│              排名 | 名称 | 请求/错误 | 占请求/占错误 | 估算计费               │
└───────────────────────────────────────────────────────────────────────────┘
```

各区块说明：

- **头部**
  - 预设按钮对应 `range`。
  - 「自定义」用 `Popover` 打开起止日期和小时选择，提交后变成 `from` / `to`。
  - 时间范围和下钻状态写进 URL 查询参数（`?range=` 或 `?from=&to=`），方便分享链接和刷新后保留。
  - 当前 Tab 写进 URL 查询参数（`?tab=local`、`?tab=external`、`?tab=rankings`），切换范围时保留 Tab。
  - 自动刷新复用现有的 `useAutoRefreshPreference`，只对包含「现在」的范围生效。
- **汇总 Tab**
  - 事实负载放在最顶部，数据来自最近 60 秒实时快照，明确标注“合计口径”。
  - 下方只保留整体请求、成功率、Token、估算计费、原始计费、Kiro 积分和平均耗时。
  - 趋势以本地/外部堆叠展示，点击某个桶下钻到小时或天。
- **本地账号 Tab**
  - 展示本地账号池自己的积分、费用、错误率、当前运行态和全部本地账号。
  - 页面显式区分“全部账号 / 有流量 / 无流量 / 禁用”计数；本地账号表不使用 Top N，不把有流量账号数当成列表上限。
  - 账号列表由全量账号列表、当前 runtime 和所选时间范围内的本地用量合并而来；无流量账号也显示，指标为 0。
  - 每行显示当前状态、在途/并发、RPM、成功/失败计数、请求数、Kiro 积分、估算计费、原始计费和错误率，并保留到 Usage 明细的时间范围筛选链接。
- **外部池 Tab**
  - 展示外部池自己的原始成本、可计费金额、利润、保底触发和当前运行态。
  - 表格列出时间范围内有流量的全部外部池，不截断。
- **排行 Tab**
  - 内部 Tab 分为模型、账号、秘钥、路径、错误五个维度。
  - 模型、秘钥、路径的百分比为该行请求数 / 总请求数；账号排行的百分比为该账号请求数 / 本地账号池总请求数；错误的百分比为该行错误数 / 总错误数。
  - 模型、秘钥、路径为合计口径；账号排行来自本地账号 rollup；错误排行按错误类型。
- **趋势交互**
  - 一张堆叠柱状图：本地一种颜色，外部一种颜色，指标可以切换；图表本体显示纵坐标、柱顶数值和 hover 明细，不在底部再放一张冗余趋势明细表。
  - 横坐标按时间桶数量动态扩宽；少量桶时铺满，桶多时保留单桶可读宽度并通过横向滚动查看；纵坐标固定在左侧，不跟随横向滚动移动。
  - 图例可点击开关本地账号池和外部池，隐藏或显示对应来源；柱顶数值、纵坐标和 hover 的“当前显示”随已选来源重新计算。
  - 点击某根柱子：把该桶的 `[bucketStart, bucketStart + 粒度)` 作为新的 `from` / `to` 重新查询。小时桶下钻后看的就是这一小时，天桶下钻后按小时展示那一天。
  - 头部显示「已下钻」并提供返回按钮，返回的是下钻前的时间范围（存放在 URL 的 `back` 参数里）。

### 5.3 删除或移出总览的内容

| 现有内容 | 处理 |
| --- | --- |
| 五个 tab（实时、流量、费用、账号质量、异常诊断） | 取消，改为汇总、本地账号、外部池、排行四个主 Tab |
| 三口径对账表 | 从页面移除；保留后端 all/local/external 不变式作为数据正确性约束 |
| `StatCard` 重复的费用和请求卡 | 汇总 Tab 只保留整体事实负载和少量整体指标；分域金额进入对应 Tab |
| 写入器健康（`UsageWriterHealthPanel`） | 移出总览，数据和接口保留。是否新建诊断页见 9.2 节 Q4 |
| Sticky 回退、usage 来源分布、状态分布（`BreakdownTabPanel`） | 同上，见 9.2 节 Q4 |
| 账号质量全量分页表（`AccountQualityPanel`） | 移出总览。总览本地 Tab 保留轻量全量账号状态 + 时间范围用量；账号编辑、批量操作、诊断等完整管理仍看 `/ui/credentials` |
| `ExternalPoolBillingPanel`（`ui/src/features/usage/usage-billing.tsx`） | 总览不再使用，由 ④ 外部池表替代。目前只有总览用到它，默认一并删除（见 9.1 节第 3 条） |

### 5.4 前端组织

API 和 hook 已按职责拆分；本次实现将页面子区块（汇总、本地池、外部池、排行、实时条）保留在同一个 `overview-page.tsx` 中，优先保证 Tab 行为和接口职责清楚。后续如果继续扩展交互，再按下表拆成独立文件，属于代码组织优化，不改变接口契约：

| 文件 | 内容 |
| --- | --- |
| `overview-page.tsx` | 页面组装，以及 URL 参数和时间范围状态 |
| `overview-range.ts` | 预设范围、URL 参数解析和序列化、下钻和返回逻辑（纯函数，可以单测） |
| `range-picker.tsx` | 头部的范围选择器，自定义范围用 `Popover` |
| `trend-panel.tsx` | 汇总 Tab 的趋势堆叠图和指标切换 |
| `local-pool-section.tsx` | 本地账号 Tab |
| `external-pool-section.tsx` | 外部池 Tab |
| `rank-panels.tsx` | 排行 Tab 及五个排行维度 |
| `realtime-strip.tsx` | 汇总 Tab 顶部实时负载 |
| `overview-format.ts` | 指标口径文案（tooltip）、金额和占比格式化 |

配套改动：

- `ui/src/api/usage.ts` 新增 `getUsageOverview(params)` 以及 `getUsageOverviewSummary/Series/Local/External/Rankings(params)`。
- `ui/src/hooks/use-usage.ts` 新增 `useUsageOverview(params, refetchInterval)` 以及五个拆分接口 hook；新版总览页使用拆分 hook。
- `ui/src/types/api.ts` 新增 4.2 节定义的类型。
- **usage 页的 URL 参数**：`/ui/usage` 需要支持从 URL 读取 `routeKind`、`credentialId`、`externalPoolId`、`since`、`until`，作为初始筛选条件，这样总览才能跳过去。usage 页面目前的筛选状态没有和 URL 同步，这是本方案唯一需要动的其他页面。是否纳入本次范围见 9.2 节 Q3。

组件约束：使用 `@/components/ui` 和 `@/components/patterns` 已有的组件，包括 `Table`、`Popover`、`Button`、`Tabs`、`SectionCard`、`StatCard`、`Callout`；图表使用 `@/components/charts`，底层是 recharts。不用原生 `<button>`，不引入新的依赖。

## 6. Demo 环境与 Mock 数据

### 6.1 要求

- 用途：本地演示和验收新总览。数据量和分布要接近现网，能直接看出拆分效果。
- **不能影响任何正在运行的实例和数据**。2026-10-02 时本机有两个实例在跑，都不能碰：
  - `127.0.0.1:19023`：库 `kiro_thinking_budget_20260901`，属于本工作区的长期测试实例。
  - `127.0.0.1:9022`：库 `kiro_console_dev`，属于另一个工作树 `2ue_kiro.rs-console` 的实例。
- 数据必须经过真实写入路径（`PostgresUsageStore::record_batch`），这样明细表和 rollup 表天然一致，演示时看到的就是真实查询结果。不允许直接往 rollup 表里写 SQL。

### 6.2 资源

| 资源 | 值 |
| --- | --- |
| PostgreSQL | 复用本机容器 `kiro-rs-postgres-local`（`127.0.0.1:25432`，用户 `kiro_rs`），新建库 `kiro_overview_demo_20261002` |
| Redis | 复用本机容器 `kiro-rs-redis-local`（`127.0.0.1:26379`），使用 db `13`，`keyPrefix` 为 `kiro_rs:overview_demo` |
| 服务端口 | `127.0.0.1:19141`（启动前检查是否被占用） |
| 配置目录 | `tmp/overview-demo/`，包含 `config.json` 和 `credentials.json`，已被 git 忽略 |
| 前端 | 先执行 `pnpm --dir ui build`，再让服务以 filesystem 模式提供 `ui/dist`（环境变量 `KIRO_NEW_UI_MODE=filesystem`），访问 `http://127.0.0.1:19141/ui/overview` |
| Kiro 上游 | 指向一个不存在的本地地址（例如 `http://127.0.0.1:1`），demo 只看统计数据，不发真实请求，也不消耗真实账号 |

### 6.3 造数工具

新增维护子命令（离线执行，和现有的 `maintenance migrate` 等命令同一套机制）：

```
kiro-rs -c tmp/overview-demo/config.json maintenance usage-demo-seed \
  --days 8 --seed 20261002 [--reset]
```

- 实现位置：`src/model/arg.rs` 新增 `MaintenanceCommand::UsageDemoSeed { days, seed, reset }`；`src/main.rs` 的 `handle_maintenance_command` 负责分发；造数逻辑放在新文件 `src/storage/usage_demo_seed.rs`。
- **安全护栏**（缺一条就拒绝执行）：
  1. 库名必须以 `kiro_overview_demo_` 开头；
  2. `usage_records` 为空，或者显式传了 `--reset`（`--reset` 只清空 demo 库里 usage 相关的表）；
  3. 复用现有的离线维护锁 `PostgresUsageLifecycleGuard::acquire_offline_maintenance`。
- 写入顺序：先通过现有存储接口写入 demo 账号和外部池，让账号和池名称能在页面上显示；然后生成 usage 记录，每 500 条调用一次 `record_batch`。
- 随机数用固定 seed，保证每次结果可以复现：使用项目已有的依赖 `fastrand`，通过 `fastrand::Rng::with_seed(seed)` 创建，不引入新依赖。

数据画像（覆盖约 8 天，总量约 1 万条；固定 seed 可复现）：

| 维度 | 设定 |
| --- | --- |
| 本地账号 | 24 个，标签为 `demo-acc-01@example.com` … `demo-acc-24@example.com`。其中 20 个启用，2 个禁用，2 个配置了但没有流量 |
| 外部池 | 3 个：`demo-pool-alpha`（流量大，有利润）、`demo-pool-beta`（流量中等，部分请求触发保底）、`demo-pool-gamma`（流量小，错误率偏高） |
| 流量曲线 | 按北京时间的昼夜曲线：白天高峰，凌晨低谷；周末下降约 30% |
| 本地 / 外部比例 | 本地约 72%，外部约 28%。第 3 天模拟一次本地容量紧张，外部占比在几个小时里升到 60% |
| 模型 | `claude-opus-5-5` 占 55%，`claude-sonnet-5` 占 35%，`claude-haiku-4-5` 占 10% |
| 状态 | success 约 97.5%，error 约 1.5%，client_dropped 约 0.8%，stream_error 约 0.2%。第 5 天下午模拟一次本地错误尖峰（约 1 小时，错误率 15%） |
| Token / 费用 | 输入 Token 按对数正态分布（中位数约 6 万），输出 Token 中位数约 1,500。费用由现有价格表（`pricing_catalog.estimate`）计算。本地记录 `original_cost_usd` 约为估算值的 1.1–1.3 倍；`kiro_metering_usage` 每请求约 0.1–2.0 |
| 外部池计费 | 构造 `ExternalPoolBilling`：`raw_cost_usd`、`billable_cost_usd` 按池设定不同加价率（alpha +35%、beta +5% 且部分请求触发保底、gamma +20%），`estimated_cost_usd = billable_cost_usd`，`original_cost_usd = raw_cost_usd`，`kiro_metering_usage = 0`，和真实写入口径一致 |
| 时间分布 | 从 `now − 8 天` 到 `now`，最后一条记录在执行时刻之前几分钟，保证「今天」和「最近 24 小时」有数据 |

## 7. 验证计划

### 7.1 后端单测（`cargo test`）

- 范围解析：每个预设 `range` 的起止时间；自定义 `from/to` 的整点对齐；超过 90 天、`from >= to`、非法时间返回 400；`day` 粒度按时区零点切分。
- 拆分不变式：按 4.2 节的 5 条不变式，构造包含本地、外部、本地拒绝（`routeKind` 为空）、外部错误的混合记录，写入后断言成立。PgSQL 测试复用现有的 `connect_test` 测试 schema 机制。
- 跨小时、跨天的 series 要连续补零，各桶之和等于 totals。
- `topN` 截断，以及 `accountsTotal` 计数。

### 7.2 前端

- `pnpm --dir ui build` 通过（包含 `tsc -b`）。
- `overview-range.ts` 的纯函数：URL 解析和序列化往返一致，下钻后能正确返回。

### 7.3 Demo 端到端（人工验收）

1. 在 demo 库上执行造数命令，再启动 19141 实例。
2. 用浏览器打开 `/ui/overview`，依次检查汇总、本地账号、外部池、排行四个主 Tab；确认实时负载出现在汇总 Tab 顶部，而不是页面底部。
3. 在排行 Tab 依次检查模型、账号、秘钥、路径、错误五个内部 Tab；确认每行显示数量、百分比和估算计费，百分比基数分别为总请求、本地请求或总错误。
4. 切换今天、最近 7 天、自定义范围和趋势下钻，确认 Tab 参数和时间参数不会互相覆盖。
5. 数据一致性验证仍在后端做：合计、本地、外部可加指标满足 all = local + external；页面不再展示三口径对账表。直接查询 SQL 只作为辅助证据，允许整点对齐带来的边界差异。
6. 验收完成后停掉 19141 实例。demo 库保留，方便以后重复演示；需要清理时执行 `DROP DATABASE kiro_overview_demo_20261002`。

## 8. 实施顺序

| 步骤 | 内容 | 产出 |
| --- | --- | --- |
| 1 | 后端：类型、范围解析、PgSQL 查询、handler 和路由、单测 | `/usage-dashboard/overview` 及五个拆分接口可用 |
| 2 | 造数命令和 demo 环境 | demo 库有约 8 天数据 |
| 3 | 前端：按 5.4 节拆文件并重写总览 | 新总览页 |
| 4 | usage 页支持 URL 参数（取决于 9.2 节 Q3） | 总览可以跳转到明细 |
| 5 | 按 7.3 节做 demo 验收并截图 | 截图和对账结果 |

提交策略：后端接口、造数命令、前端重写各自是独立功能，分别提交。

## 9. 风险与待确认事项

### 9.1 已知风险（已有默认处理，不阻塞实施）

1. **本地账号池口径包含少量本地拒绝记录**：`routeKind` 为空的记录（例如入口校验失败）会算进本地账号池。现网最近 7 天这类记录有 74 条，全部是 error，费用为 0，所以对费用没有影响，只会让本地的请求数和错误数略微偏大。如果要把它们单独列成「入口拒绝」，需要新增 rollup 维度，这属于新增写入，本方案不做。
2. **运行态只有当前快照**：账号池和外部池的运行态（可调度、冷却、在途等）来自实时接口，不会随所选时间范围变化。下钻到历史时间点时，这一行仍然显示当前状态，页面上会标注「当前」。
3. **旧组件保留为兼容代码**：新版总览不再调用 `ui/src/features/usage/usage-billing.tsx` 里的 `ExternalPoolBillingPanel`，但该文件仍被 usage 明细页使用其它费用组件，因此本次不删除文件；`ExternalPoolBillingPanel` 作为未使用的旧导出保留，后续可单独清理。
4. **工作区有一处未提交的临时改动**：`ui/src/features/overview/overview-page.tsx` 在 `c7f9edb` 之后改过一版，在费用 tab 里用前端减法拆出了本地费用，这个改动没有提交。按本方案重写总览时，这个改动会被整体替换，不需要单独提交，也不需要保留。

### 9.2 已按默认结论处理的问题

下表每一项都给了默认做法。2026-10-02 实施时按默认结论落地；如果后续要改变其中任意一项，应新开 follow-up，而不是在总览重构里继续扩大范围。

| 编号 | 问题 | 可选方案 | 默认做法 | 影响范围 | 结论 |
| --- | --- | --- | --- | --- | --- |
| Q1 | 新接口只按整点统计，能否接受？例如当前时间是 14:35 时，「今天」实际统计的是 00:00 到 15:00 这一整段，而当前小时的数据仍在持续增长；旧接口则是秒级精确到 14:35。两者在当前这一小时内会有差异。 | A. 接受整点精度，页面显示「数据截至」时间。<br>B. 和旧接口一样，对时间范围两端不足一小时的部分扫明细表，做到秒级一致。 | A | 若选择 B：第 4.3 节的查询需要增加两端扫明细的逻辑，拆分计算也要在明细上再做一遍，后端工作量大约翻倍，查询耗时也会增加。 | 结论：采用 A。当前小时纳入统计，页面显示 `dataThrough` 和整点对齐后的范围。 |
| Q2 | 模型、秘钥、路径和错误排行是否需要区分本地账号池和外部池？ | A. 只看合计，页面标注「合计口径」；账号排行单独使用本地账号维度。<br>B. 区分来源。 | A | 若选择 B：需要在 `usage_rollup_dimensions` 里新增「来源 × 模型」「来源 × 错误」等组合维度，历史数据也无法直接补齐。秘钥排行本次新增单独的 `request_api_key` 维度，但不新增来源交叉维度。 | 结论：采用 A。排行 Tab 明确标注合计口径；账号只代表本地账号池，模型/秘钥/路径/错误不区分来源。 |
| Q3 | 这次是否一起改 usage 明细页（`/ui/usage`），让它能从 URL 参数读取筛选条件？做了之后，总览里点某个账号或某个外部池，可以直接跳到对应的请求明细。 | A. 这次一起做（第 8 节步骤 4）。<br>B. 这次不做，总览里先不放跳转明细的链接。 | A | 选 A：只改 usage 页的初始筛选，从 URL 读取 `routeKind`、`credentialId`、`externalPoolId`、`since`、`until`，不改它的查询逻辑。选 B：总览里账号行和外部池行不可点击，只保留「查看全部账号」「外部池管理」这类页面级链接。 | 结论：采用 A。总览提供到 usage 明细的来源筛选跳转，筛选逻辑不变。 |
| Q4 | 写入器健康、Sticky 回退、usage 来源分布、状态分布这些诊断信息移出总览后，是否要新建一个「诊断」页来放？ | A. 这次不做，这些信息在新版 UI 里暂时看不到（接口保留，旧版 `admin-ui` 的用量面板里仍能看到一部分）。<br>B. 这次新建 `/ui/diagnostics` 页，把这些面板原样迁过去。 | A | 若选择 B：新增一个路由和页面，代码大多从旧总览迁移，工作量较小，但会扩大本次的改动和验收范围。 | 结论：采用 A。本次总览只保留业务统计，诊断接口和旧版页面继续可用，单独诊断页留作后续。 |

## 10. 本地实现与验证证据（2026-10-02）

- 新增并验证五个拆分接口：`summary`、`series`、`local`、`external`、`rankings`。完整 `/overview` 保留。
- 新版 `ui/src/features/overview/overview-page.tsx` 已改为四个主 Tab：汇总、本地账号、外部池、排行；实时负载位于汇总 Tab 顶部，页面不再展示三口径对账表。
- 本地账号 Tab 已从 Top N 改为全量账号表：前端合并 `/credentials/list`、`/credentials/runtime` 与 `/overview/local` 用量；demo 当前显示 `24` 个配置账号，并显式拆开“全部 / 有流量 / 无流量 / 禁用”计数，禁用或无流量账号也保留在表内并显示当前状态。
- 排行接口新增 `totalRequests`、`totalLocalRequests`、`totalErrors` 以及 `topModels`、`topAccounts`、`topKeys`、`topPaths`、`topErrors`；排行表显示请求/错误数量、百分比和估算计费。
- usage rollup 新增 `request_api_key` 维度，demo 数据包含多个请求秘钥和请求路径，确保排行 Tab 有真实差异。
- `/ui/usage` 已支持从 URL 初始化 `routeKind`、`credentialId`、`externalPoolId`、`since`、`until`，用于从总览行跳转到请求明细。
- 造数命令已落地：`maintenance usage-demo-seed --days 8 --seed 20261002 --reset`。本地 demo 库 `kiro_overview_demo_20261002` 写入约 1 万条记录，其中本地/外部约 72%/28%、账号 `24` 个、外部池 `3` 个，并覆盖 3 个请求秘钥和 4 个请求路径。
- demo 服务已重启在 `127.0.0.1:19141`，New UI 使用 filesystem 模式读取 `ui/dist`，访问地址是 `http://127.0.0.1:19141/ui/overview`，管理 key 是 `sk-overview-demo-admin`。
- 最近 7 天拆分接口对账通过（最终检查快照：2026-10-02 17:42 CST；该窗口会随当前时间滚动）：合计 `9227` 请求 = 本地 `6515` + 外部 `2712`；本地 Kiro 积分 `6840.534176371273`；整体估算计费 `$6056.69575182457`；整体原始计费 `$6591.479553744819`；外部原始成本 `$1409.933749843066`、可计费金额 `$1738.41256470057`、利润 `$328.478814857504`；外部池数量 `3`，活跃本地账号 `20`。
- 趋势接口最近“今天”返回 `18` 个非零小时桶；每个桶同时包含 `all/local/external` 数字，前端汇总 Tab 用趋势图本体展示纵坐标、柱顶数字、hover 数值和本地/外部图例开关，去掉底部趋势明细表。
- 排行接口同一快照返回 `totalRequests=9227`、`totalLocalRequests=6515`、`totalErrors=271`，模型 `3` 项、账号 `20` 项、秘钥 `3` 项、路径 `4` 项、错误 `4` 项；账号百分比按本地请求基数，其余请求排行按总请求基数，错误按总错误基数。
- 验证命令通过：`cargo fmt --check`、`cargo check --locked`、`cargo test --locked anthropic::usage::tests`（30 passed）、`pnpm --dir ui build`、`git diff --check`。

## 11. Usage 清理语义修正（2026-10-02）

### 11.1 问题结论

用户反馈“清理 usage 明细后，usage 页面顶部统计仍残留部分数据”。排查后确认这不是单纯缓存问题，而是清理语义混在一起导致的：

- 旧实现的 `soft_delete_cleanup_batch` 和 `hard_delete_cleanup_batch` 在清理明细时会同步对 rollup 做负数扣减，清理明细等同于清理汇总。
- 用户实际需要的是默认清理 usage 明细，但保留已经形成的汇总数据；只有显式选择“清理全部历史数据（包含汇总）”时才清空汇总。
- 旧弹层把“清理全部”按钮放在顶部、参数放在下面，还要求用户配置批次数，交互不符合运营场景。

### 11.2 落地语义

- `UsageCleanupRequest` 新增 `includeSummary` 和 `maxRows`。
- 默认请求：
  - `olderThanDays = 0`，表示从任务开始时刻往前清理。
  - `pauseMsBetweenBatches = 10`。
  - 页面默认 `maxRows = 10000`，表示本次最多清理 1 万条；API 层保留 `maxRows = 0` 或不传表示清理全部匹配明细。
  - `includeSummary = false`，只清理明细，保留 PostgreSQL rollup、账号费用汇总、趋势和排行汇总。
- 显式勾选“清理全部历史数据（包含汇总）”：
  - 固定为当前时刻之前的全量清理。
  - 固定使用软删除语义，避免 API 直接调用时出现“明细仍存在但汇总被清空”的不一致。
  - 清理明细后不会直接裸清 PostgreSQL 汇总表；会在同一组写入保护下清空 rollup 并按 `deleted_at IS NULL AND rollup_active` 的有效明细重建汇总，保护任务期间新进入的正常用量记录。
  - 重建后失效 Redis usage 汇总缓存和进程内 PgSQL 汇总缓存。
- 前端 `usage-cleanup-modal.tsx` 重写为同一流程：范围、最多清理条数、批次间隔、包含汇总勾选、预览、开始清理；不再把“清理全部”作为顶部独立按钮，也不再让用户填写批次数。
- 清理弹层新增“物理删除明细”勾选项，默认不勾选；勾选后使用 hard-delete 直接删除命中的活跃明细或旧软删除明细，但默认仍保留汇总。该选项与“包含汇总”互斥，避免无法从物理删除的明细重建汇总。
- 清理完成后的 React Query 失效范围补齐到拆分总览 key：`usage-overview-summary`、`usage-overview-series`、`usage-overview-local`、`usage-overview-external`、`usage-overview-rankings`。

### 11.3 验证证据

- `cargo fmt --check`
- `cargo check --locked`
- `cargo test --locked usage_cleanup -- --nocapture`：18 passed；未配置本地 `KIRO_RS_TEST_POSTGRES_URL` / `KIRO_RS_TEST_REDIS_URL` 的集成项按原测试逻辑跳过。
- `cargo test --locked postgres_detail_cleanup_preserves_summary_until_explicit_purge_for_three_rounds -- --nocapture`：测试编译通过；当前环境无 PgSQL 测试库时按原逻辑跳过。测试契约锁定“明细清理保留汇总；显式汇总清理才归零”。
- `cargo test --locked postgres_physical_cleanup_preserves_rollups_for_active_rows_for_three_rounds -- --nocapture`：测试编译通过；当前环境无 PgSQL 测试库时按原逻辑跳过。测试契约锁定“直接物理删除活跃明细时，默认不扣减 rollup 和账号汇总”。
- demo 真实模拟验证（2026-10-03，`kiro_overview_demo_20261002`，短数据集）：未勾选“清理全部历史数据（包含汇总）”，直接执行 `mode=hard_delete`、`includeSummary=false`、`olderThanDays=0`、`maxRows=10000`、`batchSize=5000`、`pauseMsBetweenBatches=10`。预览命中 `1223` 条，任务完成后 `usage_records` 明细总数从 `1223` 变为 `0`，但以下汇总逐字段保持不变：
  - usage 顶部统计：请求、成功、错误、输入/输出 Token、估算计费、原始计费、Kiro 积分、外部池计费和 topCredentials 均一致。
  - 账号统计：`/credentials/usage-summary?ids=1..24` 返回的 `20` 个有流量账号费用、积分、计价/未计价请求均一致；`/credentials/summary` 的 total/available/disabled/schedulable/current runtime 计数不受影响。
  - 总览统计：完整 `/overview` 以及拆分 `summary`、`local`、`external`、`rankings` 投影在忽略 `generatedAt/range` 后完全一致。
- demo 已恢复完整 8 天数据并重启（2026-10-03）：`maintenance usage-demo-seed --days 8 --seed 20261003 --reset` 写入 `9776` 条，其中本地 `6901`、外部 `2875`、账号 `24` 个、外部池 `3` 个；服务运行在 `127.0.0.1:19141`，`/ui/overview` 和 `/ui/usage` 返回 `200`，cleanup 状态为 `idle`。
- `pnpm --dir ui build`
- `git diff --check`

## 12. 账号积分与外部池展示补全（2026-10-03）

### 12.1 积分口径

账号积分展示拆成两类事实，避免“账号已经失效后，历史消耗也被一起抹掉”：

- **历史消耗**：已消耗积分、估算计费、原始计费对所有有 usage 汇总的账号计入，包括后来禁用、403、权限失效或 Token 无效的账号。
- **当前可用余额**：剩余积分和预估剩余估算费用只对当前可用且已有余额快照的账号计入。账号被禁用，或 runtime 最近错误明确属于认证/权限类错误时，剩余积分显示“不可用”，预估剩余费用显示“不可估算”。
- **估算公式**：`估算计费 / 已消耗积分 × 剩余积分`。已消耗积分为 `0` 时不除零，显示“无法估算”；没有余额快照时显示“未查询”。
- **临时运行态错误**：429、普通网络错误、5xx、短暂冷却不自动抹掉历史余额；只有明确不可用的禁用/认证权限类状态阻止余额计入。

### 12.2 账号页改动

- 顶部卡片收敛为“账号、并发、RPM、积分”，账号卡合并可调度和异常账号计数。
- 账号卡片的状态计数用颜色区分：可调度为成功色、异常为告警色、禁用为危险色；无对应状态时降为弱提示色。
- 账号列表筛选收敛为管理动作优先：默认展示账号定位、ID、状态、订阅和代理；全文、模型、Region、Endpoint、认证方式以及优先级/RPM/并发精确值放入高级筛选。
- 并发、RPM、TPM 等数字使用完整数字格式；RPM 卡只显示数字，不在数值后重复拼接 `RPM`。
- 运行配置页的保存动作改为底部悬浮操作栏；只有当前表单和已保存快照存在差异时才允许提交，保存成功后更新快照。
- “剩余可用积分”改为“积分”，点击后弹层显示全部账号的已消耗积分、估算计费、原始计费、剩余积分和预估剩余估算费用。
- 弹层汇总中历史消耗包含不可用账号；余额和剩余费用汇总显式显示已排除的不可用账号或未查询账号数量。

### 12.3 总览改动

- 本地账号 Tab 合并余额快照和 runtime；统计卡顺序调整为“已消耗积分、估算费用、积分转换率、剩余积分、预估估算费用、错误率”，原始计费并入估算费用卡片说明，不再单独占一张卡；估算费用卡展示 `估算费用 - 原始计费` 的中性差值和占原始计费百分比，不使用外部池成本收益语义。
- 本地账号全量表继续显示剩余积分与预估剩余费用；不可用账号只排除当前余额和预估剩余费用，历史消耗仍计入。
- 外部池 Tab 改回参考 main 分支 / 现网的成本拆分表格，不再把 Base URL、认证、模型映射、header、冷却、质量等配置详情塞进总览。
- 外部池 overview API 补齐现网计费字段：`shapedCostUsd`、`upliftedCostUsd`、`reportedCostUsd`、`costFloorDeltaUsd`、逐池 `costFloorAppliedRequests`。
- 外部池表格按池展示：请求、上游原始成本、展示计费、补偿后计费、差额占原始、原始/整形倍率、未计价、兜底；第一列保留跳转 Usage 明细。
- 在字段口径保持现网一致的基础上，仍合并 `/external-pools` 的全部配置池；没有流量的池以 `0` 展示并标注“无流量”。

### 12.4 Demo 与验证

- `usage-demo-seed` 为 24 个 demo 账号写入确定性的余额快照；其中 20 个启用账号和 2 个禁用账号都有历史 usage，另 2 个账号无流量，用于验证“历史消耗保留、不可用余额排除”。
- demo 服务使用 `KIRO_NEW_UI_MODE=filesystem` 重启在 `127.0.0.1:19141`，页面入口为 `/ui/overview`、`/ui/credentials`、`/ui/usage`，管理 key 为 `sk-overview-demo-admin`。
- 验证通过：`pnpm --dir ui build`、`cargo fmt --check`、`cargo check --locked`、`git diff --check`；三个 UI 入口返回 200，账号余额、usage 汇总、overview summary/external 接口均返回正常数据。

## 13. 总览 Tab 信息去重（2026-10-03）

### 13.1 展示规则

- 主 Tab 本身已经表达分区时，Tab 内不再重复显示同名大标题；只保留真正需要区分内容块的标题，例如汇总 Tab 里的“实时负载”和“趋势”。
- 卡片、表格字段和小计摘要表达同一件事时，只保留卡片或表格。总览页不再在表格上方重复展示“全部 / 有流量 / 无流量 / 禁用”这类账号计数条。
- 说明文字只保留会影响读数的口径，例如“不可用账号不计入剩余积分”和“请求 / 错误占比”的列名；能从字段名直接理解的解释性文案移除。
- 底部不再放服务端生成时间、时区、实时口径说明这类重复页脚；时间范围和数据截至时间仍保留在页面顶部。

### 13.2 当前落地

- 汇总 Tab：去掉“整体概览”分区标题和趋势说明，只保留实时负载、核心汇总卡片、趋势图、指标切换和本地/外部图例开关。
- 本地账号 Tab：去掉分区标题、账号数徽标、账号明细标题以及“全部 / 有流量 / 无流量 / 禁用”摘要。列表继续显示全部已配置账号，但字段只保留累计请求、已消耗积分、估算费用、积分转换率、剩余积分、预估估算费用和错误率。
- 本地账号卡片新增“积分转换率”，汇总和每个账号都同时展示 `估算费用 / 已消耗积分` 与 `原始费用 / 已消耗积分`，数值固定保留 3 位小数。
- 外部池 Tab：去掉分区标题、差额徽标、总体差额进度条和外部池小计条。表格仍按 main 分支 / 现网成本拆分字段展示全部配置池，包括无流量池。
- 排行 Tab：去掉外层“排行”标题、内部“模型排行 / 账号排行”等二级标题和口径说明；维度由内部 Tab 表达，表格继续保留数量、百分比和估算计费。

### 13.3 验证证据

- `pnpm --dir ui build`
- `git diff --check`
- demo 服务已用 `KIRO_NEW_UI_MODE=filesystem` 重启在 `127.0.0.1:19141`；`/ui/overview` 返回 `200`。
- `overview/summary?range=last7d` 验证通过：总请求 `8496`，本地 `6023`，外部 `2473`。
- 调试残留清理后，未发现 `chrome-devtools-mcp` 或 `remote-debugging` 相关进程。

## 14. 账号筛选模型与区域（2026-10-03）

### 14.1 当前落地

- 账号列表的“可用模型”改为下拉选择，选项来自 `/api/admin/model-capabilities` 的模型目录，并按模型 ID 去重、排序。
- `Auto` 只保留在后端能力目录中供兼容使用，不进入账号列表筛选选项；因此筛选请求不会由 UI 产生 `model=auto`。
- 新增“区域”下拉筛选，选项从全量账号目录收集显式区域和最终生效的认证/API 区域（`region`、`authRegion`、`apiRegion`、`effectiveAuthRegion`、`effectiveApiRegion`）。
- 账号列表查询同时传递 `model` 与 `region`，后端继续按支持模型和显式/生效区域过滤，不改变账号目录的分页与状态语义。

### 14.2 验证证据

- `pnpm --dir ui build` 通过；`cargo check` 通过；`git diff --check` 通过。
- Demo 模型目录返回 `32` 个模型，前端筛选后的选项为 `31` 个，`hasAuto=false`；列表中包含具体 Claude 模型，例如 `claude-haiku-4-5-20251001`。
- Demo 账号目录返回 `24` 个账号，区域选项包含 `us-east-1`；`model=claude-haiku-4-5-20251001&region=us-east-1` 返回账号结果，`region=eu-west-1` 返回 `0` 条。
- 演示服务仍运行于 `127.0.0.1:19141`，UI dev server 运行于 `127.0.0.1:9023`；清理了本次调试产生的 `chrome-devtools-mcp` / remote-debugging 残留，普通 Chrome 与 Chrome Dev 进程保持运行。

## 15. 订阅名称归一化（2026-10-03）

### 15.1 官方依据

- [Kiro Pricing](https://kiro.dev/pricing/) 当前正式列出的订阅为 `Kiro Free`、`Kiro Pro`、`Kiro Pro+`、`Kiro Pro Max`、`Kiro Power`。
- [Kiro for students](https://kiro.dev/students/) 使用 `Kiro Students tier` 命名；后台将 `Kiro Students` 与 pricing 页订阅放在同一套枚举中处理。
- 因此 `Trial` 不再作为正式订阅筛选项；历史 trial 标题只作为兼容别名归入 Free，不在 UI 中继续展示为独立官方方案。

### 15.2 当前落地

- 前端统一使用官方显示名和稳定筛选值：
  - `free` → `Kiro Free`
  - `students` → `Kiro Students`
  - `pro` → `Kiro Pro`
  - `pro_plus` → `Kiro Pro+`
  - `pro_max` → `Kiro Pro Max`
  - `power` → `Kiro Power`
  - `unknown` → `未知订阅`
- 兼容历史标题和别名，例如 `KIRO STUDENT`、`Kiro Student`、`Kiro Students` 和 `student/students` 查询值都会归一到 `students`。
- 后端订阅过滤改为规范化 key 精确匹配，不再使用标题包含匹配；筛选 `pro` 不会命中 `pro_plus` 或 `pro_max`。
- 订阅徽标、积分明细表、订阅筛选下拉、导入/新增弹层提示、模型页账号选择列表都使用同一套前端归一化函数；原始上游标题仍保留在 API 中用于审计。
- 订阅识别只用于后台展示、筛选、校验分组和积分展示，不参与本地账号调度、外部池路由、重试、限流或账号选择。
- demo 账号覆盖 `KIRO STUDENT`、`KIRO PRO+` 和 `KIRO FREE`，用于验收筛选与展示。

### 15.3 验证证据

- `cargo test --locked subscription_ -- --nocapture`：2 passed。
- `cargo test --locked credit_snapshot_uses_overage_bonus_for_all_paid_tiers -- --nocapture`：1 passed，包含学生 1,000 credits 断言。
- `pnpm --dir ui build`、`cargo check`、`cargo fmt --check`、`git diff --check` 通过。
- Demo `/api/admin/credentials-paged`：
  - `subscription=students` 返回 2 个 `KIRO STUDENT` 账号；
  - `subscription=student` 兼容查询同样返回 2 个；
  - `subscription=pro` 返回 0 个；
  - `subscription=pro_plus` 返回 18 个；
  - `subscription=free` 返回 4 个。

## 16. 外部池计费字段对齐（2026-10-03）

### 16.1 发布基线

- 已核对远端最新发布 tag `v0.0.183`；外部池计费展示基线来自 `ui/src/features/usage/usage-billing.tsx`、`ui/src/features/overview/overview-page.tsx` 与 `ExternalPoolBilling` 后端结构。
- `rawCostUsd` 是上游原始 usage 成本；`shapedCostUsd` 是按当前路径整理后的展示计费中间口径；`upliftedCostUsd` / `reportedCostUsd` 是最终上报 usage 计算出的补偿后口径；`billableCostUsd` 是外部池最终可计费金额。

### 16.2 当前落地

- 外部池总览卡片展示“外部池可计费”，其说明中同时给出补偿后计费和差额；不再把“补偿后计费”误作唯一的管理指标标题。
- 外部池表格字段与发布版保持一致：`外部池`、`请求`、`上游原始成本`、`展示计费`、`补偿后计费`、`差额占原始`、`原始/整形倍率`、`未计价`、`兜底`。
- 在表格上方补充简短口径说明，并给“展示计费”和“补偿后计费”增加 hover 解释，避免两个字段被理解成重复数据。
- 前端成本回退使用 nullish 语义，保留合法的 `0` 成本值；与 `v0.0.183` 的 `??` 处理保持一致。

### 16.3 验证证据

- `pnpm --dir ui build` 通过。
- `git diff --check` 通过。
- 修改仅位于总览 UI 与计划记录，没有改动外部池计费计算、路由选择或请求调度逻辑。
