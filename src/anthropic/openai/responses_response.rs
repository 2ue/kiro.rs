//! Anthropic Messages 响应翻译成 OpenAI Responses 格式：非流式 response 对象和流式事件序列。
//!
//! 输出项按 Anthropic 内容块的顺序生成：思考块 → `reasoning`，文本块 → `message`，
//! 工具调用 → `function_call`（声明为 custom 的工具 → `custom_tool_call`）。
//! 服务端工具（联网搜索）的调用和结果块不输出。

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::response::{SseMessage, Usage, model_or, openai_error, prefixed_id, tool_arguments};
use super::responses_request::ResponsesEcho;

/// 终态：`completed`，或 `incomplete` 加原因。
fn terminal_status(stop_reason: Option<&str>) -> (&'static str, Value) {
    match stop_reason {
        Some("max_tokens" | "model_context_window_exceeded") => {
            ("incomplete", json!({"reason": "max_output_tokens"}))
        }
        Some("refusal") => ("incomplete", json!({"reason": "content_filter"})),
        _ => ("completed", Value::Null),
    }
}

fn message_item(id: &str, text: &str, status: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "status": status,
        "role": "assistant",
        "content": [output_text_part(text)],
    })
}

fn output_text_part(text: &str) -> Value {
    json!({"type": "output_text", "text": text, "annotations": []})
}

fn reasoning_item(id: &str, text: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": id,
        "summary": [{"type": "summary_text", "text": text}],
    })
}

/// custom 工具的参数是 `{"input": "..."}`，还原成原始字符串；解析不了时原样给出。
fn custom_tool_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("input")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| arguments.to_string())
}

/// 请求里声明的工具：哪些是 custom 自由文本工具，哪些来自 `namespace` 工具组。
#[derive(Default)]
pub(super) struct DeclaredTools {
    pub(super) custom: HashSet<String>,
    /// 展开后的名字 → (组名, 原工具名)。
    pub(super) namespaced: HashMap<String, (String, String)>,
}

impl DeclaredTools {
    pub(super) fn new(custom: Vec<String>, namespaced: Vec<(String, String, String)>) -> Self {
        Self {
            custom: custom.into_iter().collect(),
            namespaced: namespaced
                .into_iter()
                .map(|(flat, namespace, name)| (flat, (namespace, name)))
                .collect(),
        }
    }

    fn is_custom(&self, name: &Value) -> bool {
        name.as_str().is_some_and(|name| self.custom.contains(name))
    }

    /// 工具组里的工具还原成 `{name, namespace}`。
    fn restore(&self, item: &mut Value) {
        let Some((namespace, name)) = item["name"]
            .as_str()
            .and_then(|flat| self.namespaced.get(flat))
            .cloned()
        else {
            return;
        };
        item["name"] = json!(name);
        item["namespace"] = json!(namespace);
    }
}

fn tool_call_item(
    id: &str,
    call_id: &Value,
    name: &Value,
    arguments: &str,
    custom: bool,
    status: &str,
) -> Value {
    if custom {
        json!({
            "type": "custom_tool_call",
            "id": id,
            "call_id": call_id,
            "name": name,
            "input": custom_tool_input(arguments),
            "status": status,
        })
    } else {
        json!({
            "type": "function_call",
            "id": id,
            "call_id": call_id,
            "name": name,
            "arguments": arguments,
            "status": status,
        })
    }
}

pub(super) struct ResponseShell {
    pub(super) id: String,
    pub(super) created_at: i64,
    pub(super) model: String,
    pub(super) echo: ResponsesEcho,
}

impl ResponseShell {
    fn object(
        &self,
        status: &str,
        output: Vec<Value>,
        usage: Option<Value>,
        incomplete_details: Value,
        error: Value,
    ) -> Value {
        json!({
            "id": self.id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "background": false,
            "error": error,
            "incomplete_details": incomplete_details,
            "instructions": self.echo.instructions,
            "max_output_tokens": self.echo.max_output_tokens,
            "model": self.model,
            "output": output,
            "parallel_tool_calls": self.echo.parallel_tool_calls,
            "previous_response_id": null,
            "reasoning": self.echo.reasoning,
            "store": false,
            "text": self.echo.text,
            "tool_choice": self.echo.tool_choice,
            "tools": self.echo.tools,
            "truncation": "disabled",
            "usage": usage,
            "metadata": self.echo.metadata,
        })
    }
}

/// 非流式：整条 Anthropic 消息转成一个 response 对象。
pub(super) fn translate_message(
    message: &Value,
    mut shell: ResponseShell,
    declared: &DeclaredTools,
) -> Value {
    shell.model = model_or(message["model"].as_str(), &shell.model).to_string();
    if let Some(id) = message["id"].as_str() {
        shell.id = prefixed_id("resp_", Some(id));
    }
    let mut output = Vec::new();
    for block in message["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => {
                let text = block["text"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    output.push(message_item(&prefixed_id("msg_", None), text, "completed"));
                }
            }
            Some("thinking") => {
                let text = block["thinking"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    output.push(reasoning_item(&prefixed_id("rs_", None), text));
                }
            }
            Some("tool_use") => {
                let custom = declared.is_custom(&block["name"]);
                let mut item = tool_call_item(
                    &prefixed_id(if custom { "ctc_" } else { "fc_" }, None),
                    &block["id"],
                    &block["name"],
                    &tool_arguments(&block["input"]),
                    custom,
                    "completed",
                );
                declared.restore(&mut item);
                output.push(item);
            }
            _ => {}
        }
    }
    let mut usage = Usage::default();
    usage.merge(&message["usage"]);
    let (status, incomplete_details) = terminal_status(message["stop_reason"].as_str());
    shell.object(
        status,
        output,
        Some(usage.to_responses()),
        incomplete_details,
        Value::Null,
    )
}

enum OpenItem {
    Message {
        id: String,
        output_index: usize,
        text: String,
    },
    Reasoning {
        id: String,
        output_index: usize,
        text: String,
    },
    Tool {
        id: String,
        output_index: usize,
        call_id: Value,
        name: Value,
        arguments: String,
        /// `content_block_start` 自带的参数；一个增量都没收到时才用它。
        start_input: Option<String>,
        custom: bool,
    },
}

enum BlockKind {
    Text,
    Thinking,
    Tool,
    Skip,
}

/// 逐事件把 Anthropic 流翻译成 Responses 事件。每个事件都带递增的 `sequence_number`。
pub(super) struct ResponsesStreamTranslator {
    shell: ResponseShell,
    declared: DeclaredTools,
    sequence: u64,
    started: bool,
    finished: bool,
    blocks: HashMap<u64, BlockKind>,
    open: HashMap<u64, OpenItem>,
    output: Vec<Option<Value>>,
    usage: Usage,
    stop_reason: Option<String>,
}

impl ResponsesStreamTranslator {
    pub(super) fn new(shell: ResponseShell, declared: DeclaredTools) -> Self {
        Self {
            shell,
            declared,
            sequence: 0,
            started: false,
            finished: false,
            blocks: HashMap::new(),
            open: HashMap::new(),
            output: Vec::new(),
            usage: Usage::default(),
            stop_reason: None,
        }
    }

    fn event(&mut self, event_type: &str, mut data: Value) -> String {
        data["type"] = json!(event_type);
        data["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        format!(
            "event: {event_type}\ndata: {}\n\n",
            serde_json::to_string(&data).unwrap_or_default()
        )
    }

    fn snapshot_output(&self) -> Vec<Value> {
        self.output.iter().flatten().cloned().collect()
    }

    fn ensure_started(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let response = self
            .shell
            .object("in_progress", Vec::new(), None, Value::Null, Value::Null);
        vec![
            self.event("response.created", json!({"response": response})),
            self.event("response.in_progress", json!({"response": response})),
        ]
    }

    fn reserve_output(&mut self) -> usize {
        self.output.push(None);
        self.output.len() - 1
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
            "ping" => vec![": keepalive\n\n".to_string()],
            "message_start" => {
                let message = &data["message"];
                if !self.started {
                    if let Some(id) = message["id"].as_str() {
                        self.shell.id = prefixed_id("resp_", Some(id));
                    }
                    if let Some(model) = message["model"].as_str().filter(|model| !model.is_empty())
                    {
                        self.shell.model = model.to_string();
                    }
                }
                self.usage.merge(&message["usage"]);
                self.ensure_started()
            }
            "content_block_start" => self.block_start(&data),
            "content_block_delta" => self.block_delta(&data),
            "content_block_stop" => self.block_stop(data["index"].as_u64().unwrap_or_default()),
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

    pub(super) fn on_end(&mut self) -> Vec<String> {
        self.fail(
            "api_error",
            "upstream stream ended before the response completed",
        )
    }

    /// 以 `response.failed` 结束，带上已经输出的内容。
    pub(super) fn fail(&mut self, error_type: &str, message: &str) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = self.ensure_started();
        self.finished = true;
        let response = self.shell.object(
            "failed",
            self.snapshot_output(),
            Some(self.usage.to_responses()),
            Value::Null,
            openai_error(error_type, message)["error"].clone(),
        );
        out.push(self.event("response.failed", json!({"response": response})));
        out
    }

    fn block_start(&mut self, data: &Value) -> Vec<String> {
        let mut out = self.ensure_started();
        let index = data["index"].as_u64().unwrap_or_default();
        let block = &data["content_block"];
        let kind = match block["type"].as_str() {
            Some("text") => BlockKind::Text,
            Some("thinking") => BlockKind::Thinking,
            Some("tool_use") => {
                let custom = self.declared.is_custom(&block["name"]);
                let output_index = self.reserve_output();
                let id = prefixed_id(if custom { "ctc_" } else { "fc_" }, None);
                let mut item =
                    tool_call_item(&id, &block["id"], &block["name"], "", custom, "in_progress");
                self.declared.restore(&mut item);
                out.push(self.event(
                    "response.output_item.added",
                    json!({"output_index": output_index, "item": item}),
                ));
                let start_input = block["input"]
                    .as_object()
                    .filter(|input| !input.is_empty())
                    .map(|_| tool_arguments(&block["input"]));
                self.open.insert(
                    index,
                    OpenItem::Tool {
                        id,
                        output_index,
                        call_id: block["id"].clone(),
                        name: block["name"].clone(),
                        arguments: String::new(),
                        start_input,
                        custom,
                    },
                );
                BlockKind::Tool
            }
            _ => BlockKind::Skip,
        };
        self.blocks.insert(index, kind);
        let initial = match self.blocks.get(&index) {
            Some(BlockKind::Text) => block["text"].as_str(),
            Some(BlockKind::Thinking) => block["thinking"].as_str(),
            _ => None,
        }
        .filter(|text| !text.is_empty())
        .map(str::to_string);
        if let Some(text) = initial {
            out.extend(self.append_text(index, &text));
        }
        out
    }

    /// 文本和思考在收到第一段非空内容时才开一个输出项，空块不产生输出项。
    fn append_text(&mut self, index: u64, delta: &str) -> Vec<String> {
        let mut out = Vec::new();
        if !self.open.contains_key(&index) {
            let output_index = self.reserve_output();
            match self.blocks.get(&index) {
                Some(BlockKind::Text) => {
                    let id = prefixed_id("msg_", None);
                    let mut item = message_item(&id, "", "in_progress");
                    item["content"] = json!([]);
                    out.push(self.event(
                        "response.output_item.added",
                        json!({"output_index": output_index, "item": item}),
                    ));
                    out.push(self.event(
                        "response.content_part.added",
                        json!({
                            "item_id": id,
                            "output_index": output_index,
                            "content_index": 0,
                            "part": output_text_part(""),
                        }),
                    ));
                    self.open.insert(
                        index,
                        OpenItem::Message {
                            id,
                            output_index,
                            text: String::new(),
                        },
                    );
                }
                Some(BlockKind::Thinking) => {
                    let id = prefixed_id("rs_", None);
                    out.push(self.event(
                        "response.output_item.added",
                        json!({
                            "output_index": output_index,
                            "item": {"type": "reasoning", "id": id, "summary": []},
                        }),
                    ));
                    out.push(self.event(
                        "response.reasoning_summary_part.added",
                        json!({
                            "item_id": id,
                            "output_index": output_index,
                            "summary_index": 0,
                            "part": {"type": "summary_text", "text": ""},
                        }),
                    ));
                    self.open.insert(
                        index,
                        OpenItem::Reasoning {
                            id,
                            output_index,
                            text: String::new(),
                        },
                    );
                }
                _ => return out,
            }
        }
        let event = match self.open.get_mut(&index) {
            Some(OpenItem::Message {
                id,
                output_index,
                text,
            }) => {
                text.push_str(delta);
                Some((
                    "response.output_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": 0,
                        "delta": delta,
                    }),
                ))
            }
            Some(OpenItem::Reasoning {
                id,
                output_index,
                text,
            }) => {
                text.push_str(delta);
                Some((
                    "response.reasoning_summary_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "delta": delta,
                    }),
                ))
            }
            _ => None,
        };
        if let Some((event_type, data)) = event {
            out.push(self.event(event_type, data));
        }
        out
    }

    fn block_delta(&mut self, data: &Value) -> Vec<String> {
        let mut out = self.ensure_started();
        let index = data["index"].as_u64().unwrap_or_default();
        let delta = &data["delta"];
        match (delta["type"].as_str(), self.blocks.get(&index)) {
            (Some("text_delta"), Some(BlockKind::Text)) => {
                if let Some(text) = delta["text"].as_str().filter(|text| !text.is_empty()) {
                    out.extend(self.append_text(index, text));
                }
            }
            (Some("thinking_delta"), Some(BlockKind::Thinking)) => {
                if let Some(text) = delta["thinking"].as_str().filter(|text| !text.is_empty()) {
                    out.extend(self.append_text(index, text));
                }
            }
            (Some("input_json_delta"), Some(BlockKind::Tool)) => {
                let partial = delta["partial_json"].as_str().unwrap_or_default();
                let event = match self.open.get_mut(&index) {
                    Some(OpenItem::Tool {
                        id,
                        output_index,
                        arguments,
                        start_input,
                        custom,
                        ..
                    }) => {
                        *start_input = None;
                        arguments.push_str(partial);
                        // custom 工具的参数是包装过的 JSON，收完整再还原，不边收边发。
                        (!*custom && !partial.is_empty()).then(|| {
                            json!({"item_id": id, "output_index": output_index, "delta": partial})
                        })
                    }
                    _ => None,
                };
                if let Some(event) = event {
                    out.push(self.event("response.function_call_arguments.delta", event));
                }
            }
            _ => {}
        }
        out
    }

    fn block_stop(&mut self, index: u64) -> Vec<String> {
        self.blocks.remove(&index);
        let Some(item) = self.open.remove(&index) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match item {
            OpenItem::Message {
                id,
                output_index,
                text,
            } => {
                out.push(self.event(
                    "response.output_text.done",
                    json!({"item_id": id, "output_index": output_index, "content_index": 0, "text": text}),
                ));
                out.push(self.event(
                    "response.content_part.done",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": output_text_part(&text),
                    }),
                ));
                let item = message_item(&id, &text, "completed");
                out.push(self.finish_item(output_index, item));
            }
            OpenItem::Reasoning {
                id,
                output_index,
                text,
            } => {
                out.push(self.event(
                    "response.reasoning_summary_text.done",
                    json!({"item_id": id, "output_index": output_index, "summary_index": 0, "text": text}),
                ));
                out.push(self.event(
                    "response.reasoning_summary_part.done",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "part": {"type": "summary_text", "text": text},
                    }),
                ));
                let item = reasoning_item(&id, &text);
                out.push(self.finish_item(output_index, item));
            }
            OpenItem::Tool {
                id,
                output_index,
                call_id,
                name,
                mut arguments,
                start_input,
                custom,
            } => {
                let pending = match start_input {
                    Some(start_input) => start_input,
                    None if arguments.is_empty() => "{}".to_string(),
                    None => String::new(),
                };
                arguments.push_str(&pending);
                if custom {
                    let input = custom_tool_input(&arguments);
                    out.push(self.event(
                        "response.custom_tool_call_input.delta",
                        json!({"item_id": id, "output_index": output_index, "delta": input}),
                    ));
                    out.push(self.event(
                        "response.custom_tool_call_input.done",
                        json!({"item_id": id, "output_index": output_index, "input": input}),
                    ));
                } else {
                    if !pending.is_empty() {
                        out.push(self.event(
                            "response.function_call_arguments.delta",
                            json!({"item_id": id, "output_index": output_index, "delta": pending}),
                        ));
                    }
                    out.push(self.event(
                        "response.function_call_arguments.done",
                        json!({"item_id": id, "output_index": output_index, "arguments": arguments}),
                    ));
                }
                let mut item =
                    tool_call_item(&id, &call_id, &name, &arguments, custom, "completed");
                self.declared.restore(&mut item);
                out.push(self.finish_item(output_index, item));
            }
        }
        out
    }

    fn finish_item(&mut self, output_index: usize, item: Value) -> String {
        if let Some(slot) = self.output.get_mut(output_index) {
            *slot = Some(item.clone());
        }
        self.event(
            "response.output_item.done",
            json!({"output_index": output_index, "item": item}),
        )
    }

    fn complete(&mut self) -> Vec<String> {
        let mut out = self.ensure_started();
        let open: Vec<u64> = self.open.keys().copied().collect();
        for index in open {
            out.extend(self.block_stop(index));
        }
        self.finished = true;
        let (status, incomplete_details) = terminal_status(self.stop_reason.as_deref());
        let response = self.shell.object(
            status,
            self.snapshot_output(),
            Some(self.usage.to_responses()),
            incomplete_details,
            Value::Null,
        );
        let event_type = if status == "completed" {
            "response.completed"
        } else {
            "response.incomplete"
        };
        out.push(self.event(event_type, json!({"response": response})));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::response::SseParser;
    use super::*;

    fn shell() -> ResponseShell {
        ResponseShell {
            id: "resp_initial".to_string(),
            created_at: 1_700_000_000,
            model: "requested-model".to_string(),
            echo: ResponsesEcho {
                instructions: Value::Null,
                max_output_tokens: Value::Null,
                parallel_tool_calls: true,
                tool_choice: json!("auto"),
                tools: json!([]),
                text: json!({"format": {"type": "text"}}),
                reasoning: json!({"effort": null, "summary": null}),
                metadata: json!({}),
            },
        }
    }

    fn sse(event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    /// `(event type, data)` pairs in order.
    fn translate_stream(body: &str, split: usize, custom: &[&str]) -> Vec<(String, Value)> {
        let mut parser = SseParser::default();
        let mut translator = ResponsesStreamTranslator::new(
            shell(),
            DeclaredTools::new(
                custom.iter().map(|name| name.to_string()).collect(),
                Vec::new(),
            ),
        );
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
            .filter_map(|frame| {
                let mut lines = frame.lines();
                let event = lines.next()?.strip_prefix("event: ")?.to_string();
                let data = lines.next()?.strip_prefix("data: ")?;
                Some((event, serde_json::from_str(data).expect("event json")))
            })
            .collect()
    }

    fn tool_stream() -> String {
        [
            sse("message_start", json!({"type": "message_start", "message": {
                "id": "msg_01abc", "model": "claude-sonnet-4-5",
                "usage": {"input_tokens": 10, "output_tokens": 1, "cache_read_input_tokens": 20}
            }})),
            "event: ping\ndata: {\"type\": \"ping\"}\n\n".to_string(),
            sse("content_block_start", json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "thinking", "thinking": ""}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "思考"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "text", "text": ""}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 1})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 2,
                "content_block": {"type": "text", "text": ""}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 2,
                "delta": {"type": "text_delta", "text": "好的"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 2})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 3,
                "content_block": {"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {}}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 3,
                "delta": {"type": "input_json_delta", "partial_json": "{\"command\":"}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 3,
                "delta": {"type": "input_json_delta", "partial_json": "[\"ls\"]}"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 3})),
            sse("content_block_start", json!({"type": "content_block_start", "index": 4,
                "content_block": {"type": "tool_use", "id": "toolu_2", "name": "apply_patch", "input": {}}})),
            sse("content_block_delta", json!({"type": "content_block_delta", "index": 4,
                "delta": {"type": "input_json_delta", "partial_json": "{\"input\":\"*** Begin\"}"}})),
            sse("content_block_stop", json!({"type": "content_block_stop", "index": 4})),
            sse("message_delta", json!({"type": "message_delta",
                "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}})),
            sse("message_stop", json!({"type": "message_stop"})),
        ]
        .concat()
    }

    #[test]
    fn stream_events_follow_the_responses_sequence() {
        let whole = translate_stream(&tool_stream(), usize::MAX, &["apply_patch"]);
        assert_eq!(
            translate_stream(&tool_stream(), 1, &["apply_patch"]).len(),
            whole.len()
        );
        for (sequence, (_, data)) in whole.iter().enumerate() {
            assert_eq!(data["sequence_number"], sequence as u64, "{data}");
        }
        let types: Vec<&str> = whole.iter().map(|(event, _)| event.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.reasoning_summary_part.added",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.done",
                "response.reasoning_summary_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.output_item.added",
                "response.custom_tool_call_input.delta",
                "response.custom_tool_call_input.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        for (event, data) in &whole {
            assert_eq!(data["type"], event.as_str());
        }
        let output_indices: Vec<u64> = whole
            .iter()
            .filter(|(event, _)| event == "response.output_item.done")
            .map(|(_, data)| data["output_index"].as_u64().unwrap())
            .collect();
        assert_eq!(
            output_indices,
            vec![0, 1, 2, 3],
            "the empty text block takes no slot"
        );

        let completed = &whole.last().unwrap().1["response"];
        assert_eq!(completed["id"], "resp_01abc");
        assert_eq!(completed["model"], "claude-sonnet-4-5");
        assert_eq!(completed["status"], "completed");
        let output = completed["output"].as_array().unwrap();
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "思考");
        assert_eq!(output[1]["content"][0]["text"], "好的");
        assert_eq!(output[2]["type"], "function_call");
        assert_eq!(output[2]["call_id"], "toolu_1");
        assert_eq!(output[2]["arguments"], "{\"command\":[\"ls\"]}");
        assert_eq!(output[3]["type"], "custom_tool_call");
        assert_eq!(output[3]["input"], "*** Begin");
        assert_eq!(
            completed["usage"],
            json!({
                "input_tokens": 30,
                "input_tokens_details": {"cached_tokens": 20},
                "output_tokens": 30,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 60
            })
        );
    }

    #[test]
    fn stream_tool_input_from_block_start_and_failures() {
        let start_only = [
            sse(
                "message_start",
                json!({"type": "message_start", "message": {"id": "msg_01s"}}),
            ),
            sse(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_s", "name": "read",
                    "input": {"path": "a"}}}),
            ),
            sse(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            sse(
                "message_delta",
                json!({"type": "message_delta",
                "delta": {"stop_reason": "max_tokens"}, "usage": {"output_tokens": 3}}),
            ),
            sse("message_stop", json!({"type": "message_stop"})),
        ]
        .concat();
        let out = translate_stream(&start_only, 9, &[]);
        let done = out
            .iter()
            .find(|(event, _)| event == "response.function_call_arguments.done")
            .unwrap();
        assert_eq!(done.1["arguments"], "{\"path\":\"a\"}");
        let last = &out.last().unwrap();
        assert_eq!(last.0, "response.incomplete");
        assert_eq!(
            last.1["response"]["incomplete_details"],
            json!({"reason": "max_output_tokens"})
        );

        let failing = [
            sse(
                "message_start",
                json!({"type": "message_start", "message": {"id": "msg_01f"}}),
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
                json!({"type": "error", "error": {"type": "overloaded_error", "message": "busy"}}),
            ),
        ]
        .concat();
        let out = translate_stream(&failing, 4, &[]);
        let last = out.last().unwrap();
        assert_eq!(last.0, "response.failed");
        assert_eq!(last.1["response"]["status"], "failed");
        assert_eq!(last.1["response"]["error"]["type"], "overloaded_error");
        assert_eq!(last.1["response"]["error"]["message"], "busy");
        assert_eq!(
            out.iter()
                .filter(|(event, _)| event == "response.failed")
                .count(),
            1
        );

        let truncated = sse(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_01t"}}),
        );
        let out = translate_stream(&truncated, 5, &[]);
        assert_eq!(out.last().unwrap().0, "response.failed");
    }

    #[test]
    fn non_stream_message_becomes_a_response_object() {
        let response = translate_message(
            &json!({
                "id": "msg_01full",
                "type": "message",
                "model": "claude-sonnet-4-5",
                "content": [
                    {"type": "thinking", "thinking": "plan", "signature": "sig"},
                    {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}},
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": []},
                    {"type": "text", "text": "running"},
                    {"type": "tool_use", "id": "toolu_1", "name": "shell", "input": {"command": ["ls"]}},
                    {"type": "tool_use", "id": "toolu_2", "name": "apply_patch", "input": {"input": "diff"}},
                    {"type": "tool_use", "id": "toolu_3", "name": "fs__read", "input": {"path": "a"}}
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 3, "output_tokens": 7, "cache_read_input_tokens": 100}
            }),
            shell(),
            &DeclaredTools::new(
                vec!["apply_patch".to_string()],
                vec![("fs__read".to_string(), "fs".to_string(), "read".to_string())],
            ),
        );
        assert_eq!(response["id"], "resp_01full");
        assert_eq!(response["object"], "response");
        assert_eq!(response["status"], "completed");
        assert_eq!(response["incomplete_details"], Value::Null);
        assert_eq!(response["model"], "claude-sonnet-4-5");
        assert_eq!(response["store"], false);
        let output = response["output"].as_array().unwrap();
        let types: Vec<&str> = output
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            vec![
                "reasoning",
                "message",
                "function_call",
                "custom_tool_call",
                "function_call"
            ]
        );
        assert_eq!(output[4]["name"], "read");
        assert_eq!(output[4]["namespace"], "fs");
        assert!(output[2].get("namespace").is_none());
        assert_eq!(output[1]["content"][0]["text"], "running");
        assert_eq!(output[2]["arguments"], "{\"command\":[\"ls\"]}");
        assert_eq!(output[3]["input"], "diff");
        assert_eq!(response["usage"]["input_tokens"], 103);
        assert_eq!(
            response["usage"]["input_tokens_details"]["cached_tokens"],
            100
        );

        let refused = translate_message(
            &json!({"type": "message", "content": [{"type": "text", "text": "no"}],
                "stop_reason": "refusal"}),
            shell(),
            &DeclaredTools::default(),
        );
        assert_eq!(refused["status"], "incomplete");
        assert_eq!(
            refused["incomplete_details"],
            json!({"reason": "content_filter"})
        );
        assert_eq!(refused["id"], "resp_initial");
        assert_eq!(refused["model"], "requested-model");
    }
}
