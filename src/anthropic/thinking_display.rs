//! `thinking.display: "omitted"` 的输出侧实现。
//!
//! 官方语义：thinking 块照常出现并携带 signature，但不下发 thinking 文本。
//! 流式时 `content_block_start` 的 thinking 为空串、不发送 `thinking_delta`，
//! `signature_delta` 与 `content_block_stop` 照常发送；非流式时 thinking 字段为空串。
//! `redacted_thinking` 本身不含可读文本，原样保留。

use serde_json::{Value, json};

use super::stream::SseEvent;

/// 流式：移除 thinking 文本，保留块结构与 signature。
pub(crate) fn omit_thinking_text_in_events(events: Vec<SseEvent>) -> Vec<SseEvent> {
    events
        .into_iter()
        .filter_map(|mut event| match event.event.as_str() {
            "content_block_delta" if event.data["delta"]["type"] == "thinking_delta" => None,
            "content_block_start" => {
                if event.data["content_block"]["type"] == "thinking" {
                    event.data["content_block"]["thinking"] = json!("");
                }
                Some(event)
            }
            _ => Some(event),
        })
        .collect()
}

/// 非流式：把 thinking 块的文本清空，保留 signature。
pub(crate) fn omit_thinking_text_in_content(content: &mut [Value]) {
    for block in content.iter_mut() {
        if block["type"] == "thinking" {
            block["thinking"] = json!("");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_omits_thinking_text_but_keeps_signature() {
        let events = vec![
            SseEvent::new(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "leak"}}),
            ),
            SseEvent::new(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "secret"}}),
            ),
            SseEvent::new(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
            ),
            SseEvent::new(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            SseEvent::new(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "answer"}}),
            ),
        ];

        let out = omit_thinking_text_in_events(events);

        assert_eq!(out.len(), 4);
        assert_eq!(out[0].data["content_block"]["thinking"], "");
        assert_eq!(out[1].data["delta"]["signature"], "sig");
        assert_eq!(out[2].event, "content_block_stop");
        assert_eq!(out[3].data["delta"]["text"], "answer");
        assert!(
            !out.iter()
                .any(|event| event.data["delta"]["type"] == "thinking_delta")
        );
    }

    #[test]
    fn non_streaming_clears_thinking_text_only() {
        let mut content = vec![
            json!({"type": "thinking", "thinking": "secret", "signature": "sig"}),
            json!({"type": "redacted_thinking", "data": "opaque"}),
            json!({"type": "text", "text": "answer"}),
        ];
        omit_thinking_text_in_content(&mut content);
        assert_eq!(content[0]["thinking"], "");
        assert_eq!(content[0]["signature"], "sig");
        assert_eq!(content[1]["data"], "opaque");
        assert_eq!(content[2]["text"], "answer");
    }
}
