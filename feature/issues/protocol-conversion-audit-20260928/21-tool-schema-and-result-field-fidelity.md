# P21 工具定义与工具结果字段保真度：isError、描述截断、schema 清洗、非 object input、document content 源

Status: partially-fixed-in-a6abce7 (description truncation order; others open)
Severity: Low
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

本文合并 5 个低风险保真度问题。每项都标注“已知上游约束 / 有意降级 / 未验证”，避免把有意的兼容处理误判为 bug。

| # | 行为 | 位置 | 定性 |
| --- | --- | --- | --- |
| 1 | Kiro toolResult 在错误时额外序列化 `isError: true` | `src/kiro/model/requests/tool.rs:71-73`、`src/kiro/model/requests/tool.rs:98-111` | 未验证字段；生产长期使用，经验上被容忍 |
| 2 | 工具描述静默截断到 10,000 字符，且截在 Write/Edit 后缀之后 | `src/anthropic/converter/tools.rs:523-542` | 已知上游有长度上限（数值未验证）；截断顺序是 bug |
| 3 | schema 清洗全局删除 `additionalProperties`；根级 `oneOf/anyOf` 展平并对 required 取交集 | `src/anthropic/converter/schema.rs:28-32`、`src/anthropic/converter/schema.rs:147-157` | 前者是社区/sub2api 已知 Kiro 约束；后者对齐官方“根级不支持组合子”约束 |
| 4 | 非 object 的 `tool_use.input` 被包成 `{"value": ...}` | `src/anthropic/converter/content.rs:662-668`，调用点 `src/anthropic/converter/history.rs:336-338` | 代理宽容处理；官方会 400 |
| 5 | `document.source.type="content"` 返回 400；`title`/`context` 被丢弃 | `src/anthropic/converter/content.rs:223-226`、`src/anthropic/types.rs:413-438` | 功能缺口 |

逐项影响：

1. `isError`：`ToolResult::error` 同时设置 `status:"error"` 和 `is_error:true`，后者经 `#[serde(default, skip_serializing_if = "is_false")]`
   在错误时输出 `"isError":true`；`process_message_content` 又再次覆盖 `status`（`src/anthropic/converter/content.rs:64-72`）。
   Kiro 已知形状只有 `toolUseId/status/content`（`sub2api-kiro` 的 `KiroToolResult` 只有这三个字段）。该字段从首个提交
   （`ab78f3c`，2025-12-27）就存在，Claude Code 工具失败（`is_error:true`）是高频场景，生产未见因此 400，说明上游大概率忽略未知字段。
   风险是未来上游收紧 schema 校验时成为一类无法从文案定位的 400。
2. 描述截断：`description.char_indices().nth(10000)` 按字符截断，没有省略标记，没有 warning 计数。
   - 截断发生在追加 `WRITE_TOOL_DESCRIPTION_SUFFIX` / `EDIT_TOOL_DESCRIPTION_SUFFIX` 之后（`src/anthropic/converter/tools.rs:523-536`），
     长描述会先把代理自己注入的分块写入策略截掉，策略静默失效。`sub2api-kiro` 的实现保留后缀、截断正文并加
     `... (description truncated)` 标记（`../sub2api-kiro/backend/internal/pkg/kiro/translator.go` 的 `truncateKiroToolDescription` 附近）。
   - 单位不一致：这里是 10,000 字符，`sub2api-kiro` 是 10,237 字节（`kiroMaxToolDescLen`）。中文描述 10,000 字符约 30,000 字节，
     如果上游按字节计，会超限。上游真实上限和单位未验证。
   - 另有 payload guard 的工具定义压缩（`tool_description_max_chars`，`src/anthropic/payload_guard.rs:2190-2216`），只在体积超限时触发，
     与这里的无条件截断是两层机制。
     （压缩调用在 `apply_payload_shaping` 内，即 `guard_kiro_request` 的超限分支 `src/anthropic/payload_guard.rs:582-584`。）
3. schema 清洗：
   - `normalize_schema_object` 在每一层删除 `additionalProperties`、`additionalItems`、`unevaluated*` 等（`src/anthropic/converter/schema.rs:147-157`）。
     `sub2api-kiro` 注释说明 Kiro Smithy 校验遇到 `additionalProperties` 会拒绝整个请求（社区一致经验，本仓库无抓包），
     本项目从 2026-05-29（`df60bef`）起全局删除且生产稳定。副作用：`{"type":"object","additionalProperties":{"type":"string"}}`
     这种“字典/映射”参数丢失值类型，模型只看到空 `properties` 的 object；`additionalProperties:false` 的严格约束也消失。
     这与 `docs/analysis/sub2api-kiro-protocol-interop-optimization-20260916.md` “不建议全局删除 additionalProperties”的建议相反，
     但该建议写于删除已上线之后，且没有给出“保留后上游接受”的证据，本文不据此判定为回归。
   - 根级组合子：`flatten_root_schema_combinators`（`src/anthropic/converter/schema.rs:28-32`）把 `allOf` required 取并集，
     `oneOf/anyOf` required 取交集；各分支 properties 以“先出现者为准”合并进根（`src/anthropic/converter/schema.rs:64-86`）。
     效果：“二选一必填”变成“都不必填”，同名属性在不同分支中类型不同时只保留第一个。已有测试固化此行为
     （`src/anthropic/converter.rs` 中 `test_normalize_json_schema_flattens_root_*`）。
     Anthropic 官方对根级 `oneOf/anyOf/allOf` 直接 400（见下节），所以代理展平是比官方更宽容的降级，不是偏离。
     嵌套层的组合子保留并递归清洗（`src/anthropic/converter/schema.rs:216-218`）。
4. 非 object input：`Null -> {}`、字符串/数字/数组 -> `{"value": x}`。历史 tool_use 以这个形态回放，模型看到的调用参数与工具 schema 不符，
   可能在后续轮次模仿 `{"value": ...}` 形态。payload guard 有对应诊断计数 `non_object_tool_use_inputs`
   （`src/anthropic/payload_guard.rs:90`）。正常 Claude Code 不会产生非 object input，影响面很小。
5. document：`ImageSource` 只有 `type/media_type/data/url/file_id`（`src/anthropic/types.rs:442-453`），`convert_document_source_to_text`
   对 `content` 源落到 `other =>` 分支，返回 `unsupported document source type: content`（`src/anthropic/converter/content.rs:223-226`），HTTP 400。
   `ContentBlock` 没有 `title`、`context`、`citations` 字段（`src/anthropic/types.rs:413-438`），顶层 document 的标题和上下文说明被丢弃，
   `<document media_type="...">` 包装里只有 media_type（`src/anthropic/converter/content.rs:264-269`）。

> 2026-09-29 代码核对（HEAD a4227c1）：本文引用的 `tool.rs`、`tools.rs`、`schema.rs`、`content.rs`、`history.rs`、`types.rs`、`converter.rs`、`payload_guard.rs` 行号在 HEAD 上全部核对一致，5 项均仍存在。3a1306d 没有改动这些文件；它只让本地 body 准备阶段的转换拒绝（例如 `unsupported document source type: content`，公开文案即 `ConversionError::UnsupportedContent` 原文，见 `src/anthropic/handlers/local_body_pipeline.rs:94-102`）在 usage 记录里额外带上 model、stream、max_tokens 和模型解析上下文，对外响应不变。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic 官方文档 / 官方错误文案，按已知内容，本次未重新请求）：

- `tool_result` 字段：`tool_use_id`、`content`、`is_error`。`is_error` 是 Anthropic 字段，Kiro 侧对应 `status`。
- `tool_use.input` 必须是 object；非 object 会被官方拒绝（`input` 需为字典）。
- 工具 `input_schema` 根级不支持 `oneOf`/`anyOf`/`allOf`，官方返回 400
  （`input_schema does not support oneOf, allOf, or anyOf at the top level`）；`additionalProperties` 在官方是合法关键字。
- 官方对工具描述没有公开的硬性字符上限（经验上很长的描述也被接受）。
- `document` 支持 `source.type = base64 | text | content | url | file`；`content` 源内含 text/image 块，用于自定义分块引用；
  `title`、`context` 是可选字段，`context` 不参与引用但对模型可见；`citations.enabled` 开启引用。

Kiro 侧：

- 已知约束（社区与 `sub2api-kiro`，本仓库无抓包）：schema 中 `additionalProperties`、`$schema`、`format` 等关键字会触发 400；
  工具描述有长度上限（`sub2api-kiro` 取 10,237 字节）。
- 已知形状：`toolResult` 仅 `toolUseId/status/content[{text}]`。`isError` 是否被忽略：未验证，生产行为表明被容忍。
- 未验证：Kiro 是否有任何 citations 能力。当前代理不产出 citations，本文不要求实现。

## 源码链与根因

```text
convert_tools (tools.rs:484)
  normalize_tool_description (tools.rs:400-411)
  + Write/Edit suffix (tools.rs:523-536)
  -> nth(10000) 截断 (tools.rs:538-542)          // 截在后缀之后、无标记、无计数
  -> input_schema_from_json -> normalize_json_schema (schema.rs:8-26)
       normalize_schema_object (schema.rs:147 起)  // 每层删 additionalProperties 等
       flatten_root_schema_combinators (schema.rs:28-32)

process_message_content (content.rs:17)
  "document" -> convert_document_source_to_text (content.rs:180-228)   // content 源 400，title/context 未反序列化
  "tool_result" -> ToolResult::success/error (tool.rs:82-111)          // error 时 is_error=true -> "isError":true

convert_assistant_message_with_known_tools (history.rs:244)
  "tool_use" -> normalize_tool_use_input (content.rs:662-668)          // 非 object 包 {"value":...}
```

根因分别是：Kiro 结构体直接复用了带 Anthropic 语义的 `is_error` 字段；截断实现早于后缀注入逻辑且未随之调整；
schema 清洗按“上游已知拒绝集合”做了全局删除而没有把语义降级记录下来；document 源类型和元数据字段没有进入类型定义。

## 复现

### 最小复现（单测）

`src/kiro/model/requests/tool.rs` 的 `mod tests`：

```rust
#[test]
fn tool_result_error_serializes_only_status() {
    let json = serde_json::to_string(&ToolResult::error("tool-1", "boom")).unwrap();
    assert!(json.contains("\"status\":\"error\""));
    assert!(!json.contains("isError")); // 当前实现失败
}
```

`src/anthropic/converter.rs` 的 `mod tests`（复用 `test_tool`）：

```rust
#[test]
fn overlong_tool_description_keeps_marker_and_is_counted() {
    let mut tool = test_tool("Bash");
    tool.description = "a".repeat(12_000);
    let tools = Some(vec![tool]);
    let converted = convert_tools(&tools, &None, &mut HashMap::new(), ConverterOptions::default())
        .expect("convert");
    let desc = &converted.tools[0].tool_specification.description;
    assert!(desc.chars().count() <= 10_000);
    assert!(desc.ends_with("(description truncated)")); // 当前实现失败：无标记
}

#[test]
fn map_type_parameter_keeps_value_type_hint_after_additional_properties_removed() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"env": {"type": "object", "additionalProperties": {"type": "string"}}}
    });
    let normalized = normalize_json_schema(schema);
    assert!(normalized["properties"]["env"].get("additionalProperties").is_none());
    // 期望降级为描述提示；当前实现失败：description 不存在
    assert!(normalized["properties"]["env"]["description"]
        .as_str()
        .is_some_and(|d| d.contains("string")));
}

#[test]
fn document_content_source_is_converted_and_title_kept() {
    let req_content = serde_json::json!([{
        "type": "document",
        "title": "Design Notes",
        "context": "internal",
        "source": {"type": "content", "content": [{"type": "text", "text": "chunk one"}]}
    }]);
    // 当前实现失败：Err(unsupported document source type: content)
    let (text, _, _) = content::process_message_content(&req_content).expect("convert");
    assert!(text.contains("chunk one") && text.contains("Design Notes"));
}
```

Write 后缀被截断的复现需要开启 `promptSteering.chunkedWrite.toolDescriptionEnabled`（`src/anthropic/converter.rs:160-167`），
构造 `Write` 工具描述 10,000 字符，断言结果仍包含 `WRITE_TOOL_DESCRIPTION_SUFFIX` 的前 30 个字符。

### 端到端复现

```bash
curl -sS http://127.0.0.1:PORT/v1/messages -H 'x-api-key: <KEY>' -H 'anthropic-version: 2023-06-01' \
  -H 'content-type: application/json' -d '{
  "model":"claude-sonnet-4-5","max_tokens":128,
  "messages":[{"role":"user","content":[
    {"type":"document","title":"Spec","source":{"type":"content","content":[{"type":"text","text":"The limit is 42."}]}},
    {"type":"text","text":"What is the limit?"}]}]}'
# 现状：400 invalid_request_error "unsupported document source type: content"
```

`isError`：真实 Claude Code 执行一个必然失败的 Bash 命令（如 `ls /nonexistent`），开启 tool format debug 请求体采样，
确认 toolResult 含 `"isError":true` 且上游 200。这一步可作为“上游容忍 isError”的正式证据补进本文。

## 修复方案

### 候选方案

1. `isError`：A 从 Kiro `ToolResult` 移除 `is_error` 字段，只保留 `status`；B 保留字段但 `#[serde(skip_serializing)]`。
2. 描述截断：A 先截正文再追加后缀，加 `... (description truncated)` 标记并计入 `ProxyWarnings`；B 改为字节上限（对齐 sub2api）。
3. schema：A 删除 `additionalProperties` 时，若其值是 schema 对象，把值类型写入 `description` 提示（例如
   `Map of string keys to string values.`）；`additionalProperties:false` 时追加 `No extra properties allowed.`；B 维持现状只记录 normalization diff。
4. 非 object input：A 维持包装但记 warning；B 严格模式返回 400（与官方一致）。
5. document：A 支持 `content` 源（拼接其中 text 块，image 块提升到 images），增加 `title`/`context` 字段并渲染到 `<document>` 属性；
   B 仅把 400 文案改清楚。

### 推荐方案

- 1B：`#[serde(default, skip_serializing)]` 保留反序列化兼容（Kiro 响应若带该字段不受影响），发送时只用 `status`，改动最小，
  并与 `sub2api-kiro` 形状一致。`process_message_content` 中重复设置 `status` 的两行可删除。
- 2A：

```rust
const KIRO_TOOL_DESCRIPTION_MAX_CHARS: usize = 10_000;
const TRUNCATION_MARKER: &str = "... (description truncated)";
let budget = KIRO_TOOL_DESCRIPTION_MAX_CHARS
    .saturating_sub(if suffix.is_empty() { 0 } else { suffix.chars().count() + 1 });
if description.chars().count() > budget {
    description = truncate_chars(&description, budget.saturating_sub(TRUNCATION_MARKER.chars().count()));
    description.push_str(TRUNCATION_MARKER);
    warnings.truncated_tool_descriptions += 1;
}
if !suffix.is_empty() { description.push('\n'); description.push_str(suffix); }
```

  上限单位（字符还是字节）先不改，等真实上游用中文长描述补证后再定；`convert_tools` 需要把 warning 计数带回
  `ConversionResult.warnings`。
- 3A（仅 `additionalProperties` 为 schema 对象或 `false` 时追加提示；为 `true` 时直接删除即可）。根级组合子维持现状，
  另在 `ProxyWarnings` 增加 `flattened_root_schema_combinators` 计数，方便观测。
- 4A：在 `normalize_tool_use_input` 包装时计入 warning；strict profile 下改为返回 `ConversionError`（strict 已对其他配对问题采取拒绝策略）。
- 5A：`ImageSource` 增加 `content: Option<Vec<Value>>`；`ContentBlock` 增加 `title`、`context`；`format_document_text` 改为接收可选 title/context，
  输出 `<document media_type="..." title="..." context="...">`（属性值做 XML 转义）。与 [P04](04-tool-result-document-and-media-blocks-stringified.md)
  的嵌套 document 渲染共用同一函数。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 上述单测转绿；`test_tool_result_serialize`、`test_normalize_json_schema_flattens_root_*`、现有 additionalProperties 删除测试保持通过
  （3A 只新增 description，不恢复 additionalProperties）。
- 描述截断：普通工具 < 10,000 字符 byte-identical；Write/Edit 超长时后缀完整保留。
- 真实上游补证（低风险、单请求）：
  - 失败工具结果去掉 `isError` 后仍 200，且模型能识别错误；
  - 10,000 个中文字符的工具描述是否被接受，用于确定字符/字节口径；
  - 带 `additionalProperties` 描述提示的 schema 被接受。
- Claude Code 真实会话：MCP 工具（常见 `additionalProperties`、`format`）正常调用，参数类型正确。

## 兼容性与风险

- 去掉 `isError` 是发送字段收缩，理论上最安全；若上游某些版本实际依赖 `isError` 判错（无证据），模型对错误结果的理解可能变化，需要真实请求确认。
- 描述截断口径调整会改变工具定义文本，导致 prompt cache 前缀失效一次。
- schema 描述提示增加少量 token；只在命中字典类型参数时出现。
- document `content` 源从 400 变为可用，属于能力扩展；`title/context` 进入 prompt 会改变包含 document 的请求文本。
- strict profile 下非 object input 改为拒绝，可能影响依赖宽容行为的非官方客户端，因此只在 strict 生效。

## 修复结果与验证（2026-09-29）

- 修复（`a6abce7`）：先把客户端的工具描述截断到 10000 字符，再追加代理自己的 Write/Edit 策略后缀，这样截断不会再把策略后缀截掉。其余几项（`isError`、`additionalProperties`、非对象 input）维持原结论。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。
