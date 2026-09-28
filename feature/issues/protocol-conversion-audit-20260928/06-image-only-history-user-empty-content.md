# P06 历史中只含图片（或只含被忽略块）的 user 消息被转成空 content

Status: open / documented / not-fixed
Severity: Medium
Area: request
Discovered: 2026-09-28 协议互转审计

## 问题与影响

历史 user 消息由 `merge_user_messages`（`src/anthropic/converter/history.rs:187-223`）转换。它只在“文本为空且存在
tool_results”时填占位 `Tool result received.`（`src/anthropic/converter/history.rs:204-207`）：

```rust
let mut content = content_parts.join("\n");
if content.trim().is_empty() && !all_tool_results.is_empty() {
    content = TOOL_RESULTS_PROVIDED_PLACEHOLDER.to_string();
}
```

所以以下历史 user 消息会以 `content: ""` 发给 Kiro：

- 只有 `image` 块（用户只贴截图、不打字）；
- `content` 是空字符串或空数组；
- 只含当前 converter 会忽略的块，例如顶层 `search_result`（解析失败被跳过，见 [P04](04-tool-result-document-and-media-blocks-stringified.md)）、
  user 侧 `redacted_thinking`（`src/anthropic/converter/content.rs:80-84`）或其他未知类型；
- 只有 `text: ""` 的块。

当前消息路径则有兜底：文本为空时，有 tool_results 填 `Tool result received.`，否则填 `.`
（`src/anthropic/converter.rs:556-565`）。assistant 历史也有兜底：空内容填 `" "`
（`src/anthropic/converter/history.rs:380-394`、`src/anthropic/converter/history.rs:554-558`，源码注释写明
“Kiro API 要求 content 字段不能为空”）。只有历史 user 缺这一层。

影响：一旦历史里出现一条纯图片 user 消息，此后该会话的每一轮都携带空 content 的 history user。
若 Kiro 对空 content 做校验（下节），整段会话会持续 400，直到客户端 compact/clear 把这条消息挤出历史。

核对“后续是否有兜底”：

- payload guard 的 `repair_request`（`src/anthropic/payload_guard.rs:4528-4555`）只规范化空 tool_result 内容，
  不处理空 user content。
- guard 唯一写 `.` 占位的地方是 `repair_tool_results`：仅当“移除孤儿 tool_result 后结果为空且 content 为空”时才写
  （`src/anthropic/payload_guard.rs:4812-4826`）。纯图片消息没有 tool_results，不命中。
- `drop_oversized_history_images` 只在丢弃超过 5 MB 的图片时通过 `append_text` 追加说明文本
  （`src/anthropic/payload_guard.rs:1969-2010`、`src/anthropic/payload_guard.rs:5031-5041`）；普通尺寸图片不命中。
- `UserMessage` 序列化没有跳过空 content（`src/kiro/model/requests/conversation.rs:281-307`）。

结论：审计所述成立，当前没有任何后续环节为纯图片历史 user 填充 content。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

- 已验证事实（Anthropic 文档）：user 消息的 `content` 可以只包含 `image` 块，官方正常接受；空字符串 content 官方会 400
  （`messages.N: all messages must have non-empty content`，按已知官方错误文案，本次未重新请求验证）。
  也就是说，纯图片消息在官方是合法请求，代理必须把它转成 Kiro 可接受的形式，而不是转成非法形式。
- Kiro 侧（已知约束，未通过本次抓包验证）：user/assistant `content` 不能为空。证据是本项目源码注释
  （`src/anthropic/converter.rs:78-82`、`src/anthropic/converter/history.rs:380`）和 `sub2api-kiro` 的实现：
  它在“有图片但文本为空”时把历史和当前 user content 设为 `" "`
  （`../sub2api-kiro/backend/internal/pkg/kiro/translator.go` 约 2660-2663 行和约 526-529 行）。
  空字符串具体返回哪种 400（`Improperly formed request` 还是其他），没有本地证据。

## 源码链与根因

```text
convert_request_with_model_id (converter.rs:417)
  -> build_history (history.rs:27-184)
     for messages[0..n-1]:
       user -> user_buffer; 遇到 assistant 时 merge_user_messages(user_buffer)
       末尾孤立 user -> merge_user_messages + 自动 "OK" assistant (history.rs:173-181)
  merge_user_messages (history.rs:187-223)
     process_message_content -> (text="", images=[img], tool_results=[])
     content="" （204-207 只看 tool_results）
     UserMessage::new("", model).with_images([img])
  -> guard_kiro_request -> repair_request   // 不补空 content
  -> serialize -> {"userInputMessage":{"content":"","images":[...],...}}
```

根因：空内容占位逻辑在三个地方各写一份（current user、history assistant、history user），history user 只覆盖了 tool_result
一种场景，没有统一的“Kiro 非空 content”不变量。

## 复现

### 最小复现（单测）

放入 `src/anthropic/converter.rs` 的 `mod tests`（复用 `VALID_PNG_1X1_BASE64` 常量，`src/anthropic/converter.rs:650`）：

```rust
#[test]
fn image_only_history_user_message_gets_non_empty_content() {
    use super::super::types::Message as AnthropicMessage;

    let req = MessagesRequest {
        model: "claude-sonnet-4".to_string(),
        max_tokens: 1024,
        messages: vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!([{
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/png", "data": VALID_PNG_1X1_BASE64}
                }]),
            },
            AnthropicMessage { role: "assistant".to_string(), content: serde_json::json!("I see a pixel.") },
            AnthropicMessage { role: "user".to_string(), content: serde_json::json!("What color?") },
        ],
        stream: false,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let result = convert_request(&req).unwrap();
    let Message::User(first) = &result.conversation_state.history[0] else {
        panic!("expected history user");
    };
    assert_eq!(first.user_input_message.images.len(), 1);
    // 当前实现失败：content == ""
    assert!(!first.user_input_message.content.trim().is_empty());
}

#[test]
fn empty_string_history_user_message_gets_non_empty_content() {
    // messages: user "", assistant "ok", user "next"；断言 history[0] content 非空
}
```

### 端到端复现

```bash
IMG=$(base64 < small.png | tr -d '\n')
curl -sS http://127.0.0.1:PORT/v1/messages -H 'x-api-key: <KEY>' -H 'anthropic-version: 2023-06-01' \
  -H 'content-type: application/json' -d @- <<JSON
{"model":"claude-sonnet-4-5","max_tokens":128,"messages":[
 {"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"$IMG"}}]},
 {"role":"assistant","content":"Got the screenshot."},
 {"role":"user","content":"Describe it again."}]}
JSON
```

观察：开启 tool format debug 的请求体采样，或日志 `Kiro request prepared`，确认 history[0] `content:""`；记录 Kiro 返回码。
这一步同时补齐“Kiro 是否拒绝空 content”的证据。

真实 Claude Code：粘贴一张截图直接回车（不输入文字），得到回复后再发一句普通文本，观察第二轮是否 400。
新版 Claude Code 可能会自动附带 `[Image #1]` 之类文本（经验推断），需以抓包为准。

## 修复方案

### 候选方案

A. 在 `merge_user_messages` 内补齐：`content.trim().is_empty()` 时，有 tool_results 用 `Tool result received.`，否则用 `.`，
   与 current user 路径一致（推荐）。
B. 在 payload guard `repair_request` 增加 `ensure_non_empty_user_content(history, current)`，作为最终不变量。
   能兜住所有来源（包括 guard 自己的修改），但 guard 关闭时（`payloadGuardEnabled=false`，`src/anthropic/payload_guard.rs:466-475` 直接返回）失效。
C. 空内容时用 `" "`（sub2api 做法）。assistant 路径已用 `" "`；但 current user 路径特意选了 `.`，历史 user 跟随 current 更一致。

### 推荐方案

A 为主，B 作为不变量兜底：

1. 抽一个共享函数（放 `converter.rs`，current 与 history 共用）：

```rust
pub(super) fn non_empty_user_content(text: String, has_tool_results: bool, has_images: bool,
                                     warnings: &mut ProxyWarnings) -> String {
    if !text.trim().is_empty() { return text; }
    if has_tool_results {
        warnings.tool_result_content_placeholders += 1;
        TOOL_RESULTS_PROVIDED_PLACEHOLDER.to_string()
    } else {
        // has_images 仅用于统计；占位与 current 路径一致
        warnings.empty_content_placeholders += 1;
        EMPTY_USER_CONTENT_PLACEHOLDER.to_string()
    }
}
```

   `merge_user_messages` 需要多接一个 `&mut ProxyWarnings` 参数（`build_history` 已持有 warnings）。
2. current user 路径（`src/anthropic/converter.rs:556-565`）改为调用同一函数，行为不变。
3. payload guard 的 `repair_request` 末尾增加对 history user / current user 的非空检查，仅在确实为空时写 `.`，
   计入一个新的 `RepairStats` 计数，便于观测“converter 之外产生空 content”的情况。
4. 占位内容选择：图片场景也用 `.`。注意 `src/anthropic/converter.rs:78-82` 的注释说明 `.` 在 tool_result 场景会让模型忽略结果，
   但纯图片场景没有 tool_result，风险不同；若担心模型忽略图片，可改用 `[image]`，需要真实 CLI 验证后定。

## 测试与验收

- 两条新单测转绿；已有 `current_empty_user_message_gets_inert_placeholder`、tool_result 占位测试保持通过。
- 纯图片历史 user：`images` 保留，content 为占位；`warnings.empty_content_placeholders` 计数 +1。
- payload guard：构造 history user `content=""` 且无图片/无 tool_results 的 `KiroRequest`，guard 后 content 非空。
- 端到端：上面的 curl 请求成功，回复能描述第一轮图片。
- 抓包补证：记录一次“空 content 历史 user”在 Kiro 的真实返回，写入本文“官方协议对照”一节。

## 兼容性与风险

- 仅影响“原本为空”的 content，非空消息 byte-identical，prompt cache 前缀不受影响（空消息原本就可能导致失败）。
- 占位文本进入模型上下文，理论上有轻微语义噪音；`.` 已在 current 路径长期使用，风险可控。
- 若事后证明 Kiro 实际接受空 content，本改动仍无害。
