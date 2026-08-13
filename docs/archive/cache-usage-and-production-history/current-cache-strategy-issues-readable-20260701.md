# 当前缓存策略实际问题分析（字段中文说明版）

日期：2026-07-01

范围：基于当前本地 `kiro.rs` 代码，只分析“当前已经在运行的缓存策略”里实际需要处理的问题。本文不强行把所有设计差异都说成 bug；当前策略已经在现网运行，能正常工作的部分要保留。

---

## 1. 结论

当前缓存策略真正需要优先处理的问题，主要是两类：

1. **第一次请求 / first miss 可能在标准 `usage` 字段里出现 `cache_read_input_tokens`。**
2. **当前本地缓存命中范围仍然绑定 `credential_id`（凭证/账号）和 `model`（模型），不符合后续希望的“按 session 会话缓存”。**

另有一类问题不是线上 bug，但会影响后续改造：

3. **缓存计算、标准 `usage` 上报、usage record、external pool 成本计算现在耦合较深。改造时必须拆职责，但不能破坏现网已有的 raw / shaped / reported 记录链路。**

还有一些点，比如 dynamic system 头是否污染缓存、最后一条 message 是否应该进入自动缓存，不应该现在直接判定为“当前策略 bug”。它们需要结合真实请求样本和现网日志确认。本文会单独放到“需要验证，不直接定性”的章节。

---

## 2. 字段翻译

| 字段 | 中文说明 |
| --- | --- |
| `usage` | Claude/Anthropic 标准响应字段。真正返回给 Claude Code CLI / 下游客户端的是这个字段。 |
| `reportedUsage` | 当前项目里的配置名，用来决定最终写进标准 `usage` 字段的 token 口径。它不是下游协议字段。 |
| `reported_usage` | 项目内部变量/记录名。表示最终写进标准 `usage` 字段、并作为 usage record 主字段记录的 usage 口径。 |
| `raw_usage` | 原始 usage。通常来自上游 metadata，或者没有 metadata 时用请求估算值补齐，用于后续分析对照。 |
| `shaped_usage` | external pool 里的整形后 usage。表示经过当前路径缓存策略、creation 频控等处理后的 usage。 |
| `usage_source` | usage 来源。用于区分这条记录来自上游 metadata、本地 prompt cache 模拟、上下文估算还是请求估算。 |
| `PromptCacheScope` | 缓存隔离范围。也就是“哪些请求可以共用同一批缓存”。 |
| `credential_id` | 凭证 ID，也可以理解为上游账号 ID。 |
| `conversation_id` | 会话 ID，也就是同一个 Claude Code 会话或同一段连续对话。 |
| `model` | 模型名，比如 `claude-sonnet-4-5`。 |
| `route_namespace` | 路径隔离名。某个路径单独配置缓存参数时，当前代码可能把它放进独立缓存空间。 |
| `cache_creation_input_tokens` | 本次新写入缓存的输入 token 数。第一次 miss 通常应该体现在这里。 |
| `cache_read_input_tokens` | 本次从缓存读取的输入 token 数。第一次 miss 不应该有这个数。 |
| `input_tokens` | 没有走缓存的普通输入 token 数。 |
| `targetReadRatio` | 目标缓存读取比例。当前高缓存策略用它把请求模拟成较高缓存命中率。 |
| `tokenScale` | token 放大倍数。当前高缓存策略用它把输入规模放大后再模拟。 |
| `creationControl` | cache creation 上报频控。控制不要每次都上报大量新建缓存。 |
| `bounds` | 缓存容量和时间上限配置。 |

---

## 3. 已确认问题一：第一次请求可能在标准 usage 里出现 cache read

### 3.1 底层 tracker 的 first miss 是对的

底层 `PromptCacheTracker`（本地缓存记录器）在没有缓存 entry 时，返回的是：

```text
cache_creation_input_tokens > 0
cache_read_input_tokens = 0
```

代码证据：

- `src/anthropic/prompt_cache.rs:269`

也就是说，底层缓存命中计算没有把第一次 miss 算成 read。

### 3.2 问题出在标准 usage 上报口径转换

当前 `reportedUsage`（内部配置名，用来决定标准 `usage` 字段口径）会把 `input_tokens` 压低，然后把差值加进 `cache_read_input_tokens`。

代码证据：

- `src/anthropic/cache.rs:136`
- `src/anthropic/cache.rs:531`

默认 `/cc` 路径启用了这个行为：

- `src/model/config.rs:435`
- `src/model/config.rs:611`

所以实际链路是：

```text
底层缓存计算：
    第一次请求 = creation > 0, read = 0

标准 usage 上报口径转换：
    input 被压低
    input 差值被加到 cache_read_input_tokens

下游看到的标准 usage 字段：
    可能出现 cache_read_input_tokens > 0
```

### 3.3 为什么这是实际问题

这不是“展示美化”问题。Claude Code CLI / 下游客户端解析的是标准 `usage` 字段。  
如果第一次请求在标准 `usage` 里出现 `cache_read_input_tokens`，下游就会认为本次确实读了缓存。

这和缓存语义冲突：

```text
第一次 miss 可以有 cache_creation_input_tokens
第一次 miss 不应该有 cache_read_input_tokens
```

### 3.4 修复原则

本地模拟缓存可以写进标准 `usage` 字段，但规则必须成立：

```text
只有原始计算结果里已经有 cache_read_input_tokens > 0，
才允许把压低 input 后的差值并入 cache_read_input_tokens。
```

如果本次只有 `cache_creation_input_tokens > 0`，没有真实 `cache_read_input_tokens`，就不能凭空制造 read。

---

## 4. 已确认问题二：当前缓存 scope 仍然绑定凭证和模型

当前 `PromptCacheScope`（缓存隔离范围）包含：

```rust
pub struct PromptCacheScope {
    pub credential_id: u64,
    pub conversation_id: String,
    pub model: String,
    pub route_namespace: Option<String>,
}
```

代码证据：

- `src/anthropic/prompt_cache.rs:24`

普通本地账号路径构造 scope 时，也确实放入了 `credential_id` 和 `model`：

- `src/anthropic/handlers.rs:3256`

external pool 路径同样会放入 pool 对应的 credential id 和 model：

- `src/external_pool.rs:4406`

### 4.1 为什么这是实际问题

按当前逻辑，同一个 Claude Code 会话，只要换了上游账号或模型，就会进入不同缓存空间：

```text
同一个 conversation_id
不同 credential_id
=> 不共享缓存

同一个 conversation_id
不同 model
=> 不共享缓存
```

这和后续目标冲突：当前缓存策略希望修成“按 session 会话缓存”，不再考虑凭证、模型。

### 4.2 只改 PromptCacheScope 还不够

当前缓存 fingerprint（缓存指纹，用 hash 判断前缀是否相同）里也包含 model。

代码证据：

- `src/anthropic/prompt_cache.rs:494`

也就是说，如果只从 `PromptCacheScope` 里去掉 `model`，但 fingerprint 里还保留 `req.model`，同一会话换模型仍可能不命中。

### 4.3 修复原则

如果目标是当前策略按 session 缓存，那么至少要同时处理：

```text
scope 层：不再用 credential_id / model 隔离
fingerprint 层：不要默认把 model 放进缓存指纹
```

是否保留 `route_namespace`（路径隔离名）要单独判断。它可能仍然有意义，因为现网已经按路径配置不同缓存参数。

---

## 5. 已确认问题三：缓存计算、标准 usage 上报、记录和成本链路耦合较深

这一点不是“现网 bug”，但它是后续改造必须面对的实际问题。

当前系统不是只算缓存，还要同时做：

```text
缓存计算
标准 usage 字段上报
usage record 记录
external pool 成本计算
```

这些职责现在互相依赖较深。改造缓存策略时，不能把这些链路打断。

### 5.1 下游真正接收的是标准 usage 字段

下游 Claude Code CLI 按标准 Claude/Anthropic 响应格式解析。

所以真正给下游的是：

```json
{
  "usage": {
    "input_tokens": 123,
    "cache_creation_input_tokens": 456,
    "cache_read_input_tokens": 0
  }
}
```

不是：

```json
{
  "reportedUsage": {}
}
```

`reportedUsage` 只是项目内部配置名。

### 5.2 record 里应该同时保留原始 usage 和最终上报口径

代码里已经能看到这个设计：

- `reported_usage_for_downstream(...)` 把本地计算结果转成最终写入标准 `usage` 字段的口径：`src/anthropic/handlers.rs:1788`
- `record_success_reported(...)` 记录成功请求时，同时传入 `reported_usage` 和 `raw_usage`：`src/anthropic/handlers.rs:2238`
- `UsageRecord` 里有 `raw_usage`，也有 `cache_read_input_tokens` / `cache_creation_input_tokens` 等主字段：`src/anthropic/handlers.rs:2513`

这个方向是合理的，不能在改造时丢掉。

正确理解应该是：

```text
raw_usage：
    上游真实 usage 或原始估算，用于排查和对账。

reported_usage：
    内部最终上报口径，最终写进标准 usage 字段，也写进 usage record 主字段。
```

### 5.3 external pool 也按路径策略做 usage projection

external pool（外部池）不是旁路逻辑，它也带着当前路径解析后的缓存参数：

```rust
pub reported_usage: ReportedUsageConfig,
pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
pub prompt_cache_route_namespace: Option<String>,
pub prompt_cache_target_read_ratio: f64,
pub prompt_cache_token_scale: f64,
pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
pub prompt_cache_bounds: PromptCacheBounds,
```

代码证据：

- `src/external_pool.rs:373`

external pool 会按当前路径策略构造模拟缓存 usage：

- `src/external_pool.rs:4338`
- `src/external_pool.rs:4348`
- `src/external_pool.rs:4355`

也会按当前路径的 `reported_usage` 配置生成内部上报策略：

- `src/external_pool.rs:4365`

### 5.4 external pool 会记录 raw / shaped / reported

external pool 对 usage 至少分三层：

```text
raw_usage：
    外部上游原始 usage。

shaped_usage：
    按当前路径缓存策略、creationControl 等处理后的 usage。

reported_usage：
    内部最终上报口径，最终写进标准 usage 字段，也作为 usage record 主字段。
```

非流式响应：

- `src/external_pool.rs:3760`
- `src/external_pool.rs:3765`
- `src/external_pool.rs:3766`

流式响应：

- `src/external_pool.rs:3820`
- `src/external_pool.rs:3826`
- `src/external_pool.rs:3969`

### 5.5 成本计算是独立职责，不能绑死缓存策略

external pool 会基于 raw / shaped / reported 分别估算成本：

- `src/external_pool.rs:4262`
- `src/external_pool.rs:4274`
- `src/external_pool.rs:4299`

这说明成本计算需要使用缓存策略产出的 usage 口径。

但职责上应该拆开：

```text
缓存策略职责：
    算 cache creation / cache read。
    产出最终可以写进标准 usage 字段的 usage。

记录职责：
    记录 raw_usage、shaped_usage、reported_usage。

成本职责：
    基于这些 usage 口径计算 raw_cost、reported_cost、billable_cost、profit。
```

所以后续缓存改造不能破坏成本计算输入，但也不能把 billing 规则写进缓存策略里。

---

## 6. 需要验证，暂不直接定性为当前 bug

下面这些点确实值得关注，但目前不应该强行说成“当前策略已经有 bug”。需要结合真实请求样本、日志或专门测试确认。

### 6.1 dynamic system 头是否污染缓存命中

当前 `flatten_cache_blocks` 会把 system 内容纳入缓存 hash：

- `src/anthropic/prompt_cache.rs:509`

Kiro-RS-Tool 会跳过首个 `cache_control` 前面的动态 system 头，避免每轮变化的系统头污染稳定前缀：

- `~/Desktop/procode/Kiro-RS-Tool/src/anthropic/cache_metering.rs:540`

这可能导致当前项目“该命中时不命中”。  
但是否已经影响现网，需要用真实 Claude Code 请求样本确认。

### 6.2 最后一条 message 是否应该自动进入缓存

当前 high-cache 会遍历所有 messages：

- `src/anthropic/prompt_cache.rs:515`

Kiro-RS-Tool 的自动前缀链默认不把最后一条 message 切成自动缓存段，除非它显式带 `cache_control`：

- `~/Desktop/procode/Kiro-RS-Tool/src/anthropic/cache_metering.rs:641`

这是策略差异，但不一定是当前策略 bug。  
如果当前高缓存策略本来就是更激进的模拟策略，那它可以是现有行为的一部分。后续如果要做 Kiro-RS-Tool 对齐，应放在新策略里处理，不要直接判定当前策略错误。

### 6.3 本地缓存 entry 存的是加工后的 token

当前 `update_with_bounds` 会按 `targetReadRatio` 把每个前缀 token 缩放后写入缓存 entry：

- `src/anthropic/prompt_cache.rs:372`
- `src/anthropic/prompt_cache.rs:386`

这会让底层缓存记录带有当前策略痕迹。  
它是后续抽离策略时需要处理的设计约束，但不一定是现网 bug。

---

## 7. 哪些不应该当成问题

### 7.1 `usage` 和 `reported_usage` 同时存在不是问题

这是必要设计。

系统既要给下游返回标准 `usage` 字段，也要在 usage record 里保留原始 usage，方便分析和成本计算。

问题不是“存在多个 usage 口径”，而是这些口径必须命名清楚、职责清楚。

### 7.2 external pool 做 usage projection 不是问题

external pool 按路径策略做 usage projection 是现网能力的一部分。  
问题不是它存在，而是缓存改造时必须保证它仍能：

```text
按路径策略计算缓存 usage
写回标准 usage 字段
记录 raw / shaped / reported
给 billing 提供输入
```

### 7.3 成本计算依赖 usage 不是缓存问题

成本计算需要 usage 数据，这是正常的。  
但成本计算和缓存策略应该是不同职责，缓存策略只产出清楚的 usage 口径，billing 模块再基于这些口径计算成本。

---

## 8. 当前文档是否需要更新

需要更新。

原因是之前那版文档把一些“设计差异”和“后续改造约束”也写得像当前 bug，容易误导实现者。

更新后的文档应该保持这个边界：

```text
已确认问题：
    first miss 可能在标准 usage 字段里出现 cache read
    当前 scope/fingerprint 仍绑定 credential/model
    缓存计算、usage 上报、记录、external pool 成本链路耦合较深，需要改造时拆职责

需要验证：
    dynamic system 头是否实际污染命中
    最后一条 message 自动缓存是否需要在当前策略修
    加工后 token 写入 entry 是否要在第一阶段重构

不当成问题：
    多 usage 口径同时存在
    external pool 做路径级 usage projection
    billing 使用 usage 结果计算成本
```

---

## 9. 最终建议

后续修复当前缓存策略时，优先处理：

1. 修正 first miss 的标准 `usage` 字段，creation-only 不能变成 cache read。
2. 当前策略改成 session 维度缓存，同时处理 scope 和 fingerprint 两层。
3. 梳理职责边界：缓存策略产出 usage，记录模块保存 raw/shaped/reported，billing 模块基于 usage 计算成本。

暂时不要把所有和 Kiro-RS-Tool 不一致的地方都当成当前 bug。  
当前策略已经在现网运行，改造目标应该是修明确问题、保留现有可用能力，再为后续新增策略留出干净边界。

---

## 10. 本次实施顺序

本次改造按下面顺序做，避免一边修 bug 一边换策略导致问题不好定位。

### 10.1 先修当前策略 bug

先修两个已经确认的问题：

1. `first miss` 不能在标准 `usage` 字段里出现 `cache_read_input_tokens`。
2. 当前缓存 scope 改成按会话缓存，不再按凭证和模型隔离。

修复后要保证：

```text
第一次请求：
    可以有 cache_creation_input_tokens
    不能凭空出现 cache_read_input_tokens

第二次或后续命中：
    只有本地缓存里真的已有可读前缀，才出现 cache_read_input_tokens
```

### 10.2 再抽缓存策略类型

给路径缓存配置增加一个策略类型字段：

```text
cacheType
```

兼容规则：

```text
旧配置没有 cacheType：
    默认按当前现网策略处理。

旧配置里已有 simulation / creationControl / reportedUsage / cachePoint / bounds：
    继续按原含义读取、回显、保存。
```

这样现网已经针对 `/cc`、`/ha`、`/na` 或其他路径设置过的缓存参数，不需要迁移，也不会因为新增字段丢失。

### 10.3 再增加 Kiro-RS-Tool 策略

`Kiro-RS-Tool` 策略作为新的策略类型加入。它和当前策略不是同一组参数：

```text
当前策略：
    主要使用 targetReadRatio / tokenScale / creationControl / reportedUsage 等参数。

Kiro-RS-Tool 策略：
    重点是按 cache_control / 自动历史前缀 / session 隔离 / 成功后提交缓存来模拟。
```

如果某个 Kiro-RS-Tool 行为本身不是自然参数，就不要为了“看起来可配置”强行做成配置项。  
可以参数化的边界，例如容量、TTL、是否使用路径 namespace，可以作为该策略自己的参数。

### 10.4 UI 和 admin-ui 必须支持回显

页面要做到：

```text
旧路径：
    没有 cacheType 时，页面显示为当前策略。
    旧参数照常显示。
    保存后旧参数不丢。

新路径：
    可以选择 Kiro-RS-Tool 策略。
    页面只展示该策略真正需要的参数。
```

不能出现“后端兼容，但页面保存一次把旧路径缓存参数清空”的情况。

---

## 11. 验收标准

本次改造不是只看缓存数值，还要证明系统运行不会出问题。

### 11.1 功能正确

至少要覆盖：

1. 当前策略第一次 miss 不出现 cache read。
2. 当前策略同一会话换凭证、换模型仍能复用缓存。
3. 旧路径配置没有 `cacheType` 时仍走当前策略。
4. 旧路径配置的 `simulation`、`creationControl`、`reportedUsage`、`cachePoint`、`bounds` 仍能解析和回显。
5. Kiro-RS-Tool 策略第一次 miss 不出现 cache read。
6. Kiro-RS-Tool 策略成功响应后才提交缓存，失败请求不能污染下一次缓存。
7. external pool 仍保留 raw / shaped / reported 三层 usage，成本计算输入不断。

### 11.2 资源安全

至少要证明：

1. 缓存 entry 有容量上限和 TTL 上限。
2. 多会话、多路径请求不会导致缓存无限增长。
3. 请求结束后文件描述符不会持续上涨。
4. 负载停止后 RSS 内存不会持续上涨。
5. 新增策略不会在每个请求里做大对象长期持有。

### 11.3 性能不退化

至少要证明：

1. 当前策略默认路径没有因为新增策略字段多走重逻辑。
2. Kiro-RS-Tool 策略只在被路径选中时运行。
3. 缓存 hash / canonical 计算仍然和请求大小线性相关，没有额外的全局扫描。
4. 路径策略选择仍使用现有最长前缀匹配，不引入新的慢查询。

### 11.4 必跑验证

本次完成后必须运行：

```text
cargo fmt --check
git diff --check
cargo test
cargo build --release
前端类型检查 / 构建
Claude Code CLI 本地服务真实验证
轻量负载和资源验证
```

本地 Claude Code CLI 验证必须使用临时端口和隔离 HOME / CLAUDE_CONFIG_DIR，不能动现网 `9022`。
