//! Anthropic content block conversion, including images, documents, and tool results.

use std::cell::Cell;
use std::sync::{Condvar, Mutex, Once, OnceLock};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use sha2::{Digest, Sha256};

use crate::anthropic::types::{ContentBlock, ImageSource};
use crate::kiro::model::requests::conversation::KiroImage;
use crate::kiro::model::requests::tool::ToolResult;

use super::{ConversionError, EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER};

const TOOL_RESULT_IMAGE_PLACEHOLDER: &str = "[image attached]";

/// The upstream only accepts inline base64 image bytes (verified 2026-10-22 with a real account:
/// `source.url` and `source.s3Location` both return 400). Remote URLs therefore only work when
/// `imageProcessing.safeDownloadRemoteSources` materializes them before conversion.
pub(crate) const REMOTE_IMAGE_URL_UNSUPPORTED_MESSAGE: &str =
    "remote image URL sources are not supported; send the image as base64 data";

/// Text that replaces a remote URL image in history, so one old link does not fail every later
/// turn of the session.
const HISTORY_REMOTE_IMAGE_PLACEHOLDER: &str = "[image omitted: remote image URLs are not supported; ask the user to send the image as base64 data]";

/// How images in a user message that cannot be sent upstream are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UserImageMode {
    /// The current turn: the user is asking about this image, so fail explicitly.
    Strict,
    /// History: replace an unsendable remote URL image with a text note.
    History,
}

/// 处理消息内容，提取文本、图片和工具结果
pub(super) fn process_message_content(
    content: &serde_json::Value,
) -> Result<(String, Vec<KiroImage>, Vec<ToolResult>), ConversionError> {
    process_message_content_with_mode(content, UserImageMode::Strict)
}

pub(super) fn process_message_content_with_mode(
    content: &serde_json::Value,
    image_mode: UserImageMode,
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
                match item.get("type").and_then(|value| value.as_str()) {
                    Some("document") => {
                        let document_text = document_block_to_text(item)?;
                        if !document_text.is_empty() {
                            text_parts.push(document_text);
                        }
                        continue;
                    }
                    // Server-tool and `search_result` blocks are rendered from raw JSON: for
                    // example `search_result.source` is a string, which ContentBlock rejects.
                    _ => {
                        if let Some(text) = render_server_tool_block(item) {
                            text_parts.push(text);
                            continue;
                        }
                    }
                }
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
                            if image_mode == UserImageMode::History
                                && is_remote_image_url_source(&source)
                            {
                                tracing::warn!(
                                    "历史消息中的远程 URL 图片无法发送，已替换为占位文本"
                                );
                                text_parts.push(HISTORY_REMOTE_IMAGE_PLACEHOLDER.to_string());
                                continue;
                            }
                            images.push(convert_image_source(source)?);
                        }
                        "tool_result" => {
                            if let Some(tool_use_id) =
                                block.tool_use_id.as_deref().and_then(sanitize_tool_use_id)
                            {
                                let (result_images, omitted_images) =
                                    extract_tool_result_images(&block.content);
                                images.extend(result_images);
                                let mut result_text = extract_tool_result_content(&block.content);
                                for note in omitted_images {
                                    if !result_text.is_empty() {
                                        result_text.push('\n');
                                    }
                                    result_text.push_str(&note);
                                }
                                let result_content = normalize_tool_result_content(result_text);
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

/// A `url` image source that is not an inline data URL (i.e. it was not downloaded).
fn is_remote_image_url_source(source: &ImageSource) -> bool {
    source.source_type == "url"
        && source
            .url
            .as_deref()
            .is_some_and(|url| parse_data_url(url).is_none())
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
                    REMOTE_IMAGE_URL_UNSUPPORTED_MESSAGE.to_string(),
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
    install_pdf_extract_panic_filter();
    let _slot = PdfExtractSlot::acquire();
    let _scope = PdfExtractPanicScope::enter();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(bytes)
    }))
    .map_err(|_| ())
}

thread_local! {
    static IN_PDF_EXTRACT: Cell<bool> = const { Cell::new(false) };
}

/// 进程内同时执行 pdf-extract 的上限。PDF 抽取是纯 CPU + 内存工作，单个大 PDF 可能占用数百
/// 毫秒到数秒；不设上限时一批大 PDF 会同时把所有核心和内存打满。早期实现用全局互斥锁串行化，
/// 现在放宽到 CPU 核数：并发 PDF 请求不再互相串行，但总量仍然有界。
fn pdf_extract_max_concurrency() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2)
            .clamp(1, 16)
    })
}

struct PdfExtractGate {
    running: Mutex<usize>,
    available: Condvar,
}

fn pdf_extract_gate() -> &'static PdfExtractGate {
    static GATE: OnceLock<PdfExtractGate> = OnceLock::new();
    GATE.get_or_init(|| PdfExtractGate {
        running: Mutex::new(0),
        available: Condvar::new(),
    })
}

/// 占用一个 pdf-extract 并发名额；drop 时（包括 panic 展开）归还并唤醒一个等待者。
/// 调用方都在同步转换路径上：大请求已经通过 `block_in_place` 移出 tokio worker。
struct PdfExtractSlot;

impl PdfExtractSlot {
    fn acquire() -> Self {
        let gate = pdf_extract_gate();
        let limit = pdf_extract_max_concurrency();
        let mut running = gate
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *running >= limit {
            running = gate
                .available
                .wait(running)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *running += 1;
        Self
    }
}

impl Drop for PdfExtractSlot {
    fn drop(&mut self) {
        let gate = pdf_extract_gate();
        let mut running = gate
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *running = running.saturating_sub(1);
        drop(running);
        gate.available.notify_one();
    }
}

/// 标记当前线程正在执行 pdf-extract；drop 时（包括 panic 展开）清除标记。
struct PdfExtractPanicScope {
    previous: bool,
}

impl PdfExtractPanicScope {
    fn enter() -> Self {
        Self {
            previous: IN_PDF_EXTRACT.with(|flag| flag.replace(true)),
        }
    }
}

impl Drop for PdfExtractPanicScope {
    fn drop(&mut self) {
        IN_PDF_EXTRACT.with(|flag| flag.set(self.previous));
    }
}

/// 进程内只安装一次的 panic hook：只静默当前线程处于 pdf-extract 调用期间、且源自
/// pdf-extract 的 panic（它们会被 `catch_unwind` 转成回退解析），其余 panic 一律交给原 hook。
///
/// 早期实现每次抽取都 take/set 全局 hook 并持有全局锁，导致并发 PDF 请求串行，且替换期间
/// 其他线程的 panic 也会经过临时 hook。前提：本仓库只有这里调用 `std::panic::set_hook`，
/// 安装后不会被别处覆盖。
fn install_pdf_extract_panic_filter() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let in_pdf_extract = IN_PDF_EXTRACT.try_with(Cell::get).unwrap_or(false);
            let location = info.location().map(|location| location.file());
            if should_suppress_pdf_extract_panic(in_pdf_extract, location) {
                return;
            }
            previous(info);
        }));
    });
}

fn should_suppress_pdf_extract_panic(in_pdf_extract: bool, location_file: Option<&str>) -> bool {
    in_pdf_extract && location_file.is_some_and(|file| file.contains("pdf-extract"))
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
    // The trailer checks are advisory only: valid JPEGs often carry bytes after the EOI
    // marker, and Kiro decides whether an image is usable (answering an official 400 for
    // truly broken ones), so a local mismatch is logged instead of rejecting the request.
    if !image_bytes_are_structurally_valid(detected, &bytes) {
        tracing::warn!(
            declared_media_type = %media_type,
            detected_format = detected,
            image_bytes = bytes.len(),
            "图片字节未通过轻量结构校验，仍按识别格式转发，由上游判断"
        );
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
///
/// Claude Code 协议允许 `tool_result.content` 包含 text、image、document、search_result。
/// Kiro 的 toolResult 只承载文本，所以 document 与顶层一样转换为文本，search_result
/// 渲染为带来源的文本；其他未知块只保留简短占位，避免把整块 JSON/base64 塞给模型。
fn extract_tool_result_content(content: &Option<serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            let mut parts = Vec::new();
            for item in arr {
                match item.get("type").and_then(|value| value.as_str()) {
                    Some("image") => parts.push(TOOL_RESULT_IMAGE_PLACEHOLDER.to_string()),
                    Some("document") => match document_block_to_text(item) {
                        Ok(text) if !text.is_empty() => parts.push(text),
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(%error, "tool_result document could not be converted");
                            parts.push(format!("[document omitted: {error}]"));
                        }
                    },
                    Some("search_result") => parts.push(render_search_result_block(item)),
                    _ => {
                        if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                            parts.push(text.to_string());
                        } else if let Some(block_type) =
                            item.get("type").and_then(|value| value.as_str())
                        {
                            parts.push(format!("[{block_type} block omitted]"));
                        } else if !item.is_null() {
                            parts.push(item.to_string());
                        }
                    }
                }
            }
            parts.join("\n")
        }
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// Converts a Claude Code `document` block (top-level or nested in a tool_result) to text,
/// including the `content` source form whose payload is an array of text blocks.
fn document_block_to_text(item: &serde_json::Value) -> Result<String, ConversionError> {
    let source = item.get("source").ok_or_else(|| {
        ConversionError::UnsupportedContent("document block missing source".to_string())
    })?;
    let text = if source.get("type").and_then(|value| value.as_str()) == Some("content") {
        let text = match source.get("content") {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(|value| value.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if text.is_empty() {
            return Ok(String::new());
        }
        format_document_text("text/plain", text)
    } else {
        let block = serde_json::from_value::<ContentBlock>(item.clone()).map_err(|error| {
            ConversionError::UnsupportedContent(format!("invalid document block: {error}"))
        })?;
        let source = block.source.ok_or_else(|| {
            ConversionError::UnsupportedContent("document block missing source".to_string())
        })?;
        convert_document_source_to_text(source)?
    };
    let title = item
        .get("title")
        .and_then(|value| value.as_str())
        .filter(|title| !title.trim().is_empty());
    let context = item
        .get("context")
        .and_then(|value| value.as_str())
        .filter(|context| !context.trim().is_empty());
    let mut header = String::new();
    if let Some(title) = title {
        header.push_str(&format!("Document title: {title}\n"));
    }
    if let Some(context) = context {
        header.push_str(&format!("Document context: {context}\n"));
    }
    Ok(format!("{header}{text}"))
}

const SERVER_TOOL_TEXT_MAX_CHARS: usize = 8_000;
const SERVER_TOOL_MAX_RESULTS: usize = 20;
const SERVER_TOOL_FIELD_MAX_CHARS: usize = 512;

fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((index, _)) => format!("{}...", &text[..index]),
        None => text.to_string(),
    }
}

/// Renders Claude Code server-tool blocks (web search/fetch) as bounded plain text so replayed
/// history keeps their context after conversion to Kiro, which has no server-tool block type.
/// Returns `None` for other block types. `encrypted_content` is never rendered.
pub(super) fn render_server_tool_block(item: &serde_json::Value) -> Option<String> {
    let field = |value: &serde_json::Value, key: &str| {
        truncate_chars(
            value.get(key).and_then(|v| v.as_str()).unwrap_or_default(),
            SERVER_TOOL_FIELD_MAX_CHARS,
        )
    };
    let rendered = match item.get("type")?.as_str()? {
        "server_tool_use" => {
            let name = field(item, "name");
            let input = item.get("input").cloned().unwrap_or_default();
            match input.get("query").and_then(|v| v.as_str()) {
                Some(query) => format!(
                    "[{name}] query: {}",
                    truncate_chars(query, SERVER_TOOL_FIELD_MAX_CHARS)
                ),
                None => format!(
                    "[{name}] input: {}",
                    truncate_chars(&input.to_string(), SERVER_TOOL_FIELD_MAX_CHARS)
                ),
            }
        }
        "web_search_tool_result" => match item.get("content") {
            Some(serde_json::Value::Array(results)) => {
                let mut lines = vec!["[web_search results]".to_string()];
                for (index, result) in results.iter().take(SERVER_TOOL_MAX_RESULTS).enumerate() {
                    let page_age = field(result, "page_age");
                    lines.push(format!(
                        "{}. {} - {}{}",
                        index + 1,
                        field(result, "title"),
                        field(result, "url"),
                        if page_age.is_empty() {
                            String::new()
                        } else {
                            format!(" ({page_age})")
                        }
                    ));
                }
                if results.len() > SERVER_TOOL_MAX_RESULTS {
                    lines.push(format!(
                        "... {} more results omitted",
                        results.len() - SERVER_TOOL_MAX_RESULTS
                    ));
                }
                lines.join("\n")
            }
            Some(error) => format!("[web_search error: {}]", field(error, "error_code")),
            None => "[web_search results]".to_string(),
        },
        "web_fetch_tool_result" => {
            let content = item.get("content").cloned().unwrap_or_default();
            let url = field(&content, "url");
            let document = content
                .get("content")
                .filter(|document| {
                    document.get("type").and_then(|v| v.as_str()) == Some("document")
                })
                .and_then(|document| document_block_to_text(document).ok())
                .unwrap_or_default();
            if let Some(code) = content.get("error_code").and_then(|v| v.as_str()) {
                format!("[web_fetch error: {code}]")
            } else {
                format!("[web_fetch {url}]\n{document}")
            }
        }
        "search_result" => render_search_result_block(item),
        _ => return None,
    };
    Some(truncate_chars(&rendered, SERVER_TOOL_TEXT_MAX_CHARS))
}

/// Renders a Claude Code `search_result` block as plain text with its source and title.
fn render_search_result_block(item: &serde_json::Value) -> String {
    let source = item
        .get("source")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let title = item
        .get("title")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let body = match item.get("content") {
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(|value| value.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(serde_json::Value::String(text)) => text.clone(),
        _ => String::new(),
    };
    format!(
        "<search_result source=\"{}\" title=\"{}\">\n{}\n</search_result>",
        source.replace('"', "&quot;"),
        title.replace('"', "&quot;"),
        body
    )
}

/// Converts the images inside a tool_result.
///
/// An image a tool returned (a screenshot, a rendered file) stays in the conversation history, so
/// one unusable image would make every later turn of that session fail. Unusable images are
/// replaced by a short text note in the tool result instead; images in the user's own message
/// still fail the request explicitly.
fn extract_tool_result_images(
    content: &Option<serde_json::Value>,
) -> (Vec<KiroImage>, Vec<String>) {
    let Some(serde_json::Value::Array(items)) = content else {
        return (Vec::new(), Vec::new());
    };

    let mut images = Vec::new();
    let mut omitted = Vec::new();
    for item in items {
        if item.get("type").and_then(|value| value.as_str()) != Some("image") {
            continue;
        }
        let converted = serde_json::from_value::<ContentBlock>(item.clone())
            .map_err(|error| {
                ConversionError::UnsupportedContent(format!("invalid image block: {error}"))
            })
            .and_then(|block| {
                block.source.ok_or_else(|| {
                    ConversionError::UnsupportedContent("image block missing source".to_string())
                })
            })
            .and_then(convert_image_source);
        match converted {
            Ok(image) => images.push(image),
            Err(error) => {
                tracing::warn!(error = %error, "tool_result image could not be converted; replaced with a text note");
                omitted.push(format!("[image omitted: {error}]"));
            }
        }
    }
    (images, omitted)
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

/// Longest toolUseId sent upstream. Production 2026-10-21: one 100-character
/// `call_<hex>_fc_<hex>` id from an OpenAI-style client sat in a history where every other id
/// was at most 64 characters, and Kiro rejected every resend with "Invalid tool use format".
/// Longer ids are shortened deterministically, so a tool_use and its tool_result still match.
const MAX_TOOL_USE_ID_CHARS: usize = 64;

pub(super) fn sanitize_tool_use_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return None;
    }
    let allowed = |ch: char| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
    if trimmed.len() <= MAX_TOOL_USE_ID_CHARS && trimmed.chars().all(allowed) {
        return Some(trimmed.to_string());
    }

    let sanitized = trimmed
        .chars()
        .map(|ch| if allowed(ch) { ch } else { '_' })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('_');
    let mut hasher = Sha256::new();
    hasher.update(trimmed.as_bytes());
    let digest = hasher.finalize();
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    );
    // Keep a readable prefix and leave room for "_" + 8 hex chars.
    let max_prefix = MAX_TOOL_USE_ID_CHARS - suffix.len() - 1;
    let prefix: String = if sanitized.is_empty() {
        "toolu".to_string()
    } else {
        sanitized.chars().take(max_prefix).collect()
    };
    Some(format!("{}_{}", prefix.trim_end_matches('_'), suffix))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    const SIMPLE_PDF: &[u8] = b"%PDF-1.1\nBT /F1 12 Tf 20 100 Td (kiro-pdf-test) Tj ET\n%%EOF";
    /// 页面对象缺 MediaBox 等必需字段的畸形 PDF，用于覆盖 pdf-extract 内部 panic/错误分支。
    const MALFORMED_PDF: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R>>endobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000052 00000 n \n0000000101 00000 n \ntrailer<</Size 4/Root 1 0 R>>\nstartxref\n145\n%%EOF";

    #[test]
    fn concurrent_pdf_extraction_is_correct_and_not_serialized_by_a_global_lock() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..20 {
                        let text = extract_text_from_pdf_bytes(SIMPLE_PDF)
                            .expect("simple PDF text should be extracted");
                        assert!(text.contains("kiro-pdf-test"));
                        // 畸形 PDF 无论走 panic 还是错误分支，都必须安全返回而不是传播 panic。
                        let _ = extract_text_from_pdf_bytes(MALFORMED_PDF);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .expect("PDF extraction must never propagate a panic");
        }
    }

    #[test]
    fn pdf_extract_slots_never_exceed_the_concurrency_limit() {
        let limit = pdf_extract_max_concurrency();
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..limit * 4)
            .map(|_| {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                std::thread::spawn(move || {
                    let _slot = PdfExtractSlot::acquire();
                    let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("slot holder must not panic");
        }
        let peak = peak.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            peak >= 1 && peak <= limit,
            "peak {peak} exceeded limit {limit}"
        );

        // panic 展开时名额也必须归还，否则之后的 PDF 请求会永久阻塞。
        let _ = std::panic::catch_unwind(|| {
            let _slot = PdfExtractSlot::acquire();
            panic!("release the slot while unwinding");
        });
        let running = *pdf_extract_gate()
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(running < limit, "slot leaked after unwinding: {running}");
    }

    #[test]
    fn pdf_panic_filter_only_suppresses_pdf_extract_panics_inside_the_scope() {
        let pdf_file = Some("/registry/src/pdf-extract-0.7.12/src/lib.rs");
        let other_file = Some("src/anthropic/handlers.rs");
        assert!(should_suppress_pdf_extract_panic(true, pdf_file));
        assert!(!should_suppress_pdf_extract_panic(false, pdf_file));
        assert!(!should_suppress_pdf_extract_panic(true, other_file));
        assert!(!should_suppress_pdf_extract_panic(true, None));
    }

    #[test]
    fn pdf_extract_scope_flag_is_restored_on_return_nesting_and_unwind() {
        let flags = std::thread::spawn(|| {
            let before = IN_PDF_EXTRACT.with(Cell::get);
            let nested_restored = {
                let _outer = PdfExtractPanicScope::enter();
                drop(PdfExtractPanicScope::enter());
                IN_PDF_EXTRACT.with(Cell::get)
            };
            let after_return = IN_PDF_EXTRACT.with(Cell::get);
            let _ = std::panic::catch_unwind(|| {
                let _scope = PdfExtractPanicScope::enter();
                panic!("unwinding through the pdf scope");
            });
            let after_unwind = IN_PDF_EXTRACT.with(Cell::get);
            (before, nested_restored, after_return, after_unwind)
        })
        .join()
        .expect("thread should not die");
        assert_eq!(flags, (false, true, false, false));
    }

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
    fn long_tool_use_ids_are_shortened_deterministically() {
        // Captured from production: 100-character id from an OpenAI-style client.
        let long = "call_ce6ce2f9833f4533ad4473dea889819a_fc_04b64eaef0ee9346016ad87fd852248191a6166ad01de2a30d_2349e5e8";
        assert_eq!(long.len(), 100);
        let short = sanitize_tool_use_id(long).expect("id");
        assert!(short.len() <= MAX_TOOL_USE_ID_CHARS, "{short}");
        assert!(
            short.starts_with("call_ce6ce2f9833f4533ad4473de"),
            "{short}"
        );
        assert!(
            short
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        );
        assert_eq!(
            sanitize_tool_use_id(long),
            Some(short.clone()),
            "deterministic"
        );
        let other = format!("{long}x");
        assert_ne!(
            sanitize_tool_use_id(&other),
            Some(short),
            "distinct ids stay distinct"
        );

        let at_limit = "a".repeat(MAX_TOOL_USE_ID_CHARS);
        assert_eq!(
            sanitize_tool_use_id(&at_limit).as_deref(),
            Some(at_limit.as_str())
        );
        assert_eq!(
            sanitize_tool_use_id("toolu_01ABC").as_deref(),
            Some("toolu_01ABC")
        );
    }

    #[test]
    fn long_tool_use_id_stays_paired_across_tool_use_and_tool_result() {
        let long = "call_".to_string() + &"f".repeat(120);
        let content = json!([{"type": "tool_result", "tool_use_id": long, "content": "ok"}]);
        let (_, _, results) = process_message_content(&content).expect("converted");
        assert_eq!(results.len(), 1);
        assert_eq!(
            Some(results[0].tool_use_id.clone()),
            sanitize_tool_use_id(&long)
        );
    }

    #[test]
    fn unusable_tool_result_image_becomes_a_text_note_instead_of_failing() {
        // A bad image a tool returned stays in history; failing on it would break every later
        // turn of the session. It is replaced by a note and the remaining content is kept.
        let content = json!([
            {
                "type": "tool_result",
                "tool_use_id": "toolu_123",
                "content": [
                    {"type": "text", "text": "screenshot taken"},
                    {
                        "type": "image",
                        "source": {"type": "base64", "media_type": "image/png", "data": ""}
                    },
                    {
                        "type": "image",
                        "source": {"type": "base64", "media_type": "image/bmp", "data": "Qk0="}
                    },
                    {"type": "image"}
                ]
            }
        ]);

        let (_, images, tool_results) = process_message_content(&content)
            .expect("tool_result images must not fail the request");
        assert!(images.is_empty());
        assert_eq!(tool_results.len(), 1);
        let text = serde_json::to_string(&tool_results[0]).expect("tool result json");
        assert!(text.contains("screenshot taken"), "{text}");
        assert_eq!(text.matches("[image omitted:").count(), 3, "{text}");
    }

    #[test]
    fn remote_url_image_in_history_becomes_placeholder_but_current_turn_stays_strict() {
        let content = json!([
            {"type": "text", "text": "look at this"},
            {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}},
            {"type": "image", "source": {"type": "url", "url": "data:image/png;base64,iVBORw0KGgo="}}
        ]);
        for round in 0..5 {
            let (text, images, _) =
                process_message_content_with_mode(&content, UserImageMode::History)
                    .expect("history URL image must not fail the request");
            assert!(text.starts_with("look at this"), "round {round}: {text}");
            assert!(
                text.contains(HISTORY_REMOTE_IMAGE_PLACEHOLDER),
                "round {round}"
            );
            assert!(
                !text.contains("example.com"),
                "round {round}: URL is not echoed"
            );
            assert_eq!(
                images.len(),
                1,
                "round {round}: inline data URL is still sent"
            );

            let err = process_message_content(&content).expect_err("current turn stays strict");
            match err {
                ConversionError::UnsupportedContent(message) => {
                    assert_eq!(message, REMOTE_IMAGE_URL_UNSUPPORTED_MESSAGE)
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn empty_user_image_is_still_rejected_with_clear_error() {
        let content = json!([
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": ""}}
        ]);
        let err = process_message_content(&content).expect_err("user image stays strict");
        assert_empty_image_error(err);
    }
}
