# Prompt Cache 三策略并列与 Stable Segment Cache 设计专项文档

日期：2026-10-01（Asia/Shanghai）  
状态：专项设计 + 已实现接入 + mock upstream 端到端验证  
范围：分析当前 `current_high_cache`、`kiro_rs_tool` 两种缓存策略，设计第三种
`stable_segment_cache` 策略，并说明配置参数、路由接入、现有系统接入点和验证方案。

## 1. 背景与目标

本轮真实账号验证覆盖了多账号、多轮对话、长 system prompt、tools schema、
长历史、流式和非流式请求。结论是：Kiro 上游在这些真实请求中没有返回真实
prompt-cache breakdown。

典型上游事实：

- 流式上游事件只有 `assistantResponseEvent`、`contextUsageEvent`、
  `meteringEvent`。
- `sawUpstreamMetadata=false`。
- `metadataUsage=null`。
- `rawUsage.cacheReadInputTokens=0`。
- `rawUsage.cacheCreationInputTokens=0`。
- 下游响应里的 `cache_creation_input_tokens` 和 `cache_read_input_tokens`
  是本地兼容层构造出来的 reported usage。

用户明确约束：

1. 不改变调用方协议。
2. 不给 Claude Code 或 Anthropic 兼容调用方新增 `cacheSource` 等字段。
3. 返回字段继续保持 Claude Code 可解析的标准 usage 字段。
4. 需要新增一种缓存方案，和当前高缓存、`kiro-rs-tool` 缓存并列。
5. 新方案需要支持路由级配置和策略参数配置。
6. 新方案需要接入当前缓存系统，而不是绕开现有 `CachePolicyConfig`、
   `PromptCacheTracker`、`CacheUsage`。

本文把新增策略命名为：

```text
stable_segment_cache
```

Rust enum 对应建议：

```rust
PromptCacheStrategyType::StableSegmentCache
```

## 2. 当前系统事实

### 2.1 路由缓存策略框架已存在

当前代码已经有路径级缓存策略框架：

- `CachePolicyConfig.default`
- `CachePolicyConfig.current_high_cache`
- `CachePolicyConfig.kiro_rs_tool`
- `CachePolicyConfig.path_overrides`
- `resolve_cache_policy_for_path(...)`
- 最长路径前缀匹配
- `routeNamespace` 控制跨路由缓存隔离

相关位置：

- `src/model/config.rs`：`CachePolicyConfig`、`CacheRoutePolicyPatch`、
  `CacheRoutePolicy`、`PromptCacheStrategyType`。
- `src/anthropic/handlers.rs`：按 route policy 准备 request usage context。
- `src/anthropic/prompt_cache.rs`：`PromptCacheTracker`、profile 构建、
  fingerprint、TTL、hit/miss 和 bounds。
- `src/anthropic/cache.rs`：`CacheSimulation` 和 `CacheUsage` 到 Anthropic
  usage JSON 的转换。

因此新增策略不需要另起系统，应该沿用现有骨架。

### 2.2 现有策略枚举

当前策略枚举是：

```rust
pub enum PromptCacheStrategyType {
    NoCache,
    CurrentHighCache,
    KiroRsTool,
}
```

目标扩展为：

```rust
pub enum PromptCacheStrategyType {
    NoCache,
    CurrentHighCache,
    KiroRsTool,
    StableSegmentCache,
}
```

serde `snake_case` 后，对应配置值：

```json
"cacheType": "stable_segment_cache"
```

### 2.3 当前返回字段必须保持不变

调用方仍然只看到标准字段：

```json
{
  "input_tokens": 64,
  "cache_creation_input_tokens": 0,
  "cache_read_input_tokens": 42000,
  "output_tokens": 3
}
```

新策略不向调用方新增任何字段。

内部 usage 记录可以继续沿用现有字段和现有 `usageSource=local_prompt_cache`、
`simulated=true` 口径；本文不要求新增 DB 字段。

## 3. 三种缓存方案对比

### 3.1 `current_high_cache`

定位：当前高缓存模拟策略。

核心特点：

1. 使用本地 prompt-cache tracker 记录 session 级别的 cache profile。
2. 使用 `CacheSimulationPolicy` 控制目标读缓存比例。
3. 支持 `targetReadRatio`、`tokenScale`、`maxSimulatedInputTokens`、
   cap jitter 等参数。
4. 通过 `reportedUsage` 把最终返回给下游的 usage 调整为高缓存形态。

优势：

- 能稳定让 Claude Code 看到较高 cache read/cache creation。
- 参数较少，容易整体调高缓存效果。
- 现有 `/cc`、`/v1`、`/ha` 内置路径已经能使用。

主要问题：

- 目标比例驱动较强，不完全等价于“之前真的见过同一稳定片段”。
- 短请求或第一次请求也可能被 reported usage 调整出很大的
  `cache_creation_input_tokens`。
- 不适合表达更保守、可解释的 per-segment cache hit/miss。
- 对“多账号是否应该共享本地缓存投影”的控制不够精细。

适合继续保留的场景：

- 需要维持旧版行为兼容。
- 需要非常高的 cache 展示效果。
- 运营者明确接受“目标比例模拟”的路由。

### 3.2 `kiro_rs_tool`

定位：面向 Kiro-RS-Tool 风格的结构化缓存策略。

核心特点：

1. 从 tools、system、history、当前 user stable prefix 中构造 cache blocks。
2. 支持 `coverageRatio`、`maxCoverageTokens`、
   `incrementalCreateEnabled`、`maxNewCreationTokensPerRequest`。
3. 支持 `cacheCurrentUserStablePrefix` 和 current user prefix token cap。
4. 使用 `reportedInputMinTokens` / `reportedInputMaxTokens` 控制 cached input
   拆分后的下游 `input_tokens` 范围。
5. 成功请求后 commit，失败请求不 commit。

优势：

- 比 `current_high_cache` 更接近结构化 segment 缓存。
- 对 tools/system/history 更有针对性。
- 已经有独立参数模型。
- 已有 `compute_kiro_rs_tool_with_bounds(...)` 和
  `commit_kiro_rs_tool_success_with_bounds(...)` 这类接入形态。

主要问题：

- 策略语义偏专用，不适合作为普通 `/cc` 默认策略直接泛化。
- 当前 scope 主要围绕 session/route namespace，适合继续作为专用策略使用。
- 与 tool/cache_control 的策略语义耦合较强。

适合继续保留的场景：

- Kiro-RS-Tool 专用路由。
- 工具 schema 稳定、history 边界清晰、需要 tool 风格 cache projection 的路由。
- 需要小范围保留当前 Kiro-RS-Tool 行为的兼容入口。

### 3.3 新策略：`stable_segment_cache`

定位：普通 Anthropic/Claude Code 路由可用的稳定片段缓存策略。

设计目标：

1. 与 `current_high_cache` 和 `kiro_rs_tool` 平级。
2. 路由可通过 `cacheType` 选择。
3. 参数通过 `stableSegment` 独立配置。
4. 下游 usage 字段保持现状。
5. 计算依据从“目标比例”转为“已成功提交过的稳定片段 fingerprint”。
6. 复用既有 session/route namespace scope，不按模型或账号额外拆分。
7. 对短请求和首次请求更保守，避免无意义高缓存。

核心语义：

```text
同一 scope 下，只有之前成功请求提交过的稳定 segment fingerprint，
后续再次出现时才计入 cache_read_input_tokens。
本轮新出现且满足阈值的稳定 segment 计入 cache_creation_input_tokens。
```

## 4. `stable_segment_cache` 策略配置

### 4.1 配置位置

已扩展 `CachePolicyConfig`：

```rust
pub struct CachePolicyConfig {
    pub default: CacheRoutePolicyPatch,
    pub current_high_cache: CacheRoutePolicyPatch,
    pub kiro_rs_tool: CacheRoutePolicyPatch,
    pub stable_segment_cache: CacheRoutePolicyPatch,
    pub path_overrides: BTreeMap<String, CacheRoutePolicyPatch>,
}
```

JSON 形态：

```json
{
  "cachePolicy": {
    "currentHighCache": {},
    "kiroRsTool": {},
    "stableSegmentCache": {},
    "pathOverrides": {}
  }
}
```

配置合并仍走现有全局缓存策略框架，不新增另一套并行系统。有效策略的解析顺序是：

```text
历史全局 prompt-cache 默认值
-> cachePolicy.default
-> 被 cacheType 选中的策略模板：currentHighCache / kiroRsTool / stableSegmentCache
-> cachePolicy.pathOverrides 的最长前缀路由覆盖
```

因此参数分两类：

- 通用参数：`cacheType`、`routeNamespace`、`bounds`、`cachePoint`、`reportedUsage`，位于
  `CacheRoutePolicyPatch`，三种策略共用。
- 策略专属参数：`simulation` 属于 `current_high_cache`，`kiroRsTool` 属于
  `kiro_rs_tool`，`stableSegment` 属于 `stable_segment_cache`。

`routeNamespace` 是复用既有路由隔离开关，不是新策略私有字段。新策略的 scope
直接沿用现有 `PromptCacheScope`：`conversation_id + route_namespace`，不再按
model 或 credential 额外拆分。

### 4.2 新增策略参数

已新增：

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StableSegmentCachePolicy {
    pub coverage_ratio: f64,
    pub max_coverage_tokens: i32,
    pub min_total_input_tokens: i32,
    pub min_segment_tokens: i32,

    pub incremental_create_enabled: bool,
    pub max_new_creation_tokens_per_request: i32,

    pub include_tools: bool,
    pub include_system: bool,
    pub include_history: bool,
    pub history_message_limit: usize,

    pub honor_explicit_cache_control: bool,
    pub auto_cache_system: bool,
    pub auto_cache_tools: bool,
    pub auto_cache_history_message_ends: bool,

    pub cache_current_user_stable_prefix: bool,
    pub current_user_stable_prefix_max_tokens: i32,

    pub default_ttl_secs: u64,
    pub extended_ttl_secs: u64,

    pub reported_input_min_tokens: i32,
    pub reported_input_max_tokens: i32,
}
```

并新增 patch：

```rust
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StableSegmentCachePolicyPatch {
    pub coverage_ratio: Option<f64>,
    pub max_coverage_tokens: Option<i32>,
    pub min_total_input_tokens: Option<i32>,
    pub min_segment_tokens: Option<i32>,

    pub incremental_create_enabled: Option<bool>,
    pub max_new_creation_tokens_per_request: Option<i32>,

    pub include_tools: Option<bool>,
    pub include_system: Option<bool>,
    pub include_history: Option<bool>,
    pub history_message_limit: Option<usize>,

    pub honor_explicit_cache_control: Option<bool>,
    pub auto_cache_system: Option<bool>,
    pub auto_cache_tools: Option<bool>,
    pub auto_cache_history_message_ends: Option<bool>,

    pub cache_current_user_stable_prefix: Option<bool>,
    pub current_user_stable_prefix_max_tokens: Option<i32>,

    pub default_ttl_secs: Option<u64>,
    pub extended_ttl_secs: Option<u64>,

    pub reported_input_min_tokens: Option<i32>,
    pub reported_input_max_tokens: Option<i32>,
}
```

### 4.3 推荐默认值

```text
coverageRatio = 0.90
maxCoverageTokens = 120000
minTotalInputTokens = 4096
minSegmentTokens = 1024

incrementalCreateEnabled = true
maxNewCreationTokensPerRequest = 30000

includeTools = true
includeSystem = true
includeHistory = true
historyMessageLimit = 40

honorExplicitCacheControl = true
autoCacheSystem = true
autoCacheTools = true
autoCacheHistoryMessageEnds = true

cacheCurrentUserStablePrefix = false
currentUserStablePrefixMaxTokens = 0

defaultTtlSecs = 300
extendedTtlSecs = 3600

reportedInputMinTokens = 32
reportedInputMaxTokens = 256
```

### 4.4 参数含义

| 参数 | 含义 |
| --- | --- |
| `coverageRatio` | 本轮最多把多少比例的 input 视为 cache-covered |
| `maxCoverageTokens` | 本轮最多覆盖多少 token；`0` 表示不额外限制 |
| `minTotalInputTokens` | 请求总 input 低于该值时不产生 cache |
| `minSegmentTokens` | 稳定 segment 累计 token 低于该值时不建立 cache point |
| `incrementalCreateEnabled` | 已有 read 命中后，是否允许为新增稳定尾部继续 creation |
| `maxNewCreationTokensPerRequest` | 单请求最多新增多少 creation；`0` 表示不限制 |
| `includeTools` | tools schema 是否参与稳定 segment |
| `includeSystem` | system prompt 是否参与稳定 segment |
| `includeHistory` | 历史 messages 是否参与稳定 segment |
| `historyMessageLimit` | 最多纳入多少条历史 message；`0` 表示不限制 |
| `honorExplicitCacheControl` | 是否识别请求里的 `cache_control` TTL |
| `autoCacheSystem` | 没有显式 `cache_control` 时，是否自动缓存 system |
| `autoCacheTools` | 没有显式 `cache_control` 时，是否自动缓存 tools |
| `autoCacheHistoryMessageEnds` | 是否在历史 message 边界自动建立 cache point |
| `cacheCurrentUserStablePrefix` | 是否缓存当前 user 消息的稳定前缀 |
| `currentUserStablePrefixMaxTokens` | 当前 user 稳定前缀最多 token |
| `defaultTtlSecs` | 默认短 TTL |
| `extendedTtlSecs` | 长 TTL |
| `reportedInputMinTokens` | 下游 `input_tokens` 折算后的最小值 |
| `reportedInputMaxTokens` | 下游 `input_tokens` 折算后的最大值；`0` 表示不限制 |

### 4.5 校验规则

建议校验：

- `coverageRatio` 必须在 `0..=1`。
- token 类参数不能小于 `0`。
- `reportedInputMaxTokens > 0` 时，`reportedInputMinTokens <= reportedInputMaxTokens`。
- `defaultTtlSecs >= 1`。
- `extendedTtlSecs >= defaultTtlSecs`。
- `historyMessageLimit` 允许为 `0`，表示不限制。
- 新策略不提供 model/credential scope 参数；隔离边界固定为会话 + 路由 namespace。

## 5. 路由配置示例

### 5.1 保守替换 `/cc`

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc": {
        "cacheType": "stable_segment_cache",
        "routeNamespace": true,
        "stableSegment": {
          "coverageRatio": 0.9,
          "maxCoverageTokens": 120000,
          "minTotalInputTokens": 4096,
          "minSegmentTokens": 1024,
          "includeTools": true,
          "includeSystem": true,
          "includeHistory": true,
          "historyMessageLimit": 40,
          "incrementalCreateEnabled": true,
          "maxNewCreationTokensPerRequest": 30000,
          "reportedInputMinTokens": 32,
          "reportedInputMaxTokens": 128
        }
      }
    }
  }
}
```

语义：

- 同一个 Claude Code session 内可以产生 read。
- 默认按路由 namespace 隔离，避免不同路由互相读取。
- 短 prompt 不制造 cache。

### 5.2 团队共享缓存投影路由

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/dfcache/team-a": {
        "cacheType": "stable_segment_cache",
        "routeNamespace": true,
        "stableSegment": {
          "coverageRatio": 0.95,
          "maxCoverageTokens": 200000,
          "minTotalInputTokens": 2048,
          "reportedInputMinTokens": 32,
          "reportedInputMaxTokens": 256
        }
      }
    }
  }
}
```

语义：

- `/dfcache/team-a` 使用独立 route namespace。
- 适合明确需要共享 cache 展示效果的路由。

### 5.3 保留旧策略并列

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc": {
        "cacheType": "stable_segment_cache"
      },
      "/ha": {
        "cacheType": "current_high_cache"
      },
      "/kiro-tool": {
        "cacheType": "kiro_rs_tool"
      },
      "/na": {
        "cacheType": "no_cache"
      }
    }
  }
}
```

## 6. 计算模型

### 6.1 Segment 来源

`stable_segment_cache` 构造稳定片段时建议使用以下顺序：

1. request prelude：影响 token 结构的稳定请求元信息，例如 `tool_choice`。
2. tools schema。
3. system blocks。
4. historical messages。
5. optional current user stable prefix。

当前 user 完整消息默认不进入 cache，因为它通常是本轮新增问题；只有启用
`cacheCurrentUserStablePrefix` 且能提取稳定前缀时，才截取前缀参与。

### 6.2 Canonical fingerprint

每个 segment 都转换成 canonical JSON，再做 token count 和 hash：

```text
canonical = canonicalize_cache_value(segment)
fingerprint = sha256(canonical)
tokens = token::count_tokens(canonical)
```

lookup point 使用累计 token：

```text
point_1 = segment_1
point_2 = segment_1 + segment_2
point_3 = segment_1 + segment_2 + segment_3
...
```

### 6.3 Breakpoint 选择

候选 point 必须满足：

```text
total_input_tokens >= minTotalInputTokens
cumulative_tokens >= minSegmentTokens
cumulative_tokens <= maxCoverageTokens, if maxCoverageTokens > 0
cumulative_tokens <= total_input_tokens * coverageRatio
```

如果多个 point 满足条件，优先选择最大的可覆盖 point。

### 6.4 Hit / miss

在当前 scope 中查 fingerprint：

- 命中且未过期：计入 `cache_read_input_tokens`。
- 未命中：计入 `cache_creation_input_tokens`。
- 已命中旧前缀且本轮新增稳定尾部：同时产生 read + creation。

示例：

```text
第 1 轮：system 40k 首次出现
=> cache_creation_input_tokens = 40000
=> cache_read_input_tokens = 0

第 2 轮：同 session、同 route namespace 复用 system 40k
=> cache_creation_input_tokens = 0
=> cache_read_input_tokens = 40000

第 3 轮：复用 system 40k，并新增稳定历史 2k
=> cache_creation_input_tokens = 2000
=> cache_read_input_tokens = 40000
```

### 6.5 Commit 时机

只有成功请求 commit：

```text
status=success => update PromptCacheTracker
stream_error / upstream_error / timeout / client_disconnect => 不更新
```

这避免失败请求污染后续 cache read。

### 6.6 TTL 拆分

沿用现有 `cache_creation_5m_input_tokens` 和
`cache_creation_1h_input_tokens` 语义。

建议规则：

- 显式 `cache_control.ttl` 存在时尊重。
- 没有显式 TTL 且自动缓存时使用 `defaultTtlSecs`。
- 大于默认 TTL 的值归一为 `extendedTtlSecs`。
- 最终仍受 `PromptCacheBounds.entry_ttl` 限制。

## 7. Scope 设计

当前 `PromptCacheScope` 只有：

```rust
pub struct PromptCacheScope {
    pub conversation_id: String,
    pub route_namespace: Option<String>,
}
```

不新增字段，也不把 model、credential 或外部池 ID 拼进 namespace。stable segment
直接复用既有 scope 语义：

```text
conversation_id = stable conversation id
route_namespace = cachePolicy.pathOverrides 解析出的 route namespace
```

这样可以复用现有 HashMap key，不必迁移 tracker 结构，也避免策略配置面膨胀。

推荐默认 `/cc`：

```text
conversation_id = metadata.user_id 中的 session_id，或 fallback conversation id
route_namespace = /cc
```

推荐 `/dfcache/team-a`：

```text
conversation_id = session_id
route_namespace = /dfcache/team-a
```

## 8. 接入当前请求流程

### 8.1 `prepare_usage_context`

在 `prepare_usage_context` 阶段，已知：

- payload
- endpoint
- route policy
- request input token estimate
- resolved model 信息的一部分

但此时不一定已经知道最终 credential。

因此新策略在该阶段只做：

```text
build stable segment profile
保存 profile/policy 到 RequestUsageContext
```

不要在这里最终计算 usage。

### 8.2 `prepare_credential_usage_context`

在 provider 选中账号后，沿用 `prepare_usage_context` 已解析出的 route namespace
构造 scope：

```text
PromptCacheScope {
  conversation_id,
  route_namespace
}
```

然后：

```text
PromptCacheTracker::compute_stable_segment_with_bounds(...)
CacheSimulation::from_prompt_cache_split_input_with_reported_input_range(...)
usage_context.simulated_usage = Some(...)
usage_context.simulated_source = Some(UsageSource::LocalPromptCache)
```

调用方仍不感知。

### 8.3 成功记录后 commit

成功路径中：

```text
if cacheType == StableSegmentCache && request success:
    prompt_cache.commit_stable_segment_success_with_bounds(...)
```

失败路径不 commit。

### 8.4 流式 usage

流式 `message_start.usage` 可以用当前已计算出的 simulation 作为估算。

最终 `message_delta.usage` 继续用 final usage，为 authoritative usage。

字段仍是：

```text
input_tokens
cache_creation_input_tokens
cache_read_input_tokens
output_tokens
```

## 9. 建议代码改动清单

### 9.1 `src/model/config.rs`

新增：

- `StableSegmentCachePolicy`
- `StableSegmentCachePolicyPatch`
- default / normalized / validate / apply_to / is_empty / validate_raw
- `PromptCacheStrategyType::StableSegmentCache`
- `CachePolicyConfig.stable_segment_cache`
- `CacheRoutePolicyPatch.stable_segment`
- `CacheRoutePolicy.stable_segment`
- `stable_segment_cache_template(...)`
- `strategy_policy(...)` 分支
- `resolve_cache_policy_for_path(...)` namespace 分支

### 9.2 `src/anthropic/prompt_cache.rs`

新增：

- `StableSegmentPromptCachePlan`
- `stable_segment_cache_blocks(...)`
- `build_stable_segment_profile_for_model(...)`
- `compute_stable_segment_with_bounds(...)`
- `commit_stable_segment_success_with_bounds(...)`

尽量复用：

- `CacheBlock`
- `PromptCacheUsage`
- `PromptCacheBounds`
- `canonicalize_cache_value`
- `append_tool_block`
- `append_system_block`
- `append_message_blocks`
- `current_user_stable_prefix_text`
- bounds 淘汰逻辑

### 9.3 `src/anthropic/handlers.rs`

扩展：

- `RequestUsageContext` 增加 stable segment profile/plan 字段。
- `prepare_usage_context` 根据 `StableSegmentCache` 构建 profile。
- `prepare_credential_usage_context` 在 credential 已知后计算 simulation。
- 成功记录后 commit stable segment plan。

### 9.4 `src/anthropic/cache.rs`

原则上不需要新增下游字段。

复用：

- `CacheSimulation::from_prompt_cache_split_input_with_reported_input_range(...)`
- `CacheUsage::to_anthropic_usage_json()`

如现有 helper 不够，可只新增内部 helper，不改变输出 JSON 结构。

### 9.5 Admin / runtime config

需要让 runtime config API、UI 表单或 JSON 编辑能力识别：

```json
{
  "cacheType": "stable_segment_cache",
  "stableSegment": {}
}
```

如果 UI 暂时不支持表单，可以先通过 JSON runtime config 暴露。

## 10. 与三种策略的行为边界

| 行为 | `current_high_cache` | `kiro_rs_tool` | `stable_segment_cache` |
| --- | --- | --- | --- |
| 路由选择 | 支持 | 支持 | 支持 |
| 策略模板 | `currentHighCache` | `kiroRsTool` | `stableSegmentCache` |
| 参数模型 | `simulation` + `reportedUsage` | `kiroRsTool` | `stableSegment` |
| 主要依据 | target ratio | tool/system/history plan | observed stable segment fingerprint |
| 默认短请求 cache | 可能出现 | 通常较少 | 低于阈值直接 0；若系统提示、工具或 prompt steering 使稳定段超过阈值，仍可 creation |
| 首轮行为 | 可 creation，也可能被 reported usage 调整 | creation | creation |
| 二轮行为 | read | read | read |
| 新尾部 | 依策略和 profile | incremental creation | read + incremental creation |
| 账号隔离 | 不按账号隔离 | 不按账号隔离 | 不按账号隔离 |
| 模型隔离 | 可通过 route namespace 间接做 | 可通过 route namespace 间接做 | 可通过 route namespace 间接做；不自动按模型拆分 |
| 调用方字段 | 标准字段 | 标准字段 | 标准字段 |

## 11. 推荐迁移策略

### 阶段 1：只新增策略，不改默认

实现 `stable_segment_cache`，但保持内置默认不变。

运营者可通过配置显式启用：

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc": {
        "cacheType": "stable_segment_cache"
      }
    }
  }
}
```

### 阶段 2：本地真实账号回归

使用指定本地实例：

```text
127.0.0.1:19023
tmp/thinking-budget-local/config.json
```

验证：

- Sonnet 4.5 非流式短请求。
- Sonnet 4.5 长 system 第一轮。
- Sonnet 4.5 长 system 第二/三轮流式。
- tools schema 请求。
- 长历史请求。
- Haiku 4.5 两轮。
- 多账号轮转。
- 失败请求不 commit。

### 阶段 3：灰度切 `/cc`

把 `/cc` 从 `current_high_cache` 切到 `stable_segment_cache`。

观察：

- Claude Code usage 是否仍正常显示。
- `input_tokens` 是否不为 0。
- `cache_read_input_tokens` 是否只在二轮后出现。
- 是否没有短请求异常大 cache creation。
- dashboard 的 cache 统计是否可解释。

### 阶段 4：保留旧策略作为兼容 preset

保留：

- `/ha` 或显式高缓存路由使用 `current_high_cache`。
- Kiro-RS-Tool 专用路由使用 `kiro_rs_tool`。
- 普通 `/cc` 使用 `stable_segment_cache`。

## 12. 验收测试清单

### 配置测试

- `cacheType=stable_segment_cache` 可反序列化。
- `stableSegmentCache` template 可生效。
- `pathOverrides["/cc"].stableSegment` 能覆盖 template。
- 无效 ratio、负 token、min/max 输入范围错误会被拒绝。
- 未配置新字段时旧配置保持兼容。

### 策略计算测试

- 短请求低于 `minTotalInputTokens` 时 cache 为 0。
- 第一轮长 system 返回 creation，不返回 read。
- 第二轮相同 scope 返回 read，不返回 creation。
- 第三轮相同前缀加稳定尾部返回 read + creation。
- 换 credential 但同会话同路由时，继续复用 route scope cache。
- 换 model 但同会话同路由时，继续复用 route scope cache。
- `routeNamespace=true` 时不同路由不互相读。
- 失败请求不 commit，后续不误报 read。

### 协议测试

- `/cc/v1/messages` 非流式 usage 字段保持 Claude Code 标准。
- `/cc/v1/messages` 流式 `message_start.usage` 和 final
  `message_delta.usage` 字段保持 Claude Code 标准。
- 不新增调用方字段。
- 不暴露内部 credential、fallback pool、upstream pool 等私有术语。

### 真实账号测试

- 多账号可用池下，同 session 同路由 cache 行为稳定。
- 多 session 或不同路由不会互相报 read。
- Sonnet 4.5 / Haiku 4.5 都能工作。
- upstream raw cache 仍为 0 时，下游 reported usage 仍可按本地策略显示。

## 13. 实现状态与验证证据

### 13.1 已接入代码路径

本策略已经接入以下位置：

- `src/model/config.rs`：新增 `StableSegmentCachePolicy`、
  `StableSegmentCachePolicyPatch`、`PromptCacheStrategyType::StableSegmentCache`、
  `cachePolicy.stableSegmentCache`、`pathOverrides[*].stableSegment`。
- `src/anthropic/prompt_cache.rs`：新增 stable segment profile、compute 和
  success commit 路径。
- `src/anthropic/handlers.rs`：复用现有 `conversation_id + route_namespace` scope，
  并在成功响应后 commit；下游继续只返回标准 usage 字段。
- `src/external_pool.rs` 与 `src/external_pool/usage_projection.rs`：外部池使用同一
  policy，并复用相同 route namespace scope。

### 13.2 scope 组成

新策略的 cache scope 直接复用已有结构：

```text
conversation_id + namespace
```

其中 namespace 就是 route policy 解析得到的 `route_namespace`。它由通用
`routeNamespace` 控制，不包含 model、credential 或 external pool id。

```text
conversation_id = stable conversation id
route_namespace = /cc 或 /dfcache/team-a 等路由前缀
```

因此推荐 `/cc` 灰度配置：

```json
{
  "cacheType": "stable_segment_cache",
  "routeNamespace": true,
  "stableSegment": {}
}
```

### 13.3 C0 验证

已运行：

- `feature/tests/run-cargo-scoped.sh stable-segment-route-scope-fmt-apply -- cargo fmt`
- `feature/tests/run-cargo-scoped.sh stable-segment-route-scope-check3 -- bash -lc 'cargo fmt --check && cargo check && cargo test stable_segment -- --nocapture'`

结果：

- fmt 通过。
- check 通过。
- `stable_segment` 相关 4 个测试通过。

补充运行：

- `feature/tests/run-cargo-scoped.sh stable-segment-cache-mock-cache -- cargo test cache -- --nocapture`

该批次中 stable segment 与既有缓存相关用例大量通过，但最终命中已知测试夹具问题：
`dfcache-precommit-eventstream-fixture` 线程 stack overflow。该问题已有独立记录：
`feature/issues/runtime-stack-overflow-and-handler-future-size.md`。

### 13.4 mock upstream 端到端验证

本轮按专项例外启动了临时 `kiro-rs` 实例，原因是需要导入 mock 账号、覆盖上游
URL、切换 `/cc` 路由策略，避免污染项目固定实例 `127.0.0.1:19023` 的共享测试库和
真实账号状态。

运行资源：

- 冻结二进制：`/var/folders/9p/fpr69g_x7pz9_g386g1kfpnc0000gn/T//kiro-stable-segment-bin.wq46UO/kiro-rs`
- SHA-256：`016b220639359301c6cfe0c067b15edc8659023a4f87230f439f6df675e85c1f`
- 临时代理：`127.0.0.1:19131`
- mock upstream：`127.0.0.1:39091`
- 临时数据库：`kiro_stable_segment_mock_20261001`
- 临时配置：`tmp/stable-segment-mock-e2e/config.json`
- mock 账号：3 个 API-key 账号，均显式 `endpoint=ide`
- 验证后已停止临时代理与 mock upstream，并清理冻结二进制临时目录。

请求形态：

- 非流式短请求。
- 非流式长 system + tools schema + 多轮 history。
- 重复稳定上下文。
- 追加稳定历史尾部。
- 流式重复稳定上下文。
- 禁用账号 1 后切到账号 2，重复同一会话同一路由请求组。
- `/na` 与 `/v1` 路由对照。

关键结果：

| 场景 | credential | usage_source | input_tokens | cache_creation_input_tokens | cache_read_input_tokens |
| --- | ---: | --- | ---: | ---: | ---: |
| `/cc` 短请求会话首轮 | 1 | `local_prompt_cache` | 170 | 1064 | 0 |
| `/cc` 首轮稳定上下文 | 1 | `local_prompt_cache` | 79 | 3122 | 0 |
| `/cc` 二轮重复稳定上下文 | 1 | `local_prompt_cache` | 39 | 0 | 3164 |
| `/cc` 追加历史尾部 | 1 | `local_prompt_cache` | 70 | 284 | 3151 |
| `/cc` 流式重复稳定上下文 | 1 | `local_prompt_cache` | 93 | 0 | 3098 |
| 禁用账号 1 后短请求同会话 | 2 | `local_prompt_cache` | 170 | 0 | 1064 |
| 禁用账号 1 后首轮稳定上下文重放 | 2 | `local_prompt_cache` | 42 | 0 | 3159 |
| 禁用账号 1 后重复稳定上下文 | 2 | `local_prompt_cache` | 39 | 0 | 3164 |
| 禁用账号 1 后追加历史尾部 | 2 | `local_prompt_cache` | 119 | 0 | 3386 |
| 禁用账号 1 后流式重复稳定上下文 | 2 | `local_prompt_cache` | 93 | 0 | 3098 |
| `/na` 对照 | 2 | `upstream_metadata` | 1234 | 0 | 0 |
| `/v1` 对照 | 2 | `upstream_metadata` | 1234 | 0 | 0 |

结论：

- 上游 mock 每次返回固定 raw usage：`uncachedInputTokens=1234`、
  `cacheReadInputTokens=0`、`cacheWriteInputTokens=0`。
- `/cc` 下游 usage 由本地 stable segment 策略投影为标准字段。
- 首轮 creation，二轮 read，追加历史为 read + incremental creation。
- 流式 `message_start.usage` 与最终 `message_delta.usage` 均保持 Claude Code 标准字段。
- 新版 scope 不按账号隔离；禁用账号 1 后，同会话同路由继续读取已有 route scope cache。
- `/na` 和 `/v1` 对照没有接入新策略，未出现 stable segment cache read/creation。

注意：本轮临时配置将 `minTotalInputTokens` 降到 120 以便用较小 mock payload 验证 e2e。
短请求因 prompt steering/request prelude 也超过该阈值，出现了 creation。这不是推荐生产阈值；
生产默认仍是 `4096`，用于抑制短请求无意义 creation。

## 14. 风险与注意事项

### 14.1 本地 reported cache 不等于上游真实 cache

新策略仍然是本地 reported usage 策略，不代表 Kiro 上游真实 cache hit。

调用方不需要新增字段，但内部分析和文档口径必须避免把它描述为 upstream
cache。

### 14.2 scope 只表达本地路由隔离

新策略的 scope 只表达本地兼容层的会话与路由隔离，不试图模拟上游真实账号、
组织、模型或服务端内部 cache 边界。需要更强隔离时，应通过不同路由或
`routeNamespace=true` 的专用路径完成。

### 14.3 不要用新策略修正计费真值

新策略只影响下游 usage 展示和兼容字段，不应被当作真实计费依据。

如果现有统计路径会把 reported `billableInputTokens` 当真实成本，应另开专项
修正 billing 口径。本文只设计缓存策略，不扩大到计费系统重构。

### 14.4 默认切换要谨慎

`current_high_cache` 行为可能已经被某些运营配置依赖。新增策略第一版不应直接
改变内置默认；应通过 path override 灰度。

## 15. 推荐结论

推荐新增第三个并列策略：

```text
stable_segment_cache
```

它的定位是普通 Claude Code / Anthropic 兼容路由的稳健本地缓存投影策略。

它应当：

1. 接入 `CachePolicyConfig`。
2. 支持 template 和 path override。
3. 使用独立 `stableSegment` 参数。
4. 复用 `PromptCacheTracker`、`PromptCacheUsage`、`CacheSimulation`、
   `CacheUsage`。
5. 默认按 session/route namespace 隔离。
6. 基于稳定 segment fingerprint，而不是单纯 target ratio。
7. 对调用方保持完全相同的标准 usage 字段。

推荐迁移方向：

```text
/cc       -> stable_segment_cache
/v1       -> stable_segment_cache 或 current_high_cache，按运营目标选择
/ha       -> current_high_cache，作为高缓存展示兼容路由
/kiro-tool -> kiro_rs_tool
/na       -> no_cache
```

这样三种缓存方案各自有清晰职责：

- `current_high_cache`：高缓存展示兼容。
- `kiro_rs_tool`：Kiro-RS-Tool 专用结构化策略。
- `stable_segment_cache`：普通路由默认的稳健稳定片段缓存策略。
