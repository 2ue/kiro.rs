# OpenAI 官方协议与 Kiro 协议互转改造方案

Role: OpenAI Chat Completions/Responses ↔ Kiro 协议互转的架构方案、实施阶段、验证矩阵和发布门禁

Status: `Planning / implementation not started`

Last reviewed: 2026-10-14 Asia/Shanghai

Owner: 当前 Rust 项目 `kiro.rs` 的协议兼容、模型调度和外部池边界

Related:

- [Rust Runtime Scheduler Stabilization](../README.md)
- [当前协议和 API baseline](../../../baseline/protocol-and-api-contracts.md)
- [当前运行时流程](../../../baseline/runtime-flows.md)
- [官方 Claude Code / Anthropic 与 Kiro 协议互操作审计](official-claude-code-kiro-protocol-audit-20260928.md)
- [sub2api-kiro 协议互转优化执行计划](sub2api-kiro-protocol-interop-optimization.md)
- [验证与发版门禁](validation-and-release-gates.md)
- [OpenAI 官方 OpenAPI 规范](https://github.com/openai/openai-openapi/blob/master/openapi.yaml)
- [OpenAI Chat Completions API reference](https://platform.openai.com/docs/api-reference/chat/create)
- [OpenAI Responses API reference](https://platform.openai.com/docs/api-reference/responses/create)
- [OpenAI Models API reference](https://platform.openai.com/docs/api-reference/models/list)

本文只记录方案和实施规划，不代表代码已经支持 OpenAI 协议，也不授权在本计划确认前修改生产行为。当前首要目标是把“OpenAI 请求能否进入、如何转换到 Kiro、Kiro 结果如何转换回 OpenAI、GPT 模型如何安全调度”拆成可验证的边界，而不是把现有 Anthropic 路由通过若干字段兼容技巧包装成未经证明的 OpenAI 兼容。

## 1. 执行结论

### 1.1 当前结论

截至 2026-10-14、当前 `main`/`v0.0.191`：

| 能力 | 当前状态 | 证据 |
| --- | --- | --- |
| Anthropic Messages → Kiro | 已有生产主路径 | `src/anthropic/types.rs`、`src/anthropic/converter.rs`、`src/kiro/model/requests/*` |
| Claude Code `/cc/v1/messages` → Kiro | 已有并有真实 CLI 验证 | `src/anthropic/router.rs`、`src/anthropic/stream.rs`、既有 CLI evidence |
| `POST /v1/chat/completions` | 不支持 | 本地 `127.0.0.1:9022` 返回 `404 Not Found` |
| `POST /v1/responses` | 不支持 | 本地 `127.0.0.1:9022` 返回 `404 Not Found` |
| `GET /v1/models` | 有接口，但主要是 Anthropic/Kiro 兼容形态 | 本地返回 Claude、Deepseek、GLM、MiniMax、Qwen 等列表 |
| `GET /v1/models/gpt-4o` | 当前本地目录不支持 | 本地返回 `404 model: gpt-4o` |
| OpenAI-style usage 归一化 | 有限支持 | 仅主要用于 external pool 上游 usage/billing projection |
| `gpt-*` 模型名解析 | 可透传，但不是 GPT 协议支持 | `resolve_model_with_catalog` 对未知族 pass-through；传统 `map_model` 对 GPT 返回 unsupported |
| GPT 本地 Kiro 调度 | 不能宣称支持 | 必须以真实 Kiro `ListAvailableModels`、账号 supported-model catalog 和 converter capability 同时成立为准 |
| OpenAI 官方协议 ↔ Kiro 完整互转 | 尚未实现 | 缺少 OpenAI 入站 DTO、统一中间表示、OpenAI response/SSE renderer 和 protocol-aware route selection |

### 1.2 本计划的目标

最终让一个标准 OpenAI 客户端能够在明确启用的情况下：

1. 使用官方 `POST /v1/chat/completions` 发起请求；
2. 使用官方 `POST /v1/responses` 发起请求；
3. 通过本项目的统一内部请求表示进入本地 Kiro 账号、Anthropic external pool 或 OpenAI-compatible external pool；
4. 从 Kiro/外部上游返回 OpenAI 官方兼容的非流式 JSON 或 SSE；
5. 获取与实际可调度能力一致的 `/v1/models`；
6. 在模型、工具、thinking/reasoning、usage、错误、上下文和重试边界上保持可解释、可回滚、可测试。

### 1.3 本计划不做的事情

- 不把 `/v1/messages` 的 Anthropic body 直接解释为 OpenAI body。
- 不通过读取 `prompt_tokens/completion_tokens` 就宣称完整 OpenAI 协议兼容。
- 不把 `gpt-*` 静态写进公开模型列表；模型列表必须以真实可执行上游能力为准。
- 不把所有 GPT 模型自动映射到某个 Claude 模型。
- 不在无真实 capability evidence 时凭模型名猜测 GPT 的 context window、thinking 或工具能力。
- 不把 OpenAI raw body 发送到只接受 Anthropic Messages 的 external pool。
- 不把 Anthropic SSE 先渲染成客户端 SSE，再反向解析成 OpenAI SSE；应优先建立协议无关的事件层。
- 不改变现有 Anthropic/Claude Code 路由的默认语义，不以 OpenAI 兼容改造为理由放宽既有 fail-closed 边界。
- 不在本计划内实现 OpenAI Audio、Images、Embeddings、Moderations、Assistants、Realtime 等非文本生成 API。
- 不在没有独立容量、错误和成本证据前把 OpenAI upstream 与 Anthropic upstream 混为一个池。

## 2. 官方协议范围

本计划以 OpenAI 官方 OpenAPI 规范为协议基准，而不是以某个第三方 OpenAI-compatible 服务的实现为基准。

### 2.1 首批官方接口

| API | 首批规划 | 说明 |
| --- | --- | --- |
| `GET /v1/models` | 保持现有接口并补齐语义 | OpenAI 与 Anthropic 都使用该路径；返回体需要同时保持现有客户端兼容和真实可路由能力 |
| `GET /v1/models/{model}` | 补齐/校准 | 未知模型必须返回 OpenAI 可识别的 model-not-found 语义 |
| `POST /v1/chat/completions` | P0/P1 | 优先实现，生态最成熟，便于用原始 HTTP 和官方 SDK 验证 |
| `POST /v1/responses` | P2 | 第二阶段实现；输入项、response id、事件类型和 reasoning 语义比 Chat Completions 更复杂 |

### 2.2 明确排除的官方接口

以下接口不属于本次首批互转范围，后续必须另立计划：

- `/audio/*`
- `/images/*`
- `/embeddings`
- `/moderations`
- `/assistants/*`
- `/threads/*`
- `/vector_stores/*`
- `/realtime/*`
- `/batches`
- `/fine_tuning/*`

### 2.3 兼容等级

实施和发布不能使用一个笼统的“OpenAI compatible”标签，应按等级报告：

| 等级 | 含义 | 允许对外宣称 |
| --- | --- | --- |
| L0 | 路由不存在或固定返回 404 | 不支持 |
| L1 | Chat Completions 非流式纯文本、模型、基础 usage | OpenAI Chat 基础兼容 |
| L2 | Chat Completions 流式、终止事件、usage、错误 | OpenAI Chat 流式兼容 |
| L3 | Chat tools、tool call、tool result、图像输入、reasoning 参数 | OpenAI Chat 扩展兼容 |
| L4 | Responses 非流式 input/items/output/usage | OpenAI Responses 基础兼容 |
| L5 | Responses 流式事件、函数调用事件、终止/error 事件 | OpenAI Responses 流式兼容 |
| L6 | 真实模型目录、协议感知调度、external pool、成本和生产观察闭环 | OpenAI/Kiro 生产兼容 |

在 L6 之前，版本说明必须写清支持等级和未支持字段，不得只写“支持 OpenAI 格式”。

## 3. 当前实现事实与问题边界

### 3.1 下游入口事实

当前公开路由由 `src/anthropic/router.rs` 注册：

```text
GET  /v1/models
GET  /v1/models/{model_id}
POST /v1/messages
POST /v1/messages/count_tokens
```

以及 `/na/v1`、`/ha/v1`、`/cc/v1` 的对应 Anthropic/Claude Code 路径。没有：

```text
POST /v1/chat/completions
POST /v1/responses
```

本地 HTTP 探针结果：

```text
POST /v1/chat/completions -> 404 Not Found
POST /v1/responses        -> 404 Not Found
GET  /v1/models           -> 200
GET  /v1/models/gpt-4o    -> 404 model: gpt-4o
```

探针只访问当前本地服务的路由层，没有发送真实模型生成请求，也没有重启、压测或修改 `127.0.0.1:9022`。

### 3.2 当前请求转换事实

当前 typed request 是 `MessagesRequest`：

```text
model
max_tokens
messages
system
tools
tool_choice
thinking
output_config
metadata
stop_sequences
```

当前转换方向是：

```text
Anthropic Messages
  -> MessagesRequest
  -> model/content/history/tools/thinking/schema conversion
  -> Kiro ConversationState
  -> Kiro IDE/CLI request envelope
```

这条链路不能直接接收 OpenAI 的：

```text
Chat Completions: messages + max_completion_tokens + tool_calls + response_format
Responses: input/input_items + instructions + max_output_tokens + reasoning + response events
```

### 3.3 当前模型事实

当前代码存在两种不同语义，必须在改造中保留区分：

1. `src/anthropic/converter/model.rs::map_model`
   - 主要将 Claude/Claude Code 名称映射到 Kiro 模型 ID；
   - `map_model("gpt-4")` 明确返回 `None`；
   - 不能作为 OpenAI GPT 入口的模型解析器。

2. `src/anthropic/model_capabilities.rs`
   - 对真实上游目录中的未知族模型允许 pass-through；
   - `gpt-4o` 可以作为“未知模型名”保留给上游；
   - pass-through 不等于模型存在、不等于模型可调度、不等于协议可转换。

因此，OpenAI/GPT 改造必须新增“协议能力 + 上游能力 + 模型能力”的联合判断，而不能复用 Claude family fallback。

### 3.4 当前 external pool 事实

当前 external pool 已支持：

- `Normalized`；
- `RawPassthrough`；
- Anthropic header profile；
- Bearer/X-API-Key 认证；
- OpenAI-style `prompt_tokens/completion_tokens` usage 归一化；
- OpenAI-style `choices[]` 输出估算和部分 stream 判定。

但现有 raw path 的前提是：

```text
当前入口仍然是 Anthropic /v1/messages
  -> RawMessagesBodyProbe 探测 model/stream
  -> external pool raw_passthrough
```

它不是 OpenAI 入站适配。现有 `forward_headers` 还会按当前 external pool profile 注入 `anthropic-version`，不能直接复用到 OpenAI upstream。改造必须显式增加 upstream protocol/header/path 维度。

### 3.5 当前测试事实

以下 focused tests 已通过，但它们只证明有限的 OpenAI-style 上游兼容：

```text
openai_usage_is_normalized_for_non_stream_external_pool_body
openai_usage_is_captured_for_stream_external_pool_billing
openai_stream_usage_keeps_local_shaping_separate_from_raw_billing
extracts_model_ids_from_anthropic_and_openai_models_response
resolver_passes_unknown_family_through_without_mapping_match
test_map_model_unsupported
supported_model_filter_empty_list_allows_future_model_without_fallback
supported_model_filter_keeps_custom_names_exact_unless_resolution_is_explicit
supported_model_filter_does_not_family_match_custom_names
```

这些测试不得被当作 OpenAI downstream compatibility 的验收证据。

## 4. 目标架构

### 4.1 总体数据流

目标不是在 Anthropic handler 中追加一堆 `if openai`，而是把下游协议、内部请求、上游协议和响应渲染拆开：

```text
OpenAI Chat / OpenAI Responses / Anthropic Messages
                     |
                     v
          protocol-specific ingress DTO
                     |
                     v
       protocol-neutral request intermediate form
                     |
                     v
     route intent + model/capability resolution
                     |
        +------------+-------------+
        |            |             |
        v            v             v
   Kiro IDE      Kiro CLI    External pool
                                  |
                    +-------------+-------------+
                    |                           |
                    v                           v
              Anthropic upstream          OpenAI upstream
                    |                           |
                    +-------------+-------------+
                                  v
                 protocol-neutral response events
                                  |
             +--------------------+--------------------+
             |                    |                    |
             v                    v                    v
       Anthropic SSE       OpenAI Chat SSE       OpenAI Responses SSE
       / JSON response     / JSON response       / JSON response
```

### 4.2 目标模块边界

建议新增模块，但不在本阶段实现：

```text
src/protocol/
  mod.rs
  common.rs              # 协议无关 request/response/event/usage 基础类型
  capability.rs          # 下游/上游协议能力矩阵
  error.rs               # 协议无关错误分类

src/openai/
  mod.rs
  router.rs              # /v1/chat/completions、/v1/responses
  types_chat.rs          # Chat Completions DTO
  types_responses.rs     # Responses DTO
  request_adapter.rs     # OpenAI -> internal request
  response_renderer.rs   # internal events -> JSON
  stream_renderer.rs     # internal events -> OpenAI SSE
  errors.rs              # OpenAI error envelope

src/anthropic/
  # 保留现有入口；逐步复用 protocol-neutral request/event

src/upstream/
  mod.rs
  kiro_adapter.rs
  anthropic_adapter.rs
  openai_adapter.rs
```

实际文件名可以按现有模块风格调整，但必须保持这些所有权边界：

| 边界 | 所有者 | 禁止事项 |
| --- | --- | --- |
| OpenAI 入站 schema | `src/openai/*` | 不在 Kiro provider 中直接解析 OpenAI JSON |
| Anthropic 入站 schema | `src/anthropic/*` | 不为 OpenAI 兼容改写现有 `MessagesRequest` 语义 |
| 统一内部请求 | `src/protocol/*` | 不退化为无约束 `serde_json::Value` 大包 |
| Kiro body | `src/upstream/kiro_adapter.rs` 或现有 converter | 不让 OpenAI 字段绕过 payload guard |
| 上游 response events | `src/protocol/*` + provider parser | 不把一种客户端 SSE 当作内部标准 |
| 客户端响应 | 各协议 renderer | 不用 Anthropic envelope 返回 OpenAI 路由错误 |
| 调度资格 | Route planner/token manager/external pool | 协议兼容性必须先于优先级和负载选择 |

### 4.3 协议无关内部表示

建议建立 typed intermediate representation（名称待定），至少包含：

```text
InferenceRequest
  request_id
  downstream_protocol
  requested_model
  resolved_model
  stream
  instructions: Vec<InstructionBlock>
  messages: Vec<ConversationTurn>
  tools: Vec<ToolDefinition>
  tool_choice: ToolChoice
  reasoning: ReasoningRequest
  output_constraints: OutputConstraints
  media: Vec<MediaReference>
  stop_sequences
  metadata
  session: SessionIntent
  source_facts

ConversationTurn
  role: user | assistant | system | developer | tool
  content: Vec<ContentPart>
  tool_calls
  tool_results

ResponseEvent
  ResponseStarted
  TextDelta
  ReasoningDelta       # 仅内部，是否公开由 renderer policy 决定
  ToolCallStarted
  ToolCallArgumentsDelta
  ToolCallCompleted
  UsageUpdated
  KeepAlive
  ResponseCompleted
  ResponseError
```

内部表示必须保留足够信息生成三种下游协议，但不能把所有客户端原始字段无限透传。未知字段按协议 adapter 的白名单/扩展字段策略处理，默认不静默丢弃。

### 4.4 下游协议与上游协议的矩阵

| 下游请求 | 本地 Kiro | Anthropic external pool | OpenAI external pool |
| --- | --- | --- | --- |
| Anthropic Messages | 当前已有 | 当前已有 | 后续可通过 internal IR |
| OpenAI Chat | P1 支持 | P1/P2 通过 IR | P1/P2 normalized 或 raw 同协议 |
| OpenAI Responses | P2 支持 | P2/P3 通过 IR | P2/P3 normalized 或 raw 同协议 |

raw passthrough 只允许以下情况：

```text
downstream protocol == upstream protocol
且 pool 明确声明支持该 protocol
且没有需要本地改写的模型/工具/媒体/usage policy
```

跨协议路径必须使用 normalized/internal IR，不得把 OpenAI raw body 直接交给 Anthropic upstream，也不得把 Anthropic body 直接交给 OpenAI upstream。

## 5. 请求转换方案

### 5.1 OpenAI Chat Completions 请求

#### 5.1.1 基础字段

| OpenAI 字段 | 内部字段 | Kiro/Anthropic 映射 | 首批策略 |
| --- | --- | --- | --- |
| `model` | `requested_model` | 由 capability resolver 决定 upstream model | 必填；不做 GPT→Claude 猜测 |
| `messages` | `messages` | system/history/current message、tool pair | 必填；严格校验角色和 pair |
| `stream` | `stream` | Kiro stream 或一次性收集 | 支持 true/false |
| `max_completion_tokens` | `max_output_tokens` | Anthropic `max_tokens` / Kiro output budget | 优先支持 |
| `max_tokens` | `max_output_tokens` | 同上 | 兼容旧客户端；两者冲突时 400 |
| `stop` | `stop_sequences` | 本地输出截断 | 支持 string/string[] |
| `temperature` | `sampling.temperature` | 仅在上游 capability 明确支持时映射 | 默认不静默丢失 |
| `top_p` | `sampling.top_p` | 同上 | 默认不静默丢失 |
| `metadata` | `metadata` | request facts/session metadata | 保留受限键空间 |
| `user` | `client_user` | 不发送到 Kiro，进入脱敏诊断/安全标识 | 明确记录所有权 |
| `n` | `candidate_count` | Kiro 单次生成不能自然复刻多候选 | 首批只支持 `n=1`，其他返回 `unsupported_parameter` |
| `seed` | `determinism.seed` | Kiro 未证明支持 | 默认返回 `unsupported_parameter` |
| `logprobs` | `logprobs` | Kiro 未证明支持 | 默认返回 `unsupported_parameter` |
| `response_format` | `output_constraints` | 只在 capability profile 支持时映射 | 先支持有限子集 |

#### 5.1.2 role 和 content

支持以下角色：

```text
system
developer
user
assistant
tool
```

建议转换规则：

1. `system` 和 `developer` 保留顺序，进入内部 `instructions`；
2. 首批 Kiro adapter 将 `developer` 作为高优先级 instruction，并在 `system` 之前保留明确边界；
3. `user`/`assistant` 进入历史；
4. `tool` 必须匹配先前 assistant `tool_calls[].id`；
5. assistant `tool_calls` 转为内部 `ToolCall`，不能只转成可见文本；
6. 无法配对的 tool result 直接返回 OpenAI `invalid_request_error`，不猜造工具名称；
7. string content 直接变成 text part；
8. array content 逐 part 转换，未知 part 不能静默 textify。

OpenAI 的 `developer` 与 Anthropic `system` 不是完全等价语义。实现必须在 request facts 中记录 `developer_to_instruction`，并在兼容 profile 中可观测；不能把两者简单拼接后假装语义完全相同。

#### 5.1.3 Chat tools

支持的标准形态：

```json
{
  "type": "function",
  "function": {
    "name": "read_file",
    "description": "Read a file",
    "parameters": {
      "type": "object",
      "properties": {}
    }
  }
}
```

转换要求：

- 建立 request-scoped 原名 ↔ Kiro-safe 名称映射；
- 同一个映射必须同时用于 tools、tool_choice、assistant tool call、tool result；
- JSON Schema normalization 只能改写 Kiro 明确不接受的结构；
- `required`、`properties`、`dependentRequired` 等引用必须同步改写；
- OpenAI `tool_choice=none|auto|required|{type:"function",...}` 映射到内部 tool-choice；
- `parallel_tool_calls` 只有在 Kiro capability 明确支持多个并行 tool use 时才允许；
- Kiro 只返回单个 tool use 时，不伪造并行调用；
- 工具名称、参数 JSON 或 pair 失败必须进入 fail-closed 路径。

首批不支持：

- legacy `functions`/`function_call` 的所有复杂旧行为；
- 未经 Kiro capability 证明的 custom tool；
- 将任意文本中出现的 JSON 强行恢复为 tool call。

#### 5.1.4 图像和文件

首批支持范围建议：

- `type=text`；
- `type=image_url`，仅 data URL 或通过现有安全 remote fetch policy；
- OpenAI file input 仅在能映射到现有 Anthropic Files/materialization contract 时支持；
- audio、视频、未知 input part 明确返回 `unsupported_content`。

远程 URL 必须继续经过现有 SSRF、大小、超时、媒体类型和聚合预算限制。OpenAI adapter 不得绕过 `body_processing`、`files` 或 payload guard。

#### 5.1.5 reasoning/thinking

Chat Completions 的 `reasoning_effort` 只能转换成内部 `ReasoningRequest`：

```text
none | low | medium | high | xhigh | max
```

实际下发前必须与 resolved model 的 authoritative reasoning capability 相交：

1. 上游 capability 明确支持该 effort：下发 native reasoning/output_config；
2. 只支持较近 effort：是否向下映射由兼容 profile 决定，并记录 `requested_effort` 与 `effective_effort`；
3. 上游无 capability 或拒绝：返回 OpenAI `unsupported_parameter` 或执行显式的 bounded fallback；
4. 不得把 Kiro signed/redacted reasoning 当成普通 assistant text；
5. 默认不把隐藏 thinking 文本放入 Chat Completion `message.content`；
6. `reasoning_tokens` 只有在上游 usage 明确提供或可信推导时才填写，禁止伪造。

### 5.2 OpenAI Responses 请求

Responses 不能只把 `input` 字符串复制到 Chat `messages`，需要保留 item 语义。

#### 5.2.1 首批支持字段

| 字段 | 规划 |
| --- | --- |
| `model` | 必填，走统一模型解析 |
| `input` string | 转为一轮 user text |
| `input` item array | 支持 message、input_text、input_image、function_call_output 的受限子集 |
| `instructions` | 转为内部 instruction |
| `stream` | 进入 Responses SSE renderer |
| `max_output_tokens` | 转为内部 output budget |
| `tools` | 转为内部工具定义 |
| `tool_choice` | 转为内部选择策略 |
| `parallel_tool_calls` | 依 capability 支持 |
| `reasoning` | 只支持可映射的 effort/summary 子集 |
| `metadata` | 进入 request facts |

#### 5.2.2 默认不支持或延后字段

以下字段在没有 response/session store 或 Kiro 语义证据前，不能静默忽略：

- `previous_response_id`
- `conversation`
- `truncation`
- `include` 中的特定内部字段
- `background`
- `store`
- `service_tier`
- `prompt_cache_key`
- `safety_identifier`
- 任意只对 OpenAI Responses backend 有意义的托管状态字段

首批遇到这些字段时，优先返回规范的 `unsupported_parameter`；如果后续实现了 response state store，再单独增加版本化能力。

#### 5.2.3 Responses input item 到内部消息

建议建立显式转换表：

| Responses item | 内部表示 |
| --- | --- |
| `message` | `ConversationTurn` |
| `input_text` | `ContentPart::Text` |
| `input_image` | `ContentPart::Image` |
| `function_call_output` | `ToolResult` |
| `reasoning` | 内部 reasoning history；默认不直接公开 |
| unknown item | 明确 unsupported，不转成普通文本 |

Responses 的 output item 也必须保持类型，不要把 tool call、reasoning、message 全部压扁为一段字符串。

## 6. Kiro 上游适配方案

### 6.1 复用与重构边界

现有 Anthropic → Kiro converter 仍是重要行为参考，但 OpenAI adapter 不应直接构造 `MessagesRequest` 再走一遍反向伪装。建议分两步：

1. 把现有 Anthropic converter 的“语义转换部分”提取为 protocol-neutral builder；
2. Anthropic/OpenAI 两个 ingress adapter 都构造同一内部表示，再由 Kiro adapter 生成 `ConversationState`。

可复用的能力：

- content/image/document materialization；
- tool name mapping；
- schema normalization；
- tool pair validation；
- thinking capability resolution；
- payload guard；
- upstream retry/lease/capacity；
- Kiro parser 和内部 event extraction。

不得复用为 OpenAI response 的能力：

- Anthropic SSE event 作为最终客户端协议；
- Anthropic error envelope；
- `message_delta.usage` 直接返回给 OpenAI；
- Anthropic `stop_reason` 原样返回；
- `thinking` block 原样塞进 Chat `message.content`。

### 6.2 Kiro IDE/CLI body

无论下游是 Anthropic 还是 OpenAI，发往本地 Kiro 的 body 都必须经过同一套：

```text
model resolution
  -> history/tool/reasoning conversion
  -> Kiro ConversationState
  -> payload guard
  -> tool pairing invariant
  -> endpoint-specific Kiro envelope
  -> bounded retry
```

OpenAI-specific request facts 需要附加到 request-scoped diagnostics，但不能改变 Kiro body 的字段所有权。

### 6.3 中间事件层

推荐把 Kiro typed event 先转为内部事件：

```text
Kiro EventStream
  -> frame/header/CRC/EOF validation
  -> typed Kiro event
  -> ProtocolResponseEvent
  -> Anthropic renderer / OpenAI Chat renderer / OpenAI Responses renderer
```

内部事件层必须至少能表达：

- response/message started；
- text delta；
- reasoning delta；
- tool call start；
- tool call arguments delta；
- tool call completed；
- usage；
- keepalive；
- completed；
- error。

如果某个 Kiro event 无法安全归入上述事件，不得默认为 text。应进入 bounded diagnostic 或协议错误。

## 7. OpenAI 响应转换方案

### 7.1 Chat Completions 非流式

目标形态：

```json
{
  "id": "chatcmpl-...",
  "object": "chat.completion",
  "created": 0,
  "model": "requested-model",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "..."
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 0,
    "completion_tokens": 0,
    "total_tokens": 0
  }
}
```

规则：

- `id` 使用本地 response id，不复用 Kiro 内部 session id；
- `model` 默认返回客户端请求模型，resolved/upstream model 只进入诊断；
- text 输出写入 `message.content`；
- tool call 输出写入 `message.tool_calls`，不能同时伪造可见文本；
- Anthropic `end_turn` → `finish_reason=stop`；
- `max_tokens`/上下文终止 → `finish_reason=length`；
- tool use → `finish_reason=tool_calls`；
- content filter/safety 只有上游有可靠语义时才映射；
- 无法确定终止原因时使用安全的 `stop` 或公开错误，不能输出错误的 `length`；
- usage 使用统一 usage domain renderer，不能直接序列化 Anthropic usage。

### 7.2 Chat Completions 流式

目标最小生命周期：

```text
HTTP 200 text/event-stream
  -> chat.completion.chunk (role=assistant)
  -> chat.completion.chunk (content/tool delta)*
  -> chat.completion.chunk (finish_reason)
  -> optional final usage chunk when stream_options.include_usage=true
  -> data: [DONE]
```

要求：

- 首个可见 delta 之前发现上游错误，仍允许按既有 attempt budget 进行 pre-output retry；
- 首个可见 delta 之后不得伪造新的成功 response；
- stream read error、协议污染、非法 tool JSON 必须发 OpenAI stream error 或关闭连接并记录 error；
- 不得把 Anthropic `ping` 事件作为 OpenAI 内容 chunk；
- tool call arguments 必须保持源顺序、index 和 call id；
- `[DONE]` 只能在完整 terminal event 后发送；
- `stream_options.include_usage=false` 时不得额外发送 usage chunk；
- keepalive 可使用 SSE comment，但不能生成伪造 token delta。

### 7.3 Responses 非流式

Responses response 至少需要独立表示：

```text
id
object = "response"
created_at
status
model
output[]
usage
error
incomplete_details
```

规则：

- text assistant output 生成 `message`/`output_text` 类型 item；
- function call 生成 function_call item；
- function call output 只作为下一轮输入，不伪造为 assistant text；
- reasoning item 只有在安全且 capability-authoritative 时公开；
- `status=incomplete` 必须带明确 `incomplete_details`；
- response id 与 Kiro conversation id 分离；
- `previous_response_id` 在未实现 state store 前必须拒绝，而不是静默丢弃。

### 7.4 Responses 流式

首批只承诺官方规范中已明确、且内部事件可以稳定生成的子集：

```text
response.created
response.in_progress
response.output_item.added
response.output_text.delta
response.output_text.done
response.function_call_arguments.delta
response.function_call_arguments.done
response.completed
response.failed
error
```

不得为了“看起来像 Responses”而把所有 Kiro event 名称硬翻译成 `response.*`。每种 event 必须有内部事件来源和测试 fixture。

## 8. Usage、缓存与成本边界

### 8.1 统一 usage domain

内部 usage 必须区分：

```text
raw_upstream_usage
effective_request_usage
cache_evidence
reported_usage
billable_usage
```

现有项目已经有 raw/reported/cache 分层的部分实现，本计划要求 OpenAI renderer 复用该分层，不新增一套互相矛盾的 token 计数。

### 8.2 OpenAI 字段映射

| 内部事实 | Chat Completions | Responses |
| --- | --- | --- |
| 输入 token | `usage.prompt_tokens` | `usage.input_tokens` |
| 输出 token | `usage.completion_tokens` | `usage.output_tokens` |
| 总量 | `usage.total_tokens` | `usage.total_tokens` |
| 真实缓存读取 | `prompt_tokens_details.cached_tokens` | `input_tokens_details.cached_tokens` |
| 真实 reasoning token | `completion_tokens_details.reasoning_tokens` | `output_tokens_details.reasoning_tokens` |
| 本地模拟 cache | 不得直接冒充 cached_tokens | 不得直接冒充 cached_tokens |

只有真实上游或可信 cache evidence 支持时，才允许填写 OpenAI cached/reasoning details。`tokenScale`、`maxSimulatedInputTokens`、`reportedUsage` projection 等本地整形策略不能直接伪装成 OpenAI 官方 usage。

### 8.3 缺少上游 usage

成功响应缺少 usage 时：

1. 先记录 `usage_source=estimated`；
2. 保留 `raw_usage` 为空；
3. 使用内部 request/output estimate 作为运营诊断；
4. 只有兼容 profile 明确允许时，才向 OpenAI 客户端返回估算值；
5. 不把估算值标记成真实 provider billing；
6. 失败响应不得把大输入估算写入成功标准 usage 字段。

## 9. 模型目录与 GPT 调度方案

### 9.1 模型列表原则

`GET /v1/models` 必须遵守：

1. 不凭静态代码新增 GPT；
2. 本地 Kiro 模型来自真实 `ListAvailableModels`/能力同步；
3. external pool 模型来自该 pool 的真实 discovery 或显式 operator allowlist；
4. 只有至少存在一个可执行 route 时才公开模型；
5. disabled、quota-blocked、过期 discovery 不能独立证明模型仍可用；
6. `gpt-*` 与 Claude family 完全分离，不使用 family fallback；
7. `maxInputTokens`、`maxOutputTokens`、reasoning/tool/media capability 优先使用真实 catalog；
8. 未知 GPT 允许进入内部 pass-through 诊断，但不应进入公开列表。

当前 `/v1/models` 已经有 `object/list/data[]` 外形，但 `display_name`、`owned_by`、`contextWindow`、`maxInputTokens` 等字段是当前扩展。OpenAI adapter 需要保持旧字段，同时确保 OpenAI 客户端不会被伪造模型或非执行模型误导。

### 9.2 联合可调度判断

候选 route 必须同时满足：

```text
downstream protocol supported
  && upstream protocol adapter exists
  && model exists in authoritative catalog or explicit allowlist
  && selected credential/pool supports model
  && body/tool/media/reasoning capability matches
  && route policy allows the endpoint
  && capacity/cooldown/health/priority permits dispatch
```

协议兼容性和模型能力过滤必须发生在优先级、sticky、负载选择之前。不能因为某个 GPT route 优先级高，就把不兼容的 Anthropic-only pool 选中后再失败。

### 9.3 1M context

GPT 或其他模型的 1M context 只允许按以下优先级确定：

```text
真实上游模型 catalog maxInputTokens
  > credential/pool authoritative capability
  > explicit operator model capability record
  > conservative fallback
```

不能仅凭 `gpt-*`、模型家族或名称后缀推断 1M。任何 `[1m]` 兼容后缀都必须先验证 base model 的真实 1M capability，否则应返回 model/context incompatibility。

### 9.4 优先级与 sticky

OpenAI route 复用现有“健康情况下高优先级优先、但不暴力切换”的调度原则：

1. 协议和模型 capability 先过滤；
2. sticky session 在可用且健康时保留；
3. 优先级数值更小的健康候选获得更高选择权；
4. 优先级变更后新请求平滑偏向高优先级账号；
5. 正在执行的请求不迁移、不打断；
6. 高优先级暂时失败时按 bounded retry/failover 进入同协议候选；
7. 不因一次 GPT model-not-found 把所有 Claude/Kiro 账号长期冷却；
8. OpenAI downstream 与 Anthropic downstream 的 route health 可以共享基础质量样本，但协议错误和 body capability 不能混为同一 failure reason。

## 10. External pool 改造方案

### 10.1 新增 upstream protocol 维度

建议在 external pool 增加明确字段，名称待最终 schema review：

```text
upstreamProtocol:
  anthropic_messages
  openai_chat_completions
  openai_responses
```

默认值必须是 `anthropic_messages`，保证旧配置行为不变。

可选附加字段：

```text
modelDiscoveryProtocol
requestBodyMode
rawModelMode
headerProfile
pathMode
responseUsageMode
```

### 10.2 URL 和 header

协议-aware URL 构造规则：

| upstream protocol | 默认 path |
| --- | --- |
| `anthropic_messages` | `/v1/messages` |
| `openai_chat_completions` | `/v1/chat/completions` |
| `openai_responses` | `/v1/responses` |

要求：

- 正确处理 base URL 已经带 `/v1`、带具体 path 或 trailing slash 的情况；
- `preservePath` 语义必须真正生效；
- OpenAI upstream 不得无条件注入 `anthropic-version`；
- Anthropic upstream 继续按现有 header profile 处理；
- Authorization/Bearer 与 x-api-key 根据 pool protocol/profile 显式配置；
- 客户端 header 只按 allowlist 转发；
- 不把 upstream API key、cookie、x-goog-api-key 等敏感 header 透传到另一边；
- header overrides 必须有协议级校验，禁止用 OpenAI profile 注入 Anthropic-only header。

### 10.3 body mode

`raw_passthrough` 只有在下游和上游协议相同时可用：

```text
OpenAI Chat -> OpenAI Chat pool
OpenAI Responses -> OpenAI Responses pool
Anthropic Messages -> Anthropic Messages pool
```

以下路径必须 normalized：

```text
OpenAI -> Kiro
OpenAI -> Anthropic pool
Anthropic -> OpenAI pool
Responses -> Chat pool
Chat -> Responses pool
```

如果 pool 选择 raw 但 route 需要跨协议转换，应在发送前 fail closed，并记录 `body_protocol_mismatch`，不能把原始 body 误发。

### 10.4 external pool model mapping

非 Claude 模型默认 exact match：

- `gpt-4o` 不得因包含某个 family substring 被映射到其他 GPT；
- `gpt-5.1`、`gpt-5-1` 是否等价只能由显式 model mapping rule 或真实 discovery 证明；
- OpenAI upstream 的模型 canonicalization 不复用 Claude dotted/dashed normalization；
- 映射后的 outbound model、原始 requested model、来源和规则 ID 都要进入诊断。

### 10.5 external response

external pool 的响应处理分为：

1. `upstreamProtocol=anthropic_messages`：进入现有 Anthropic parser，再转内部 events；
2. `upstreamProtocol=openai_chat_completions`：新增 OpenAI response parser；
3. `upstreamProtocol=openai_responses`：新增 Responses response/event parser；
4. usage、错误、stream terminal 和 tool call 都先进入内部 domain，再由 downstream renderer 输出。

不要直接把 OpenAI external pool response 原样返回给 Anthropic client，也不要把 Anthropic external response 原样返回给 OpenAI client。

## 11. 错误、重试和公开响应

### 11.1 OpenAI error envelope

OpenAI 路由错误统一使用：

```json
{
  "error": {
    "message": "public message",
    "type": "invalid_request_error",
    "param": "messages",
    "code": "invalid_request"
  }
}
```

同时保留现有 request-id/error-id header contract，但不返回 Anthropic `type/error/request_id` 外形作为 OpenAI body。

### 11.2 分类原则

| 错误 | 默认动作 |
| --- | --- |
| JSON malformed / required field missing | OpenAI 400，禁止换账号/池 |
| unsupported parameter/content | OpenAI 400，禁止换账号/池 |
| model not found / model unavailable | 仅在存在其他协议兼容、模型兼容候选时 bounded failover |
| quota/rate limit | 按 route policy 和 retry-after 处理 |
| Kiro transient 5xx/network/timeout | 复用现有 attempt budget，不突破总 deadline |
| upstream protocol corruption before output | 可在 pre-output budget 内换候选 |
| protocol corruption after visible output | 不伪造成功，不重放已见内容 |
| thinking signature invalid | 按现有同凭据受控 retry/降级边界，不能由 OpenAI adapter 自行放宽 |
| context length exceeded | OpenAI `context_length_exceeded`，必要时只走明确启用的 guard/retry |

### 11.3 retry header

OpenAI renderer 应提供：

- `x-should-retry: true|false`；
- `retry-after`（只有上游/本地状态有可靠值时）；
- 标准 HTTP status；
- request id；
- 不暴露 credential、pool、Redis、scheduler、上游 token 或内部 URL。

## 12. 配置、Admin 和兼容迁移

### 12.1 Runtime config

建议新增明确配置，默认关闭：

```text
openaiCompat.enabled = false
openaiCompat.chatCompletions.enabled = false
openaiCompat.responses.enabled = false
openaiCompat.allowExternalOpenAIUpstreams = false
openaiCompat.unsupportedParameterMode = "reject"
openaiCompat.reasoningExposure = "safe_only"
```

实际字段名可按现有 `Config` 风格调整，但必须满足：

- 默认不改变现有 Anthropic 路由；
- Chat 和 Responses 可以独立启用；
- downstream OpenAI 与 upstream OpenAI pool 可以独立启用；
- 任何 reload 都生成 request-scoped immutable snapshot；
- 配置变更有版本号和审计记录。

### 12.2 External pool schema migration

数据库迁移要求：

1. 新字段 nullable 或带安全默认值；
2. 旧行全部解释为 `anthropic_messages`；
3. migration 不改变既有 URL、header、supported_models、route rules；
4. rollback 时旧 binary 能读取新表；
5. 新 binary 对缺失字段使用 Anthropic 默认；
6. 管理 API 导入/导出保留未知字段处理策略和版本。

### 12.3 Admin/UI

Admin 与 `/ui` 需要显示但不能隐藏以下维度：

- downstream protocol；
- upstream protocol；
- body mode；
- model discovery source；
- supported model count；
- last successful discovery time；
- protocol mismatch/error count；
- raw/normalized compatibility；
- whether usage is raw or estimated。

UI 的“测试连接/测试模型”必须根据 upstream protocol 发送相应 body，不能继续所有 external pool 都发送 Anthropic `/v1/messages`。

## 13. 分阶段实施规划

### P0：协议合同和内部设计冻结

状态：`Planning`

工作：

- 固定 Chat/Responses 首批支持字段；
- 固定 unsupported parameter 策略；
- 定义 internal request/event/usage/error types；
- 定义 downstream/upstream protocol enum；
- 定义 model capability 和 route compatibility matrix；
- 固定 OpenAI error/SSE contract；
- 记录现有 Anthropic 行为不得回归的 golden cases。

交付物：

- 本文获得 decision review；
- 新增 OpenAI protocol contract 文档；
- OpenAPI schema fixtures；
- capability matrix；
- field support table；
- response/event golden fixtures。

通过条件：

- 不需要在编码时临时决定字段语义；
- 每个暂不支持字段都有错误类型和测试预期；
- old Anthropic route behavior remains unchanged。

### P1：OpenAI ingress 和 Chat 非流式最小闭环

状态：`Not started`

实现范围：

- 注册 `/v1/chat/completions`；
- `ChatCompletionRequest` typed DTO；
- OpenAI auth/header normalization；
- string-only `messages`；
- system/developer/user/assistant 基础历史；
- `max_tokens/max_completion_tokens`；
- model resolution；
- local Kiro normalized path；
- Chat JSON response；
- OpenAI error envelope；
- `/v1/models` 基础 OpenAI shape 兼容校验。

明确不做：

- stream；
- tools；
- Responses；
- image/file；
- previous response state；
- reasoning output exposure。

验收：

- `hi` 非流式能经过 fake Kiro upstream 产生 `chat.completion`；
- Kiro upstream 错误能映射为 OpenAI error；
- GPT 模型只有在 fake/real capability catalog 存在时才可调度；
- Anthropic `/v1/messages` focused regression 全通过。

### P2：Chat 流式和 terminal lifecycle

实现范围：

- OpenAI Chat SSE renderer；
- role/content delta；
- finish_reason；
- `[DONE]`；
- `stream_options.include_usage`；
- pre-output retry 和 post-output fail-closed；
- keepalive/error mapping。

验收：

- fake Kiro event stream 能稳定生成 Chat chunks；
- stream read error 不生成伪造成功；
- terminal usage 与非流式 usage 一致；
- 断开、取消、超时不泄漏 lease/upstream task。

### P3：Chat tools、图像、reasoning

实现范围：

- tools/tool_choice；
- tool call streaming；
- tool result round-trip；
- tool name/schema map；
- image_url/data URL；
- `reasoning_effort` capability mapping；
- safe reasoning usage；
- `response_format` 的明确子集。

验收：

- tool call id/name/arguments 原样可逆；
- orphan pair fail closed；
- unsupported schema/content 返回标准 OpenAI 400；
- thinking/signature 不泄漏；
- reasoning token 不被虚构。

### P4：Responses 非流式

实现范围：

- `/v1/responses` route；
- `input` string/items；
- `instructions`；
- output item renderer；
- function call/function call output；
- response usage/status/error；
- `previous_response_id` 明确拒绝或接入 state store。

验收：

- text、tool、invalid item 三类 fixture；
- response id 与 session id 分离；
- status/incomplete/error 语义可回放；
- Chat 和 Responses 共用 internal request/event，但 renderer 不互相伪装。

### P5：Responses 流式

实现范围：

- response lifecycle events；
- text delta/done；
- function call arguments delta/done；
- response.completed/failed；
- stream usage；
- pre-output retry。

验收：

- 官方 OpenAPI 事件 fixture；
- 每个事件有内部来源；
- 没有将未知 Kiro event 静默转成 Responses text；
- stream cancellation/EOF/invalid event 具备 fail-closed 行为。

### P6：External pool protocol-aware upstream

实现范围：

- `upstreamProtocol` schema/config/API/UI；
- OpenAI Chat/Responses URL/header/auth；
- protocol-aware raw/normalized body mode；
- OpenAI upstream response parser；
- model discovery；
- usage/billing；
- protocol mismatch guard；
- old row migration/default。

验收：

- fake Anthropic pool、fake OpenAI Chat pool、fake OpenAI Responses pool 三组；
- downstream/upstream cross-product matrix；
- raw same-protocol path byte identity；
- cross-protocol path normalized；
- Anthropic old external pool regression zero behavior drift。

### P7：模型目录、调度和 1M context

实现范围：

- protocol-aware model capability；
- local Kiro discovery 与 external discovery source；
- GPT exact model support；
- model list routeability filter；
- priority/sticky/cooldown integration；
- real catalog context window；
- OpenAI `context_length_exceeded` mapping。

验收：

- 未发现的 GPT 不出现在 `/v1/models`；
- 发现的 GPT 只进入兼容 upstream route；
- high-priority healthy candidate wins without hard cutover；
- priority change is smooth for in-flight requests；
- 1M 只由 authoritative capability 触发；
- model unavailable fallback 不打到不兼容协议池。

### P8：灰度、发布和生产观察

实现范围：

- request API key/route level enablement；
- metrics and audit；
- dashboard protocol dimension；
- release notes；
- rollback switch；
- staged canary。

验收：

- OpenAI flag off 时既有路由、启动日志和资源使用不变化；
- canary key only；
- no protocol mismatch spike；
- usage raw/reported/billable split remains valid；
- release candidate and rollback both verified。

## 14. 测试与验证矩阵

### 14.1 单元测试

至少覆盖：

- Chat DTO strict/lenient parsing；
- Responses input item parsing；
- role/content conversion；
- tool call/result pairing；
- tool name/schema mapping；
- model exact matching；
- model protocol capability filtering；
- reasoning effort mapping；
- context window selection；
- OpenAI error mapping；
- usage mapping；
- cache details mapping；
- response id generation；
- OpenAI SSE event rendering；
- malformed/unknown event fail closed。

### 14.2 HTTP contract tests

使用本地 fake upstream 和临时端口，不触碰现有 `9022`：

| Case | Expected |
| --- | --- |
| `POST /v1/chat/completions` basic non-stream | 200 `chat.completion` |
| Chat stream | `text/event-stream` + chunks + `[DONE]` |
| Chat tool call | tool_calls with stable id/name/arguments |
| Chat tool result continuation | next request reaches Kiro with paired history |
| Chat image | valid image mapping or explicit unsupported error |
| Chat reasoning | capability-dependent mapping, no hidden leak |
| `POST /v1/responses` text | 200 response object |
| Responses stream | official response event subset |
| malformed JSON | OpenAI 400 |
| unsupported field | OpenAI 400 `unsupported_parameter` |
| unknown model | OpenAI model-not-found |
| Kiro 429/5xx/timeout | bounded retry/error |
| upstream protocol corruption | no fake success |
| no usage | estimated/unknown semantics explicit |
| cache evidence | only real evidence becomes cached token details |
| `/v1/models` | only routeable real models |

### 14.3 Upstream cross-product tests

必须至少有三种 fake upstream：

1. fake Kiro IDE/CLI EventStream；
2. fake Anthropic Messages upstream；
3. fake OpenAI Chat/Responses upstream。

每种 upstream 至少返回：

- text；
- tool call；
- thinking/reasoning metadata；
- usage；
- stream event；
- model unavailable；
- rate limit；
- protocol malformed；
- early error before output；
- error after visible output。

### 14.4 真实客户端验证

首选顺序：

1. `curl`/Node raw HTTP contract；
2. 官方 OpenAI SDK smoke（若本地环境具备，不把依赖写入生产项目）；
3. 真实 Claude Code CLI 继续验证 Anthropic `/cc` 不回归；
4. 真实 OpenAI client 只在用户明确授权和隔离服务上执行。

不能把一次 `curl` 200 当成 SDK 兼容完成；至少要验证 SDK 能解析 response object、stream iterator、tool calls 和 errors。

### 14.5 调度/资源/清理门禁

每次动态验证必须：

- 使用临时端口；
- 使用独立 PostgreSQL database/schema；
- 使用 Redis 独立 DB 或 caller-owned prefix；
- 使用仓库外 scoped target/artifact root；
- 不调用现有 `9022`；
- 不发送真实生产凭据；
- 子进程使用最小环境；
- 测试结束停止自己的 process group；
- 确认临时端口无 listener；
- 删除 owned target、fake upstream、报告和临时数据库/Redis prefix；
- 记录 `removed=true`、`reservation_released=true` 或等价清理证据。

当前 `/tmp/kiro-rs-rust-analyzer-target` 约 31G 且被现有本地服务 PID 2205 使用。后续 OpenAI 动态验证禁止复用该目录，避免再次扩大共享 target 和磁盘占用。

## 15. 发布、灰度和回滚

### 15.1 默认关闭

第一个实现版本必须满足：

```text
OpenAI route flag = off
OpenAI external upstream flag = off
existing Anthropic route = unchanged
```

即使代码已合并，未启用时也不能改变：

- `/v1/messages` body parsing；
- `/cc/v1/messages` stream；
- external Anthropic pool；
- model list 中现有模型；
- usage/billing 字段；
- scheduler selection。

### 15.2 灰度顺序

1. fake upstream isolated；
2. local low-concurrency Kiro test account；
3. canary request API key；
4. 单一 OpenAI external pool；
5. 低比例真实流量；
6. 扩大到其他 model/protocol；
7. 才考虑默认启用。

### 15.3 回滚条件

出现以下任一情况立即关闭 OpenAI flag，不需要回滚整个 Anthropic release：

- OpenAI stream 终止事件缺失；
- tool call arguments 不可逆；
- usage 与 raw/reporting 口径混淆；
- GPT 请求进入错误协议 pool；
- OpenAI route 泄漏 Anthropic/Kiro 内部错误；
- 模型列表出现无法执行的 GPT；
- OpenAI 兼容开启后 Anthropic latency/error/regression 明显上升；
- 资源/lease/process cleanup 不完整。

## 16. 验收标准

只有同时满足以下条件，才能把状态从 `Planning` 改为 `Implementation Ready`：

1. P0 字段支持/拒绝表被接受；
2. internal request/event/usage/error contract 被接受；
3. downstream/upstream protocol matrix 被接受；
4. raw/normalized/跨协议边界被接受；
5. GPT 模型目录和调度规则被接受；
6. reasoning/thinking 暴露策略被接受；
7. `previous_response_id`/state store 范围被接受；
8. fake upstream 测试矩阵和清理门禁被接受；
9. release flag/rollback strategy 被接受；
10. 无需在编码阶段临时发明核心语义。

只有同时满足以下条件，才能把实现状态改为 `Focused Validated`：

- Chat L1/L2 contract 全通过；
- Anthropic existing regression 全通过；
- local Kiro fake/isolated dynamic pass；
- model list 只展示真实可路由模型；
- OpenAI errors/usage/stream lifecycle 经过独立证据；
- 没有残留进程、端口、数据库、Redis prefix 或大体积测试产物。

只有满足以下条件，才能宣称 `Production Compatible`：

- Chat 与 Responses 的已承诺等级均有真实客户端证据；
- local Kiro、Anthropic external、OpenAI external 三类 route 有协议矩阵证据；
- 生产低流量观察无 protocol mismatch/usage contamination；
- 版本、配置迁移和回滚均完成；
- 生产观察窗口结束后更新 evidence index。

## 17. 开放问题

以下问题在实现前必须形成 decision record，当前不能默认为已决定：

1. OpenAI Chat 与 Responses 是否同时开放，还是先只开放 Chat？
2. `developer` 与 `system` 在 Kiro 内部的最终优先级和拼接方式是什么？
3. Chat `response_format=json_schema` 是否首批支持，还是统一返回 unsupported？
4. OpenAI reasoning output 是否只提供 usage，不提供 reasoning item/text？
5. Responses `previous_response_id` 是否实现 durable response store？
6. OpenAI external pool 是否允许下游 Anthropic 通过 internal IR 转发，还是只允许同协议 raw/normalized？
7. `/v1/models` 是否按 request API key、route 或协议筛选，还是公开合并可路由目录？
8. OpenAI 模型 alias 是否允许显式 mapping rule，还是首批 exact-only？
9. `n>1`、seed、logprobs、audio、custom tool 的拒绝错误字段如何固定？
10. 是否需要给 OpenAI route 单独的 cache/usage policy namespace？
11. Responses 事件中 reasoning、tool call、text 的公开顺序如何与当前 Kiro event 保持一致？
12. 是否允许用户为某个 external pool 指定“仅 OpenAI Chat”而禁止跨协议 normalized bridge？
13. 官方 OpenAI SDK smoke 是否纳入 CI，还是只作为隔离验证脚本？

## 18. 当前状态与下一步

当前状态保持：

```text
Planning / implementation not started
```

下一步只允许做以下文档/评审动作：

1. 审核 P0 字段和不支持字段表；
2. 审核 internal IR 与 upstream protocol matrix；
3. 审核 GPT 模型列表和 1M context 规则；
4. 审核 thinking/reasoning、response state 和 external pool migration 决策；
5. 将已确认决定写入该 plan 的 `decisions/`；
6. 只有 P0 验收条件满足后，才进入代码实现。
