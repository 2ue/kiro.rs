# Anthropic 工具支持与签名保真兼容性分析

日期：2026-05-30
更新：2026-05-31

## 1. 背景

当前服务是一个 Anthropic/Claude Code 兼容入口，实际请求会被转换后发送到 Kiro 上游。用户侧希望解决黑盒检测或真实使用中暴露出的以下问题：

- 工具调用使用时不稳定，可能出现 `400 Improperly formed request`、工具不触发、工具结果不匹配、工具参数异常等问题。
- 模型签名验证只达到“部分合格”，尤其是 thinking signature、redacted thinking、工具调用多轮之后的签名 round-trip 容易不符合官方表现。
- 后续还需要处理 PDF 文档识别、结构化输出等官方能力兼容问题，但本阶段优先级为：
  1. 先解决工具支持，保证真实使用不报错。
  2. 再解决签名过检，保证签名和相关协议特性尽量符合官方。

本文目标是脱离当前对话也能完整理解问题原因、工程目的、方案设计、实施顺序、风险边界和验收标准。

## 2. 核心结论

优先解决工具支持是正确顺序。签名过检不是一个孤立的单轮响应问题，很多检测会走多轮工具链路：

```text
user request
-> assistant thinking + signature + tool_use
-> user tool_result
-> assistant final response
```

如果工具链路本身存在以下问题，签名检测一定会受影响：

- `tool_use` 和 `tool_result` 配对不稳定。
- 历史消息被 payload guard 或 converter 静默修复。
- 上一轮 assistant 的 thinking/signature 在转换历史时被丢弃。
- 工具结果内容被降级、忽略或错误转成普通文本。
- `tool_choice` 没有真正生效，导致检测期望的工具没有被调用。

因此正确路线是：

```text
第一阶段：稳定工具调用链路，减少真实使用报错。
第二阶段：在稳定工具链路上做 thinking/signature/redacted_thinking 保真。
第三阶段：继续补 PDF、structured output 等官方能力。
```

### 2.1 2026-05-31 已实施状态

截至 2026-05-31，第一阶段中的 `tool_choice` 兼容和 payload 诊断已经落地，代码位置如下：

- `src/anthropic/converter.rs`
  - 支持解析 Anthropic 风格 `tool_choice` 字符串和对象：`auto`、`any`、`none`、`{"type":"tool","name":"..."}`。
  - `tool_choice: none` 会在当前请求中省略 tools，降低本轮误触发工具的概率。
  - `tool_choice: {"type":"tool","name":"x"}` 会把当前请求 tools 过滤到指定工具；匹配同时考虑原始工具名和 Kiro sanitized 后的工具名。
  - 如果强制指定的工具不存在，不做本地拒绝，而是保留全部工具并记录 warning，优先保证真实使用不中断。
  - 非 strict 兼容模式会插入很小的 Kiro-facing steering 前缀，用于表达 `any`、`tool`、`none` 的意图。
  - `anthropic-strict` 模式只过滤工具，不注入 steering prompt，避免影响官方形态检测。
- `src/anthropic/payload_guard.rs`
  - 增加 `PayloadByteBreakdown`，记录总字节、历史字节、当前消息字节、当前 tools/tool_results/images 字节、最大工具定义字节、历史 tool_use/tool_result 数量等。
  - payload guard 仍会裁剪旧历史并修复工具配对，但如果裁剪后仍超出 `payloadGuardMaxBytes`，不再用本地 `Oversized` 拒绝请求，而是标记 `still_oversized=true` 后继续发送给 Kiro，让 Kiro 返回真实上游错误。
  - `payloadGuardMaxBytes = 0` 仍表示关闭 size limit，只保留配对修复等非 size 相关处理。
- `src/anthropic/handlers.rs`
  - `/v1/messages` 和 `/cc/v1/messages` 都会记录 payload breakdown。
  - 当 payload guard 修改了请求、payload 接近上限或仍超限时，会输出更详细日志，便于定位到底是历史、tools、tool_result、图片/PDF 文档还是当前消息导致过大。

本次明确没有实现“当前 user message、tool_result、PDF、图片过大时直接本地拒绝”。这些场景应交给 Kiro 上游返回错误，代理层只做历史裁剪、配对修复和可观测性增强。

## 3. 关键边界

### 3.1 签名不能伪造

Anthropic extended thinking 的 `signature` 是不透明签名字段，用于证明 thinking block 由 Claude 生成。代理层不能生成官方有效签名，也不能修改签名内容。

代理层可以做的只有：

- 原样透传上游返回的 signature。
- 原样保存用户下一轮请求带回的 signed thinking block。
- 原样回放 `redacted_thinking.data`。
- 保证流式事件顺序与官方一致，例如 `thinking_delta`、`signature_delta`、`content_block_stop`。

代理层不能做：

- 从普通文本 `<thinking>...</thinking>` 中合成一个带签名的官方 thinking block。
- 自己生成 `signature`。
- 修改 `signature`、`redacted_thinking.data` 或 signed thinking block 的顺序。

如果 Kiro 上游没有返回真实 signature，那么本服务最多只能做到“不破坏已有签名”和“官方形态兼容”，不能凭空达到官方加密签名验证。

官方参考：

- Anthropic extended thinking: https://platform.claude.com/docs/en/build-with-claude/extended-thinking

### 3.2 “真实使用不报错”和“官方协议严格保真”存在冲突

当前服务为了让 Kiro 上游更容易接受请求，会做一些兼容修复：

- 修复或删除孤立的 `tool_use`。
- 修复或删除孤立的 `tool_result`。
- 把 orphan tool result 转成普通文本。
- 给历史中出现但当前 tools 缺失的工具创建 placeholder。
- 删除 Kiro 上游不接受的 JSON Schema 字段，例如 `additionalProperties`。
- 注入 Write/Edit 分块策略或工具描述后缀，降低工具调用过大导致失败的概率。

这些行为对生产可用性有帮助，但对官方检测不利，因为它们会改变 Anthropic transcript 的真实形态。

因此需要把数据结构和目标拆开：

```text
AnthropicTranscript：面对下游客户端/检测网站，保存官方原始语义和签名信息。
KiroRequest：面对 Kiro 上游，可以做必要兼容转换和修复。
```

只有这样才能同时追求：

- 真实使用尽量不报错。
- 官方特性不被代理层破坏。

## 4. 当前代码现状

### 4.1 请求类型已包含 tools 和 tool_choice；基础兼容已实现

当前请求结构位于 `src/anthropic/types.rs`：

```rust
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: i32,
    pub messages: Vec<Message>,
    pub stream: bool,
    pub system: Option<Vec<SystemMessage>>,
    pub tools: Option<Vec<Tool>>,
    pub tool_choice: Option<serde_json::Value>,
    pub thinking: Option<Thinking>,
    pub output_config: Option<OutputConfig>,
    pub metadata: Option<Metadata>,
}
```

历史问题是：`tool_choice` 只是被反序列化，没有在 `convert_request_with_model_id()` 中真正转成 Kiro 行为。这会导致以下官方语义失效：

```json
{"type": "auto"}
{"type": "any"}
{"type": "tool", "name": "some_tool"}
{"type": "none"}
```

截至 2026-05-31，当前代码已经实现基础兼容：

- `auto`：保留全部工具，不额外约束。
- `any`：保留全部工具；非 strict 模式增加轻量 steering，提示本轮应使用至少一个工具。
- `none`：当前请求不发送 tools；非 strict 模式增加轻量 steering，提示本轮不要调用工具。
- `tool{name}`：当前请求只发送指定工具；非 strict 模式增加轻量 steering，提示本轮使用该工具。

仍需注意：

- Kiro 上游如果没有原生 `tool_choice`，`any` 和强制工具只能通过工具列表过滤和 prompt steering 近似表达，不能保证与 Anthropic 官方 constrained behavior 完全等价。
- 如果历史消息里出现过某个工具，但当前请求没有携带该工具定义，兼容模式仍可能为历史 tool_use 创建 placeholder，避免 Kiro 因历史不完整报错。
- structured output 若依赖官方严格工具调用和 strict schema，还需要后续 schema 双轨与结构化输出阶段继续补齐。

### 4.2 Tool 类型缺少部分官方字段

当前 `Tool` 类型主要包含：

```rust
pub struct Tool {
    pub tool_type: Option<String>,
    pub name: String,
    pub description: String,
    pub input_schema: HashMap<String, serde_json::Value>,
    pub max_uses: Option<i32>,
    pub cache_control: Option<serde_json::Value>,
}
```

问题：

- 没有显式建模 `strict`。
- 未知官方扩展字段会被 serde 忽略。
- strict tools、structured output 检测所需字段可能在进入转换层前就丢失。

### 4.3 JSON Schema 被 Kiro 兼容化，可能破坏官方 strict schema

`src/anthropic/converter.rs` 中的 `normalize_json_schema()` 会递归删除 `additionalProperties` 并清理 `required`：

```rust
fn normalize_schema_object(obj: &mut serde_json::Map<String, serde_json::Value>, is_root: bool) {
    obj.remove("additionalProperties");
    ...
}
```

目的：

- 修复 MCP 工具定义中常见的异常 schema。
- 避免 Kiro 上游返回 `400 Improperly formed request`。

副作用：

- 官方 strict schema 通常依赖 `additionalProperties: false`。
- 如果只保留一份被 Kiro 改写后的 schema，官方 structured output/tool strict 检测会失败。

结论：

```text
schema 需要双轨：
1. original_input_schema：官方语义、校验、检测使用。
2. kiro_input_schema：发送 Kiro 上游使用，可做兼容清理。
```

### 4.4 tool_result 内容处理过窄

当前 `process_message_content()` 会处理 `tool_result`，但 `extract_tool_result_content()` 主要抽取文本：

```rust
fn extract_tool_result_content(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
            parts.join("\n")
        }
        Some(v) => v.to_string(),
        None => String::new(),
    }
}
```

问题：

- `tool_result.content` 如果是 text block 数组，可以部分保留。
- 如果包含 image/document 等非 text block，当前容易被忽略。
- 如果工具结果是结构化 JSON，可能被转成字符串而失去官方 block 语义。

影响：

- 文本工具结果大多可用。
- 多模态工具结果、文档工具结果、PDF 工具结果不可靠。
- 检测网站如果让工具返回图片/PDF/复杂 JSON，再让模型继续处理，可能失败。

### 4.5 非流式工具输入 JSON 解析失败会被替换为 `{}`

非流式响应聚合工具调用时，当前逻辑位于 `src/anthropic/handlers.rs`：

```rust
let input: serde_json::Value = if buffer.is_empty() {
    serde_json::json!({})
} else {
    serde_json::from_str(buffer).unwrap_or_else(|e| {
        tracing::warn!("工具输入 JSON 解析失败: {}, tool_use_id: {}", e, tool_use.tool_use_id);
        serde_json::json!({})
    })
};
```

问题：

- JSON 解析失败后直接变成 `{}`。
- 客户端可能拿到错误工具参数并执行。

这比直接报错更危险。例如模型原本想调用：

```json
{"path": "/tmp/a.txt"}
```

解析失败后变成：

```json
{}
```

客户端执行工具时仍会失败，甚至可能执行错误默认行为。

更合理的策略：

- 尝试轻量修复明显可修复的 JSON。
- 不能修复时返回规范错误。
- 不要把无效工具输入伪造成空对象。

### 4.6 工具配对当前依赖兼容修复

转换阶段 `validate_tool_pairing()` 会检查工具配对：

- 收集历史 assistant 的 `tool_use_id`。
- 收集历史 user 的 `tool_result`。
- 判断当前 user 的 `tool_result` 是否对应未配对 tool_use。
- 在兼容模式下移除孤立 tool_use，或把孤立 tool_result 转为普通文本。

payload guard 阶段还会再次修复：

- 删除空 tool_uses。
- 修复历史中的 orphan tool_result。
- 修复当前消息中的 orphan tool_result。
- 移除没有结果的 tool_use。

这些修复减少上游 Kiro 400，但会改变 transcript。签名过检时，如果上一轮 assistant 的 signed thinking + tool_use 被修复或重排，后续检测就可能失败。

### 4.7 assistant 历史中的 thinking signature 当前会丢失

`convert_assistant_message()` 当前会把 assistant content 中的 thinking 拼进普通文本：

```rust
"<thinking>{}</thinking>\n\n{}"
```

并且代码中已有日志说明：

```text
当前 Kiro history 模型不支持 Anthropic thinking signature；仅透传 thinking 文本
当前 Kiro history 模型不支持 redacted_thinking；已跳过该历史块
```

问题：

- `thinking.signature` 不保留。
- `redacted_thinking.data` 不保留。
- signed thinking block 在工具多轮请求中不能 round-trip。

这就是“模型签名验证部分合格”的核心原因之一。

## 5. 官方工具链路要求

官方工具调用链路应保持以下形态：

1. 请求中声明 tools。
2. assistant 可以返回一个或多个 `tool_use` content block。
3. 每个 `tool_use` 必须有稳定 `id`。
4. 客户端执行工具后，下一条 user 消息中带 `tool_result`。
5. `tool_result.tool_use_id` 必须对应上一轮 assistant 的 `tool_use.id`。
6. assistant 最终继续回答。

典型形态：

```json
{
  "role": "assistant",
  "content": [
    {
      "type": "tool_use",
      "id": "toolu_01...",
      "name": "read_file",
      "input": {"path": "/tmp/a.txt"}
    }
  ]
}
```

下一轮：

```json
{
  "role": "user",
  "content": [
    {
      "type": "tool_result",
      "tool_use_id": "toolu_01...",
      "content": "file content"
    }
  ]
}
```

官方参考：

- Anthropic tool use: https://platform.claude.com/docs/en/agents-and-tools/tool-use/how-tool-use-works

## 6. 第一阶段目标：工具支持，使用不报错

第一阶段目标：

```text
真实工具调用多轮对话稳定，不再频繁触发 Kiro 400 或客户端执行错误工具参数。
```

这阶段暂不追求全部官方检测过关，但要为后续签名保真打基础。

### 6.1 补完整工具请求建模

建议新增明确的 `ToolChoice` 类型：

```rust
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
    None,
}
```

同时保留原始 JSON，以兼容未来扩展：

```rust
pub tool_choice: Option<ToolChoice>,
pub raw_tool_choice: Option<serde_json::Value>,
```

`Tool` 类型建议增加：

```rust
pub strict: Option<bool>,
pub extra: serde_json::Map<String, serde_json::Value>,
```

目的：

- 不丢官方字段。
- structured output 和 strict tools 有基础。
- 后续检测字段扩展时不需要重构。

### 6.2 实现 tool_choice 转换策略

Kiro 上游如果没有原生 `tool_choice`，可以先做兼容策略：

| tool_choice | 兼容策略 |
|---|---|
| `auto` | 正常传 tools |
| `none` | 不向 Kiro 传 tools，避免模型调用工具 |
| `any` | 传 tools，并在 Kiro-facing prompt 中提示必须调用至少一个工具 |
| `tool{name}` | 只传目标工具，并提示必须调用该工具 |

注意：

- 这不是 100% 官方 constrained behavior。
- 但可以先解决“工具使用不报错”和“检测期望工具调用但没有调用”的问题。
- 后续如果 Kiro 上游支持原生 tool_choice，应优先走原生能力。

### 6.3 schema 双轨

当前只生成 Kiro 兼容 schema，会破坏官方 strict schema。因此需要拆为：

```rust
struct ToolSchemaPair {
    original: serde_json::Value,
    kiro: serde_json::Value,
}
```

使用原则：

- `original`：用于官方响应、strict 校验、结构化输出检测。
- `kiro`：用于上游请求，允许删除 Kiro 不支持字段。

这样既能减少 Kiro 400，又不会让检测认为官方 schema 被代理改坏。

### 6.4 增强 tool_result 内容处理

`tool_result.content` 应至少支持：

- string
- `{"type": "text", "text": "..."}`
- image block
- document block
- arbitrary JSON fallback

转换策略：

| 输入内容 | Kiro 转换建议 |
|---|---|
| string | 原样作为文本 |
| text block | 拼接文本 |
| image block | 转 Kiro image，如果 Kiro tool_result 不支持 image，则加入当前消息 images 或文本说明 |
| document block | 走 document/PDF 转换逻辑 |
| JSON object | 保留 JSON 字符串，避免丢字段 |
| 不支持内容 | 不静默丢弃，返回明确降级说明或 400 |

### 6.5 工具配对状态机前置校验

发送 Kiro 之前，应构建工具配对状态机，明确判断：

- 当前 user 的 `tool_result` 是否对应最近未完成的 assistant `tool_use`。
- 是否存在重复 `tool_result`。
- 是否存在 orphan `tool_result`。
- 是否存在 orphan `tool_use`。
- 当前 tools 是否包含历史 tool_use 中出现过的工具名。
- 多工具调用是否全部有结果。

兼容模式策略：

- 能修复就修复。
- 不能修复则返回清晰本地 400，避免把问题交给 Kiro 返回模糊 `Improperly formed request`。

官方模式策略：

- 不静默修复 transcript。
- 不合规直接按官方语义报错。

### 6.6 非流式工具输入不能静默变成 `{}`

建议替换当前行为：

```text
JSON parse fail -> {}
```

为：

```text
JSON parse fail
-> 尝试轻量修复
-> 修复成功则使用修复后 JSON
-> 修复失败则返回规范错误，不生成错误 tool_use
```

理由：

- 空对象可能让客户端执行错误工具。
- 对用户而言，明确错误比错误工具调用更安全。
- 对检测而言，伪造 `{}` 也不符合官方表现。

### 6.7 流式和非流式工具输出统一

流式应保证事件顺序：

```text
message_start
content_block_start(tool_use)
content_block_delta(input_json_delta)
content_block_stop
message_delta(stop_reason=tool_use)
message_stop
```

非流式应保证：

```json
{
  "type": "tool_use",
  "id": "...",
  "name": "...",
  "input": {}
}
```

其中 `input` 必须是合法 JSON object，不能是无效 JSON，不能因为解析失败被伪造成 `{}`。

### 6.8 工具名映射要保持可追踪

当前 Kiro 上游对工具名有限制，因此代码会 sanitize/shorten tool name，并在响应里 reverse mapping。

这条思路是对的，但需要确保：

- request tools 中长名称被映射。
- assistant history 中 tool_use 名称被同样映射。
- stream/non-stream 响应中都恢复原始工具名。
- 下一轮 tool_result 按原始 tool_use id 配对，不受名称映射影响。
- 如果当前 tools 缺失历史工具定义，兼容模式可 placeholder，官方模式应报错。

## 7. 第二阶段目标：签名过检

工具链路稳定后，再处理签名。

第二阶段目标：

```text
不生成假签名；不丢上游真实签名；signed thinking/redacted thinking 在工具多轮中可 round-trip。
```

### 7.1 保留 assistant 原始 content blocks

当前 `convert_assistant_message()` 把 thinking 转成文本，这是签名丢失根因。

需要新增官方 transcript 层，保留 assistant 原始 content：

```json
[
  {
    "type": "thinking",
    "thinking": "...",
    "signature": "..."
  },
  {
    "type": "tool_use",
    "id": "toolu_...",
    "name": "...",
    "input": {}
  }
]
```

KiroRequest 层可以继续把部分内容转换成 Kiro history，但 AnthropicTranscript 层不能丢 signature。

### 7.2 redacted_thinking 原样保留

当前 redacted thinking 被跳过。应改为：

- 下游 transcript 原样保留。
- 下一轮用户请求带回时接受并保存。
- 不解析 `data`。
- 不修改 `data`。

### 7.3 流式 signature_delta 顺序严格化

如果上游返回 native reasoning signature，流式输出必须保证：

```text
content_block_start(thinking)
content_block_delta(thinking_delta)
content_block_delta(signature_delta)
content_block_stop
```

如果上游没有 signature：

- 不能自己生成。
- 不要输出 fake `signature_delta`。

### 7.4 禁用合成 thinking 的官方伪装

在 official fidelity 模式下，应禁用：

- XML thinking prefix 注入。
- 从 `<thinking>...</thinking>` 文本提取成官方 thinking block。
- 给合成 thinking 添加 signature。

合成 thinking 可以在生产兼容模式下作为用户体验功能存在，但不能用于官方过检模式。

## 8. 建议的模式设计

当前已有：

```json
"compatProfile": "claude-code"
"compatProfile": "anthropic-strict"
"compatProfile": "debug"
```

建议后续明确区分：

### 8.1 `claude-code`

目标：

- 真实 Claude Code/Kiro 使用优先。
- 尽量修复请求，减少上游 400。
- 可注入 Kiro 需要的提示/策略。

允许：

- 修复 orphan tool_result。
- 移除 orphan tool_use。
- placeholder tool。
- schema Kiro 兼容化。
- payload guard 修复。
- XML thinking 兼容提取。

### 8.2 `anthropic-official` 或增强后的 `anthropic-strict`

目标：

- 官方协议保真。
- 面向检测和协议一致性。
- 不静默改写 transcript。

禁止：

- 伪造 signature。
- 丢弃 signed thinking。
- 丢弃 redacted_thinking。
- 合成 official thinking。
- 静默丢弃/修复 tool transcript。
- 暴露代理 warning header。
- 注入工具描述后缀或 system prompt 策略。

允许：

- 在 KiroRequest 层做必要上游兼容转换，但不能改变下游官方 transcript。

## 9. 实施顺序

### 阶段 A：工具链路稳定

任务：

1. 建模 `ToolChoice`。
2. 建模 `tools[].strict` 和 unknown fields。
3. 实现 `tool_choice` 兼容策略。
4. schema 双轨。
5. 增强 `tool_result.content` 解析。
6. 引入工具配对状态机。
7. 非流式工具 JSON 解析失败不再返回 `{}`。
8. 加强 stream/non-stream 工具输出一致性测试。

验收：

- 普通工具调用不报错。
- 多工具调用不报错。
- 工具结果下一轮可继续对话。
- 非流式和流式输出都能让客户端正确执行工具。
- Kiro 400 明显减少。
- 出错时是本地清晰错误，不是上游模糊错误。

### 阶段 B：签名保真

任务：

1. 引入 AnthropicTranscript 或等价结构。
2. assistant history 原样保留 thinking/signature/redacted_thinking/tool_use 顺序。
3. 修复 `convert_assistant_message()` 丢 signature 问题。
4. 流式 `signature_delta` 顺序校验。
5. 禁止 official 模式下合成 signed thinking。
6. 加工具多轮 signature round-trip 测试。

验收：

- 单轮 native thinking signature 不丢。
- thinking + tool_use 后，下一轮 tool_result 请求不丢上一轮 signature。
- redacted_thinking 不丢。
- 没有 signature 时不伪造。
- official 模式下不会把 `<thinking>` 文本伪装成官方 signed thinking。

### 阶段 C：PDF 与结构化输出

工具和签名稳定后再做：

- PDF base64/url/file_id。
- Files API adapter。
- PDF 文本 + 页面图片处理。
- `output_config.format`。
- JSON Schema 校验和重试。
- strict tools。

这些能力和工具/签名有关，但不应抢在工具稳定之前做。

## 10. 验收测试清单

### 10.1 工具基础测试

- `tool_choice: none` 不调用工具。
- `tool_choice: auto` 可自由决定是否调用工具。
- `tool_choice: any` 必须返回至少一个 tool_use，或返回明确失败。
- `tool_choice: {"type":"tool","name":"x"}` 只调用工具 x。
- 单个 tool_use -> tool_result -> final answer。
- 多个 tool_use -> 多个 tool_result -> final answer。
- 工具名超长时，上游短名、下游原名。
- 历史中工具名缺失时，兼容模式 placeholder，official 模式报错。

### 10.2 工具结果测试

- string tool_result。
- text block array tool_result。
- JSON object tool_result。
- image tool_result。
- document/PDF tool_result。
- `is_error: true` tool_result。
- 重复 tool_result。
- orphan tool_result。
- orphan tool_use。

### 10.3 流式测试

- tool_use block start/delta/stop 顺序正确。
- `stop_reason = "tool_use"`。
- 多 tool_use index 不冲突。
- thinking block 在 tool_use 前正确关闭。
- 工具 JSON partial delta 拼接后是合法 object。

### 10.4 非流式测试

- tool_use input 是合法 object。
- input JSON parse fail 不返回假 `{}`。
- 多 tool_use 能全部返回。
- native thinking + tool_use content 顺序正确。

### 10.5 签名测试

- native thinking response 带 signature。
- stream 输出 `signature_delta`。
- signed thinking + tool_use 后，下一轮 tool_result 不丢 signature。
- redacted_thinking round-trip。
- 修改 signature 后不能被代理伪装成成功。
- 没有 signature 时不生成 fake signature。

## 11. 风险

### 11.1 Kiro 上游能力限制

如果 Kiro 上游没有官方 constrained decoding、原生 tool_choice、原生 signed thinking history 支持，代理层不能保证与 Anthropic 官方完全等价。

可实现的是：

- 尽量官方形态兼容。
- 本地校验和清晰错误。
- 原样透传已有签名。
- 避免代理破坏签名。

不能实现的是：

- 生成官方有效加密 signature。
- 在上游不支持时实现完全等价 constrained decoding。

### 11.2 兼容修复可能掩盖真实客户端错误

生产模式修复 orphan tools 可以减少报错，但也可能让客户端看不到自己历史构造错误。

建议：

- 生产模式继续修复，但记录 metrics。
- official 模式拒绝修复。
- admin 后台展示工具修复次数和类型。

### 11.3 payload guard 可能影响工具和签名

payload guard 会裁剪历史并修复工具配对。它有助于减少超大请求导致的 Kiro 400，但可能影响签名 round-trip。

建议：

- official 模式下要谨慎裁剪 signed thinking/tool_use 附近的历史。
- 如果必须裁剪，优先按完整消息轮次裁剪，不能裁剪半个 tool loop。
- 对 signed thinking + tool_use + tool_result 三元组应作为原子窗口处理。

## 12. 建议落地结论

短期应先做工具稳定，不应先追签名检测。

原因：

1. 工具调用是签名 round-trip 的前置路径。
2. `tool_choice` 基础兼容已实现，但受 Kiro 上游原生能力限制，仍不能保证与 Anthropic 官方 constrained behavior 完全等价。
3. 当前 schema 只有 Kiro 兼容形态，无法支持官方 strict schema。
4. 当前 tool_result 内容处理过窄，会导致真实工具结果丢失。
5. 当前非流式 JSON parse fail 返回 `{}`，可能导致错误工具执行。

推荐第一批改动及状态：

```text
1. ToolChoice 建模与转换。（已完成基础兼容）
2. Tool 增加 strict/extra 字段。
3. schema 双轨。
4. tool_result 内容增强。
5. 工具配对状态机。
6. 非流式工具 JSON 解析失败处理。
7. 工具链路回归测试。（已补充 tool_choice 与 payload breakdown 单测）
```

完成后，再做：

```text
1. AnthropicTranscript 官方保真层。
2. thinking signature/redacted_thinking 原样保留。
3. signed thinking + tool_use + tool_result round-trip。
4. official 模式下禁止合成 thinking/signature。
```

最终目标不是“伪装成官方”，而是：

```text
真实支持的能力尽量官方兼容；
上游提供的签名完整保真；
不能真实支持的能力明确降级或规范报错；
不通过伪造签名、伪造工具结果、静默吞字段来过检。
```

## 13. 本轮验证记录

验证日期：2026-05-31

本轮已完成的本地验证：

- `cargo fmt --check` 通过。
- `CC=/usr/bin/cc CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/cc cargo test --locked --no-default-features` 通过，结果为 `394 passed`。
- `cargo build --locked --no-default-features` 通过。
- 本地服务使用新二进制重启，监听端口保持 `127.0.0.1:9022`。
- `GET /healthz` 返回 `{"service":"kiro-rs","status":"ok"}`。
- `GET /cc/v1/models` 使用本地配置 API key 请求成功，返回模型列表。
- `ccman cc` 已切换到 `local-kiro-rs-9022`，URL 为 `http://127.0.0.1:9022/cc`。
- Claude Code CLI 非交互请求已经命中本地 `/cc/v1/messages`，日志显示：
  - 模型 `claude-sonnet-4-6` 映射为 Kiro 上游 `claude-sonnet-4.6`。
  - Claude Code 默认工具定义进入转换链路。
  - 工具名称映射完成，超长工具名被缩短并保留 reverse mapping。

当前未能完成真实上游生成验收，原因不是本地转换报错，而是 Kiro 上游对当前全部 5 个凭据返回 `429 Too Many Requests`，提示 suspicious activity temporary limits。服务最终返回：

```text
API Error: Request rejected (429) · Upstream temporarily rate limited.
```

这说明本地服务、`ccman` 切换、Claude Code CLI 到 `/cc` 路由、模型映射和工具转换链路已经打通；剩余生成验收需要等 Kiro 凭据冷却结束或换一组可用凭据后再跑。
