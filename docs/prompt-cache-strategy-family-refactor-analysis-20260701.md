# Prompt Cache 策略族参数化与模块抽离改造分析

日期：2026-07-01

范围：仅分析和方案设计，不做实现代码修改。

这份文档修正前一版分析里的一个重点：不是“当前策略参数化，`Kiro-RS-Tool` 策略硬编码”。正确方向应该是：

- 所有缓存策略都参数化。
- 当前策略是策略族 A。
- `Kiro-RS-Tool` 是策略族 B。
- A-1、A-2、B-1、B-2 是同一策略族下不同参数实例或 preset。
- 路径只绑定“某个策略实例”，而不是直接散落一堆参数。

---

## 1. 用户目标用人话重述

目标不是简单多加几个配置字段，而是把 prompt-cache 从“散参数驱动”改成“策略族 + 策略实例 + 路径绑定”。

期望结果：

1. 缓存模块先抽离清楚，后面再加策略不会继续堆在 `prompt_cache.rs`、`handlers.rs`、`external_pool.rs` 里。
2. 当前策略继续存在，但它也要修正明显问题：
   - 第一次请求不能对外显示 cache read。
   - 缓存 scope 改成基于会话，不考虑凭证、模型等维度。
3. `Kiro-RS-Tool` 作为另一种策略族加入。
4. 所有策略族都有自己的参数模型。
5. 路由只选择策略实例，比如：
   - `/cc/v1/messages` 用 `B-1`
   - `/v1/messages` 用 `A-1`
   - `/test/cache-a2` 用 `A-2`
6. 旧配置和旧路径不能被意外破坏；如果必须改默认行为，要把改动点说清楚，并有测试兜底。

---

## 2. 当前代码事实

### 2.1 当前缓存模块职责混在一个文件里

当前 `src/anthropic/prompt_cache.rs` 同时承担：

- scope 定义。
- profile 构建。
- cache block flatten。
- canonical JSON。
- TTL 解析。
- tracker 内存表。
- hit/miss 计算。
- bounds 淘汰。
- current high-cache 的目标比例计算。

证据：

- `src/anthropic/prompt_cache.rs:24`：`PromptCacheScope`。
- `src/anthropic/prompt_cache.rs:128`：`PromptCacheTracker` 内存表。
- `src/anthropic/prompt_cache.rs:161`：profile 构建。
- `src/anthropic/prompt_cache.rs:240`：`compute_with_bounds(...)`。
- `src/anthropic/prompt_cache.rs:324`：`update_with_bounds(...)`。
- `src/anthropic/prompt_cache.rs:494`：`flatten_cache_blocks(...)`。
- `src/anthropic/prompt_cache.rs:653`：`target_cache_tokens(...)`。

这就是为什么现在继续加策略会越来越乱：核心存储、算法、配置语义都挤在同一个模块。

### 2.2 当前路径级策略能力已经有雏形

当前项目已有：

- `CachePolicyConfig.default`
- `CachePolicyConfig.path_overrides`
- `resolve_cache_policy_for_path(...)`
- 最长路径前缀匹配
- path override 影响缓存状态时自动设置 namespace

证据：

- `src/model/config.rs:1195`：`CachePolicyConfig`。
- `src/model/config.rs:1269`：`resolve_cache_policy_for_path(...)`。
- `src/model/config.rs:1283`：最长前缀匹配。
- `src/model/config.rs:1289`：影响缓存状态时设置 namespace。

这套能力可以复用，但不建议继续把所有配置都塞进 `pathOverrides` 里的 `simulation`。

### 2.3 当前策略 A 是“目标比例 high-cache 模拟”

当前 `CacheSimulationPolicy` 参数：

- `enabled`
- `targetReadRatio`
- `tokenScale`
- `maxSimulatedInputTokens`
- `capJitterMinTokens`
- `capJitterMaxTokens`
- `scaleMinInputTokens`

证据：`src/model/config.rs:817`。

默认值：

- `targetReadRatio = 0.98`
- `tokenScale = 1.6`
- `maxSimulatedInputTokens = 300000`
- jitter 12000 到 24000
- `scaleMinInputTokens = 20000`

证据：`src/model/config.rs:2479`。

这不是单纯“真实前缀缓存”。它是一种策略族，可以命名为：

```text
策略族 A：weighted_high_cache / current_weighted
```

它的核心是：

1. 用本地前缀 fingerprint 做 hit/miss。
2. 用 `targetReadRatio` 控制最终缓存 token 目标量。
3. 用 tokenScale / cap / jitter 放大或限制 total input。
4. 用 reportedUsage 控制对下游显示的 usage。
5. 用 creation control 抑制 cache creation 上报。

### 2.4 当前第一次“可能出现 cache read”的根因

底层 `compute_with_bounds(...)` 首次 miss 时返回 `cache_read_input_tokens = 0`。

证据：`src/anthropic/prompt_cache.rs:269`。

但是 reported usage 会把 input 压低后的差值搬进 `cache_read_input_tokens`：

- `src/anthropic/cache.rs:136`
- `src/anthropic/cache.rs:139`
- `src/anthropic/cache.rs:140`

`/cc` 默认 reported usage 使用 `sample_input_max(96)`，而这个 helper 默认开启 `move_delta_to_cache_read`。

证据：

- `src/model/config.rs:435`
- `src/model/config.rs:616`

所以这不是 tracker 命中了缓存，而是显示层把 input delta 伪装成了 cache read。

### 2.5 当前 scope 过宽

当前 scope 是：

```rust
credential_id + conversation_id + model + route_namespace
```

证据：`src/anthropic/prompt_cache.rs:24`。

普通路径构造 scope 的地方：

- `src/anthropic/handlers.rs:3256`

external pool 构造 scope 的地方：

- `src/external_pool.rs:4406`

这会导致：

- 同一会话换凭证不命中。
- 同一会话换模型不命中。
- 不同 route namespace 不命中。

你这次明确要求：当前缓存策略也要改成“基于会话做缓存，不考虑凭证、模型等”。所以 scope 需要收敛，不只是 `Kiro-RS-Tool` 策略要改。

---

## 3. 核心设计原则

### 3.1 策略族和策略实例分开

不要把“策略”理解成一个固定行为。

建议分成两层：

```text
策略族 family：算法类型
策略实例 instance：该算法类型的一组参数
```

例如：

```text
A = current_weighted
A-1 = current_weighted + 默认参数
A-2 = current_weighted + 更保守参数
A-test = current_weighted + 小 bounds + raw usage

B = kiro_rs_tool
B-1 = kiro_rs_tool + 默认参数
B-2 = kiro_rs_tool + 更小容量 + 关闭动态 system 跳过
B-test = kiro_rs_tool + 低 TTL + 测试容量
```

这样后面再加策略 C，不会继续污染 A/B 的参数。

### 3.2 所有策略族都参数化

策略 B 不能硬编码。

`Kiro-RS-Tool` 风格也应该有参数，例如：

- scope 使用 session-only 还是 session-then-client-key。
- 是否跳过动态 system prelude。
- 是否把 model 放入 hash。
- 是否使用 route namespace。
- 容量上限。
- TTL 上限。
- 是否持久化。
- 是否启用严格 first-miss guard。

只是这些参数和策略 A 的 `targetReadRatio/tokenScale` 不是同一类参数。

### 3.3 共同能力抽到公共层，策略差异留在策略层

可复用的公共能力：

- canonical JSON。
- token 估算。
- TTL 解析。
- scope key 类型。
- tracker 存储。
- bounds 淘汰。
- usage 三项自洽校验。

不应该混用的策略能力：

- A 的 `targetReadRatio` 不应该进入 B。
- A 的 token amplification 不应该进入 B。
- B 的动态 system 跳过逻辑不应该无条件影响 A，除非 A 明确配置开启。
- B 的 session-then-client-key 不应该无条件影响 A，除非 A 的 scope 参数选择它。

### 3.4 先抽离模块，再做行为替换

这次可以先把缓存模块抽离出来。原因是：

- 当前代码已经把存储、profile、策略混在一起。
- 直接在现有文件里加 B 策略，会让 `prompt_cache.rs` 更难维护。
- 先抽离可以降低后面实现 A/B 策略时的冲突。

但抽离要“机械搬迁优先”，不要一边搬一边改算法。

建议顺序：

1. 机械抽离模块，行为不变。
2. 加策略族/实例配置模型，默认仍映射到当前行为。
3. 修 first-read 不变量。
4. 改当前策略 scope 为 session-only。
5. 加 B 策略。

---

## 4. 推荐模块结构

建议把当前 `src/anthropic/prompt_cache.rs` 拆成目录模块：

```text
src/anthropic/prompt_cache/
  mod.rs
  types.rs
  scope.rs
  profile.rs
  canonical.rs
  tracker.rs
  bounds.rs
  usage.rs
  strategies/
    mod.rs
    current_weighted.rs
    kiro_rs_tool.rs
```

### 4.1 `types.rs`

放通用类型：

- `PromptCacheUsage`
- `PromptCacheProfile`
- `PromptCacheBreakpoint`
- `PromptCacheLookupPoint`
- `PromptCacheEntry`
- `PromptCacheFingerprint`

注意：目前 `PromptCacheLookupPoint` 是私有类型。策略 B 如果要复用 tracker，lookup point 需要在 prompt_cache 模块内部公共，至少 `pub(super)`。

### 4.2 `scope.rs`

放 scope 相关：

- `PromptCacheScope`
- `PromptCacheScopePolicy`
- `PromptCacheScopeParts`
- `ClientKeySeed`
- session id 提取。
- scope resolver。

建议改造 `PromptCacheScope`，不要继续固定四字段：

当前：

```rust
pub struct PromptCacheScope {
    pub credential_id: u64,
    pub conversation_id: String,
    pub model: String,
    pub route_namespace: Option<String>,
}
```

建议改成更通用：

```rust
pub struct PromptCacheScope {
    pub namespace: Option<String>,
    pub identity: PromptCacheScopeIdentity,
}
```

其中：

```rust
pub enum PromptCacheScopeIdentity {
    Session(String),
    SessionClientKey { session: String, client_key_seed: u64 },
    ClientKey(u64),
    Legacy {
        credential_id: u64,
        conversation_id: String,
        model: String,
    },
}
```

但是考虑改动面，第一阶段也可以保留原结构，用 sentinel 填字段：

```text
credential_id = 0
model = "*"
route_namespace = None 或策略 namespace
conversation_id = session id
```

不推荐长期这样做，因为字段名会骗人。短期可以降低改动风险，长期应改为语义化结构。

### 4.3 `profile.rs`

放 profile 构建公共框架：

- `PromptCacheProfileBuilder`
- block flatten 策略接口。
- TTL breakpoint 处理。
- lookup point 生成。

建议引入：

```rust
pub enum PromptCacheProfileFamily {
    CurrentWeighted,
    KiroRsTool,
}
```

或者更直接：

```rust
pub trait PromptCacheProfileBuilder {
    fn build(&self, req: &MessagesRequest, total_input_tokens: i32, model: &str) -> Option<PromptCacheProfile>;
}
```

第一版建议用 enum + match，不建议 trait object。Rust 里 enum 更直观，测试也更好写。

### 4.4 `canonical.rs`

放：

- `canonicalize_cache_value(...)`
- `write_canonical_json(...)`
- volatile id 过滤。
- position key 过滤。
- billing header 过滤。

这部分 A/B 都能复用。

### 4.5 `tracker.rs`

只负责存储和命中：

```rust
pub struct PromptCacheTracker {
    entries: Mutex<HashMap<PromptCacheScope, HashMap<Fingerprint, PromptCacheEntry>>>,
}
```

建议 tracker 的接口从“带策略参数”改成“纯命中/写入”：

```rust
pub fn lookup(
    &self,
    scope: Option<&PromptCacheScope>,
    points: &[PromptCacheLookupPoint],
    bounds: PromptCacheBounds,
) -> PromptCacheLookupResult

pub fn commit(
    &self,
    scope: Option<PromptCacheScope>,
    entries: Vec<PromptCacheCommitPoint>,
    bounds: PromptCacheBounds,
)
```

不要让 tracker 接收 `targetReadRatio`。`targetReadRatio` 是策略 A 的参数，不是 tracker 的公共能力。

当前 `compute_with_bounds(...)` 里混了：

- 查缓存。
- 算 target ratio。
- 算 creation/read。

建议拆到策略 A。

### 4.6 `bounds.rs`

放：

- `PromptCacheBounds`
- `CacheBoundsPolicy` 转换。
- entry ttl 限制。
- global/per-scope 淘汰。
- estimated bytes limit。

当前 bounds 已经比较完整，应该复用。

### 4.7 `usage.rs`

放缓存 usage 的共同不变量：

- `input + creation + read == total`
- first miss 不允许 read。
- cache read 不能凭空生成。
- reported usage 不能破坏底层 read 状态。

建议增加一个明确类型：

```rust
pub struct PromptCacheAccounting {
    pub input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_read_input_tokens: i32,
}
```

并提供：

```rust
pub fn assert_or_normalize(self, total_input_tokens: i32) -> Self
```

不要把所有账本规则散在 `cache.rs`、`prompt_cache.rs`、`handlers.rs`。

### 4.8 `strategies/current_weighted.rs`

策略 A。

参数模型：

```rust
pub struct CurrentWeightedCacheParams {
    pub enabled: bool,
    pub target_read_ratio: f64,
    pub token_scale: f64,
    pub max_simulated_input_tokens: i32,
    pub cap_jitter_min_tokens: i32,
    pub cap_jitter_max_tokens: i32,
    pub scale_min_input_tokens: i32,
    pub scope: PromptCacheScopePolicy,
    pub creation_control: PromptCacheCreationControlConfig,
    pub reported_usage: ReportedUsagePathPolicy,
    pub bounds: CacheBoundsPolicy,
}
```

当前策略修正后的 scope 默认：

```text
session-only
```

也就是说 A 的默认参数要变成：

```rust
scope = PromptCacheScopePolicy::SessionOnly
```

而不是当前的 credential + session + model + route。

注意：这会改变当前缓存命中范围。既然用户明确要求“不考虑凭证、模型等”，这里应写成目标行为，同时在实施时用测试保护。

### 4.9 `strategies/kiro_rs_tool.rs`

策略 B。

参数模型建议：

```rust
pub struct KiroRsToolCacheParams {
    pub enabled: bool,
    pub scope: KiroRsToolScopePolicy,
    pub include_model_in_hash: bool,
    pub include_tool_choice_in_hash: bool,
    pub include_route_namespace: bool,
    pub skip_dynamic_system_before_cache_control: bool,
    pub commit_on_success_only: bool,
    pub bounds: CacheBoundsPolicy,
    pub persistence: PromptCachePersistencePolicy,
}
```

默认 B-1：

```text
enabled = true
scope = session_then_client_key
include_model_in_hash = false
include_tool_choice_in_hash = false
include_route_namespace = false 或 true，需要按部署风险决定
skip_dynamic_system_before_cache_control = true
commit_on_success_only = true
persistence = disabled first version
```

为什么 persistence 第一版建议 disabled：

- 当前项目没有本地 prompt cache 持久化。
- 直接加 JSON 落盘会引入文件大小、并发写、schema 版本、加载耗时等问题。
- 用户现在主要目标是策略和 scope，不是重启后保持命中。

---

## 5. 配置模型建议

### 5.1 为什么不建议继续只用 `cachePolicy.pathOverrides`

当前形态：

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc": {
        "simulation": {
          "targetReadRatio": 0.98
        }
      }
    }
  }
}
```

问题：

- 看不出这是策略 A 还是 B。
- 参数和路径强绑定，不能复用。
- A/B 参数不同，继续塞在 `simulation` 会很别扭。
- 想配置 A-1/A-2/B-1/B-2 时会重复大量 JSON。

### 5.2 推荐新配置：策略实例注册表

建议新增：

```json
{
  "promptCacheStrategies": {
    "defaultA": {
      "family": "current_weighted",
      "preset": "default",
      "params": {
        "scope": {
          "mode": "session_only"
        },
        "targetReadRatio": 0.98,
        "tokenScale": 1.6,
        "maxSimulatedInputTokens": 300000,
        "capJitterMinTokens": 12000,
        "capJitterMaxTokens": 24000,
        "scaleMinInputTokens": 20000
      }
    },
    "toolB": {
      "family": "kiro_rs_tool",
      "preset": "default",
      "params": {
        "scope": {
          "mode": "session_then_client_key"
        },
        "skipDynamicSystemBeforeCacheControl": true,
        "commitOnSuccessOnly": true,
        "includeModelInHash": false,
        "includeToolChoiceInHash": false
      }
    },
    "toolBTest": {
      "family": "kiro_rs_tool",
      "preset": "default",
      "params": {
        "scope": {
          "mode": "session_then_client_key"
        },
        "bounds": {
          "maxEntriesGlobal": 512
        }
      }
    }
  },
  "cachePolicy": {
    "defaultStrategy": "defaultA",
    "pathStrategies": {
      "/cc/v1/messages": "toolB",
      "/test/kiro-cache": "toolBTest"
    }
  }
}
```

优点：

- 策略实例可以复用。
- 路由绑定更清楚。
- A/B 参数可以不同。
- 测试策略 B-2 不需要污染生产策略 B-1。

### 5.3 兼容旧配置

不能一次性废弃当前配置。

兼容规则建议：

1. 如果没有 `promptCacheStrategies`，按旧配置解析。
2. 旧配置解析成一个隐式策略实例：

```text
__legacy_default => family=current_weighted, params=旧 CacheSimulationPolicy + reportedUsage + creationControl + bounds
```

3. 如果配置了 `cachePolicy.pathOverrides`，继续生效。
4. 如果同一路径同时配置新 `pathStrategies` 和旧 `pathOverrides`：
   - 新 `pathStrategies` 优先。
   - 旧 `pathOverrides` 可以作为附加 patch，但必须明确规则，避免双重覆盖。

更保守建议：

```text
同一路径不允许同时出现 pathStrategies 和 pathOverrides，启动时报配置错误。
```

这样最清楚。

### 5.4 preset 和 params 的关系

每个 family 有自己的 preset。

例如策略 A：

```text
current_weighted/default
current_weighted/conservative
current_weighted/raw_reporting
current_weighted/test_small_bounds
```

策略 B：

```text
kiro_rs_tool/default
kiro_rs_tool/session_only
kiro_rs_tool/session_then_client_key
kiro_rs_tool/test_small_bounds
```

解析规则：

1. 先加载 family preset 默认参数。
2. 再应用 `params` 覆盖。
3. 再 validate。

这样 A-1/A-2/B-1/B-2 都是普通配置，不需要新增 enum variant。

---

## 6. 当前策略 A 必须修的问题

### 6.1 first-read 伪读修复

目标：

```text
底层没有 cache read 时，reported usage 不允许生成 cache read。
```

建议改法：

在 `CacheUsage::with_reported_cache_usage_policy_and_raw(...)` 里记录原始本地缓存是否真的读过：

```rust
let had_real_cache_read = self.cache_read_input_tokens > 0;
```

只有 `had_real_cache_read` 为 true，才允许：

```rust
input_delta -> cache_read_input_tokens
```

同样修 `ReportedCacheUsagePolicy::apply_final_input_guard(...)`。

为什么要两个地方都修：

- `with_reported_cache_usage_policy_and_raw(...)` 是主 reported usage 路径。
- `apply_final_input_guard(...)` 是额外 guard 路径。
- 两边只修一边，仍可能漏出 first-read 伪读。

### 6.2 当前策略 A 改成 session-only scope

目标：

```text
同一个 session 下，不因为 credential/model/route 变化而 miss。
```

你明确说：“不考虑凭证，模型等，现在的是能够选择凭证，会话，模型，这些通通不需要。”

因此策略 A 的 scope 应默认：

```text
SessionOnly
```

推荐语义：

```rust
PromptCacheScopePolicy::SessionOnly {
    session_source: StableConversationId,
}
```

当前项目有 `extract_stable_conversation_id(payload)`：

- 优先取 `metadata.user_id`。
- 否则用 `system + tools + first_user_message` 派生确定性 UUID。

证据：当前 handlers 里已经把它传成 `stable_conversation_id`，见 `src/anthropic/handlers.rs:3171`。

策略 A 可以继续使用这个 stable conversation id，因为它是当前项目已有行为。

策略 B 如果要严格对齐 `Kiro-RS-Tool`，则不应该使用这个 fallback，而应使用 `_session_` 或 client key fallback。

### 6.3 route namespace 是否保留

你这次说“不考虑凭证、模型等”，没有明确说是否也不考虑 route。

当前 `route_namespace` 是为了避免不同 path override 的缓存状态互相污染。

建议分两层：

1. cache identity 不考虑 route。
2. strategy namespace 默认保留，用于避免不同策略实例共享同一个桶。

也就是说：

```text
同一个策略实例内：按 session 共享
不同策略实例之间：默认隔离
```

这样既满足“不考虑凭证、模型”，又避免 A/B 策略互相污染。

配置可参数化：

```json
{
  "scope": {
    "mode": "session_only",
    "includeStrategyNamespace": true
  }
}
```

默认建议：

```text
includeStrategyNamespace = true
```

因为 A 和 B 的账本算法不同，默认共享同一个缓存桶风险很高。

---

## 7. 策略 B：Kiro-RS-Tool 参数化设计

### 7.1 策略 B 的算法目标

策略 B 不是 A 的参数组合。

它的核心行为：

1. 按前缀段链计算。
2. 第一次 miss：read = 0，covered prefix 算 creation。
3. 第二次相同前缀：最深命中段算 read。
4. 成功后才 commit。
5. 不做 targetReadRatio。
6. 不做 tokenScale。
7. 不做 reportedUsage 采样。
8. 可选支持 `Kiro-RS-Tool` 的动态 system 跳过。

### 7.2 策略 B 参数

建议：

```rust
pub struct KiroRsToolStrategyParams {
    pub enabled: bool,
    pub scope: KiroRsToolScopeParams,
    pub fingerprint: KiroRsToolFingerprintParams,
    pub accounting: KiroRsToolAccountingParams,
    pub bounds: CacheBoundsPolicy,
    pub persistence: PromptCachePersistencePolicy,
}
```

#### scope 参数

```rust
pub struct KiroRsToolScopeParams {
    pub mode: KiroRsToolScopeMode,
    pub include_strategy_namespace: bool,
}

pub enum KiroRsToolScopeMode {
    SessionThenClientKey,
    SessionOnly,
    ClientKeyOnly,
}
```

默认：

```text
SessionThenClientKey
```

#### fingerprint 参数

```rust
pub struct KiroRsToolFingerprintParams {
    pub skip_dynamic_system_before_cache_control: bool,
    pub include_model: bool,
    pub include_tool_choice: bool,
    pub ignore_volatile_ids: bool,
}
```

默认：

```text
skip_dynamic_system_before_cache_control = true
include_model = false
include_tool_choice = false
ignore_volatile_ids = true
```

#### accounting 参数

```rust
pub struct KiroRsToolAccountingParams {
    pub split_against_total: bool,
    pub commit_on_success_only: bool,
    pub forbid_first_miss_read: bool,
}
```

默认：

```text
split_against_total = true
commit_on_success_only = true
forbid_first_miss_read = true
```

#### persistence 参数

```rust
pub struct PromptCachePersistencePolicy {
    pub enabled: bool,
    pub path: Option<String>,
    pub flush_interval_secs: u64,
    pub max_file_bytes: u64,
}
```

默认第一版：

```text
enabled = false
```

理由：

- 当前项目没有本地缓存持久化。
- 直接引入落盘可能带来文件增长、并发写、schema 版本、启动加载耗时。
- 可以作为后续 B-2/B-persistent preset。

### 7.3 策略 B preset 示例

```json
{
  "promptCacheStrategies": {
    "B-1": {
      "family": "kiro_rs_tool",
      "preset": "default"
    },
    "B-2": {
      "family": "kiro_rs_tool",
      "preset": "default",
      "params": {
        "scope": {
          "mode": "session_only"
        },
        "bounds": {
          "maxEntriesGlobal": 4096
        }
      }
    },
    "B-persistent-test": {
      "family": "kiro_rs_tool",
      "preset": "default",
      "params": {
        "persistence": {
          "enabled": true,
          "path": "cache_dir/prompt_cache_kiro_tool.json",
          "flushIntervalSecs": 60,
          "maxFileBytes": 10485760
        }
      }
    }
  }
}
```

---

## 8. 路径绑定设计

### 8.1 新设计

推荐：

```json
{
  "cachePolicy": {
    "defaultStrategy": "A-1",
    "pathStrategies": {
      "/cc/v1/messages": "B-1",
      "/v1/messages": "A-1",
      "/debug/a2": "A-2",
      "/debug/b2": "B-2"
    }
  }
}
```

### 8.2 与旧 `pathOverrides` 的关系

旧配置继续支持，但它属于 legacy patch 模型。

建议规则：

1. `pathStrategies` 是新模型。
2. `pathOverrides` 是旧模型。
3. 同一路径不能同时配置两者。
4. 如果都配置，启动时报错，不做隐式合并。

原因：

- 两套模型合并规则太容易让人误解。
- 用户希望策略纯粹，不能让 B 策略偷偷继承 A 的 reportedUsage 或 targetReadRatio。

### 8.3 策略 namespace

当前 namespace 是按 path override prefix 设置。

新设计建议 namespace 按策略实例设置：

```text
namespace = strategy instance id
```

例如：

```text
/cc/v1/messages -> B-1 -> namespace "strategy:B-1"
/debug/b2 -> B-2 -> namespace "strategy:B-2"
```

这样：

- 同一个策略实例绑定多个路径，可以共享缓存。
- 不同策略实例默认隔离。
- 不再由路径前缀本身决定是否隔离。

这比当前“path override 影响状态就 namespace=prefix”更符合策略实例模型。

---

## 9. 实施顺序建议

### 阶段 0：补测试，锁住当前事实

先加测试，不改行为：

- 当前 first miss 底层 read=0。
- 当前 reportedUsage 会制造 first-read。
- 当前 scope 包含 credential/model。
- 当前 path override 最长前缀匹配。

这些测试可以先写成当前行为测试，其中 first-read reportedUsage 测试标注为待修复，或者直接在修复 PR 中改成目标断言。

### 阶段 1：机械抽离模块

只搬代码，不改行为。

目标结构：

```text
prompt_cache/
  mod.rs
  types.rs
  canonical.rs
  profile.rs
  tracker.rs
  bounds.rs
```

此阶段不引入 A/B。

验证：

- `cargo fmt --check`
- `cargo test`
- 现有 prompt_cache 测试全部通过。

### 阶段 2：引入策略执行层，但只接入 A

新增：

```text
strategies/current_weighted.rs
strategies/mod.rs
```

把现有逻辑搬成策略 A。

外部接口从：

```rust
build_high_cache_profile_for_model(...)
compute_with_bounds(...)
CacheSimulation::from_prompt_cache_with_ratio_and_amplification(...)
```

逐步收敛成：

```rust
PromptCacheStrategyExecutor::compute(...)
PromptCacheStrategyExecutor::commit_success(...)
```

此阶段 A 行为仍保持不变。

### 阶段 3：修 first-read 伪读

改 `cache.rs` reported usage 逻辑。

目标不变量：

```text
如果策略执行结果 read=0，reported usage 不允许把 read 改成 >0。
```

测试：

- first miss + `/cc` reportedUsage => read 仍为 0。
- second hit read > 0 => 允许 reportedUsage 基于真实 read 做整形。

### 阶段 4：策略 A scope 改为 session-only

把 A 的默认 scope 参数改为：

```text
SessionOnly
```

并移除 A scope 对 credential/model 的依赖。

代码影响点：

- `CredentialUsageContext::scope(...)`
- `prepare_credential_usage_context(...)`
- `external_prompt_cache_scope(...)`

建议不要在这些函数里手写 scope，而是统一调用：

```rust
strategy.resolve_scope(...)
```

测试：

- 同 session、不同 credential 命中。
- 同 session、不同 model 命中。
- 不同 session 不命中。
- external pool 和普通路径 scope 规则一致。

### 阶段 5：配置模型引入 strategy registry

新增配置结构：

```rust
pub struct PromptCacheStrategiesConfig {
    pub strategies: BTreeMap<String, PromptCacheStrategyConfig>,
}

pub struct PromptCacheStrategyConfig {
    pub family: PromptCacheStrategyFamily,
    pub preset: Option<String>,
    pub params: serde_json::Value,
}
```

为了强类型，最终可以改成 tagged enum：

```rust
#[serde(tag = "family", rename_all = "snake_case")]
pub enum PromptCacheStrategyConfig {
    CurrentWeighted(CurrentWeightedStrategyConfig),
    KiroRsTool(KiroRsToolStrategyConfig),
}
```

第一版建议直接用 tagged enum，validate 更安全。

### 阶段 6：实现策略 B

实现 `kiro_rs_tool`。

复用：

- tracker。
- canonical。
- TTL。
- bounds。

不复用：

- A 的 targetReadRatio。
- A 的 tokenScale。
- A 的 creation control。
- A 的 reported usage 采样。

### 阶段 7：真实验证

实现后必须跑：

- 单元测试。
- 直接 `/cc/v1/messages` 协议测试。
- Claude Code CLI 真实测试。
- 内存/并发测试。

---

## 10. 风险与边界

### 10.1 当前策略 A 改 session-only 是行为变化

这会让：

- 同 session 跨 credential 命中。
- 同 session 跨 model 命中。

这是用户明确目标，但仍属于行为变化。需要：

- 测试覆盖。
- 文档说明。
- 最好在配置中仍允许恢复旧 scope：

```json
{
  "scope": {
    "mode": "legacy_credential_conversation_model"
  }
}
```

这样出现问题时能回滚。

### 10.2 同 session 跨 model 命中可能和官方行为不完全一致

Claude Code 官方说明 model 是 cache key 的一部分。

但用户明确要求“不考虑模型”。因此实现应按用户目标走，同时文档标注：

```text
session-only 是产品策略，不是官方 Claude cache key 的完整复刻。
```

### 10.3 内存风险

scope 从 credential+model 收敛到 session-only 后，单个 session 下缓存条目会集中。

必须保留：

- `maxEntriesGlobal`
- 每 scope 上限，或者改名为 `maxEntriesPerScope`
- estimated bytes limit
- TTL

建议把 `maxEntriesPerAccount` 改名或兼容映射为：

```text
maxEntriesPerScope
```

因为 session-only 后 “account” 这个词不准确。

### 10.4 策略实例共享缓存的风险

如果 A-1 和 B-1 共用同一个 session scope，会污染。

建议默认：

```text
scope namespace = strategy instance id
```

除非显式配置共享。

---

## 11. 最终建议

推荐方案：

1. 先抽离缓存模块，但第一阶段只机械搬迁，不改行为。
2. 把当前策略定义成策略族 A：`current_weighted`。
3. 引入策略实例注册表，让 A-1/A-2/B-1/B-2 都是配置实例。
4. 修复 first-read 伪读，作为全局不变量。
5. 把策略 A 默认 scope 改为 session-only，同时保留 legacy scope 参数用于回滚。
6. 新增策略 B：`kiro_rs_tool`，参数化实现，不复用 A 的 target ratio / amplification / reported usage。
7. 路由绑定策略实例，而不是直接绑定散参数。

一句话：

应该先把 prompt-cache 变成“公共 tracker + 公共 profile/canonical/bounds + 多个参数化策略族”的结构，再分别落 A/B 策略。当前 A 策略也要按目标修正：first miss 不能显示 read，scope 默认只按 session。B 策略则作为另一个参数化策略族加入，由路径选择具体策略实例。

---

## 12. 现网路径配置与前端回显兼容方案

这一节补充一个很重要的约束：现网已经有人在 `cachePolicy.pathOverrides` 和 `reportedUsage.pathOverrides` 里按路径配置了缓存参数。改造不能只让后端“能读旧配置”，还必须让前端打开配置页时能正常回显、编辑、保存，并且不会把旧配置或新配置丢掉。

### 12.1 当前前端真实行为

当前 UI 已经有一套旧路径配置编辑器。

事实：

- `ui/src/types/api.ts:1001` 定义 `ReportedUsageConfig.pathOverrides`。
- `ui/src/types/api.ts:1037` 定义 `CachePolicyConfig.pathOverrides`。
- `ui/src/lib/runtime-config-defaults.ts:347` 的 `normalizeCachePolicy(...)` 会规范化旧 `cachePolicy.pathOverrides`。
- `ui/src/features/runtime/runtime-sections.tsx:478` 会把三类路径合并成一张路径列表：
  - `cachePolicy.pathOverrides`
  - `reportedUsage.pathOverrides`
  - `definedCacheRoutes`
- `ui/src/features/runtime/runtime-sections.tsx:503` 的 `mergedPolicyForPath(...)` 会把旧 `reportedUsage.pathOverrides[prefix]` 合并到 `cachePolicy.pathOverrides[prefix].reportedUsage` 里显示。
- `ui/src/features/runtime/runtime-sections.tsx:508` 保存某个路径时，会把旧的 `reportedUsage.pathOverrides[prefix]` 删除，并把内容并入 `cachePolicy.pathOverrides[prefix]`。

这说明现在 UI 已经在做一次“旧 reportedUsage 路径覆盖 -> 统一 cachePolicy 路径覆盖”的隐式迁移。新策略实例模型必须保留这条兼容路径。

### 12.2 当前 UI 的新字段丢失风险

如果以后加新字段，例如：

```json
{
  "cachePolicy": {
    "pathStrategies": {
      "/cc/v1/messages": "B-1"
    }
  }
}
```

或者在旧 `pathOverrides` 里加：

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc/v1/messages": {
        "strategyId": "B-1"
      }
    }
  }
}
```

当前前端可能会出问题。

原因：

`ui/src/lib/runtime-config-defaults.ts:342` 的 `isEmptyCachePolicyPatch(...)` 只判断旧字段：

```ts
return !policy.simulation
  && !policy.creationControl
  && !policy.reportedUsage
  && !policy.cachePoint
  && !policy.bounds
```

如果一个 path patch 只有新字段，比如 `strategyId`，这个函数会把它当成空 patch。然后 `normalizeCachePolicy(...)` 会把这个路径过滤掉。

同类问题在 `ui/src/features/runtime/runtime-sections.tsx:124` 的 `isEmptyRoutePatch(...)` 也存在。它也只认识旧字段。

所以 UI 兼容不是“加几个控件”这么简单，必须先保证 normalize / empty 判断 / 保存逻辑不会丢新字段。

### 12.3 后端兼容原则

后端必须支持两套配置同时存在一段时间：

1. 旧模型：

```json
{
  "cachePolicy": {
    "default": {},
    "pathOverrides": {
      "/cc": {
        "simulation": {
          "targetReadRatio": 0.95
        },
        "reportedUsage": {
          "enabled": true
        }
      }
    }
  },
  "reportedUsage": {
    "pathOverrides": {
      "/ha": {
        "enabled": true
      }
    }
  }
}
```

2. 新模型：

```json
{
  "promptCacheStrategies": {
    "A-1": {
      "family": "current_weighted",
      "preset": "default"
    },
    "B-1": {
      "family": "kiro_rs_tool",
      "preset": "default"
    }
  },
  "cachePolicy": {
    "defaultStrategy": "A-1",
    "pathStrategies": {
      "/cc": "B-1"
    }
  }
}
```

兼容规则建议：

1. 如果没有新模型字段，完全按旧模型解析。
2. 如果有新模型字段，则优先按新模型解析。
3. 旧 `cachePolicy.pathOverrides` 仍要能解析，并映射成隐式策略实例。
4. 旧 `reportedUsage.pathOverrides` 仍要能解析，并继续参与旧模型路径回显。
5. 同一路径同时存在新 `pathStrategies[prefix]` 和旧 `pathOverrides[prefix]` 时，不建议静默合并。推荐启动/保存时报错，提示用户二选一。

为什么不建议静默合并：

- 新策略实例是“整套策略”。
- 旧 path override 是“散字段 patch”。
- 如果把旧 patch 静默叠到新策略上，UI 很难解释最终值来自哪里，也容易让 B 策略混入 A 的参数。

### 12.4 后端迁移层设计

推荐在配置解析阶段增加一个中间层，不要让 handler 直接读两套配置。

新增一个“已解析策略图”：

```rust
pub struct ResolvedPromptCacheStrategyRegistry {
    pub strategies: BTreeMap<String, ResolvedPromptCacheStrategy>,
    pub default_strategy_id: String,
    pub path_bindings: BTreeMap<String, String>,
    pub legacy_path_patches: BTreeMap<String, CacheRoutePolicyPatch>,
}
```

解析步骤：

1. 读取显式 `promptCacheStrategies`。
2. 如果不存在，创建隐式策略实例：

```text
__legacy_default_current_weighted
```

这个实例来自旧全局字段：

- `promptCacheTargetReadRatio`
- `promptCacheTokenScale`
- `promptCacheMaxSimulatedInputTokens`
- `promptCacheCreationControl`
- `reportedUsage.default`
- `promptCacheMaxEntriesPerAccount`
- `promptCacheMaxEntriesGlobal`
- `promptCacheEntryTtlSecs`
- `promptCacheEstimatedBytesLimit`

3. 对每个旧 `cachePolicy.pathOverrides[prefix]` 创建隐式策略实例：

```text
__legacy_path_{hash(prefix)}
```

它的内容等于：

```text
legacy default strategy + path override patch
```

4. 对每个旧 `reportedUsage.pathOverrides[prefix]`，如果同路径没有 `cachePolicy.pathOverrides[prefix].reportedUsage`，就合并进对应隐式策略。
5. 如果存在新 `pathStrategies[prefix]`，则直接绑定到显式策略实例。
6. 最终 handler 只看：

```text
path -> strategy id -> resolved strategy
```

好处：

- 后端执行层只面对一种结构。
- 旧配置和新配置都能统一解析。
- UI 也可以展示“这个路径来自旧配置”还是“这个路径来自新策略实例”。

### 12.5 前端数据模型要同时支持旧模型和新模型

`ui/src/types/api.ts` 需要新增类型，但不能删除旧类型。

建议新增：

```ts
export type PromptCacheStrategyFamily = 'current_weighted' | 'kiro_rs_tool'

export interface PromptCacheStrategyConfig {
  family: PromptCacheStrategyFamily
  preset?: string
  params?: Record<string, unknown>
}

export interface PromptCacheStrategiesConfig {
  strategies: Record<string, PromptCacheStrategyConfig>
}

export interface CachePolicyConfig {
  default?: CacheRoutePolicyPatch
  pathOverrides: Record<string, CacheRoutePolicyPatch>

  defaultStrategy?: string
  pathStrategies?: Record<string, string>
}
```

或者把 `promptCacheStrategies` 放在 `RuntimeConfig` 顶层：

```ts
export interface RuntimeConfig {
  promptCacheStrategies?: Record<string, PromptCacheStrategyConfig>
  cachePolicy: CachePolicyConfig
}
```

关键点：

- 旧字段保持可选但继续存在。
- 新字段必须被 `normalizeConfig(...)` 保留。
- 新字段必须被 `normalizeCachePolicy(...)` 保留。
- `isEmptyCachePolicyPatch(...)` 和 `isEmptyRoutePatch(...)` 必须认识新字段，不能过滤掉。

### 12.6 前端 normalize 兼容方案

`normalizeCachePolicy(...)` 需要改成“保留未知但合法的新字段”。

建议：

```ts
function isEmptyCachePolicyPatch(policy: CacheRoutePolicyPatch): boolean {
  return !policy.simulation
    && !policy.creationControl
    && !policy.reportedUsage
    && !policy.cachePoint
    && !policy.bounds
    && !policy.strategyId
}
```

如果使用 `pathStrategies`，则 `normalizeCachePolicy(...)` 还要保留：

```ts
defaultStrategy
pathStrategies
```

并规范化路径：

```ts
pathStrategies: normalizePathRecord(source.pathStrategies ?? {})
```

同时 `normalizeConfig(draft)` 需要带上：

```ts
promptCacheStrategies: normalizePromptCacheStrategies(draft.promptCacheStrategies)
cachePolicy: normalizeCachePolicy(draft.cachePolicy)
```

否则 UI 保存一次，就可能把后端返回的新策略配置清空。

### 12.7 前端回显建议：双模式显示

为了兼容现网，前端“缓存策略”区域建议分两层：

#### 第一层：策略实例管理

显示：

- 策略实例 ID。
- 策略族：`current_weighted` / `kiro_rs_tool`。
- preset。
- 参数摘要。
- 被哪些路径引用。

例如：

```text
A-1
  类型：当前高缓存策略
  参数：targetReadRatio=0.98, tokenScale=1.6, scope=session_only
  使用路径：/v1/messages, /ha

B-1
  类型：Kiro-RS-Tool
  参数：scope=session_then_client_key, skipDynamicSystem=true
  使用路径：/cc/v1/messages
```

#### 第二层：路径绑定

每个路径显示：

- 路径前缀。
- 当前使用的策略实例。
- 来源：
  - 新策略绑定。
  - 旧 path override。
  - 旧 reportedUsage override。
  - definedCacheRoute。
- 是否可迁移。

旧路径配置不要一上来强制迁移。应该回显为：

```text
/cc
  来源：旧路径覆盖
  当前等效策略：__legacy_path_xxx
  操作：继续编辑旧参数 / 转换为策略实例
```

这样现网用户打开 UI 不会看到配置丢失，也不会被迫理解新模型。

### 12.8 旧路径配置的 UI 展示规则

旧路径存在三种来源：

1. 只在 `reportedUsage.pathOverrides` 里。
2. 只在 `cachePolicy.pathOverrides` 里。
3. 两边都有。

当前 UI 已经合并展示这三种，必须保留。

建议新 UI 展示时：

```text
路径 /cc
来源：
  cachePolicy.pathOverrides: 有
  reportedUsage.pathOverrides: 有或无
模式：
  旧路径覆盖
等效策略：
  current_weighted + path patch
```

保存时要有两种按钮：

1. “按旧模型保存”
   - 保持写回 `cachePolicy.pathOverrides`。
   - 兼容现网。

2. “转换为策略实例”
   - 创建新的 strategy instance，例如 `legacy-/cc`。
   - 写入 `promptCacheStrategies`。
   - 写入 `cachePolicy.pathStrategies["/cc"] = "legacy-/cc"`。
   - 删除旧 `cachePolicy.pathOverrides["/cc"]` 和旧 `reportedUsage.pathOverrides["/cc"]`。

不要在用户不知情时自动转换。

### 12.9 后端 API 回显建议

如果后端只返回原始 config，前端需要自己做很多推导。

更稳的方式是后端额外提供一个“解析结果视图”，用于 UI 回显：

```json
{
  "rawConfig": { "...": "..." },
  "resolvedCachePolicy": {
    "defaultStrategyId": "A-1",
    "paths": [
      {
        "path": "/cc",
        "strategyId": "__legacy_path_cc",
        "family": "current_weighted",
        "source": "legacy_path_override",
        "hasLegacyCachePolicyOverride": true,
        "hasLegacyReportedUsageOverride": true,
        "canConvertToStrategy": true,
        "summary": {
          "targetReadRatio": 0.95,
          "tokenScale": 1.6,
          "scope": "session_only"
        }
      }
    ]
  }
}
```

这个 API 不一定要第一阶段就做，但长期建议有。否则 UI 和后端各自实现一套解析逻辑，容易出现 UI 看到的和后端实际执行的不一致。

### 12.10 保存兼容原则

前端保存 runtime config 时必须做到：

1. 不认识的字段不能丢。
2. 旧字段不自动删除。
3. 新字段不被 normalize 当成空值过滤。
4. 同路径新旧冲突要提示用户，而不是静默覆盖。
5. 转换旧路径配置时必须是显式操作。

当前 `RuntimePage` 的保存逻辑是：

- 从 `draft` 构造 `next`。
- 对大量字段做 normalize。
- 删除 proxy secret 字段。
- 调用 update。

证据：`ui/src/features/runtime/runtime-page.tsx:160` 到 `ui/src/features/runtime/runtime-page.tsx:235`。

因此一旦 `RuntimeConfig` 顶层新增 `promptCacheStrategies`，必须确认：

- `draft` 初始化时保留它。
- `normalizeConfig` 返回时保留它。
- `editable` 提交时保留它。

否则打开 UI 后保存一次，新策略配置就可能丢失。

### 12.11 UI 校验规则

前端应该提前校验：

1. `defaultStrategy` 必须存在于 `promptCacheStrategies`。
2. `pathStrategies[prefix]` 引用的策略必须存在。
3. 同一路径不能同时配置：
   - `cachePolicy.pathStrategies[prefix]`
   - `cachePolicy.pathOverrides[prefix]`
4. 同一路径如果有新策略绑定，不允许再通过旧 `reportedUsage.pathOverrides[prefix]` 覆盖它，除非 UI 明确进入“高级混合模式”。
5. 策略实例 ID 不允许为空，不允许重复。
6. 删除策略实例前必须检查是否有路径引用。

这些校验后端也必须再做一遍，前端只负责提前提示。

### 12.12 推荐迁移路径

不要一次性把现网旧路径配置全部迁移成新策略模型。

推荐三阶段：

#### 阶段 1：只读兼容

后端：

- 支持旧模型。
- 支持新模型。
- 能把旧模型解析成内部 strategy instance。

前端：

- 能显示旧路径配置。
- 能显示新策略配置。
- 保存时不丢任何字段。

#### 阶段 2：显式转换

前端给每个旧路径提供按钮：

```text
转换为策略实例
```

转换后：

- 新建策略实例。
- path 绑定到该实例。
- 旧 path override 删除。

#### 阶段 3：弱化旧入口

等现网旧配置基本迁移后：

- 新建路径默认用策略实例模型。
- 旧 path override 仍可编辑，但标为“兼容模式”。
- 后端继续长期支持旧字段。

### 12.13 必须增加的测试

后端测试：

1. 只有旧 `cachePolicy.pathOverrides` 时，解析结果和现在一致。
2. 只有旧 `reportedUsage.pathOverrides` 时，仍参与路径策略。
3. 新 `pathStrategies` 能绑定策略实例。
4. 同一路径新旧同时出现时报错。
5. 旧路径 override 能生成隐式策略实例。
6. 旧配置序列化/反序列化不丢字段。

前端测试：

1. 加载旧 config，路径列表能显示 `cachePolicy.pathOverrides`。
2. 加载旧 config，路径列表能显示 `reportedUsage.pathOverrides`。
3. 加载新 config，策略实例列表能显示。
4. 加载新 config 后点保存，不丢 `promptCacheStrategies` 和 `pathStrategies`。
5. 只有 `strategyId` 或 `pathStrategies` 的路径不会被 normalize 过滤。
6. 同一路径新旧冲突时 UI 显示错误。
7. 旧路径点击“转换为策略实例”后，生成的新 config 符合预期。

### 12.14 最终建议

兼容现网路径配置和 UI 回显，要按“双模型长期共存”设计：

1. 后端执行层统一成 strategy registry。
2. 配置层同时接受旧 `pathOverrides` 和新 `pathStrategies`。
3. 旧路径覆盖在解析阶段生成隐式策略实例，不要求用户立刻迁移。
4. 前端保留旧路径编辑器，同时增加策略实例管理和路径绑定视图。
5. 前端 normalize / empty 判断必须认识新字段，不能把新策略路径当空配置删掉。
6. 同一路径新旧配置冲突要显式报错，不要静默合并。
7. 旧配置迁移必须由用户点击触发，不能打开页面保存一次就自动改结构。

一句话：

这次改造不能只从 Rust handler 的角度做。现网已经存在按路径配置的缓存参数，UI 也已经有旧路径覆盖编辑逻辑。正确方案是后端把旧路径配置解析成隐式策略实例，前端同时回显旧模型和新策略模型，并保证保存时不丢旧字段也不丢新字段。
