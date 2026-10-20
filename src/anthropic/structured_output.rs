//! Anthropic structured outputs (`output_config.format`) on the local Kiro path.
//!
//! Kiro has no JSON-schema output mode, so the schema is offered to the model as an internal tool
//! whose input is the final answer. The response side turns that tool call back into one plain
//! `text` block holding the JSON, with `stop_reason: end_turn`, the shape the official API returns.
//!
//! - Without callable client tools the internal tool is forced (`tool_choice: tool`).
//! - With client tools the model must either call one of them or give the structured answer
//!   (`auto` / `any` become `any`), matching the official behavior where a tool call may come
//!   first and any final answer conforms to the schema. A client `tool_choice: tool` is kept.
//! - When the answer is the structured call, text written before it is dropped so the text
//!   content parses as JSON; when a client tool is called, that text is kept.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::stream::SseEvent;
use super::types::{MessagesRequest, Tool};

const TOOL_NAME: &str = "structured_output";
const TOOL_DESCRIPTION: &str = "Return the final answer to the user. The input of this tool IS the final response: it must be one JSON object that satisfies the schema exactly, with no other text. Call it once, when you are ready to answer.";

/// What the response side needs to restore the structured answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructuredOutputTarget {
    /// Client-facing name of the internal tool.
    pub(crate) tool_name: String,
    /// The internal tool was forced because no client tool can be called this turn.
    pub(crate) forced: bool,
}

/// Returns the request to convert and the response target when the request asks for structured
/// output. `None` keeps the request unchanged (no `format`, or a format this path cannot serve).
pub(crate) fn plan(payload: &MessagesRequest) -> Option<(MessagesRequest, StructuredOutputTarget)> {
    // `output_config.format` wins; the deprecated top-level `output_format` is the fallback.
    let format = payload
        .output_config
        .as_ref()
        .and_then(|config| config.format.as_ref())
        .or(payload.output_format.as_ref())?;
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        tracing::debug!("output_config.format without json_schema type is ignored");
        return None;
    }
    let Some(schema) = format.get("schema").and_then(Value::as_object) else {
        tracing::debug!("output_config.format.schema is not an object; ignored");
        return None;
    };

    let client_tools: Vec<&Tool> = payload
        .tools
        .iter()
        .flatten()
        .filter(|tool| tool.tool_type.is_none() || tool.input_schema.contains_key("type"))
        .collect();
    let client_choice = payload
        .tool_choice
        .as_ref()
        .and_then(|choice| choice.get("type"))
        .and_then(Value::as_str);
    let forced = client_tools.is_empty() || client_choice == Some("none");
    let tool_name = unused_tool_name(payload);

    let mut request = payload.clone();
    request.tools.get_or_insert_with(Vec::new).push(Tool {
        tool_type: None,
        name: tool_name.clone(),
        description: TOOL_DESCRIPTION.to_string(),
        input_schema: schema
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<HashMap<_, _>>(),
        max_uses: None,
        allowed_domains: None,
        blocked_domains: None,
        cache_control: None,
    });
    if forced {
        request.tool_choice = Some(json!({"type": "tool", "name": tool_name}));
    } else if client_choice != Some("tool") {
        // A final answer must be the structured call; keep disable_parallel_tool_use.
        let mut choice = payload
            .tool_choice
            .as_ref()
            .filter(|choice| choice.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        choice["type"] = json!("any");
        request.tool_choice = Some(choice);
    }
    Some((request, StructuredOutputTarget { tool_name, forced }))
}

fn unused_tool_name(payload: &MessagesRequest) -> String {
    let taken = |name: &str| {
        payload
            .tools
            .iter()
            .flatten()
            .any(|tool| tool.name.eq_ignore_ascii_case(name))
    };
    if !taken(TOOL_NAME) {
        return TOOL_NAME.to_string();
    }
    (1..)
        .map(|n| format!("{TOOL_NAME}_{n}"))
        .find(|name| !taken(name))
        .expect("an unused name exists")
}

fn is_target_tool_block(block: &Value, target: &StructuredOutputTarget) -> bool {
    block.get("type").and_then(Value::as_str) == Some("tool_use")
        && block.get("name").and_then(Value::as_str) == Some(target.tool_name.as_str())
}

/// Rewrites a complete non-streaming content array. Returns the new stop reason when the
/// structured answer replaced the internal tool call.
pub(crate) fn rewrite_content(
    content: &mut Vec<Value>,
    target: &StructuredOutputTarget,
) -> Option<&'static str> {
    let position = content
        .iter()
        .position(|block| is_target_tool_block(block, target))?;
    let input = content[position].get("input").cloned().unwrap_or(json!({}));
    let text = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
    let has_client_tool_use = content.iter().any(|block| {
        block.get("type").and_then(Value::as_str) == Some("tool_use")
            && !is_target_tool_block(block, target)
    });
    content[position] = json!({"type": "text", "text": text});

    let mut index = 0;
    content.retain(|block| {
        let is_text = block.get("type").and_then(Value::as_str) == Some("text");
        let keep = index == position
            || !(is_target_tool_block(block, target) || (!has_client_tool_use && is_text));
        index += 1;
        keep
    });
    (!has_client_tool_use).then_some("end_turn")
}

/// Streaming counterpart of [`rewrite_content`].
///
/// The internal tool block becomes a text block whose deltas are the tool input JSON. Text blocks
/// are held back until the stream shows what follows them: they are dropped when the structured
/// call arrives and released when a client tool call or the end of the message does. Block indexes are renumbered so the
/// client always sees contiguous indexes.
#[derive(Debug)]
pub(crate) struct StructuredOutputStreamRewriter {
    target: StructuredOutputTarget,
    /// Upstream index → downstream index for blocks that are forwarded.
    index_map: HashMap<i64, i64>,
    next_index: i64,
    /// Upstream index of the internal tool block, once started.
    structured_index: Option<i64>,
    structured_seen: bool,
    client_tool_seen: bool,
    /// Held text blocks: upstream index and their events, in order.
    held: Vec<(i64, Vec<SseEvent>)>,
    held_indexes: Vec<i64>,
}

impl StructuredOutputStreamRewriter {
    pub(crate) fn new(target: StructuredOutputTarget) -> Self {
        Self {
            target,
            index_map: HashMap::new(),
            next_index: 0,
            structured_index: None,
            structured_seen: false,
            client_tool_seen: false,
            held: Vec::new(),
            held_indexes: Vec::new(),
        }
    }

    /// Whether the answer was delivered through the internal tool and no client tool was called.
    pub(crate) fn replaced_stop_reason(&self) -> bool {
        self.structured_seen && !self.client_tool_seen
    }

    pub(crate) fn apply(&mut self, events: Vec<SseEvent>) -> Vec<SseEvent> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            self.push(event, &mut out);
        }
        out
    }

    fn push(&mut self, mut event: SseEvent, out: &mut Vec<SseEvent>) {
        let index = event.data.get("index").and_then(Value::as_i64);
        match event.event.as_str() {
            "content_block_start" => {
                let Some(index) = index else {
                    out.push(event);
                    return;
                };
                let block = &event.data["content_block"];
                if is_target_tool_block(block, &self.target) && self.structured_index.is_none() {
                    self.structured_index = Some(index);
                    self.structured_seen = true;
                    // Preamble text is not part of a structured answer.
                    self.held.clear();
                    self.held_indexes.clear();
                    event.data["content_block"] = json!({"type": "text", "text": ""});
                    self.forward(index, event, out);
                    return;
                }
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    if is_target_tool_block(block, &self.target) {
                        // A second structured call carries nothing new; drop it.
                        self.held_indexes.push(index);
                        return;
                    }
                    self.client_tool_seen = true;
                }
                if !self.structured_seen
                    && !self.client_tool_seen
                    && block.get("type").and_then(Value::as_str) == Some("text")
                {
                    self.held_indexes.push(index);
                    self.held.push((index, vec![event]));
                    return;
                }
                self.release_held(out);
                self.forward(index, event, out);
            }
            "content_block_delta" | "content_block_stop" => {
                let Some(index) = index else {
                    out.push(event);
                    return;
                };
                if self.structured_index == Some(index) && event.event == "content_block_delta" {
                    let partial = event.data["delta"]
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if partial.is_empty() {
                        return;
                    }
                    event.data["delta"] = json!({"type": "text_delta", "text": partial});
                }
                if let Some((_, held)) = self.held.iter_mut().find(|(idx, _)| *idx == index) {
                    held.push(event);
                    return;
                }
                if self.held_indexes.contains(&index) {
                    // Dropped preamble or duplicate structured call.
                    return;
                }
                self.forward(index, event, out);
            }
            "message_delta" => {
                self.release_held(out);
                if self.replaced_stop_reason()
                    && event.data["delta"]
                        .get("stop_reason")
                        .and_then(Value::as_str)
                        == Some("tool_use")
                {
                    event.data["delta"]["stop_reason"] = json!("end_turn");
                }
                out.push(event);
            }
            "error" | "message_stop" => {
                self.release_held(out);
                out.push(event);
            }
            _ => out.push(event),
        }
    }

    fn forward(&mut self, upstream_index: i64, mut event: SseEvent, out: &mut Vec<SseEvent>) {
        let next_index = &mut self.next_index;
        let downstream = *self.index_map.entry(upstream_index).or_insert_with(|| {
            let index = *next_index;
            *next_index += 1;
            index
        });
        event.data["index"] = json!(downstream);
        out.push(event);
    }

    /// Releases held text blocks once it is known the structured call did not replace them.
    fn release_held(&mut self, out: &mut Vec<SseEvent>) {
        for (index, events) in std::mem::take(&mut self.held) {
            self.held_indexes.retain(|held| *held != index);
            for event in events {
                self.forward(index, event, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: Value) -> MessagesRequest {
        serde_json::from_value(body).expect("request")
    }

    fn schema_request(tools: Option<Value>) -> MessagesRequest {
        let mut body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Generate a fictional person."}],
            "output_config": {"format": {"type": "json_schema", "schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
                "required": ["name", "age"],
                "additionalProperties": false
            }}}
        });
        if let Some(tools) = tools {
            body["tools"] = tools;
        }
        request(body)
    }

    fn event(name: &str, data: Value) -> SseEvent {
        SseEvent::new(name, data)
    }

    #[test]
    fn plan_forces_internal_tool_without_client_tools() {
        let (converted, target) = plan(&schema_request(None)).expect("structured plan");
        assert!(target.forced);
        assert_eq!(target.tool_name, "structured_output");
        let tools = converted.tools.expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].input_schema["type"], "object");
        assert_eq!(
            converted.tool_choice,
            Some(json!({"type": "tool", "name": "structured_output"}))
        );
    }

    #[test]
    fn plan_lets_the_model_choose_when_client_tools_exist() {
        let tools = json!([{
            "name": "structured_output",
            "description": "client tool with the same name",
            "input_schema": {"type": "object", "properties": {}}
        }]);
        let original = schema_request(Some(tools));
        let (converted, target) = plan(&original).expect("structured plan");
        assert!(!target.forced);
        assert_eq!(target.tool_name, "structured_output_1");
        assert_eq!(converted.tools.expect("tools").len(), 2);
        assert_eq!(converted.tool_choice, Some(json!({"type": "any"})));

        let mut named = original.clone();
        named.tool_choice = Some(json!({"type": "tool", "name": "structured_output"}));
        assert_eq!(plan(&named).unwrap().0.tool_choice, named.tool_choice);
        let mut none = original.clone();
        none.tool_choice = Some(json!({"type": "none"}));
        let (converted, target) = plan(&none).unwrap();
        assert!(target.forced);
        assert_eq!(
            converted.tool_choice,
            Some(json!({"type": "tool", "name": "structured_output_1"}))
        );
        let mut parallel = original.clone();
        parallel.tool_choice = Some(json!({"type": "auto", "disable_parallel_tool_use": true}));
        assert_eq!(
            plan(&parallel).unwrap().0.tool_choice,
            Some(json!({"type": "any", "disable_parallel_tool_use": true}))
        );
    }

    #[test]
    fn plan_accepts_the_deprecated_top_level_output_format() {
        let mut req = schema_request(None);
        let format = req.output_config.take().unwrap().format;
        req.output_format = format;
        let (converted, target) = plan(&req).expect("structured plan");
        assert!(target.forced);
        assert_eq!(converted.tools.expect("tools")[0].name, "structured_output");
    }

    #[test]
    fn plan_ignores_requests_without_json_schema_format() {
        let mut req = schema_request(None);
        req.output_config.as_mut().unwrap().format = Some(json!({"type": "text"}));
        assert!(plan(&req).is_none());
        req.output_config.as_mut().unwrap().format = None;
        assert!(plan(&req).is_none());
    }

    #[test]
    fn non_stream_content_becomes_plain_json_text() {
        let target = StructuredOutputTarget {
            tool_name: "structured_output".to_string(),
            forced: true,
        };
        let mut content = vec![
            json!({"type": "thinking", "thinking": "plan", "signature": ""}),
            json!({"type": "text", "text": "Here is the person:"}),
            json!({"type": "tool_use", "id": "toolu_01a", "name": "structured_output",
                   "input": {"name": "Ada", "age": 36}}),
        ];
        assert_eq!(rewrite_content(&mut content, &target), Some("end_turn"));
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[1]["type"], "text");
        let parsed: Value = serde_json::from_str(content[1]["text"].as_str().unwrap()).unwrap();
        assert_eq!(parsed, json!({"name": "Ada", "age": 36}));
    }

    #[test]
    fn non_stream_choice_mode_drops_preamble_when_the_answer_is_structured() {
        let target = StructuredOutputTarget {
            tool_name: "structured_output".to_string(),
            forced: false,
        };
        let mut content = vec![
            json!({"type": "text", "text": "Here you go:"}),
            json!({"type": "tool_use", "id": "toolu_01d", "name": "structured_output",
                   "input": {"name": "Ada"}}),
        ];
        assert_eq!(rewrite_content(&mut content, &target), Some("end_turn"));
        assert_eq!(
            content,
            vec![json!({"type": "text", "text": "{\"name\":\"Ada\"}"})]
        );
    }

    #[test]
    fn non_stream_keeps_client_tool_calls_in_choice_mode() {
        let target = StructuredOutputTarget {
            tool_name: "structured_output".to_string(),
            forced: false,
        };
        let mut content = vec![
            json!({"type": "text", "text": "Checking first."}),
            json!({"type": "tool_use", "id": "toolu_01b", "name": "get_weather", "input": {}}),
        ];
        assert_eq!(rewrite_content(&mut content, &target), None);
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn stream_turns_internal_tool_into_text_and_drops_preamble() {
        let mut rewriter = StructuredOutputStreamRewriter::new(StructuredOutputTarget {
            tool_name: "structured_output".to_string(),
            forced: true,
        });
        let events = vec![
            event("message_start", json!({"type": "message_start"})),
            event(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            ),
            event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "Sure:"}}),
            ),
            event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            event(
                "content_block_start",
                json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "tool_use", "id": "toolu_01c",
                    "name": "structured_output", "input": {}}}),
            ),
            event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"name\":"}}),
            ),
            event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "\"Ada\"}"}}),
            ),
            event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 1}),
            ),
            event(
                "message_delta",
                json!({"type": "message_delta",
                "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 9}}),
            ),
            event("message_stop", json!({"type": "message_stop"})),
        ];
        let out = rewriter.apply(events);
        let names: Vec<&str> = out.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert_eq!(out[1].data["index"], 0);
        assert_eq!(out[1].data["content_block"]["type"], "text");
        let text: String = out
            .iter()
            .filter_map(|e| e.data["delta"].get("text").and_then(Value::as_str))
            .collect();
        assert_eq!(text, "{\"name\":\"Ada\"}");
        assert_eq!(out[5].data["delta"]["stop_reason"], "end_turn");
        assert_eq!(out[5].data["usage"]["output_tokens"], 9);
        assert!(rewriter.replaced_stop_reason());
    }

    #[test]
    fn stream_releases_held_text_when_the_model_skips_the_tool() {
        let mut rewriter = StructuredOutputStreamRewriter::new(StructuredOutputTarget {
            tool_name: "structured_output".to_string(),
            forced: true,
        });
        let mut out = rewriter.apply(vec![
            event(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            ),
            event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "{\"name\":\"Ada\"}"}}),
            ),
        ]);
        assert!(out.is_empty(), "text is held until the stream ends");
        out.extend(rewriter.apply(vec![
            event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            event(
                "message_delta",
                json!({"type": "message_delta",
                "delta": {"stop_reason": "end_turn"}}),
            ),
        ]));
        let names: Vec<&str> = out.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(
            names,
            [
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta"
            ]
        );
        assert!(!rewriter.replaced_stop_reason());
    }
}
