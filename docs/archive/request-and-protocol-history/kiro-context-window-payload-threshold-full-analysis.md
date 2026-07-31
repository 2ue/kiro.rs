# Kiro 1M 上下文、请求体阈值与 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 完整分析

## 1. 文档目的

本文档用于独立说明以下线上问题，不依赖任何历史对话：

```text
400 Bad Request
{"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}
```

以及相关的另一类错误：

```text
400 Bad Request
{"message":"Improperly formed request.","reason":null}
```

本文档需要回答：

1. 为什么模型支持 1M token 上下文，但 Kiro 仍可能拒绝一个约 754KB 的 JSON 请求体。
2. 这是否意味着 1M 上下文能力无效。
3. 当前 `kiro.rs` 是否存在 JSON 格式化、消息拼接、协议转换或策略缺陷。
4. 是否可以通过 Anthropic 兼容接口触发 Claude Code CLI 在本地自动 compact。
5. 其他参考项目是否尝试解决该问题，它们的策略是否值得吸收。
6. 在不破坏工具调用、PDF、图片、多模态、Claude Code CLI 和 Anthropic 兼容能力的前提下，当前项目应如何落地优化。

本文档只做分析和落地设计。文档本身不代表所有方案已经实现。

## 2. 核心结论

### 2.1 不能把 754KB 请求体等同于超过 1M token 上下文

`754KB JSON body` 和 `1M token context window` 是两个不同维度。

- JSON body 字节数衡量的是发送给 Kiro HTTP 接口的原始序列化请求体大小。
- token 上下文窗口衡量的是模型推理阶段可接受的输入 token 数量。
- JSON body 中还包含协议字段、工具定义、JSON schema、转义字符、base64 图片、PDF 提取文本、工具结果、历史消息包装和 Kiro 特有字段。
- 字节数和 token 数没有固定一一对应关系。

因此，不能因为 body 是 754KB 就判断模型上下文已经用满，也不能因为模型支持 1M token 就推导出 Kiro HTTP 入口必须接受任意小于 1MB 的 JSON body。

### 2.2 现象更像 Kiro 入口层或内容校验层的额外限制

现网错误由 Kiro 上游明确返回：

```json
{
  "message": "Input is too long.",
  "reason": "CONTENT_LENGTH_EXCEEDS_THRESHOLD"
}
```

更合理的解释是：

- 模型上下文窗口是一层限制。
- Kiro API 入口、网关、业务校验或内容块校验还有另一层限制。
- 入口层可能按原始 JSON body、单个字段、单个 content block、tool result、图片、文档或工具定义总量限制请求。
- 入口层限制可能比模型上下文窗口更保守，并且可能因 endpoint、模型 SKU、请求形态或多模态内容而变化。

从使用体验看，这种分层限制确实不理想：客户端认为 1M 上下文仍有空间，但 Kiro 入口提前拒绝请求。项目需要在代理层补齐兼容策略，而不是把错误简单归类为“模型上下文满了”。

### 2.3 当前证据不支持“JSON 格式化 bug 导致 body 膨胀”

当前项目在 `src/anthropic/payload_guard.rs` 中使用：

```rust
serde_json::to_string(request)
```

它生成紧凑 JSON，不会添加 pretty print 缩进。

现网样本中：

```text
pre_endpoint_body_bytes = 754518
upstream_body_bytes     = 754599
```

endpoint transform 前后只增加了 81 字节。这个增量不足以解释问题，也不支持“provider 层重复包装导致请求体异常膨胀”的判断。

当前更高概率的问题是：

1. 请求本身包含较大的当前消息、当前 tool result、PDF 提取文本、图片或工具定义。
2. 当前生产配置关闭了 payload guard 和压缩，导致大请求基本原样透传。
3. 当前 payload guard 主要裁剪旧 history，无法处理由当前内容或 tools 引起的超限。
4. Kiro 上游存在独立于模型上下文窗口的 payload/content threshold。

### 2.4 当前项目存在策略缺口，但没有发现明确的序列化错误

当前 `kiro.rs` 已经具备：

- Anthropic -> Kiro 请求转换。
- 工具名称缩短。
- JSON schema 规范化。
- `tool_use` / `tool_result` 配对修复。
- 发送 Kiro 前按最终 JSON body 字节数检查的 payload guard。
- 超限时从最旧 history 开始裁剪。
- payload byte breakdown 诊断。
- 对 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 和 `Improperly formed request` 的下游错误映射。

但当前项目仍缺少：

- 针对历史 tool result 的内容级截断。
- 针对 tools schema / description 的预算控制。
- 针对 WebFetch、PDF 文本和图片的专门预算策略。
- 可选的服务端历史摘要压缩。
- upstream 400 发生时稳定落库的 payload breakdown。
- endpoint/model-aware 的阈值标定。
- 明确区分“旧历史可安全裁剪”和“当前用户输入不应静默修改”的策略分层。

### 2.5 不建议依赖接口强制触发 Claude Code CLI 本地 compact

当前没有发现 Anthropic `/v1/messages` 兼容协议中存在可靠的“服务端要求 Claude Code CLI 立即 compact 并自动重试”指令。

可以做的事情包括：

- 提供 `/v1/messages/count_tokens`，让 Claude Code CLI 自己进行 token 判断。
- 对真实 1M 模型在响应 `model` 字段保留正确的 `[1m]` 特征，避免 CLI 错按 200k 提前 compact。
- 在代理端主动裁剪或摘要旧历史。
- 对超限请求返回清晰错误。

不建议通过伪造 token 数、故意高估 `/count_tokens` 或返回模糊错误来诱导 CLI compact。该行为依赖客户端内部实现，版本变化后不稳定，也可能导致过度 compact 和上下文损失。

## 3. 已确认的线上样本

### 3.1 样本信息

本次线上排查是只读分析，没有重启、修改或部署远程服务。

确认到的失败样本：

```text
时间:            2026-06-01 10:51:13.694952 CST
request_id:      req_01qmHTjPR5tsPHV2kU6yrLsp
endpoint:        /v1/messages
stream:          true
requested_model: claude-haiku-4-5-20251001
upstream_model:  claude-haiku-4.5
conversation_id: 62f07c90-8d95-4d2a-9cf0-03aa7c6dda0f
credential:      #18，邮箱已脱敏
```

请求体尺寸：

```text
pre_endpoint_body_bytes = 754518
upstream_body_bytes     = 754599
compression_enabled     = false
```

Kiro 上游响应：

```json
{
  "message": "Input is too long.",
  "reason": "CONTENT_LENGTH_EXCEEDS_THRESHOLD"
}
```

### 3.2 线上配置快照

排查时读取到的线上运行配置：

```text
payloadGuardEnabled      = false
payloadGuardMaxBytes     = 460800
payloadGuardTrimHistory  = false
compression.enabled      = false
compression.whitespaceCompression = true
```

关键点：

- `payloadGuardMaxBytes=460800` 虽然存在，但 `payloadGuardEnabled=false`，所以不会执行 payload guard。
- `compression.enabled=false`，所以不会运行 JSON whitespace 处理。
- 该请求在发送给 Kiro 前没有经过 history trimming。

### 3.3 该样本能证明什么

该样本可以证明：

1. 当前服务完成了凭据调度和模型映射。
2. 请求确实已经发送到 Kiro。
3. Kiro 上游返回了 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`。
4. provider endpoint transform 只增加了 81 字节，不是主要膨胀来源。
5. 该请求失败不是凭据不可用、账号额度不足、网络连接失败或调度失败。

### 3.4 该样本不能证明什么

仅凭现有日志不能证明：

1. Kiro 官方限制固定是 450KiB、600KB、615KB 或其他常量。
2. 754KB 的主要来源到底是 history、tools、tool_results、PDF、图片还是当前 user content。
3. 所有模型和所有 endpoint 都具有相同阈值。
4. 请求在 token 层已经超过模型上下文窗口。
5. 只要 body 小于某个固定值就一定成功。

现有日志缺少失败请求的组件级 byte breakdown，因此不能对根因做过度确定的归类。

## 4. 为什么 1M 上下文和 754KB 拒绝可以同时发生

### 4.1 模型上下文窗口

模型上下文窗口通常表示模型推理可接受的 token 总量。它关注：

- system prompt。
- messages。
- tools。
- tool results。
- 图片或文档转换后的模型输入。
- 预留输出 token。

### 4.2 HTTP 请求体和 Kiro payload

代理发给 Kiro 的 body 不只是“用户文本”。它还包括：

- `conversationState`。
- `history`。
- `currentMessage`。
- `userInputMessageContext`。
- `tools`。
- 每个工具的 `toolSpecification`。
- 每个工具的 JSON schema。
- `toolResults`。
- 图片 base64 数据或 Kiro 图片结构。
- 文档转文本后的内容。
- `conversationId`。
- `agentContinuationId`。
- `agentTaskType`。
- `chatTriggerType`。
- endpoint transform 注入字段。

这些字段都会计入 JSON 字节数，但它们与模型 token 使用量并非固定比例关系。

### 4.3 特别容易产生高字节数的内容

#### base64 图片

base64 通常会比原始二进制更大。图片即使在 token 侧经过视觉编码，HTTP body 中仍可能携带较大的 base64 字符串。

#### PDF 或 document

当前转换逻辑会把 document source 转成文本并加入 message content。大型 PDF 的文本提取结果可能快速增大 body。

#### tool result

Claude Code、MCP、文件读取、日志查看、网页抓取和命令输出很容易产生大段工具结果。对模型来说这些内容可能仍处于 1M token 窗口内，但 Kiro 入口可能先按字节或字段长度拒绝。

#### tools schema

工具定义每次请求都可能再次发送。大量 MCP 工具、较长 description、复杂 JSON schema 会显著增加 body。即使对话只有少量 message，请求仍可能很大。

#### JSON 转义

字符串中的引号、反斜杠、换行等会经过 JSON 转义。转义后 body 字节数可能比原始文本明显增加。

### 4.4 正确理解

正确的理解不是：

```text
754KB > Kiro 上下文，所以失败。
```

也不是：

```text
模型支持 1M token，所以任何 754KB 请求必然成功。
```

正确的理解是：

```text
模型可能仍有大量 token 空间，
但 Kiro 请求入口或内容校验层提前拒绝了当前 payload。
代理层必须同时适配 token window 和 payload threshold。
```

## 5. 当前 `kiro.rs` 请求转换链路

### 5.1 主链路

当前 `/v1/messages` 大致执行以下流程：

1. 接收 Anthropic 兼容请求。
2. 解析 requested model 并执行模型映射。
3. 将 Anthropic messages 转换为 Kiro `ConversationState`。
4. 构造 `KiroRequest`。
5. 调用 `guard_kiro_request`。
6. provider 根据凭据和 endpoint transform body。
7. 调用 Kiro 上游。
8. 把 Kiro 响应映射回 Anthropic 兼容响应。

调用 payload guard 的入口：

```text
src/anthropic/handlers.rs
```

核心实现：

```text
src/anthropic/payload_guard.rs
```

### 5.2 current message 和 history

转换逻辑位于：

```text
src/anthropic/converter.rs
```

主要行为：

- 最后一条 user message 转为 `currentMessage.userInputMessage`。
- 更早消息转为 `conversationState.history`。
- 当前 user content 中的 text 汇总为文本。
- image 转为 Kiro image。
- document 转为文本并加入 content。
- tool_result 转为 `userInputMessageContext.toolResults`。
- tools 转为当前 message context 中的工具定义。

这不是明显错误，但有一个重要后果：

> 即使 messages 数量很少，当前消息、当前工具列表、当前 tool results、图片和 PDF 仍然可能让 body 很大。

### 5.3 system prompt

当前 converter 会把系统提示转换到 Kiro 可接受的对话结构中。系统提示本身可能较长，Claude Code 还会携带运行环境、工具描述和其他上下文。

这属于兼容性策略，不是序列化 bug。但当系统提示很大时，它会消耗 body 预算。

### 5.4 tools

当前 converter 会：

- 规范化 schema。
- 缩短超长工具名。
- 为历史中存在但当前 tools 列表缺失的工具生成 placeholder definition。
- 保证工具历史与 tools 列表尽量一致。

这是为避免 Kiro `Improperly formed request` 所需的兼容处理。

但当前没有工具预算策略：

- 没有统计 tools schema 总量后主动压缩 description。
- 没有按需加载或 tool search。
- 没有针对 schema 注释字段的预算清理。
- 没有在工具数量极多时只发送必要工具。

因此，大量 MCP 工具可能导致 current tools 成为 body 的主要来源。

### 5.5 tool result

当前 converter 会把当前 user message 中的 tool result 转为 Kiro tool results。

payload guard 在修复孤立 tool result 时，会把无法配对的 tool result 从结构化结果中移除，并把可读内容追加到文本 content，以减少上下文丢失。

这个行为是合理的协议修复，但存在边界：

- 如果孤立 tool result 很大，文本化后仍然很大。
- 如果超限主要来自当前合法 tool result，当前 guard 不会截断。
- 如果历史 tool result 很大，当前 guard 只会通过裁剪完整旧 history unit 间接删除，不会先做内容级压缩。

### 5.6 document 和 image

当前 converter 支持：

- base64 图片。
- data URL 图片。
- URL 图片。
- document source 转文本。

但当前 payload guard 不做：

- 图片降采样。
- base64 图片预算。
- 历史图片移除。
- PDF 文本截断。
- document 摘要。

这保证了能力不被静默削弱，但也意味着大多模态请求可能继续触发 Kiro 上游阈值。

## 6. 当前 payload guard 的实际行为

### 6.1 默认配置

当前代码默认配置：

```text
payloadGuardEnabled     = true
payloadGuardMaxBytes    = 450 * 1024 = 460800
payloadGuardTrimHistory = true
```

定义位置：

```text
src/model/config.rs
```

必须强调：

> `460800` 是当前项目采用的保守本地阈值，不是已经证明的 Kiro 官方统一限制。

### 6.2 guard 会执行的操作

当 guard 开启时：

1. 使用紧凑 JSON 序列化计算 body 字节数。
2. 对齐 history 起点，使其从 user message 开始。
3. 移除空 `tool_uses`。
4. 修复孤立 `tool_results`。
5. 文本化被移除的孤立 tool result，尽量保留可读内容。
6. 移除无配对 `tool_uses`。
7. 如果超过 `payloadGuardMaxBytes` 且允许 trim history，则从最旧 history unit 开始裁剪。
8. 每次裁剪后重新修复配对并重新序列化。
9. 记录最终大小和 `still_oversized`。

### 6.3 guard 不会执行的操作

当前 guard 不会：

1. 截断当前 user content。
2. 截断当前合法 tool result。
3. 截断历史 tool result 内容，只会删除完整旧 history unit。
4. 压缩 tools schema 或 description。
5. 压缩 PDF 文本。
6. 压缩图片或 base64。
7. 生成旧历史摘要。
8. 主动要求 Claude Code CLI compact。

### 6.4 `still_oversized` 的行为

当前代码在删完 history 后，如果 body 仍超过本地阈值：

```text
report.still_oversized = true
```

随后继续把请求发给 Kiro，而不是本地拒绝。

该行为符合此前确定的产品约束：

> 如果超限来自当前 user message、当前 tool result、PDF 或图片，不要默认本地拒绝或静默截断；让 Kiro 返回真实错误。

### 6.5 配置说明与实际行为曾存在文档漂移

实施前，部分配置注释和 README 仍描述：

```text
仍超限会直接返回客户端错误
```

但当前实际代码已经改为：

```text
标记 still_oversized 后继续发送给 Kiro
```

本轮实施已经同步更新：

- `src/model/config.rs` 注释。
- `README.md` 配置表。
- 部署文档。
- 管理后台字段说明。
- 既有分析文档中的历史描述。

## 7. 当前诊断能力

### 7.1 已有 byte breakdown

当前项目已经有：

```text
breakdown_kiro_request
```

它可以记录：

```text
total_bytes
history_bytes
current_message_bytes
current_content_bytes
current_tools_bytes
current_tool_results_bytes
current_images_bytes
history_entries
current_tool_count
current_tool_result_count
current_image_count
largest_tool_bytes
history_tool_use_count
history_tool_result_count
```

这是正确方向。

### 7.2 当前日志触发条件

当前 breakdown 只会在以下情况记录为 info：

- guard 修改了 payload。
- `still_oversized=true`。
- `final_bytes > max_bytes * 70%`。

如果 guard 被关闭：

- report 是 disabled。
- `max_bytes` 不一定进入有效判断。
- 大请求可能只留下 provider 层的 body 总大小。
- upstream 返回 400 后，不一定能稳定看到组件级 breakdown。

这正是现网样本只能看到 754518 / 754599 总字节数，而无法立即确认主要来源的原因。

### 7.3 建议补齐

upstream 返回以下错误时，无论 guard 是否开启，都应该记录并落库 breakdown：

```text
CONTENT_LENGTH_EXCEEDS_THRESHOLD
Input is too long
Improperly formed request
IMPROPERLY_FORMED
```

usage detail 至少应持久化：

```json
{
  "payload_breakdown": {
    "total_bytes": 0,
    "history_bytes": 0,
    "current_message_bytes": 0,
    "current_content_bytes": 0,
    "current_tools_bytes": 0,
    "current_tool_results_bytes": 0,
    "current_images_bytes": 0,
    "history_entries": 0,
    "current_tool_count": 0,
    "current_tool_result_count": 0,
    "current_image_count": 0,
    "largest_tool_bytes": 0,
    "history_tool_use_count": 0,
    "history_tool_result_count": 0
  }
}
```

建议再补充：

```text
system_prompt_bytes
history_images_bytes
history_tool_results_bytes
largest_history_tool_result_bytes
largest_current_tool_result_bytes
document_text_bytes
largest_document_text_bytes
endpoint_transform_added_bytes
```

不要记录完整敏感内容，只记录大小、计数、类型和必要的脱敏摘要。

## 8. 当前样本的高概率来源分析

现网样本只有约 3 条 messages，但 body 已达到约 754KB。

因此，不能优先假设是“长历史消息过多”。更值得优先排查：

1. 当前 user content 很大。
2. 当前合法 tool result 很大。
3. 当前 tools schema / description 总量很大。
4. 当前 PDF document 转文本后很大。
5. 当前图片 base64 很大。
6. system prompt 很大。
7. 某些孤立 tool result 被文本化追加到 current content。

这只是高概率推断，不是已证实结论。必须通过失败请求 breakdown 验证。

## 9. Claude Code CLI 自动 compact 分析

### 9.1 Claude Code CLI 的 compact 属于客户端行为

Claude Code CLI 会根据自身的上下文估算、模型元信息和内部策略决定何时 compact。

代理可以影响的主要输入：

- `/v1/messages/count_tokens` 返回值。
- 响应中的 `model` 字段。
- 1M 上下文标识。
- 服务端错误响应。

但当前没有可靠的 Anthropic 兼容 API 字段可以表达：

```text
请本地 compact，然后自动重试本次请求。
```

### 9.2 `[1m]` 的作用

参考项目 `kirocc` 明确说明：

- Claude Code CLI 会通过响应 model ID 判断上下文窗口。
- 对真实 1M 路由，响应 model 需要保留 `[1m]` 特征。
- 如果响应没有正确标识，CLI 可能回退到 200k，并在约 160k 过早 compact。

这解决的是：

```text
CLI 过早 compact
```

它不能解决：

```text
Kiro 入口拒绝 754KB payload
```

甚至在 CLI 正确认识 1M 后，长会话会保留更多上下文，更容易触发 Kiro 入口 payload threshold。因此必须同时具备代理侧 payload shaping。

### 9.3 `/count_tokens` 的作用

当前项目提供：

```text
POST /v1/messages/count_tokens
POST /cc/v1/messages/count_tokens
POST /ha/v1/messages/count_tokens
POST /na/v1/messages/count_tokens
```

它的作用是估算 token 数，不是 compact RPC。

可以继续提高 token 估算准确度，但不应故意夸大 token 数来诱导 compact。

### 9.4 是否可以返回某种错误让 CLI compact

理论上可以实验 Claude Code CLI 对不同上下文错误的行为，但不应依赖该行为作为生产主方案：

- 客户端版本可能变化。
- 不同错误可能只会终止当前任务。
- 即使触发 compact，也不一定自动重试。
- 模糊错误会让用户难以定位真正问题。
- 当前错误可能来自当前单条大消息，compact 历史也无效。

结论：

> 保持 `/count_tokens` 正确、保持 1M model 标识正确，但把 payload threshold 兼容处理放在代理侧。

## 10. 参考项目对比

本次只读分析了以下项目：

```text
~/Desktop/procode/kiro-gateway
~/Desktop/procode/Kiro-Go
~/Desktop/procode/kiro-account-manager
~/Desktop/procode/kiro-account-manager-3460-enhanced-20260524
~/Desktop/procode/kiro.rs-foxfishc
~/Desktop/procode/kirocc
~/Desktop/procode/kiro2api
```

### 10.1 对比表

| 项目 | payload byte guard | history 裁剪 | history tool result 截断 | tools schema 压缩 | 服务端摘要历史 | 主动触发本地 CLI compact | 对 754KB 默认是否一定有效 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 当前 `kiro.rs` | 有，默认 450KiB | 有 | 无 | 无 | 无 | 无 | 仅对旧 history 型超限有效 |
| `kiro-gateway` | 有，经验值约 600KB | 可选 | 无 | 无 | 无 | 无 | 默认 auto trim 关闭，不一定有效 |
| Rust `kiro-account-manager` | 有，约 450KiB | 有 | 无 | 无 | 有实现但未确认接入主链路 | 无 | 对旧 history 型超限有机会有效 |
| enhanced account-manager | 默认 1.5MB | 可选 token trim | 有，主要处理历史结果 | 无 | 无 | 无 | 754KB 默认不会触发 byte trim |
| `kiro.rs-foxfishc` | 有，默认 hard body 阈值较大 | 有 | 有 | 有 | 无 | 无 | 内容可压缩时更强，但 754KB 不一定触发二次压缩 |
| `kirocc` | 未发现通用 guard | 无通用策略 | WebFetch 专项 | tool search 按需加载 | 无 | 无 | 仅部分场景有效 |
| `Kiro-Go` | 未发现通用 guard | 无通用策略 | 无通用策略 | 无通用策略 | 无 | 无 | 可减少系统提示噪声，但不保证解决 |
| `kiro2api` | 有部分专项处理 | 有限 | WebFetch 专项 | 无通用策略 | 无 | 无 | 仅部分场景有效 |

### 10.2 `kiro-gateway`

关键文件：

```text
kiro/payload_guards.py
kiro/config.py
kiro/converters_core.py
```

特征：

- 文档注释明确提到 Kiro 会对较大 payload 返回误导性 `Improperly formed request`。
- 经验阈值约为 `~615KB`，默认安全阈值约 `600000` 字节。
- 使用 compact JSON 计算 body。
- 可以从最旧 history 开始裁剪。
- 会对齐 history 起点。
- 会修复 orphaned tool results。
- 默认 `AUTO_TRIM_PAYLOAD=false`。

可吸收点：

- 把阈值定义为经验值而不是官方常量。
- 对 `Improperly formed request` 和大 payload 建立关联诊断。

不足：

- 默认不开 trim。
- 主要裁 history。
- 不处理当前 tool result、PDF、图片和 tools schema。

### 10.3 Rust `kiro-account-manager`

关键文件：

```text
src-tauri/src/gateway/compress.rs
src-tauri/src/gateway/converter.rs
src-tauri/src/gateway/proxy.rs
```

特征：

- 有服务端历史摘要压缩实现。
- 消息达到一定数量后，保留最近消息，把更旧消息发给 Kiro 模型生成摘要。
- 摘要请求有 token 上限，并对待摘要消息做长度限制。
- 另有约 450KiB 的 payload trimming。

重要结论：

- 当前分析只找到摘要函数定义，没有确认主请求路径实际调用。
- 实际主链路更像依赖 history trimming。

可吸收点：

- 旧历史摘要是保留语义的长期方向。
- 摘要必须缓存，避免每轮重复生成。

风险：

- 摘要本身会额外调用模型。
- 摘要失败需要可靠 fallback。
- 摘要可能丢失工具状态和精确上下文。

### 10.4 enhanced account-manager

关键文件：

```text
src/main/proxy/kiroApi.ts
src/main/proxy/tokenCounter.ts
src/main/proxy/accountPool.ts
```

特征：

- `payloadSizeLimitKB = 1536`，默认约 1.5MB。
- `enableTokenBufferReserve = false`，默认关闭 token buffer。
- 可以按模型 context window 减去 reserve 后裁剪旧 history。
- byte 阶段会截断历史中的长 tool result 到约 4000 字符。
- 把 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 归为 fatal，不切换账号重试。

可吸收点：

- 历史 tool result 优先做内容级截断，比直接删除整段历史更温和。
- `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 不应切换凭据重试。

不足：

- 默认 1.5MB 阈值对当前 754KB 样本不会触发。
- token buffer 默认关闭。
- 不能处理当前内容型超限。

### 10.5 `kiro.rs-foxfishc`

关键文件：

```text
src/anthropic/compressor.rs
src/anthropic/tool_compression.rs
src/anthropic/handlers.rs
docs/troubleshooting/400-improperly-formed-request.md
```

特征：

- 多层 input compressor。
- whitespace compression。
- thinking discard / truncate。
- tool result 头尾截断。
- tool use input 中长字符串截断。
- history 截断。
- tool schema / description 压缩。
- 空 content 修复。
- tool pairing 修复。
- 二次 adaptive compression。
- 图片相关诊断。

可吸收点：

- 多层、预算驱动的压缩管道。
- 优先压缩可恢复或低价值冗余。
- tool result 使用头尾保留，而不是简单截掉尾部。
- tools schema 独立预算。
- 图片和 base64 需要专项诊断。

不足：

- 默认 hard body 阈值约 4.5MiB。
- 如果 Kiro 实际在约 600KB 级别拒绝，754KB 不一定触发第二轮 adaptive compression。
- 部分强压缩策略会改变当前输入语义，需要配置开关。

### 10.6 `kirocc`

关键文件：

```text
README.md
internal/app/messages/execute.go
internal/app/messages/webfetch_trim.go
```

特征：

- 响应 model 保留 `[1m]`，让 Claude Code CLI 正确识别 1M 上下文。
- WebFetch 正文专项裁剪。
- 去除 WebFetch 中 data image、导航、footer、重复链接等噪声。
- 支持 tool search，把 deferred tools 留在代理侧，需要时再发送给 Kiro。
- 某些 invalid state 错误会清空 conversation id 后重试一次。

可吸收点：

- 正确回传 `[1m]`，避免 CLI 错误地过早 compact。
- WebFetch 专项裁剪。
- 大量 tools 场景可以考虑 tool search / defer loading。

风险：

- 清空 conversation id 重试不能解决真实 payload 过大。
- 对 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 无条件重试可能制造一次无效上游调用。

### 10.7 `Kiro-Go`

关键文件：

```text
proxy/translator.go
```

特征：

- 可识别 Claude Code 系统提示。
- 可替换为精简 backend prompt。
- 可清理环境噪声、git 状态、自动 memory、fast mode 标签等内容。

可吸收点：

- Claude Code 系统提示存在可压缩空间。
- 应允许可选的 Claude Code prompt filter。

风险：

- system prompt 可能包含重要约束。
- 默认替换整个系统提示会改变模型行为和签名特征。
- 更适合做显式配置项，不适合作为默认强制行为。

### 10.8 `kiro2api`

特征：

- 有 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 错误处理。
- 有 WebFetch trimming。
- 有部分 request capture 和错误分类。

可吸收点：

- WebFetch 属于高收益专项优化。
- content-length 错误应明确分类，不要误认为账号错误。

## 11. 风险分类

为了不破坏现有能力，必须把优化分为三类。

### 11.1 安全级优化

默认可开启，通常不改变用户语义：

- 紧凑 JSON 序列化。
- payload byte breakdown。
- 失败时落库组件大小。
- tool pairing 修复。
- schema 非语义字段规范化。
- 删除空数组、空对象和无效占位字段。
- 删除已经确认无配对的 tool use。
- 对齐 history 起点。
- 裁剪最旧、已经完整结束的 history unit。
- WebFetch 中删除 data image、导航、footer 和重复链接等明显噪声。

### 11.2 有损但低风险优化

需要独立配置项，并记录发生次数：

- 历史 tool result 头尾截断。
- 历史 thinking 丢弃或截断。
- 历史图片降级为占位说明。
- tools description 截断。
- schema description、examples、title 等注释字段清理。
- 旧历史摘要。

### 11.3 高风险优化

默认不启用：

- 截断当前 user message。
- 截断当前合法 tool result。
- 截断当前 PDF 文本。
- 丢弃当前图片。
- 强制替换完整 Claude Code system prompt。
- 故意伪造 `/count_tokens` 结果诱导 CLI compact。
- 把 Kiro 入口阈值误当成模型 token window。

## 12. 最终落地方案

### 12.1 总体原则

最终方案应遵循：

1. 保留真实 1M 上下文能力，不把 1M 人为降成 200k。
2. 正确回传 1M model 标识，避免 Claude Code CLI 过早 compact。
3. 不默认静默修改当前用户输入、当前 tool result、当前 PDF 或当前图片。
4. 优先压缩旧历史和可确认冗余。
5. 用真实 Kiro JSON body 字节数做预算。
6. 阈值是经验安全预算，不宣称为 Kiro 官方统一上限。
7. upstream 400 必须有组件级 breakdown，避免继续盲猜。
8. `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 属于请求级 fatal，不应切换凭据重试。
9. 不依赖客户端 compact 作为唯一解决方案。

### 12.2 P0：补齐观测，不改变请求语义

目标：

```text
下一次线上出现 CONTENT_LENGTH_EXCEEDS_THRESHOLD 时，
可以立即知道 body 的主要来源。
```

落地内容：

1. 在 converter 完成后计算完整 `PayloadByteBreakdown`。
2. provider 调用 Kiro 前记录：

```text
pre_endpoint_body_bytes
upstream_body_bytes
endpoint_transform_added_bytes
```

3. upstream 返回以下错误时，无论 guard 是否开启，写入 usage detail：

```text
CONTENT_LENGTH_EXCEEDS_THRESHOLD
Input is too long
Improperly formed request
IMPROPERLY_FORMED
```

4. breakdown 增加：

```text
system_prompt_bytes
history_images_bytes
history_tool_results_bytes
largest_history_tool_result_bytes
largest_current_tool_result_bytes
document_text_bytes
largest_document_text_bytes
```

5. 管理后台 usage detail 展示脱敏 breakdown。

验收：

- 构造 history 大、tools 大、tool result 大、PDF 大、图片大五类请求。
- 确认日志和 usage detail 能区分主要来源。
- 不记录完整 prompt、工具结果、PDF 文本、图片 base64 或敏感凭据。

### 12.3 P1：默认启用安全级 payload guard

目标：

```text
在不改变当前用户输入语义的前提下，
减少可以通过旧历史裁剪解决的 upstream 400。
```

建议默认配置：

```json
{
  "payloadGuardEnabled": true,
  "payloadGuardMaxBytes": 460800,
  "payloadGuardTrimHistory": true
}
```

说明：

- `460800` 是保守初始预算。
- 线上灰度期间根据 endpoint、模型和实际成功率标定。
- 不应写成 Kiro 官方限制。
- 如果设为 `0`，表示关闭 size limit，但仍保留协议修复。

行为：

1. 协议修复始终执行。
2. body 超预算时，从最旧完整 history unit 开始裁剪。
3. 每次裁剪后修复 tool pairing。
4. 删完 history 仍超预算时：

```text
记录 still_oversized=true
继续发送 Kiro
让 Kiro 返回真实错误
```

5. 默认不本地截断当前 message、当前 tool result、PDF 或图片；如果用户显式开启当前内容整形，再在历史处理后仍超预算时按配置执行有损裁剪。

验收：

- 旧历史导致的 754KB 请求能裁到预算内。
- 当前单条大消息仍保持原样。
- tool use / tool result 配对不被裁剪破坏。
- stream 和 non-stream 两条路径行为一致。
- `/v1/messages` 和 `/cc/v1/messages` 行为一致。

### 12.4 P2：增加可配置的历史内容压缩

目标：

```text
比删除整段旧历史更温和地回收 body 预算。
```

本轮已落地配置：

```json
{
  "payloadShaping": {
    "enabled": true,
    "truncateHistoricalToolResults": true,
    "historicalToolResultMaxChars": 8000,
    "historicalToolResultHeadLines": 80,
    "historicalToolResultTailLines": 40,
    "discardHistoricalThinking": true,
    "compressToolDefinitions": true,
    "toolDefinitionsBudgetBytes": 20000,
    "toolDescriptionMaxChars": 4000,
    "toolSchemaAnnotationMaxChars": 1000,
    "webFetchTrimEnabled": true,
    "webFetchBodyMaxChars": 12000,
    "fitCurrentPayloadToBudget": false,
    "truncateCurrentToolResults": false,
    "currentToolResultMaxChars": 80000,
    "truncateCurrentUserContent": false,
    "currentUserContentMaxChars": 120000,
    "truncateCurrentDocuments": false,
    "currentDocumentMaxChars": 80000,
    "truncateCurrentImages": false,
    "currentImagesMaxBytes": 180000
  }
}
```

当前已落地执行顺序：

1. 协议修复。
2. 清理无效空字段。
3. 普通历史 tool result 头尾保留。
4. 历史 WebFetch 专项去噪；WebFetch 不先走普通 tool result 的 `8000` 字符预算。
5. 历史 thinking 丢弃。
6. tools description 和 schema 注释字段预算压缩。
7. 裁剪最旧完整 history unit。
8. 如果仍超预算且显式开启当前内容整形，则按实际序列化 JSON body 字节数循环处理当前 tool_result、当前 document、当前纯文本 user content 和当前 images，直到低于 `payloadGuardMaxBytes` 或没有可继续处理的内容。
9. 重新计算 breakdown。
10. 仍超预算则标记 `still_oversized` 并透传。

历史 tool result 截断示例：

```text
[historical tool result truncated by proxy]
original_chars=128400
preserved=head:80_lines,tail:40_lines
```

约束：

- 只默认处理历史内容。
- 当前合法 tool result 默认不截断。
- schema 压缩不能删除 required、type、properties、enum 等语义字段。
- 每次压缩必须记录 saved bytes 和压缩类型。
- 当前内容截断是明确 opt-in：`truncateCurrentToolResults`、`truncateCurrentUserContent`、`truncateCurrentDocuments`、`truncateCurrentImages` 默认都关闭，开启后才参与“直到 fit”的循环。
- 历史摘要、历史多模态降级和图片重新编码仍是预留增强；默认关闭，当前实现不做图片缩放或重编码。

验收：

- 历史工具输出很大时，优先截断 tool result，不立即删除整轮。
- 模型仍能看到工具结果头尾和省略说明。
- schema 仍可被 Kiro 接受。
- 工具调用正确率不下降。

### 12.5 P3：WebFetch、PDF、图片专项策略

目标：

```text
对高体积内容使用领域相关压缩，而不是统一粗暴截断。
```

#### WebFetch

参考 `kirocc`：

- 移除 data image。
- 删除重复链接。
- 删除导航、footer、空白噪声。
- 保留正文开头、关键段落和结尾。
- 添加明确 proxy note。

#### PDF

建议：

- 记录 PDF 原始大小、提取文本大小、页数和最大页文本大小。
- 历史 PDF 文本允许摘要或降级。
- 当前 PDF/document 默认透传，不静默截断。
- 提供显式 opt-in 配置允许对当前 `<document>` 正文做头尾保留截断，并保持 document 标签不破坏；暂未实现分页摘要。

#### 图片

建议：

- 记录当前和历史图片数量、base64 字节数、格式和最大图片大小。
- 历史图片可配置替换为占位说明。
- 当前图片默认保留。
- 提供显式 opt-in 配置允许在仍超预算时按 JSON 体积从大到小丢弃当前图片，并向当前文本追加 proxy note；当前不做缩放、重新编码和质量预算。

验收：

- WebFetch 大页面不再携带 data image 和明显导航噪声。
- PDF、图片相关超限能通过 breakdown 快速识别。
- 默认行为不破坏当前多模态请求。

### 12.6 P4：可选服务端历史摘要

目标：

```text
长会话在保留语义的情况下减少旧历史占用。
```

建议：

- 只摘要旧历史。
- 保留最近 N 轮完整消息。
- 工具状态、文件路径、关键决策、失败原因和待办项使用结构化摘要。
- 摘要按 conversation id 和摘要边界缓存。
- 摘要失败时回退到 history trimming。
- 摘要必须可配置关闭。

建议结构：

```json
{
  "historySummary": {
    "enabled": false,
    "preserveRecentTurns": 8,
    "triggerBytes": 350000,
    "maxSummaryTokens": 2000,
    "cacheEnabled": true
  }
}
```

为什么默认关闭：

- 会增加一次模型调用。
- 会引入语义损失风险。
- 工具状态摘要不完整可能影响 Claude Code CLI。
- 需要单独做回归测试。

### 12.7 P5：正确处理 Claude Code 1M 标识

目标：

```text
让 Claude Code CLI 正确识别真实 context window，
同时不把 1M 标识误当成 payload threshold 解决方案。
```

落地要求：

1. 模型映射必须区分 200k 和 1M SKU。
2. 对真实 1M 路由，响应 model ID 应保留 CLI 可识别的 1M 特征。
3. `/messages/count_tokens` 使用真实估算，不故意夸大。
4. 不依赖 CLI compact 解决 Kiro payload threshold。
5. 对 1M 模型单独监控：

```text
context_window_tokens
estimated_input_tokens
payload_bytes
payload_threshold_failures
```

### 12.8 P6：阈值标定

目标：

```text
用实测数据替代猜测。
```

需要建立测试矩阵：

| 维度 | 示例 |
| --- | --- |
| endpoint | Kiro IDE endpoint、其他已支持 endpoint |
| 模型 | Haiku、Sonnet、Opus、200k SKU、1M SKU |
| stream | true、false |
| 内容 | 纯文本、tools、tool result、PDF、图片、混合多模态 |
| body 大小 | 300KB、400KB、450KB、500KB、600KB、700KB、900KB、1.2MB |
| history | 旧历史主导、当前内容主导 |

每次记录：

```text
HTTP status
Kiro error body
payload breakdown
model mapping
endpoint
body bytes
estimated tokens
context usage event
```

输出：

- endpoint/model-aware 的经验阈值。
- 默认安全预算。
- 灰度告警线。
- 是否需要按模型配置不同 max bytes。

## 13. 推荐配置语义

建议把配置语义明确为：

| 配置 | 含义 |
| --- | --- |
| `payloadGuardEnabled=false` | 不做 size trimming，也不做 guard 协议修复；仅保留 converter 自身修复 |
| `payloadGuardEnabled=true` | 启用发送前 payload guard |
| `payloadGuardMaxBytes=0` | 不限制 body 大小，但仍做 guard 协议修复 |
| `payloadGuardMaxBytes>0` | 使用本地经验预算进行诊断和 history trimming |
| `payloadGuardTrimHistory=true` | 超预算时允许裁剪旧 history |
| `payloadGuardTrimHistory=false` | 不裁剪 history；仍记录 breakdown 并透传 |
| `still_oversized=true` | guard 已尝试处理但仍超过本地经验预算；默认继续发 Kiro |

注意：

- 当前 README 和部分注释需要与此语义对齐。
- `payloadGuardMaxBytes` 不是模型上下文限制。
- `payloadGuardMaxBytes=0` 不等于关闭所有修复。

## 14. 错误分类建议

### 14.1 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`

下游建议返回：

```json
{
  "type": "error",
  "error": {
    "type": "invalid_request_error",
    "message": "Kiro upstream rejected the request because input content length exceeded its request threshold. This limit is separate from the model context window. Reduce oversized tools, system prompt, documents, images, tool results, or conversation history."
  }
}
```

策略：

- 不切换凭据。
- 不重复重试同一 payload。
- 落库 breakdown。
- 标记 request-level fatal。

### 14.2 `Improperly formed request`

下游建议返回：

```json
{
  "type": "error",
  "error": {
    "type": "invalid_request_error",
    "message": "Kiro rejected the converted request as improperly formed. Check model mapping, tool_use/tool_result pairing, tool schema, multimodal sources, and payload size."
  }
}
```

策略：

- 不切换凭据。
- 记录 converter warnings。
- 落库 breakdown。
- 记录 tools、tool results、images、documents 计数。

### 14.3 `Context window is full`

需要单独处理，不要与 body threshold 混淆：

- 记录 estimated tokens。
- 记录模型真实 context window。
- 记录 Kiro context usage event。
- 检查模型映射是否把 1M SKU 错映射成 200k SKU。
- 检查响应 model 是否让 Claude Code CLI 错误识别上下文窗口。

## 15. 实施顺序

建议按以下顺序执行：

### 第一阶段：观测和文档对齐

- 失败时落库完整 breakdown。
- 增加 system、document、history tool result、largest item 字节数。
- 修正文档漂移。
- 管理后台展示脱敏 breakdown。

### 第二阶段：默认安全优化

- 线上启用 payload guard。
- 保持 `payloadGuardMaxBytes=460800` 作为保守初始值。
- 保持 `payloadGuardTrimHistory=true`。
- 维持 `still_oversized` 透传策略。
- 对 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` 禁止账号切换重试。

### 第三阶段：历史内容级压缩

- 历史 tool result 头尾截断。
- 历史 thinking 清理。
- tools description/schema 注释字段预算。
- WebFetch 专项压缩。

### 第四阶段：可选增强

- PDF 分页摘要。
- 历史图片降级。
- 服务端旧历史摘要。
- tool search / defer loading。
- endpoint/model-aware 阈值。

## 16. 测试计划

### 16.1 单元测试

至少补充：

1. `PayloadByteBreakdown` 能区分 history、tools、tool results、images、documents。
2. guard disabled 时，upstream 400 仍能持久化 breakdown。
3. history tool result 头尾截断保留配对。
4. tools schema 压缩不删除 `required`、`properties`、`type`、`enum`。
5. `payloadGuardMaxBytes=0` 时只修复协议，不裁剪大小。
6. `still_oversized` 时继续透传。
7. stream 和 non-stream 错误映射一致。
8. `/v1/messages` 和 `/cc/v1/messages` 行为一致。

### 16.2 集成测试

构造：

- 大 history。
- 大 current user message。
- 大 current tool result。
- 大 historical tool result。
- 大 tools schema。
- 大 PDF document。
- 大 base64 image。
- image + tools。
- document + tools。
- thinking + tools。
- tool pairing 边界。

验证：

- 请求是否被裁剪。
- 裁剪是否只发生在允许区域。
- Kiro body 最终字节数。
- converter warnings。
- breakdown。
- 下游错误类型。
- usage detail。

### 16.3 Claude Code CLI 验证

使用 ccman 切换到当前服务后验证：

1. 普通文本会话。
2. 多轮工具调用。
3. Read 大文件。
4. WebFetch 大页面。
5. PDF。
6. 图片。
7. MCP 工具较多的会话。
8. 真实 1M 模型响应是否保留 CLI 所需 model 标识。
9. CLI 是否仍能按预期 compact。
10. payload guard 是否没有破坏工具调用。

## 17. 需要避免的错误方向

### 17.1 不要把本地 450KiB 写成 Kiro 官方上限

当前 `460800` 是保守预算。参考项目使用约 600000、615KB、1.5MB 或 4.5MiB 等不同值，说明这些都是各项目策略，不是统一官方常量。

### 17.2 不要把 754KB 直接解释为超过 1M context

这会混淆 byte 和 token，也无法解释客户端只显示约 13% 上下文使用率的情况。

### 17.3 不要默认静默截断当前输入

当前 user message、当前 tool result、当前 PDF 和当前图片可能正是用户本轮任务的核心输入。默认静默截断会造成更难发现的错误。

### 17.4 不要依赖账号切换重试

`CONTENT_LENGTH_EXCEEDS_THRESHOLD` 是 payload 级错误。换账号不会改变 payload，只会制造额外失败请求。

### 17.5 不要依赖 CLI 自动 compact 作为唯一方案

CLI compact 不能解决当前单条大输入，也不能保证处理 Kiro 入口层 body threshold。

## 18. 最终建议

当前最合理的最终方案是：

1. 保持真实 1M 上下文模型映射和响应标识。
2. 线上默认启用当前 payload guard，使用 450KiB 作为保守经验预算。
3. 保持当前内容有损处理默认关闭；未显式开启或开启后仍无法 fit 时继续 `still_oversized` 透传，让 Kiro 返回真实错误。
4. 优先增加 upstream 400 时的完整、脱敏 payload breakdown 落库。
5. 增加历史 tool result 头尾截断、历史 thinking 清理、tools schema/description 预算和 WebFetch 专项裁剪。
6. 当前 tool result、当前纯文本 user content、当前 document/PDF 文本和当前图片的有损处理已经做成明确 opt-in，不默认开启；如果开启，会按实际 JSON body 字节数循环收缩到配置预算内。
7. 后续通过 endpoint/model-aware 压测标定阈值，不把 450KiB 或 600KB 当成官方结论。
8. 不通过接口伪造 token 或错误来强制 Claude Code CLI compact。
9. 可在第二阶段后评估服务端旧历史摘要，但默认关闭，先保证工具调用和多模态能力稳定。

## 19. 关联文件

当前项目：

```text
src/anthropic/converter.rs
src/anthropic/payload_guard.rs
src/anthropic/handlers.rs
src/kiro/provider.rs
src/http_client.rs
src/model/config.rs
README.md
[`kiro-400-improperly-formed-request-analysis.md`](kiro-400-improperly-formed-request-analysis.md)
[`anthropic-tools-signature-compatibility-analysis.md`](anthropic-tools-signature-compatibility-analysis.md)
```

参考项目：

```text
../kiro-gateway/kiro/payload_guards.py
../kiro-gateway/kiro/config.py
../kiro-account-manager/src-tauri/src/gateway/compress.rs
../kiro-account-manager/src-tauri/src/gateway/proxy.rs
../kiro-account-manager-3460-enhanced-20260524/src/main/proxy/kiroApi.ts
../kiro.rs-foxfishc/src/anthropic/compressor.rs
../kiro.rs-foxfishc/src/anthropic/tool_compression.rs
../kiro.rs-foxfishc/src/anthropic/handlers.rs
../kirocc/README.md
../kirocc/internal/app/messages/execute.go
../kirocc/internal/app/messages/webfetch_trim.go
../Kiro-Go/proxy/translator.go
```

## 20. 文档状态

```text
文档日期: 2026-06-01
分析范围: 当前本地代码、只读线上样本、多个本地参考项目
代码修改: 已实施 payload shaping、payload breakdown 落库、运行时配置和双管理后台配置项
线上服务修改: 无
远程服务重启: 无
远程部署: 无
```
