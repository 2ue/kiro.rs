# 备用池成本保底与成本差值分析方案

## 背景

当前系统已经支持备用池/外部号池。当本地凭据不可调度、显式直连策略命中，或者本地调用失败后满足 fallback 条件时，请求会转发到外部池。

外部池的请求和返回主体原则上需要透传，只有一个例外：如果外部池配置了 `usageProjectionMode=current_path_policy`，系统会按当前入口路径对应的 `reportedUsage` 策略调整返回体或 SSE 事件里的 `usage` 字段，让下游看到的缓存上报形态更接近当前服务自己的缓存特征。

问题在于，`usage` 整形会改变下游看到的输入、输出、缓存读取、缓存创建 token 组合。如果系统后续按整形后的 `usage` 计算费用，可能低于外部池原始返回对应的渠道成本。长期看，这会造成备用池流量的成本损失，而且页面上无法直接看到“渠道原始成本”和“最终计费成本”的差值。

另一个现状缺口是：外部池成功请求目前主要按 request estimate 记录，无法稳定记录外部池原始 usage、整形后 usage、成本保底是否触发，也无法在清理使用记录明细后保留这类统计。

## 需求

1. 备用池响应保持原有行为：
   - `pass_through` 时完全透传 usage。
   - `current_path_policy` 时继续按当前请求路径对应策略整形 usage。
   - 不为了计费保底而额外篡改返回给下游的输入、输出、缓存读取、缓存创建 token。

2. 系统内部需要记录三套成本：
   - `rawCost`：外部池原始返回 usage 按系统价格表计算出的渠道参考成本。
   - `reportedCost`：最终返回给下游的 usage 按系统价格表计算出的展示/计费参考成本。
   - `billableCost`：系统最终用于统计和账务的费用。

3. `billableCost` 需要满足成本保底：
   - 当 raw/reported 两套 usage 都能按当前价格表计价时，`billableCost = max(rawCost, reportedCost)`。
   - 当 usage 或价格缺失时，不伪造成本，标记为未计价或部分缺失，避免把错误数据当成可靠账务数据。

4. 管理后台需要能分析备用池成本差值：
   - 在 usage/dashboard 总览中展示备用池请求数、渠道成本、整形展示成本、最终计费成本、保底补差、触发保底次数。
   - 在使用记录详情中展示单条外部池请求的 raw/reported/billable 成本和 usage 快照。
   - 查询不能依赖扫描所有 `usage_records` 明细，否则记录多时页面会慢。

5. 清理使用记录明细后，顶部/总览统计仍保留：
   - 外部池成本统计必须进入 rollup 表。
   - 使用记录软删除或批量清理不应回滚已累计的成本统计。

6. 实现后必须验证：
   - 单元测试覆盖 pass-through、整形后更便宜、整形后更贵、SSE usage 捕获。
   - PostgreSQL 集成测试模拟几百到上千条备用池调用数据，验证 rollup 聚合、清理明细后统计仍存在。
   - 前端两个管理后台都能 build。

## 反思与取舍

### 反思 1：不能用“放大返回 token”来解决亏损

一种直觉方案是：如果整形后费用低于渠道成本，就修改返回体里的 `input_tokens`、`output_tokens`、`cache_read_input_tokens` 或 `cache_creation_input_tokens`，把 reportedCost 补到 rawCost 以上。

这个方案不采用，原因是：

1. 返回 usage 是协议层信息，随意补 token 会让下游看到的 token 与真实外部池返回不一致，且很难解释。
2. 缓存读取/缓存创建 token 有语义，不能只为了钱随意挪动，否则会破坏“缓存特征大致一致”的目标。
3. 外部池可能本身已经返回 cache read/write，也可能完全不返回缓存字段。强行补齐会引入更多边界分支。
4. 后续排查单条请求时，无法区分“外部池真实 usage”“系统展示 usage”“为了保底临时补出来的 usage”。

最终方案将响应整形和账务保底解耦：下游响应仍由 `usageProjectionMode` 决定，内部账务通过 `billableCost` 保底。

### 反思 2：不能只在使用记录 JSON 里存，不进 rollup

如果只把 raw/reported/billable 放进 `usage_records.data`，页面要统计“最近 24 小时/7 天备用池成本差值”就必须扫明细 JSON。记录量大以后这会让 usage 页面慢，也会让明细清理后统计丢失。

因此实现必须把外部池成本字段加入 `usage_rollup_totals` 和 `usage_rollup_time_buckets`，并从 `UsageRecord` 增量写入。明细清理只影响列表，不影响已经累计的统计。

### 反思 3：成本保底不等于盈利策略

`billableCost = max(rawCost, reportedCost)` 只能保证按同一套价格表计算时不低于渠道原始 usage 成本。它不是利润率策略，也不会覆盖以下情况：

1. 外部池真实采购价高于系统价格表。
2. 外部池 usage 不可信或缺失。
3. 模型价格表没有对应模型。

这些情况需要在 dashboard 上显式显示为未计价/无法判断，而不是自动编造成本。

### 反思 4：没有备用池时不应影响本地凭据链路

实现只在 `UsageRouteKind::ExternalPool` 的记录路径生效。本地凭据调用仍按现有 Kiro usage、缓存模拟和 pricing 逻辑记录，不读取外部池字段，不改变调度、不改变并发、不改变 payload 处理。

## 最终方案

### 1. UsageRecord 增加外部池账务快照

新增结构：

- `ExternalPoolUsageSnapshot`
  - `totalInputTokens`
  - `inputTokens`
  - `billableInputTokens`
  - `outputTokens`
  - `cacheReadInputTokens`
  - `cacheCreationInputTokens`
  - `cacheCreation5mInputTokens`
  - `cacheCreation1hInputTokens`

- `ExternalPoolBilling`
  - `rawUsage`
  - `reportedUsage`
  - `rawCostUsd`
  - `reportedCostUsd`
  - `billableCostUsd`
  - `costFloorDeltaUsd`
  - `costFloorApplied`
  - `pricingAvailable`
  - `pricingModel`
  - `usageProjectionMode`

`UsageRecord` 增加 `externalPoolBilling: Option<ExternalPoolBilling>`。

### 2. 外部池转发路径捕获 usage

非流式：

1. 读取外部池响应 body。
2. 从原始 JSON 的 `usage` 解析 `rawUsage`。
3. 根据 `usageProjectionMode` 得到最终返回 body。
4. 从最终 JSON 的 `usage` 解析 `reportedUsage`。
5. 计算 `ExternalPoolBilling` 并附加到成功记录。

流式：

1. SSE 事件仍边读边转发。
2. 对每个包含 `usage` 的事件，记录原始 usage。
3. 对整形后的事件，记录 reported usage。
4. 以最后一次非空 usage 作为最终 usage 快照。
5. stream 正常结束时记录成功；stream 出错或客户端断开时保留错误状态，只有已捕获到 usage 且适合记录时才附加账务信息。

### 3. 成本计算

使用当前系统的 `PricingCatalog::estimate(model, CacheUsage)`。

模型取 `route.payload.model`。原因是外部池可能不返回稳定 `model` 字段；系统对请求模型已经有价格映射/同步逻辑。后续如果要支持“按上游响应 model 计价”，可以在 billing 结构里扩展 `rawModel/reportedModel`。

计算规则：

```text
rawCost = price(rawUsage)
reportedCost = price(reportedUsage)
billableCost = max(rawCost, reportedCost)
costFloorDelta = max(0, billableCost - reportedCost)
costFloorApplied = rawCost > reportedCost
```

当任一 usage 缺失或价格不可用：

```text
pricingAvailable = false
billableCost = 0
costFloorDelta = 0
```

此时使用记录仍会显示外部池路径、尝试链路和 usageProjectionMode，但不会把不可靠成本计入账务。

### 4. Usage 记录语义

外部池成功且有可计价 billing：

- `usageSource = upstream_metadata`
- token 字段写 `reportedUsage`
- `estimatedCostUsd = billableCost`
- `pricingAvailable = true`
- `pricingModel = billing.pricingModel`
- `externalPoolBilling = Some(...)`

外部池成功但没有 usage 或无法计价：

- 如果外部池返回了 usage，则 token 字段写 `reportedUsage`，这样详情页仍能看到真实/整形后的 token 结构。
- 如果外部池没有返回 usage，则保留 request estimate 作为 token 估计。
- `estimatedCostUsd = 0`
- `pricingAvailable = false`
- 有 usage 但无法计价时，`externalPoolBilling.pricingAvailable=false`，raw/reported usage 会保留，成本字段为 0。
- 没有 usage 时，`externalPoolBilling` 为空。

失败请求：

- 保持现有错误记录语义。
- 不把失败请求计入外部池成本保底，避免把没有完成的渠道请求当成账务成功。

### 5. Rollup 统计

在 `usage_rollup_totals` 和 `usage_rollup_time_buckets` 增加字段：

- `external_pool_requests`
- `external_pool_priced_requests`
- `external_pool_unpriced_requests`
- `external_pool_cost_floor_applied_requests`
- `external_pool_raw_cost_usd`
- `external_pool_reported_cost_usd`
- `external_pool_billable_cost_usd`
- `external_pool_cost_floor_delta_usd`

`UsageRollupMetrics::from_record()` 从 `UsageRecord.externalPoolBilling` 提取这些字段。所有维度都累计这些数据，包括 global/model/endpoint/status/source，dashboard 的总览窗口只读取 global 维度。

### 6. 管理后台展示

新版和旧版 UI 都补充：

1. Dashboard/总览选中窗口中新增“备用池成本保护”区块：
   - 外部池请求
   - 可计价请求
   - 渠道成本
   - 整形展示成本
   - 最终计费
   - 保底补差
   - 保底触发次数
   - 如果 `reportedCost > rawCost`，差值为 0，说明没有低于渠道成本。

2. 使用记录详情中，外部池记录显示：
   - raw usage/cost
   - reported usage/cost
   - billable cost
   - floor delta
   - pricing model
   - projection mode

## 验证计划

1. 后端普通测试：
   - `cargo fmt`
   - `cargo check -q`
   - `cargo test -q`

2. PostgreSQL 集成测试：
   - 使用 `KIRO_RS_TEST_POSTGRES_URL` 跑完整测试。
   - 新增测试模拟至少 1000 条外部池成功 usage record：
     - 一部分 rawCost > reportedCost，验证保底触发。
     - 一部分 reportedCost >= rawCost，验证不触发保底。
     - 一部分 unpriced，验证不计价。
   - 验证 dashboard 今天/最近 24 小时窗口的外部池成本统计。
   - 执行 usage 明细清理后再次查询 dashboard，确认统计仍保留。

3. 前端：
   - `admin-ui` build。
   - `admin-ui-daisy` build。

## 实现过程中的自检清单

每次代码改动后检查：

1. 是否影响了没有备用池的本地凭据路径。
2. 是否把账务保底错误地写回了下游响应。
3. 是否让 usage/dashboard 重新依赖全表扫描。
4. 是否破坏了 stream 的实时转发和 lease 释放。
5. 是否在清理明细后丢失统计。
6. 是否在价格缺失时错误地把 0 成本当成可靠成本。
