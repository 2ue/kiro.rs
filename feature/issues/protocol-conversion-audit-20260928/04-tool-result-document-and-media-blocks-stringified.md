# P04 tool_result 内的 document / search_result 等块被整体 JSON 字符串化，嵌套 url/file 图片未物化导致 400

Status: fixed-in-a6abce7 (not released)
Severity: High
Area: request
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

> 2026-09-29 代码核对（HEAD a4227c1）：3a1306d 未改动 converter、`body_processing.rs`、`files.rs`，问题仍存在。所引 `content.rs`、`body_processing.rs`、`files.rs`、`handlers.rs:5691-5704` 行号与 HEAD 一致，仅微调 `types.rs` 与 `config.rs` 两处范围。3a1306d 新增的本地转换回退（`local_body_pipeline.rs:186-215`）只对"多 native reasoning 块"两类 `UnsupportedContent` 生效，嵌套 url/file 图片的 `not materialized before conversion` 错误仍直接 400。

## 问题与影响

`extract_tool_result_content`（`src/anthropic/converter/content.rs:606-625`）把 `tool_result.content` 数组中
“不是 image、也没有顶层 `text` 字段”的元素直接 `item.to_string()`（`src/anthropic/converter/content.rs:616-618`），
作为 Kiro `toolResult.content[0].text` 发出。结果：

- `document`（`source.type=base64` 的 PDF）：整个 `{"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"JVBERi0..."}}`
  变成文本。模型看到的是几十万到上百万字符的 base64，而不是 PDF 文字。
- `document`（`source.type=text`）：文本藏在 `source.data`，同样被整块 JSON 化（带转义），而不是像顶层那样包成 `<document>`。
- `search_result`：`{"type":"search_result","source":"https://...","title":"...","content":[{"type":"text","text":"..."}]}`
  整块 JSON 化，引用/来源结构丢失，只剩一串 JSON。
- 其他未知块（例如 Claude Code tool search 返回的 `tool_reference`，经验推断）也会原样 JSON 化。

连带影响：

- 体积：base64 全是 ASCII，weighted = 字节数。一个约 1 MB 的 PDF 编码后约 1.33M 字符，单个 tool_result 就超过 Kiro 默认
  weighted 阈值 `1,300,000`（`src/anthropic/payload_guard.rs:33`）。当前 tool_result 截断默认关闭（`src/model/config.rs:686`），
  `on_too_long` 重试只能删历史，删不动当前 tool_result，最终 400。进入历史后又被 8,000 字符 head/tail 截断
  （`src/model/config.rs:652-653`、`src/model/config.rs:4441-4443`），留下的是 base64 头尾，毫无信息量。
- 语义：模型可能尝试“解码 base64”或回答“文件内容无法读取”，工具循环质量显著下降。

第二个子问题：tool_result 内的 `image` 若 `source.type` 为 `url`（非 data URL）或 `file`/`file_id`，请求直接 400。

- 远程 URL 物化 `materialize_content_sources` / `remote_source_info`（`src/anthropic/body_processing.rs:553-601`）、
  远程计数 `count_remote_content_sources`（`src/anthropic/body_processing.rs:434-444`）、light 模式拒绝
  `reject_non_inline_sources_in_content`（`src/anthropic/body_processing.rs:307-350`）、Files API 物化
  `materialize_file_sources_in_content`（`src/anthropic/files.rs:422-469`）全部只遍历 `message.content` 的顶层块，
  只认 `type in {image, document}`，不进入 `tool_result.content`。
- converter 的 `extract_tool_result_images`（`src/anthropic/converter/content.rs:627-652`）对嵌套 image 调
  `convert_image_source`，`url` 非 data URL 分支返回 `remote image URL source was not materialized before conversion`
  （`src/anthropic/converter/content.rs:141-145`），`file|file_id` 返回 `image file source was not materialized before conversion`
  （`src/anthropic/converter/content.rs:147-149`），映射为 HTTP 400 `invalid_request_error`（`src/anthropic/handlers.rs:5691-5704`）。

对审计原结论的修正/补充：

- 审计写“tool_result images with source url/file ... → convert_image_source rejects → 400”：对 image 成立。
  但嵌套 `document` 的 url/file 源不会 400，因为它根本不走 `convert_document_source_to_text`，而是被字符串化，
  URL 或 file_id 以文本形式发给模型，同样是静默错误。
- 审计写“顶层 document 在 content.rs ~45-55 处理”：成立（`src/anthropic/converter/content.rs:45-55` →
  `convert_document_source_to_text` `src/anthropic/converter/content.rs:180-228`，PDF 走 `extract_text_from_pdf_bytes`）。
- 补充：顶层 `search_result` 块被整块静默丢弃。`ContentBlock.source` 类型是 `Option<ImageSource>`（结构体，
  `src/anthropic/types.rs:437`、`src/anthropic/types.rs:441-453`），`search_result.source` 是字符串，
  `serde_json::from_value::<ContentBlock>` 失败，`process_message_content` 的 `if let Ok(block)`（`src/anthropic/converter/content.rs:30`）
  直接跳过；即使解析成功也会落到 `_ => {}`（`src/anthropic/converter/content.rs:85`）。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic Messages API 文档，本次未重新抓取，按已知文档内容）：

- `tool_result.content` 可以是字符串，或由 `text`、`image`、`document`、`search_result` 块组成的数组。
- `document.source` 支持 `base64`（PDF）、`text`（纯文本）、`content`（自定义内容块）、`url`、`file`（Files API）。
- `search_result` 块字段：`source`（字符串）、`title`、`content`（text 块数组）、可选 `citations`。它是为 RAG 类工具返回结果设计的，
  官方模型会把它当作可引用的检索结果。
- 官方侧 PDF 会同时提供文本与页面图像给模型；代理只能做文本抽取，这是已接受的降级（顶层 document 已如此）。

Kiro 侧：

- 已知形状（本项目与 `sub2api-kiro` 实现一致）：`toolResult = { toolUseId, status, content: [{ text }] }`，只放文本。
  `sub2api-kiro` 对 tool_result 数组只取 `text`/`input_text` 和纯字符串元素，其余丢弃
  （`../sub2api-kiro/backend/internal/pkg/kiro/translator.go` 的 tool_result 分支），不做字符串化。
- 未验证：Kiro toolResult content 是否接受 `json` / `image` / `document` 子块。没有抓包证据，本方案不依赖它。
- 已验证（本项目现状）：Kiro user message 的 `images` 字段接受 base64 图片，tool_result 中的 base64 image 已被提升到该字段
  （`src/anthropic/converter/content.rs:60`），并在文本中放 `[image attached]` 占位。

经验推断（未抓包）：Claude Code 的 Read 工具读 PDF 时会在 tool_result 中返回 base64 `document` 块；MCP 工具可返回
`search_result` / 图片 URL。这些都是 Claude Code 日常高频路径，需要真实 CLI 抓包确认比例。

## 源码链与根因

```text
process_message_content (content.rs:17-94)
  "tool_result" (content.rs:56-76)
    -> extract_tool_result_images(&block.content)   // 只处理 type=image，嵌套 url/file => Err => 400
    -> extract_tool_result_content(&block.content)  // 其余元素 to_string()
    -> normalize_tool_result_content
    -> ToolResult::success/error(id, text)
```

```rust
// src/anthropic/converter/content.rs:606-625
fn extract_tool_result_content(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts = Vec::new();
            for item in arr {
                if item.get("type").and_then(|value| value.as_str()) == Some("image") {
                    parts.push(TOOL_RESULT_IMAGE_PLACEHOLDER.to_string());
                } else if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                } else if !item.is_null() {
                    parts.push(item.to_string());   // document / search_result / 未知块 => 整块 JSON
                }
            }
            parts.join("\n")
        }
        Some(v) => v.to_string(),
        None => String::new(),
    }
}
```

另外 `item.get("text")` 不看 `type`，任何带 `text` 字段的块都当 text 用，这是宽松但无害的行为。

根因：顶层块和嵌套块走了两套提取逻辑。顶层有完整的 image/document 处理和前置物化；嵌套 tool_result 只实现了 image
一种，其余用 `to_string()` 兜底，且所有前置物化器都不递归进入 `tool_result.content`。

## 复现

### 最小复现（单测）

放入 `src/anthropic/converter/content.rs` 的 `mod tests`（沿用该模块直接调用 `process_message_content` 的风格）：

```rust
#[test]
fn tool_result_text_document_is_extracted_not_stringified() {
    let content = json!([{
        "type": "tool_result",
        "tool_use_id": "toolu_doc",
        "content": [{
            "type": "document",
            "source": {"type": "text", "media_type": "text/plain", "data": "hello from document"}
        }]
    }]);
    let (_text, _images, results) = process_message_content(&content).expect("convert");
    let text = results[0].content[0]["text"].as_str().unwrap();
    // 当前实现失败：text 是 {"source":{...},"type":"document"} 的 JSON
    assert!(!text.contains("\"type\":\"document\""), "stringified: {text}");
    assert!(text.contains("hello from document"));
}

#[test]
fn tool_result_base64_pdf_does_not_leak_base64() {
    let data = BASE64_STANDARD.encode(b"%PDF-1.4\n1 0 obj\n(Hello PDF) Tj\n%%EOF");
    let content = json!([{
        "type": "tool_result", "tool_use_id": "toolu_pdf",
        "content": [{"type": "document",
                     "source": {"type": "base64", "media_type": "application/pdf", "data": data}}]
    }]);
    let (_t, _i, results) = process_message_content(&content).expect("convert");
    let text = results[0].content[0]["text"].as_str().unwrap();
    assert!(!text.contains(&data), "base64 leaked into tool result text");
}

#[test]
fn tool_result_search_result_is_rendered_as_text_with_source() {
    let content = json!([{
        "type": "tool_result", "tool_use_id": "toolu_sr",
        "content": [{"type": "search_result", "source": "https://example.com/a",
                     "title": "Doc A", "content": [{"type": "text", "text": "alpha body"}]}]
    }]);
    let (_t, _i, results) = process_message_content(&content).expect("convert");
    let text = results[0].content[0]["text"].as_str().unwrap();
    assert!(!text.starts_with('{'));
    assert!(text.contains("https://example.com/a") && text.contains("Doc A") && text.contains("alpha body"));
}

#[test]
fn top_level_search_result_is_not_silently_dropped() {
    let content = json!([{"type": "search_result", "source": "https://example.com/b",
                          "title": "Doc B", "content": [{"type": "text", "text": "beta"}]}]);
    let (text, _i, _r) = process_message_content(&content).expect("convert");
    assert!(text.contains("beta")); // 当前实现失败：text 为空
}
```

嵌套远程图片 400（放 `src/anthropic/body_processing.rs` 的 `mod tests`，复用 `spawn_test_media_server` / `local_test_client`）：
构造 `tool_result.content=[{"type":"image","source":{"type":"url","url": local_test_url(&server,"/image.png")}}]`，
断言 `count_remote_multimodal_sources == 1`（当前为 0），物化后 source 变为 base64。

### 端到端复现

```bash
# 1) PDF document 嵌套在 tool_result
PDF_B64=$(base64 < sample.pdf | tr -d '\n')
curl -sS http://127.0.0.1:PORT/v1/messages -H 'x-api-key: <KEY>' -H 'anthropic-version: 2023-06-01' \
  -H 'content-type: application/json' -d @- <<JSON
{"model":"claude-sonnet-4-5","max_tokens":256,
 "tools":[{"name":"Read","description":"read file","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}],
 "messages":[
  {"role":"user","content":"Summarize sample.pdf"},
  {"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"path":"sample.pdf"}}]},
  {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1",
     "content":[{"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"$PDF_B64"}}]}]}]}
JSON
# 预期（现状）：回答提到 base64/无法读取，或大 PDF 直接 400 too-long
# 2) 嵌套远程图片：把 document 换成 {"type":"image","source":{"type":"url","url":"https://<可访问图片>"}}
# 预期（现状）：400 invalid_request_error "remote image URL source was not materialized before conversion"
```

真实 Claude Code：在项目中放一个 PDF，让 CLI “读取并总结这个 PDF”，开启 tool format debug 或 payload breakdown，
观察 `largestCurrentToolResultBytes` 与 base64 大小同量级。

## 修复方案

### 候选方案

A. 在 converter 内为 tool_result 数组元素实现与顶层一致的逐块转换（推荐）。
B. 在 converter 前把 tool_result 中的 document/search_result 预展开成 text 块（改写 Anthropic 请求），converter 不变。
   会让外部池/Anthropic 透传路径也被改写，不适合。
C. 丢弃非 text 块（sub2api 做法）。不会泄漏 base64，但丢信息，不如 A。

### 推荐方案

1. `extract_tool_result_content` 改为返回 `Result<String, ConversionError>`，逐元素分派：

```rust
fn extract_tool_result_content(content: &Option<Value>) -> Result<String, ConversionError> {
    let Some(Value::Array(arr)) = content else { /* 字符串/其他沿用现状 */ };
    let mut parts = Vec::new();
    for item in arr {
        match item.get("type").and_then(Value::as_str) {
            Some("image") => parts.push(TOOL_RESULT_IMAGE_PLACEHOLDER.to_string()),
            Some("text") | None if item.get("text").is_some() => parts.push(text_of(item)),
            Some("document") => {
                let block: ContentBlock = from_value(item.clone())?;
                let source = block.source.ok_or(missing("document block in tool_result missing source"))?;
                let text = convert_document_source_to_text(source)?;   // 复用顶层逻辑（content.rs:180-228）
                parts.push(with_title(item, text));                   // 见第 3 点
            }
            Some("search_result") => parts.push(render_search_result(item)),
            _ if item.is_null() => {}
            Some(other) => parts.push(format!("[unsupported tool_result block omitted: {other}]")),
        }
    }
    Ok(parts.join("\n"))
}
```

   - `render_search_result`：`<search_result source="{source}" title="{title}">\n{content 中 text 块用 \n 连接}\n</search_result>`，
     与 `format_document_text` 的 `<document>` 包装风格一致。顶层 `search_result` 在 `process_message_content` 用同一函数，
     并且必须在 `from_value::<ContentBlock>` 之前按 `type` 分派，避免 `source` 为字符串时解析失败被吞。
   - 未知块不再 `to_string()`，改为短占位，避免泄漏大块 JSON/base64。
2. 前置物化递归：抽出 `for_each_media_block_mut(content, |block| ...)`，遍历顶层块和 `tool_result.content` 中的
   `image`/`document`，供以下四处共用：`count_remote_content_sources`、`materialize_content_sources`、
   `reject_non_inline_sources_in_content`、`materialize_file_sources_in_content`；`normalize_content_base64_image_media_types`
   也可顺带覆盖。远程下载预算、SSRF 防护、数量上限保持现有全局计数（嵌套块计入同一预算）。
3. 标题：document 块的 `title`/`context`（当前 `ContentBlock` 没有这两个字段，顶层同样丢失，见 P21）在渲染时放进
   `<document title="..." ...>` 属性；实现时在 `ContentBlock` 增加 `title`、`context` 可选字段即可。
4. payload guard 口径：展开后的 PDF 文本属于“当前 tool_result”，沿用现有 current/historical tool_result 截断规则，无需新规则。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 上述 4 条 converter 单测、1 条 body_processing 单测转绿。
- 回归：现有 `empty_tool_result_image_is_rejected_with_clear_error`、tool_result 图片提升到 `images` 的测试保持通过；
  纯字符串/纯 text 的 tool_result 输出与现状 byte-identical。
- 负例：嵌套 document 的 PDF 无法抽取时返回与顶层相同的 400 文案（`PDF document text could not be extracted ...`），
  而不是字符串化。
- 远程物化：嵌套 URL 图片计入 `remote_source_count` 上限；超过上限在 DNS/HTTP 前拒绝（沿用
  `remote_source_count_is_rejected_before_dns_or_http` 的模式）。
- 真实 Claude Code：读 1 页和 50 页 PDF 各一次，模型能引用 PDF 文字；payload breakdown 中 tool_result 大小与抽取文本同量级。

## 兼容性与风险

- 嵌套 PDF 抽取失败会从“静默字符串化”变成 400。这与顶层行为一致、更接近官方（官方不会把 base64 给模型），
  但可能暴露此前被掩盖的坏 PDF；可以考虑失败时降级为 `[document could not be extracted]` 占位而不是 400，需产品决策。
- PDF 抽取是 CPU 密集型且已有 panic guard 和全局锁（`src/anthropic/converter/content.rs:296-337`）；tool_result 路径高频，
  需要关注锁竞争，必要时把抽取移到 blocking 线程池。
- 嵌套远程物化会增加一次网络下载，受现有预算和 SSRF 防护约束；light 模式会开始拒绝嵌套远程源（行为收紧）。
- 外部池 Anthropic 透传路径不受影响（改动只在 Kiro converter 与本地物化）。

## 修复结果与验证（2026-09-29）

- 修复（`a6abce7`）：`tool_result.content` 中的内容按类型处理：document（base64、text、url 以及 `source.type=content`）按顶层规则转换为文本，并带上 title 和 context；search_result 渲染为带来源的文本；未知块只保留 `[<type> block omitted]` 占位，不再输出 JSON 或 base64。顶层的 search_result 与 `source.type=content` 的 document 也不再丢弃或返回 400。嵌套在 tool_result 里的图片和文档，如果来源是 url 或 file，现在会先物化，走与顶层相同的 SSRF 防护和预算限制。
- 单测：`tool_result_document_and_search_result_are_rendered_as_text`、`top_level_search_result_and_content_document_are_not_dropped`、`tool_result_nested_media_sources_are_counted_and_checked`、`materializes_file_source_nested_in_tool_result`。
- 真实上游：tool_result 中的文本 document 和 search_result 返回 200，模型准确复述了 `AURORA-17` 和 `Zephyr Launch Notes`；`source.type=content` 的 document 返回 200，内容为 `PELICAN`；tool_result 中通过 Files API `file_id` 引用的图片返回 200，修复前会返回 400。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。
