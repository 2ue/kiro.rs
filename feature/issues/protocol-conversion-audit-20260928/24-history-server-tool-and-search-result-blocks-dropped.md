# P24 历史中的 server_tool_use / web_search_tool_result 等服务端工具块被静默丢弃

Status: open / documented / not-fixed
Severity: Low
Area: request
Discovered: 2026-09-28 协议互转审计

## 问题与影响

客户端回放历史时，assistant 消息里的服务端工具块在 Claude Code → Kiro 转换中被整块丢弃，不留任何文本，也不计入告警：

- assistant 历史：`convert_assistant_message_with_known_tools` 只处理 `thinking`、`redacted_thinking`、`text`、`tool_use`，
  其余类型落到 `_ => {}`（`src/anthropic/converter/history.rs:286-346`，丢弃点在 `src/anthropic/converter/history.rs:345`）。
- user 内容：`process_message_content` 同样只处理 `text`、`image`、`document`、`tool_result`，其余落到 `_ => {}`
  （`src/anthropic/converter/content.rs:31-86`，丢弃点在 `src/anthropic/converter/content.rs:85`）。

逐类型核对（`ContentBlock` 没有 `deny_unknown_fields`，未知字段会被忽略，`src/anthropic/types.rs:412-438`）：

| 块类型 | 通常位置 | 能否解析成 `ContentBlock` | 结果 |
| --- | --- | --- | --- |
| `server_tool_use` | assistant | 能（`id`/`name`/`input` 都是已有字段） | `history.rs:345` 丢弃 |
| `web_search_tool_result` | assistant | 能（`content` 是 `serde_json::Value`，`types.rs:427`） | `history.rs:345` 丢弃 |
| `web_fetch_tool_result` | assistant | 能（`content` 是对象，同样落到 `Value`） | `history.rs:345` 丢弃 |
| `tool_reference` | tool_result 内 | 不单独解析 | 在 tool_result 内被 `item.to_string()` 整块字符串化（`content.rs:616-617`），不是丢弃 |
| 顶层 `search_result` | user | 不能：`source` 是字符串，`ContentBlock.source` 是 `Option<ImageSource>` 结构体（`types.rs:437`、`types.rs:442-453`） | `from_value` 失败，被 `if let Ok(block)`（`content.rs:30`）跳过 |

顶层 `search_result` 与 tool_result 内的 `search_result` 已由 [P04](04-tool-result-document-and-media-blocks-stringified.md) 记录并给出渲染方案，
本文不重复，只要求两边共用同一个渲染函数（见“修复方案”）。

### 实际影响为什么是 Low

审计原结论是“WebSearch 之后的轮次丢失搜索结果，模型失去上下文”。按源码核对，这对本项目自己产生的 WebSearch 回复基本不成立：

1. 本项目的 WebSearch 回复在 `server_tool_use` 和 `web_search_tool_result` 前后各带一个 `text` 块
   （非流式 `src/anthropic/websearch.rs:672-692`，流式 `src/anthropic/websearch.rs:752-833` 及其后的 index 3 文本块）。
   第二个 `text` 是 `generate_search_summary` 生成的摘要，每条结果包含标题、最多 200 字符的 snippet 和 `Source: url`
   （`src/anthropic/websearch.rs:959-985`）。`text` 块在历史中会保留（`history.rs:315-323`），所以回放时结果的标题、URL 和摘要仍然会进入 Kiro history。
2. Claude Code CLI 主会话不经过这条路径。按
   [Claude Code 本地账号 WebSearch/工具/图片分析](../claude-code-local-accounts-websearch-tools-image-analysis-20260729.md)
   的真实 CLI 证据，CLI 使用客户端 `WebSearch` 工具：主会话里是普通 `tool_use name="WebSearch"` 加 `tool_result`（文本），
   原生 `web_search_*` 只出现在一次性的子请求里。
3. 只要请求声明了原生 `web_search_*` 工具，handler 就直接走 MCP 分支（`src/anthropic/handlers.rs:6482` 起，
   调用 `src/anthropic/handlers.rs:6559` 的 `handle_websearch_request`），根本不进入 converter。
   这是 [WebSearch/MCP 协议与隐私边界](../websearch-mcp-protocol-usage-and-privacy.md) 和上面那份文档里定下的决策（纯原生和混用都在服务端执行），本文不改变它。

所以，历史中的 `server_tool_use` 只有在后续请求不再声明原生 web_search 工具时才会到达 converter。
真正丢失信息的场景是：

- 历史由别的实现产生（例如官方协议客户端产生的会话迁移到本代理），其中 `web_search_tool_result` 是唯一携带结果的地方，模型最终回答里只有引用；
- `web_fetch_tool_result`：本项目不模拟 web_fetch 服务端工具（`src/` 中没有 `web_fetch_2*` 的处理），只要历史里有，它的抓取内容就会被整块丢弃；
- 所有情况下，“曾经发生过一次搜索、查询词是什么”这一结构信息都会丢失，且没有任何观测计数。

附带的小瑕疵：丢弃服务端块后，前后两个 `text` 块用 `push_str` 直接拼接，中间没有分隔
（`history.rs:315-323`），例如 `I'll search for "q".Here are the search results for "q":`。

## 官方协议对照

- Claude Code 协议侧（接口要接受什么）：assistant 历史可以包含 `server_tool_use`、`web_search_tool_result`
  （`content` 是 `web_search_result` 数组，或 `web_search_tool_result_error`）、`web_fetch_tool_result`；
  user 内容和 tool_result 内容可以包含 `search_result`。客户端回放历史时会原样带回这些块，接口需要把它们表达的上下文交给模型。
  `web_search_result.encrypted_content` 在官方语义里是不透明密文，只用于回放，不可读。本项目生成的
  `encrypted_content` 实际放的是明文 snippet（`src/anthropic/websearch.rs:617-623`），这是 WebSearch 模拟的已知偏差，不在本文处理。
- Kiro 协议侧（上游要求）：history 的 `assistantResponseMessage` 只有 `content`（文本）、`toolUses`、`reasoningContent`；
  没有服务端工具块的结构化表示。`toolUses` 需要与后续 user 的 `toolResults` 配对，不能拿来承载服务端工具
  （服务端工具的结果在同一条 assistant 内，没有对应的 user tool_result）。所以唯一可行的表示是转换成 assistant 文本。
- 已验证：Kiro assistant `content` 不能为空（源码注释 `history.rs:380`，空内容写 `" "`，`history.rs:390-391`）。
  只有服务端块的 assistant 消息目前会变成 `" "`，渲染成文本后这个占位也不再需要。

## 源码链与根因

```text
convert_request_with_model_id
  -> build_history (history.rs:27-184)
     assistant 消息 -> merge_assistant_messages_with_known_tools (history.rs:443)
        -> convert_assistant_message_with_known_tools (history.rs:244)
           for item in content:
             redacted_thinking 预处理 (268-284)
             from_value::<ContentBlock> (285)
             match block_type: thinking | redacted_thinking | text | tool_use | _ => {} (286-346)
     user 消息 -> merge_user_messages (history.rs:187) -> process_message_content (content.rs:17)
        match block_type: text | image | document | tool_result | tool_use | redacted_thinking | _ => {} (31-86)
  -> guard_kiro_request -> apply_payload_shaping (payload_guard.rs:2150)
```

根因：converter 按“已知块白名单”转换，白名单之外一律静默丢弃，没有“无法结构化表示的块降级为文本”这一层，也没有告警计数。

payload guard 的后续环节不会补回这些内容，也不会限制渲染后的大小：

- `truncate_history_tool_results` 只处理 history user 的 `toolResults` 文本（`payload_guard.rs:2951-2966`），
  默认上限 `historical_tool_result_max_chars = 8_000`（`src/model/config.rs:652-653`、`src/model/config.rs:4440-4442`）。
- `trim_history_web_fetch_content` 只处理 history user 的 content 和 `toolResults`，并且只识别 `Web page content:\n---\n` 标记
  （`payload_guard.rs:3229-3269`、`payload_guard.rs:3295-3300`），默认 `web_fetch_body_max_chars = 12_000`（`config.rs:4464-4466`）。
- 两者都不处理 assistant history 的 `content`。所以如果把服务端块渲染进 assistant 文本，必须在 converter 里自带上限，不能依赖 guard 截断。

## 复现

### 最小复现（单测）

放入 `src/anthropic/converter.rs` 的 `mod tests`，沿用 `convert_assistant_message` + `ConverterOptions::default()` 的写法
（参考 `test_convert_assistant_message_tool_use_only`，`src/anthropic/converter.rs:4733`）：

```rust
#[test]
fn history_web_search_blocks_are_rendered_as_bounded_text() {
    let msg = super::super::types::Message {
        role: "assistant".to_string(),
        content: serde_json::json!([
            {"type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search",
             "input": {"query": "rust 2024 edition"}},
            {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01", "content": [
                {"type": "web_search_result", "title": "Rust 2024",
                 "url": "https://example.com/rust-2024",
                 "encrypted_content": "opaque-ciphertext", "page_age": "February 20, 2025"}
            ]},
            {"type": "text", "text": "Rust 2024 shipped with 1.85."}
        ]),
    };

    let result =
        convert_assistant_message(&msg, &mut HashMap::new(), ConverterOptions::default())
            .expect("convert");
    let content = &result.assistant_response_message.content;

    // 当前实现失败：content == "Rust 2024 shipped with 1.85."
    assert!(content.contains("rust 2024 edition"));
    assert!(content.contains("https://example.com/rust-2024"));
    assert!(content.contains("Rust 2024"));
    // 不透明字段不得进入模型上下文
    assert!(!content.contains("opaque-ciphertext"));
    // 服务端工具不能变成需要配对的 Kiro toolUses
    assert!(result.assistant_response_message.tool_uses.is_none());
}

#[test]
fn history_web_search_rendering_is_bounded() {
    let results: Vec<_> = (0..200)
        .map(|i| serde_json::json!({
            "type": "web_search_result",
            "title": "T".repeat(5_000),
            "url": format!("https://example.com/{i}/{}", "p".repeat(5_000)),
            "encrypted_content": "x"
        }))
        .collect();
    let msg = super::super::types::Message {
        role: "assistant".to_string(),
        content: serde_json::json!([
            {"type": "server_tool_use", "id": "srvtoolu_02", "name": "web_search", "input": {"query": "q"}},
            {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_02", "content": results}
        ]),
    };
    let result =
        convert_assistant_message(&msg, &mut HashMap::new(), ConverterOptions::default())
            .expect("convert");
    // 与 historical_tool_result_max_chars 默认值同量级（见“推荐方案”的常量）
    assert!(result.assistant_response_message.content.chars().count() <= 8_000 + 256);
}

#[test]
fn proxy_generated_websearch_summary_text_survives_history_replay() {
    // 回归护栏：本项目自己的 WebSearch 回复形状，当前实现已通过，修复后必须保持
    let msg = super::super::types::Message {
        role: "assistant".to_string(),
        content: serde_json::json!([
            {"type": "text", "text": "I'll search for \"q\"."},
            {"type": "server_tool_use", "id": "srvtoolu_03", "name": "web_search", "input": {"query": "q"}},
            {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_03", "content": []},
            {"type": "text", "text": "Here are the search results for \"q\":\n\n1. **A**\n   Source: https://a.example\n"}
        ]),
    };
    let result =
        convert_assistant_message(&msg, &mut HashMap::new(), ConverterOptions::default())
            .expect("convert");
    assert!(result.assistant_response_message.content.contains("Source: https://a.example"));
}
```

`web_fetch_tool_result` 与 `web_search_tool_result_error` 各加一条同形状的用例（断言 URL/title 或 `error_code` 出现在 content 中）。
顶层 `search_result` 的用例见 P04 的 `top_level_search_result_is_not_silently_dropped`。

### 端到端复现

后续请求不声明原生 web_search 工具，历史里带一条官方形状的搜索结果（结果只存在于 `web_search_tool_result` 中）：

```bash
curl -sS http://127.0.0.1:8990/cc/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: <key>' \
  -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"claude-sonnet-4-5","max_tokens":256,
       "messages":[
         {"role":"user","content":"Search: what is the codename in the probe page?"},
         {"role":"assistant","content":[
           {"type":"server_tool_use","id":"srvtoolu_e2e","name":"web_search","input":{"query":"probe page codename"}},
           {"type":"web_search_tool_result","tool_use_id":"srvtoolu_e2e","content":[
             {"type":"web_search_result","title":"Probe codename is BLUE-HERON-42","url":"https://example.com/probe","encrypted_content":"x"}]},
           {"type":"text","text":"I found the probe page."}]},
         {"role":"user","content":"Which URL did you find, and what was the page title? Answer exactly."}]}'
```

- 修复前：模型答不出 `https://example.com/probe` 和 `BLUE-HERON-42`（history 里只有 `I found the probe page.`）；
  用 tool format debug 采样确认 history assistant `content` 不含 URL。
- 修复后：回答包含 URL 和标题；采样中 assistant `content` 含渲染块，`toolUses` 为空。

## 修复方案

### 候选方案

A. 在 converter 里把服务端工具块渲染成有界纯文本，追加到 assistant 文本（推荐）。
B. 转成 Kiro `toolUses` + 伪造的下一条 user `toolResults`。会改变 history 角色结构，还要伪造 user 轮次，与 P20 的“伪造 OK”问题同类，不采用。
C. 保持丢弃，只加告警计数。对本项目自产历史够用（摘要文本已在），但官方形状历史和 web_fetch 仍然丢信息。

### 推荐方案

1. 在 `src/anthropic/converter/content.rs` 新增共享渲染函数（按 `type` 字段直接读 `serde_json::Value`，不经过 `ContentBlock`，
   避免 `source` 这类字段类型冲突），history assistant 和 `process_message_content` 共用：

```rust
const SERVER_TOOL_TEXT_MAX_CHARS: usize = 8_000;   // 与 historical_tool_result_max_chars 默认值一致
const SERVER_TOOL_MAX_RESULTS: usize = 20;
const SERVER_TOOL_TITLE_MAX_CHARS: usize = 200;    // 与 generate_search_summary 的 snippet 上限一致
const SERVER_TOOL_URL_MAX_CHARS: usize = 512;

/// 返回 None 表示不是服务端工具块，调用方走原有逻辑。
pub(super) fn render_server_tool_block(item: &serde_json::Value) -> Option<String> {
    match item.get("type")?.as_str()? {
        // 查询词：Search query: "..."
        "server_tool_use" => Some(render_server_tool_use(item)),
        // 每条结果：序号、title、url、page_age；不读 encrypted_content
        // 错误形态：[web_search error: <error_code>]
        "web_search_tool_result" => Some(render_web_search_result(item)),
        // url、title、retrieved_at；document 文本复用 P04 的 document 渲染并截断
        "web_fetch_tool_result" => Some(render_web_fetch_result(item)),
        // 与 P04 共用：<search_result source=".." title="..">text</search_result>
        "search_result" => Some(render_search_result(item)),
        _ => None,
    }
}
```

2. `convert_assistant_message_with_known_tools`：在 `from_value::<ContentBlock>` 之前（`history.rs:285` 前，
   与 `redacted_thinking` 预处理同一位置）调用 `render_server_tool_block`，命中则把结果按段落追加到 `text_content`，然后 `continue`。
   渲染块前后补 `\n\n`，顺带消除相邻 `text` 块粘连。
3. `process_message_content`：同样在 `from_value` 之前分派 `search_result`（这一步就是 P04 的顶层修复），
   其他服务端块出现在 user 里时也用同一函数渲染成文本，不再丢弃。
4. 有界规则：
   - 单条结果的 title、url 按字符截断（UTF-8 安全，沿用 `generate_search_summary` 的 `char_indices().nth(n)` 写法）；
   - 每个块最多渲染 `SERVER_TOOL_MAX_RESULTS` 条，超出写 `... N more results omitted`；
   - 每个块总长不超过 `SERVER_TOOL_TEXT_MAX_CHARS`；
   - 永不渲染 `encrypted_content`：官方形态下它是密文，本项目形态下它的明文 snippet 已经出现在后面的摘要 `text` 中。
   上限取 8_000，是因为这些块在语义上是“历史中的工具结果”，与 guard 对 history tool_result 的默认截断口径一致；
   guard 不处理 assistant content，所以上限必须在 converter 内生效。整体 payload 超限仍由 guard 的现有裁剪处理（见 [P03](03-payload-guard-silent-context-trimming.md)）。
5. 观测：`ProxyWarnings`（`src/anthropic/converter.rs:192-213`）新增 `server_tool_blocks_textified` 计数并编码进 `x-kiro-rs-warnings`；
   仍然未知的块类型新增 `unknown_content_blocks_dropped` 计数，替代现在完全静默的 `_ => {}`。
6. 不改变 WebSearch 路由决策（声明原生 web_search 就走 MCP），不改变本项目 WebSearch 回复的块形状。

## 测试与验收

- 上面 3 条单测：前 2 条修复前失败、修复后通过；第 3 条修复前后都通过（本项目自产形状不回退）。
- 补充：`web_fetch_tool_result`、`web_search_tool_result_error`、只含服务端块的 assistant（content 不再是 `" "`，而是渲染文本）。
- 回归：`test_convert_assistant_message_tool_use_only`、`sanitizes_continue_transcript_but_preserves_following_tool_use`，
  以及 P22 列出的 reasoning 合并边界测试保持通过；普通 `text`/`tool_use` 历史的转换结果逐字节不变。
- 端到端：上面的 curl 修复后能答出 URL 和标题；`x-kiro-rs-warnings` 含 `server-tool-blocks-textified=2`（命名以实现为准）。
- 真实 Claude Code CLI：执行一次 `WebSearch` 后再追问两轮，确认行为与修复前一致（主会话本来不含服务端块）。

## 兼容性与风险

- 只影响“原本被丢弃”的块；不含这些块的请求转换结果不变，prompt cache 前缀不受影响。
- 含服务端块的历史，修复后 assistant 文本会变化，这类会话在升级那一轮会有一次缓存前缀失效。
- 搜索结果是不可信外部内容。渲染成 history 文本后，结果中的提示注入会进入模型上下文。这个风险在本项目自产摘要中已经存在
  （[WebSearch/MCP 协议与隐私边界](../websearch-mcp-protocol-usage-and-privacy.md) 残余风险一节），本方案通过长度上限、
  不渲染 snippet/密文加以收敛，不能完全消除。
- 渲染格式可能被模型在可见输出中模仿。采用与现有摘要相近的纯文本行格式，不引入新的伪 XML 标签（`search_result` 沿用 P04 的包装）。
- 本文不涉及 WebSearch 模拟本身的偏差（query 来源、citations、`encrypted_content` 放明文），这些属于审计索引中的 P13。
- 回滚：去掉 `render_server_tool_block` 的调用即可恢复为丢弃。
