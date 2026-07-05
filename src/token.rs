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
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
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
    model: &str,
    system: Option<&[SystemMessage]>,
    messages: &[Message],
    tools: Option<&[Tool]>,
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
    model: &str,
    system: &Option<&[SystemMessage]>,
    messages: &&[Message],
    tools: &Option<&[Tool]>,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let client = build_client(config.proxy.as_ref(), 300, config.tls_backend)?;

    // 构建请求体
    let request = CountTokensRequest {
        model: model.to_string(), // 模型名称用于 token 计算
        messages: messages.to_vec(),
        system: system.map(<[SystemMessage]>::to_vec),
        tools: tools.map(<[Tool]>::to_vec),
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
    system: Option<&[SystemMessage]>,
    messages: &[Message],
    tools: Option<&[Tool]>,
) -> u64 {
    let mut total = 0;

    // 系统消息
    if let Some(system) = system {
        for msg in system {
            total += count_tokens(&msg.text);
        }
    }

    // 消息内容。Anthropic content blocks may carry large text outside the
    // top-level `text` field, especially tool_result.content and tool_use.input.
    for msg in messages {
        total += count_message_content_tokens(&msg.content);
    }

    // 工具定义
    if let Some(tools) = tools {
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
    if source_type.is_none() && source.get("file_id").is_some() {
        return DEFAULT_IMAGE_TOKENS;
    }
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
            .map(estimate_image_tokens_from_base64_or_data_url)
            .unwrap_or(DEFAULT_IMAGE_TOKENS),
        Some("url") => source
            .get("url")
            .and_then(Value::as_str)
            .map(|url| {
                if let Some((_, data)) = parse_data_url(url) {
                    estimate_image_tokens_from_base64(&data)
                } else {
                    DEFAULT_IMAGE_TOKENS + count_tokens(url).min(64)
                }
            })
            .unwrap_or(DEFAULT_IMAGE_TOKENS),
        Some("file") | Some("file_id") => DEFAULT_IMAGE_TOKENS,
        _ => count_json_value_tokens(source),
    }
}

const DEFAULT_IMAGE_TOKENS: u64 = 1_600;
const MIN_IMAGE_TOKENS: u64 = 85;
const CLAUDE_IMAGE_MAX_BILLED_PIXELS: u64 = 1_150_000;
const CLAUDE_IMAGE_PIXELS_PER_TOKEN: u64 = 750;

fn estimate_image_tokens_from_base64_or_data_url(data: &str) -> u64 {
    if let Some((_, data)) = parse_data_url(data) {
        return estimate_image_tokens_from_base64(&data);
    }
    estimate_image_tokens_from_base64(data)
}

fn estimate_image_tokens_from_base64(data: &str) -> u64 {
    let normalized = strip_base64_whitespace(data);
    let Ok(bytes) = BASE64_STANDARD.decode(normalized.as_bytes()) else {
        return DEFAULT_IMAGE_TOKENS;
    };
    estimate_image_tokens_from_bytes(&bytes)
}

fn estimate_image_tokens_from_bytes(bytes: &[u8]) -> u64 {
    let Some((width, height)) = image_dimensions(bytes) else {
        return DEFAULT_IMAGE_TOKENS;
    };
    let pixels = (width as u64).saturating_mul(height as u64);
    if pixels == 0 {
        return DEFAULT_IMAGE_TOKENS;
    }
    let billed_pixels = pixels.min(CLAUDE_IMAGE_MAX_BILLED_PIXELS);
    billed_pixels
        .div_ceil(CLAUDE_IMAGE_PIXELS_PER_TOKEN)
        .clamp(MIN_IMAGE_TOKENS, DEFAULT_IMAGE_TOKENS)
}

fn parse_data_url(value: &str) -> Option<(String, String)> {
    let data_part = value.strip_prefix("data:")?;
    let (metadata, data) = data_part.split_once(',')?;
    if !metadata
        .split(';')
        .skip(1)
        .any(|part| part.trim().eq_ignore_ascii_case("base64"))
    {
        return None;
    }
    let media_type = metadata
        .split(';')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some((media_type.to_string(), data.to_string()))
}

fn strip_base64_whitespace(data: &str) -> String {
    if data.bytes().any(|byte| byte.is_ascii_whitespace()) {
        data.chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect()
    } else {
        data.to_string()
    }
}

fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    png_dimensions(bytes)
        .or_else(|| jpeg_dimensions(bytes))
        .or_else(|| gif_dimensions(bytes))
        .or_else(|| webp_dimensions(bytes))
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'])
    {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }

    let mut index = 2usize;
    while index + 9 < bytes.len() {
        while index < bytes.len() && bytes[index] != 0xff {
            index += 1;
        }
        if index + 3 >= bytes.len() {
            return None;
        }
        while index < bytes.len() && bytes[index] == 0xff {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        let marker = bytes[index];
        index += 1;
        if matches!(marker, 0xd8 | 0xd9 | 0x01) || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if index + 2 > bytes.len() {
            return None;
        }
        let segment_len = u16::from_be_bytes(bytes[index..index + 2].try_into().ok()?) as usize;
        if segment_len < 2 || index + segment_len > bytes.len() {
            return None;
        }
        if is_jpeg_sof_marker(marker) && segment_len >= 7 {
            let height = u16::from_be_bytes(bytes[index + 3..index + 5].try_into().ok()?) as u32;
            let width = u16::from_be_bytes(bytes[index + 5..index + 7].try_into().ok()?) as u32;
            return Some((width, height));
        }
        index += segment_len;
    }

    None
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

fn gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    let width = u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32;
    let height = u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32;
    Some((width, height))
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WEBP" {
        return None;
    }
    match &bytes[12..16] {
        b"VP8X" if bytes.len() >= 30 => {
            let width = 1
                + u32::from(bytes[24])
                + (u32::from(bytes[25]) << 8)
                + (u32::from(bytes[26]) << 16);
            let height = 1
                + u32::from(bytes[27])
                + (u32::from(bytes[28]) << 8)
                + (u32::from(bytes[29]) << 16);
            Some((width, height))
        }
        b"VP8L" if bytes.len() >= 25 => {
            let b0 = u32::from(bytes[21]);
            let b1 = u32::from(bytes[22]);
            let b2 = u32::from(bytes[23]);
            let b3 = u32::from(bytes[24]);
            let width = 1 + (((b1 & 0x3f) << 8) | b0);
            let height = 1 + (((b3 & 0x0f) << 10) | (b2 << 2) | ((b1 & 0xc0) >> 6));
            Some((width, height))
        }
        b"VP8 " if bytes.len() >= 30 && bytes[23..26] == [0x9d, 0x01, 0x2a] => {
            let width = u16::from_le_bytes(bytes[26..28].try_into().ok()?) as u32 & 0x3fff;
            let height = u16::from_le_bytes(bytes[28..30].try_into().ok()?) as u32 & 0x3fff;
            Some((width, height))
        }
        _ => None,
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
        count_all_tokens_local(None, &messages, None)
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

    #[test]
    fn local_count_uses_multimodal_estimate_for_base64_images() {
        let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1024u32.to_be_bytes());
        png.extend_from_slice(&768u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]);
        let data = BASE64_STANDARD.encode(png);
        let tokens = estimate(vec![Message {
            role: "user".to_string(),
            content: json!([{
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": data
                }
            }]),
        }]);

        assert!(
            (1_000..1_200).contains(&tokens),
            "1024x768 image should be counted by visual estimate, got {tokens}"
        );
    }

    #[test]
    fn local_count_does_not_treat_large_image_base64_as_text_context() {
        let data = BASE64_STANDARD.encode(vec![b'a'; 2 * 1024 * 1024]);
        let tokens = estimate(vec![Message {
            role: "user".to_string(),
            content: json!([{
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": data
                }
            }]),
        }]);

        assert_eq!(tokens, DEFAULT_IMAGE_TOKENS);
    }

    #[test]
    fn local_count_handles_data_url_images() {
        let mut png = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&400u32.to_be_bytes());
        png.extend_from_slice(&300u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]);
        let data_url = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png));
        let tokens = estimate(vec![Message {
            role: "user".to_string(),
            content: json!([{
                "type": "image",
                "source": {
                    "type": "url",
                    "url": data_url
                }
            }]),
        }]);

        assert!(
            (150..180).contains(&tokens),
            "400x300 image should be counted by visual estimate, got {tokens}"
        );
    }
}
