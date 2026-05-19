# High-Cache 连续请求缓存断层分析与实施方案

> 2026-05 重构后补充：当前项目已经引入 `app_config`、持久化 usage、`/admin` 旧 UI 与 `/console` 新 UI。新的实施依据见 `docs/high-cache-runtime-continuity-proposal.md`。本文中未覆盖运行时配置热更新和双 UI 的旧描述，以新提案为准。

本文档记录 `promptCacheSimulationMode = "high-cache"` 下连续请求中间出现 cache read/write 断层的问题、根因分析、最终修复方案、风险边界、测试清单和实施步骤。

目标是让后续实现者脱离当前会话也能完整理解问题，并按本文稳定实施。

## 状态

- 文档状态：方案分析完成，尚未实施代码改动。
- 适用范围：仅适用于本地 high-cache usage 模拟。
- 不适用范围：不改变真实 Kiro upstream metadata cache，不改变 `local-prompt-cache` 的严格语义。

## 背景

当前项目把 Anthropic-compatible 请求代理到 Kiro upstream，并在本地支持 prompt-cache usage 模拟。high-cache 模式的目标是：当真实 upstream metadata 没有返回 cache read/write，或者没有 metadata 时，本地生成一组看起来合理且自洽的 Anthropic usage cache 字段。

相关配置包括：

```json
{
  "promptCacheSimulationMode": "high-cache",
  "promptCacheTargetReadRatio": 0.98,
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000
}
```

当前 high-cache 已经具备这些能力：

1. 即使请求没有显式 `cache_control`，也会构造稳定 prompt-cache profile。
2. cache scope 使用 `credential_id + conversation_id + model`，避免跨账号、跨会话、跨模型读缓存。
3. 首次同 scope 请求通常产生 cache creation，后续同 scope 且 prefix 命中后产生 cache read。
4. 真实 Kiro metadata 中已有非零 cache read/write 时，本地模拟不会覆盖真实值。
5. high-cache token scale 和 soft cap jitter 可以让大请求的 cache read/write 数字更大，并避免频繁固定贴到上限。

## 现象

在一组连续请求中，可能出现以下不自然现象：

```text
请求 1：cache_creation_input_tokens 很大，cache_read_input_tokens = 0
请求 2：cache_read_input_tokens 很大
请求 3：cache_read_input_tokens 很大
请求 4：cache_creation_input_tokens = 0，cache_read_input_tokens = 0
请求 5：cache_read_input_tokens 又恢复
```

也就是：同一个客户端、同一段会话、同一批连续调用中，中间某些请求突然没有 cache。

用户侧观察会觉得不科学：

1. 如果同一会话前面已经创建过缓存，后续连续请求不应该仅因为本次输入较短就完全没有 cache read。
2. cache read 应该随着连续会话保持稳定，至少不应该在活跃会话中突然归零。
3. 到后面累计上下文更多时，cache read 理论上应该更容易出现，而不是更容易断掉。

需要注意：当前问题主要与本次请求的 input token 估算有关，output token 少本身不会直接决定是否有 cache。但在真实请求中，短回复、工具调用中间态、测试类小 turn 往往也伴随本次 input 较小，所以表现上容易被误认为是“输入输出比较少导致没有缓存”。

## 当前代码链路

### 请求进入阶段

文件：`src/anthropic/handlers.rs`

`prepare_usage_context(...)` 会为请求构建 prompt-cache profile：

```rust
let prompt_cache_profile =
    if state.prompt_cache_simulation_mode == PromptCacheSimulationMode::HighCache {
        state.prompt_cache.build_high_cache_profile(payload, input_tokens)
    } else {
        state.prompt_cache.build_profile(payload, input_tokens)
    };
```

high-cache 模式使用 `build_high_cache_profile(...)`，即使请求没有显式 `cache_control`，也会合成一个稳定 prefix breakpoint。

同一请求的 cache scope conversation id 来自：

```rust
PromptCacheSimulationMode::HighCache => extract_stable_conversation_id(payload)
```

文件：`src/anthropic/converter.rs`

`extract_stable_conversation_id(...)` 的规则是：

```rust
extract_metadata_conversation_id(req).or_else(|| derive_fallback_conversation_id(req))
```

也就是：

1. 优先从 `metadata.user_id` 中提取 session id。
2. 如果没有显式 session，则从 system/tools/first user message 派生稳定 fallback conversation id。

### 账号确定后计算模拟 cache

文件：`src/anthropic/handlers.rs`

`prepare_credential_usage_context(...)` 在 provider 选中 credential 后，构造完整 scope：

```rust
PromptCacheScope {
    credential_id,
    conversation_id,
    model,
}
```

然后调用：

```rust
let prompt_usage = usage_context.prompt_cache.compute(
    scope,
    usage_context.prompt_cache_profile.as_ref(),
    usage_context.prompt_cache_target_read_ratio,
);
```

计算结果再转换为 `CacheSimulation`：

```rust
CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
    prompt_usage,
    usage_context.prompt_cache_target_read_ratio,
    usage_context.cache_amplification(),
)
```

### PromptCacheTracker 当前计算

文件：`src/anthropic/prompt_cache.rs`

当前 `PromptCacheTracker::compute(...)` 的核心逻辑是：

```rust
let min_tokens = min_cacheable_tokens_for_model(&profile.model);
let effective_ratio = effective_cache_read_ratio(profile, target_read_ratio);
let target_tokens =
    target_cache_tokens(profile.total_input_tokens, effective_ratio, min_tokens);

if target_tokens <= 0 {
    return PromptCacheUsage::default();
}
```

这个判断发生在读取同一 scope 下已有缓存 entry 之前。

`target_cache_tokens(...)` 的规则是：

```rust
target = round(total_input_tokens * target_read_ratio)
target = clamp(target, 0, total_input_tokens - 1)
if target >= min_cacheable_tokens { target } else { 0 }
```

当前最小 cacheable token：

1. 普通模型：`1024`
2. 模型名包含 `opus`：`4096`

因此如果本次请求估算 input 很小，`target_tokens` 会变成 `0`，后续逻辑不会检查当前 scope 是否已经有历史缓存。

### PromptCacheTracker 当前更新

文件：`src/anthropic/prompt_cache.rs`

`PromptCacheTracker::update(...)` 也有相同早退：

```rust
let target_tokens =
    target_cache_tokens(profile.total_input_tokens, effective_ratio, min_tokens);

if target_tokens <= 0 {
    return;
}
```

结果是：小请求不仅不会读历史缓存，也不会刷新本地缓存 TTL。

### 最终 usage 生成

文件：`src/anthropic/cache.rs`

`CacheSimulation::to_usage(...)` 会把模拟 cache 转成响应 usage：

```rust
let total_input_tokens = self
    .amplification
    .map(|amplification| amplification.apply(total_input_tokens))
    .unwrap_or_else(|| total_input_tokens.max(0));
```

然后如果有 target ratio，会进入：

```rust
to_target_ratio_usage(total_input_tokens, output_tokens, target_ratio)
```

该函数会基于最终 `total_input_tokens` 重新计算：

```text
target_cached = round(total_input_tokens * target_ratio)
target_cached <= total_input_tokens - 1
```

因此即使 tracker 层未来补上了连续 read，如果最终 `total_input_tokens` 仍然是一个很小的数，cache read 也会被压成很小的数。

这说明修复不能只改 `PromptCacheTracker::compute(...)`，还需要让 high-cache 模拟在连续会话小请求中具备一个合理的 `total_input_tokens` floor。

## 根因

根因可以拆成两层。

### 根因 1：是否有缓存完全依赖本次请求 token

当前 `compute(...)` 先计算本次请求的 `target_tokens`，如果不达标直接返回 0。

它没有考虑：

1. 同一 `credential_id + conversation_id + model` 下是否已经有未过期 cache entry。
2. 这个请求是否是活跃连续会话中的后续 turn。
3. 小请求是否只是一次中间工具调用、短追问、短恢复、状态同步或测试命令。

所以会出现：

```text
同一 scope 已有 100k cache entry
本次请求估算 input 只有 600 tokens
target_tokens < 1024
compute 直接返回 0
最终响应没有 cache
```

这就是连续请求中间断 cache 的直接原因。

### 根因 2：最终响应会被本次 total input 再次压低

即使 tracker 层允许小请求读取历史 cache，如果最终 `CacheSimulation::to_usage(...)` 仍使用本次请求的小 `total_input_tokens`，最终也会被压缩。

例如：

```text
历史 cache read floor = 100000
本次 total_input_tokens = 800
target ratio = 0.98
最终 target_cached = round(800 * 0.98) = 784
```

这会导致响应里的 read 仍然只有几百 tokens，不符合 high-cache 连续会话预期。

因此需要在 high-cache 模拟层引入连续会话 total input floor，而不是只修改 prompt-cache entry 命中逻辑。

## 不应修改的行为

以下行为必须保持不变：

1. 真实 upstream metadata cache 非零时，本地模拟不得覆盖。
2. `local-prompt-cache` 模式不得继承 high-cache 的宽松连续读逻辑。
3. 首次小请求不得凭空产生大 cache read/write。
4. 不得跨 credential 共享缓存。
5. 不得跨 model 共享缓存。
6. 不得跨 conversation id 共享缓存。
7. 非 `/v1/messages`、`/cc/v1/messages` 的普通辅助接口不得产生 usage cache 记录。
8. 失败请求不应创建新的 cache entry。
9. 达到 `promptCacheMaxSimulatedInputTokens` 时不得每次固定输出上限值，仍需经过 deterministic soft cap jitter。

## 最终方案

采用 high-cache 专用连续性策略，而不是直接改变 `PromptCacheTracker::compute(...)` 的默认语义。

### 方案总览

新增 high-cache 专用计算路径：

```rust
PromptCacheTracker::compute_high_cache(...)
```

保留现有严格路径：

```rust
PromptCacheTracker::compute(...)
```

调用规则：

```rust
match simulation_mode {
    PromptCacheSimulationMode::HighCache => compute_high_cache(...),
    PromptCacheSimulationMode::LocalPromptCache => compute(...),
    PromptCacheSimulationMode::Disabled => no simulation,
}
```

这样可以把“连续会话小请求也允许读取已有 high-cache 模拟缓存”的行为限制在 high-cache 模式内。

### 新增计算结果结构

建议新增结构，不直接给现有 `PromptCacheUsage` 增加字段，避免影响大量现有构造代码和 local-prompt-cache 测试：

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PromptCacheComputation {
    pub usage: PromptCacheUsage,
    pub simulated_total_input_floor_tokens: Option<i32>,
}
```

含义：

1. `usage`：cache creation/read 原始计算结果。
2. `simulated_total_input_floor_tokens`：仅 high-cache 连续会话小请求使用，用于告诉 `CacheSimulation` 最终响应不要按本次小 input 压低 cache。

现有 `compute(...)` 继续返回 `PromptCacheUsage`。

新增 `compute_high_cache(...)` 返回 `PromptCacheComputation`。

### compute_high_cache 详细规则

建议实现顺序如下。

#### 第一步：复用正常计算

先执行当前严格逻辑的主体：

```text
1. scope/profile 为空 => 返回 default
2. profile.breakpoints 为空 => 返回 default
3. 计算 min_tokens
4. 计算 effective_ratio
5. 计算 current_target_tokens
```

如果 `current_target_tokens > 0`，继续走现有逻辑：

1. 如果 scope 没有 entry：返回 creation。
2. 如果 lookup point 命中 entry：返回 read + creation 差额。
3. 如果没命中：返回 creation。

这部分行为与当前 `compute(...)` 一致。

如果正常路径已经产生非空 usage，`simulated_total_input_floor_tokens` 可以设置为：

```text
None
```

因为本次请求本身已经足够大，不需要连续小请求 floor。

#### 第二步：只在小请求早退场景启用连续性 fallback

当 `current_target_tokens <= 0` 时，high-cache 不应立即返回 0，而是检查同一 scope 是否已有未过期 cache entry。

触发条件必须同时满足：

```text
scope 存在
profile 存在
profile.breakpoints 非空
profile.lookup_points 非空
effective_ratio > 0
current_target_tokens <= 0
同一 scope 下存在未过期 entry
entry.cached_tokens >= min_cacheable_tokens_for_model(profile.model)
```

如果不满足任一条件，返回 default。

这保证：

1. 首次小请求不会凭空创建 cache。
2. 没有 profile 的请求不会被模拟。
3. ratio 配置为 0 时不会产生 cache。
4. 已过期 cache 不会被继续读取。

#### 第三步：选择 continuity floor

在同一 scope 的未过期 entries 中，选择一个历史 cached token 作为连续性 floor。

推荐规则：

```text
best_existing_cached_tokens = max(valid_entries.cached_tokens)
```

其中 valid entry 必须满足：

```text
entry.expires_at > now
entry.cached_tokens >= min_tokens
```

选择最大值的理由：

1. 同一 scope 已经代表同一 credential、conversation、model。
2. high-cache 模拟目标是让活跃会话表现为稳定高缓存。
3. 多轮会话中，最大 cached prefix 通常最接近历史最大稳定上下文。

如果担心 fallback conversation id 误复用，后续可以再加更保守策略。但第一版推荐保持逻辑简单，并用 scope、TTL、首请求禁止、真实 metadata 优先来控制风险。

#### 第四步：生成连续 read usage

小请求 fallback 不应该创建新 cache，只应该读取已有 cache：

```rust
PromptCacheUsage {
    cache_creation_input_tokens: 0,
    cache_read_input_tokens: best_existing_cached_tokens,
    cache_creation_5m_input_tokens: 0,
    cache_creation_1h_input_tokens: 0,
    effective_cache_ratio: Some(effective_ratio),
}
```

同时生成 total floor：

```text
simulated_total_input_floor_tokens = ceil(best_existing_cached_tokens / effective_ratio)
```

并保证：

```text
simulated_total_input_floor_tokens >= best_existing_cached_tokens + 1
simulated_total_input_floor_tokens >= min_tokens + 1
```

不要在这里直接使用 `promptCacheMaxSimulatedInputTokens`。上限和 jitter 应继续由 `CacheAmplification` 负责，这样不会出现所有请求固定贴到 300k 的问题。

#### 第五步：刷新 TTL

连续 fallback 读取到已有 entry 后，可以刷新被选中 entry 的 TTL：

```rust
entry.expires_at = now + entry.ttl
```

这与当前正常 read 命中时刷新 TTL 的行为一致。

注意：这会让活跃会话的小请求延续本地模拟缓存生命期。该行为符合连续会话 high-cache 预期，但必须仍受 entry 原始 TTL 限制：

1. 默认 `5m` entry 继续按 5 分钟滑动刷新。
2. 显式 `1h` entry 继续按 1 小时滑动刷新。
3. process restart 后内存 entry 消失。

### CacheSimulation 需要新增 total floor

文件：`src/anthropic/cache.rs`

建议给 `CacheSimulation` 新增字段：

```rust
pub simulated_total_input_floor_tokens: Option<i32>,
```

默认值为 `None`。

新增构造或 builder 方法，例如：

```rust
pub fn with_total_input_floor(mut self, floor: Option<i32>) -> Self {
    self.simulated_total_input_floor_tokens = floor.map(|v| v.max(0));
    self
}
```

或者新增完整构造函数：

```rust
pub fn from_prompt_cache_with_ratio_amplification_and_floor(
    usage: PromptCacheUsage,
    target_cache_ratio: f64,
    amplification: Option<CacheAmplification>,
    total_input_floor_tokens: Option<i32>,
) -> Option<Self>
```

`to_usage(...)` 的 total input 应按以下顺序计算：

```rust
let base_total_input_tokens = total_input_tokens
    .max(self.simulated_total_input_floor_tokens.unwrap_or(0))
    .max(0);

let total_input_tokens = self
    .amplification
    .map(|amplification| amplification.apply(base_total_input_tokens))
    .unwrap_or(base_total_input_tokens);
```

这样可以保证：

1. 小请求连续 read 不会被本次小 input 压成几百 tokens。
2. high-cache token scale 仍然只在 high-cache 模式启用。
3. `promptCacheScaleMinInputTokens` 仍然有效。
4. `promptCacheMaxSimulatedInputTokens` 和 cap jitter 仍由 `CacheAmplification` 统一处理。
5. 最终 usage 仍由 `to_target_ratio_usage(...)` 保证 `input_tokens >= 1`。

### Handler 调用方式

文件：`src/anthropic/handlers.rs`

`prepare_credential_usage_context(...)` 中应按 simulation mode 分支：

```rust
match usage_context.simulation_mode {
    PromptCacheSimulationMode::HighCache => {
        let computation = usage_context.prompt_cache.compute_high_cache(
            scope,
            usage_context.prompt_cache_profile.as_ref(),
            usage_context.prompt_cache_target_read_ratio,
        );

        usage_context.simulated_usage =
            CacheSimulation::from_prompt_cache_with_ratio_amplification_and_floor(
                computation.usage,
                usage_context.prompt_cache_target_read_ratio,
                usage_context.cache_amplification(),
                computation.simulated_total_input_floor_tokens,
            );
    }
    PromptCacheSimulationMode::LocalPromptCache => {
        let prompt_usage = usage_context.prompt_cache.compute(
            scope,
            usage_context.prompt_cache_profile.as_ref(),
            usage_context.prompt_cache_target_read_ratio,
        );

        usage_context.simulated_usage =
            CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
                prompt_usage,
                usage_context.prompt_cache_target_read_ratio,
                None,
            );
    }
    PromptCacheSimulationMode::Disabled => {
        usage_context.simulated_usage = None;
    }
}
```

注意：local-prompt-cache 不应传入 high-cache amplification，也不应使用 continuity floor。

## 行为示例

### 首次小请求

输入：

```text
scope = session-a
本次 input = 600
scope 下没有历史 entry
```

输出：

```text
cache_creation_input_tokens = 0
cache_read_input_tokens = 0
```

理由：不能让小测试请求凭空产生缓存。

### 首次大请求

输入：

```text
scope = session-a
本次 input = 80000
target ratio ~= 0.98
scope 下没有历史 entry
```

输出：

```text
cache_creation_input_tokens ~= 78400
cache_read_input_tokens = 0
```

请求成功后，`update(...)` 写入本地 entry。

### 后续大请求命中

输入：

```text
scope = session-a
本次 input = 85000
scope 下已有匹配 entry
```

输出：

```text
cache_read_input_tokens > 0
cache_creation_input_tokens = 0 或较小
```

与现有行为一致。

### 后续小请求连续 fallback

输入：

```text
scope = session-a
本次 input = 700
scope 下已有未过期 entry.cached_tokens = 78000
target ratio ~= 0.98
```

tracker 层输出：

```text
cache_creation_input_tokens = 0
cache_read_input_tokens = 78000
simulated_total_input_floor_tokens ~= ceil(78000 / 0.98) = 79592
```

CacheSimulation 层再应用 scale/cap/jitter：

```text
base_total = max(700, 79592)
scaled_total = round(base_total * promptCacheTokenScale)
soft_cap = promptCacheMaxSimulatedInputTokens - deterministic_jitter
final_total = min(scaled_total, soft_cap)
final_cache_read = round(final_total * effective_ratio)
```

最终响应会表现为同一连续会话仍然有稳定 cache read，而不是突然归零。

### 不同 credential

输入：

```text
scope = credential 2 + session-a + same model
credential 1 下有历史 entry
credential 2 下没有历史 entry
```

输出：

```text
cache_creation_input_tokens = 0
cache_read_input_tokens = 0
```

理由：credential 变化代表上游账号变化，不应跨账号模拟读取缓存。

### 不同 model

输入：

```text
scope = same credential + same session + model B
model A 下有历史 entry
```

输出：

```text
cache_creation_input_tokens = 0
cache_read_input_tokens = 0
```

理由：model 是 cache scope 的一部分，不跨模型共享。

## 风险分析

### 风险 1：同一 session id 被客户端复用于多个任务

如果客户端把 `metadata.user_id` 当作长期用户 id，而不是一次任务/会话 id，则同一用户不同任务可能共享 high-cache scope。

连续 fallback 会放大这个风险，因为小请求也能继承历史 cache。

缓解：

1. scope 仍包含 credential 和 model。
2. 只读取未过期 entry。
3. 首次小请求不创建 cache。
4. 真实 metadata cache 非零时本地模拟不覆盖。
5. 如果后续观察到误复用，可以扩展 scope，把 stable prompt anchor hash 加入 high-cache scope，或对 fallback read 要求当前 profile 与历史 entry 有 fingerprint 交集。

第一版不建议扩大 scope，否则会削弱连续会话模拟效果。

### 风险 2：fallback conversation id 误复用

缺少 metadata session 时，high-cache 会从 system/tools/first user message 派生 stable conversation id。如果两个独立请求 system/tools/first user 都相同，可能在 TTL 内共享同一个 fallback scope。

缓解：

1. TTL 默认 5 分钟。
2. process restart 清空内存 entry。
3. 不同 credential/model 不共享。
4. 首次小请求不创建 cache。

如果需要更保守，可以在 `PromptCacheScope` 中增加 `conversation_source` 或 `scope_strength`，让显式 metadata session 允许宽松 continuity fallback，而 fallback-derived session 只允许 fingerprint 命中后 read。第一版可以暂不引入，避免结构变复杂。

### 风险 3：小请求 read 数字过大

连续 fallback 可能让本次只有几十 tokens 的请求返回几十 k 或上百 k cache read。

这是 high-cache 模拟想解决的问题，但不能无边界。

边界：

1. 只有已有历史 entry 才允许。
2. 不创建新 cache。
3. 最终 total 仍受 `promptCacheMaxSimulatedInputTokens` soft cap 控制。
4. soft cap 有 deterministic jitter，不会固定贴顶。
5. `input_tokens >= 1` 的自洽约束保持。

### 风险 4：TTL 被小请求持续刷新

连续小请求 fallback 会刷新 entry TTL，活跃会话可能长期保持 cache。

这是 prompt cache 活跃续期的合理模拟，但要注意：

1. 只刷新被选中的 entry。
2. 仍使用 entry 自己的 ttl。
3. 失败请求是否刷新与当前正常 read 命中行为保持一致。

如果未来需要更严格，可以把 TTL 刷新移动到 `record_success(...)` 后执行，但这会比当前行为改动更大。

### 风险 5：统计占比显著升高

修复后，中间小请求也可能被计入 high-cache read，请求级缓存占比、超过 100k/200k 的 read 占比都会提高。

这是预期效果，但发布前必须重新跑统计：

1. cache 请求占全部 message 请求比例。
2. `cache_read_input_tokens > 100k` 占 cache 请求比例。
3. `cache_read_input_tokens > 200k` 占 cache 请求比例。
4. `cache_creation_input_tokens > 100k` 占 cache 请求比例。
5. `cache_creation_input_tokens > 200k` 占 cache 请求比例。
6. first small request 是否仍为 0。

### 风险 6：local-prompt-cache 被误改

如果直接修改 `compute(...)`，`local-prompt-cache` 也会拥有宽松连续 fallback，这会改变严格 prompt-cache 语义。

因此必须新增 high-cache 专用方法，或给内部实现加明确 policy 参数并保持 `compute(...)` 默认严格。

第一版推荐新增方法，代码意图最清晰。

### 风险 7：并发请求顺序

同一 scope 并发请求可能出现：

1. 多个请求同时发现没有 entry，于是都报告 creation。
2. 一个请求成功更新 entry 后，后续请求才开始 read。

这是当前内存 tracker 的自然限制。连续 fallback 不会解决首批并发种子请求问题，也不应该为了模拟效果在请求未成功前创建 entry。

可接受行为：

```text
seed 请求成功前，并发请求可能都 creation 或 no cache
seed 请求成功后，后续连续请求 read
```

### 风险 8：失败请求

失败请求不应创建新 entry。当前新 entry 写入发生在 `record_success(...)` 调用 `prompt_cache.update(...)` 时。

连续 fallback 在 `compute_high_cache(...)` 阶段可能会刷新已存在 entry TTL，这与当前正常 read 命中时的刷新行为一致。但它不应创建新 entry。

## 实施步骤

### 步骤 1：新增 PromptCacheComputation

文件：`src/anthropic/prompt_cache.rs`

新增：

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PromptCacheComputation {
    pub usage: PromptCacheUsage,
    pub simulated_total_input_floor_tokens: Option<i32>,
}
```

### 步骤 2：抽取 compute 内部公共逻辑

为了避免复制过多代码，可将当前 `compute(...)` 主体抽成内部方法：

```rust
fn compute_with_policy(
    &self,
    scope: Option<PromptCacheScope>,
    profile: Option<&PromptCacheProfile>,
    target_read_ratio: f64,
    allow_high_cache_continuity: bool,
) -> PromptCacheComputation
```

但对外保持：

```rust
pub fn compute(...) -> PromptCacheUsage {
    self.compute_with_policy(..., false).usage
}

pub fn compute_high_cache(...) -> PromptCacheComputation {
    self.compute_with_policy(..., true)
}
```

### 步骤 3：实现 continuity fallback

在 `current_target_tokens <= 0` 时：

1. 如果 `allow_high_cache_continuity = false`，返回 default。
2. 如果 `allow_high_cache_continuity = true`，查找同 scope 未过期 entries。
3. 找出 `best_existing_cached_tokens`。
4. 如果不存在有效 entry，返回 default。
5. 刷新被选中 entry TTL。
6. 返回 read-only usage 和 total floor。

伪代码：

```rust
if target_tokens <= 0 {
    if !allow_high_cache_continuity {
        return PromptCacheComputation::default();
    }

    let mut entries_by_scope = self.entries.lock();
    prune_expired_locked(&mut entries_by_scope, now);

    let Some(entries) = entries_by_scope.get_mut(&scope) else {
        return PromptCacheComputation::default();
    };

    let Some(best_entry) = entries
        .values_mut()
        .filter(|entry| entry.expires_at > now)
        .filter(|entry| entry.cached_tokens >= min_tokens)
        .max_by_key(|entry| entry.cached_tokens)
    else {
        return PromptCacheComputation::default();
    };

    best_entry.expires_at = now + chrono::Duration::from_std(best_entry.ttl).unwrap_or_default();

    let read_tokens = best_entry.cached_tokens.max(0);
    let floor = continuity_total_floor(read_tokens, effective_ratio, min_tokens);

    return PromptCacheComputation {
        usage: PromptCacheUsage {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: read_tokens,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            effective_cache_ratio: Some(effective_ratio),
        },
        simulated_total_input_floor_tokens: Some(floor),
    };
}
```

辅助函数：

```rust
fn continuity_total_floor(read_tokens: i32, effective_ratio: f64, min_tokens: i32) -> i32 {
    if read_tokens <= 0 || effective_ratio <= 0.0 {
        return 0;
    }

    let floor = ((read_tokens as f64) / effective_ratio.clamp(0.01, 0.99)).ceil() as i32;
    floor
        .max(read_tokens.saturating_add(1))
        .max(min_tokens.saturating_add(1))
}
```

### 步骤 4：给 CacheSimulation 增加 total floor

文件：`src/anthropic/cache.rs`

新增字段：

```rust
pub simulated_total_input_floor_tokens: Option<i32>,
```

所有构造函数默认填 `None`。

新增构造函数：

```rust
pub fn from_prompt_cache_with_ratio_amplification_and_floor(
    usage: PromptCacheUsage,
    target_cache_ratio: f64,
    amplification: Option<CacheAmplification>,
    total_input_floor_tokens: Option<i32>,
) -> Option<Self>
```

在 `to_usage(...)` 开头应用 floor：

```rust
let base_total_input_tokens = total_input_tokens
    .max(self.simulated_total_input_floor_tokens.unwrap_or(0))
    .max(0);

let total_input_tokens = self
    .amplification
    .map(|amplification| amplification.apply(base_total_input_tokens))
    .unwrap_or(base_total_input_tokens);
```

### 步骤 5：handlers high-cache 分支调用新方法

文件：`src/anthropic/handlers.rs`

修改 `prepare_credential_usage_context(...)`：

1. high-cache 使用 `compute_high_cache(...)`。
2. local-prompt-cache 使用旧 `compute(...)`。
3. high-cache 构建 `CacheSimulation` 时传入 `simulated_total_input_floor_tokens`。

### 步骤 6：保持 update 行为

第一版不需要修改 `PromptCacheTracker::update(...)`。

理由：

1. 小请求连续 fallback 不应该创建新 entry。
2. `compute_high_cache(...)` 已经可以刷新读取到的 entry TTL。
3. 大请求成功后的新 entry 创建仍由现有 `record_success(...) -> update(...)` 完成。

如果未来要把 TTL 刷新移动到成功后执行，再单独设计。

## 测试清单

### prompt_cache.rs 单元测试

新增测试 1：high-cache 连续小请求读取已有 scope cache

流程：

```text
1. 创建 tracker。
2. 构造同一 scope 的大请求 profile，input_tokens = 80000。
3. compute_high_cache 第一次应 creation > 0，read = 0。
4. update 成功写入 entry。
5. 构造同一 scope 的小请求 profile，input_tokens = 600。
6. compute_high_cache 应 read > 0，creation = 0。
7. 返回的 simulated_total_input_floor_tokens 应 > 600。
```

新增测试 2：high-cache 首次小请求不创建 cache

流程：

```text
1. scope 下没有 entry。
2. 小请求 input_tokens = 600。
3. compute_high_cache 返回 read = 0，creation = 0，floor = None。
```

新增测试 3：local-prompt-cache 小请求保持旧行为

流程：

```text
1. 先用大请求在同 scope 写入 entry。
2. 小请求调用 compute(...)，不是 compute_high_cache(...)。
3. 返回 read = 0，creation = 0。
```

这能保证 high-cache 新逻辑没有污染 local-prompt-cache。

新增测试 4：scope 隔离

覆盖：

```text
credential_id 不同 => 不读
conversation_id 不同 => 不读
model 不同 => 不读
```

新增测试 5：ratio 为 0 不 fallback

流程：

```text
1. scope 下已有 entry。
2. target_read_ratio = 0。
3. compute_high_cache 小请求返回 0。
```

新增测试 6：过期 entry 不 fallback

如果测试中不方便操作时间，可以新增内部 helper 或构造短 TTL entry。目标是确认 `prune_expired_locked(...)` 后不会读过期缓存。

### cache.rs 单元测试

新增测试 1：total floor 让小请求 read 保持可见

输入：

```text
CacheSimulation:
  cache_read_input_tokens = 78000
  target_cache_ratio = 0.98
  amplification = Some(scale 1.6, cap 300000, scale_min 20000)
  floor = 79592

to_usage(total_input_tokens = 700)
```

断言：

```text
usage.total_input_tokens >= 79592
usage.cache_read_input_tokens > 700
usage.input_tokens >= 1
```

新增测试 2：没有 floor 时小请求仍不放大

保留当前 `amplification_does_not_scale_small_requests` 语义，确保短测试请求不会因为新增字段自动变大。

新增测试 3：floor 仍受 cap jitter 控制

输入：

```text
floor = 250000
scale = 1.6
max = 300000
jitter = 12000..24000
```

断言：

```text
usage.total_input_tokens < 300000
usage.total_input_tokens 在 soft cap 范围内
```

### handlers.rs 集成型单元测试

新增或扩展测试：

1. high-cache 模式调用 `compute_high_cache(...)` 后，小请求同 scope 生成 simulated usage。
2. local-prompt-cache 模式仍调用旧 compute，不出现 continuity floor。
3. upstream metadata 非零 cache 时，最终 usage 使用 metadata，不使用 simulation。
4. upstream metadata cache 为 0 且 high-cache 有 simulation 时，允许 fill。

### 手工回归测试

本地启动后执行：

```text
1. 清空 usage 记录。
2. 固定 session id。
3. 先发一个大上下文请求。
4. 连续发多个短请求、工具结果请求、流式短请求、非流式短请求。
5. 查看每条 usage。
```

预期：

```text
首次大请求 creation > 0，read = 0
后续短请求 read > 0，不应中间突然为 0
首次短测试请求仍为 0
不同 session/credential/model 不串 cache
```

## 验收标准

实现完成后，必须同时满足以下条件。

### 功能验收

1. 同一 high-cache scope 内，大请求成功创建 entry 后，后续小请求能产生 cache read。
2. 首次小请求不会产生 cache read/write。
3. 小请求连续 read 的最终响应 cache read 不会被本次小 input 压成几百 tokens。
4. high-cache 大请求原有 creation/read 行为不退化。
5. local-prompt-cache 模式行为不变。
6. 真实 upstream metadata 非零 cache 时，本地模拟不覆盖。
7. metadata cache 为 0 时，high-cache simulation 仍可填充。

### 安全验收

1. 不跨 credential 读 cache。
2. 不跨 conversation id 读 cache。
3. 不跨 model 读 cache。
4. 失败请求不创建新 cache entry。
5. 过期 entry 不被读取。
6. `input_tokens` 不为负，且最终 usage 自洽。
7. `cache_creation_input_tokens + cache_read_input_tokens <= total_input_tokens - 1`。

### 统计验收

至少统计以下指标：

```text
message 请求总数
带 cache 请求数
带 cache 请求占总 message 请求比例
cache_read_input_tokens > 100000 的请求数和占比
cache_read_input_tokens > 200000 的请求数和占比
cache_creation_input_tokens > 100000 的请求数和占比
cache_creation_input_tokens > 200000 的请求数和占比
```

期望趋势：

1. 连续 high-cache 会话中，cache read 覆盖率上升。
2. 中间小请求不再大量掉到 0。
3. 首次小测试请求仍不进入 cache 请求统计。
4. 超过 100k/200k 的 read 比例提升，但不会所有请求固定接近 300k。

## 不推荐方案

### 不推荐 1：直接降低 min cacheable tokens

例如把普通模型 `1024` 改成 `100`。

问题：

1. 首次小请求也会更容易出现 cache。
2. local-prompt-cache 语义会被污染。
3. 不能解决最终 `to_usage(...)` 被小 total input 压低的问题。

### 不推荐 2：直接对所有小请求塞固定 read

例如：

```text
if high-cache && input 小 => cache_read = 200000
```

问题：

1. 首次测试请求会异常。
2. 不依赖 scope，没有连续性含义。
3. 数字固定，容易显得不自然。
4. 与现有 cap jitter 设计冲突。

### 不推荐 3：修改真实 metadata 优先级

不能为了模拟大 cache 覆盖真实 upstream 已返回的非零 metadata cache。

真实 metadata 是权威来源，必须保留。

### 不推荐 4：跨 credential 共享 cache

credential 代表不同 Kiro 账号。真实 prompt cache 不应跨账号共享，本地模拟也不应该跨账号共享。

## 回滚策略

如果实施后统计明显异常，可以按以下顺序回滚：

1. 在 handler 中让 high-cache 暂时改回调用旧 `compute(...)`。
2. 保留 `CacheSimulation` floor 字段但不传入 floor。
3. 如果仍异常，再移除 `compute_high_cache(...)` 分支。

由于方案不修改真实 metadata 优先级，也不修改 upstream 请求体，回滚主要影响本地 usage 数字，不应影响 Kiro 实际调用成功率。

## 最终结论

当前连续请求中间 cache 断层的问题真实存在。

直接原因是：当前 high-cache 模拟先按本次请求 token 计算 `target_tokens`，本次小请求低于 cacheable 门槛时直接返回 0，没有检查同一 scope 是否已有历史缓存。

完整修复必须同时做两件事：

1. 在 high-cache 模式中新增连续会话 fallback：小请求只有在同一 scope 已存在未过期 cache entry 时，才能读取历史 cache。
2. 在 `CacheSimulation` 中新增 total input floor：避免连续小请求虽然 tracker 层读到了历史 cache，但最终 usage 又被本次小 input 压成很小。

该修复应严格限制在 high-cache 模拟路径内，不影响真实 cache、不影响 local-prompt-cache、不让首次小测试请求凭空产生缓存，并继续使用现有 scale/cap/jitter 机制保证数字自然波动。
