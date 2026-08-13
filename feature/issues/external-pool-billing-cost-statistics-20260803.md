# 外部池费用口径与 Dashboard 聚合差异分析：原始成本高于展示计费

Status: `analysis-confirmed / implementation-complete / focused-verified / release-build-passed / production-observation-pending`

Severity: `High`

Scope: 三台现网 `kiro.rs` 的外部池费用计算、usage 捕获、UsageRecord、PostgreSQL rollup、Redis Dashboard、两套 UI 展示，以及“单条明细多数原始计费低于展示计费，但 Dashboard 汇总原始成本明显更高”的现象。

Analysis date: `2026-08-03`（Asia/Shanghai）

Affected deployments:

- `152.53.243.159`
- `152.53.194.170`
- `152.53.194.142`

本轮只读分析，没有修改生产配置、PostgreSQL、Redis、容器或服务进程。

## 结论摘要

当前现象不是一个单一的“Dashboard 加总错误”，而是四个因素叠加：

1. 页面“上游原始成本”并不是外部供应商实际扣款。当前代码使用外部池的原始 usage（有时是本地估算 usage）乘以 `kiro.rs` 本地价格目录计算，属于“渠道参考成本”。
2. 大部分外部池配置使用“按当前路径策略”的用量整理。它会把原始普通输入重新分配为缓存读取/缓存创建，并再次应用缓存和输出补偿。由于缓存价格低于普通输入，长输入请求可能从数美元降到几角钱。
3. 单条明细是重尾分布。大量短请求可以出现“原始成本低于展示计费”，但少量几十万 token 的请求会贡献远高于普通请求的原始成本，最终把汇总方向反转。
4. 历史 `jinnyapi` 流式请求存在大量 `missing_stream_usage` / `unrecognized_success_body`，旧版本代码会使用本地请求输入和本地输出估算填充“原始 usage”。这会进一步降低“上游原始成本”字段作为供应商真实账单的可信度；当前修复已补齐流式 OpenAI 兼容 usage 的归一化，但历史记录不会被回写成真实上游费用。

已对账的完整小时内，PostgreSQL 明细与 rollup 完全相等，没有发现重复累计或漏累计。Dashboard 的 Redis/ PostgreSQL 数据源分叉仍是实时一致性风险，但目前没有证据表明它制造了上述“原始成本总额更高”的主现象。

## 页面字段与代码字段对照

| 页面中文字段 | 记录字段 | 当前真实含义 |
| --- | --- | --- |
| 上游原始 usage | `externalPoolBilling.rawUsage` | 外部池捕获的原始 usage；可能是真实上游 usage，也可能是本地估算 |
| 上游原始成本 | `rawCostUsd` | `rawUsage × kiro.rs 本地价格目录` 的参考成本，不等于供应商账单 |
| 展示计费 | `shapedCostUsd` | 按当前路径 usage 整理后的 usage 计算的成本 |
| 补偿后计费 | `upliftedCostUsd` | 展示 usage 再应用缓存/输出补偿后的成本 |
| 上报费用 | `reportedCostUsd` | 最终上报 usage 的本地价格估算 |
| 可计费费用 | `billableCostUsd` | 当前等于补偿后计费，并没有真正把成本抬到原始成本 |
| 计费差额 | `profitUsd` | `补偿后计费 - 上游原始成本`，是本地参考口径差额 |
| 兜底/成本底价 | `costFloorApplied` | 仅表示补偿后费用低于原始成本并记录差额；没有改变可计费费用 |

## 现网证据

### 1. 三台机器最近 6 小时汇总

来源：

- [159 明细与汇总证据](../../tmp/prod-evidence/20260803-220600-external-billing-dashboard-159/raw/db/detail-cost-stats.txt)
- [170 汇总与样本证据](../../tmp/prod-evidence/20260803-220600-external-billing-dashboard-170/raw/combined2.txt)
- [142 汇总与样本证据](../../tmp/prod-evidence/20260803-220600-external-billing-dashboard-142/raw/combined2.txt)

| 机器 | 请求数 | 上游原始成本 | 展示计费 | 补偿后/可计费 | 原始 - 补偿后 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 152.53.243.159 | 22,959 | 5,929.5163 | 3,504.2819 | 4,289.6896 | -1,639.8266 |
| 152.53.194.170 | 12,945 | 2,778.9134 | 1,682.1714 | 2,098.3537 | -680.5597 |
| 152.53.194.142 | 15,364 | 4,018.0712 | 2,119.3835 | 2,604.7341 | -1,413.3370 |

这三个窗口的方向一致：`上游原始成本 > 展示计费`，且 `上游原始成本 > 补偿后/可计费`。

### 2. 单条“多数相反”并不矛盾

159 最近 6 小时明细的关系统计：

- 原始成本 < 展示计费：12,645 条；
- 原始成本 > 展示计费：8,742 条；
- 原始成本 = 展示计费：1,572 条；
- 原始成本 < 补偿后计费：12,645 条；
- 原始成本 > 补偿后计费：8,742 条。

但总和仍然是原始成本更高。170 和 142 的 1,000 条样本也出现相同结构：

- 170：558 条原始成本低于展示计费，391 条高于展示计费；样本总原始成本 `295.5734`，展示计费 `123.9210`。
- 142：547 条原始成本低于展示计费，427 条高于展示计费；样本总原始成本 `295.5734`，展示计费 `123.9210`（证据中的样本统计）。

这是重尾分布的加权结果，不是计数逻辑与求和逻辑互相矛盾。159 完整小时 `13:00-14:00 UTC` 的原始成本最高请求包括 `3.3822`、`3.1491`、`3.1455`、`3.1412` 等单条大请求；同一小时共有 3,580 条请求。

### 3. 明细与 rollup 对账

固定窗口：`2026-08-03 13:00:00-14:00:00 UTC`，机器 `152.53.243.159`。

明细总计：

- 请求：3,580；
- 上游原始成本：733.5404737；
- 展示计费：416.21054215；
- 补偿后/可计费：512.2172343。

rollup 分池相加：

- `jinnyapi`：2,075 请求，原始 476.607039，展示 229.08419505，补偿后 286.6557675；
- `kkkkyue`：1,505 请求，原始 256.9334347，展示 187.1263471，补偿后 225.5614668。

两池相加与明细完全一致。因此已核对窗口没有发现 SQL 重复统计、rollup 重复累加或明细漏计。

### 4. 外部池 usage 不是“整体没有返回”

绕过 `kiro.rs`，直接调用三台机器当前配置的外部池 `kkkkyue`：

- 每台 25 次，共 75 次；
- 非流式、独立流式、连续三轮追问均覆盖；
- HTTP 200：75/75；
- 捕获真实 usage：75/75；
- usage 解析错误：0；
- 顶层或 `message.usage` 均可见输入、输出、缓存创建、缓存读取字段。

因此“外部池都不返回 usage”不是成立的总括结论。生产历史中仍有 `jinnyapi`，需要按池、模型、流式/非流式、响应格式拆分。

159 近期样本的 `usageEstimated` 分组显示，至少存在大量本地估算记录，同时也存在真实上游 usage 记录；具体分组见上述 159 证据文件中的 `SAMPLE_FIELDS`。

159 最近 6 小时 `SAMPLE_FIELDS` 的完整分组为：

| 用量整理模式 | 用量整理已应用 | 响应体用量整理已应用 | 用量是否为估算 | 请求数 |
| --- | --- | --- | --- | ---: |
| `current_path_policy` | `true` | `true` | `true` | 15,328 |
| `current_path_policy` | `true` | `true` | `false` | 6,440 |
| 字段为空（旧记录/缺少外部池账务快照） | - | - | - | 1,245 |

这组数据直接证明：生产窗口中的“上游原始成本”不是同一种数据来源，至少混合了 15,328 条本地估算 usage 和 6,440 条非估算 usage。

## 复现与证据边界

本轮现象可用两类复现稳定重现：

1. 生产数据库固定窗口：同一窗口中统计“原始成本 < 展示计费”的条数，再对同一批记录求和，会得到“条数多数低于、金额总和反而高于”的结果。
2. 本地直接调用外部池：对当前 `kkkkyue` 做非流式、独立流式、连续追问共 75 次，均能获得真实 usage；因此“外部池没有 usage”不能作为全局解释。

尚未完成的复现是 `jinnyapi` 原始 SSE 响应格式。生产记录已经出现 `missing_stream_usage`，但没有保留对应的原始 SSE 脱敏片段，不能仅凭记录断言其必然是 OpenAI 风格字段或增量字段。

## 源码链与根因

### A. “上游原始成本”是本地价格估算，不是供应商账单

`src/external_pool.rs` 的 `external_pool_billing(...)` 先选择计价模型，然后分别调用本地 `PricingCatalog`：

- [external_pool.rs:10659](../../src/external_pool.rs#L10659)
- [pricing.rs:161](../../src/anthropic/pricing.rs#L161)

计算关系是：

```text
上游原始成本 = 本地价格目录(原始 usage)
展示计费     = 本地价格目录(展示 usage)
补偿后计费   = 本地价格目录(最终上报 usage)
计费差额     = 补偿后计费 - 上游原始成本
```

`ModelPricing::estimate` 只按输入、输出、缓存创建、缓存读取四类 token 乘本地单价，代码没有读取外部供应商的美元账单字段。即使外部池返回了真实 usage，当前 `rawCostUsd` 仍然只是本地价格目录下的成本参考值。

因此页面当前“按外部上游返回 usage 估算”只说明 usage 来源，未说明价格来源，容易被理解成“供应商真实扣款”。

### B. “按当前路径策略”会改变 usage 的价格结构

当外部池不是 `pass_through` 而是 `current_path_policy` 时，`project_usage_value(...)` 会：

1. 用当前请求的输入 token、缓存状态和路径策略生成受控 usage；
2. 把普通输入重新分配为缓存读取/缓存创建；
3. 应用缓存上浮、输出上浮和最终标准字段保护；
4. 将整理后的 usage 写回响应体、SSE 和 `UsageRecord`。

代码入口：

- [external_pool.rs:10331](../../src/external_pool.rs#L10331)
- [external_pool/usage_projection.rs](../../src/external_pool/usage_projection.rs)

普通输入单价高于缓存创建，缓存创建又高于缓存读取。于是：

- 短请求、低原始输入：整理后新增缓存创建或输出补偿，可能出现 `原始成本 < 展示计费`；
- 长请求、原始普通输入很多：整理后大量变成缓存读取，可能出现 `原始成本 >> 展示计费`；
- 少量长请求的金额远大于大量短请求，汇总方向由金额而不是条数决定。

159 证据中的极端样本清楚展示了这一点：原始普通输入 `472,097`，原始成本 `2.360485`；最终上报主要是缓存读取，补偿后费用只有 `0.2743295`。

### C. 缺少上游 usage 时，`rawUsage` 会退回本地估算

非流式成功体没有可识别 usage 时，代码会用请求输入和输出文本估算：

- [external_pool.rs:8883](../../src/external_pool.rs#L8883)
- [external_pool.rs:9150](../../src/external_pool.rs#L9150)
- [external_pool.rs:9311](../../src/external_pool.rs#L9311)

流式结束没有捕获 usage 时，会走 `external_pool_billing_from_stream_estimate(...)`，把本地请求输入和本地输出估算结果当作 `rawUsage`，并标记：

```text
usageEstimated = true
usageEstimateReason = missing_stream_usage
usageCandidatePath = $stream.estimated
```

生产 `jinnyapi` 样本中已出现大量 `missing_stream_usage` 和 `unrecognized_success_body`。这说明当前“上游原始成本”混合了真实上游 usage 和本地估算 usage，不能直接当作供应商真实成本。

### D. 流式 usage 解析存在格式覆盖风险

非流式路径使用 `cache_usage_from_any_value`，同时支持 Anthropic 风格和 OpenAI 风格 `prompt_tokens/completion_tokens`：

- [external_pool.rs:9038](../../src/external_pool.rs#L9038)
- [external_pool.rs:9091](../../src/external_pool.rs#L9091)

但流式 `process_single_usage_value(...)` 直接调用 `cache_usage_from_value(...)`：

- [external_pool.rs:9451](../../src/external_pool.rs#L9451)

如果某个外部池只在 SSE 中返回 OpenAI 风格 usage，非流式可能能识别，流式则可能进入 `missing_stream_usage` 本地估算。这个是已确认的代码缺口，和生产现象相符，但尚未取得 `jinnyapi` 原始 SSE 响应，当前应标记为“高概率候选”，不能写成已经由上游响应直接证明的唯一根因。

## 本轮已实现的修复

### 1. 流式 usage 归一化

流式响应中的 usage 现在与非流式响应使用同一套识别边界：

- Anthropic 风格字段继续按原字段读取；
- OpenAI 兼容的 `prompt_tokens` / `completion_tokens` 会归一化为内部
  “输入 token”/“输出 token”字段；
- 只在启用“响应体用量整理”时改写下游响应；Raw 透传关闭重写时，只用于
  内部账务捕获，不修改下游原始字节；
- 顶层 `usage`、`message.usage`、`delta.usage` 的流式事件均覆盖；
- 无法识别 usage 时仍走原有本地估算 fallback，并标记“用量是否为估算”和
  “用量估算原因”。

### 2. 原始成本字段口径修正

外部池成功记录的“原始成本”现在严格取：

1. 上游 usage 已识别：按上游真实 usage 计算的 `rawCostUsd`；
2. 上游没有可识别 usage：按明确标记为本地估算 fallback 的
   `rawCostUsd`；
3. 没有外部池账务对象的成功边界：才保留通用估算费用 fallback；
4. 错误记录的标准费用字段保持为零。

不会再因为本地价格不可用，就把“展示计费/上报费用”偷换成“原始成本”。
“原始成本”仍是 `kiro.rs` 本地价格目录下的参考成本，不等于供应商实际扣款；
只有供应商明确返回金额或账单接口时，才可以另设“外部供应商真实费用”。

### 3. 本地 usage 整形保持独立

修复没有改变缓存创建、缓存读取、输入保护、输出补偿、缓存状态提交或路径
策略。验证同时检查：

- `rawUsage` 保留上游真实值；
- `shapedUsage`/`reportedUsage` 继续按本地路径配置生成；
- 下游响应只在配置允许时整形；
- usage 缺失不影响下游生成正常的本地整形 usage。

## 修复后验证结果

本轮 focused 复核证据见
[外部池 usage 原始成本与 Dashboard 计费验证（2026-08-04）](../evidence/external-pool-billing-verification-20260804.md)。

### Rust 与存储

- `external_pool::tests`：`214 passed / 0 failed`；
- 全量 Rust 测试：`1,873` 个测试目标执行成功，失败数为 `0`；
- 本地隔离 PostgreSQL：
  - `postgres_persists_runtime_config_credentials_stats_usage_and_pricing`：`1/1`；
  - `postgres_rolls_up_external_pool_billing_for_large_samples_and_removes_after_cleanup`：`1/1`；
- 本地隔离 Redis：
  - `redis_usage_summary_and_dashboard_are_materialized`：`1/1`；
- PG rollup 测试验证原始、整形、补偿后、上报和清理后的汇总均保持原有合同。

### UI 与构建

- `admin-ui`：`tsc -b && vite build` 通过；
- `cargo fmt --check` 通过；
- `cargo build --release` 通过；
- 严格 `cargo clippy --all-targets -- -D warnings` 仍被仓库既有
  `313` 项基线问题阻断，未发现新增修改行专属 warning；本轮不修改无关
  的旧 clippy 问题。

### 真实外部池验证边界

此前对服务端外部池配置的本地脱敏直接调用已覆盖非流式、独立流式和连续追问：
`75/75` 返回真实 usage。当前修复新增的 OpenAI 风格流式字段由本地协议单测
覆盖；2026-08-04 本地 focused 复核还覆盖 PgSQL rollup、Redis Dashboard
materialization、Admin UI build、文档合同和格式检查。尚未再次对三台生产服务
发起请求，也没有修改生产配置。

### E. “成本底价”目前是标记，不是实际 floor

代码只计算：

```text
cost_floor_delta = max(原始成本 - 补偿后计费, 0)
cost_floor_applied = 原始成本 > 补偿后计费
可计费费用 = 补偿后计费
```

因此页面的“兜底”容易让人以为已经把可计费费用抬到原始成本，实际并没有。若产品目标是“绝不低于参考成本”，这属于产品/计费语义未实现；若产品目标只是监控亏损风险，则字段名称和 UI 文案应改成“低于参考成本标记”，不能直接改算法。

### F. Dashboard 存在实时数据源分叉，但不是已确认主因

`/usage-dashboard/windows` 优先 Redis，失败才回 PostgreSQL：

- [usage.rs:2330](../../src/anthropic/usage.rs#L2330)

`/usage-dashboard/external-pool-billing` 固定走 PostgreSQL：

- [usage.rs:2534](../../src/anthropic/usage.rs#L2534)

前端也分开请求窗口数据与外部池费用拆分：

- [overview-page.tsx:989](../../ui/src/features/overview/overview-page.tsx#L989)
- [overview-page.tsx:1019](../../ui/src/features/overview/overview-page.tsx#L1019)

费用页的总卡片和“外部账号成本与差额”表也不是同一响应对象：总卡片使用窗口响应中的 `summary.externalPoolBilling`，按池表格使用独立的外部池费用拆分响应。这意味着即使两次请求的窗口 key 相同，只要生成时刻或数据源不同，页面就可能出现“总额”和“各池相加”暂时不相等。

这会带来：

- 两个接口 `generatedAt` 不同；
- Redis writer 延迟或丢观测时，窗口总览与 PostgreSQL 外部池拆分短时不一致；
- 同一页面可能出现请求数和费用金额不是同一快照。

但完整小时的明细-rollup 已经精确对账，所以目前没有证据证明这个分叉造成“原始成本总额高于展示计费”的方向性差异。

## 已排除或尚未证实的解释

### 已排除为主因

- PostgreSQL rollup 重复统计：完整小时已对账相等；
- 外部池完全没有 usage：直接池测试 75/75 成功；
- 单纯因为单条明细页面截图或排序：生产 SQL 统计已经复现同样方向。

### 尚未证实

- `jinnyapi` 的具体 SSE usage 字段是否为 OpenAI 风格、增量格式或某些事件根本不带 usage；
- 外部供应商真实美元账单是否与 `kiro.rs` 本地价格目录一致；
- Redis writer 延迟/丢观测是否在实时窗口造成额外差异；
- 历史版本中旧字段回退是否在某些记录上重复使用“上报费用”替代“展示计费”。

## 不应立即做的改动

在没有明确产品口径前，不应直接：

1. 把 `可计费费用` 强行抬到 `上游原始成本`；
2. 把 `rawCostUsd` 改名为“外部供应商真实费用”但仍使用本地价格目录；
3. 看到单条 `原始成本 < 展示计费` 就判断 Dashboard 错误；
4. 在没有 `jinnyapi` 原始 SSE 证据前，强行重写所有流式 usage；
5. 用重试掩盖 usage 解析缺失。usage 缺失通常是成功响应协议/解析问题，不是重试能解决的问题。

## 后续观察与未关闭事项

以下事项不属于本轮 usage 代码修复，继续保留：

- 生产 `jinnyapi` 等池按模型/协议拆分的真实 usage 覆盖率；
- Dashboard 两个接口统一快照时间或明确显示数据时间差；
- “上游原始成本”与供应商真实美元账单的产品字段区分；
- “低于参考成本”是否只是监控标记，还是需要真正成本底价；
- 三台机器升级到包含本修复的版本后，`usageEstimated`、`missing_stream_usage`
  和费用方向的生产复发观察。

## 历史建议（本轮不直接执行）

### P0：先修正账务字段语义

页面和接口明确区分：

- `上游原始 usage`：真实上游 usage / 本地估算 usage；
- `上游原始成本（本地价格估算）`：当前 `rawCostUsd`；
- `外部供应商真实费用`：只有上游明确返回金额或供应商账单接口提供时才展示；
- `usage 来源`：真实上游 / 本地估算；
- `本地估算请求数`、`真实 usage 请求数`、`未计价请求数`。

不改变现有计费算法，只先消除误解。

### P1：按外部池和协议补齐流式 usage 归一化

对流式 `usage` 复用非流式的 Anthropic/OpenAI 归一化逻辑，并增加：

- 池名、模型、流式/非流式维度的 `usageEstimated` 统计；
- `usageCandidatePath` 统计；
- 增量 usage 与累计 usage 的协议判别；
- 原始 SSE 脱敏证据采集。

验收要求：同一池/模型/请求模式下，真实 usage 不能无故变成本地估算。

### P1：Dashboard 使用同一快照口径

优先选择以下一个方向：

- 外部池费用拆分也从与窗口汇总相同的 Redis 快照读取；
- 或窗口汇总和费用拆分统一从 PostgreSQL 读取；
- 至少让两个接口返回同一个 `generatedAt`/快照版本，并在前端显示“数据时间不同”。

### P2：明确“成本底价”产品决策

二选一：

- 只是监控标记：改为“低于参考成本请求数/差额”，不要称“兜底”；
- 真正成本 floor：明确把 `可计费费用` 抬到参考成本，并新增价格来源、审计和回滚测试。

### 验证矩阵

1. 真实外部池直连：非流式、独立流式、连续追问、工具、图片，分别记录原始响应 usage；
2. `pass_through` 与 `current_path_policy` 对照；
3. Anthropic usage、OpenAI usage、顶层/嵌套 usage、增量/累计 SSE；
4. 同一固定窗口：明细、rollup、Redis、`/usage-dashboard/windows`、`/usage-dashboard/external-pool-billing` 五方对账；
5. 清理 usage 明细后，rollup 与费用拆分是否按产品合同保留；
6. 两套 UI 对真实 usage/估算 usage、价格来源和成本底价文案的一致性。

## 版本边界

现网证据主要来自 `0.0.130` 或更早版本；本地当前工作树为 `v0.0.131`（`HEAD 82a1c99`）。`v0.0.130..v0.0.131` 的主要改动是 usage 清理批量上限，外部池费用核心计算与 Dashboard rollup 逻辑没有变化。因此本报告的费用口径结论对当前代码仍然有效，但生产复现必须在 `0.0.131` 上重新观察。
