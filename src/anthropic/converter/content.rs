//! Anthropic content block conversion, including images, documents, and tool results.

use std::sync::{Arc, Mutex, OnceLock};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use sha2::{Digest, Sha256};

use crate::anthropic::types::{ContentBlock, ImageSource};
use crate::kiro::model::requests::conversation::KiroImage;
use crate::kiro::model::requests::tool::ToolResult;

use super::{ConversionError, EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER};

const TOOL_RESULT_IMAGE_PLACEHOLDER: &str = "[image attached]";

/// 处理消息内容，提取文本、图片和工具结果
pub(super) fn process_message_content(
    content: &serde_json::Value,
) -> Result<(String, Vec<KiroImage>, Vec<ToolResult>), ConversionError> {
    let mut text_parts = Vec::new();
    let mut images = Vec::new();
    let mut tool_results = Vec::new();

    match content {
        serde_json::Value::String(s) => {
            text_parts.push(s.clone());
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Ok(block) = serde_json::from_value::<ContentBlock>(item.clone()) {
                    match block.block_type.as_str() {
                        "text" => {
                            if let Some(text) = block.text {
                                text_parts.push(text);
                            }
                        }
                        "image" => {
                            let source = block.source.ok_or_else(|| {
                                ConversionError::UnsupportedContent(
                                    "image block missing source".to_string(),
                                )
                            })?;
                            images.push(convert_image_source(source)?);
                        }
                        "document" => {
                            let source = block.source.ok_or_else(|| {
                                ConversionError::UnsupportedContent(
                                    "document block missing source".to_string(),
                                )
                            })?;
                            let document_text = convert_document_source_to_text(source)?;
                            if !document_text.is_empty() {
                                text_parts.push(document_text);
                            }
                        }
                        "tool_result" => {
                            if let Some(tool_use_id) =
                                block.tool_use_id.as_deref().and_then(sanitize_tool_use_id)
                            {
                                images.extend(extract_tool_result_images(&block.content)?);
                                let result_content = normalize_tool_result_content(
                                    extract_tool_result_content(&block.content),
                                );
                                let is_error = block.is_error.unwrap_or(false);

                                let mut result = if is_error {
                                    ToolResult::error(tool_use_id.clone(), result_content)
                                } else {
                                    ToolResult::success(tool_use_id.clone(), result_content)
                                };
                                result.status =
                                    Some(if is_error { "error" } else { "success" }.to_string());

                                tool_results.push(result);
                            }
                        }
                        "tool_use" => {
                            // tool_use 在 assistant 消息中处理，这里忽略
                        }
                        "redacted_thinking" => {
                            tracing::debug!(
                                "用户消息中的 redacted_thinking 无法传递给当前 Kiro upstream，已跳过"
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    Ok((text_parts.join("\n"), images, tool_results))
}

fn convert_image_source(source: ImageSource) -> Result<KiroImage, ConversionError> {
    match source.source_type.as_str() {
        "base64" => {
            let data = source.data.ok_or_else(|| {
                ConversionError::UnsupportedContent("base64 image source missing data".to_string())
            })?;
            let media_type = source
                .media_type
                .or_else(|| parse_data_url(&data).map(|(media_type, _)| media_type))
                .ok_or_else(|| {
                    ConversionError::UnsupportedContent(
                        "base64 image source missing media_type".to_string(),
                    )
                })?;
            let (media_type, data) = normalize_inline_base64_source(&media_type, &data);
            if data.is_empty() {
                return Err(ConversionError::UnsupportedContent(
                    empty_image_source_message(&media_type),
                ));
            }
            let format =
                image_format_from_base64_or_media_type(&media_type, &data).ok_or_else(|| {
                    ConversionError::UnsupportedContent(invalid_image_source_message(&media_type))
                })?;
            Ok(KiroImage::from_base64(format, data))
        }
        "url" => {
            let url = source.url.ok_or_else(|| {
                ConversionError::UnsupportedContent("image URL source missing url".to_string())
            })?;
            if let Some((media_type, data)) = parse_data_url(&url) {
                let media_type = normalize_media_type(&media_type);
                let data = strip_base64_whitespace(&data);
                if data.is_empty() {
                    return Err(ConversionError::UnsupportedContent(
                        empty_image_source_message(&media_type),
                    ));
                }
                let format = image_format_from_base64_or_media_type(&media_type, &data)
                    .ok_or_else(|| {
                        ConversionError::UnsupportedContent(invalid_image_source_message(
                            &media_type,
                        ))
                    })?;
                Ok(KiroImage::from_base64(format, data))
            } else {
                Err(ConversionError::UnsupportedContent(
                    "remote image URL source was not materialized before conversion".to_string(),
                ))
            }
        }
        "file" | "file_id" => Err(ConversionError::UnsupportedContent(
            "image file source was not materialized before conversion".to_string(),
        )),
        other => Err(ConversionError::UnsupportedContent(format!(
            "unsupported image source type: {}",
            other
        ))),
    }
}

fn normalize_inline_base64_source(media_type: &str, data: &str) -> (String, String) {
    if let Some((data_url_media_type, data_url_data)) = parse_data_url(data) {
        return (
            normalize_media_type(&data_url_media_type),
            strip_base64_whitespace(&data_url_data),
        );
    }
    (
        normalize_media_type(media_type),
        strip_base64_whitespace(data),
    )
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

fn convert_document_source_to_text(source: ImageSource) -> Result<String, ConversionError> {
    match source.source_type.as_str() {
        "text" => {
            let media_type = source
                .media_type
                .unwrap_or_else(|| "text/plain".to_string());
            let data = source.data.ok_or_else(|| {
                ConversionError::UnsupportedContent("text document source missing data".to_string())
            })?;
            Ok(format_document_text(&media_type, data))
        }
        "base64" => {
            let data = source.data.ok_or_else(|| {
                ConversionError::UnsupportedContent(
                    "base64 document source missing data".to_string(),
                )
            })?;
            let media_type = source
                .media_type
                .or_else(|| parse_data_url(&data).map(|(media_type, _)| media_type))
                .ok_or_else(|| {
                    ConversionError::UnsupportedContent(
                        "base64 document source missing media_type".to_string(),
                    )
                })?;
            let (media_type, data) = normalize_inline_base64_source(&media_type, &data);
            decode_document_to_text(&media_type, &data)
        }
        "url" => {
            let url = source.url.ok_or_else(|| {
                ConversionError::UnsupportedContent("document URL source missing url".to_string())
            })?;
            if let Some((media_type, data)) = parse_data_url(&url) {
                decode_document_to_text(&media_type, &data)
            } else {
                Err(ConversionError::UnsupportedContent(
                    "remote document URL source was not materialized before conversion".to_string(),
                ))
            }
        }
        "file" | "file_id" => Err(ConversionError::UnsupportedContent(
            "document file source was not materialized before conversion".to_string(),
        )),
        other => Err(ConversionError::UnsupportedContent(format!(
            "unsupported document source type: {}",
            other
        ))),
    }
}

fn decode_document_to_text(media_type: &str, data: &str) -> Result<String, ConversionError> {
    let bytes = BASE64_STANDARD.decode(data).map_err(|_| {
        ConversionError::UnsupportedContent(format!(
            "base64 document source contains invalid data for {}",
            media_type
        ))
    })?;

    let text = match media_type {
        "text/plain" | "text/markdown" | "text/html" | "text/csv" | "application/json" => {
            String::from_utf8(bytes).map_err(|_| {
                ConversionError::UnsupportedContent(format!(
                    "document media_type {} is not valid UTF-8 text",
                    media_type
                ))
            })?
        }
        "application/pdf" => extract_text_from_pdf_bytes(&bytes).ok_or_else(|| {
            ConversionError::UnsupportedContent(
                "PDF document text could not be extracted (encrypted, image-only, or malformed PDF)"
                    .to_string(),
            )
        })?,
        _ => {
            return Err(ConversionError::UnsupportedContent(format!(
                "unsupported document media_type: {}",
                media_type
            )));
        }
    };

    Ok(format_document_text(media_type, text))
}

fn format_document_text(media_type: &str, text: String) -> String {
    format!(
        "<document media_type=\"{}\">\n{}\n</document>",
        media_type, text
    )
}

fn extract_text_from_pdf_bytes(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"%PDF") {
        return None;
    }

    // 优先使用 pdf-extract（支持压缩流、字体编码、布局）
    match extract_pdf_text_with_panic_guard(bytes) {
        Ok(Ok(text)) => {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
            tracing::debug!("pdf-extract 返回空文本，尝试简易解析回退");
        }
        Ok(Err(err)) => {
            tracing::debug!("pdf-extract 抽取失败，回退到简易解析: {}", err);
        }
        Err(_) => {
            tracing::warn!("pdf-extract 抽取过程发生 panic，回退到简易解析");
        }
    }

    extract_text_from_pdf_bytes_fallback(bytes)
}

fn extract_pdf_text_with_panic_guard(
    bytes: &[u8],
) -> Result<Result<String, pdf_extract::OutputError>, ()> {
    let _guard = pdf_extract_panic_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous_hook = std::panic::take_hook();
    let previous_hook_slot = Arc::new(Mutex::new(Some(previous_hook)));
    let hook_slot = Arc::clone(&previous_hook_slot);
    std::panic::set_hook(Box::new(move |info| {
        if is_pdf_extract_panic(info) {
            return;
        }
        if let Some(hook) = hook_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            hook(info);
        }
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }));
    let previous_hook = previous_hook_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .unwrap_or_else(|| Box::new(|_| {}));
    std::panic::set_hook(previous_hook);
    result.map_err(|_| ())
}

fn is_pdf_extract_panic(info: &std::panic::PanicHookInfo<'_>) -> bool {
    info.location()
        .is_some_and(|location| location.file().contains("pdf-extract"))
}

fn pdf_extract_panic_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 简易 PDF 文本抽取兜底：仅处理未压缩的 `(...) Tj` / `TJ` 形态。
///
/// 当 pdf-extract 解析失败（坏 PDF、不支持的编码等）时使用，能力非常有限。
fn extract_text_from_pdf_bytes_fallback(bytes: &[u8]) -> Option<String> {
    let raw = String::from_utf8_lossy(bytes);
    if !raw.contains("%PDF") {
        return None;
    }

    let chars: Vec<char> = raw.chars().collect();
    let mut pieces = Vec::new();
    let mut idx = 0;

    while idx < chars.len() {
        if chars[idx] != '(' {
            idx += 1;
            continue;
        }

        let start = idx + 1;
        idx = start;
        let mut escaped = false;
        let mut piece = String::new();
        while idx < chars.len() {
            let ch = chars[idx];
            if escaped {
                piece.push(match ch {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'b' => '\u{0008}',
                    'f' => '\u{000c}',
                    '(' | ')' | '\\' => ch,
                    other => other,
                });
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == ')' {
                break;
            } else {
                piece.push(ch);
            }
            idx += 1;
        }

        if idx >= chars.len() {
            break;
        }

        let tail: String = chars.iter().skip(idx + 1).take(80).collect::<String>();
        if tail.contains("Tj") || tail.contains("TJ") || tail.contains('\'') || tail.contains('"') {
            let trimmed = piece.trim();
            if !trimmed.is_empty() {
                pieces.push(trimmed.to_string());
            }
        }

        idx += 1;
    }

    if pieces.is_empty() {
        None
    } else {
        Some(pieces.join("\n"))
    }
}

fn parse_data_url(url: &str) -> Option<(String, String)> {
    let data_part = url.strip_prefix("data:")?;
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

/// 从 media_type 获取图片格式
fn get_image_format(media_type: &str) -> Option<String> {
    match normalize_media_type(media_type).as_str() {
        "image/jpeg" => Some("jpeg".to_string()),
        "image/png" => Some("png".to_string()),
        "image/gif" => Some("gif".to_string()),
        "image/webp" => Some("webp".to_string()),
        _ => None,
    }
}

fn invalid_image_source_message(media_type: &str) -> String {
    if get_image_format(media_type).is_some() {
        format!("invalid image data for media_type: {}", media_type)
    } else {
        format!("unsupported image media_type: {}", media_type)
    }
}

fn empty_image_source_message(media_type: &str) -> String {
    format!("Image data cannot be empty. media_type={}", media_type)
}

fn normalize_media_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase()
}

fn image_format_from_base64_or_media_type(media_type: &str, data: &str) -> Option<String> {
    let declared = get_image_format(media_type);
    let bytes = match BASE64_STANDARD.decode(data) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(
                declared_media_type = %media_type,
                error = %err,
                "图片 base64 无法解码，已拒绝转发到上游"
            );
            return None;
        }
    };
    let detected = infer_image_format_from_bytes(&bytes)?;
    if !image_bytes_are_structurally_valid(detected, &bytes) {
        tracing::warn!(
            declared_media_type = %media_type,
            detected_format = detected,
            image_bytes = bytes.len(),
            "图片字节未通过轻量结构校验，已拒绝转发到上游"
        );
        return None;
    }
    if declared.as_deref().is_some_and(|value| value != detected) {
        tracing::warn!(
            declared_media_type = %media_type,
            detected_format = detected,
            "图片 media_type 与内容字节不一致，已按字节识别结果修正"
        );
    }
    Some(detected.to_string())
}

fn infer_image_format_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

fn image_bytes_are_structurally_valid(format: &str, bytes: &[u8]) -> bool {
    match format {
        "png" => png_bytes_have_iend(bytes),
        "jpeg" => jpeg_bytes_have_eoi(bytes),
        "gif" => gif_bytes_have_trailer(bytes),
        "webp" => webp_bytes_have_riff_payload(bytes),
        _ => false,
    }
}

fn png_bytes_have_iend(bytes: &[u8]) -> bool {
    const PNG_SIGNATURE_LEN: usize = 8;
    if !bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        return false;
    }
    let mut pos = PNG_SIGNATURE_LEN;
    let mut saw_ihdr = false;
    while pos.checked_add(12).is_some_and(|min| min <= bytes.len()) {
        let length =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                as usize;
        let chunk_type_start = pos + 4;
        let chunk_type_end = pos + 8;
        let Some(data_end) = chunk_type_end.checked_add(length) else {
            return false;
        };
        let Some(next_pos) = data_end.checked_add(4) else {
            return false;
        };
        if next_pos > bytes.len() {
            return false;
        }
        let chunk_type = &bytes[chunk_type_start..chunk_type_end];
        if !saw_ihdr {
            if chunk_type != b"IHDR" || length != 13 {
                return false;
            }
            saw_ihdr = true;
        }
        if chunk_type == b"IEND" {
            return saw_ihdr && length == 0 && next_pos == bytes.len();
        }
        pos = next_pos;
    }
    false
}

fn jpeg_bytes_have_eoi(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9])
}

fn gif_bytes_have_trailer(bytes: &[u8]) -> bool {
    bytes.len() >= 14
        && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
        && bytes.last() == Some(&0x3b)
}

fn webp_bytes_have_riff_payload(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if riff_size
        .checked_add(8)
        .is_none_or(|declared| declared > bytes.len())
    {
        return false;
    }
    matches!(&bytes[12..16], b"VP8 " | b"VP8L" | b"VP8X")
}

pub(crate) fn infer_image_format_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some("jpeg".to_string())
    } else if path.ends_with(".png") {
        Some("png".to_string())
    } else if path.ends_with(".gif") {
        Some("gif".to_string())
    } else if path.ends_with(".webp") {
        Some("webp".to_string())
    } else {
        None
    }
}

pub(crate) fn infer_document_media_type_from_url(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".pdf") {
        "application/pdf".to_string()
    } else if path.ends_with(".md") {
        "text/markdown".to_string()
    } else if path.ends_with(".html") || path.ends_with(".htm") {
        "text/html".to_string()
    } else if path.ends_with(".txt") {
        "text/plain".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// 提取工具结果内容
fn extract_tool_result_content(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts = Vec::new();
            for item in arr {
                if item.get("type").and_then(|value| value.as_str()) == Some("image") {
                    parts.push(TOOL_RESULT_IMAGE_PLACEHOLDER.to_string());
                } else if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                } else if !item.is_null() {
                    parts.push(item.to_string());
                }
            }
            parts.join("\n")
        }
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn extract_tool_result_images(
    content: &Option<serde_json::Value>,
) -> Result<Vec<KiroImage>, ConversionError> {
    let Some(serde_json::Value::Array(items)) = content else {
        return Ok(Vec::new());
    };

    let mut images = Vec::new();
    for item in items {
        if item.get("type").and_then(|value| value.as_str()) != Some("image") {
            continue;
        }
        let block = serde_json::from_value::<ContentBlock>(item.clone()).map_err(|error| {
            ConversionError::UnsupportedContent(format!(
                "invalid image block in tool_result: {error}"
            ))
        })?;
        let source = block.source.ok_or_else(|| {
            ConversionError::UnsupportedContent(
                "image block in tool_result missing source".to_string(),
            )
        })?;
        images.push(convert_image_source(source)?);
    }
    Ok(images)
}

fn normalize_tool_result_content(content: String) -> String {
    if content.trim().is_empty() {
        EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER.to_string()
    } else {
        content
    }
}

pub(super) fn normalize_tool_use_input(input: serde_json::Value) -> serde_json::Value {
    match input {
        serde_json::Value::Object(_) => input,
        serde_json::Value::Null => serde_json::json!({}),
        other => serde_json::json!({ "value": other }),
    }
}

pub(super) fn sanitize_tool_use_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Some(trimmed.to_string());
    }

    let sanitized = trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('_');
    let prefix = if sanitized.is_empty() {
        "toolu".to_string()
    } else {
        sanitized.to_string()
    };
    let mut hasher = Sha256::new();
    hasher.update(trimmed.as_bytes());
    let digest = hasher.finalize();
    Some(format!(
        "{}_{:02x}{:02x}{:02x}{:02x}",
        prefix, digest[0], digest[1], digest[2], digest[3]
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn assert_empty_image_error(err: ConversionError) {
        match err {
            ConversionError::UnsupportedContent(message) => {
                assert!(
                    message.contains("Image data cannot be empty."),
                    "unexpected error message: {message}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn empty_base64_image_is_rejected_with_clear_error() {
        let err = convert_image_source(ImageSource {
            source_type: "base64".to_string(),
            media_type: Some("image/png".to_string()),
            data: Some(" \n\t ".to_string()),
            url: None,
            file_id: None,
        })
        .expect_err("empty image data should be rejected");

        assert_empty_image_error(err);
    }

    #[test]
    fn empty_data_url_image_is_rejected_with_clear_error() {
        let err = convert_image_source(ImageSource {
            source_type: "url".to_string(),
            media_type: None,
            data: None,
            url: Some("data:image/png;base64, \n".to_string()),
            file_id: None,
        })
        .expect_err("empty data URL image should be rejected");

        assert_empty_image_error(err);
    }

    #[test]
    fn empty_tool_result_image_is_rejected_with_clear_error() {
        let content = json!([
            {
                "type": "tool_result",
                "tool_use_id": "toolu_123",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": ""
                        }
                    }
                ]
            }
        ]);

        let err = process_message_content(&content)
            .expect_err("empty tool_result image should be rejected");

        assert_empty_image_error(err);
    }
}
