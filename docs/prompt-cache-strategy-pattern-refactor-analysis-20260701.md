# Prompt Cache 策略模式抽离与 Kiro-RS-Tool 策略接入分析

日期：2026-07-01

范围：仅分析，不实施代码。

这份文档按最新理解整理：这次不是要把现网配置迁移成另一套复杂模型，也不是要把现有参数和新策略参数强行统一。真正要做的是：

1. 修复当前缓存策略已有 bug。
2. 把当前缓存策略从混杂代码里抽离出来，作为一个独立策略实现。
3. 增加一个“缓存策略类型”字段。
4. 缺省策略类型就是当前现网策略，因此现网已有路径参数继续原样生效。
5. 再新增 `Kiro-RS-Tool` 风格策略类型。
6. 每种策略读取自己的参数；能共享的公共能力抽出去，策略之间互不影响。

---

## 1. 核心结论

最合理的改造不是“迁移现网路径配置”，而是“给现有配置补一个策略类型解释层”。

现网现在已经有路径级缓存参数：

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc": {
        "simulation": {
          "targetReadRatio": 0.98,
          "tokenScale": 1.6
        },
        "creationControl": {},
        "reportedUsage": {},
        "bounds": {}
      }
    }
  }
}
```

改造后，这条路径可以被解释为：

```text
路径 /cc
缓存策略类型：current_high_cache，缺省得到
策略参数：继续使用 simulation / creationControl / reportedUsage / bounds
```

也就是说，旧配置不需要迁移。只要 `cacheType` 没写，就默认认为它是当前策略类型。

新增 `Kiro-RS-Tool` 风格后，某个路径可以写：

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc": {
        "cacheType": "kiro_rs_tool",
        "kiroRsTool": {
          "scopeMode": "session_then_client_key",
          "skipDynamicSystemBeforeCacheControl": true
        },
        "bounds": {
          "maxEntriesGlobal": 4096
        }
      }
    }
  }
}
```

此时解析顺序是：

```text
先看 cacheType
如果没写：按 current_high_cache
如果写 current_high_cache：读取现有 simulation / creationControl / reportedUsage / bounds
如果写 kiro_rs_tool：读取 kiroRsTool / bounds / 该策略允许的参数
```

这样就兼容现网，也清楚表达了新增策略。

---

## 2. 当前代码里的问题

### 2.1 当前策略没有独立边界

现在 `src/anthropic/prompt_cache.rs` 里混在一起的东西太多：

- scope 定义。
- profile 构建。
- canonical JSON。
- token 估算。
- TTL 解析。
- tracker 存储。
- 当前 high-cache 算法。
- target ratio 计算。
- bounds 淘汰。

证据：

- `src/anthropic/prompt_cache.rs:24`：`PromptCacheScope`。
- `src/anthropic/prompt_cache.rs:128`：`PromptCacheTracker`。
- `src/anthropic/prompt_cache.rs:161`：profile 构建。
- `src/anthropic/prompt_cache.rs:240`：`compute_with_bounds(...)`。
- `src/anthropic/prompt_cache.rs:324`：`update_with_bounds(...)`。
- `src/anthropic/prompt_cache.rs:494`：`flatten_cache_blocks(...)`。
- `src/anthropic/prompt_cache.rs:653`：`target_cache_tokens(...)`。

这导致后面想加 `Kiro-RS-Tool` 策略时，要么继续往一个文件里堆分支，要么把当前策略逻辑拆干净。

建议先拆干净。

### 2.2 当前路径配置已经存在，不能破坏

当前后端已有：

- `CachePolicyConfig.default`
- `CachePolicyConfig.path_overrides`
- `resolve_cache_policy_for_path(...)`
- 路径最长前缀匹配

证据：

- `src/model/config.rs:1195`
- `src/model/config.rs:1269`
- `src/model/config.rs:1283`

当前前端也已经支持这些旧路径配置：

- `ui/src/types/api.ts:1037`：`CachePolicyConfig.pathOverrides`。
- `ui/src/features/runtime/runtime-sections.tsx:478`：路径列表合并 `cachePolicy.pathOverrides`、`reportedUsage.pathOverrides`、`definedCacheRoutes`。
- `ui/src/features/runtime/runtime-sections.tsx:503`：旧 reported usage 路径覆盖会合并进路径策略编辑器里显示。

因此方案必须做到：

```text
旧路径配置继续读。
旧路径配置继续显示。
旧路径配置保存后不能丢字段。
```

### 2.3 当前策略有 bug：第一次请求可能显示 cache read

底层 tracker 首次 miss 时，`cache_read_input_tokens = 0`。

证据：`src/anthropic/prompt_cache.rs:269`。

但 reported usage 会把 input 被压低后的差值搬进 `cache_read_input_tokens`。

证据：

- `src/anthropic/cache.rs:136`
- `src/anthropic/cache.rs:139`
- `src/anthropic/cache.rs:140`
- `src/model/config.rs:435`
- `src/model/config.rs:616`

这会造成“第一次请求也出现缓存读取”的显示问题。

这个问题应该先作为当前策略 bug 修掉，而不是等新增 `Kiro-RS-Tool` 策略后再处理。

### 2.4 当前 scope 包含凭证和模型

当前 scope 是：

```rust
credential_id + conversation_id + model + route_namespace
```

证据：`src/anthropic/prompt_cache.rs:24`。

构造点：

- `src/anthropic/handlers.rs:3256`
- `src/external_pool.rs:4406`

按最新目标，当前策略也要改成基于会话，不考虑凭证、模型等。

这属于当前策略本身的修正，不是 `Kiro-RS-Tool` 策略专属。

---

## 3. 推荐总体架构

### 3.1 增加缓存策略类型字段

建议在路径策略 patch 里新增字段：

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheStrategyType {
    CurrentHighCache,
    KiroRsTool,
}
```

配置字段建议叫：

```json
cacheType
```

或者：

```json
strategyType
```

更贴近用户理解的是 `cacheType`，因为它表达“用哪一种缓存策略”。

示例：

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc": {
        "cacheType": "current_high_cache",
        "simulation": {
          "targetReadRatio": 0.98
        }
      },
      "/kiro-tool": {
        "cacheType": "kiro_rs_tool",
        "kiroRsTool": {
          "scopeMode": "session_then_client_key"
        }
      }
    }
  }
}
```

兼容规则：

```text
cacheType 缺省 = current_high_cache
```

所以现网已有配置完全不需要新增字段，也能继续按当前策略读。

### 3.2 每个策略读自己的参数

当前策略读取：

```text
simulation
creationControl
reportedUsage
cachePoint
bounds
```

`Kiro-RS-Tool` 策略读取：

```text
kiroRsTool
bounds
必要时读取 reportedUsage，但默认不做采样
```

不要为了统一把 `Kiro-RS-Tool` 硬塞进 `simulation.targetReadRatio/tokenScale`。

这两个策略的参数可以长得不一样。

### 3.3 策略执行层统一入口

handler 不应该直接知道当前是哪种算法细节。

建议新增统一入口：

```rust
pub enum PromptCacheStrategy {
    CurrentHighCache(CurrentHighCacheStrategy),
    KiroRsTool(KiroRsToolStrategy),
}
```

每个策略实现同一组能力：

```rust
impl PromptCacheStrategy {
    pub fn build_profile(...)
    pub fn resolve_scope(...)
    pub fn compute_usage(...)
    pub fn apply_reported_usage(...)
    pub fn commit_success(...)
}
```

或者先不用 trait，直接 enum + match。

建议第一版用 enum + match，不要上 trait object。原因：

- 策略数量少。
- 编译期类型更清楚。
- 单测更直接。
- 不会把生命周期和动态分发搞复杂。

---

## 4. 模块抽离方案

建议把 `src/anthropic/prompt_cache.rs` 拆成目录模块：

```text
src/anthropic/prompt_cache/
  mod.rs
  types.rs
  canonical.rs
  profile.rs
  scope.rs
  tracker.rs
  bounds.rs
  accounting.rs
  strategy/
    mod.rs
    current_high_cache.rs
    kiro_rs_tool.rs
```

### 4.1 `types.rs`

放通用类型：

- `PromptCacheUsage`
- `PromptCacheProfile`
- `PromptCacheBreakpoint`
- `PromptCacheLookupPoint`
- `PromptCacheEntry`
- fingerprint 类型。

这部分不应该包含某个具体策略的 target ratio 或 Kiro-RS-Tool 特有行为。

### 4.2 `canonical.rs`

放共享 canonical 方法：

- JSON 字段排序。
- 去掉 `cache_control`。
- volatile id 归一。
- position key 归一。
- billing header 判断。

当前 `canonicalize_cache_value(...)` 这类逻辑可以搬到这里。

### 4.3 `profile.rs`

放通用 profile 构建辅助。

但注意：不同策略可能有不同 flatten 规则。

当前策略的 flatten：

- 会加入 request prelude。
- prelude 包含 `model` 和 `tool_choice`。
- system 全部纳入。

证据：`src/anthropic/prompt_cache.rs:494`。

`Kiro-RS-Tool` 策略的 flatten：

- 不一定加入 model。
- 可跳过首个 cache_control 前的动态 system block。
- 最后一条 message 默认不切段，除非显式 cache_control。

所以 `profile.rs` 应该提供公共 builder 能力，但具体 flatten policy 由策略传入。

### 4.4 `scope.rs`

放 scope 解析。

当前目标是当前策略也改为 session-only，所以 scope policy 至少要支持：

```rust
pub enum PromptCacheScopeMode {
    SessionOnly,
    SessionThenClientKey,
    LegacyCredentialConversationModel,
}
```

注意：

- `SessionOnly` 是当前策略目标默认。
- `SessionThenClientKey` 是 `Kiro-RS-Tool` 默认。
- `LegacyCredentialConversationModel` 保留用于回滚或兼容测试，不作为默认。

### 4.5 `tracker.rs`

tracker 只负责存储，不应该知道策略参数。

当前 `compute_with_bounds(...)` 把 target ratio 也放进 tracker 了：

```rust
compute_with_bounds(scope, profile, target_read_ratio, bounds)
```

建议拆成两层：

1. tracker lookup：

```rust
lookup(scope, lookup_points, bounds) -> hit result
```

2. 策略根据 hit result 算 usage。

当前策略自己算：

```text
targetReadRatio
targetTokens
creation/read
amplification
```

`Kiro-RS-Tool` 策略自己算：

```text
deepest hit
covered prefix
split against total
```

### 4.6 `bounds.rs`

放边界控制：

- max entries per scope。
- max entries global。
- TTL。
- estimated bytes limit。

当前字段叫 `maxEntriesPerAccount`，但 session-only 后“account”不准确。为了兼容，字段可以先不改名，内部解释成 per-scope：

```text
maxEntriesPerAccount 旧名保留
内部含义逐步改为 maxEntriesPerScope
前端文案改成“单 scope 条目上限”或“单会话条目上限”
```

不要马上删除旧字段，否则现网配置会坏。

### 4.7 `accounting.rs`

放账本不变量：

- first miss 不能 read。
- reported usage 不能凭空制造 read。
- input + creation + read 要自洽。
- 失败请求不能 commit。

这个模块不是某个策略独有，两个策略都应该能用它检查结果。

### 4.8 `strategy/current_high_cache.rs`

当前策略独立放这里。

它拥有自己的参数：

```rust
pub struct CurrentHighCacheConfig {
    pub simulation: CacheSimulationPolicy,
    pub creation_control: PromptCacheCreationControlConfig,
    pub reported_usage: ReportedUsagePathPolicy,
    pub cache_point: CachePointPolicy,
    pub bounds: CacheBoundsPolicy,
    pub scope_mode: PromptCacheScopeMode,
}
```

重点：

- 现网参数继续原样读。
- 缺省 `cacheType` 时就是这个策略。
- bug 修复在这里或 accounting 层生效。
- scope 默认改成 `SessionOnly`。

### 4.9 `strategy/kiro_rs_tool.rs`

新增策略独立放这里。

它有自己的参数：

```rust
pub struct KiroRsToolCacheConfig {
    pub scope_mode: PromptCacheScopeMode,
    pub skip_dynamic_system_before_cache_control: bool,
    pub include_model_in_fingerprint: bool,
    pub include_tool_choice_in_fingerprint: bool,
    pub commit_on_success_only: bool,
    pub bounds: CacheBoundsPolicy,
}
```

默认建议：

```text
scopeMode = session_then_client_key
skipDynamicSystemBeforeCacheControl = true
includeModelInFingerprint = false
includeToolChoiceInFingerprint = false
commitOnSuccessOnly = true
```

这些参数不需要和当前策略的 simulation 参数保持一致。

---

## 5. 参数中英文说明

下面是现有参数和建议新增参数的中文含义。

### 5.1 当前策略参数

| JSON 字段 | 中文含义 | 属于哪个策略 | 是否现网已有 |
|---|---|---|---|
| `cacheType` | 缓存策略类型 | 通用 | 新增，缺省为当前策略 |
| `simulation.enabled` | 是否启用当前高缓存模拟 | 当前策略 | 是 |
| `simulation.targetReadRatio` | 目标缓存读取比例 | 当前策略 | 是 |
| `simulation.tokenScale` | 输入 token 展示放大倍数 | 当前策略 | 是 |
| `simulation.maxSimulatedInputTokens` | 模拟输入 token 上限 | 当前策略 | 是 |
| `simulation.capJitterMinTokens` | 触顶扣减下限 | 当前策略 | 是 |
| `simulation.capJitterMaxTokens` | 触顶扣减上限 | 当前策略 | 是 |
| `simulation.scaleMinInputTokens` | 启用放大的输入门槛 | 当前策略 | 是 |
| `creationControl.enabled` | 是否启用缓存创建频次控制 | 当前策略 | 是 |
| `creationControl.scopeMode` | 创建频次控制的统计维度 | 当前策略 | 是 |
| `creationControl.minSuccessfulRequestsBetweenCreation` | 两次 creation 上报之间至少间隔多少个成功请求 | 当前策略 | 是 |
| `creationControl.minCreationIntervalSecs` | 两次 creation 上报之间至少间隔多少秒 | 当前策略 | 是 |
| `creationControl.minCreationDeltaTokens` | 累积多少被抑制 creation token 后才允许再次上报 | 当前策略 | 是 |
| `creationControl.maxCreationTokensPerEvent` | 单次最多允许上报多少 creation token | 当前策略 | 是 |
| `creationControl.creationBudgetWindowSecs` | creation 预算窗口秒数 | 当前策略 | 是 |
| `creationControl.maxCreationTokensPerWindow` | 一个预算窗口内最多 creation token | 当前策略 | 是 |
| `creationControl.expireAfterIdleSecs` | 空闲多久后清理频控状态 | 当前策略 | 是 |
| `reportedUsage.enabled` | 是否启用 usage 展示策略 | 当前策略 | 是 |
| `reportedUsage.input` | input token 展示规则 | 当前策略 | 是 |
| `reportedUsage.output` | output token 展示规则 | 当前策略 | 是 |
| `reportedUsage.cacheRead` | cache read token 展示规则 | 当前策略 | 是 |
| `reportedUsage.cacheCreation` | cache creation token 展示规则 | 当前策略 | 是 |
| `cachePoint.enabled` | 是否给上游发送真实 cachePoint | 当前策略相关功能 | 是 |
| `cachePoint.toolsOnly` | 是否只给工具插 cachePoint | 当前策略相关功能 | 是 |
| `cachePoint.recordPlan` | 是否记录 cachePoint 插入计划 | 当前策略相关功能 | 是 |
| `bounds.maxEntriesPerAccount` | 单账号条目上限；session-only 后建议解释为单 scope 条目上限 | 通用 | 是 |
| `bounds.maxEntriesGlobal` | 全局缓存条目上限 | 通用 | 是 |
| `bounds.entryTtlSecs` | 单条本地缓存最长保留秒数 | 通用 | 是 |
| `bounds.estimatedBytesLimit` | 估算内存上限 | 通用 | 是 |

### 5.2 新增 Kiro-RS-Tool 策略参数

| JSON 字段 | 中文含义 | 属于哪个策略 | 是否必须 |
|---|---|---|---|
| `kiroRsTool.scopeMode` | scope 模式，比如 session 优先、否则 client key | Kiro-RS-Tool | 否，有默认 |
| `kiroRsTool.skipDynamicSystemBeforeCacheControl` | 是否跳过首个 cache_control 前的动态 system 块 | Kiro-RS-Tool | 否，有默认 |
| `kiroRsTool.includeModelInFingerprint` | 指纹里是否包含模型 | Kiro-RS-Tool | 否，有默认 |
| `kiroRsTool.includeToolChoiceInFingerprint` | 指纹里是否包含 tool_choice | Kiro-RS-Tool | 否，有默认 |
| `kiroRsTool.commitOnSuccessOnly` | 是否仅成功响应后写缓存 | Kiro-RS-Tool | 否，默认 true |
| `bounds.*` | 缓存容量和 TTL 边界 | 通用 | 否，可复用 |

### 5.3 为什么这些参数能兼容

因为解析逻辑可以很简单：

```text
读取路径配置
先看 cacheType
如果 cacheType 缺省：current_high_cache
如果 current_high_cache：读现有 simulation / creationControl / reportedUsage / cachePoint / bounds
如果 kiro_rs_tool：读 kiroRsTool / bounds
```

现网旧配置没有 `cacheType`，所以自然进入 `current_high_cache`。

这不需要迁移。

---

## 6. 后端配置兼容方案

### 6.1 修改 `CacheRoutePolicyPatch`

建议从：

```rust
pub struct CacheRoutePolicyPatch {
    pub simulation: Option<CacheSimulationPolicyPatch>,
    pub creation_control: Option<PromptCacheCreationControlConfig>,
    pub reported_usage: Option<ReportedUsagePathPolicy>,
    pub cache_point: Option<CachePointPolicyPatch>,
    pub bounds: Option<CacheBoundsPolicyPatch>,
}
```

扩展成：

```rust
pub struct CacheRoutePolicyPatch {
    #[serde(default)]
    pub cache_type: Option<PromptCacheStrategyType>,

    #[serde(default)]
    pub simulation: Option<CacheSimulationPolicyPatch>,

    #[serde(default)]
    pub creation_control: Option<PromptCacheCreationControlConfig>,

    #[serde(default)]
    pub reported_usage: Option<ReportedUsagePathPolicy>,

    #[serde(default)]
    pub cache_point: Option<CachePointPolicyPatch>,

    #[serde(default)]
    pub bounds: Option<CacheBoundsPolicyPatch>,

    #[serde(default)]
    pub kiro_rs_tool: Option<KiroRsToolCachePolicyPatch>,
}
```

兼容点：

- `cache_type` 是 `Option`。
- 旧配置没有这个字段，反序列化没问题。
- 解析时 `None` 当成 `CurrentHighCache`。

### 6.2 修改解析结果

当前：

```rust
pub struct CacheRoutePolicy {
    pub simulation: CacheSimulationPolicy,
    pub creation_control: PromptCacheCreationControlConfig,
    pub reported_usage: ReportedUsagePathPolicy,
    pub cache_point: CachePointPolicy,
    pub bounds: CacheBoundsPolicy,
}
```

建议改成：

```rust
pub struct CacheRoutePolicy {
    pub cache_type: PromptCacheStrategyType,
    pub current_high_cache: CurrentHighCachePolicy,
    pub kiro_rs_tool: KiroRsToolCachePolicy,
    pub bounds: CacheBoundsPolicy,
}
```

或者为了减少第一阶段改动，也可以先保留旧字段，再加：

```rust
pub cache_type: PromptCacheStrategyType,
pub kiro_rs_tool: KiroRsToolCachePolicy,
```

第一阶段推荐保守：

```rust
pub struct CacheRoutePolicy {
    pub cache_type: PromptCacheStrategyType,
    pub simulation: CacheSimulationPolicy,
    pub creation_control: PromptCacheCreationControlConfig,
    pub reported_usage: ReportedUsagePathPolicy,
    pub cache_point: CachePointPolicy,
    pub bounds: CacheBoundsPolicy,
    pub kiro_rs_tool: KiroRsToolCachePolicy,
}
```

这样现有代码改动小，后面策略抽离完成后再把旧字段收进 `CurrentHighCachePolicy`。

### 6.3 路径解析逻辑

现有 `resolve_cache_policy_for_path(...)` 可以保留，只是在 patch apply 时增加：

```rust
if let Some(cache_type) = self.cache_type {
    policy.cache_type = cache_type;
}
if let Some(kiro_rs_tool) = self.kiro_rs_tool {
    policy.kiro_rs_tool = kiro_rs_tool.apply_to(policy.kiro_rs_tool);
}
```

旧路径 override 完全继续生效。

### 6.4 `affects_cache_state` 也要包含新字段

当前：

```rust
self.simulation is_some
|| self.creation_control is_some
|| self.bounds is_some
```

新增后要包括：

```rust
|| self.cache_type.is_some()
|| self.kiro_rs_tool.is_some()
```

否则某个路径只设置：

```json
{
  "cacheType": "kiro_rs_tool"
}
```

可能不会触发 namespace，导致不同策略混用缓存桶。

---

## 7. 前端兼容方案

### 7.1 类型扩展

`ui/src/types/api.ts` 增加：

```ts
export type PromptCacheStrategyType = 'current_high_cache' | 'kiro_rs_tool'

export interface KiroRsToolCachePolicyPatch {
  scopeMode?: 'session_then_client_key' | 'session_only' | 'client_key_only'
  skipDynamicSystemBeforeCacheControl?: boolean
  includeModelInFingerprint?: boolean
  includeToolChoiceInFingerprint?: boolean
  commitOnSuccessOnly?: boolean
}

export interface CacheRoutePolicyPatch {
  cacheType?: PromptCacheStrategyType
  simulation?: CacheSimulationPolicyPatch
  creationControl?: PromptCacheCreationControlConfig
  reportedUsage?: ReportedUsagePathPolicy
  cachePoint?: CachePointPolicyPatch
  bounds?: CacheBoundsPolicyPatch
  kiroRsTool?: KiroRsToolCachePolicyPatch
}
```

旧字段不删。

### 7.2 normalize 不能丢新字段

当前 `isEmptyCachePolicyPatch(...)` 只认识旧字段。

要改成：

```ts
function isEmptyCachePolicyPatch(policy: CacheRoutePolicyPatch): boolean {
  return !policy.cacheType
    && !policy.simulation
    && !policy.creationControl
    && !policy.reportedUsage
    && !policy.cachePoint
    && !policy.bounds
    && !policy.kiroRsTool
}
```

`runtime-sections.tsx` 里的 `isEmptyRoutePatch(...)` 同理。

否则只设置新策略类型的路径会被 UI 保存时过滤掉。

### 7.3 UI 回显方式

每个路径卡片顶部增加一个选择：

```text
缓存类型：
  当前高缓存策略
  Kiro-RS-Tool 策略
```

如果 `cacheType` 缺省，显示：

```text
当前高缓存策略（默认）
```

这就是现网兼容回显。

然后根据类型显示不同参数区：

#### 当前高缓存策略

显示现有 UI：

- 高缓存模拟。
- 缓存创建频次。
- 用量展示。
- 真实 cachePoint。
- 缓存边界。

#### Kiro-RS-Tool 策略

显示新 UI：

- scope 模式。
- 是否跳过动态 system。
- 是否包含模型进指纹。
- 是否包含 tool_choice 进指纹。
- 是否仅成功后写缓存。
- 缓存边界。

不要在 Kiro-RS-Tool 策略 UI 里显示 `targetReadRatio/tokenScale`，因为它们不是这个策略的参数。

### 7.4 默认路径配置

现在新增路径时，`defaultPathCachePatch(prefix)` 会直接创建一整套旧参数。

可以保持不变：

```text
新增路径默认 cacheType 缺省，也就是 current_high_cache
```

如果用户切换到 `Kiro-RS-Tool`，再写入：

```json
{
  "cacheType": "kiro_rs_tool",
  "kiroRsTool": {}
}
```

旧参数是否删除？

建议 UI 不立即强删，但保存时可以做策略相关清理：

- 如果 `cacheType = current_high_cache`，保留旧参数。
- 如果 `cacheType = kiro_rs_tool`，不提交 `simulation/creationControl/cachePoint`，避免误解。
- `bounds` 可以保留，因为是共享参数。
- `reportedUsage` 是否保留要谨慎。默认建议 Kiro-RS-Tool 不启用旧 reported usage 采样。

### 7.5 现网旧配置为何能正常回显

现网路径没有 `cacheType`，例如：

```json
{
  "simulation": {
    "targetReadRatio": 0.95
  }
}
```

UI 读取时：

```text
cacheType 缺省 -> 当前高缓存策略
显示当前高缓存策略参数区
simulation.targetReadRatio 正常填入 0.95
```

保存时：

```text
仍然保存 simulation.targetReadRatio
cacheType 可以继续省略
```

所以这不是迁移，而是给旧配置增加解释。

---

## 8. 实施顺序

### 阶段 1：修当前策略 bug

先修：

- first miss 不能出现 cache read。
- 当前策略 scope 改成 session-only。

测试：

- 首次请求底层和最终 reported usage 都 read=0。
- 第二次相同会话 read>0。
- 同会话不同 credential 命中。
- 同会话不同 model 命中。
- 不同会话不命中。

### 阶段 2：抽离当前策略

先只抽离当前策略，不加新策略。

目标：

```text
当前策略成为 current_high_cache 独立实现
handler 通过策略入口调用
行为保持阶段 1 修正后的行为
```

这一步是重构，不应该改配置语义。

### 阶段 3：加 `cacheType` 字段

新增：

```text
cacheType 缺省 current_high_cache
```

此时仍然只有一种可用类型。

这一步用来验证：

- 后端兼容旧配置。
- 前端兼容旧配置。
- UI 能正常显示“当前高缓存策略（默认）”。
- 保存不丢旧路径参数。

### 阶段 4：新增 Kiro-RS-Tool 策略类型

新增：

```text
cacheType = kiro_rs_tool
kiroRsTool = {...}
```

然后实现该策略。

### 阶段 5：CLI 和内存验证

必须做：

- 单元测试。
- 直接 HTTP/SSE 测试。
- Claude Code CLI 真实验证。
- 并发和 bounds 测试。

---

## 9. 最终建议

最终方案应该是：

1. 先修当前策略已有问题。
2. 把当前策略抽离成独立策略实现。
3. 给路径策略加 `cacheType` 字段，但缺省就是当前策略。
4. 现网旧路径配置不迁移、不丢弃，继续按当前策略参数读取。
5. 前端在路径卡片上增加“缓存类型”选择；缺省显示当前策略。
6. 新增 `Kiro-RS-Tool` 策略时，只读取它自己的 `kiroRsTool` 参数和通用 bounds。
7. 共享逻辑抽到公共模块，策略逻辑互相隔离。

一句话：

这次改造本质是把“当前高缓存逻辑”从散落代码里抽成一个策略实现，再用 `cacheType` 做策略选择。现网配置没有 `cacheType` 时默认就是当前策略，所以完全可以兼容；新增 `Kiro-RS-Tool` 只是再加一个策略类型和它自己的参数，不需要强行改变现有参数模型。
