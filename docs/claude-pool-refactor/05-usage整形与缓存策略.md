# 05 usage 整形与缓存策略

这是 R4 要求完整保留的主能力。结论先行：**算法不改，代码位置不变，只换输入类型和名字**。本章记录现有实现的契约，作为迁移后回归的依据。

## 1. 现有实现的组成

| 模块 | 职责 | 位置 |
|---|---|---|
| 策略解析 | 路由 → `ResolvedCachePolicy`：base 旧顶层字段 → reportedUsage 路径覆盖 → 按 `cacheType` 取模板 → 路由补丁 | `config.rs:3850-3904`、`3357-3451` |
| 前缀追踪 | `PromptCacheTracker`：scope=(账号, conversation, model, 路由 namespace)，TTL、容量上限、淘汰 | `prompt_cache.rs` |
| 缓存模拟 | `CacheSimulation::to_usage`：目标读取比例 0.98、tokenScale 1.6、模拟上限 300k、cap jitter | `cache.rs:1872-1946`、`1749-1790` |
| 字段整形 | `ReportedUsagePolicy`：input/output/cacheRead/cacheCreation 各自 raw / preserve / sample-max / sample-target；input 差额转入 read 或 creation；最终上限 | `cache.rs:257-327`、`479-506`、`630-656`、`999-1039`、`1114-1133`、`742-866` |
| 写入频次控制 | `PromptCacheCreationController`：成功次数、间隔、增量、单次上限、窗口预算 | `prompt_cache_creation_control.rs`、`config.rs:1505-1545` |
| 命中率整形 | `shape_cache_hit_usage`：总量不变按目标命中率分配读取、minCostRatio 补成本、小值抬高 | `cache.rs:1210-1289` |
| 冷请求约束 | 未命中时读取清零，input+写入 ≤ 真实输入 | `cache.rs:1301-1351` |
| bounded 收口 | 上下限 > 频次 > 命中率 > 不亏损；范围外丢弃；补不上跨请求记账 | `bounded_shaping.rs` |
| plus 整形 | 四字段倍率与浮动 → 频次 → 上下限 → 计费下限 | `kiro_plus_shaping.rs`、`model/kiro_rs_tool_plus.rs` |
| TTL 拆分 | `apply_cache_write_ttl`：按 `cacheTtl` 模式与请求 `cache_control.ttl` 拆 5m/1h | `cache.rs:100-116`、`209-218` |
| 账号侧投影 | 把以上串起来并原地改写 JSON | `external_pool/usage_projection.rs`、`external_pool.rs:15818-16445` |

六种策略类型（`config.rs:3509`）：`no_cache`、`current_high_cache`、`kiro_rs_tool`、`stable_segment_cache`、`kiro_rs_tool_plus`、`bounded_high_cache`。全部保留，后两个带 Kiro 字样的改名 `prefix_tool`、`prefix_tool_plus`，配置键 `kiroRsTool` → `prefixTool`、`kiroRsToolPlus` → `prefixToolPlus`。导入旧配置时自动映射。

## 2. 策略挂载

usage 整形只由路由挂载的策略决定（C2、R7）：

- 路由定义里的 `cachePolicy` 与 `reportedUsage` 就是现有 `cachePolicy.pathOverrides[prefix]` 与 `reportedUsage.pathOverrides[prefix]`，结构一字不改。
- 全局保留六个策略模板的默认参数（`cachePolicy.default`、`currentHighCache`、`prefixTool`、`stableSegmentCache`、`prefixToolPlus`、`boundedHighCache`）和 legacy 基础值（`promptCache*` 顶层字段）。
- 内置路由默认：`""`、`/cc`、`/ha` 为 `current_high_cache`，`/na` 为 `no_cache`。
- 账号只有一个开关 `usageProjectionMode`：`current_path_policy`（按路由策略整形，新默认）或 `pass_through`（原样）。账号不能改策略参数。
- 账号级 `overrides.usageUplift`（uplift 百分比、成本下限、output 放大）覆盖全局 `accounts.usageProjection*`，但都在路由 bounds 之内生效。

## 3. 账号侧投影顺序

`project_usage_value`（`external_pool.rs:15818-15964`）的现有顺序，不改：

```
 1. 门槛判断（build_template）：
      非流式且 skipNonStreamUsageProjection → 不整形
      高缓存策略但 reportedUsage.enabled=false → 不整形
      strictCacheEligibility 开且不满足官方缓存条件 → 不整形
 2. 基准输入 = 本地 token 估算（不用上游 input）
 3. output = max(上游 output, 响应估算)
 4. 上游 cache_read > 0 只作为"读取证据"
 5. 前缀追踪查找 → 缓存模拟
 6. （plus）四字段倍率
 7. 写入频次控制预览
 8. reportedUsage 字段整形
 9. 最终 input 与缓存上限
10. （plus）finalize
11. 账号 uplift：读写 ×(1+uplift%)，output 放大
12. 再执行一次最终上限
13. 成本下限：目标 = 上游真实 usage 计费 ×(1+margin)；依次补 creation、read、input
14. （bounded）收口；收口后不再改写
15. TTL 拆分：cacheTtl 模式 + 请求 ttl + 上游是否报告 1h 写入
16. 原地覆盖 JSON usage 字段
```

流式：逐个 SSE `data:` 里的 usage 槽改写（`external_pool.rs:14211-14255`、`14745-14812`）；`message_start.usage` 只预览不提交，`message_delta.usage` 提交。非流式改写整包。成功终止后才提交前缀追踪与频次状态（`usage_projection.rs:522-575`）。

### 3.1 零用量估算（`zeroUsageRepair`）

上游返回 200 但 usage 里输入（输入 + 缓存读 + 缓存写）或输出为 0 时，在上游 usage 进入本系统的入口先估算写回，之后的门槛判断、整形、收口、采集都当作上游本来就给了这个值。只对按路由策略整形 usage 的账号生效，透传上游 usage 的账号不改；配置挂在路由缓存策略里（模板与路由两层，所有策略含 `no_cache` 都生效），输入、输出各一组：

| 字段 | 默认 | 说明 |
|---|---|---|
| `enabled` | 开 | 输入按本次请求内容估算，输出按实际返回内容估算；关闭即保持上游原值 |
| `fallback` | `keep` | 估算仍为 0 时：`keep` 保持 0；`range` 在下限到上限间随机；`ratio` 按另一侧用量的百分比生成（输出按输入、输入按输出），再收在下限到上限之间 |
| `minTokens` / `maxTokens` | 1 / 20 | `range` 的范围，也是 `ratio` 结果的范围 |
| `ratioPercent` | 0.5 | `ratio` 的百分比 |

- 整形的请求：估算值进入上面第 1–16 步，最终上下限照常生效（第 2 步基准输入本来就用本地估算，零输入主要影响原始成本与成本下限的依据）。
- 不整形的请求（门槛判断不通过，原样转发上游 usage）：补出来的输出按路由最终输出上限（`finalOutputMaxTokens` 扣掉最大扣减量）裁剪；输入与上游真实输入一样原样转发（输入字段 `sample-max` 是把超出部分挪进缓存的整形规则，不是截断上限）。
- 流式只修最终用量事件（`message_delta` 的顶层 `usage`），判断时合并 `message_start` 已给出的用量；`message_start` 不动。
- 非流式上游 usage 全为 0 时沿用“缺失用量”估算（`missing_upstream_usage`），估算结果里仍为 0 的一侧再按 `fallback` 补。
- 修过的请求在计费记录里 `usageEstimated=true`，原因 `zero_upstream_input` / `zero_upstream_output` / `zero_upstream_input_output`。
- 开销：每个 SSE 事件只做一次字节查找；请求输入估算只在真要补输入时才解析请求体（整形上下文已有估算时直接复用）。

### 3.2 与 C1 的关系

第 13 步"成本下限"在 `current_high_cache` 链路里可以突破 input 上限（这是旧的已知行为，memory 记录现网出现过上限 500 被补到 8198）。本方案**不改这个行为**：用户此前明确要求旧策略行为不动，新语义一律做成新策略（`bounded_high_cache` 就是为此而生）。需要上下限绝对约束的路由挂 `bounded_high_cache`。

### 3.3 上游请求 TTL 与下游展示 TTL

路由 `cacheTtl.account` 仅决定按路由整形时，下游 usage 的缓存写入归入 5m 或 1h，以及该份 usage 对应的展示计费。账号透传上游 usage 时不重新拆分。账号选择、优先级、并发、排队和重试不读取这项设置。

模拟缓存状态写入也固定为 5 分钟（仍受全局 entry TTL 上限约束），不再使用最终展示 TTL 延长缓存留存。客户端原始 1h 标记只作为展示策略的输入。

发往上游的有效 `cache_control.type=ephemeral` 标记统一使用 `ttl="5m"`，包括顶层自动缓存、system、tools、消息内容及 tool_result 的嵌套内容。已有 1h 或其他 TTL 改为 5m，未指定 TTL 的已有标记补 5m；没有缓存标记时不新增断点。这项转发规则同时适用于整形和透传账号，不随下游展示时长变化。

归一化发生在账号请求准备边界，仅定点修改出站字节；客户端原始请求和探测结果保留。因此“跟随请求”仍读取客户端原始 TTL，即使上游始终收到 5m，下游仍可按客户端的 1h 标记展示。原始 usage 与原始成本继续采用上游实际响应。工具输入、JSON schema、metadata 和文本里的同名字段不作为缓存协议标记处理。

本地验收：Rust 1.92 的 TTL 专项 10 项通过（含 5 种展示模式 × 客户端未指定/5m/1h × 流式/非流式 × 整形/透传的 60 组出站与 usage 联合校验）；前端缓存时长与策略导入导出 19 项通过，TypeScript 检查通过。验证构建目录已由 scoped wrapper 清理。尚未发布或部署。

## 4. 输入类型

现有 `cache.rs:5` 引用 `kiro::model::events::MetadataTokenUsage`（字段：`uncached_input_tokens`、`output_tokens`、`total_tokens`、`cache_read_input_tokens`、`cache_write_input_tokens`）。替换为 `shaping::UpstreamUsage`：

```rust
pub struct UpstreamUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_creation_5m_input_tokens: Option<i32>,
    pub cache_creation_1h_input_tokens: Option<i32>,
}
```

它直接对应 Anthropic 响应里的 `usage` 对象。`cache.rs:2182-2265` 的 `build_usage_with_simulation_policy` 与 `usage_from_metadata_or_estimate` 改为接收这个类型，语义：有上游 usage 就以它为 raw；`cache_read` 与 `cache_creation` 都为 0 且本地投影开启时才用模拟补足。这和现有外部池投影的做法一致。

## 5. 计费记录

`ExternalPoolBilling`（`usage.rs:162-222`）改名 `AccountBilling`，字段不变：raw / shaped / reported 三份 usage，raw / shaped / uplifted / reported 四种 cost，profit 和 costFloor 标记。`UsageRecord` 的 `estimated_cost_usd` 按 reported 计，`original_cost_usd` 按 raw 计。密钥额度按 reported 累加（`usage.rs:2756-2779`）。新增 `account_cost_usd = raw cost × account.costMultiplier`，供统计页展示真实成本（C10 公开页仍不展示）。

## 6. 删除的部分

- `handlers.rs:3537-4126`、`4542-5645` 本地 Kiro 链路的整套整形编排（`RequestUsageContext`、`CredentialUsageContext`）。账号侧已有等价实现。
- `cachePolicy.*.cachePoint`（`config.rs:1782-1810`）：Anthropic cache_control 转 Kiro cachePoint，与 Anthropic 上游无关。请求已有的缓存断点保留，上游 TTL 固定为 5m（见 3.3）。
- `cacheTtl.local`：只剩 `cacheTtl.externalPool` 一份，改名 `cacheTtl`。
- `usageProjectionMode` 为 `pass_through` 时的 OpenAI 字段归一（随 OpenAI 入口删除）。

## 7. 回归依据

- `external_pool/tests.rs` 中全部 usage 投影测试（约 2.2 万行文件的主要内容）原样保留并通过。
- `anthropic/cache.rs`、`prompt_cache.rs`、`prompt_cache_creation_control.rs`、`bounded_shaping.rs`、`kiro_plus_shaping.rs` 的单测原样保留并通过；凡是构造 `MetadataTokenUsage` 的测试改为构造 `UpstreamUsage`，字段一一对应。
- 新增一组 golden 测试：对 `main` 上的现网配置（已脱敏）加固定请求与固定上游 usage，记录当前 reported usage 输出，迁移后必须逐字节一致。详见 [09](09-实施阶段与验收.md)。
