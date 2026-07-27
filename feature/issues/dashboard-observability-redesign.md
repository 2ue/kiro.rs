# Dashboard / UI observability redesign

Status: `analysis-complete / implementation-in-progress`

Severity: `P1`

Owner intent: dashboard 不是把 usage 字段堆在页面上，也不是简单拆接口、换 Tab、加几张卡片。它必须让维护者在服务出问题时快速回答：系统现在是否健康、流量是否异常、调度是否卡住、费用/积分是否正常、哪些账号/外部池质量差、错误根因集中在哪里、统计系统本身是否在拖垮主业务。

Last reviewed: 2026-07-28 Asia/Shanghai

## 0. 结论

当前工作树里的 dashboard 改动还不能算完整重构，只是一个局部技术改造：

- 新 UI `OverviewPage` 把页面拆成了 `实时 / 流量 / 费用 / 异常` 四个 Tab，并把部分查询按 Tab 延迟加载。
- 后端把 `/usage-dashboard/top` 做成了可按 `windowKey` 查询。
- UI 补了一些费用、Kiro metering/积分、本地账号质量字段。

但这些不等于 dashboard 设计完成。核心缺口是：

1. 没有先定义 dashboard 要回答的运维问题。
2. 没有定义每个指标的数据时间语义：实时、当前窗口、趋势范围、累计、库存状态、后台统计健康。
3. 没有定义新旧 UI 能力一致性边界。
4. 没有定义费用口径：本地估算、本地实际/原始、Kiro 积分、外部池 raw/shaped/uplifted/billable/profit。
5. 没有定义账号质量视角：账号能不能调度、是否正在卡队列、错误率、延迟、成本、积分、余额、模型限制、选中压力。
6. 没有定义统计查询和主业务之间的硬隔离验收。

因此本文件把当前 dashboard/UI 需求重新拉平，作为后续实现的权威合同。后续不能再按“看到页面缺啥就加啥”的方式改。

## 0.1 根因

Dashboard/UI 问题的根因不是单个接口慢或单张卡片缺字段，而是信息架构和数据时间语义没有被定义成产品合同：

- 实时运行态、选定窗口统计、趋势范围、累计费用、账号库存和统计系统健康被混在同一个总览页面。
- 新旧 UI 没有共用能力矩阵，导致同名指标可能口径不同。
- 成本口径只局部覆盖外部池，未把本地账号成本、Kiro 积分、返回下游 usage、未计价请求统一起来。
- 账号质量被简化成 Top credentials，无法解释调度质量、错误率、余额、模型覆盖和 selection pressure。
- dashboard/usage 聚合查询的性能故障域没有在 UI/API 合同里和主业务隔离。

## 0.2 复现

当前问题可以通过以下方式稳定暴露：

1. 切换 dashboard 顶部时间窗口，观察实时卡片、趋势图、费用卡片、错误分布和维度排行是否全部按同一语义变化。
2. 对比 `ui/` 新 UI 和 `admin-ui/` 旧 UI，在相同时间窗口下检查 summary、top、cost、credits、账号质量、外部池成本是否同口径。
3. 点击“查询启用积分”或“查询选中积分”，观察顶部积分汇总、账号卡片、详情弹窗、dashboard 账号质量是否同步刷新。
4. 在大 usage 表或慢 PgSQL 查询下访问 dashboard，观察是否出现整页失败、`error returned from database` 或 `usage dashboard 查询繁忙`，以及主业务接口是否被影响。

## 0.3 方案

本文件选定的方案是先冻结 dashboard 产品合同，再分阶段实现：

- 按时间语义拆分指标：Realtime、Inventory、Selected window、Trend range、Lifetime、Observability health。
- Overview 只做轻量健康入口；Traffic、Cost、Accounts Quality、Errors/Diagnostics 分页或分区承载重查询。
- API 拆成独立区块接口，响应必须带 `scope/freshness`，局部失败不能拖垮整页。
- usage/dashboard 使用独立 pool/gate/cache，查询慢只影响对应区块，不能影响模型请求主路径。
- 新旧 UI 用同一能力矩阵对齐；旧 UI 若不完整实现，必须明确降级边界。
- 积分刷新走统一 cache/refetch contract，不再只更新某一个局部来源。

## 0.4 残余风险与回滚

残余风险：

- 当前文档是 dashboard 产品合同，不代表页面已经完整重构完成。
- 当前工作树的局部 dashboard 拆接口和字段补充只能降低单点加载风险，不能替代完整信息架构重做。
- 生产大数据量下仍需要验证独立 usage pool、query gate、stale/partial cache 是否足够。

回滚边界：

- 如果 dashboard 改造导致页面不可用，应回滚 UI/API dashboard 改动或关闭新 dashboard 入口，但不能回滚主业务 runtime/storage 解耦修复。
- 如果重查询仍拖慢主业务，应优先关闭 dashboard 重区块或降低并发 gate，而不是把统计查询重新并入主业务热路径。

## 1. 用户可见问题

### 1.1 时间窗口切换语义混乱

页面顶部有一个时间窗口切换器，看起来像控制整个 dashboard，但实际不是。

当前新 UI 源码证据：

- `ui/src/features/overview/overview-page.tsx:833-866`
  - `windowsQuery` 用于所有窗口摘要。
  - `usageSummaryQuery` 是实时 summary，不带 `windowKey`。
  - `seriesQuery` 只带 `timezone`，不带 `windowKey`。
  - `topQuery / breakdownQuery / externalPoolBillingQuery` 才带 `effectiveWindowKey`。
- `ui/src/features/overview/overview-page.tsx:1000-1024`
  - `实时` Tab 里同时展示了实时负载、账号池和“当前窗口摘要”。
  - 这会让用户误以为“实时”也被选中的历史窗口控制。
- `ui/src/features/overview/overview-page.tsx:1057-1063`
  - 趋势图固定使用 `hourly24h / daily7d`，不随 `today / yesterday / last7d / last30d / thisMonth` 切换。
- `ui/src/features/overview/overview-page.tsx:1074-1096`
  - 费用卡片、账号质量、外部池计费使用当前窗口。
- `ui/src/features/overview/overview-page.tsx:1099-1109`
  - 异常卡片、Top errors、breakdown 使用当前窗口。

这不是一个小 bug，而是信息架构问题。正确做法是把指标按时间语义分区，并在 UI 上明确展示“这个切换器影响哪些区域”。

### 1.2 新 UI 和旧 UI 能力不一致

旧 UI `admin-ui/src/components/usage-dashboard-panel.tsx` 和新 UI `ui/src/features/overview/overview-page.tsx` 不是同一套信息架构。

当前旧 UI 源码证据：

- `admin-ui/src/components/usage-dashboard-panel.tsx:636-662`
  - 同样有窗口选择、series、top、breakdown、external-pool-billing。
- `admin-ui/src/components/usage-dashboard-panel.tsx:721-757`
  - 旧 UI 是一个长面板，所有指标基本一次性展示。
  - 维度排行在页面底部保留，外部池计费也在同一页。

当前新 UI 源码证据：

- `ui/src/features/overview/overview-page.tsx:992-1110`
  - 新 UI 改成四个 Tab，维度排行只在 `流量` Tab，账号质量只在 `费用` Tab，错误分布只在 `异常` Tab。

问题不是哪个 UI 好看，而是两套 UI 没有共同能力矩阵：

- 同名指标是否同口径？
- 维度排行要不要存在？如果存在，它属于“流量排行”还是“诊断 drilldown”？
- 外部池成本只在 Usage 页有价值，还是总览也应该显示？
- 账号质量是费用页的一部分，还是独立账号健康页？
- 旧 UI 是否保留完整运维能力，还是只保留兼容入口？

这些没有定义清楚，导致新老页面“看起来都叫 dashboard”，但实际能力不同。

### 1.3 费用视角不完整

用户关心的费用不只有外部池。

必须同时展示并区分：

- 本地估算费用：本系统按价格表、usage、整形逻辑估算出来的费用。
- 本地原始/实际费用：上游原始 usage 或原始成本口径，用于和本地估算对账。
- Kiro metering / 积分：上游 meteringEvent 累积积分，用于判断真实 Kiro 消耗。
- 外部池 raw cost：外部池上游真实 cost。
- 外部池 shaped/reported/uplifted/billable：整形、返回下游、放大计费、最终可计费口径。
- 未计价请求：价格表没匹配、模型别名没匹配、usage 缺失、错误请求不应计费等导致的计价空洞。

当前新 UI 只是在费用 Tab 里补了几张卡片，仍没有把“成本关系”讲清楚。用户需要看到的是：

```text
请求收入/返回给下游 usage
  ├─ 本地账号成本/积分消耗
  │    ├─ estimatedCostUsd
  │    ├─ originalCostUsd
  │    └─ kiroMeteringUsage
  └─ 外部池成本/利润
       ├─ rawCostUsd
       ├─ shapedCostUsd
       ├─ upliftedCostUsd
       ├─ billableCostUsd
       └─ profitUsd = uplifted/billable - raw
```

否则 dashboard 不能解释“为什么费用是 0”、“为什么积分消耗了但本地计费没记”、“为什么外部池有成本但下游 usage 正常/异常”。

### 1.4 账号质量不是 Top credentials

当前新 UI 的“本地账号质量”主要基于窗口化 Top credentials + 累计 `credentialUsageSummary`：

- `ui/src/features/overview/overview-page.tsx:1082-1085`
- `ui/src/features/overview/overview-page.tsx:718-797`

这只能回答“哪个账号请求多、费用多”，不能回答“哪个账号质量差、是否拖垮调度”。

真正的账号质量至少需要合并三类数据：

1. 当前运行态：
   - enabled/disabled
   - available/cooling/rate_limited
   - inFlight / maxConcurrent
   - RPM 当前消耗 / RPM 限额
   - oldest in-flight age
   - scheduler score / selection pressure
   - recent error rate EWMA
   - latency EWMA
   - last error kind/reason/at
2. 当前窗口用量：
   - requests / success / error
   - error rate
   - latency p50/p95 或 average/p95
   - input/output/cache tokens
   - estimated/original cost
   - Kiro metering usage
   - priced/unpriced
3. 账号库存/余额：
   - email / masked key
   - provider / region / endpoint / proxy
   - subscription title
   - credit remaining / limit
   - overage status
   - supported models
   - last checked at

只用 Top credentials 无法判断“为什么两个账号限制 5 并发/10 RPM，但队列里一直 api_error”、“为什么账号看起来正常但调度能力掉了”、“为什么页面看到账号可用但请求全去外部池”。

### 1.5 错误分析不够面向根因

当前 dashboard 的错误区域主要有：

- error rate
- error requests
- Top errors
- status breakdown
- usage source breakdown

这些还不够。生产问题里真正需要聚类的是：

- request endpoint：`/v1/messages`、`/ha/v1/messages`、`/v1/chat/completions` 等。
- route kind/subtype：local credential、external pool、fallback after local attempts、local_error_no_fallback。
- stage/phase：request parse、selection、dispatch queue、local_account、upstream、streaming、usage write。
- client status/public status/upstream status：200/400/429/502/503 之间的差异。
- error class/reason：api_protocol_error、websearch_mcp_scheduler_unavailable、thinking_signature_retry_failed、sampled_request_rejection 等。
- model requested/resolved/upstream：模型别名是否导致价格、上游调用、usage 统计分裂。
- stream vs non-stream：流解析问题和非流 JSON usage 问题完全不同。
- fallback/retry path：首输出前重试、外部池 fallback、scheduler degraded fallback、no fallback。

如果错误只按字符串 Top 展示，用户不能区分“真实上游错误”和“本系统解析/调度错误”。

### 1.6 积分查询刷新依赖不统一

用户明确观察到：新版“查询积分”只更新顶部统计，下面卡片/明细没有按预期同步；旧版行为又不一样。

当前新 UI 源码证据：

- `ui/src/features/credentials/credentials-page.tsx:345-353`
  - `invalidate()` 会 invalidate 多个 query key。
- `ui/src/features/credentials/credentials-page.tsx:358-374`
  - 批量返回的账号 info 只写入当前可见卡片的 `balanceMap`。
- `ui/src/features/credentials/credentials-page.tsx:436-464`
  - “查询启用积分”对所有启用账号批量 refresh，写当前可见卡片，然后 invalidate，再 refetch `creditSummary`。
- `ui/src/features/credentials/credentials-page.tsx:479-502`
  - “查询选中积分”写选中账号对应的卡片，然后 invalidate，再 refetch `creditSummary`。

问题是积分数据在前端存在至少三套来源：

1. `credential-credit-summary`：顶部汇总卡片。
2. `balanceMap`：当前页面临时覆盖的卡片详情。
3. `credential-account-info / credentials / credentials-page`：列表、明细、分页和弹窗的数据源。

刷新按钮没有一个明确的“积分刷新后必须更新哪些数据面”的契约，所以容易出现：

- 顶部汇总更新了，当前页卡片没更新。
- 当前页卡片更新了，跨页不更新。
- 详情弹窗打开时和卡片不一致。
- dashboard 里的账号质量仍读取旧缓存。

## 2. Dashboard 必须回答的问题

Dashboard 的设计应该从运维问题反推，而不是从已有字段反推。

### 2.1 系统现在是否健康？

必须一眼看到：

- 服务是否有流量：RPM、TPM、成功 RPM、错误 RPM。
- 现在是否拥塞：global in-flight、queue depth、queue age、最老请求年龄。
- 本地账号池是否可调度：total、enabled、available、cooling、rate-limited、disabled、runtime stale。
- 外部池是否在接流量：外部池请求占比、fallback 占比、外部池错误。
- 调度是否退化：Redis scheduler degraded、local_all_disabled、no_available_credentials、capacity_exhausted。
- 统计系统是否健康：usage writer queue、dropped usage、rollup lag、dashboard cache freshness。

这类数据是实时/近实时，不应受“今天/昨天/最近 7 天”控制。

### 2.2 过去选定时间内用了多少？

必须按选中窗口展示：

- requests / success / errors / error rate
- stream vs non-stream
- input/output/cache read/cache creation tokens
- high cache requests
- average duration / p95 duration / first token latency
- estimated/original cost
- Kiro metering usage
- priced/unpriced
- local/external route split
- cache source / usage source

这类数据必须受当前窗口控制。

### 2.3 流量趋势是否异常？

趋势不是“当前窗口摘要”的替代。趋势需要独立的 range + grain：

- 最近 15m：按分钟，用于看突发。
- 最近 1h：按分钟或 5 分钟，用于看短时放大。
- 最近 24h：按小时。
- 最近 7d/30d：按天。
- 自定义窗口：按自动粒度。

趋势图需要展示：

- requests/success/errors
- RPM 峰值或每 bucket 请求数
- latency p95/TTFB p95
- local/external split
- cost/credits
- fallback/error spike 标记

趋势图不应被 `today/yesterday` 这种 summary window 隐式控制，除非 UI 明确把趋势范围和 summary window 绑定。

### 2.4 钱花在哪里？有没有计费漏洞？

必须拆成本地、外部池、返回下游三个维度：

- 本地账号：
  - estimated cost
  - original/upstream cost
  - Kiro metering usage
  - credit remaining / limit
  - cost by credential / model / endpoint / API key
- 外部池：
  - raw cost
  - shaped cost
  - uplifted cost
  - reported/billable cost
  - profit
  - unpriced / cost floor
- 下游返回：
  - 返回给下游的 usage 是否符合配置整形。
  - 是否存在成功请求但内部计费为 0。
  - 是否存在错误请求仍记 cost 或消耗积分但 usage 失败。

### 2.5 哪些账号质量差？

账号质量不是费用页里的附属表，而应该有独立视角。

至少支持：

- 按当前窗口排名：请求、错误率、p95、费用、Kiro 积分、未计价。
- 按实时运行态排名：in-flight、oldest lease age、selection pressure、recent error EWMA、cooldown/rate-limit。
- 按库存/余额排名：remaining credits、subscription、overage、last checked、supported model coverage。
- 过滤：启用/禁用、provider、region、endpoint、proxy、model support、错误账号。

### 2.6 当前错误是哪里来的？

错误页应优先支持根因定位，而不是只显示“错误数量”：

- Top error class / reason。
- Top failing model / credential / external pool / endpoint / API key。
- route kind/subtype。
- phase/stage。
- upstream status vs public status。
- stream vs non-stream。
- retry/fallback path。
- 最近 N 条示例 request id，可跳转 usage detail。

### 2.7 统计系统有没有影响主业务？

这是当前项目的硬要求。Dashboard 还必须暴露 observability 自身健康：

- usage writer queue depth / dropped / backpressure。
- Redis usage writer errors / scheduler Redis errors 分离。
- PgSQL usage pool in-use / wait / timeout。
- rollup last successful timestamp / lag。
- dashboard query gate in-use / rejected / average wait。
- cache hit/miss/stale age。

这些指标不一定第一版全部实现，但信息架构必须预留，不要继续把统计系统健康混进普通 usage 成本卡片里。

## 3. 时间语义合同

所有 dashboard 指标必须属于下面一种时间语义。UI、API、测试都按这个表验收。

| 类型 | 是否受主时间窗口控制 | 示例 | UI 标记 | 正确行为 |
| --- | --- | --- | --- | --- |
| Realtime / now | 否 | 最近 60 秒 RPM/TPM、当前 in-flight、排队、账号 available | `实时` / `最近 60s` / `当前` | 切换 today/yesterday 不应变化；自动刷新可变化 |
| Point-in-time inventory | 否 | 账号总数、启用/禁用、模型限制、proxy、余额快照 | `当前库存` / `上次查询时间` | 时间窗口切换不应变化；查询积分/刷新账号信息后变化 |
| Selected window | 是 | 今天/昨天/最近 7 天请求数、错误率、费用、积分、Top errors | `当前窗口` + from/to | 切换窗口必须变化 |
| Trend range | 独立控制 | 最近 15m/1h/24h/7d 趋势 | `趋势范围` + grain | 用趋势自己的 range 控制，不跟 summary window 暗中混用 |
| Lifetime / cumulative | 否，除非显式选择 lifetime | 累计费用、累计积分、账号历史总成本 | `累计` / `Lifetime` | 不随 today/yesterday 变化 |
| Observability health | 通常否，或固定最近 5m | usage writer dropped、rollup lag、dashboard query gate | `统计健康` | 不能和业务用量混为一谈 |

强制规则：

- 页面上任何数字都必须能从标题/副标题看出时间语义。
- 一个全局时间切换器只能控制 `Selected window` 区域。
- 如果某个区域不受时间窗口控制，必须标注“实时/当前/累计/趋势范围”。
- 如果趋势需要随窗口变化，必须把趋势接口改成 `range/windowKey + grain` 并在 UI 上显式说明。

## 4. 推荐信息架构

### 4.1 第一层：Overview，总览只回答“现在要不要处理”

Overview 首屏应该轻量，不做重 SQL，不加载大排行。

首屏内容：

1. 当前健康灯：
   - 正常 / 警告 / 故障
   - 故障原因摘要：调度退化、账号不可用、错误率高、统计延迟、外部池错误。
2. 实时负载：
   - RPM、错误 RPM、TPM、in-flight、queue depth、queue age。
3. 本地账号池：
   - total/enabled/available/cooling/disabled/rate-limited。
   - global in-flight / max。
   - scheduler degraded 状态。
4. 当前窗口摘要：
   - 请求、错误率、P95、费用、Kiro 积分、未计价。
   - 明确标注“受时间窗口控制”。
5. 统计健康：
   - dashboard cache freshness。
   - rollup lag。
   - usage writer dropped/backpressure。

Overview 不应该承载完整排行榜、完整外部池成本表、完整账号质量表。它只给入口和告警。

### 4.2 第二层：Traffic，流量与性能

Traffic 页面/Tab 展示：

- 选定窗口 summary。
- 独立趋势范围选择。
- requests/success/errors。
- stream/non-stream。
- model/endpoint/API key/client 维度排行。
- latency/TTFB 趋势。
- cache read/write/creation。
- local/external route split。

维度排行如果保留，应明确叫“流量排行”，不是泛化“维度排行”。

### 4.3 第二层：Cost，费用与计费

Cost 页面/Tab 展示：

- 当前窗口总览：
  - estimated cost
  - original/upstream cost
  - Kiro metering usage
  - priced/unpriced
  - successful-but-zero-cost
  - errored-but-costed / credited
- 本地账号成本：
  - by credential、model、endpoint。
  - cost、credits、unpriced、success/error。
- 外部池成本：
  - raw/shaped/uplifted/reported/billable/profit。
  - by pool。
- 计价异常：
  - model pricing unmatched。
  - alias mismatch。
  - usage missing。
  - cost floor applied。

### 4.4 第二层：Accounts Quality，账号质量

Accounts Quality 页面/Tab 展示：

- 当前运行态排行榜：
  - in-flight、oldest lease、selection pressure、recent error EWMA、latency EWMA。
- 当前窗口质量：
  - requests、success/error、error rate、P95、cost、credits。
- 库存和余额：
  - credit remaining、limit、subscription、overage、last checked。
- 操作入口：
  - 查询启用积分。
  - 查询选中积分。
  - 跳转账号管理。

账号质量可以复用 Credentials 页数据，但不能只靠 Usage Top credentials。

### 4.5 第二层：Errors / Diagnostics，异常诊断

Errors 页面/Tab 展示：

- 错误总览：error rate、error RPM、最近 spike。
- 根因分组：
  - error class/reason。
  - route kind/subtype。
  - stage/phase。
  - upstream status/public status。
  - model。
  - credential/external pool。
  - endpoint。
  - stream/non-stream。
- 示例请求：
  - 最近 request id。
  - 可跳转 usage detail。
  - 显示是否已脱敏、是否有 body fingerprint。

### 4.6 Usage 明细页的定位

Usage 明细页不应该和 Dashboard 抢职责。

Usage 明细页负责：

- 单条请求明细。
- 条件筛选。
- request id 调试。
- 导出。
- 清理。

Dashboard 负责聚合判断和引导跳转。

## 5. API 拆分合同

现有 API：

- `/api/admin/usage-summary`
- `/api/admin/usage-dashboard/windows`
- `/api/admin/usage-dashboard/series`
- `/api/admin/usage-dashboard/top`
- `/api/admin/usage-dashboard/breakdown`
- `/api/admin/usage-dashboard/external-pool-billing`
- `/api/admin/credentials/summary`
- `/api/admin/credentials/credit-summary`

这些可以兼容保留，但新 dashboard 不应直接依赖“接口名字像 dashboard 就随便混用”。

推荐目标 API 形态：

```text
GET /api/admin/dashboard/health-now
GET /api/admin/dashboard/window-summary?windowKey=&timezone=
GET /api/admin/dashboard/series?range=&grain=&timezone=&metrics=
GET /api/admin/dashboard/top?windowKey=&timezone=&dimension=&limit=
GET /api/admin/dashboard/cost?windowKey=&timezone=&groupBy=
GET /api/admin/dashboard/account-quality?windowKey=&timezone=&statusScope=
GET /api/admin/dashboard/errors?windowKey=&timezone=&groupBy=&limit=
GET /api/admin/dashboard/observability-health
```

每个响应都必须包含 scope/freshness 元信息：

```json
{
  "generatedAt": "2026-07-27T00:00:00Z",
  "scope": {
    "type": "selected_window",
    "timezone": "Asia/Shanghai",
    "windowKey": "today",
    "from": "2026-07-27T00:00:00+08:00",
    "to": "2026-07-27T12:00:00+08:00"
  },
  "freshness": {
    "source": "rollup",
    "stale": false,
    "staleAgeMs": 0,
    "partial": false,
    "omittedSections": []
  },
  "data": {}
}
```

这个元信息用于解决两个问题：

- 用户知道数字是不是当前窗口、实时、累计或缓存。
- 某个重查询失败时，页面可以展示 stale cache/partial data，而不是整页失败。

## 6. 性能和故障域合同

Dashboard/Usage 统计不能影响主业务。

### 6.1 业务请求路径不能等待 dashboard

禁止：

- 模型请求 handler 同步等待 dashboard/usage 聚合。
- usage writer 队列满后在主请求路径同步写库。
- 统计 Redis 慢导致 scheduler Redis 热路径被阻塞。
- dashboard 查询占用主业务 PgSQL pool。

要求：

- 主业务请求只做 bounded enqueue / best-effort telemetry。
- usage/dashboard 使用独立 PgSQL usage pool。
- usage/statistics/admin cache 使用独立 observability Redis；如果未配置，宁可降级到 PgSQL/进程内缓存，也不能回落占用 scheduler Redis。
- dashboard 查询有独立并发闸门。
- 慢查询允许继续加载，但只阻塞对应区块，不阻塞整页和主业务。

### 6.2 数据多时不能“一等就几秒报错”

用户明确要求：数据多查很久可以接受，但不能影响主业务，也不能整个页面直接失败。

目标行为：

- 首屏实时健康必须快，失败也局部显示。
- 重聚合区块可以显示骨架屏/加载中。
- 查询超过短阈值时 UI 显示“仍在查询”，不直接让整页失败。
- 服务端可以对重查询设置较长 statement timeout，但必须有并发闸门和独立 pool。
- 如果有 stale cache，优先展示 stale 数据并标注生成时间。

### 6.3 Dashboard 自身健康也要展示

如果 dashboard 查不出数据，页面必须能说明是：

- PgSQL usage pool 等待。
- statement timeout。
- rollup lag。
- Redis observability cache miss/timeout。
- query gate saturated。
- schema/migration 缺列。

不能只显示 `error returned from database` 或 `usage dashboard 查询繁忙，请稍后重试`。

## 7. 积分查询和账号卡片刷新合同

“查询启用积分”不是一个简单刷新顶部汇总的按钮。

### 7.1 按钮行为

按钮名称：

- `查询启用积分`：查询所有 enabled 账号的订阅/积分信息。
- `查询选中积分`：查询用户选中的账号。
- 单账号卡片：`查询信息/积分`。

默认行为：

- 查询订阅/积分，不发模型请求。
- 需要验活时使用单独动作，不和积分查询混在一起。

### 7.2 刷新后必须同步的数据面

一次积分刷新成功后必须更新：

1. 顶部积分汇总卡片：
   - enabled credit remaining / limit
   - enabled recorded cost
   - last checked at
2. 当前可见账号卡片：
   - subscription title
   - credit remaining / limit
   - current usage
   - overage status
   - last checked at
3. 账号详情/积分明细弹窗：
   - 如果弹窗打开，必须立即刷新。
4. 账号列表 query cache：
   - `credentials`
   - `credentials-page`
   - `credential-account-info`
   - `credential-credit-summary`
5. Dashboard 账号质量：
   - 如果账号质量展示余额/订阅/积分快照，必须 invalidate 相关 query。

### 7.3 前端实现原则

- 不要让 `balanceMap` 成为长期事实源，它只能作为“当前页面刚查询完的覆盖缓存”。
- 后端 `refreshCredentialInfo` 返回的 `items[].info` 应该写入 query cache 或触发统一 refetch。
- 跨页数据以服务端持久化 accountInfo 为准。
- 顶部卡片、卡片列表、弹窗、dashboard 账号质量都必须从同一个刷新 contract 派生。

## 8. 新旧 UI 统一原则

当前项目有两套 UI：

- 新 UI：`ui/src/...`
- 旧 UI：`admin-ui/src/...`

不能继续让两套 UI 自由漂移。

### 8.1 统一策略

推荐策略：

- 新 UI 作为 canonical dashboard。
- 旧 UI 保留兼容入口，但核心指标、时间语义、费用口径必须和新 UI 一致。
- 如果旧 UI 不做完整重构，必须在旧 UI 文案中明确“这是兼容总览，完整运行态请看新 UI dashboard”。

### 8.2 能力矩阵

| 能力 | 新 UI 当前状态 | 旧 UI 当前状态 | 目标 |
| --- | --- | --- | --- |
| 时间窗口 summary | 有 | 有 | 口径一致，标注当前窗口 |
| 实时负载 | 有，来自 usage-summary | 缺或不突出 | 两套一致，实时不受窗口控制 |
| 账号池实时状态 | 有，来自 credentials summary | 旧 dashboard 缺，旧首页有部分 | 两套一致或旧 UI 明确降级 |
| 趋势 | 有，固定 24h/7d | 有，固定 24h/7d | 独立趋势范围/粒度 |
| Top 模型/账号/入口/错误 | 有，在流量 Tab | 有，在底部 | 名称和口径一致 |
| 本地账号质量 | 有，但不完整 | 缺 | 独立账号质量视角 |
| 外部池成本 | 有 | 有 | 费用页完整展示，口径一致 |
| 本地成本/Kiro积分 | 有局部卡片 | 有局部卡片 | 成本关系图和账号维度齐全 |
| 错误诊断 | 有摘要 | 有摘要 | 增加 phase/route/status/reason/grouping |
| 统计系统健康 | 基本缺 | 缺 | 必须新增 |
| 积分查询刷新 | 新旧行为不一致 | 旧 UI 另有逻辑 | 统一刷新 contract |

## 9. 当前工作树改动的评价

当前工作树里已经做的点不是完全没价值，但还不能满足目标。

保留价值：

- Top 查询支持 `windowKey` 是对的，维度排行必须能随窗口变化。
- 分接口加载比原来的大接口更好。
- Kiro metering/积分进入 summary/series/top 是必要的。
- 外部池计费拆分应该保留。
- 新 UI 已经从单页堆叠收敛为 5 个明确区块：实时、流量、费用、账号质量、异常诊断。
- 新 UI 已补 `usage-writer-stats` 统计健康入口，避免 dashboard 完全看不到观测持久化状态。
- 旧 UI 也至少补了统计健康的最小对齐，不再只显示费用/排行。
- `block_on_usage_runtime` 已改为在 usage 专用 runtime/线程执行 usage/dashboard Future，避免由 HTTP Tokio worker 直接驱动统计查询。

必须重做/补齐：

- 页面信息架构需要按本文件重新规划，不能只是简单加几个 Tab。
- 时间窗口语义必须在 UI 和 API 层明确。
- 趋势范围要独立，或明确绑定当前窗口，不能现在这样隐式固定。
- 账号质量要合并运行态、窗口质量、余额库存，而不是 Top credentials 表。
- 费用页要把本地、外部池、下游返回 usage 的关系讲清楚。
- 错误页要面向根因聚类。
- 积分刷新必须统一 query/cache contract。
- Dashboard 查询失败要返回可解释原因和 partial/stale state。
- 新旧 UI 要有明确 parity/兼容策略。
- 后端 dashboard response 仍需补 scope/freshness 元信息，当前前端已经按“区块语义”开始收敛，但 API 合同还未完全显式化。

## 10. 实施计划

### Phase A：数据合同和 UI 信息架构

交付：

- 明确所有 dashboard 指标的时间语义。
- 定义 canonical dashboard 页面结构。
- 定义旧 UI 兼容策略。
- 定义 API response scope/freshness 元信息。

验收：

- 文档中每个卡片都能说清楚数据源、时间范围、是否受窗口控制、刷新触发。

### Phase B：接口拆分与后端补字段

交付：

- 保留现有接口兼容。
- 新增或调整 dashboard API，使每类数据有明确 scope。
- account quality 接口合并 runtime + window usage + account info。
- errors 接口支持 phase/route/status/reason 分组。
- observability health 接口暴露 usage writer/dashboard query/rollup/cache 状态。

验收：

- 每个接口可单独失败，不影响其它 dashboard 区块。
- 每个接口响应有 generatedAt/scope/freshness。
- 重查询只用 usage pool/observability redis，不占主业务热路径。

### Phase C：新 UI 重构

交付：

- Overview 只保留轻量健康和入口。
- Traffic/Cost/Accounts Quality/Errors 分区清晰。
- 时间窗口只控制 windowed analytics。
- 趋势有独立范围控制。
- 每个卡片标题/副标题标注时间语义。
- skeleton 和 partial error 只影响当前区块。

验收：

- 切换时间窗口时，所有 windowed 区块变化；realtime/current/lifetime 区块不变化且标注清楚。
- 切换趋势范围时，只有趋势区变化。
- 错误接口失败时，Overview 仍能看到实时负载和账号池。

### Phase D：旧 UI 对齐或降级

交付：

- 旧 UI 使用同一数据口径。
- 不完整的功能明确降级提示。
- 能力矩阵不再出现“同名但不同口径”的功能。

验收：

- 新旧 UI 同一窗口下 summary、top、cost、credits 数字一致。

### Phase E：积分刷新统一

交付：

- 查询启用积分、查询选中积分、单账号查询使用同一刷新 contract。
- 顶部汇总、当前页卡片、详情弹窗、账号质量、query cache 同步。

验收：

- 刷新后不出现顶部变了、卡片不变。
- 跨页回到之前页面显示后端持久化后的最新数据。
- dashboard account quality 若展示余额，也能看到刷新后的值。

### Phase F：性能/异常验证

交付：

- 大数据 dashboard 查询不会拖垮主业务。
- 查询慢时页面显示局部 loading/stale，不整页失败。
- 统计系统故障时主业务接口仍正常。

验收：

- 模拟 usage PgSQL 慢查询：主业务 `/v1/messages` 不等待 dashboard。
- 模拟 observability Redis 慢/不可用：scheduler Redis 不受影响。
- dashboard query gate 饱和时只影响 dashboard 区块。
- usage writer 队列满时主业务只丢统计/记录 dropped，不同步写库。

## 11. 测试计划

### 11.1 数据正确性

构造固定 usage 数据集：

- today/yesterday/last7d/last30d/thisMonth/lifetime 都有不同请求数。
- 本地账号和外部池都有成功/失败。
- priced/unpriced 都存在。
- Kiro metering usage 非 0。
- 不同模型/endpoint/credential/error reason 分布不同。

验收：

- 切换窗口后，summary/top/breakdown/cost/account-window-quality 全部变化。
- realtime/account inventory/lifetime 不随窗口变化。
- series 按独立 range/grain 变化。

### 11.2 UI 行为

验收：

- Overview 首屏只需要轻量接口。
- Traffic 加载 trend/top。
- Cost 加载 cost/account quality/external billing。
- Errors 加载 errors/breakdown/examples。
- 局部接口失败只显示局部错误。
- loading 状态不是全页卡死。

### 11.3 积分刷新

验收：

- 查询启用积分后，顶部卡、可见账号卡、详情弹窗、account info cache 一致。
- 查询选中积分只更新选中账号，但汇总会重新计算。
- 单账号查询只更新该账号和汇总。
- 默认不验活，不发模型请求。

### 11.4 性能隔离

验收：

- dashboard 慢查询期间，主业务请求延迟不明显放大。
- usage writer 队列满不阻塞模型响应。
- Redis observability 操作失败不影响 scheduler Redis。
- dashboard 返回 stale/partial/freshness 信息。

### 11.5 新旧 UI parity

验收：

- 同一窗口下，新旧 UI 的 summary/top/cost/credits 数字一致。
- 如果旧 UI 不实现某个新能力，页面明确标注并给出跳转。

## 12. 当前证据

### 12.1 源码证据

- 新 UI dashboard 查询和 Tab：`ui/src/features/overview/overview-page.tsx:831-1110`
- 旧 UI dashboard 查询和长面板：`admin-ui/src/components/usage-dashboard-panel.tsx:636-770`
- 新 UI usage hook query key：`ui/src/hooks/use-usage.ts:48-96`
- 新 UI 积分刷新逻辑：`ui/src/features/credentials/credentials-page.tsx:345-510`

### 12.2 本地 API 采样

本地服务：`127.0.0.1:9022`

采样结果：

| Endpoint | HTTP | 耗时 | 返回体 | 顶层字段 |
| --- | ---: | ---: | ---: | --- |
| `/api/admin/usage-summary` | 200 | 0.040s | 3248B | `totalRequests,successRequests,errorRequests,...` |
| `/api/admin/usage-dashboard/windows` | 200 | 0.022s | 6473B | `generatedAt,timezone,windows` |
| `/api/admin/usage-dashboard/series` | 200 | 0.028s | 9594B | `generatedAt,timezone,series` |
| `/api/admin/usage-dashboard/top?windowKey=today` | 200 | 0.030s | 134B | `generatedAt,top` |
| `/api/admin/usage-dashboard/breakdown?windowKey=today` | 200 | 0.016s | 144B | `generatedAt,timezone,windowKey,statusBreakdown,usageSourceBreakdown` |
| `/api/admin/usage-dashboard/external-pool-billing?windowKey=today` | 200 | 0.012s | 128B | `generatedAt,timezone,windowKey,externalPoolBillingByPool` |
| `/api/admin/credentials/summary` | 200 | 0.002s | 222B | `total,available,disabled,currentId,...` |
| `/api/admin/credentials/credit-summary` | 200 | 0.009s | 556B | `totalCredentials,enabledCredentials,...` |

解读：

- 这些端点天然分属于不同时间语义，不能在 UI 中被一个总切换器隐式混合。
- 当前本地样本数据较小，不能证明生产大数据量性能安全。
- 当前采样只证明接口可返回，不证明 dashboard 设计正确。

## 13. 不应继续犯的错误

- 不要把“拆接口”当成 dashboard 重构完成。
- 不要把“加 Tab”当成信息架构完成。
- 不要让一个时间切换器隐式控制一部分卡片、另一部分不控制。
- 不要把实时状态、历史窗口、累计成本、账号库存混在一个卡片组里。
- 不要只展示外部池费用，忽略本地账号成本和 Kiro 积分。
- 不要把账号质量降级成 Top credentials。
- 不要让新旧 UI 同名能力不同口径。
- 不要用“查询繁忙/数据库错误”掩盖 dashboard 自身健康问题。
- 不要让统计/usage/dashboard 查询影响调度和模型调用主链路。
