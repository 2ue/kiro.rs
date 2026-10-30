//! OpenAI Chat Completions 请求翻译成 Anthropic Messages 请求体。
//!
//! 只做协议层面的字段映射，模型解析、提示词注入、缓存、调度等仍由消息入口按 Anthropic 请求处理。
//! 不支持的能力（多候选、音频、旧版 functions）直接返回 400，不静默丢弃。

use serde_json::{Map, Value, json};

use crate::anthropic::stream::{ToolInputJson, classify_tool_input_json};

pub(super) type Result<T> = std::result::Result<T, String>;

/// `thinking.type = enabled` 要求预算至少 1024，且小于 `max_tokens`。
const MIN_THINKING_BUDGET_TOKENS: i32 = 1_024;

pub(super) struct TranslatedRequest {
    pub(super) body: Value,
    pub(super) include_usage: bool,
}

enum ReasoningEffort {
    /// 没传：和 Anthropic 一样默认不开思考（`-thinking` 模型名后缀仍按原规则生效）。
    Default,
    /// `none`：显式关闭思考。
    Off,
    Level(&'static str),
}

/// 旧模型只认 `thinking.type = enabled` + 预算，按 OpenAI 的档位换算成预算。
fn enabled_thinking_budget(effort: &str) -> i32 {
    match effort {
        "low" => 2_048,
        "medium" => 8_192,
        "high" => 16_384,
        "xhigh" => 24_576,
        _ => 32_000,
    }
}

/// `default_max_tokens`：客户端没传输出上限时补的值（OpenAI 协议里这个字段可选）。
/// `uses_adaptive_thinking`：模型是否使用 `thinking.type = adaptive`。
pub(super) fn translate_request(
    raw: &[u8],
    default_max_tokens: i32,
    uses_adaptive_thinking: impl Fn(&str) -> bool,
) -> Result<TranslatedRequest> {
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
    if field("n").is_some_and(|n| n.as_u64() != Some(1)) {
        return Err("n: only 1 is supported".to_string());
    }
    if field("functions").is_some() || field("function_call").is_some() {
        return Err(
            "functions and function_call are not supported; use tools and tool_choice".to_string(),
        );
    }
    let audio_modality = field("modalities")
        .and_then(Value::as_array)
        .is_some_and(|modalities| modalities.iter().any(|item| item == "audio"));
    if field("audio").is_some() || audio_modality {
        return Err("audio output is not supported".to_string());
    }
    let stream = match field("stream") {
        None => false,
        Some(Value::Bool(stream)) => *stream,
        Some(_) => return Err("stream: must be a boolean".to_string()),
    };
    let include_usage = stream
        && field("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let max_tokens = match field("max_completion_tokens").or_else(|| field("max_tokens")) {
        None => default_max_tokens,
        Some(value) => value
            .as_i64()
            .filter(|tokens| *tokens >= 1)
            .map(|tokens| tokens.min(i64::from(i32::MAX)) as i32)
            .ok_or("max_completion_tokens: must be a positive integer")?,
    };

    let (system, messages) = translate_messages(field("messages"))?;
    let mut body = Map::new();
    body.insert("model".to_string(), json!(model));
    body.insert("max_tokens".to_string(), json!(max_tokens));
    body.insert("stream".to_string(), json!(stream));
    body.insert("messages".to_string(), Value::Array(messages));
    if !system.is_empty() {
        body.insert("system".to_string(), Value::Array(system));
    }

    let tools = translate_tools(field("tools"))?;
    if !tools.is_empty() {
        let disable_parallel = field("parallel_tool_calls").and_then(Value::as_bool) == Some(false);
        if let Some(choice) = translate_tool_choice(field("tool_choice"), disable_parallel)? {
            body.insert("tool_choice".to_string(), choice);
        }
        body.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(stop) = translate_stop(field("stop"))? {
        body.insert("stop_sequences".to_string(), stop);
    }

    let mut output_config = Map::new();
    let effort = field("reasoning_effort").or_else(|| {
        field("reasoning")
            .and_then(|reasoning| reasoning.get("effort"))
            .filter(|effort| !effort.is_null())
    });
    apply_reasoning_effort(
        &mut body,
        &mut output_config,
        effort,
        model,
        max_tokens,
        &uses_adaptive_thinking,
    )?;
    if let Some(format) = translate_response_format(field("response_format"))? {
        output_config.insert("format".to_string(), format);
    }
    if !output_config.is_empty() {
        body.insert("output_config".to_string(), Value::Object(output_config));
    }
    // OpenAI 对长前缀自动缓存；顶层缓存标记是 Anthropic 的同类行为。
    body.insert("cache_control".to_string(), json!({"type": "ephemeral"}));

    Ok(TranslatedRequest {
        body: Value::Object(body),
        include_usage,
    })
}

/// OpenAI 的思考档位换成 Anthropic 的思考配置：4.6 及以上模型用 adaptive + 档位，旧模型换算成
/// 预算（`max_tokens` 不够放下最小预算时不开思考）；`none` 显式关闭；没传时不改。
pub(super) fn apply_reasoning_effort(
    body: &mut Map<String, Value>,
    output_config: &mut Map<String, Value>,
    effort: Option<&Value>,
    model: &str,
    max_tokens: i32,
    uses_adaptive_thinking: &impl Fn(&str) -> bool,
) -> Result<()> {
    match reasoning_effort(effort)? {
        ReasoningEffort::Default => {}
        ReasoningEffort::Off => {
            body.insert("thinking".to_string(), json!({"type": "disabled"}));
        }
        ReasoningEffort::Level(effort) => {
            if uses_adaptive_thinking(model) {
                body.insert("thinking".to_string(), json!({"type": "adaptive"}));
                output_config.insert("effort".to_string(), json!(effort));
            } else if max_tokens > MIN_THINKING_BUDGET_TOKENS {
                let budget = enabled_thinking_budget(effort).min(max_tokens - 1);
                body.insert(
                    "thinking".to_string(),
                    json!({"type": "enabled", "budget_tokens": budget}),
                );
            }
        }
    }
    Ok(())
}

/// 返回 `(system 块, 对话轮次)`。system/developer 消息不论出现在哪里都提到 system。
fn translate_messages(messages: Option<&Value>) -> Result<(Vec<Value>, Vec<Value>)> {
    let messages = messages
        .and_then(Value::as_array)
        .ok_or("messages: field is required")?;
    let mut system = Vec::new();
    let mut turns = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("messages[{index}].role: field is required"))?;
        let content = message.get("content").filter(|content| !content.is_null());
        let (role, blocks) = match role {
            "system" | "developer" => {
                system.extend(text_blocks(content, index)?);
                continue;
            }
            "user" => ("user", user_blocks(content, index)?),
            "assistant" => ("assistant", assistant_blocks(message, content, index)?),
            "tool" => ("user", vec![tool_result_block(message, content, index)?]),
            "function" => {
                return Err(format!(
                    "messages[{index}].role: function messages are not supported; use tool messages"
                ));
            }
            other => {
                return Err(format!(
                    "messages[{index}].role: unsupported role '{other}'"
                ));
            }
        };
        push_turn(&mut turns, role, blocks);
    }
    if turns.is_empty() {
        return Err("messages: at least one user or assistant message is required".to_string());
    }
    Ok((system, turns))
}

/// 相邻的同角色消息（例如连续几条 tool 结果和随后的 user 消息）合并成一轮。
pub(super) fn push_turn(turns: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = turns.last_mut()
        && last["role"] == role
        && let Some(content) = last["content"].as_array_mut()
    {
        content.extend(blocks);
        return;
    }
    turns.push(json!({"role": role, "content": blocks}));
}

/// Anthropic 不接受空白文本块，空内容直接跳过。
pub(super) fn text_block(text: &str) -> Option<Value> {
    (!text.trim().is_empty()).then(|| json!({"type": "text", "text": text}))
}

/// 只允许文本的内容（system、developer、tool）。
fn text_blocks(content: Option<&Value>, index: usize) -> Result<Vec<Value>> {
    match content {
        None => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(text_block(text).into_iter().collect()),
        Some(Value::Array(parts)) => {
            let mut blocks = Vec::new();
            for part in parts {
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    return Err(format!(
                        "messages[{index}].content: only text parts are supported for this role"
                    ));
                }
                blocks.extend(text_block(
                    part.get("text").and_then(Value::as_str).unwrap_or_default(),
                ));
            }
            Ok(blocks)
        }
        Some(_) => Err(format!(
            "messages[{index}].content: must be a string or an array of content parts"
        )),
    }
}

fn user_blocks(content: Option<&Value>, index: usize) -> Result<Vec<Value>> {
    let parts = match content {
        None => return Ok(Vec::new()),
        Some(Value::String(text)) => return Ok(text_block(text).into_iter().collect()),
        Some(Value::Array(parts)) => parts,
        Some(_) => {
            return Err(format!(
                "messages[{index}].content: must be a string or an array of content parts"
            ));
        }
    };
    let mut blocks = Vec::new();
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => blocks.extend(text_block(
                part.get("text").and_then(Value::as_str).unwrap_or_default(),
            )),
            Some("image_url") => blocks.push(image_block(part.get("image_url"), index)?),
            Some("file") => blocks.push(file_block(part.get("file"), index)?),
            Some("input_audio") => {
                return Err(format!(
                    "messages[{index}].content: audio input is not supported"
                ));
            }
            other => {
                return Err(format!(
                    "messages[{index}].content: unsupported content part type '{}'",
                    other.unwrap_or("<missing>")
                ));
            }
        }
    }
    Ok(blocks)
}

fn parse_base64_data_url(url: &str) -> Option<(&str, &str)> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    (!media_type.is_empty() && !data.is_empty()).then_some((media_type, data))
}

/// data URL 转成 base64 图片；http(s) 地址交给多模态预处理按现有规则下载。
pub(super) fn image_block(image_url: Option<&Value>, index: usize) -> Result<Value> {
    let url = match image_url {
        Some(Value::String(url)) => url.as_str(),
        Some(Value::Object(object)) => object
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        _ => "",
    };
    if let Some((media_type, data)) = parse_base64_data_url(url) {
        return Ok(json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }));
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return Ok(json!({"type": "image", "source": {"type": "url", "url": url}}));
    }
    Err(format!(
        "messages[{index}].content: image_url must be an http(s) URL or a base64 data URL"
    ))
}

/// OpenAI 的文件输入以 PDF 为主：不是 data URL 的 `file_data` 按 PDF 处理。
pub(super) fn file_block(file: Option<&Value>, index: usize) -> Result<Value> {
    let file = file.and_then(Value::as_object);
    if let Some(data) = file
        .and_then(|file| file.get("file_data"))
        .and_then(Value::as_str)
    {
        let (media_type, data) = parse_base64_data_url(data).unwrap_or(("application/pdf", data));
        return Ok(json!({
            "type": "document",
            "source": {"type": "base64", "media_type": media_type, "data": data}
        }));
    }
    if let Some(file_id) = file
        .and_then(|file| file.get("file_id"))
        .and_then(Value::as_str)
    {
        return Ok(json!({"type": "document", "source": {"type": "file", "file_id": file_id}}));
    }
    Err(format!(
        "messages[{index}].content: file parts need file_data or file_id"
    ))
}

fn assistant_blocks(message: &Value, content: Option<&Value>, index: usize) -> Result<Vec<Value>> {
    let mut blocks = Vec::new();
    match content {
        None => {}
        Some(Value::String(text)) => blocks.extend(text_block(text)),
        Some(Value::Array(parts)) => {
            for part in parts {
                let text = match part.get("type").and_then(Value::as_str) {
                    Some("text") => part.get("text"),
                    Some("refusal") => part.get("refusal"),
                    _ => {
                        return Err(format!(
                            "messages[{index}].content: assistant content supports text and refusal parts only"
                        ));
                    }
                };
                blocks.extend(text_block(text.and_then(Value::as_str).unwrap_or_default()));
            }
        }
        Some(_) => {
            return Err(format!(
                "messages[{index}].content: must be a string or an array of content parts"
            ));
        }
    }
    let calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for (call_index, call) in calls.enumerate() {
        let path = format!("messages[{index}].tool_calls[{call_index}]");
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| format!("{path}.id: field is required"))?;
        let function = call.get("function");
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("{path}.function.name: field is required"))?;
        // 历史里的参数是之前某一轮的输出，坏了也修不回来；按空参数回放，
        // 不能让一条坏历史把之后每一轮都变成 400。
        let input = tool_call_input(function.and_then(|function| function.get("arguments")))
            .unwrap_or_else(|| {
                tracing::warn!(
                    path = %path,
                    "历史里的工具调用参数不是合法 JSON 对象，按空参数回放"
                );
                json!({})
            });
        blocks.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
    }
    Ok(blocks)
}

/// 历史里的工具参数：空参数视为 `{}`；可安全清洗的多余尾巴（见 `classify_tool_input_json`）
/// 直接去掉；其余非对象一律拒绝。
pub(super) fn tool_call_input(arguments: Option<&Value>) -> Option<Value> {
    let input = match arguments {
        None | Some(Value::Null) => return Some(json!({})),
        Some(Value::Object(_)) => return arguments.cloned(),
        Some(Value::String(text)) if text.trim().is_empty() => return Some(json!({})),
        Some(Value::String(text)) => match classify_tool_input_json(text) {
            ToolInputJson::Valid => serde_json::from_str::<Value>(text).ok()?,
            ToolInputJson::Cleaned(cleaned) => serde_json::from_str::<Value>(&cleaned).ok()?,
            ToolInputJson::Invalid => return None,
        },
        Some(_) => return None,
    };
    input.is_object().then_some(input)
}

fn tool_result_block(message: &Value, content: Option<&Value>, index: usize) -> Result<Value> {
    let id = message
        .get("tool_call_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("messages[{index}].tool_call_id: field is required"))?;
    let mut block = json!({"type": "tool_result", "tool_use_id": id});
    match content {
        Some(Value::String(text)) if !text.is_empty() => block["content"] = json!(text),
        Some(Value::Array(_)) => {
            let blocks = text_blocks(content, index)?;
            if !blocks.is_empty() {
                block["content"] = Value::Array(blocks);
            }
        }
        None | Some(Value::String(_)) => {}
        Some(_) => {
            return Err(format!(
                "messages[{index}].content: must be a string or an array of text parts"
            ));
        }
    }
    Ok(block)
}

fn translate_tools(tools: Option<&Value>) -> Result<Vec<Value>> {
    let Some(tools) = tools else {
        return Ok(Vec::new());
    };
    let tools = tools.as_array().ok_or("tools: must be an array")?;
    let mut translated = Vec::with_capacity(tools.len());
    for (index, tool) in tools.iter().enumerate() {
        // 非函数工具（例如 OpenAI 托管的 file_search）上游没有对应能力：跳过并记日志，
        // 不让整个请求失败。
        if let Some(tool_type) = tool
            .get("type")
            .and_then(Value::as_str)
            .filter(|tool_type| *tool_type != "function")
        {
            tracing::warn!(
                tool_type,
                index,
                "Chat Completions 请求里的工具类型不受支持，已跳过"
            );
            continue;
        }
        let function = tool
            .get("function")
            .ok_or_else(|| format!("tools[{index}].function: field is required"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("tools[{index}].function.name: field is required"))?;
        let mut schema = match function.get("parameters") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(schema)) => schema.clone(),
            Some(_) => {
                return Err(format!(
                    "tools[{index}].function.parameters: must be a JSON object"
                ));
            }
        };
        schema
            .entry("type")
            .or_insert_with(|| Value::String("object".to_string()));
        let mut tool = json!({"name": name, "input_schema": schema});
        if let Some(description) = function
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.is_empty())
        {
            tool["description"] = json!(description);
        }
        translated.push(tool);
    }
    Ok(translated)
}

/// 没传 `tool_choice` 时保持 Anthropic 默认（auto），只有要关闭并行调用时才显式写出。
fn translate_tool_choice(choice: Option<&Value>, disable_parallel: bool) -> Result<Option<Value>> {
    let mut translated = match choice {
        None => json!({"type": "auto"}),
        Some(Value::String(mode)) => match mode.as_str() {
            "auto" => json!({"type": "auto"}),
            "required" => json!({"type": "any"}),
            "none" => return Ok(Some(json!({"type": "none"}))),
            other => return Err(format!("tool_choice: unsupported value '{other}'")),
        },
        Some(Value::Object(object))
            if object.get("type").and_then(Value::as_str) == Some("function") =>
        {
            let name = object
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .ok_or("tool_choice.function.name: field is required")?;
            json!({"type": "tool", "name": name})
        }
        Some(_) => {
            return Err(
                "tool_choice: must be 'none', 'auto', 'required' or a function choice".to_string(),
            );
        }
    };
    if disable_parallel {
        translated["disable_parallel_tool_use"] = json!(true);
    } else if choice.is_none() {
        return Ok(None);
    }
    Ok(Some(translated))
}

fn translate_stop(stop: Option<&Value>) -> Result<Option<Value>> {
    const INVALID: &str = "stop: must be a string or an array of strings";
    let sequences: Vec<&str> = match stop {
        None => return Ok(None),
        Some(Value::String(sequence)) => vec![sequence.as_str()],
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().ok_or(INVALID))
            .collect::<std::result::Result<_, _>>()?,
        Some(_) => return Err(INVALID.to_string()),
    };
    let sequences: Vec<&str> = sequences
        .into_iter()
        .filter(|sequence| !sequence.is_empty())
        .collect();
    Ok((!sequences.is_empty()).then(|| json!(sequences)))
}

fn reasoning_effort(value: Option<&Value>) -> Result<ReasoningEffort> {
    let Some(value) = value else {
        return Ok(ReasoningEffort::Default);
    };
    let effort = value.as_str().ok_or("reasoning_effort: must be a string")?;
    Ok(match effort.trim().to_ascii_lowercase().as_str() {
        "none" => ReasoningEffort::Off,
        "minimal" | "low" => ReasoningEffort::Level("low"),
        "medium" => ReasoningEffort::Level("medium"),
        "high" => ReasoningEffort::Level("high"),
        "xhigh" => ReasoningEffort::Level("xhigh"),
        "max" => ReasoningEffort::Level("max"),
        other => return Err(format!("reasoning_effort: unsupported value '{other}'")),
    })
}

/// `json_object` 没有 schema；OpenAI 要求提示词里自己写明输出 JSON，这里不额外约束。
pub(super) fn translate_response_format(format: Option<&Value>) -> Result<Option<Value>> {
    let Some(format) = format else {
        return Ok(None);
    };
    match format.get("type").and_then(Value::as_str) {
        Some("text") | Some("json_object") => Ok(None),
        Some("json_schema") => {
            let schema = format
                .get("json_schema")
                .and_then(|json_schema| json_schema.get("schema"))
                .filter(|schema| schema.is_object())
                .ok_or("response_format.json_schema.schema: must be a JSON object")?;
            Ok(Some(json!({"type": "json_schema", "schema": schema})))
        }
        _ => {
            Err("response_format.type: must be 'text', 'json_object' or 'json_schema'".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(request: Value) -> Result<TranslatedRequest> {
        translate_request(
            request.to_string().as_bytes(),
            4_096,
            crate::anthropic::handlers::model_uses_adaptive_thinking,
        )
    }

    fn body(request: Value) -> Value {
        translate(request).expect("request should translate").body
    }

    #[test]
    fn messages_map_to_anthropic_turns() {
        let body = body(json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "list files"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "a.txt"},
                {"role": "developer", "content": [{"type": "text", "text": "answer in English"}]},
                {"role": "user", "content": [{"type": "text", "text": "and now?"}]}
            ]
        }));
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["max_tokens"], 4_096);
        assert_eq!(body["stream"], false);
        assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
        assert_eq!(
            body["system"],
            json!([
                {"type": "text", "text": "be brief"},
                {"type": "text", "text": "answer in English"}
            ])
        );
        assert_eq!(
            body["messages"],
            json!([
                {"role": "user", "content": [{"type": "text", "text": "list files"}]},
                {"role": "assistant", "content": [{
                    "type": "tool_use", "id": "call_1", "name": "Bash", "input": {"command": "ls"}
                }]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "a.txt"},
                    {"type": "text", "text": "and now?"}
                ]}
            ])
        );
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn reasoning_effort_follows_the_model_thinking_mode() {
        let request = |model: &str, effort: Value, max_tokens: i64| {
            json!({
                "model": model,
                "max_completion_tokens": max_tokens,
                "reasoning_effort": effort,
                "messages": [{"role": "user", "content": "hi"}]
            })
        };
        let adaptive = body(request("claude-sonnet-4-6", json!("minimal"), 8_000));
        assert_eq!(adaptive["thinking"], json!({"type": "adaptive"}));
        assert_eq!(adaptive["output_config"], json!({"effort": "low"}));

        let enabled = body(request("claude-sonnet-4-5", json!("high"), 8_000));
        assert_eq!(
            enabled["thinking"],
            json!({"type": "enabled", "budget_tokens": 7_999})
        );
        assert!(enabled.get("output_config").is_none());

        let too_small = body(request("claude-sonnet-4-5", json!("high"), 1_024));
        assert!(too_small.get("thinking").is_none());

        let off = body(request("claude-sonnet-4-6", json!("none"), 8_000));
        assert_eq!(off["thinking"], json!({"type": "disabled"}));

        let nested = body(json!({
            "model": "claude-opus-4-8",
            "reasoning": {"effort": "max"},
            "messages": [{"role": "user", "content": "hi"}]
        }));
        assert_eq!(nested["output_config"], json!({"effort": "max"}));

        assert!(translate(request("claude-sonnet-4-6", json!("extreme"), 8_000)).is_err());
    }

    #[test]
    fn tools_tool_choice_and_parallel_calls() {
        let request = |extra: Value| {
            let mut request = json!({
                "model": "claude-sonnet-4-5",
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"type": "function", "function": {
                    "name": "Bash",
                    "description": "run a command",
                    "parameters": {"properties": {"command": {"type": "string"}}}
                }}]
            });
            request
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            body(request)
        };
        let plain = request(json!({}));
        assert_eq!(
            plain["tools"],
            json!([{
                "name": "Bash",
                "description": "run a command",
                "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}}
            }])
        );
        assert!(plain.get("tool_choice").is_none());
        assert_eq!(
            request(json!({"tool_choice": "required"}))["tool_choice"],
            json!({"type": "any"})
        );
        assert_eq!(
            request(json!({"tool_choice": "none"}))["tool_choice"],
            json!({"type": "none"})
        );
        assert_eq!(
            request(json!({"tool_choice": {"type": "function", "function": {"name": "Bash"}}}))["tool_choice"],
            json!({"type": "tool", "name": "Bash"})
        );
        assert_eq!(
            request(json!({"parallel_tool_calls": false}))["tool_choice"],
            json!({"type": "auto", "disable_parallel_tool_use": true})
        );

        let without_tools = body(json!({
            "model": "claude-sonnet-4-5",
            "tool_choice": "required",
            "messages": [{"role": "user", "content": "hi"}]
        }));
        assert!(without_tools.get("tool_choice").is_none());
        assert!(without_tools.get("tools").is_none());
    }

    #[test]
    fn media_stop_format_and_stream_options() {
        let translated = translate(json!({
            "model": "claude-sonnet-4-5",
            "stream": true,
            "stream_options": {"include_usage": true},
            "stop": "END",
            "response_format": {"type": "json_schema", "json_schema": {
                "name": "answer", "schema": {"type": "object"}
            }},
            "temperature": 0.2,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
                {"type": "image_url", "image_url": {"url": "https://example.com/cat.png"}},
                {"type": "file", "file": {"file_data": "JVBERi0x"}}
            ]}]
        }))
        .expect("request should translate");
        assert!(translated.include_usage);
        let body = translated.body;
        assert_eq!(body["stream"], true);
        assert_eq!(body["stop_sequences"], json!(["END"]));
        assert_eq!(
            body["output_config"],
            json!({"format": {"type": "json_schema", "schema": {"type": "object"}}})
        );
        assert!(body.get("temperature").is_none());
        assert_eq!(
            body["messages"][0]["content"],
            json!([
                {"type": "text", "text": "what is this?"},
                {"type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="
                }},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/cat.png"}},
                {"type": "document", "source": {
                    "type": "base64", "media_type": "application/pdf", "data": "JVBERi0x"
                }}
            ])
        );

        let non_stream = translate(json!({
            "model": "claude-sonnet-4-5",
            "stream_options": {"include_usage": true},
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .expect("request should translate");
        assert!(!non_stream.include_usage);
    }

    #[test]
    fn history_tool_arguments_are_cleaned_or_replayed_empty() {
        let request = |arguments: &str| {
            translate(json!({
                "model": "claude-sonnet-4-5",
                "messages": [
                    {"role": "user", "content": "go"},
                    {"role": "assistant", "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "Bash", "arguments": arguments}
                    }]},
                    {"role": "tool", "tool_call_id": "call_1", "content": "done"}
                ]
            }))
        };
        let input = |arguments: &str| {
            request(arguments).expect("arguments should translate").body["messages"][1]["content"]
                [0]["input"]
                .clone()
        };
        assert_eq!(input(""), json!({}));
        assert_eq!(input(r#"{"command":"ls"}}"#), json!({"command": "ls"}));
        // Unrepairable history arguments are replayed as `{}` instead of failing every turn.
        assert_eq!(input(r#"{"command":"ls""#), json!({}));
        assert_eq!(input("[1,2]"), json!({}));
    }

    #[test]
    fn unsupported_requests_are_rejected() {
        for (request, expected) in [
            (
                json!({"messages": [{"role": "user", "content": "hi"}]}),
                "model",
            ),
            (json!({"model": "m"}), "messages"),
            (
                json!({"model": "m", "n": 2, "messages": [{"role": "user", "content": "hi"}]}),
                "n:",
            ),
            (
                json!({"model": "m", "functions": [], "messages": [{"role": "user", "content": "hi"}]}),
                "functions",
            ),
            (
                json!({"model": "m", "modalities": ["text", "audio"], "messages": [{"role": "user", "content": "hi"}]}),
                "audio",
            ),
            (
                json!({"model": "m", "max_tokens": 0, "messages": [{"role": "user", "content": "hi"}]}),
                "max_completion_tokens",
            ),
            (
                json!({"model": "m", "messages": [{"role": "system", "content": "only system"}]}),
                "at least one",
            ),
            (
                json!({"model": "m", "messages": [{"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": "file:///etc/passwd"}}
                ]}]}),
                "image_url",
            ),
            (
                json!({"model": "m", "messages": [{"role": "tool", "content": "orphan"}]}),
                "tool_call_id",
            ),
        ] {
            let error = translate(request.clone()).err().unwrap_or_default();
            assert!(error.contains(expected), "{request}: {error}");
        }
        assert!(
            translate_request(b"[]", 4_096, |_| false)
                .err()
                .is_some_and(|error| error.contains("JSON object"))
        );
    }
}
