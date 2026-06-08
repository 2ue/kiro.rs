# Prompt Cache Simulation Strategy

本文档描述当前 `kiro.rs` 的缓存模拟策略、配置入口、默认行为和排查口径。它面向后续维护者，脱离历史对话也应能判断“为什么会出现 cache read / cache creation、哪些配置会影响下游看到的 usage、哪些配置不会影响真实上游请求”。

## 结论

当前系统有三类和缓存相关但语义不同的能力：

1. 本地 prompt-cache tracker：在本进程内模拟 Anthropic prompt cache 的创建和读取，用来生成 `cache_creation_input_tokens`、`cache_read_input_tokens` 等 usage 字段。
2. 路径级 usage 上报投影：按入口路径把已经计算出的 usage 投影成下游看到的 usage，例如 `/cc` 默认压低 `input_tokens` 并把差值转入 `cache_read_input_tokens`。
3. 外部备用池 usage 整形：外部池请求和正文默认透传；只有单个外部池设置 `usageProjectionMode=current_path_policy` 时，才会按当前入口路径的 `reportedUsage` 策略改写外部池返回体里的 `usage` 字段，并额外对 cache read / cache creation 做上浮。

最重要的边界：

- 缓存模拟只影响下游响应里的 `usage` 和后台 usage record，不改变发给 Kiro 的请求内容。
- `reportedUsage` 只改变下游和后台看到的 usage，不改变本地 tracker 的命中状态。
- `promptCacheCreationControl` 只限制最终上报的 `cache_creation_input_tokens` 出现频次，不改变 tracker 是否已经创建缓存，也不改变 `cache_read_input_tokens` 的命中计算。
- 真实上游 metadata 里已经有非零 cache read/write 时，真实 metadata 优先，不用本地模拟覆盖。
- 真实上游 metadata 存在但 cache read/write 都为 0 时，在 high-cache 模式下可以用本地模拟补足 cache usage。
- 外部池 `pass_through` 是严格透传 usage；`current_path_policy` 才会改写 usage。

## 代码入口

主要实现文件：

- `src/anthropic/prompt_cache.rs`：本地 prompt cache profile、fingerprint、TTL、命中和更新。
- `src/anthropic/cache.rs`：把真实 metadata、本地模拟和路径级上报策略合成为 Anthropic-compatible `usage`。
- `src/anthropic/prompt_cache_creation_control.rs`：限制本地模拟 `cache_creation_input_tokens` 的最终上报频次。
- `src/anthropic/handlers.rs`：请求生命周期中构建 profile、绑定凭据后计算本地 cache usage、成功后更新 tracker、记录 usage。
- `src/anthropic/stream.rs`：流式响应的首尾 usage 生成和最终 usage 投影。
- `src/anthropic/router.rs`：把 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 消息路径固定为 high-cache 模式。
- `src/model/config.rs`：运行时配置结构和默认值。
- `src/external_pool.rs`：外部备用池 usage 透传/投影、cache 上浮和成本保底记录。

## 请求生命周期

### 1. 路由进入 high-cache 模式

当前消息路径：

- `/v1/messages`
- `/cc/v1/messages`
- `/ha/v1/messages`
- `/na/v1/messages`

都会进入 `PromptCacheSimulationMode::HighCache`。这不等于一定向下游展示模拟 cache；最终展示仍受 `reportedUsage` 路径策略控制。

### 2. 生成稳定会话 ID

本地 prompt cache 需要稳定 scope。当前稳定会话 ID 规则是：

1. 优先从 `metadata.user_id` 中提取 `session_id` UUID。
2. 如果没有 metadata session，在 high-cache 模式下从 `system + tools + first_user_message` 派生确定性 UUID。
3. 如果请求没有 user message，则从 `system + tools + messages` 派生。

影响：

- 同一客户端会话如果能传稳定 metadata session，缓存命中最稳定。
- 没传 metadata 时，只要首条 user/system/tools 稳定，也可以在同进程内得到稳定 scope。
- 如果客户端每轮首条 user 或工具定义大幅变化，派生会话 ID 会变化，cache read 会降低。

### 3. 构建 cache profile

`PromptCacheTracker` 会把以下内容按顺序摊平成 cache blocks：

- request prelude：模型、tool_choice。
- tools：名称、描述、input_schema、max_uses、cache_control。
- system blocks。
- messages blocks。

high-cache 模式即使没有显式 `cache_control`，也会合成一个 5 分钟稳定前缀 breakpoint，让普通 Claude Code 请求也能模拟首轮 creation、后续 read。

显式 `cache_control` 时支持：

- 默认 `ephemeral`：5 分钟。
- `ttl` 大于 5 分钟或写成 `1h` 等：归一为 1 小时。

特殊处理：

- canonical fingerprint 会忽略 `cache_control` 字段本身，避免只因为 cache marker 变化导致前缀不同。
- `tool_index`、`system_index`、`message_index`、`block_index` 等位置字段在 fingerprint 中归一为 `null`。
- 文本块如果以 `x-anthropic-billing-header:` 开头，不会作为可缓存内容，避免计费 header 干扰缓存。

### 4. 绑定凭据后计算 cache usage

本地 cache scope 默认为：

```text
credential_id + conversation_id + model
```

其中 model 使用解析后的上游模型名；没有解析结果时使用请求模型名。

这意味着默认情况下：

- 同一会话、同一模型、同一凭据，首轮会 creation，后续相同/增长前缀会 read。
- 换凭据后不会读到另一个凭据创建过的本地缓存。
- 换模型后不会共享缓存。

模型最小可缓存 token 门槛：

- 普通模型：1024 tokens。
- Haiku 3.5：2048 tokens。
- Haiku 4.x：4096 tokens。
- Opus 4.5 / 4.6 / 4.7 以及 `opus`、`opusplan`：4096 tokens。

注意：当前 `min_cacheable_tokens_for_model` 里 Opus 4.8 没有显式列入 4096 特例，会走普通 1024 门槛。这只影响本地模拟的最小可缓存门槛，不影响真实上游模型能力、模型映射或请求发送。若希望 Opus 4.8 也按 4096 门槛，应补充该函数或改成能力表驱动。

### 5. 真实 metadata 和本地模拟如何合并

生成 usage 时优先级：

1. 上游 metadata 有非零 `cache_read_input_tokens` 或 `cache_write_input_tokens`：直接使用真实 metadata。
2. 上游 metadata 存在但 cache read/write 都为 0，且 high-cache 模式下本地模拟有 cache：使用本地模拟补足 cache usage。
3. 没有 metadata，但有 context usage 估算或请求估算：使用本地模拟或估算值生成 usage。

`UsageSource` 的判定结果会写入后台记录：

- `upstream_metadata`：使用真实上游 metadata。
- `local_prompt_cache`：使用本地 prompt cache 模拟或用本地模拟补足零 cache metadata。
- `context_estimate`：没有 metadata，但有上下文 token 估算。
- `request_estimate`：只有请求侧估算。

### 6. 成功后更新 tracker

只有成功请求，并且 usage source 是 `local_prompt_cache` 时，才会调用 `PromptCacheTracker::update` 写入本地缓存 fingerprint。

失败请求不会更新 tracker，所以不会因为失败请求“创建缓存”。

## 默认配置

代码默认值在 `src/model/config.rs`。当前默认配置等价于：

```json
{
  "promptCacheTargetReadRatio": 0.98,
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000,
  "promptCacheCreationControl": {
    "enabled": false,
    "scopeMode": "credential_conversation_model",
    "minSuccessfulRequestsBetweenCreation": 3,
    "minCreationIntervalSecs": 60,
    "minCreationDeltaTokens": 12000,
    "maxCreationTokensPerEvent": 30000,
    "creationBudgetWindowSecs": 300,
    "maxCreationTokensPerWindow": 120000,
    "expireAfterIdleSecs": 3600
  },
  "reportedUsage": {
    "default": {
      "enabled": true,
      "input": {
        "mode": "raw",
        "maxTokens": 0,
        "targetTokens": 0,
        "normalMaxMultiplier": 1.1,
        "moveDeltaToCacheRead": false
      },
      "output": {
        "mode": "raw",
        "maxTokens": 0,
        "targetTokens": 0,
        "normalMaxMultiplier": 1.1,
        "moveDeltaToCacheRead": false
      },
      "cacheRead": {
        "mode": "preserve",
        "maxTokens": 0,
        "targetTokens": 0,
        "normalMaxMultiplier": 1.1,
        "moveDeltaToCacheRead": false
      },
      "cacheCreation": {
        "mode": "preserve",
        "maxTokens": 0,
        "targetTokens": 0,
        "normalMaxMultiplier": 1.1,
        "moveDeltaToCacheRead": false
      }
    },
    "pathOverrides": {
      "/na": {
        "enabled": false,
        "input": { "mode": "raw", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false },
        "output": { "mode": "raw", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false },
        "cacheRead": { "mode": "preserve", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false },
        "cacheCreation": { "mode": "preserve", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false }
      },
      "/cc": {
        "enabled": true,
        "input": { "mode": "sample-max", "maxTokens": 96, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": true },
        "output": { "mode": "raw", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false },
        "cacheRead": { "mode": "preserve", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false },
        "cacheCreation": { "mode": "sample-target", "maxTokens": 0, "targetTokens": 3000, "normalMaxMultiplier": 1.2, "moveDeltaToCacheRead": false }
      },
      "/ha": {
        "enabled": true,
        "input": { "mode": "sample-max", "maxTokens": 96, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": true },
        "output": { "mode": "raw", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false },
        "cacheRead": { "mode": "preserve", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false },
        "cacheCreation": { "mode": "preserve", "maxTokens": 0, "targetTokens": 0, "normalMaxMultiplier": 1.1, "moveDeltaToCacheRead": false }
      }
    }
  },
  "highCacheThreshold": 10000,
  "externalPools": {
    "externalPoolUsageProjectionUpliftPercent": 25
  }
}
```

`config.example.json` 不一定列出所有字段；缺失字段会使用这些代码默认值。后台运行配置页面会展示并保存这些字段。

## 配置项说明

### `promptCacheTargetReadRatio`

默认：`0.98`。

含义：本地模拟中，目标 cache token 占模拟 total input 的中心比例。实际值会围绕目标值做约 `±0.03` 的确定性浮动，并限制在 `0..0.99`。

影响：

- 影响本地 `cache_read_input_tokens` / `cache_creation_input_tokens` 的目标体量。
- 不会凭空跨 scope 读缓存；没有已创建 fingerprint 时首轮仍是 creation。
- 不影响真实上游 metadata。

### `promptCacheTokenScale`

默认：`1.6`，允许范围 `1.0..3.0`。

含义：high-cache 本地模拟专用的 total input 放大倍数。只有基础输入达到 `promptCacheScaleMinInputTokens` 后才启用。

影响：

- 只影响本地模拟 cache usage 的计算基础。
- 不改变发给 Kiro 的请求。
- 不代表最终下游看到的 `input_tokens` 一定放大，最终还会经过 `reportedUsage` 投影。

### `promptCacheMaxSimulatedInputTokens`

默认：`300000`，`0` 表示不设置上限。

含义：放大后的模拟 total input 上限。触顶时会扣掉一个确定性 jitter，让结果不要总是固定等于上限。

### `promptCacheCapJitterMinTokens` / `promptCacheCapJitterMaxTokens`

默认：`12000` / `24000`。

含义：当模拟 total input 触顶时，从上限扣减的范围。实际最大 jitter 还会限制在上限的约 8% 以内。

### `promptCacheScaleMinInputTokens`

默认：`20000`。

含义：基础输入达到多少 tokens 后才启用 `promptCacheTokenScale`。

建议：

- 如果希望短请求也被放大，可降低该值。
- 如果只希望长会话体现高缓存，可保持默认或升高。

### `promptCacheCreationControl`

默认：关闭。

作用：控制本地模拟最终上报的 `cache_creation_input_tokens` 出现频次。它不改变本地 tracker 是否创建缓存，也不改变是否能够产生 cache read。

字段：

| 字段 | 默认 | 作用 |
| --- | ---: | --- |
| `enabled` | `false` | 总开关。关闭时保持旧行为。 |
| `scopeMode` | `credential_conversation_model` | 控制频次状态维度。 |
| `minSuccessfulRequestsBetweenCreation` | `3` | 同一维度下，两次 creation 上报之间至少间隔多少次成功请求。 |
| `minCreationIntervalSecs` | `60` | 同一维度下，两次 creation 上报之间至少间隔多少秒。 |
| `minCreationDeltaTokens` | `12000` | 被抑制的 creation 累计到多少 tokens 后才允许下一次 creation。 |
| `maxCreationTokensPerEvent` | `30000` | 单次最多上报多少 creation tokens；超出部分回到 `input_tokens`。 |
| `creationBudgetWindowSecs` | `300` | creation 额度窗口长度，`0` 表示关闭窗口额度。 |
| `maxCreationTokensPerWindow` | `120000` | 单个窗口内最多上报多少 creation tokens，`0` 表示不限制。 |
| `expireAfterIdleSecs` | `3600` | 控制器状态空闲多久后清理，`0` 表示不按空闲清理。 |

`scopeMode` 可选：

- `credential_conversation_model`：按凭据 + 会话 + 模型控制。最贴近真实账号缓存隔离，默认值。
- `conversation_model`：按会话 + 模型控制。跨凭据共享 creation 频次状态，适合减少调度换号后的重复 creation 上报。

当 creation 被抑制时：

- 被抑制的 tokens 会加回 `input_tokens`。
- `total_input_tokens = input_tokens + cache_read_input_tokens + cache_creation_input_tokens` 的口径保持一致。
- `cache_creation_5m_input_tokens` / `cache_creation_1h_input_tokens` 会按允许的 creation 上限截断。

## `reportedUsage` 路径上报策略

`reportedUsage` 是最终下游响应和后台 usage record 的投影层。它只在 usage source 是 `local_prompt_cache` 且 high-cache 模式时处理本地模拟 usage。真实上游 metadata 的非零 cache 不会被这层本地模拟策略覆盖。

匹配规则：

- 先使用 `reportedUsage.default`。
- 再按 `pathOverrides` 做最长路径前缀匹配。
- `/cc` 会匹配 `/cc/v1/messages`。
- `/ha`、`/na` 和 `/cc` 互相独立，不继承彼此策略。

字段策略：

| mode | 含义 |
| --- | --- |
| `raw` | 使用原始请求/上游字段。对 input 来说是原始请求 token；对 output 来说优先使用上游输出，缺失时用本地估算。 |
| `preserve` | 保留计算后的字段，例如本地 high-cache 计算出的 cache read/write。 |
| `sample-max` | 在 `maxTokens` 以内做确定性采样，结果偏自然分布，不固定贴上限。 |
| `sample-target` | 围绕 `targetTokens` 做采样，常规最大值为 `targetTokens * normalMaxMultiplier`。 |

`moveDeltaToCacheRead`：

- 主要用于 input 字段。
- 当 input 被 sample 降低时，把降低的差值转入 `cache_read_input_tokens`。
- `/cc` 和 `/ha` 默认开启这个效果，所以它们可以展示“低 input + 高 cache read”的形态。

默认路径效果：

- `/v1/messages`：默认策略，input/output 用 raw，cache read/write 保留本地计算。
- `/cc/v1/messages`：input sample 到约 96 tokens 以内，差值转入 cache read；cache creation 默认围绕 3000 tokens、最大倍率 1.2 采样。
- `/ha/v1/messages`：input sample 到约 96 tokens 以内，差值转入 cache read；cache creation 保留计算值。
- `/na/v1/messages`：路径策略默认 disabled。对本地模拟 cache usage 不向下游和后台展示；如果真实上游 metadata 自己有非零 cache usage，仍按真实值处理。

## 外部备用池 usage 策略

外部备用池与本地 Kiro 凭据池不同：

- 请求固定发到外部池自己的 `/v1/messages`。
- 原始入口路径只用于决定 usage 投影策略，不用于拼接外部池请求路径。
- 外部池请求体和响应正文默认透传，不参与本地 prompt-cache tracker。

单个外部池字段：

```json
{
  "usageProjectionMode": "pass_through"
}
```

可选值：

- `pass_through`：严格透传外部池返回的 `usage`，不改写。
- `current_path_policy`：只改写响应里的 `usage` 字段；按当前入口路径命中的 `reportedUsage` 策略投影，再对 cache read / cache creation 做上浮。

全局字段：

```json
{
  "externalPools": {
    "externalPoolUsageProjectionUpliftPercent": 25
  }
}
```

`externalPoolUsageProjectionUpliftPercent` 默认 `25`，最大按代码限制为 `200`。它只在外部池 `usageProjectionMode=current_path_policy` 时参与 usage 字段整形。

当前上浮算法：

1. 对 `cache_read_input_tokens` 使用 `max(projected.cache_read, raw.cache_read)` 作为基础。
2. 对 `cache_creation_input_tokens` 使用 `max(projected.cache_creation, raw.cache_creation)` 作为基础。
3. 按 `ceil(base * (100 + percent) / 100)` 上浮。
4. `input_tokens` 和 `output_tokens` 不因为上浮而增加。
5. `total_input_tokens` 重算为 `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`。
6. creation 的 5m/1h breakdown 按 raw 或 projected 的原比例重分配；如果没有原比例，默认放到 5m。

成本记录：

- 外部池使用记录会保存 raw usage 和 reported usage。
- raw cost 和 reported cost 都按系统价格表估算。
- `billable_cost_usd = max(raw_cost_usd, reported_cost_usd)`。
- 如果投影后成本低于 raw 成本，记录 `cost_floor_applied=true` 和差额。
- 这只是后台记录和费用估算保底；如果 `usageProjectionMode=current_path_policy` 已经改写了响应体，下游实际看到的是 reported usage，不会额外看到成本保底字段。

## 推荐配置示例

### 保持默认高缓存外观

适合希望 `/v1`、`/cc`、`/ha` 都有高缓存特征，且 `/cc` 更像 Claude Code 低 input 高 cache read 的场景。

```json
{
  "promptCacheTargetReadRatio": 0.98,
  "promptCacheTokenScale": 1.6,
  "promptCacheMaxSimulatedInputTokens": 300000,
  "promptCacheCapJitterMinTokens": 12000,
  "promptCacheCapJitterMaxTokens": 24000,
  "promptCacheScaleMinInputTokens": 20000,
  "promptCacheCreationControl": {
    "enabled": false
  }
}
```

### 降低“每轮都创建缓存”的观感

适合用户反馈连续对话频繁出现 cache creation，希望 creation 更像偶发写入。

```json
{
  "promptCacheCreationControl": {
    "enabled": true,
    "scopeMode": "conversation_model",
    "minSuccessfulRequestsBetweenCreation": 3,
    "minCreationIntervalSecs": 60,
    "minCreationDeltaTokens": 12000,
    "maxCreationTokensPerEvent": 30000,
    "creationBudgetWindowSecs": 300,
    "maxCreationTokensPerWindow": 120000,
    "expireAfterIdleSecs": 3600
  }
}
```

说明：

- 如果调度经常换凭据，`conversation_model` 能减少跨凭据重复 creation 上报。
- 如果想更贴近真实账号隔离，使用 `credential_conversation_model`。
- 开启后首个 creation 仍可出现；后续 creation 会受成功次数、时间、增量和窗口额度限制。

### 禁止本地模拟 cache 展示给某路径

例如只想 `/na` 不展示本地模拟 cache：

```json
{
  "reportedUsage": {
    "pathOverrides": {
      "/na": {
        "enabled": false
      }
    }
  }
}
```

注意：关闭路径策略不是关闭路由 high-cache profile 构建，而是关闭本地模拟 cache usage 的下游展示/后台记录投影。真实上游 metadata 仍会按真实值处理。

### 外部池严格透传

```json
{
  "usageProjectionMode": "pass_through"
}
```

适合希望外部池“请求和返回都不要改变任何东西”的场景。

### 外部池按当前路径展示本系统缓存特征

```json
{
  "usageProjectionMode": "current_path_policy"
}
```

同时保留全局默认上浮：

```json
{
  "externalPools": {
    "externalPoolUsageProjectionUpliftPercent": 25
  }
}
```

适合希望外部池返回给下游时也呈现当前系统路径级 cache 特征，同时尽量避免整形后长期低于渠道 raw usage 成本的场景。

## 常见现象解释

### 为什么第一轮是 creation，第二轮才 read

本地 tracker 在成功请求后才写入 fingerprint。第一次看到某个 scope + prefix 时没有旧 entry，只能 creation；成功结束后更新 tracker，后续相同或增长前缀才能 read。

### 为什么换账号后又 creation

默认 cache scope 包含 `credential_id`。调度换凭据后，本地缓存隔离，新的凭据 scope 下没有旧 fingerprint，所以可能 creation。

如果只是想减少“上报 creation 的频率”，不要改 tracker scope，可以开启：

```json
{
  "promptCacheCreationControl": {
    "enabled": true,
    "scopeMode": "conversation_model"
  }
}
```

它只控制最终上报频次，不改变实际本地 cache read 计算。

### 为什么有真实上游 metadata 时本地配置好像没生效

如果上游 metadata 已经返回非零 cache read/write，系统优先使用真实 metadata，不用本地模拟覆盖。`reportedUsage` 对本地模拟 usage 的路径投影不会强行改写真实非零 metadata。

### 为什么 `/na` 没有本地模拟 cache

`/na/v1/messages` 路由仍是 high-cache，但默认 `reportedUsage.pathOverrides["/na"].enabled=false`。这会让下游响应和后台记录隐藏本地模拟 cache usage；真实上游 metadata cache 不受影响。

### 为什么 usage 里 `total_input_tokens` 可能大于 `input_tokens`

当前语义是：

```text
total_input_tokens = input_tokens + cache_read_input_tokens + cache_creation_input_tokens
```

`input_tokens` 表示下游兼容口径里的未缓存输入；`total_input_tokens` 表示完整输入口径。

### 为什么外部池整形后成本仍有保底

外部池 `current_path_policy` 可能把 usage 改成更像本系统路径特征，理论上可能让 reported cost 低于外部池 raw usage 估算成本。系统会在 usage record 的外部池 billing 中记录 raw cost、reported cost，并用 `max(raw, reported)` 作为内部 billable cost。

## 已知限制和风险

1. 本地 prompt cache tracker 是进程内状态。服务重启会清空；多实例负载均衡下，不同实例之间不会共享 fingerprint。
2. creation 频次控制器也是进程内状态。重启或多实例切换会导致频次状态重置。
3. 默认 tracker scope 包含 credential id；本地凭据调度换号会降低 cache read 连续性，但这更接近真实账号隔离。
4. `promptCacheCreationControl` 只隐藏/限制 creation 上报，不会让没有 read 的请求凭空 read。
5. 外部池 `current_path_policy` 只改写 usage 字段，不改正文、tool、message id、stop reason 等内容。
6. 外部池成本保底依赖价格目录；没有价格时无法可靠比较 raw/reported 成本。
7. 当前 Opus 4.8 的本地模拟最小可缓存 token 门槛未在 `min_cacheable_tokens_for_model` 中按 Opus 4.6/4.7 一样特判。

## 排查建议

看单条 usage record 时重点字段：

- `usageSource`：确认是 `upstream_metadata` 还是 `local_prompt_cache`。
- `cacheReadInputTokens` / `cacheCreationInputTokens`：判断 read/write 体量。
- `routeKind` / `routeSubtype`：确认是本地凭据还是外部池 fallback/direct。
- `externalPoolBilling.rawUsage` / `reportedUsage`：外部池是否发生 usage 投影。
- `externalPoolBilling.costFloorApplied`：整形后是否低于 raw 成本并触发保底。

如果用户反馈“每轮都创建缓存”：

1. 确认这些记录是否同一个 `conversationId`。
2. 确认是否同一个 `credentialId` 和同一个上游模型。
3. 确认 `usageSource` 是否为 `local_prompt_cache`；如果是 `upstream_metadata`，说明创建字段来自真实上游。
4. 确认请求是否每轮 system/tools/首条 user 大幅变化，导致 fallback conversation id 或 fingerprint 变化。
5. 开启 `promptCacheCreationControl.enabled=true`，并按需求选择 `scopeMode`。

如果用户反馈“没有 cache read”：

1. 确认输入是否低于模型最小可缓存 token 门槛。
2. 确认是否首次请求，首次通常 creation。
3. 确认请求是否失败，失败不会 update tracker。
4. 确认是否重启、多实例切换或换凭据。
5. 确认路径是否 `/na` 且上报策略 disabled。
