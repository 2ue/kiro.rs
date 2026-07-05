use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::StreamExt;
use reqwest::header::{CONTENT_TYPE as REQWEST_CONTENT_TYPE, LOCATION as REQWEST_LOCATION};
use serde_json::{Value, json};
use std::time::Duration;

use crate::model::config::{ImageProcessingConfig, ImageProcessingMode};

use super::{
    converter::{infer_document_media_type_from_url, infer_image_format_from_url},
    files::{self, AnthropicFileStore},
    types::{Message, MessagesRequest},
};

const MAX_REMOTE_MULTIMODAL_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BodyProcessingReport {
    pub mode: ImageProcessingMode,
    pub materialized_file_sources: usize,
    pub materialized_remote_sources: usize,
    pub normalized_image_media_types: usize,
}

impl BodyProcessingReport {
    pub(crate) fn was_modified(self) -> bool {
        self.materialized_file_sources > 0
            || self.materialized_remote_sources > 0
            || self.normalized_image_media_types > 0
    }
}

pub(crate) async fn prepare_multimodal_sources(
    store: &AnthropicFileStore,
    payload: &mut MessagesRequest,
    caller_user_agent: Option<&str>,
    config: ImageProcessingConfig,
) -> Result<BodyProcessingReport, String> {
    prepare_multimodal_message_sources(store, &mut payload.messages, caller_user_agent, config)
        .await
}

pub(crate) async fn prepare_multimodal_message_sources(
    store: &AnthropicFileStore,
    messages: &mut [Message],
    caller_user_agent: Option<&str>,
    config: ImageProcessingConfig,
) -> Result<BodyProcessingReport, String> {
    let config = config.normalized();
    let mut report = BodyProcessingReport {
        mode: config.mode,
        ..BodyProcessingReport::default()
    };

    match config.mode {
        ImageProcessingMode::Safe => {
            if config.safe_materialize_file_sources {
                report.materialized_file_sources =
                    files::materialize_file_sources(store, messages)?;
            }
            if config.safe_download_remote_sources {
                report.materialized_remote_sources =
                    materialize_remote_multimodal_sources(messages, caller_user_agent).await?;
            }
            if config.safe_normalize_base64_media_types {
                report.normalized_image_media_types =
                    normalize_message_base64_image_media_types(messages);
            }
        }
        ImageProcessingMode::Light => {
            reject_non_inline_sources(messages)?;
        }
    }

    if report.was_modified() {
        tracing::debug!(
            mode = ?report.mode,
            materialized_file_sources = report.materialized_file_sources,
            materialized_remote_sources = report.materialized_remote_sources,
            normalized_image_media_types = report.normalized_image_media_types,
            "Anthropic request body multimodal preprocessing finished"
        );
    }

    Ok(report)
}

fn reject_non_inline_sources(messages: &[Message]) -> Result<(), String> {
    for message in messages {
        reject_non_inline_sources_in_content(&message.content)?;
    }
    Ok(())
}

fn reject_non_inline_sources_in_content(content: &Value) -> Result<(), String> {
    let Value::Array(items) = content else {
        return Ok(());
    };

    for item in items {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(block_type) = obj.get("type").and_then(Value::as_str) else {
            continue;
        };
        if block_type != "image" && block_type != "document" {
            continue;
        }
        let Some(source) = obj.get("source").and_then(Value::as_object) else {
            continue;
        };
        match source.get("type").and_then(Value::as_str) {
            Some("base64") => {}
            Some("url")
                if source
                    .get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.starts_with("data:")) => {}
            Some(source_type) => {
                return Err(format!(
                    "{} source type '{}' requires safe image processing; light mode only accepts inline base64/data URLs",
                    block_type, source_type
                ));
            }
            None if source.get("file_id").is_some() || source.get("id").is_some() => {
                return Err(format!(
                    "{} file source requires safe image processing; light mode only accepts inline base64/data URLs",
                    block_type
                ));
            }
            _ => {}
        }
    }

    Ok(())
}

async fn materialize_remote_multimodal_sources(
    messages: &mut [Message],
    caller_user_agent: Option<&str>,
) -> Result<usize, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(ua) = caller_user_agent {
        if !ua.is_empty() {
            builder = builder.user_agent(ua);
        }
    }
    let client = builder
        .build()
        .map_err(|e| format!("failed to create remote source client: {}", e))?;

    let mut materialized = 0usize;
    for message in messages {
        materialized += materialize_content_sources(&client, &mut message.content).await?;
    }

    Ok(materialized)
}

#[cfg(test)]
pub(crate) fn normalize_base64_image_media_types(payload: &mut MessagesRequest) -> usize {
    normalize_message_base64_image_media_types(&mut payload.messages)
}

fn normalize_message_base64_image_media_types(messages: &mut [Message]) -> usize {
    let mut fixed = 0usize;
    for message in messages {
        fixed += normalize_content_base64_image_media_types(&mut message.content);
    }
    if fixed > 0 {
        tracing::warn!(
            fixed,
            "base64 image media_type mismatches were corrected before upstream routing"
        );
    }
    fixed
}

fn normalize_content_base64_image_media_types(content: &mut Value) -> usize {
    let Value::Array(items) = content else {
        return 0;
    };

    let mut fixed = 0usize;
    for item in items {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        if obj.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let Some(source) = obj.get_mut("source").and_then(Value::as_object_mut) else {
            continue;
        };
        if source.get("type").and_then(Value::as_str) != Some("base64") {
            continue;
        }
        let Some(data) = source.get("data").and_then(Value::as_str) else {
            continue;
        };
        let Some(bytes) = decode_inline_base64_payload(data) else {
            continue;
        };
        let Some(detected_media_type) = infer_image_media_type_from_bytes(bytes.as_slice()) else {
            continue;
        };
        let declared_media_type = source
            .get("media_type")
            .and_then(Value::as_str)
            .map(normalize_media_type);
        if declared_media_type.as_deref() == Some(detected_media_type) {
            continue;
        }
        source.insert(
            "media_type".to_string(),
            Value::String(detected_media_type.to_string()),
        );
        fixed += 1;
    }
    fixed
}

fn decode_inline_base64_payload(data: &str) -> Option<Vec<u8>> {
    let base64_payload = data_url_base64_payload(data).unwrap_or(data);
    let normalized = strip_base64_ascii_whitespace(base64_payload);
    BASE64_STANDARD.decode(normalized.as_bytes()).ok()
}

fn data_url_base64_payload(value: &str) -> Option<&str> {
    let data_part = value.strip_prefix("data:")?;
    let (metadata, data) = data_part.split_once(',')?;
    if !metadata
        .split(';')
        .skip(1)
        .any(|part| part.trim().eq_ignore_ascii_case("base64"))
    {
        return None;
    }
    Some(data)
}

fn strip_base64_ascii_whitespace(data: &str) -> String {
    if data.bytes().any(|byte| byte.is_ascii_whitespace()) {
        data.chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect()
    } else {
        data.to_string()
    }
}

async fn materialize_content_sources(
    client: &reqwest::Client,
    content: &mut Value,
) -> Result<usize, String> {
    let Value::Array(items) = content else {
        return Ok(0);
    };

    let mut materialized = 0usize;
    for item in items {
        let Some((block_type, url, provided_media_type)) = remote_source_info(item) else {
            continue;
        };
        if url.starts_with("data:") {
            continue;
        }

        let (media_type, data) =
            download_remote_multimodal_source(client, &block_type, &url, provided_media_type)
                .await?;
        replace_source_with_base64(item, media_type, data);
        materialized += 1;
    }

    Ok(materialized)
}

fn remote_source_info(item: &Value) -> Option<(String, String, Option<String>)> {
    let obj = item.as_object()?;
    let block_type = obj.get("type")?.as_str()?;
    if block_type != "image" && block_type != "document" {
        return None;
    }
    let source = obj.get("source")?.as_object()?;
    if source.get("type")?.as_str()? != "url" {
        return None;
    }
    let url = source.get("url")?.as_str()?.to_string();
    let media_type = source
        .get("media_type")
        .and_then(|v| v.as_str())
        .map(normalize_media_type);
    Some((block_type.to_string(), url, media_type))
}

async fn download_remote_multimodal_source(
    client: &reqwest::Client,
    block_type: &str,
    url: &str,
    provided_media_type: Option<String>,
) -> Result<(String, String), String> {
    let mut current_url = url.to_string();
    let mut response = None;

    for redirect_count in 0..=5 {
        if !current_url.starts_with("https://") && !current_url.starts_with("http://") {
            return Err(format!(
                "{} URL source must use http or https: {}",
                block_type, current_url
            ));
        }

        ensure_safe_remote_url_resolves(&current_url)
            .await
            .map_err(|reason| format!("{} URL rejected: {}", block_type, reason))?;

        let candidate = client
            .get(&current_url)
            .send()
            .await
            .map_err(|e| format!("failed to download {} URL source: {}", block_type, e))?;

        if candidate.status().is_redirection() {
            if redirect_count >= 5 {
                return Err(format!("{} URL source has too many redirects", block_type));
            }

            let location = candidate
                .headers()
                .get(REQWEST_LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!(
                        "{} URL source redirect is missing Location header",
                        block_type
                    )
                })?;
            let next_url = candidate
                .url()
                .join(location)
                .map_err(|e| format!("invalid {} URL redirect: {}", block_type, e))?;
            current_url = next_url.to_string();
            continue;
        }

        response = Some(candidate);
        break;
    }

    let response =
        response.ok_or_else(|| format!("failed to download {} URL source", block_type))?;
    let final_url = response.url().to_string();
    if !final_url.starts_with("https://") && !final_url.starts_with("http://") {
        return Err(format!(
            "{} URL source must use http or https: {}",
            block_type, final_url
        ));
    }
    ensure_safe_remote_url_resolves(&final_url)
        .await
        .map_err(|reason| format!("{} URL rejected: {}", block_type, reason))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "failed to download {} URL source: HTTP {}",
            block_type, status
        ));
    }

    if response
        .content_length()
        .is_some_and(|len| len > MAX_REMOTE_MULTIMODAL_BYTES as u64)
    {
        return Err(format!(
            "{} URL source exceeds {} bytes",
            block_type, MAX_REMOTE_MULTIMODAL_BYTES
        ));
    }

    let response_media_type = response
        .headers()
        .get(REQWEST_CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(normalize_media_type);
    let bytes = read_limited_response_body(response, block_type).await?;

    let media_type = infer_remote_media_type(
        block_type,
        &final_url,
        provided_media_type.as_deref(),
        response_media_type.as_deref(),
        bytes.as_slice(),
    )
    .ok_or_else(|| {
        format!(
            "unsupported {} URL media type for {}",
            block_type, final_url
        )
    })?;

    Ok((media_type, BASE64_STANDARD.encode(bytes.as_slice())))
}

async fn read_limited_response_body(
    response: reqwest::Response,
    block_type: &str,
) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| format!("failed to read {} URL source: {}", block_type, e))?;
        if bytes.len() + chunk.len() > MAX_REMOTE_MULTIMODAL_BYTES {
            return Err(format!(
                "{} URL source exceeds {} bytes",
                block_type, MAX_REMOTE_MULTIMODAL_BYTES
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
}

fn replace_source_with_base64(item: &mut Value, media_type: String, data: String) {
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    obj.insert(
        "source".to_string(),
        json!({
            "type": "base64",
            "media_type": media_type,
            "data": data
        }),
    );
}

fn infer_remote_media_type(
    block_type: &str,
    url: &str,
    provided: Option<&str>,
    response: Option<&str>,
    bytes: &[u8],
) -> Option<String> {
    for candidate in [provided, response].into_iter().flatten() {
        if is_supported_remote_media_type(block_type, candidate) {
            return Some(candidate.to_string());
        }
    }

    if block_type == "image" {
        if let Some(media_type) = infer_image_media_type_from_bytes(bytes) {
            return Some(media_type.to_string());
        }
        return infer_image_format_from_url(url)
            .and_then(|format| image_media_type_from_format(&format).map(str::to_string));
    }

    if bytes.starts_with(b"%PDF") {
        return Some("application/pdf".to_string());
    }
    let inferred = infer_document_media_type_from_url(url);
    is_supported_remote_media_type(block_type, &inferred).then_some(inferred)
}

fn is_supported_remote_media_type(block_type: &str, media_type: &str) -> bool {
    match block_type {
        "image" => matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ),
        "document" => matches!(
            media_type,
            "application/pdf"
                | "text/plain"
                | "text/markdown"
                | "text/html"
                | "text/csv"
                | "application/json"
        ),
        _ => false,
    }
}

fn normalize_media_type(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase()
}

fn infer_image_media_type_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn image_media_type_from_format(format: &str) -> Option<&'static str> {
    match format {
        "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub(crate) fn ensure_safe_remote_url(url_str: &str) -> Result<(), String> {
    let parsed = ::url::Url::parse(url_str).map_err(|e| format!("invalid URL: {}", e))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL missing host".to_string())?;

    let lower = host.to_ascii_lowercase();
    const BLOCKED_HOSTS: &[&str] = &[
        "localhost",
        "ip6-localhost",
        "ip6-loopback",
        "metadata.google.internal",
        "metadata",
        "instance-data",
    ];
    if BLOCKED_HOSTS.contains(&lower.as_str()) || lower.ends_with(".localhost") {
        return Err(format!("host {} is blocked", host));
    }

    let parsed_host_ip = match parsed.host() {
        Some(::url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
        Some(::url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
        _ => host.parse::<std::net::IpAddr>().ok(),
    };
    if let Some(addr) = parsed_host_ip {
        if is_blocked_ip(&addr) {
            return Err(format!("IP {} is in a blocked range", addr));
        }
    }

    Ok(())
}

async fn ensure_safe_remote_url_resolves(url_str: &str) -> Result<(), String> {
    ensure_safe_remote_url(url_str)?;

    let parsed = ::url::Url::parse(url_str).map_err(|e| format!("invalid URL: {}", e))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL missing host".to_string())?;
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }

    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "URL has no resolvable port".to_string())?;
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("DNS lookup failed for {}: {}", host, e))?;

    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        let ip = addr.ip();
        if is_blocked_ip(&ip) {
            return Err(format!("resolved IP {} is in a blocked range", ip));
        }
    }

    if !resolved_any {
        return Err(format!("DNS lookup returned no records for {}", host));
    }

    Ok(())
}

fn is_blocked_ip(addr: &std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    match addr {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
                || *v4 == Ipv4Addr::new(0, 0, 0, 0)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .map(|m| is_blocked_ip(&IpAddr::V4(m)))
                    .unwrap_or(false)
                || *v6 == Ipv6Addr::UNSPECIFIED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::Message as AnthropicMessage;

    fn payload_with_image_source(source: Value) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 128,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: json!([{"type": "image", "source": source}]),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    #[test]
    fn normalize_base64_image_media_types_uses_detected_bytes() {
        let jpeg = BASE64_STANDARD.encode([0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00]);
        let mut payload = payload_with_image_source(json!({
            "type": "base64",
            "media_type": "image/png",
            "data": jpeg
        }));

        let fixed = normalize_base64_image_media_types(&mut payload);

        assert_eq!(fixed, 1);
        assert_eq!(
            payload.messages[0].content[0]["source"]["media_type"],
            "image/jpeg"
        );
    }

    #[test]
    fn normalize_base64_image_media_types_accepts_data_url_data() {
        let jpeg = BASE64_STANDARD.encode([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]);
        let mut payload = payload_with_image_source(json!({
            "type": "base64",
            "media_type": "image/png",
            "data": format!("data:image/jpeg;base64,{}", jpeg)
        }));

        let fixed = normalize_base64_image_media_types(&mut payload);

        assert_eq!(fixed, 1);
        assert_eq!(
            payload.messages[0].content[0]["source"]["media_type"],
            "image/jpeg"
        );
    }

    #[test]
    fn normalize_base64_image_media_types_strips_base64_whitespace() {
        let mut payload = payload_with_image_source(json!({
            "type": "base64",
            "media_type": "image/jpeg",
            "data": "/9j/\n2wBD"
        }));

        let fixed = normalize_base64_image_media_types(&mut payload);

        assert_eq!(fixed, 0);
    }

    #[test]
    fn light_mode_rejects_remote_image_without_downloading() {
        let payload = payload_with_image_source(json!({
            "type": "url",
            "url": "https://example.com/image.png"
        }));

        let err = reject_non_inline_sources(&payload.messages)
            .expect_err("remote image should be rejected");

        assert!(err.contains("light mode only accepts inline"));
    }

    #[test]
    fn light_mode_allows_data_url_image() {
        let payload = payload_with_image_source(json!({
            "type": "url",
            "url": "data:image/png;base64,iVBORw0KGgo="
        }));

        reject_non_inline_sources(&payload.messages).expect("data URL is inline");
    }
}
