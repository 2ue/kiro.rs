# 请求处理管线模块化重构分析

日期：2026-07-06

## 背景

当前系统已经同时支持本地凭证、外部池、显式外部直连、Raw body 透传、模型映射、usage 整形、路径缓存策略、payload guard、图片处理、错误统一包装等能力。问题是这些能力目前主要集中在 `handlers.rs` 和 `external_pool.rs` 内部，很多配置看起来是独立的，但执行路径上存在隐式耦合。

这会带来几个直接风险：

- **配置语义不聚焦**：例如 Raw body 透传本应只控制“出站 body 是否进入处理链”，但它容易和模型映射、usage 整形、显式直连、payload guard 发生耦合。
- **调度能力被处理模式误伤**：例如普通外部池 fallback 如果强制筛选 `Normalized` body mode，就会把配置为 `raw_passthrough` 的外部池过滤掉，最终出现 `external_pool_unavailable: No available external fallback pools`。
- **修复容易变成补丁堆叠**：单点修复能解决当前问题，但会继续扩大 `external_pool.rs` 的职责，后续新增能力时更容易引入新的互相影响。
- **测试边界不清楚**：如果 body 处理、模型处理、usage 整形、调度混在同一条链里，case 很难精准覆盖“某个配置只影响某个模块”。

## 核心原则

模块化重构的目标不是简单拆文件，而是拆清楚职责和数据契约。

1. **Body 处理只处理 body**
   - 决定发给上游的请求体 bytes。
   - 不决定走哪个池。
   - 不决定 usage 怎么上报。
   - 不决定模型是否命中映射，但可以接受模型模块输出的可选 patch。

2. **Usage 计算只计算 usage**
   - 输入是请求事实、上游 usage、路径策略、外部池策略、价格表。
   - 输出是下游响应 usage、系统记录 usage、成本拆分。
   - 不关心 body 是 Raw 还是 Normalized，只关心是否有足够的 `RequestFacts`。

3. **模型处理只处理模型**
   - 输入是客户端原始模型、本系统解析后的模型、外部池模型映射规则。
   - 输出是 outbound model、解析来源、可选 body patch。
   - 不强制绑定 Raw body，也不强制绑定 usage 整形。

4. **调度只做调度**
   - 决定本地凭证、外部池、哪个外部池、重试、冷却、并发、队列。
   - 不解析 body。
   - 不做 usage 改写。
   - 不因为 body mode 误过滤池，除非调用方明确要求某类能力，例如显式 Raw 直连。

5. **显式直连不是 Raw body 模式的唯一入口**
   - 显式直连是一种路由策略。
   - Raw body 透传是外部池的 body 处理策略。
   - 外部池 fallback 也应该能调度 Raw body 外部池；不能因为未开启显式直连就忽略 Raw 外部池。

6. **配置应该是能力开关，而不是复合语义**
   - 一个配置项只表达一个层面的意图。
   - 组合行为由管线编排决定，不应该藏在某个配置项里。

## 当前需要修正的耦合点

### 1. 普通 fallback 不能强制 body mode 为 Normalized

当前错误根源是普通外部池 fallback 构造 `ExternalRouteRequest` 时如果强制设置：

```text
body_mode_filter = Normalized
```

那么调度层会过滤掉 `requestBodyMode=raw_passthrough` 的外部池。结果是：

- 外部池启用了。
- Raw 外部池也可用。
- 但显式直连没有开启。
- 本地凭证不可用后尝试 fallback。
- Raw 外部池被过滤。
- 最终报 `external_pool_unavailable: No available external fallback pools`。

正确语义应该是：

- **显式 Raw 直连入口**：只筛 Raw 外部池，因为这个入口要求 byte-level raw passthrough。
- **普通外部池 fallback**：不按 body mode 过滤，选到哪个池就按哪个池自己的 body 配置执行。

### 2. Raw body 透传不能自动禁用 usage 整形

Raw body 模式只表示 body 不进入处理链，不应该隐含 usage 透传。

正确语义：

- `requestBodyMode=raw_passthrough`：控制出站请求体处理。
- `usageProjectionMode=pass_through/current_path_policy`：控制 usage 上报。
- 两者互不覆盖。
- 如果 Raw 请求能旁路提取 `RequestFacts`，usage 就可以按路径整形。
- 如果 Raw 请求不是标准 Anthropic Messages JSON，无法提取 facts，则 usage 整形只能降级为透传并记录原因。

### 3. 模型处理不应该强制绑定 body 处理

Raw body 和模型写回不矛盾，也不应该强制绑定。

合理配置应为：

- `modelMappingMode`：模型映射策略。
- `rawModelMode`：Raw body 下是否只探测模型、是否写回顶层 model。
- `requestBodyMode`：body 处理模式。

示例：

- Raw body + 不写回 model：完全 body 透传，只用 raw 顶层 model 做调度/记录。
- Raw body + 写回顶层 model：只 patch 顶层 `model` 字段，其他 body 原样保留。
- Normalized body + 模型映射：标准链路序列化时写入 outbound model。

模型处理模块应该输出：

```text
ModelDecision {
  original_model,
  processed_model,
  outbound_model,
  source,
  note,
  optional_body_patch,
}
```

Body 模块自己决定是否应用 `optional_body_patch`。

## 建议模块边界

### 1. `RequestEnvelope`

只表示入口请求的原始事实：

```text
RequestEnvelope {
  request_id,
  endpoint,
  headers,
  raw_body,
  received_at,
}
```

这个结构不可变。后续模块不能直接改它。

### 2. `RequestFactsExtractor`

负责从 raw body 或 parsed payload 中提取 facts。

```text
RequestFacts {
  model,
  stream,
  max_tokens,
  conversation_id,
  input_tokens,
  has_images,
  has_tools,
  parse_status,
  parse_error,
}
```

特点：

- lazy parse，只有需要 facts 的模块才触发。
- Raw body 模式也可以提取 facts，但提取 facts 不等于 body 被处理。
- 解析失败不一定是请求失败，取决于当前路径是否需要标准协议语义。

### 3. `BodyPipeline`

负责生成出站请求体。

```text
BodyPipelineInput {
  envelope,
  parsed_payload,
  body_mode,
  image_processing_config,
  payload_guard_config,
  optional_model_patch,
  optional_schema_patch,
}

BodyPipelineOutput {
  outbound_body,
  body_mode_used,
  modified,
  diagnostics,
}
```

可实现的 processor：

- `RawBodyProcessor`
  - 默认直接返回原始 bytes。
  - 可选应用顶层 model patch。
  - 不进入图片处理、schema 修正、payload guard。

- `NormalizedAnthropicProcessor`
  - 解析 payload。
  - 图片处理。
  - schema 修正。
  - payload guard。
  - thinking/model 兼容处理。
  - 序列化为标准 Anthropic body。

后续如果需要插件化，可以定义：

```text
trait BodyProcessor {
  fn supports(mode, route, upstream) -> bool;
  fn process(input) -> Result<BodyPipelineOutput>;
}
```

### 4. `ModelPipeline`

负责模型解析、映射、校验。

```text
ModelPipelineInput {
  original_model,
  processed_model,
  mapping_mode,
  mapping_rules,
  require_match,
  fallback_transform,
}

ModelPipelineOutput {
  outbound_model,
  source,
  note,
  body_patch,
}
```

注意：

- `body_patch` 是可选结果，不代表模型模块直接修改 body。
- Raw 模式下可以选择 `probe_only` 或 `rewrite_top_level`。
- Normalized 模式下通常由标准序列化写入 `model`。

### 5. `UsageProjectionEngine`

负责所有 usage 变换和记录口径。

```text
UsageProjectionInput {
  request_facts,
  upstream_usage,
  route_policy,
  external_pool_policy,
  prompt_cache_state,
  pricing_catalog,
}

UsageProjectionOutput {
  raw_usage,
  shaped_usage,
  reported_usage,
  response_usage_patch,
  billing,
  diagnostics,
}
```

规则：

- `requestBodyMode` 不直接参与 usage 是否整形。
- `usageProjectionMode` 才决定外部池 usage 是否整形。
- 路径级“同步请求不整形”是上层拦截，外部池不能重新打开。
- 如果 facts 不足，输出应降级并记录 `projection_skipped_reason`。

### 6. `Scheduler`

负责调度。

```text
ScheduleInput {
  route,
  request_facts,
  direct_policy,
  local_pool_state,
  external_pool_states,
  required_capabilities,
}

ScheduleDecision {
  target_kind,
  selected_credential,
  selected_external_pool,
  fallback_reason,
  direct_policy_reason,
  attempts,
}
```

关键点：

- 普通 fallback 不应该把 `requestBodyMode` 当作硬筛选条件。
- 显式 Raw 直连可以声明 `required_capabilities = RawBodyPassthrough`。
- 如果某个上游不支持某能力，调度模块只负责排除并记录原因，不做 body 转换。

### 7. `ResponsePipeline`

负责处理上游响应。

```text
ResponsePipelineInput {
  upstream_response,
  response_mode,
  usage_projection_context,
  error_policy,
}
```

能力：

- 非流式 JSON usage patch。
- SSE event usage patch。
- SSE error mask。
- 统一错误响应。
- usage capture。

## 配置建议

### 外部池配置

建议配置按职责分组，避免一个字段承担多个语义。

```text
externalPool {
  routing {
    enabled
    priority
    concurrency
    autoDisablePolicy
  }

  requestBody {
    mode: normalized | raw_passthrough
    rawOptions {
      allowTopLevelModelRewrite
    }
    normalizedOptions {
      imageProcessingProfile
      payloadGuardProfile
      schemaCompatibilityProfile
    }
  }

  model {
    mappingMode
    requireMappingMatch
    normalizeVersionDots
    mappingRules
  }

  usage {
    projectionMode: pass_through | current_path_policy
    skipNonStreamProjection
    uplift
  }

  errors {
    publicErrorPolicy
    retryPolicy
    cooldownPolicy
  }
}
```

### 路径级 usage 配置

路径级 usage 设置应该是上层策略。

```text
routeUsagePolicy {
  reportedUsage
  skipNonStreamUsageProjection
}
```

如果路径级禁用同步请求整形，外部池不能重新打开。

### 显式直连配置

显式直连是路由策略，不是 body 策略。

```text
directPolicy {
  enabled
  modelRules
  pathRules
  requiredBodyMode: raw_passthrough | any
}
```

当前建议：

- 原始 raw 直连入口使用 `requiredBodyMode=raw_passthrough`。
- parsed direct fallback 可使用 `any`，让池自己的 body mode 决定如何出站。

## 迁移步骤建议

### 阶段 1：止血和边界确认

- 普通 fallback 不再按 `Normalized` 过滤外部池。
- 显式 raw 直连仍只匹配 Raw 外部池。
- Raw body 不再隐式关闭 usage 整形。
- UI 文案明确 body mode 和 usage mode 独立。

### 阶段 2：抽 `RequestFacts`

- 从 `handlers.rs` 和 `external_pool.rs` 中抽出事实提取。
- 所有 usage、model、scheduler 都依赖 `RequestFacts`，不直接各自解析 body。
- Raw 解析失败时只标记 facts 不完整，不直接报错。

### 阶段 3：抽 `UsageProjectionEngine`

- 把 `maybe_project_non_stream_usage`、SSE usage patch、外部池 billing、路径 reported usage 逻辑迁出 `external_pool.rs`。
- 本地凭证和外部池共用 usage 引擎。
- 增加 `projection_skipped_reason` 诊断字段。

### 阶段 4：抽 `BodyPipeline`

- Raw 和 Normalized 变成两个 processor。
- 图片处理、payload guard、schema 修正成为 Normalized processor 的子步骤。
- Raw processor 只允许显式配置的轻量 patch，例如顶层 model patch。

### 阶段 5：抽 `ModelPipeline`

- 模型映射、别名解析、版本点横转换、必须命中规则集中处理。
- 输出 `ModelDecision`，由 body pipeline 决定是否写回 body。

### 阶段 6：调度和响应收口

- Scheduler 只关心候选池和 required capabilities。
- ResponsePipeline 统一处理 usage patch、错误包装和记录。

## 必须覆盖的测试矩阵

### 调度

- 显式直连开启 + Raw 池：必须走 Raw 池。
- 显式直连开启 + 只有 Normalized 池：应走 parsed direct 或明确降级策略，不能误报 Raw 池不可用。
- 显式直连关闭 + 本地失败 + Raw 池：Raw 池不能被忽略。
- 显式直连关闭 + 本地失败 + Normalized 池：仍正常 fallback。
- Raw 池和 Normalized 池共存：按优先级/并发/冷却正常选择。

### Body

- Raw body + 不写 model：body byte-for-byte 保持。
- Raw body + 写顶层 model：只改顶层 model。
- Raw body + 嵌套 model：不能误改嵌套字段。
- Normalized body：图片处理、schema 修正、payload guard 正常生效。

### Usage

- Raw body + `pass_through`：下游 usage 保持上游。
- Raw body + `current_path_policy`：usage 按路径整形。
- Raw body + 路径禁用同步整形：非流式 usage 保持上游。
- Normalized body + `current_path_policy`：usage 按路径整形。
- SSE usage event：流式 usage patch 和 record 一致。

### 模型

- Raw + `probe_only`：调度/记录使用映射模型，但 body 不变。
- Raw + `rewrite_top_level`：只写顶层 model。
- Normalized + mapping：序列化 body 使用 outbound model。

### 错误

- 外部池无可用候选时，应能区分：
  - 真无池。
  - 池被禁用。
  - 池冷却。
  - 并发满。
  - body capability 被要求但无匹配池。
- 不应该把“Raw 池被普通 fallback 过滤”表现成“无外部池”。

## 当前结论

当前最重要的修复方向是把“路由策略”和“body 处理策略”拆开。

短期应保证：

- Raw 外部池不会因为显式直连关闭而被普通 fallback 忽略。
- 显式 Raw 直连仍只使用 Raw 池。
- usage 整形继续由外部池 usage 设置和路径 usage 设置决定，不被 body mode 隐式覆盖。

长期应落到 `RequestFacts + BodyPipeline + ModelPipeline + UsageProjectionEngine + Scheduler + ResponsePipeline` 六个模块。这样配置会更聚焦，也更容易证明“一个配置只影响它所属的模块”。
