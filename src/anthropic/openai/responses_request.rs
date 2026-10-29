//! OpenAI Responses 请求翻译成 Anthropic Messages 请求体。
//!
//! 接口是无状态的：`previous_response_id` 需要服务端保存历史，这里不支持，直接返回 400；
//! `store` 不起作用（不保存响应）。历史由客户端在 `input` 里完整带上，Codex 就是这样用的。

use serde_json::{Map, Value, json};

use super::request::{
    Result, apply_reasoning_effort, file_block, image_block, push_turn, text_block, tool_call_input,
};

/// 回显到响应里的请求字段。
pub(super) struct ResponsesEcho {
    pub(super) instructions: Value,
    pub(super) max_output_tokens: Value,
    pub(super) parallel_tool_calls: bool,
    pub(super) tool_choice: Value,
    pub(super) tools: Value,
    pub(super) text: Value,
    pub(super) reasoning: Value,
    pub(super) metadata: Value,
}

pub(super) struct TranslatedResponsesRequest {
    pub(super) body: Value,
    pub(super) echo: ResponsesEcho,
    /// 声明为 custom（自由文本）的工具：模型输出的参数是 `{"input": "..."}`，响应里还原成
    /// `custom_tool_call` 的原始字符串。
    pub(super) custom_tools: Vec<String>,
}

pub(super) fn translate_request(
    raw: &[u8],
    default_max_tokens: i32,
    uses_adaptive_thinking: impl Fn(&str) -> bool,
) -> Result<TranslatedResponsesRequest> {
    let value: Value =
        serde_json::from_slice(raw).map_err(|error| format!("Invalid JSON body: {error}"))?;
    let Value::Object(request) = value else {
        return Err("request body must be a JSON object".to_string());
    };
    let field = |name: &str| request.get(name).filter(|value| !value.is_null());

    let model = field("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or("model: field is required")?;
    if field("previous_response_id").is_some() {
        return Err(
            "previous_response_id is not supported: responses are not stored; send the full conversation in input"
                .to_string(),
        );
    }
    if field("background").and_then(Value::as_bool) == Some(true) {
        return Err("background: background responses are not supported".to_string());
    }
    let stream = match field("stream") {
        None => false,
        Some(Value::Bool(stream)) => *stream,
        Some(_) => return Err("stream: must be a boolean".to_string()),
    };
    let max_tokens = match field("max_output_tokens") {
        None => default_max_tokens,
        Some(value) => value
            .as_i64()
            .filter(|tokens| *tokens >= 1)
            .map(|tokens| tokens.min(i64::from(i32::MAX)) as i32)
            .ok_or("max_output_tokens: must be a positive integer")?,
    };

    let mut system = Vec::new();
    if let Some(instructions) = field("instructions") {
        let instructions = instructions
            .as_str()
            .ok_or("instructions: must be a string")?;
        system.extend(text_block(instructions));
    }
    let messages = translate_input(field("input"), &mut system)?;

    let mut body = Map::new();
    body.insert("model".to_string(), json!(model));
    body.insert("max_tokens".to_string(), json!(max_tokens));
    body.insert("stream".to_string(), json!(stream));
    body.insert("messages".to_string(), Value::Array(messages));
    if !system.is_empty() {
        body.insert("system".to_string(), Value::Array(system));
    }

    let (tools, custom_tools) = translate_tools(field("tools"))?;
    let parallel_tool_calls = field("parallel_tool_calls")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !tools.is_empty() {
        if let Some(choice) = translate_tool_choice(field("tool_choice"), !parallel_tool_calls)? {
            body.insert("tool_choice".to_string(), choice);
        }
        body.insert("tools".to_string(), Value::Array(tools));
    }

    let mut output_config = Map::new();
    let effort = field("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .filter(|effort| !effort.is_null());
    apply_reasoning_effort(
        &mut body,
        &mut output_config,
        effort,
        model,
        max_tokens,
        &uses_adaptive_thinking,
    )?;
    if let Some(format) = translate_text_format(field("text"))? {
        output_config.insert("format".to_string(), format);
    }
    if !output_config.is_empty() {
        body.insert("output_config".to_string(), Value::Object(output_config));
    }
    // 与 Chat Completions 一样，顶层缓存标记模拟 OpenAI 的自动前缀缓存。
    body.insert("cache_control".to_string(), json!({"type": "ephemeral"}));

    let echo = ResponsesEcho {
        instructions: field("instructions").cloned().unwrap_or(Value::Null),
        max_output_tokens: field("max_output_tokens").cloned().unwrap_or(Value::Null),
        parallel_tool_calls,
        tool_choice: field("tool_choice")
            .cloned()
            .unwrap_or_else(|| json!("auto")),
        tools: field("tools").cloned().unwrap_or_else(|| json!([])),
        text: field("text")
            .cloned()
            .unwrap_or_else(|| json!({"format": {"type": "text"}})),
        reasoning: field("reasoning")
            .cloned()
            .unwrap_or_else(|| json!({"effort": null, "summary": null})),
        metadata: field("metadata").cloned().unwrap_or_else(|| json!({})),
    };
    Ok(TranslatedResponsesRequest {
        body: Value::Object(body),
        echo,
        custom_tools,
    })
}

fn translate_input(input: Option<&Value>, system: &mut Vec<Value>) -> Result<Vec<Value>> {
    let mut turns = Vec::new();
    match input {
        None => return Err("input: field is required".to_string()),
        Some(Value::String(text)) => {
            push_turn(&mut turns, "user", text_block(text).into_iter().collect())
        }
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                translate_input_item(item, index, system, &mut turns)?;
            }
        }
        Some(_) => return Err("input: must be a string or an array of input items".to_string()),
    }
    if turns.is_empty() {
        return Err("input: at least one user or assistant message is required".to_string());
    }
    Ok(turns)
}

fn translate_input_item(
    item: &Value,
    index: usize,
    system: &mut Vec<Value>,
    turns: &mut Vec<Value>,
) -> Result<()> {
    let item_type = item.get("type").and_then(Value::as_str);
    match item_type {
        Some("function_call") => {
            let call_id = call_id(item, index)?;
            let name = required_str(item, "name", index)?;
            let input = tool_call_input(item.get("arguments"))
                .ok_or_else(|| format!("input[{index}].arguments: must be a JSON object"))?;
            push_turn(
                turns,
                "assistant",
                vec![json!({"type": "tool_use", "id": call_id, "name": name, "input": input})],
            );
        }
        Some("custom_tool_call") => {
            let call_id = call_id(item, index)?;
            let name = required_str(item, "name", index)?;
            let input = item
                .get("input")
                .and_then(Value::as_str)
                .unwrap_or_default();
            push_turn(
                turns,
                "assistant",
                vec![json!({
                    "type": "tool_use", "id": call_id, "name": name, "input": {"input": input}
                })],
            );
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| format!("input[{index}].call_id: field is required"))?;
            let mut block = json!({"type": "tool_result", "tool_use_id": call_id});
            let content = tool_output_content(item.get("output"), index)?;
            if !content.is_null() {
                block["content"] = content;
            }
            push_turn(turns, "user", vec![block]);
        }
        // 推理摘要和服务端工具调用不回放：Kiro 不接受外来的推理签名，联网搜索由本代理重新执行。
        Some("reasoning") | Some("web_search_call") | Some("file_search_call") => {}
        Some("item_reference") => {
            return Err(format!(
                "input[{index}]: item_reference needs stored responses, which are not supported"
            ));
        }
        Some("message") | None => {
            let role = item
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("input[{index}].role: field is required"))?;
            let content = item.get("content").filter(|content| !content.is_null());
            match role {
                "system" | "developer" => system.extend(message_blocks(content, index, false)?),
                "user" => push_turn(turns, "user", message_blocks(content, index, true)?),
                "assistant" => {
                    push_turn(turns, "assistant", message_blocks(content, index, false)?)
                }
                other => {
                    return Err(format!("input[{index}].role: unsupported role '{other}'"));
                }
            }
        }
        Some(other) => {
            return Err(format!(
                "input[{index}].type: unsupported input item '{other}'"
            ));
        }
    }
    Ok(())
}

fn call_id(item: &Value, index: usize) -> Result<String> {
    item.get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("input[{index}].call_id: field is required"))
}

fn required_str<'a>(item: &'a Value, name: &str, index: usize) -> Result<&'a str> {
    item.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("input[{index}].{name}: field is required"))
}

/// `content` 可以是字符串，也可以是内容数组：`input_text` / `output_text` / `text` 文本、
/// `input_image` 图片、`input_file` 文件、`refusal` 拒答。只有 user 消息允许图片和文件。
fn message_blocks(content: Option<&Value>, index: usize, allow_media: bool) -> Result<Vec<Value>> {
    let parts = match content {
        None => return Ok(Vec::new()),
        Some(Value::String(text)) => return Ok(text_block(text).into_iter().collect()),
        Some(Value::Array(parts)) => parts,
        Some(_) => {
            return Err(format!(
                "input[{index}].content: must be a string or an array of content parts"
            ));
        }
    };
    let mut blocks = Vec::new();
    for part in parts {
        let part_type = part.get("type").and_then(Value::as_str);
        match part_type {
            Some("input_text" | "output_text" | "text") => blocks.extend(text_block(
                part.get("text").and_then(Value::as_str).unwrap_or_default(),
            )),
            Some("refusal") => blocks.extend(text_block(
                part.get("refusal")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )),
            Some("input_image") if allow_media => {
                if part.get("file_id").is_some() && part.get("image_url").is_none() {
                    return Err(format!(
                        "input[{index}].content: input_image with file_id is not supported; send image_url"
                    ));
                }
                blocks.push(image_block(part.get("image_url"), index)?);
            }
            Some("input_file") if allow_media => blocks.push(file_block(Some(part), index)?),
            Some(other) => {
                return Err(format!(
                    "input[{index}].content: unsupported content part '{other}' for this role"
                ));
            }
            None => {
                return Err(format!(
                    "input[{index}].content: content part type is required"
                ));
            }
        }
    }
    Ok(blocks)
}

/// 工具结果：字符串原样；数组里的文本拼起来，图片保留为图片块。
fn tool_output_content(output: Option<&Value>, index: usize) -> Result<Value> {
    match output {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(Value::String(text)) if text.is_empty() => Ok(Value::Null),
        Some(Value::String(text)) => Ok(json!(text)),
        Some(Value::Array(parts)) => {
            let blocks = message_blocks(output, index, true)?;
            Ok(if parts.is_empty() || blocks.is_empty() {
                Value::Null
            } else {
                Value::Array(blocks)
            })
        }
        Some(other) => Ok(json!(other.to_string())),
    }
}

/// Responses 的工具是扁平格式 `{type, name, description, parameters}`。
/// custom 工具接受自由文本，包成只有一个字符串参数 `input` 的函数，语法说明附在描述里。
/// 联网搜索换成 Anthropic 的服务端搜索工具。
fn translate_tools(tools: Option<&Value>) -> Result<(Vec<Value>, Vec<String>)> {
    let Some(tools) = tools else {
        return Ok((Vec::new(), Vec::new()));
    };
    let tools = tools.as_array().ok_or("tools: must be an array")?;
    let mut translated = Vec::with_capacity(tools.len());
    let mut custom = Vec::new();
    for (index, tool) in tools.iter().enumerate() {
        let tool_type = tool
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("function");
        let name = || {
            tool.get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| format!("tools[{index}].name: field is required"))
        };
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match tool_type {
            "function" => {
                let mut schema = match tool.get("parameters") {
                    None | Some(Value::Null) => Map::new(),
                    Some(Value::Object(schema)) => schema.clone(),
                    Some(_) => {
                        return Err(format!("tools[{index}].parameters: must be a JSON object"));
                    }
                };
                schema
                    .entry("type")
                    .or_insert_with(|| Value::String("object".to_string()));
                let mut entry = json!({"name": name()?, "input_schema": schema});
                if !description.is_empty() {
                    entry["description"] = json!(description);
                }
                translated.push(entry);
            }
            "custom" => {
                let name = name()?;
                let mut description = description.to_string();
                if let Some(definition) = tool
                    .get("format")
                    .and_then(|format| format.get("definition"))
                    .and_then(Value::as_str)
                {
                    let syntax = tool
                        .get("format")
                        .and_then(|format| format.get("syntax"))
                        .and_then(Value::as_str)
                        .unwrap_or("grammar");
                    description.push_str(&format!("\n\nInput format ({syntax}):\n{definition}"));
                }
                translated.push(json!({
                    "name": name,
                    "description": description,
                    "input_schema": {
                        "type": "object",
                        "properties": {"input": {
                            "type": "string",
                            "description": "The complete raw tool input text. Do not wrap it in JSON."
                        }},
                        "required": ["input"]
                    }
                }));
                custom.push(name.to_string());
            }
            "web_search" | "web_search_preview" => {
                translated.push(json!({"type": "web_search_20250305", "name": "web_search"}));
            }
            other => {
                return Err(format!(
                    "tools[{index}].type: unsupported tool type '{other}'; function, custom and web_search are supported"
                ));
            }
        }
    }
    Ok((translated, custom))
}

/// Responses 的 `tool_choice`：`auto` / `required` / `none`，或 `{"type":"function","name":..}`。
fn translate_tool_choice(choice: Option<&Value>, disable_parallel: bool) -> Result<Option<Value>> {
    let mut translated = match choice {
        None => json!({"type": "auto"}),
        Some(Value::String(mode)) => match mode.as_str() {
            "auto" => json!({"type": "auto"}),
            "required" => json!({"type": "any"}),
            "none" => return Ok(Some(json!({"type": "none"}))),
            other => return Err(format!("tool_choice: unsupported value '{other}'")),
        },
        Some(Value::Object(object)) => match object.get("type").and_then(Value::as_str) {
            Some("function" | "custom") => {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or("tool_choice.name: field is required")?;
                json!({"type": "tool", "name": name})
            }
            Some("web_search" | "web_search_preview") => {
                json!({"type": "tool", "name": "web_search"})
            }
            _ => {
                return Err(
                    "tool_choice: must be 'none', 'auto', 'required' or a function choice"
                        .to_string(),
                );
            }
        },
        Some(_) => return Err("tool_choice: must be a string or an object".to_string()),
    };
    if disable_parallel {
        translated["disable_parallel_tool_use"] = json!(true);
    } else if choice.is_none() {
        return Ok(None);
    }
    Ok(Some(translated))
}

/// `text.format`：`text` 不约束；`json_schema` 的 schema 在 format 顶层；`json_object` 没有 schema。
fn translate_text_format(text: Option<&Value>) -> Result<Option<Value>> {
    let Some(format) = text
        .and_then(|text| text.get("format"))
        .filter(|f| !f.is_null())
    else {
        return Ok(None);
    };
    match format.get("type").and_then(Value::as_str) {
        Some("text") | Some("json_object") => Ok(None),
        Some("json_schema") => {
            let schema = format
                .get("schema")
                .filter(|schema| schema.is_object())
                .ok_or("text.format.schema: must be a JSON object")?;
            Ok(Some(json!({"type": "json_schema", "schema": schema})))
        }
        _ => Err("text.format.type: must be 'text', 'json_object' or 'json_schema'".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(request: Value) -> Result<TranslatedResponsesRequest> {
        translate_request(
            request.to_string().as_bytes(),
            4_096,
            crate::anthropic::handlers::model_uses_adaptive_thinking,
        )
    }

    fn body(request: Value) -> Value {
        translate(request).expect("request should translate").body
    }

    /// The shape Codex sends: instructions, a developer message, user text, earlier tool calls
    /// (function and custom) with their outputs, and a reasoning item that is not replayed.
    #[test]
    fn codex_style_input_maps_to_anthropic_turns() {
        let translated = translate(json!({
            "model": "claude-sonnet-4-5",
            "instructions": "you are codex",
            "input": [
                {"type": "message", "role": "developer", "content": [
                    {"type": "input_text", "text": "sandbox: read-only"}
                ]},
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "list files"}
                ]},
                {"type": "reasoning", "id": "rs_1", "summary": []},
                {"type": "function_call", "call_id": "call_1", "name": "shell",
                 "arguments": "{\"command\":[\"ls\"]}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "a.txt"},
                {"type": "custom_tool_call", "call_id": "call_2", "name": "apply_patch",
                 "input": "*** Begin Patch"},
                {"type": "custom_tool_call_output", "call_id": "call_2", "output": "Done"},
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "patched"}
                ]},
                {"role": "user", "content": "thanks"}
            ],
            "tools": [
                {"type": "function", "name": "shell", "description": "run",
                 "parameters": {"type": "object", "properties": {"command": {"type": "array"}}}},
                {"type": "custom", "name": "apply_patch", "description": "patch files",
                 "format": {"type": "grammar", "syntax": "lark", "definition": "start: x"}}
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": {"effort": "high"},
            "store": true
        }))
        .expect("request should translate");
        assert_eq!(translated.custom_tools, vec!["apply_patch".to_string()]);
        let body = translated.body;
        assert_eq!(
            body["system"],
            json!([
                {"type": "text", "text": "you are codex"},
                {"type": "text", "text": "sandbox: read-only"}
            ])
        );
        assert_eq!(
            body["messages"],
            json!([
                {"role": "user", "content": [{"type": "text", "text": "list files"}]},
                {"role": "assistant", "content": [{
                    "type": "tool_use", "id": "call_1", "name": "shell",
                    "input": {"command": ["ls"]}
                }]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "a.txt"}
                ]},
                {"role": "assistant", "content": [{
                    "type": "tool_use", "id": "call_2", "name": "apply_patch",
                    "input": {"input": "*** Begin Patch"}
                }]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_2", "content": "Done"}
                ]},
                {"role": "assistant", "content": [{"type": "text", "text": "patched"}]},
                {"role": "user", "content": [{"type": "text", "text": "thanks"}]}
            ])
        );
        assert_eq!(
            body["tool_choice"],
            json!({"type": "auto", "disable_parallel_tool_use": true})
        );
        let custom = &body["tools"][1];
        assert_eq!(custom["name"], "apply_patch");
        assert!(
            custom["description"]
                .as_str()
                .is_some_and(|text| text.contains("lark") && text.contains("start: x"))
        );
        assert_eq!(custom["input_schema"]["required"], json!(["input"]));
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 4_095})
        );
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
        assert!(!translated.echo.parallel_tool_calls);
    }

    #[test]
    fn string_input_formats_web_search_and_media() {
        let translated = translate(json!({
            "model": "claude-sonnet-4-6",
            "input": "hi",
            "max_output_tokens": 900,
            "stream": true,
            "text": {"format": {"type": "json_schema", "name": "out",
                "schema": {"type": "object"}}},
            "tools": [{"type": "web_search"}],
            "tool_choice": {"type": "function", "name": "web_search"},
            "reasoning": {"effort": "minimal"}
        }))
        .expect("request should translate");
        let body = &translated.body;
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
        assert_eq!(body["max_tokens"], 900);
        assert_eq!(body["stream"], true);
        assert_eq!(
            body["tools"],
            json!([{"type": "web_search_20250305", "name": "web_search"}])
        );
        assert_eq!(
            body["tool_choice"],
            json!({"type": "tool", "name": "web_search"})
        );
        assert_eq!(body["thinking"], json!({"type": "adaptive"}));
        assert_eq!(
            body["output_config"],
            json!({"effort": "low", "format": {"type": "json_schema", "schema": {"type": "object"}}})
        );
        assert_eq!(translated.echo.max_output_tokens, json!(900));

        let media = body_of_media();
        assert_eq!(
            media["messages"][0]["content"],
            json!([
                {"type": "text", "text": "what is this"},
                {"type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="
                }},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}},
                {"type": "document", "source": {
                    "type": "base64", "media_type": "application/pdf", "data": "JVBERi0x"
                }}
            ])
        );
    }

    fn body_of_media() -> Value {
        body(json!({
            "model": "claude-sonnet-4-5",
            "input": [{"role": "user", "content": [
                {"type": "input_text", "text": "what is this"},
                {"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo="},
                {"type": "input_image", "image_url": "https://example.com/a.png"},
                {"type": "input_file", "file_data": "data:application/pdf;base64,JVBERi0x"}
            ]}]
        }))
    }

    #[test]
    fn unsupported_or_stateful_requests_are_rejected() {
        for (request, expected) in [
            (json!({"input": "hi"}), "model"),
            (json!({"model": "m"}), "input"),
            (
                json!({"model": "m", "input": "hi", "previous_response_id": "resp_1"}),
                "previous_response_id",
            ),
            (
                json!({"model": "m", "input": "hi", "background": true}),
                "background",
            ),
            (
                json!({"model": "m", "input": "hi", "max_output_tokens": 0}),
                "max_output_tokens",
            ),
            (
                json!({"model": "m", "input": [{"type": "item_reference", "id": "msg_1"}]}),
                "item_reference",
            ),
            (
                json!({"model": "m", "input": [{"role": "developer", "content": "only system"}]}),
                "at least one",
            ),
            (
                json!({"model": "m", "input": "hi", "tools": [{"type": "file_search"}]}),
                "file_search",
            ),
            (
                json!({"model": "m", "input": [{"type": "function_call", "call_id": "c",
                    "name": "x", "arguments": "{\"a\":"}]}),
                "arguments",
            ),
        ] {
            let error = translate(request.clone()).err().unwrap_or_default();
            assert!(error.contains(expected), "{request}: {error}");
        }
    }
}
