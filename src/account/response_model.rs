//! 返回给调用方的模型名一律用调用方请求体里的原值。
//!
//! 账号模型映射，以及上游自己换了模型返回，都不能把实际模型名漏给调用方：
//! 流式只改 `message_start` 的 `message.model`，非流式只改响应体顶层的 `model`。
//! 上游没写 `model` 时补上调用方的值（协议里这是必填字段）；其它字段原样保留。

use bytes::Bytes;
use serde_json::Value;

/// 流式事件的模型处理结果：上游在 `message_start` 里声明的模型（改写前，记进用量记录），
/// 以及改写后的事件（不需要改时为 `None`）。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SseModelRewrite {
    pub(super) upstream_model: Option<String>,
    pub(super) rewritten: Option<Vec<u8>>,
}

/// 流式事件：是 `message_start` 时取出上游声明的模型，并把 `message.model` 改成调用方模型
/// （`client_model` 为 `None` 时只取不改）。
pub(super) fn process_sse_event_model(event: &[u8], client_model: Option<&str>) -> SseModelRewrite {
    let mut result = SseModelRewrite::default();
    // 绝大多数事件是内容增量，先按字节排除，避免逐个解析 JSON。
    if !contains(event, b"message_start") {
        return result;
    }
    let Ok(text) = std::str::from_utf8(event) else {
        return result;
    };
    let mut changed = false;
    let mut out = String::with_capacity(text.len() + client_model.map_or(0, str::len));
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        let rewritten = body
            .strip_prefix("data:")
            .and_then(|data| serde_json::from_str::<Value>(data.trim()).ok())
            .and_then(|mut value| {
                if value.get("type").and_then(Value::as_str) != Some("message_start") {
                    return None;
                }
                let message = value.get_mut("message")?.as_object_mut()?;
                let declared = message.get("model").and_then(Value::as_str);
                if result.upstream_model.is_none() {
                    result.upstream_model = declared
                        .map(str::trim)
                        .filter(|model| !model.is_empty())
                        .map(str::to_string);
                }
                let client_model = client_model?;
                if declared == Some(client_model) {
                    return None;
                }
                message.insert("model".to_string(), Value::String(client_model.to_string()));
                serde_json::to_string(&value).ok()
            });
        match rewritten {
            Some(data) => {
                changed = true;
                out.push_str("data: ");
                out.push_str(&data);
                out.push_str(ending);
            }
            None => out.push_str(line),
        }
    }
    result.rewritten = changed.then(|| out.into_bytes());
    result
}

/// 流式事件：是 `message_start` 时把 `message.model` 改成调用方模型；不需要改时返回 `None`。
pub(super) fn rewrite_sse_event_model(event: &[u8], client_model: &str) -> Option<Vec<u8>> {
    process_sse_event_model(event, Some(client_model)).rewritten
}

/// 非流式响应体里上游声明的模型（顶层 `model`，或包在 `message` 里）。
pub(super) fn body_declared_model(value: &Value) -> Option<String> {
    value
        .get("model")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("model"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// 非流式响应体：JSON 消息改顶层 `model`；上游按 SSE 返回的整段文本逐个事件改 `message_start`。
/// 错误体等不是消息的 JSON 原样返回。
pub(super) fn rewrite_body_model(body: Bytes, client_model: &str) -> Bytes {
    if let Ok(mut value) = serde_json::from_slice::<Value>(&body) {
        let Some(object) = value.as_object_mut() else {
            return body;
        };
        let is_message = object.get("type").and_then(Value::as_str) == Some("message")
            || object.contains_key("model");
        if !is_message || object.get("model").and_then(Value::as_str) == Some(client_model) {
            return body;
        }
        object.insert("model".to_string(), Value::String(client_model.to_string()));
        return serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body);
    }
    if !contains(&body, b"message_start") {
        return body;
    }
    let mut out = Vec::with_capacity(body.len() + client_model.len());
    let mut changed = false;
    let mut rest: &[u8] = &body;
    while !rest.is_empty() {
        let end = event_end(rest).unwrap_or(rest.len());
        let (event, tail) = rest.split_at(end);
        match rewrite_sse_event_model(event, client_model) {
            Some(rewritten) => {
                changed = true;
                out.extend_from_slice(&rewritten);
            }
            None => out.extend_from_slice(event),
        }
        rest = tail;
    }
    if changed { Bytes::from(out) } else { body }
}

/// 一个 SSE 事件结束的位置（含空行分隔符）；兼容 `\n\n` 与 `\r\n\r\n`。
fn event_end(bytes: &[u8]) -> Option<usize> {
    let lf = find(bytes, b"\n\n").map(|idx| idx + 2);
    let crlf = find(bytes, b"\r\n\r\n").map(|idx| idx + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn data_json(event: &[u8]) -> Value {
        let text = std::str::from_utf8(event).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data:"))
            .unwrap();
        serde_json::from_str(data.trim()).unwrap()
    }

    #[test]
    fn message_start_model_is_replaced_with_client_model_and_other_fields_kept() {
        let event = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"content\":[],\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n";
        let rewritten = rewrite_sse_event_model(event, "claude-fable-5-2").unwrap();
        let text = std::str::from_utf8(&rewritten).unwrap();
        assert!(text.starts_with("event: message_start\ndata: "), "{text}");
        assert!(text.ends_with("\n\n"), "{text}");
        assert!(!text.contains("claude-opus-4-8"), "{text}");
        let value = data_json(&rewritten);
        assert_eq!(value["message"]["model"], "claude-fable-5-2");
        assert_eq!(value["message"]["id"], "msg_1");
        assert_eq!(value["message"]["usage"]["input_tokens"], 12);
        // 字段顺序保持上游原样。
        let keys = value["message"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            ["id", "type", "role", "model", "content", "usage"].map(String::from)
        );
    }

    #[test]
    fn upstream_declared_model_is_reported_before_rewrite() {
        let event = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n\n";
        let observed = process_sse_event_model(event, Some("claude-fable-5-2"));
        assert_eq!(observed.upstream_model.as_deref(), Some("claude-opus-4-8"));
        assert!(observed.rewritten.is_some());
        // 不改写时也取得到；模型与调用方一致时不改写但仍记录。
        let only_observe = process_sse_event_model(event, None);
        assert_eq!(
            only_observe.upstream_model.as_deref(),
            Some("claude-opus-4-8")
        );
        assert!(only_observe.rewritten.is_none());
        let same = process_sse_event_model(event, Some("claude-opus-4-8"));
        assert_eq!(same.upstream_model.as_deref(), Some("claude-opus-4-8"));
        assert!(same.rewritten.is_none());
        let delta = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\"}\n\n";
        assert_eq!(
            process_sse_event_model(delta, Some("x")),
            SseModelRewrite::default()
        );
        assert_eq!(
            body_declared_model(&json!({"type": "message", "model": " claude-opus-4-8 "}))
                .as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(body_declared_model(&json!({"type": "error"})), None);
    }

    #[test]
    fn message_start_without_model_gets_client_model() {
        let event = b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\"}}\r\n\r\n";
        let rewritten = rewrite_sse_event_model(event, "claude-sonnet-5-5").unwrap();
        assert!(rewritten.ends_with(b"\r\n\r\n"));
        assert_eq!(
            data_json(&rewritten)["message"]["model"],
            "claude-sonnet-5-5"
        );
    }

    #[test]
    fn other_events_and_matching_model_are_left_untouched() {
        let delta = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"model\"}}\n\n";
        assert!(rewrite_sse_event_model(delta, "claude-fable-5-2").is_none());
        let same = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-fable-5-2\"}}\n\n";
        assert!(rewrite_sse_event_model(same, "claude-fable-5-2").is_none());
        // 文本里出现 message_start 字样但不是该事件：不改。
        let text = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"message_start\"}}\n\n";
        assert!(rewrite_sse_event_model(text, "claude-fable-5-2").is_none());
        assert!(rewrite_sse_event_model(b"event: ping\ndata: [DONE]\n\n", "x").is_none());
    }

    #[test]
    fn non_stream_message_body_model_is_replaced() {
        let body = Bytes::from(
            json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-haiku-4-5",
                "content": [{"type": "text", "text": "hi"}],
                "usage": {"input_tokens": 3, "output_tokens": 1}
            })
            .to_string(),
        );
        let rewritten = rewrite_body_model(body, "claude-3-5-haiku-20241022");
        let value: Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(value["model"], "claude-3-5-haiku-20241022");
        assert_eq!(value["content"][0]["text"], "hi");
        assert!(!String::from_utf8_lossy(&rewritten).contains("claude-haiku-4-5\""));
    }

    #[test]
    fn non_stream_error_body_and_matching_model_are_returned_as_is() {
        let error = Bytes::from_static(
            br#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
        );
        assert_eq!(rewrite_body_model(error.clone(), "claude-fable-5-2"), error);
        let same = Bytes::from_static(br#"{"type":"message","model":"claude-fable-5-2"}"#);
        assert_eq!(rewrite_body_model(same.clone(), "claude-fable-5-2"), same);
    }

    #[test]
    fn non_stream_sse_text_body_rewrites_only_message_start() {
        let body = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"claude-opus-4-8\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let rewritten = rewrite_body_model(body, "claude-fable-5-2");
        let text = String::from_utf8(rewritten.to_vec()).unwrap();
        assert!(
            text.starts_with("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-fable-5-2\"}}\n\n"),
            "{text}"
        );
        // 内容里的文字不是模型字段，不改。
        assert!(text.contains("\"text\":\"claude-opus-4-8\""), "{text}");
        assert!(text.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
    }
}
