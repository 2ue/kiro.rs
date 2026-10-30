//! Anthropic Messages 响应翻译成 OpenAI Chat Completions 格式：非流式、流式和错误信封。

use std::collections::HashMap;

use serde_json::{Map, Value, json};

/// 单个 SSE 事件的上限；正常事件远小于此，超过按上游协议错误处理。
const MAX_SSE_EVENT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Usage {
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
}

impl Usage {
    /// 后到的字段覆盖先到的：`message_delta.usage` 是最终修正值。
    pub(super) fn merge(&mut self, usage: &Value) {
        let field = |name: &str| usage.get(name).and_then(Value::as_u64);
        if let Some(value) = field("input_tokens") {
            self.input = value;
        }
        if let Some(value) = field("output_tokens") {
            self.output = value;
        }
        if let Some(value) = field("cache_creation_input_tokens") {
            self.cache_creation = value;
        }
        if let Some(value) = field("cache_read_input_tokens") {
            self.cache_read = value;
        }
    }

    /// 输入 token 总数：未命中、写缓存和读缓存三部分相加（OpenAI 口径包含命中缓存的部分）。
    fn total_input(self) -> u64 {
        self.input
            .saturating_add(self.cache_creation)
            .saturating_add(self.cache_read)
    }

    /// OpenAI 的 `prompt_tokens` 包含命中缓存的部分，命中数另列在 `cached_tokens`。
    fn to_openai(self) -> Value {
        let prompt = self.total_input();
        json!({
            "prompt_tokens": prompt,
            "completion_tokens": self.output,
            "total_tokens": prompt.saturating_add(self.output),
            "prompt_tokens_details": {"cached_tokens": self.cache_read},
        })
    }
}

impl Usage {
    /// Responses 接口的用量：`input_tokens` 同样包含命中缓存的部分。上游不单独给推理 token。
    pub(super) fn to_responses(self) -> Value {
        let input = self.total_input();
        json!({
            "input_tokens": input,
            "input_tokens_details": {"cached_tokens": self.cache_read},
            "output_tokens": self.output,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": input.saturating_add(self.output),
        })
    }
}

fn finish_reason(stop_reason: Option<&str>, has_tool_calls: bool) -> &'static str {
    match stop_reason {
        Some("tool_use") => "tool_calls",
        Some("max_tokens" | "model_context_window_exceeded") => "length",
        Some("refusal") => "content_filter",
        None if has_tool_calls => "tool_calls",
        _ => "stop",
    }
}

fn completion_id(message_id: Option<&str>) -> String {
    prefixed_id("chatcmpl-", message_id)
}

/// 用 Anthropic 消息 id 的随机部分拼出 OpenAI 侧的 id；没有消息 id 时现生成一个。
pub(super) fn prefixed_id(prefix: &str, message_id: Option<&str>) -> String {
    let suffix = message_id
        .map(|id| id.strip_prefix("msg_").unwrap_or(id))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let id = crate::anthropic::envelope::message_id();
            id.strip_prefix("msg_").unwrap_or(&id).to_string()
        });
    format!("{prefix}{suffix}")
}

pub(super) fn tool_arguments(input: &Value) -> String {
    match input {
        Value::Null => "{}".to_string(),
        input => serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string()),
    }
}

pub(super) fn model_or<'a>(model: Option<&'a str>, fallback: &'a str) -> &'a str {
    model.filter(|model| !model.is_empty()).unwrap_or(fallback)
}

/// 非流式响应。服务端工具（联网搜索）的调用和结果块不属于 OpenAI 协议，不输出；
/// 思考内容放在 `reasoning_content`。
pub(super) fn translate_message(message: &Value, fallback_model: &str, created: i64) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for block in message["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("thinking") => reasoning.push_str(block["thinking"].as_str().unwrap_or_default()),
            Some("tool_use") => tool_calls.push(json!({
                "id": block["id"],
                "type": "function",
                "function": {"name": block["name"], "arguments": tool_arguments(&block["input"])},
            })),
            _ => {}
        }
    }
    let has_tool_calls = !tool_calls.is_empty();
    let mut reply = Map::new();
    reply.insert("role".to_string(), json!("assistant"));
    let content = if text.is_empty() && has_tool_calls {
        Value::Null
    } else {
        json!(text)
    };
    reply.insert("content".to_string(), content);
    if !reasoning.is_empty() {
        reply.insert("reasoning_content".to_string(), json!(reasoning));
    }
    if has_tool_calls {
        reply.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    reply.insert("refusal".to_string(), Value::Null);
    let mut usage = Usage::default();
    usage.merge(&message["usage"]);
    json!({
        "id": completion_id(message["id"].as_str()),
        "object": "chat.completion",
        "created": created,
        "model": model_or(message["model"].as_str(), fallback_model),
        "choices": [{
            "index": 0,
            "message": reply,
            "logprobs": null,
            "finish_reason": finish_reason(message["stop_reason"].as_str(), has_tool_calls),
        }],
        "usage": usage.to_openai(),
    })
}

pub(super) fn openai_error(error_type: &str, message: &str) -> Value {
    json!({"error": {"message": message, "type": error_type, "param": null, "code": null}})
}

/// Anthropic 错误信封 `{"type":"error","error":{...}}` 转成 OpenAI 的 `{"error":{...}}`；
/// 不是 Anthropic 错误信封时返回 `None`。
pub(super) fn openai_error_from_anthropic(value: &Value) -> Option<Value> {
    if value.get("type").and_then(Value::as_str) != Some("error") {
        return None;
    }
    let error = value.get("error")?;
    Some(openai_error(
        error["type"].as_str().unwrap_or("api_error"),
        error["message"].as_str().unwrap_or("upstream error"),
    ))
}

fn sse_data(value: &Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(value).unwrap_or_default()
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SseMessage {
    pub(super) event: Option<String>,
    pub(super) data: String,
}

/// 按行解析 SSE，兼容 `\r\n` 和跨分片的行。`\n` 不会出现在 UTF-8 多字节序列里，
/// 所以按字节切行不会切坏字符。
#[derive(Default)]
pub(super) struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: String,
}

impl SseParser {
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseMessage>, String> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        let mut start = 0;
        while let Some(offset) = self.buffer[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + offset;
            let line = &self.buffer[start..end];
            let line =
                String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
            start = end + 1;
            self.process_line(&line, &mut messages);
        }
        self.buffer.drain(..start);
        if self.buffer.len() + self.data.len() > MAX_SSE_EVENT_BYTES {
            return Err("upstream stream sent an oversized event".to_string());
        }
        Ok(messages)
    }

    /// 流结束时还没遇到空行的最后一个事件。
    pub(super) fn finish(&mut self) -> Option<SseMessage> {
        let rest = std::mem::take(&mut self.buffer);
        let mut messages = Vec::new();
        if !rest.is_empty() {
            let line =
                String::from_utf8_lossy(rest.strip_suffix(b"\r").unwrap_or(&rest)).into_owned();
            self.process_line(&line, &mut messages);
        }
        self.process_line("", &mut messages);
        messages.pop()
    }

    fn process_line(&mut self, line: &str, messages: &mut Vec<SseMessage>) {
        if line.is_empty() {
            if !self.data.is_empty() || self.event.is_some() {
                messages.push(SseMessage {
                    event: self.event.take(),
                    data: std::mem::take(&mut self.data),
                });
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            _ => {}
        }
    }
}

enum BlockKind {
    Text,
    Thinking,
    Tool {
        index: usize,
        /// `content_block_start` 自带的参数。Anthropic 规定这里是 `{}`、参数走增量；
        /// 只有一个增量都没收到时才用它，和 Anthropic SDK 的拼接规则一致。
        start_input: Option<String>,
        saw_delta: bool,
    },
    /// 服务端工具调用、搜索结果、脱敏思考等 OpenAI 没有对应的块。
    Skip,
}

/// 逐事件把 Anthropic 流翻译成 OpenAI chunk。工具调用的 `index` 只按工具调用出现的顺序编号，
/// 与 Anthropic 的内容块序号无关。
pub(super) struct StreamTranslator {
    id: String,
    model: String,
    created: i64,
    include_usage: bool,
    started: bool,
    finished: bool,
    blocks: HashMap<u64, BlockKind>,
    tool_calls: usize,
    usage: Usage,
    stop_reason: Option<String>,
}

impl StreamTranslator {
    pub(super) fn new(model: &str, created: i64, include_usage: bool) -> Self {
        Self {
            id: completion_id(None),
            model: model.to_string(),
            created,
            include_usage,
            started: false,
            finished: false,
            blocks: HashMap::new(),
            tool_calls: 0,
            usage: Usage::default(),
            stop_reason: None,
        }
    }

    pub(super) fn on_event(&mut self, event: SseMessage) -> Vec<String> {
        if self.finished || event.data.is_empty() {
            return Vec::new();
        }
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return self.fail("api_error", "upstream stream sent malformed data");
        };
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .or(event.event.as_deref())
            .unwrap_or_default();
        match kind {
            // 注释行，OpenAI 客户端会忽略，只用来保持连接。
            "ping" => vec![": keepalive\n\n".to_string()],
            "message_start" => {
                let message = &data["message"];
                if !self.started {
                    if let Some(id) = message["id"].as_str() {
                        self.id = completion_id(Some(id));
                    }
                    if let Some(model) = message["model"].as_str().filter(|model| !model.is_empty())
                    {
                        self.model = model.to_string();
                    }
                }
                self.usage.merge(&message["usage"]);
                self.ensure_started()
            }
            "content_block_start" => self.block_start(&data),
            "content_block_delta" => self.block_delta(&data),
            "content_block_stop" => self.block_stop(&data),
            "message_delta" => {
                self.usage.merge(&data["usage"]);
                if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
                Vec::new()
            }
            "message_stop" => self.complete(),
            "error" => {
                let error = &data["error"];
                self.fail(
                    error["type"].as_str().unwrap_or("api_error"),
                    error["message"].as_str().unwrap_or("upstream error"),
                )
            }
            _ => Vec::new(),
        }
    }

    /// 上游流在 `message_stop` 之前结束。
    pub(super) fn on_end(&mut self) -> Vec<String> {
        self.fail(
            "api_error",
            "upstream stream ended before the response completed",
        )
    }

    /// 以 OpenAI 错误 chunk 结束，不发 `[DONE]`，客户端据此知道这轮没有正常完成。
    pub(super) fn fail(&mut self, error_type: &str, message: &str) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        vec![sse_data(&openai_error(error_type, message))]
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>) -> String {
        let mut chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "logprobs": null,
                "finish_reason": finish_reason,
            }],
        });
        if self.include_usage {
            chunk["usage"] = Value::Null;
        }
        sse_data(&chunk)
    }

    fn ensure_started(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        vec![self.chunk(
            json!({"role": "assistant", "content": "", "refusal": null}),
            None,
        )]
    }

    fn block_start(&mut self, data: &Value) -> Vec<String> {
        let mut out = self.ensure_started();
        let block = &data["content_block"];
        let non_empty = |field: &str| {
            block[field]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        let kind = match block["type"].as_str() {
            Some("text") => {
                if let Some(text) = non_empty("text") {
                    out.push(self.chunk(json!({"content": text}), None));
                }
                BlockKind::Text
            }
            Some("thinking") => {
                if let Some(text) = non_empty("thinking") {
                    out.push(self.chunk(json!({"reasoning_content": text}), None));
                }
                BlockKind::Thinking
            }
            Some("tool_use") => {
                let index = self.tool_calls;
                self.tool_calls += 1;
                out.push(self.chunk(
                    json!({"tool_calls": [{
                        "index": index,
                        "id": block["id"],
                        "type": "function",
                        "function": {"name": block["name"], "arguments": ""},
                    }]}),
                    None,
                ));
                let start_input = block["input"]
                    .as_object()
                    .filter(|input| !input.is_empty())
                    .map(|_| tool_arguments(&block["input"]));
                BlockKind::Tool {
                    index,
                    start_input,
                    saw_delta: false,
                }
            }
            _ => BlockKind::Skip,
        };
        self.blocks
            .insert(data["index"].as_u64().unwrap_or_default(), kind);
        out
    }

    fn block_delta(&mut self, data: &Value) -> Vec<String> {
        let mut out = self.ensure_started();
        let delta = &data["delta"];
        let block = self
            .blocks
            .get_mut(&data["index"].as_u64().unwrap_or_default());
        let chunk_delta = match (delta["type"].as_str(), block) {
            (_, Some(BlockKind::Skip)) => None,
            (Some("text_delta"), _) => delta["text"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(|text| json!({"content": text})),
            (Some("thinking_delta"), _) => delta["thinking"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(|text| json!({"reasoning_content": text})),
            (
                Some("input_json_delta"),
                Some(BlockKind::Tool {
                    index, saw_delta, ..
                }),
            ) => {
                *saw_delta = true;
                let index = *index;
                delta["partial_json"]
                    .as_str()
                    .filter(|partial| !partial.is_empty())
                    .map(|partial| {
                        json!({"tool_calls": [{"index": index, "function": {"arguments": partial}}]})
                    })
            }
            _ => None,
        };
        out.extend(chunk_delta.map(|delta| self.chunk(delta, None)));
        out
    }

    fn block_stop(&mut self, data: &Value) -> Vec<String> {
        let Some(BlockKind::Tool {
            index,
            start_input: Some(arguments),
            saw_delta: false,
        }) = self
            .blocks
            .remove(&data["index"].as_u64().unwrap_or_default())
        else {
            return Vec::new();
        };
        vec![self.chunk(
            json!({"tool_calls": [{"index": index, "function": {"arguments": arguments}}]}),
            None,
        )]
    }

    fn complete(&mut self) -> Vec<String> {
        let mut out = self.ensure_started();
        self.finished = true;
        out.push(self.chunk(
            json!({}),
            Some(finish_reason(
                self.stop_reason.as_deref(),
                self.tool_calls > 0,
            )),
        ));
        if self.include_usage {
            out.push(sse_data(&json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [],
                "usage": self.usage.to_openai(),
            })));
        }
        out.push("data: [DONE]\n\n".to_string());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse(event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    /// 把 Anthropic SSE 文本按任意分片喂给解析器和翻译器，返回 OpenAI 侧的 `data:` 负载。
    fn translate_stream(body: &str, split: usize, include_usage: bool) -> Vec<String> {
        let mut parser = SseParser::default();
        let mut translator = StreamTranslator::new("requested-model", 1_700_000_000, include_usage);
        let mut out = Vec::new();
        for chunk in body.as_bytes().chunks(split) {
            for event in parser.feed(chunk).expect("parse") {
                out.extend(translator.on_event(event));
            }
        }
        if let Some(event) = parser.finish() {
            out.extend(translator.on_event(event));
        }
        out.extend(translator.on_end());
        out.concat()
            .split("\n\n")
            .filter_map(|frame| frame.strip_prefix("data: ").map(str::to_string))
            .collect()
    }

    fn tool_stream() -> String {
        [
            sse("message_start", json!({"type": "message_start", "message": {
                "id": "msg_01abc", "model": "claude-sonnet-4-5", "usage": {
                    "input_tokens": 10, "output_tokens": 1,
                    "cache_creation_input_tokens": 5, "cache_read_input_tokens": 20
                }
            }})),
            "event: ping\ndata: {\"type\": \"ping\"}\n\n".to_string(),
            sse("content_block_start", json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "thinking", "thinking": ""}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "思考中"}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "signature_delta", "signature": "sig"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"query\":\"x\"}"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 1})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 2,
                "content_block": {"type": "text", "text": ""}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 2,
                "delta": {"type": "text_delta", "text": "好的"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 2})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 3,
                "content_block": {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {}}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 3,
                "delta": {"type": "input_json_delta", "partial_json": "{\"command\":"}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 3,
                "delta": {"type": "input_json_delta", "partial_json": "\"ls\"}"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 3})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 4,
                "content_block": {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"path": "a"}}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 4})),
            sse("message_delta", json!({"type": "message_delta",
                "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                "usage": {"output_tokens": 42}})),
            sse("message_stop", json!({"type": "message_stop"})),
        ]
        .concat()
    }

    #[test]
    fn stream_translation_numbers_tool_calls_and_reports_usage() {
        // 逐字节和整段喂入结果一致，覆盖跨分片的行和多字节字符。
        let whole = translate_stream(&tool_stream(), usize::MAX, true);
        assert_eq!(translate_stream(&tool_stream(), 1, true), whole);
        assert_eq!(
            translate_stream(&tool_stream().replace('\n', "\r\n"), 7, true),
            whole
        );

        assert_eq!(whole.last().map(String::as_str), Some("[DONE]"));
        let chunks: Vec<Value> = whole[..whole.len() - 1]
            .iter()
            .map(|data| serde_json::from_str(data).expect("chunk json"))
            .collect();
        for chunk in &chunks {
            assert_eq!(chunk["id"], "chatcmpl-01abc");
            assert_eq!(chunk["model"], "claude-sonnet-4-5");
            assert_eq!(chunk["object"], "chat.completion.chunk");
            assert_eq!(chunk["created"], 1_700_000_000);
        }
        let deltas: Vec<&Value> = chunks
            .iter()
            .filter_map(|chunk| chunk["choices"].get(0).map(|choice| &choice["delta"]))
            .collect();
        assert_eq!(
            deltas,
            vec![
                &json!({"role": "assistant", "content": "", "refusal": null}),
                &json!({"reasoning_content": "思考中"}),
                &json!({"content": "好的"}),
                &json!({"tool_calls": [{"index": 0, "id": "toolu_1", "type": "function",
                    "function": {"name": "Bash", "arguments": ""}}]}),
                &json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"command\":"}}]}),
                &json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"ls\"}"}}]}),
                &json!({"tool_calls": [{"index": 1, "id": "toolu_2", "type": "function",
                    "function": {"name": "Read", "arguments": ""}}]}),
                &json!({"tool_calls": [{"index": 1, "function": {"arguments": "{\"path\":\"a\"}"}}]}),
                &json!({}),
            ]
        );
        let finish = &chunks[chunks.len() - 2];
        assert_eq!(finish["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(finish["usage"], Value::Null);
        let usage_chunk = chunks.last().unwrap();
        assert_eq!(usage_chunk["choices"], json!([]));
        assert_eq!(
            usage_chunk["usage"],
            json!({
                "prompt_tokens": 35,
                "completion_tokens": 42,
                "total_tokens": 77,
                "prompt_tokens_details": {"cached_tokens": 20}
            })
        );
    }

    #[test]
    fn stream_without_usage_option_omits_usage_fields() {
        let out = translate_stream(&tool_stream(), 64, false);
        assert_eq!(out.last().map(String::as_str), Some("[DONE]"));
        for data in &out[..out.len() - 1] {
            let chunk: Value = serde_json::from_str(data).unwrap();
            assert!(chunk.get("usage").is_none(), "{chunk}");
            assert_eq!(chunk["choices"].as_array().map(Vec::len), Some(1));
        }
    }

    #[test]
    fn stream_errors_end_without_done() {
        let error_stream = [
            sse(
                "message_start",
                json!({"type": "message_start", "message": {"id": "msg_01x"}}),
            ),
            sse(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            ),
            sse(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "partial"}}),
            ),
            sse(
                "error",
                json!({"type": "error", "error": {
                    "type": "overloaded_error", "message": "try later"
                }}),
            ),
        ]
        .concat();
        let out = translate_stream(&error_stream, 5, true);
        assert!(!out.iter().any(|data| data == "[DONE]"), "{out:?}");
        let last: Value = serde_json::from_str(out.last().unwrap()).unwrap();
        assert_eq!(
            last,
            json!({"error": {"message": "try later", "type": "overloaded_error", "param": null, "code": null}})
        );
        assert_eq!(
            out.iter().filter(|data| data.contains("\"error\"")).count(),
            1
        );

        let truncated = [
            sse(
                "message_start",
                json!({"type": "message_start", "message": {"id": "msg_01y"}}),
            ),
            sse(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            ),
        ]
        .concat();
        let out = translate_stream(&truncated, 3, false);
        assert!(!out.iter().any(|data| data == "[DONE]"));
        assert!(
            out.last()
                .unwrap()
                .contains("ended before the response completed")
        );
    }

    #[test]
    fn non_stream_message_translation() {
        let completion = translate_message(
            &json!({
                "id": "msg_01full",
                "type": "message",
                "model": "claude-sonnet-4-5",
                "content": [
                    {"type": "thinking", "thinking": "plan", "signature": "sig"},
                    {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}},
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": []},
                    {"type": "text", "text": "running"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 3, "output_tokens": 7, "cache_read_input_tokens": 100}
            }),
            "fallback",
            123,
        );
        assert_eq!(
            completion,
            json!({
                "id": "chatcmpl-01full",
                "object": "chat.completion",
                "created": 123,
                "model": "claude-sonnet-4-5",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "running",
                        "reasoning_content": "plan",
                        "tool_calls": [{
                            "id": "toolu_1",
                            "type": "function",
                            "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}
                        }],
                        "refusal": null
                    },
                    "logprobs": null,
                    "finish_reason": "tool_calls"
                }],
                "usage": {
                    "prompt_tokens": 103,
                    "completion_tokens": 7,
                    "total_tokens": 110,
                    "prompt_tokens_details": {"cached_tokens": 100}
                }
            })
        );

        let tool_only = translate_message(
            &json!({"type": "message", "content": [
                {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {}}
            ], "stop_reason": "tool_use"}),
            "fallback",
            1,
        );
        assert_eq!(tool_only["model"], "fallback");
        assert_eq!(tool_only["choices"][0]["message"]["content"], Value::Null);
        assert!(
            tool_only["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("chatcmpl-"))
        );

        for (stop_reason, expected) in [
            ("end_turn", "stop"),
            ("stop_sequence", "stop"),
            ("max_tokens", "length"),
            ("model_context_window_exceeded", "length"),
            ("refusal", "content_filter"),
        ] {
            let completion = translate_message(
                &json!({"type": "message", "content": [{"type": "text", "text": "x"}],
                    "stop_reason": stop_reason}),
                "m",
                1,
            );
            assert_eq!(
                completion["choices"][0]["finish_reason"], expected,
                "{stop_reason}"
            );
        }
    }

    #[test]
    fn anthropic_error_envelopes_are_converted() {
        assert_eq!(
            openai_error_from_anthropic(&json!({
                "type": "error",
                "error": {"type": "rate_limit_error", "message": "slow down"},
                "request_id": "req_1"
            })),
            Some(json!({"error": {
                "message": "slow down", "type": "rate_limit_error", "param": null, "code": null
            }}))
        );
        assert_eq!(openai_error_from_anthropic(&json!({"error": "x"})), None);
    }

    #[test]
    fn sse_parser_handles_comments_multiline_data_and_tail() {
        let mut parser = SseParser::default();
        let events = parser
            .feed(b": comment\nevent: a\ndata: one\ndata: two\n\nevent: b\ndata: tail")
            .unwrap();
        assert_eq!(
            events,
            vec![SseMessage {
                event: Some("a".to_string()),
                data: "one\ntwo".to_string()
            }]
        );
        assert_eq!(
            parser.finish(),
            Some(SseMessage {
                event: Some("b".to_string()),
                data: "tail".to_string()
            })
        );
        assert_eq!(parser.finish(), None);
    }
}
