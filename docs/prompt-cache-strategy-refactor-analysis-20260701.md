# Prompt Cache 策略化改造分析文档

日期：2026-07-01

范围：仅分析和设计，不包含代码实现。

目标：

1. 把 `Kiro-RS-Tool` 的 scope 逻辑移植到当前项目，作为可选缓存策略的一部分。
2. 修复当前项目“第一次请求可能对外显示 cache read”的问题。
3. 新增一种独立的 `Kiro-RS-Tool` 风格缓存策略，并允许按路径绑定。
4. 保持现有缓存策略和已有路径默认行为不变，除“第一次伪 read”这个 bug 修复外，不做行为翻转。

---

## 1. 当前事实

### 1.1 当前项目已经有“按路径绑定缓存配置”的骨架

当前项目不是完全没有路径级缓存策略。真实代码已经存在：

- `CachePolicyConfig.default`
- `CachePolicyConfig.path_overrides`
- `resolve_cache_policy_for_path(...)`
- 最长前缀匹配
- 如果某个 path override 会影响缓存状态，会给该路径设置 `namespace`

证据：

- `src/model/config.rs:1195`：`CachePolicyConfig` 定义了 `default` 和 `path_overrides`。
- `src/model/config.rs:1269`：`resolve_cache_policy_for_path(...)` 做路径解析。
- `src/model/config.rs:1283`：按最长前缀选择 path override。
- `src/model/config.rs:1288`：如果 override 影响缓存状态，则 `namespace = Some(prefix.clone())`。

这说明路径绑定能力已经有了，但目前绑定的是一组分散参数，不是清晰的“策略类型”。

### 1.2 当前 `CacheRoutePolicy` 是参数集合，不是策略模型

当前 `CacheRoutePolicy` 包含：

- `simulation: CacheSimulationPolicy`
- `creation_control: PromptCacheCreationControlConfig`
- `reported_usage: ReportedUsagePathPolicy`
- `cache_point: CachePointPolicy`
- `bounds: CacheBoundsPolicy`

证据：`src/model/config.rs:1235`。

问题是：这些字段混在一起，只能描述“现有 high-cache 策略的参数变化”。如果要新增一套行为不同的缓存策略，比如 `Kiro-RS-Tool` 风格，继续往 `simulation` 里塞字段会让配置越来越难懂，也容易让两个策略互相污染。

### 1.3 当前底层缓存第一次 miss 不会读缓存

当前 `PromptCacheTracker::compute_with_bounds(...)` 在 scope 下没有任何缓存记录时，会返回：

- `cache_creation_input_tokens = target_tokens`
- `cache_read_input_tokens = 0`

证据：`src/anthropic/prompt_cache.rs:269`。

对应单测也明确写了：

- first request: `cache_creation_input_tokens > 0`
- first request: `cache_read_input_tokens == 0`
- update 后 second request: `cache_read_input_tokens > 0`

证据：`src/anthropic/prompt_cache.rs:931`。

所以“第一次出现 cache read”的根因不在底层 tracker 命中逻辑，而在后续 reported usage 整形。

### 1.4 当前 `/cc` 默认上报策略会把 input 差值搬进 cache read

`ReportedUsageFieldPolicy::sample_input_max(...)` 默认会设置：

```rust
move_delta_to_cache_read: true
```

证据：`src/model/config.rs:435`。

`/cc` 默认 reported usage 策略使用：

```rust
input: ReportedUsageFieldPolicy::sample_input_max(96)
```

证据：`src/model/config.rs:616`。

实际搬移发生在：

- `src/anthropic/cache.rs:136`
- `src/anthropic/cache.rs:139`
- `src/anthropic/cache.rs:140`

逻辑是：

1. 把 input 采样压低。
2. 算出 `input_delta`。
3. 如果 `move_delta_to_cache_read` 为 true，就把差值加到 `cache_read_input_tokens`。

这会造成一个错误现象：底层本来是 first miss、read 为 0，但最终对外 usage 可能出现 cache read。

这不是 `Kiro-RS-Tool` 行为，也不符合 Claude Code 用户对 prompt cache 的直觉：第一次没有东西可读，不应该显示 read。

### 1.5 当前 high-cache 不是单纯前缀命中，而是目标比例模拟

当前 `PromptCacheTracker` 并不是直接把命中的前缀 token 原样作为 read/creation，而是先按 `targetReadRatio` 算一个目标缓存量：

```rust
target = total_input_tokens * target_read_ratio
```

证据：`src/anthropic/prompt_cache.rs:653`。

写缓存时也会把每个前缀点按 `target_tokens` 缩放：

```rust
scaled_tokens = point.cumulative_tokens / flat_total_tokens * target_tokens
```

证据：`src/anthropic/prompt_cache.rs:372`。

此外还有 token 放大：

- `tokenScale`
- `maxSimulatedInputTokens`
- `capJitterMinTokens`
- `capJitterMaxTokens`
- `scaleMinInputTokens`

证据：`src/model/config.rs:2479`。

这说明当前 high-cache 策略本质是“目标比例模拟 + 本地前缀命中”，不是 `Kiro-RS-Tool` 那种朴素缓存账本。

### 1.6 `Kiro-RS-Tool` 的 scope 逻辑

`Kiro-RS-Tool` 的 scope 不是当前项目的：

```rust
credential_id + conversation_id + model + route_namespace
```

它是把隔离种子放进 hash 链开头：

1. 优先取 `metadata.user_id` 里的 `_session_<uuid>`。
2. 如果没有 session，再退回客户端 key id。
3. 如果 session 和 client key 都没有，则不启用本地缓存写入。

证据：

- `Kiro-RS-Tool/src/anthropic/cache_metering.rs:658`
- `Kiro-RS-Tool/src/anthropic/cache_metering.rs:451`
- `Kiro-RS-Tool/src/anthropic/cache_metering.rs:452`

当前项目没有 client key id 透传。`auth_middleware` 只是判断 key 是否存在：

```rust
Some(key) if state.request_api_keys.contains(&key)
```

证据：`src/anthropic/middleware.rs:304`。

当前 `RequestApiKeyStore` 只存 SHA-256 hash 集合，没有 key id。

证据：`src/common/auth.rs:68`。

所以要实现 `Kiro-RS-Tool` 的 fallback scope，必须新增“认证后的 client key seed”传递链路。

### 1.7 `Kiro-RS-Tool` 的账本更朴素

`Kiro-RS-Tool` 的 `compute_cache_usage(...)` 逻辑是：

1. 提取前缀段。
2. 查最深命中段。
3. 全 miss 时 `cache_read = 0`。
4. 被缓存覆盖的前缀全部算 creation。
5. 请求成功后才 `commit_success()` 写缓存。
6. 最终 usage 用 `split_against_total(total_real)` 做比例分摊，保证：

```text
input + cache_creation + cache_read == total_real
```

证据：

- `Kiro-RS-Tool/src/anthropic/cache_metering.rs:86`
- `Kiro-RS-Tool/src/anthropic/cache_metering.rs:370`
- `Kiro-RS-Tool/src/anthropic/cache_metering.rs:409`
- `Kiro-RS-Tool/src/anthropic/cache_metering.rs:134`

它没有当前项目的 `targetReadRatio`、`tokenScale`、jitter、creation control、reportedUsage 采样这些层。

---

## 2. 要解决的问题拆分

### 问题 A：第一次请求可能对外显示 cache read

这是 bug 修复，应该独立于新策略。

根因：

- 底层 tracker 没有读缓存。
- reported usage 把 input 差值搬到了 `cache_read_input_tokens`。

建议修复原则：

只有底层计算结果本身已经有 `cache_read_input_tokens > 0` 时，reported usage 才允许把 input delta 继续搬到 cache read。

如果底层 read 为 0，就算 `/cc` 配了 `sample_input_max(96)`，也不能凭空制造 cache read。

建议修改点：

- `src/anthropic/cache.rs`
  - `CacheUsage::with_reported_cache_usage_policy_and_raw(...)`
  - `ReportedCacheUsagePolicy::apply_final_input_guard(...)`

建议逻辑：

```rust
let had_cache_read_before_reporting = self.cache_read_input_tokens > 0;

if policy.input_moves_delta_to_cache_read() && had_cache_read_before_reporting {
    usage.cache_read_input_tokens += input_delta;
}
```

注意：如果只是简单把最终 `cache_read_input_tokens` 强制改回 0，也能修显示问题，但会隐藏计算链路上的真实原因。更干净的方式是在“搬移 input delta”这一层加条件。

这属于正确性修复。它会改变 `/cc` first miss 的最终 usage 展示，但这是修正错误，不是新策略默认行为翻转。

### 问题 B：当前 scope 不能复现 `Kiro-RS-Tool`

当前 scope：

```rust
credential_id + conversation_id + model + route_namespace
```

影响：

- 同一个 Claude Code session，如果切换上游账号，会 miss。
- 不同客户端 key 没有参与缓存隔离。
- 没有 `session 优先，否则 client key，否则不缓存` 的语义。

建议把 scope 解析抽离成策略化逻辑，不要只在构造点手写替换 `credential_id`。

建议新增概念：

```rust
enum PromptCacheScopeMode {
    LegacyCredentialConversationModelRoute,
    ConversationModelRoute,
    KiroRsToolSessionThenClientKey,
}
```

每种模式都由一个统一函数生成 `Option<PromptCacheScope>`：

```rust
fn resolve_prompt_cache_scope(
    mode: PromptCacheScopeMode,
    payload: &MessagesRequest,
    legacy_conversation_id: Option<&str>,
    credential_id: Option<u64>,
    client_key_seed: Option<u64>,
    model: &str,
    route_namespace: Option<&str>,
) -> Option<PromptCacheScope>
```

其中 `KiroRsToolSessionThenClientKey` 的规则：

1. 从 `metadata.user_id` 提取 `_session_` 后面的 session id。
2. 如果有 session，用 session 作为隔离种子。
3. 如果没有 session，用 client key seed。
4. 如果两者都没有，返回 `None`，即不参与本地缓存。

不要在这个模式里使用当前 `extract_stable_conversation_id(...)` 的 fallback。`Kiro-RS-Tool` 没有 “system + tools + first user message 派生 conversation id” 这个 fallback。为了严格对齐，不能把这个 fallback 混进来。

### 问题 C：当前参数模型不能清楚表达“新增一种策略”

当前配置是：

```json
{
  "cachePolicy": {
    "default": {
      "simulation": {
        "targetReadRatio": 0.98,
        "tokenScale": 1.6
      }
    },
    "pathOverrides": {
      "/cc": {
        "simulation": {
          "targetReadRatio": 0.95
        }
      }
    }
  }
}
```

这个模型适合“调整当前 high-cache 策略参数”，不适合表达：

```text
/cc 使用 legacy high-cache
/kiro-tool 使用 Kiro-RS-Tool 策略
/na 禁用本地 prompt cache
```

建议新增“策略类型”，同时保留旧参数。

---

## 3. 推荐配置模型

### 3.1 增加策略类型

建议新增：

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheStrategyKind {
    Disabled,
    LegacyHighCache,
    KiroRsTool,
}
```

不要直接把现有 `PromptCacheSimulationMode` 扩成三态。原因：

- `PromptCacheSimulationMode` 当前只表达是否启用 high-cache。
- 新策略不应该叫 simulation mode。
- `KiroRsTool` 不是 high-cache 参数的另一种取值，而是另一套账本策略。

建议让 `CacheRoutePolicy` 增加：

```rust
pub strategy: PromptCacheStrategyKind
```

同时让 `CacheRoutePolicyPatch` 增加：

```rust
pub strategy: Option<PromptCacheStrategyKind>
```

兼容规则：

- 如果配置里没有 `strategy`，完全沿用当前逻辑：
  - `simulation.enabled = true` => `LegacyHighCache`
  - `simulation.enabled = false` => `Disabled`
- 如果配置里显式写了 `strategy`，则按新策略执行。

这样旧配置不需要迁移。

### 3.2 内置策略默认值

建议内置三种策略模板。

#### `disabled`

语义：

- 不构建本地 prompt cache profile。
- 不计算本地 cache read / creation。
- 不写入 `PromptCacheTracker`。

等价于当前 `simulation.enabled = false`。

#### `legacy_high_cache`

语义：

- 完全保持当前行为。
- 使用当前 `CacheSimulationPolicy`：
  - `targetReadRatio`
  - `tokenScale`
  - `maxSimulatedInputTokens`
  - jitter
  - `scaleMinInputTokens`
- 使用当前 creation control。
- 使用当前 reported usage。
- 使用当前 scope：
  - `credential_id + conversation_id + model + route_namespace`

除“第一次不能伪造 read”的 bug 修复外，这个策略不应该变。

#### `kiro_rs_tool`

语义：

- 使用 `Kiro-RS-Tool` 风格 scope：
  - session 优先
  - 无 session 时 client key seed
  - 都没有则不缓存
- 不绑定上游 credential。
- 不做 `targetReadRatio` 目标比例模拟。
- 不做 token 放大。
- 不做 jitter。
- 不做 creation frequency control。
- 不做 reported usage 采样。
- 第一次 miss 只能 creation，不能 read。
- 成功后才写入本地缓存。
- 最终 usage 满足：

```text
input_tokens + cache_creation_input_tokens + cache_read_input_tokens == total_input_tokens
```

这才是独立策略，不是把当前 high-cache 参数调成某些值。

### 3.3 路径绑定示例

推荐配置形态：

```json
{
  "cachePolicy": {
    "pathOverrides": {
      "/cc/v1/messages": {
        "strategy": "kiro_rs_tool"
      },
      "/v1/messages": {
        "strategy": "legacy_high_cache",
        "simulation": {
          "targetReadRatio": 0.98,
          "tokenScale": 1.6
        }
      },
      "/na": {
        "strategy": "disabled"
      }
    }
  }
}
```

如果希望全局默认仍是当前策略，就不写 `cachePolicy.default.strategy`。

如果希望某个路径使用 `Kiro-RS-Tool` 风格，就只在那个 path override 上显式设置：

```json
{
  "strategy": "kiro_rs_tool"
}
```

### 3.4 策略默认值与用户覆盖的关系

这里有一个容易踩坑的地方：当前 `CacheRoutePolicyPatch` 是 patch 模型，路径 override 会继承上一级配置。

如果 `/cc` 写：

```json
{
  "strategy": "kiro_rs_tool"
}
```

它不应该继续继承 `/cc` 默认 reported usage 的 `sample_input_max(96)`。否则又会回到“第一次可能伪 read”的问题。

因此，推荐解析顺序是：

1. 先得到 legacy base policy，保持旧配置兼容。
2. 应用全局 `cachePolicy.default`。
3. 命中路径 override 后：
   - 如果 override 没有 `strategy`：按当前 patch 逻辑继承并覆盖。
   - 如果 override 有 `strategy`：先切到该 strategy 的内置默认 policy，再应用这个 override 里的显式字段。

也就是说：

```text
path override 只写 strategy=kiro_rs_tool
=> 得到完整的 Kiro-RS-Tool 策略默认值

path override 写 strategy=kiro_rs_tool + bounds
=> 得到 Kiro-RS-Tool 默认值，再覆盖 bounds
```

这个规则可以避免“策略名字变了，但底下还继承 legacy reportedUsage”的混乱。

---

## 4. 缓存模块抽离建议

当前 `src/anthropic/prompt_cache.rs` 同时承担了几件事：

1. scope 类型定义。
2. profile 构建。
3. 指纹 canonical。
4. tracker 存储。
5. TTL / bounds。
6. high-cache 计算。

建议拆成模块，但不要在第一步就做大搬家。更稳妥的顺序是：先加策略边界和测试，再拆文件。

推荐最终结构：

```text
src/anthropic/prompt_cache/
  mod.rs
  scope.rs
  profile.rs
  tracker.rs
  strategy.rs
  kiro_tool.rs
  legacy.rs
```

职责：

### `scope.rs`

放：

- `PromptCacheScope`
- `PromptCacheScopeMode`
- `ClientKeySeed`
- session 提取函数
- scope resolver

注意：client key 不能明文进入 scope。只允许用 hash seed。

### `profile.rs`

放通用结构：

- `PromptCacheProfile`
- `PromptCacheLookupPoint`
- `PromptCacheBreakpoint`
- canonical JSON 工具
- TTL 解析
- token 估算工具

### `legacy.rs`

放当前 high-cache 策略逻辑：

- 当前 `build_high_cache_profile_for_model(...)`
- 当前 `targetReadRatio` 缩放逻辑
- 当前 `CacheSimulation` 转换逻辑

要求：迁移后行为不变。

### `kiro_tool.rs`

放新策略逻辑：

- `Kiro-RS-Tool` 风格 segment 提取。
- system 动态头跳过。
- session/client-key scope。
- miss / hit / commit 账本。
- `split_against_total(...)`。

这部分可以复用：

- 当前 `PromptCacheTracker` 的内存存储。
- 当前 canonical JSON 排序逻辑。
- 当前 TTL 解析。
- 当前 bounds。

但不应该复用：

- `targetReadRatio`。
- `CacheAmplification`。
- reported usage 采样。
- creation frequency control。

### `tracker.rs`

放存储：

- `PromptCacheTracker`
- `PromptCacheEntry`
- `compute/update`
- bounds enforcement

建议让 tracker 只负责：

```text
给我 scope + lookup points，我返回命中结果；成功后我写入 lookup points。
```

不要让 tracker 知道某条路径是 legacy 还是 Kiro-RS-Tool。

### `strategy.rs`

放策略调度：

```rust
enum PromptCacheExecutionStrategy {
    Disabled,
    LegacyHighCache(LegacyHighCacheOptions),
    KiroRsTool(KiroRsToolOptions),
}
```

建议用 enum + match，不建议一开始就上 trait object。原因是当前路径都在单进程内，策略数量少，enum 更容易测试和追踪。

---

## 5. `Kiro-RS-Tool` 策略的具体行为设计

### 5.1 scope 行为

严格对齐 `Kiro-RS-Tool`：

```text
if metadata.user_id contains "_session_":
    scope = session
else if authenticated client key seed exists:
    scope = client key seed
else:
    no local cache
```

当前项目需要新增 client key seed：

1. `RequestApiKeyStore` 增加 `lookup_seed(&key) -> Option<u64>`。
2. seed 用 SHA-256 digest 前 8 字节即可，不能使用原始 key。
3. `auth_middleware` 鉴权成功后把 `ClientKeySeed(seed)` 放入 request extensions。
4. handler 构建 `RequestUsageContext` 时读取 seed。
5. external pool path 也要透传同一个 seed，否则外部池投影和普通路径会不一致。

### 5.2 route namespace 要不要进入 Kiro-RS-Tool scope

严格 `Kiro-RS-Tool` 没有 route namespace。

但当前项目允许不同路径绑定不同策略。如果两个路径策略不同，却共享同一个缓存桶，可能出现难排查的问题。

建议：

- `kiro_rs_tool` 默认带策略 namespace，但不带上游 credential。
- namespace 只在 path override 影响缓存状态时出现，沿用当前 `resolve_cache_policy_for_path(...)` 的语义。
- 如需完全复刻 `Kiro-RS-Tool` 跨路径共享，可以后续加高级开关：

```json
{
  "scope": {
    "shareAcrossRoutes": true
  }
}
```

第一版不建议打开跨路径共享。

### 5.3 model 要不要进入 Kiro-RS-Tool scope

`Kiro-RS-Tool` 没有显式把 model 放入 scope。

但 Claude Code 官方文档说 model 是 cache key 的一部分。为了“严格 Kiro-RS-Tool”与“更接近官方”之间不混淆，建议明确命名：

- `kiro_rs_tool`：按 `Kiro-RS-Tool` 行为，不按上游账号隔离，不强制 model scope。
- 如果后续需要更保守版本，再新增：
  - `claude_like_local`
  - 或 `kiro_rs_tool_safe`

不要把“更安全的 model 隔离”偷偷塞进 `kiro_rs_tool`，否则名字和行为不一致。

### 5.4 profile / segment 行为

`Kiro-RS-Tool` 的关键细节：

- tools 先进入 hash 链。
- system 里如果有 cache_control，则跳过首个 cache_control 之前的动态 system block。
- message role 参与 hash。
- 最后一条 message 默认不切段，除非当前 block 显式带 cache_control。
- 成功后才 commit。
- TTL 从请求中出现过的 `cache_control.ttl` 推导，默认 5m，最大 1h。

当前项目 `flatten_cache_blocks(...)` 直接遍历全部 system block，没有跳过首个 cache_control 前的动态 system 头。

证据：`src/anthropic/prompt_cache.rs:509`。

所以 `kiro_rs_tool` 策略不能只复用当前 `build_high_cache_profile_for_model(...)`。至少需要一个新的 profile builder，或者给 profile builder 增加 policy：

```rust
enum PromptCacheProfileKind {
    LegacyHighCache,
    KiroRsTool,
}
```

---

## 6. 处理第一次伪 cache read 的推荐实现

### 6.1 最小修复

在 reported usage 把 input delta 搬进 cache read 前，加条件：

```text
只有原始 usage 已经有 cache_read_input_tokens > 0，才允许 move_delta_to_cache_read。
```

需要覆盖两个函数：

- `CacheUsage::with_reported_cache_usage_policy_and_raw(...)`
- `ReportedCacheUsagePolicy::apply_final_input_guard(...)`

### 6.2 为什么不是只在 Kiro-RS-Tool 策略修

因为这是当前 legacy high-cache 也存在的错误展示。

底层 first miss 没有 read，但 `/cc` reported usage 可以制造 read。这和策略新增无关，属于 usage 上报层的不变量问题。

建议建立一个全局不变量：

```text
reported usage 不能把一个本来没有 cache read 的本地 prompt-cache 结果改成有 cache read。
```

### 6.3 必须加的测试

新增测试：

1. 构造 `CacheUsage`：

```rust
input_tokens = 100_000
cache_creation_input_tokens = 50_000
cache_read_input_tokens = 0
```

套用 `/cc` 类似的 reported usage：

```rust
input.sample_input_max(96)
move_delta_to_cache_read = true
```

断言：

```rust
reported.cache_read_input_tokens == 0
```

2. 构造真实 read 场景：

```rust
cache_read_input_tokens > 0
```

断言原有搬移行为仍生效。

这样既修 first read，又不破坏真正 read 命中的显示逻辑。

---

## 7. 实施路径建议

### 阶段 1：修复 first-read 伪读问题

触达文件：

- `src/anthropic/cache.rs`
- 相关单测

目标：

- first miss 不能被 reported usage 改成 cache read。
- legacy high-cache 其他行为尽量不变。

这是最小闭环，应该先做。

### 阶段 2：增加 client key seed 传递

触达文件：

- `src/common/auth.rs`
- `src/anthropic/middleware.rs`
- `src/anthropic/handlers.rs`
- `src/external_pool.rs`

目标：

- 鉴权成功后产生稳定 `ClientKeySeed`。
- 不暴露明文 key。
- handler 和 external pool 都能拿到 seed。

测试：

- 同一个 key seed 稳定。
- 不同 key seed 不同。
- 未认证请求没有 seed。
- seed 不出现在日志、错误响应、usage 记录里。

### 阶段 3：抽离 scope resolver

触达文件：

- `src/anthropic/prompt_cache.rs` 或新 `prompt_cache/scope.rs`
- `src/anthropic/handlers.rs`
- `src/external_pool.rs`

目标：

- legacy scope 仍然生成当前四元组。
- `KiroRsToolSessionThenClientKey` scope 可用。
- compute 和 update 使用同一个 resolver，避免读写不同桶。

测试：

- legacy 不变。
- 同 session 不同 credential 在 Kiro 策略下命中。
- 不同 session 不互相命中。
- 没 session 时同 client key 命中。
- 没 session 且没 client key 时不缓存。

### 阶段 4：增加策略类型和路径绑定

触达文件：

- `src/model/config.rs`
- `src/anthropic/handlers.rs`
- `src/external_pool.rs`

目标：

- `strategy` 可配置。
- 不写 `strategy` 时旧配置行为不变。
- path override 可绑定 `kiro_rs_tool`。
- strategy 改变要影响 route namespace，避免跨策略共享缓存。

测试：

- 旧配置解析结果不变。
- `pathOverrides["/cc"].strategy = "kiro_rs_tool"` 只影响 `/cc`。
- 最长前缀优先仍然成立。
- reported usage-only override 不切分 cache namespace，这个当前已有语义不能破坏。
- strategy override 会切分 cache namespace。

### 阶段 5：实现 `Kiro-RS-Tool` 策略账本

触达文件：

- `src/anthropic/prompt_cache.rs` 或新 `prompt_cache/kiro_tool.rs`
- `src/anthropic/cache.rs`
- `src/anthropic/handlers.rs`
- `src/external_pool.rs`

目标：

- 新策略不走 `targetReadRatio`。
- 新策略不走 token amplification。
- 新策略不走 creation control。
- 新策略默认不走 reported usage 采样。
- first miss creation，second hit read。
- 上游成功后才写缓存。

测试：

- first miss: read = 0。
- second same prefix: read > 0。
- 上游失败不写缓存，retry 仍是 miss。
- 动态 system 头变化但稳定 cache_control 后段不变时，第二次可命中。
- 不同 session / 不同 client key 隔离。
- input + creation + read == total。

### 阶段 6：模块拆分

在行为和测试稳定后，再拆文件。

不要在同一个 PR 里同时做“大搬家 + 新策略 + bug 修复”，否则回归风险很高，也很难定位问题。

---

## 8. 内存与稳定性风险

### 8.1 scope 变宽或变窄都可能改变内存形态

legacy 当前按 credential 分桶。

Kiro-RS-Tool 策略按 session 或 client key 隔离，不绑定上游 credential。影响：

- 同 session 跨账号会共享缓存，命中率提高。
- 单个 session/client key 下可能积累更多前缀。
- 如果 session id 不做长度控制，scope key 可能变大。

建议：

- session id 不直接无限长存入 scope；可以 hash 成固定长度。
- client key 只用 hash seed。
- 保留 `PromptCacheBounds`。
- Kiro 策略默认全局上限建议不大于当前默认，甚至可以接近 `Kiro-RS-Tool` 的 4096 条。

### 8.2 不要为了复现 Kiro-RS-Tool 引入无界持久化

`Kiro-RS-Tool` 有 JSON 落盘：

- 启动读 `cache_dir/cache_metering.json`
- 每 60 秒 flush

当前项目没有持久化。

持久化不是这次最小目标。第一版建议先不做持久化，避免：

- JSON 文件无限增大。
- 写盘阻塞。
- 多进程并发写冲突。
- scope 结构变更导致兼容问题。

如果后续做持久化，必须有：

- 最大条目数。
- 最大文件大小。
- 原子写。
- 启动加载时过滤过期项。
- schema version。

### 8.3 reported usage 和真实账本要分层

`Kiro-RS-Tool` 策略应该默认原样输出账本结果，不做 reported usage 采样。

如果将来确实要对 Kiro 策略做下游显示整形，也必须保证不变量：

```text
不能把 miss 变成 read。
不能让 input + creation + read 与 total 对不上。
不能把失败请求写入缓存。
```

---

## 9. 推荐测试矩阵

### 9.1 单元测试

必须覆盖：

- legacy first miss 不显示 read。
- legacy true read 仍可按现有 reported usage 策略整形。
- `KiroRsToolSessionThenClientKey` session 优先于 client key。
- 没 session 时 fallback 到 client key。
- 没 session 且没 client key 时不缓存。
- 同 session 跨 credential 命中。
- 不同 session 隔离。
- 不同 client key 隔离。
- 动态 system 头跳过。
- 成功后才 commit，失败不 commit。
- path strategy override 不影响其他 path。
- strategy override 设置 namespace。
- bounds 生效，不会无限增长。

### 9.2 直接协议测试

用直接 HTTP/SSE 验证：

1. 清空进程内缓存。
2. 对绑定 `kiro_rs_tool` 的路径发第一次请求。
3. 断言 response usage：

```text
cache_read_input_tokens == 0
cache_creation_input_tokens > 0
```

4. 发同样前缀第二次请求。
5. 断言：

```text
cache_read_input_tokens > 0
```

6. 构造上游失败，断言不写缓存。

### 9.3 Claude Code CLI 真实验证

实现后需要使用真实 Claude Code CLI 验证，不能只靠 curl。

建议按已有 skill 约束：

- 临时端口，不碰 live `9022`。
- 隔离 `HOME` 和 `CLAUDE_CONFIG_DIR`。
- 记录 `claude --version`。
- 验证 `/cc/v1/messages` 的首轮和二轮 usage。
- 验证 CLI 不再出现 first miss 的 cache read。

### 9.4 内存测试

必须做：

- 多 session 压测。
- 多 client key 压测。
- 长 session id 输入。
- 大量不同 prompt 前缀。
- bounds 触发淘汰。
- 并发请求下 tracker 不死锁。

目标不是追求高吞吐，而是确认不会内存爆炸。

---

## 10. 最终建议

推荐改造方向：

1. 先修 first-read 伪读，这是全局 correctness bug。
2. 增加 `PromptCacheStrategyKind`，不要继续把所有行为塞进 `simulation`。
3. 保持旧配置不写 `strategy` 时行为完全不变。
4. 新增 `kiro_rs_tool` 策略，只在显式绑定路径时生效。
5. 把 scope resolver 抽出来，明确支持 `Kiro-RS-Tool` 的 `session -> client key -> no cache` 逻辑。
6. `Kiro-RS-Tool` 策略不要复用 `targetReadRatio` / token amplification / reported usage 采样。
7. 模块拆分放在行为测试稳定后做，避免大重构和行为变更混在一起。

一句话总结：

当前项目已经有路径级策略覆盖的框架，但还没有真正的“策略类型”。最稳的改法是保留现有 legacy high-cache 作为默认策略，新增一个 opt-in 的 `kiro_rs_tool` 策略；同时把“第一次 miss 不能显示 read”作为独立 bug 修掉。这样既能满足复现 `Kiro-RS-Tool`，也不会把现有路径和现有策略一起搅乱。
