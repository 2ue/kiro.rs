//! Token 计算模块
//!
//! 提供文本 token 数量计算功能。
//!
//! # 计算规则
//! - 非西文字符：每个计 4.5 个字符单位
//! - 西文字符：每个计 1 个字符单位
//! - 4 个字符单位 = 1 token（四舍五入）

use crate::anthropic::types::{
    CountTokensRequest, CountTokensResponse, Message, SystemMessage, Tool,
};
use crate::http_client::{ProxyConfig, build_client};
use crate::model::config::TlsBackend;
use serde_json::Value;
use std::sync::OnceLock;

/// Count Tokens API 配置
#[derive(Clone, Default)]
pub struct CountTokensConfig {
    /// 外部 count_tokens API 地址
    pub api_url: Option<String>,
    /// count_tokens API 密钥
    pub api_key: Option<String>,
    /// count_tokens API 认证类型（"x-api-key" 或 "bearer"）
    pub auth_type: String,
    /// 代理配置
    pub proxy: Option<ProxyConfig>,

    pub tls_backend: TlsBackend,
}

/// 全局配置存储
static COUNT_TOKENS_CONFIG: OnceLock<CountTokensConfig> = OnceLock::new();

/// 初始化 count_tokens 配置
///
/// 应在应用启动时调用一次
pub fn init_config(config: CountTokensConfig) {
    let _ = COUNT_TOKENS_CONFIG.set(config);
}

/// 获取配置
fn get_config() -> Option<&'static CountTokensConfig> {
    COUNT_TOKENS_CONFIG.get()
}

/// 判断字符是否为非西文字符
///
/// 西文字符包括：
/// - ASCII 字符 (U+0000..U+007F)
/// - 拉丁字母扩展 (U+0080..U+024F)
/// - 拉丁字母扩展附加 (U+1E00..U+1EFF)
///
/// 返回 true 表示该字符是非西文字符（如中文、日文、韩文、阿拉伯文等）
fn is_non_western_char(c: char) -> bool {
    !matches!(c,
        // 基本 ASCII
        '\u{0000}'..='\u{007F}' |
        // 拉丁字母扩展-A (Latin Extended-A)
        '\u{0080}'..='\u{00FF}' |
        // 拉丁字母扩展-B (Latin Extended-B)
        '\u{0100}'..='\u{024F}' |
        // 拉丁字母扩展附加 (Latin Extended Additional)
        '\u{1E00}'..='\u{1EFF}' |
        // 拉丁字母扩展-C/D/E
        '\u{2C60}'..='\u{2C7F}' |
        '\u{A720}'..='\u{A7FF}' |
        '\u{AB30}'..='\u{AB6F}'
    )
}

/// 计算文本的 token 数量
///
/// # 计算规则
/// - 非西文字符：每个计 4.5 个字符单位
/// - 西文字符：每个计 1 个字符单位
/// - 4 个字符单位 = 1 token（四舍五入）
/// ```
pub fn count_tokens(text: &str) -> u64 {
    // println!("text: {}", text);

    let char_units: f64 = text
        .chars()
        .map(|c| if is_non_western_char(c) { 4.0 } else { 1.0 })
        .sum();

    let tokens = char_units / 4.0;

    let acc_token = if tokens < 100.0 {
        tokens * 1.5
    } else if tokens < 200.0 {
        tokens * 1.3
    } else if tokens < 300.0 {
        tokens * 1.25
    } else if tokens < 800.0 {
        tokens * 1.2
    } else {
        tokens * 1.0
    } as u64;

    // println!("tokens: {}, acc_tokens: {}", tokens, acc_token);
    acc_token
}

/// 估算请求的输入 tokens
///
/// 优先调用远程 API，失败时回退到本地计算
pub(crate) fn count_all_tokens(
    model: String,
    system: Option<Vec<SystemMessage>>,
    messages: Vec<Message>,
    tools: Option<Vec<Tool>>,
) -> u64 {
    // 检查是否配置了远程 API
    if let Some(config) = get_config() {
        if let Some(api_url) = &config.api_url {
            // 尝试调用远程 API
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(call_remote_count_tokens(
                    api_url, config, model, &system, &messages, &tools,
                ))
            });

            match result {
                Ok(tokens) => {
                    tracing::debug!("远程 count_tokens API 返回: {}", tokens);
                    return tokens;
                }
                Err(e) => {
                    tracing::warn!("远程 count_tokens API 调用失败，回退到本地计算: {}", e);
                }
            }
        }
    }

    // 本地计算
    count_all_tokens_local(system, messages, tools)
}

/// 调用远程 count_tokens API
async fn call_remote_count_tokens(
    api_url: &str,
    config: &CountTokensConfig,
    model: String,
    system: &Option<Vec<SystemMessage>>,
    messages: &Vec<Message>,
    tools: &Option<Vec<Tool>>,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let client = build_client(config.proxy.as_ref(), 300, config.tls_backend)?;

    // 构建请求体
    let request = CountTokensRequest {
        model: model, // 模型名称用于 token 计算
        messages: messages.clone(),
        system: system.clone(),
        tools: tools.clone(),
    };

    // 构建请求
    let mut req_builder = client.post(api_url);

    // 设置认证头
    if let Some(api_key) = &config.api_key {
        if config.auth_type == "bearer" {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", api_key));
        } else {
            req_builder = req_builder.header("x-api-key", api_key);
        }
    }

    // 发送请求
    let response = req_builder
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("API 返回错误状态: {}", response.status()).into());
    }

    let result: CountTokensResponse = response.json().await?;
    Ok(result.input_tokens as u64)
}

/// 本地计算请求的输入 tokens
fn count_all_tokens_local(
    system: Option<Vec<SystemMessage>>,
    messages: Vec<Message>,
    tools: Option<Vec<Tool>>,
) -> u64 {
    let mut total = 0;

    // 系统消息
    if let Some(ref system) = system {
        for msg in system {
            total += count_tokens(&msg.text);
        }
    }

    // 消息内容。Anthropic content blocks may carry large text outside the
    // top-level `text` field, especially tool_result.content and tool_use.input.
    for msg in &messages {
        total += count_message_content_tokens(&msg.content);
    }

    // 工具定义
    if let Some(ref tools) = tools {
        for tool in tools {
            total += count_tokens(&tool.name);
            total += count_tokens(&tool.description);
            let input_schema_json = serde_json::to_string(&tool.input_schema).unwrap_or_default();
            total += count_tokens(&input_schema_json);
        }
    }

    total.max(1)
}

fn count_message_content_tokens(content: &Value) -> u64 {
    match content {
        Value::String(text) => count_tokens(text),
        Value::Array(items) => items.iter().map(count_content_block_tokens).sum(),
        other => count_json_value_tokens(other),
    }
}

fn count_content_block_tokens(block: &Value) -> u64 {
    let block_type = block.get("type").and_then(Value::as_str);
    match block_type {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(count_tokens)
            .unwrap_or_default(),
        Some("tool_result") => {
            let mut total = block
                .get("content")
                .map(count_tool_result_content_tokens)
                .unwrap_or_default();
            if let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) {
                total += count_tokens(tool_use_id);
            }
            if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                total += count_tokens("error");
            }
            total
        }
        Some("tool_use") => {
            let mut total = 0;
            if let Some(name) = block.get("name").and_then(Value::as_str) {
                total += count_tokens(name);
            }
            if let Some(id) = block.get("id").and_then(Value::as_str) {
                total += count_tokens(id);
            }
            if let Some(input) = block.get("input") {
                total += count_json_value_tokens(input);
            }
            total
        }
        Some("document") => count_document_block_tokens(block),
        Some("image") => count_image_block_tokens(block),
        Some("thinking") | Some("redacted_thinking") => block
            .get("thinking")
            .and_then(Value::as_str)
            .map(count_tokens)
            .unwrap_or_default(),
        _ => count_json_value_tokens(block),
    }
}

fn count_tool_result_content_tokens(content: &Value) -> u64 {
    match content {
        Value::String(text) => count_tokens(text),
        Value::Array(items) => items.iter().map(count_content_block_tokens).sum(),
        other => count_json_value_tokens(other),
    }
}

fn count_document_block_tokens(block: &Value) -> u64 {
    let Some(source) = block.get("source") else {
        return count_json_value_tokens(block);
    };
    let source_type = source.get("type").and_then(Value::as_str);
    match source_type {
        Some("text") => {
            let mut total = source
                .get("data")
                .and_then(Value::as_str)
                .map(count_tokens)
                .unwrap_or_default();
            if let Some(media_type) = source.get("media_type").and_then(Value::as_str) {
                total += count_tokens(media_type);
            }
            total
        }
        Some("base64") => {
            // Base64 documents are sent inline, so count the payload instead of
            // dropping it. This is an estimate, not a tokenizer.
            let mut total = source
                .get("data")
                .and_then(Value::as_str)
                .map(count_tokens)
                .unwrap_or_default();
            if let Some(media_type) = source.get("media_type").and_then(Value::as_str) {
                total += count_tokens(media_type);
            }
            total
        }
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .map(count_tokens)
            .unwrap_or_default(),
        _ => count_json_value_tokens(source),
    }
}

fn count_image_block_tokens(block: &Value) -> u64 {
    let Some(source) = block.get("source") else {
        return count_json_value_tokens(block);
    };
    let source_type = source.get("type").and_then(Value::as_str);
    match source_type {
        Some("base64") => source
            .get("data")
            .and_then(Value::as_str)
            .map(|data| {
                // Roughly align image base64 with request-size pressure. This is
                // not image token pricing, but it avoids treating inline images as free.
                count_tokens(data)
            })
            .unwrap_or_default(),
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .map(count_tokens)
            .unwrap_or_default(),
        _ => count_json_value_tokens(source),
    }
}

fn count_json_value_tokens(value: &Value) -> u64 {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => count_tokens(&value.to_string()),
        Value::String(text) => count_tokens(text),
        Value::Array(items) => items.iter().map(count_json_value_tokens).sum(),
        Value::Object(_) => serde_json::to_string(value)
            .map(|text| count_tokens(&text))
            .unwrap_or_default(),
    }
}

/// 估算输出 tokens
pub(crate) fn estimate_output_tokens(content: &[serde_json::Value]) -> i32 {
    let mut total = 0;

    for block in content {
        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
            total += count_tokens(text) as i32;
        }
        if let Some(thinking) = block.get("thinking").and_then(|v| v.as_str()) {
            total += count_tokens(thinking) as i32;
        }
        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
            // 工具调用开销
            if let Some(input) = block.get("input") {
                let input_str = serde_json::to_string(input).unwrap_or_default();
                total += count_tokens(&input_str) as i32;
            }
        }
    }

    total.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::Message;
    use serde_json::json;

    fn estimate(messages: Vec<Message>) -> u64 {
        count_all_tokens_local(None, messages, None)
    }

    #[test]
    fn local_count_includes_tool_result_content() {
        let long_result = "TOOL-RESULT ".repeat(1_000);
        let tokens = estimate(vec![
            Message {
                role: "assistant".to_string(),
                content: json!([{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "read_file",
                    "input": {"path": "/tmp/huge.txt"}
                }]),
            },
            Message {
                role: "user".to_string(),
                content: json!([{
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": long_result
                }]),
            },
        ]);

        assert!(tokens > 2_000, "tool_result content should be counted");
    }

    #[test]
    fn local_count_includes_nested_tool_result_text_blocks() {
        let tokens = estimate(vec![Message {
            role: "user".to_string(),
            content: json!([{
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": [
                    {"type": "text", "text": "nested tool result ".repeat(200)},
                    {"type": "text", "text": "second block ".repeat(200)}
                ]
            }]),
        }]);

        assert!(
            tokens > 1_000,
            "nested tool_result blocks should be counted"
        );
    }

    #[test]
    fn local_count_includes_tool_use_input_and_document_text() {
        let tokens = estimate(vec![Message {
            role: "user".to_string(),
            content: json!([
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "write_file",
                    "input": {"content": "generated content ".repeat(300)}
                },
                {
                    "type": "document",
                    "source": {
                        "type": "text",
                        "media_type": "text/plain",
                        "data": "document body ".repeat(300)
                    }
                }
            ]),
        }]);

        assert!(
            tokens > 2_000,
            "tool input and document text should be counted"
        );
    }
}
