//! Anthropic API Handler 函数

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::kiro::model::events::Event;
use crate::kiro::model::requests::kiro::KiroRequest;
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::model::config::{CompatProfile, PromptCacheSimulationMode};
use crate::token;
use anyhow::Error;
use axum::{
    Json as JsonExtractor,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use bytes::Bytes;
use chrono::Utc;
use futures::{Stream, StreamExt, stream};
use reqwest::header::{CONTENT_TYPE as REQWEST_CONTENT_TYPE, LOCATION as REQWEST_LOCATION};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{Instant, interval, sleep_until};

use super::converter::{
    ConversionError, ConverterOptions, convert_request_with_options,
    extract_stable_conversation_id, infer_document_media_type_from_url,
    infer_image_format_from_url,
};
use super::envelope;
use super::middleware::AppState;
use super::prompt_cache::{PromptCacheProfile, PromptCacheScope};
use super::stream::{SseEvent, StreamContext};
use super::types::{
    CountTokensRequest, CountTokensResponse, MessagesRequest, Model, ModelsResponse, OutputConfig,
    Thinking,
};
use super::usage::{UsageRecord, UsageRecordStatus, UsageSource};
use super::websearch;
use crate::kiro::provider::KiroStreamCompletion;

const MAX_REMOTE_MULTIMODAL_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone)]
struct RequestUsageContext {
    recorder: Arc<super::usage::UsageRecorder>,
    prompt_cache: Arc<super::prompt_cache::PromptCacheTracker>,
    request_id: String,
    endpoint: &'static str,
    stream: bool,
    model: String,
    conversation_id: Option<String>,
    input_tokens: i32,
    prompt_cache_profile: Option<PromptCacheProfile>,
    simulation_mode: PromptCacheSimulationMode,
    prompt_cache_target_read_ratio: f64,
    simulated_usage: Option<super::cache::CacheSimulation>,
    simulated_source: Option<UsageSource>,
    started_at: Instant,
}

#[derive(Clone)]
struct CredentialUsageContext {
    request: RequestUsageContext,
    credential_id: Option<u64>,
    credential_label: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
}

impl RequestUsageContext {
    fn attach_credential(
        self,
        credential_id: Option<u64>,
        credential_label: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
    ) -> CredentialUsageContext {
        CredentialUsageContext {
            request: self,
            credential_id,
            credential_label,
            sticky_bound,
            fallback_from_sticky,
        }
    }
}

impl CredentialUsageContext {
    fn scope(&self) -> Option<PromptCacheScope> {
        Some(PromptCacheScope {
            credential_id: self.credential_id?,
            conversation_id: self.request.conversation_id.clone()?,
            model: self.request.model.clone(),
        })
    }

    fn usage_source(
        &self,
        usage: &super::cache::CacheUsage,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        context_estimated: bool,
    ) -> UsageSource {
        if self.uses_local_prompt_cache_fallback(metadata_usage, usage) {
            UsageSource::LocalPromptCache
        } else if metadata_usage.is_some() {
            UsageSource::UpstreamMetadata
        } else if self.request.simulated_source.is_some() && super::cache::usage_has_cache(usage) {
            self.request.simulated_source.unwrap()
        } else if context_estimated {
            UsageSource::ContextEstimate
        } else {
            UsageSource::RequestEstimate
        }
    }

    fn uses_local_prompt_cache_fallback(
        &self,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        usage: &super::cache::CacheUsage,
    ) -> bool {
        self.request.simulation_mode == PromptCacheSimulationMode::HighCache
            && metadata_usage.is_some_and(super::cache::metadata_cache_is_empty)
            && self.request.simulated_source == Some(UsageSource::LocalPromptCache)
            && super::cache::usage_has_cache(usage)
    }

    fn record_success_from_stream(&self, ctx: &StreamContext) {
        let Some(usage) = ctx.final_usage() else {
            return;
        };
        let metadata_usage = ctx.metadata_usage();
        let context_estimated = metadata_usage.is_none() && ctx.context_input_tokens_seen();
        let usage_source = self.usage_source(&usage, metadata_usage, context_estimated);
        self.record_success(usage, usage_source, context_estimated);
    }

    fn record_stream_failure_from_context(
        &self,
        status: UsageRecordStatus,
        usage: Option<super::cache::CacheUsage>,
        error_detail: Option<(String, String)>,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        context_estimated: bool,
    ) {
        let usage = usage.unwrap_or(super::cache::CacheUsage {
            total_input_tokens: self.request.input_tokens,
            input_tokens: self.request.input_tokens,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        });
        let source = self.usage_source(&usage, metadata_usage, context_estimated);
        let (error_type, error_message) = error_detail.unwrap_or_else(|| {
            (
                "api_error".to_string(),
                "upstream stream did not complete successfully".to_string(),
            )
        });
        self.record(status, usage, source, Some(error_type), Some(error_message));
    }

    fn record_success(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        _context_estimated: bool,
    ) {
        self.record(UsageRecordStatus::Success, usage, usage_source, None, None);

        if usage_source != UsageSource::LocalPromptCache {
            return;
        }

        if let Some(scope) = self.scope() {
            self.request.prompt_cache.update(
                Some(scope),
                self.request.prompt_cache_profile.as_ref(),
                self.request.prompt_cache_target_read_ratio,
            );
        }
    }

    fn record_failure(
        &self,
        status: UsageRecordStatus,
        error_type: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        let usage = super::cache::CacheUsage {
            total_input_tokens: self.request.input_tokens,
            input_tokens: self.request.input_tokens,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        self.record(
            status,
            usage,
            UsageSource::None,
            Some(error_type.into()),
            Some(error_message.into()),
        );
    }

    fn record_client_dropped(&self) {
        self.record_failure(
            UsageRecordStatus::ClientDropped,
            "client_dropped",
            "downstream client dropped before upstream stream completed",
        );
    }

    fn record(
        &self,
        status: UsageRecordStatus,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        error_type: Option<String>,
        error_message: Option<String>,
    ) {
        self.request.recorder.record(UsageRecord {
            id: self.request.request_id.clone(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: self.request.endpoint.to_string(),
            stream: self.request.stream,
            model: self.request.model.clone(),
            conversation_id: self.request.conversation_id.clone(),
            credential_id: self.credential_id,
            credential_label: self.credential_label.clone(),
            status,
            usage_source,
            total_input_tokens: usage.total_input_tokens,
            compat_input_tokens: usage.input_tokens,
            billable_input_tokens: usage.billable_input_tokens(),
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
            duration_ms: self.request.started_at.elapsed().as_millis() as u64,
            simulated: usage_source.is_simulated(),
            sticky_bound: self.sticky_bound,
            fallback_from_sticky: self.fallback_from_sticky,
            error_type,
            error_message,
        });
    }
}

#[derive(Clone)]
struct StreamUsageGuard {
    usage_context: CredentialUsageContext,
    completed: Arc<AtomicBool>,
}

impl StreamUsageGuard {
    fn new(usage_context: CredentialUsageContext) -> Self {
        Self {
            usage_context,
            completed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn context(&self) -> &CredentialUsageContext {
        &self.usage_context
    }

    fn complete(&self) {
        self.completed.store(true, Ordering::Release);
    }
}

impl Drop for StreamUsageGuard {
    fn drop(&mut self) {
        if self.completed.load(Ordering::Acquire) {
            return;
        }
        if self.completed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.usage_context.record_client_dropped();
    }
}

fn credential_label(provider: &crate::kiro::provider::KiroProvider, id: u64) -> Option<String> {
    provider.credential_label(id)
}

async fn materialize_remote_multimodal_sources(
    payload: &mut MessagesRequest,
    caller_user_agent: Option<&str>,
) -> Result<(), String> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none());
    // 透传调用方原始 User-Agent；若调用方未提供则不强制设置。
    if let Some(ua) = caller_user_agent {
        if !ua.is_empty() {
            builder = builder.user_agent(ua);
        }
    }
    let client = builder
        .build()
        .map_err(|e| format!("failed to create remote source client: {}", e))?;

    for message in &mut payload.messages {
        materialize_content_sources(&client, &mut message.content).await?;
    }

    Ok(())
}

async fn materialize_content_sources(
    client: &reqwest::Client,
    content: &mut Value,
) -> Result<(), String> {
    let Value::Array(items) = content else {
        return Ok(());
    };

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
    }

    Ok(())
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

/// 拒绝指向私有/回环/链路本地/云元数据等敏感网络的 URL，避免 SSRF。
fn ensure_safe_remote_url(url_str: &str) -> Result<(), String> {
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
                // CGNAT 100.64.0.0/10
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
                // AWS/GCP/Azure metadata 169.254.169.254 已被 link_local 覆盖
                || *v4 == Ipv4Addr::new(0, 0, 0, 0)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // ULA fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: 解出来再判
                || v6
                    .to_ipv4_mapped()
                    .map(|m| is_blocked_ip(&IpAddr::V4(m)))
                    .unwrap_or(false)
                || *v6 == Ipv6Addr::UNSPECIFIED
        }
    }
}

fn prepare_usage_context(
    state: &AppState,
    endpoint: &'static str,
    stream: bool,
    payload: &MessagesRequest,
    conversation_id: Option<String>,
    stable_conversation_id: Option<String>,
    input_tokens: i32,
) -> RequestUsageContext {
    let prompt_cache_profile =
        if state.prompt_cache_simulation_mode == PromptCacheSimulationMode::HighCache {
            state
                .prompt_cache
                .build_high_cache_profile(payload, input_tokens)
        } else {
            state.prompt_cache.build_profile(payload, input_tokens)
        };
    let (simulated_usage, simulated_source) = build_simulated_usage(
        state,
        stable_conversation_id.as_deref(),
        prompt_cache_profile.as_ref(),
    );

    RequestUsageContext {
        recorder: state.usage_recorder.clone(),
        prompt_cache: state.prompt_cache.clone(),
        request_id: envelope::request_id(),
        endpoint,
        stream,
        model: payload.model.clone(),
        conversation_id,
        input_tokens,
        prompt_cache_profile,
        simulation_mode: state.prompt_cache_simulation_mode,
        prompt_cache_target_read_ratio: state.prompt_cache_target_read_ratio,
        simulated_usage,
        simulated_source,
        started_at: Instant::now(),
    }
}

fn build_simulated_usage(
    state: &AppState,
    conversation_id: Option<&str>,
    prompt_cache_profile: Option<&PromptCacheProfile>,
) -> (Option<super::cache::CacheSimulation>, Option<UsageSource>) {
    match state.prompt_cache_simulation_mode {
        PromptCacheSimulationMode::Disabled => (None, None),
        PromptCacheSimulationMode::LocalPromptCache | PromptCacheSimulationMode::HighCache => {
            if conversation_id.is_none() {
                return (None, None);
            }

            // credential_id 需要等 provider 选中账号后才能确定；这里先保留 profile，
            // 真正的 local prompt-cache 计算在 attach credential 后重新完成。
            if prompt_cache_profile.is_some() {
                (None, Some(UsageSource::LocalPromptCache))
            } else {
                (None, None)
            }
        }
    }
}

fn prepare_credential_usage_context(
    usage_context: RequestUsageContext,
    provider: &crate::kiro::provider::KiroProvider,
    credential_id: u64,
    sticky_bound: bool,
    fallback_from_sticky: bool,
) -> CredentialUsageContext {
    let mut usage_context = usage_context;
    if matches!(
        usage_context.simulation_mode,
        PromptCacheSimulationMode::LocalPromptCache | PromptCacheSimulationMode::HighCache
    ) {
        let scope = usage_context
            .conversation_id
            .as_ref()
            .map(|conversation_id| PromptCacheScope {
                credential_id,
                conversation_id: conversation_id.clone(),
                model: usage_context.model.clone(),
            });
        let prompt_usage = usage_context.prompt_cache.compute(
            scope,
            usage_context.prompt_cache_profile.as_ref(),
            usage_context.prompt_cache_target_read_ratio,
        );
        usage_context.simulated_usage = super::cache::CacheSimulation::from_prompt_cache_with_ratio(
            prompt_usage,
            usage_context.prompt_cache_target_read_ratio,
        );
        if usage_context.simulated_usage.is_some() {
            usage_context.simulated_source = Some(UsageSource::LocalPromptCache);
        } else {
            usage_context.simulated_source = None;
        }
    }

    usage_context.attach_credential(
        Some(credential_id),
        credential_label(provider, credential_id),
        sticky_bound,
        fallback_from_sticky,
    )
}

/// 将 KiroProvider 错误映射为 HTTP 响应
fn map_provider_error(err: Error, request_id: Option<&str>) -> Response {
    let err_str = err.to_string();

    // 上下文窗口满了（对话历史累积超出模型上下文窗口限制）
    if err_str.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") {
        tracing::warn!(error = %err, "上游拒绝请求：上下文窗口已满（不应重试）");
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Context window is full. Reduce conversation history, system prompt, or tools.",
                request_id,
            )
        } else {
            envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Context window is full. Reduce conversation history, system prompt, or tools.",
            )
        };
    }

    // 单次输入太长（请求体本身超出上游限制）
    if err_str.contains("Input is too long") {
        tracing::warn!(error = %err, "上游拒绝请求：输入过长（不应重试）");
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Input is too long. Reduce the size of your messages.",
                request_id,
            )
        } else {
            envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Input is too long. Reduce the size of your messages.",
            )
        };
    }
    tracing::error!("Kiro API 调用失败: {}", err);
    if let Some(request_id) = request_id {
        envelope::error_response_with_id(
            StatusCode::BAD_GATEWAY,
            "api_error",
            format!("上游 API 调用失败: {}", err),
            request_id,
        )
    } else {
        envelope::error_response(
            StatusCode::BAD_GATEWAY,
            "api_error",
            format!("上游 API 调用失败: {}", err),
        )
    }
}

fn conversion_error_response(e: &ConversionError) -> Response {
    let (error_type, message) = match e {
        ConversionError::UnsupportedModel(model) => {
            ("invalid_request_error", format!("模型不支持: {}", model))
        }
        ConversionError::EmptyMessages => ("invalid_request_error", "消息列表为空".to_string()),
        ConversionError::UnsupportedContent(message) => ("invalid_request_error", message.clone()),
    };
    envelope::error_response(StatusCode::BAD_REQUEST, error_type, message)
}

fn should_expose_proxy_warnings(state: &AppState) -> bool {
    state.expose_proxy_warnings && !state.compat_profile.is_strict()
}

fn should_extract_unsigned_thinking(state: &AppState, thinking_enabled: bool) -> bool {
    state.extract_thinking && thinking_enabled && state.compat_profile.allows_unsigned_thinking()
}

fn websearch_supported_for_profile(profile: CompatProfile) -> bool {
    !profile.is_strict()
}

/// GET /v1/models
///
/// 返回可用的模型列表
pub async fn get_models() -> impl IntoResponse {
    tracing::info!("Received GET /v1/models request");

    let models = vec![
        Model {
            id: "opus".to_string(),
            object: "model".to_string(),
            created: 1776276000,
            owned_by: "anthropic".to_string(),
            display_name: "Claude Code Alias: Opus".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "opusplan".to_string(),
            object: "model".to_string(),
            created: 1776276000,
            owned_by: "anthropic".to_string(),
            display_name: "Claude Code Alias: Opus Plan".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "best".to_string(),
            object: "model".to_string(),
            created: 1776276000,
            owned_by: "anthropic".to_string(),
            display_name: "Claude Code Alias: Best Available".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "default".to_string(),
            object: "model".to_string(),
            created: 1776276000,
            owned_by: "anthropic".to_string(),
            display_name: "Claude Code Alias: Default".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "sonnet".to_string(),
            object: "model".to_string(),
            created: 1771286400,
            owned_by: "anthropic".to_string(),
            display_name: "Claude Code Alias: Sonnet".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "haiku".to_string(),
            object: "model".to_string(),
            created: 1760486400,
            owned_by: "anthropic".to_string(),
            display_name: "Claude Code Alias: Haiku".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-7".to_string(),
            object: "model".to_string(),
            created: 1776276000, // Apr 16, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.7".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-7-thinking".to_string(),
            object: "model".to_string(),
            created: 1776276000, // Apr 16, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.7 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-6".to_string(),
            object: "model".to_string(),
            created: 1770163200, // Feb 4, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.6".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-6-thinking".to_string(),
            object: "model".to_string(),
            created: 1770163200, // Feb 4, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.6 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-6".to_string(),
            object: "model".to_string(),
            created: 1771286400, // Feb 17, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.6".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-6-thinking".to_string(),
            object: "model".to_string(),
            created: 1771286400, // Feb 17, 2026
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.6 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-5-20251101".to_string(),
            object: "model".to_string(),
            created: 1763942400, // Nov 24, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-opus-4-5-20251101-thinking".to_string(),
            object: "model".to_string(),
            created: 1763942400, // Nov 24, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Opus 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-5-20250929".to_string(),
            object: "model".to_string(),
            created: 1759104000, // Sep 29, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-sonnet-4-5-20250929-thinking".to_string(),
            object: "model".to_string(),
            created: 1759104000, // Sep 29, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Sonnet 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-haiku-4-5-20251001".to_string(),
            object: "model".to_string(),
            created: 1760486400, // Oct 15, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Haiku 4.5".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
        Model {
            id: "claude-haiku-4-5-20251001-thinking".to_string(),
            object: "model".to_string(),
            created: 1760486400, // Oct 15, 2025
            owned_by: "anthropic".to_string(),
            display_name: "Claude Haiku 4.5 (Thinking)".to_string(),
            model_type: "chat".to_string(),
            max_tokens: 64000,
        },
    ];

    Json(ModelsResponse {
        object: "list".to_string(),
        data: models,
    })
}

/// POST /v1/messages
///
/// 创建消息（对话）
pub async fn post_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonExtractor(mut payload): JsonExtractor<MessagesRequest>,
) -> Response {
    tracing::info!(
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages request"
    );
    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return envelope::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Kiro API provider not configured",
            );
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    let caller_ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    if let Err(message) = materialize_remote_multimodal_sources(&mut payload, caller_ua).await {
        tracing::warn!("多模态远程 source 处理失败: {}", message);
        return envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message);
    }

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        if !websearch_supported_for_profile(state.compat_profile) {
            return envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "web_search server-tool synthesis is disabled in anthropic-strict profile",
            );
        }
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");

        // 估算输入 tokens
        let input_tokens = token::count_all_tokens(
            payload.model.clone(),
            payload.system.clone(),
            payload.messages.clone(),
            payload.tools.clone(),
        ) as i32;

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request_with_options(
        &payload,
        ConverterOptions {
            compat_profile: state.compat_profile,
        },
    ) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("请求转换失败: {}", e);
            return conversion_error_response(&e);
        }
    };

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };

    let request_body = match serde_json::to_string(&kiro_request) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("序列化请求失败: {}", e);
            return envelope::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("序列化请求失败: {}", e),
            );
        }
    };

    tracing::debug!("Kiro request body: {}", request_body);

    // 估算输入 tokens
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system.clone(),
        payload.messages.clone(),
        payload.tools.clone(),
    ) as i32;
    let usage_context = prepare_usage_context(
        &state,
        "/v1/messages",
        payload.stream,
        &payload,
        Some(kiro_request.conversation_state.conversation_id.clone()),
        extract_stable_conversation_id(&payload),
        input_tokens,
    );

    // 检查是否启用了thinking
    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);

    let tool_name_map = conversion_result.tool_name_map;
    let warnings_header = if should_expose_proxy_warnings(&state) {
        conversion_result.warnings.encode_header()
    } else {
        None
    };
    let extract_xml_thinking = state.compat_profile.allows_unsigned_thinking();

    if payload.stream {
        // 流式响应
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            thinking_enabled,
            extract_xml_thinking,
            tool_name_map,
            usage_context,
            warnings_header,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = should_extract_unsigned_thinking(&state, thinking_enabled);
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            extract_thinking,
            tool_name_map,
            usage_context,
            warnings_header,
        )
        .await
    }
}

/// 处理流式请求
async fn handle_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    input_tokens: i32,
    thinking_enabled: bool,
    extract_xml_thinking: bool,
    tool_name_map: HashMap<String, String>,
    usage_context: RequestUsageContext,
    warnings_header: Option<String>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let response = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            let request_id = usage_context.request_id.clone();
            usage_context
                .attach_credential(None, None, false, false)
                .record_failure(UsageRecordStatus::Error, "api_error", message);
            return map_provider_error(e, Some(&request_id));
        }
    };
    let (response, completion) = response.into_parts();
    let credential_usage = prepare_credential_usage_context(
        usage_context,
        &provider,
        completion.credential_id(),
        completion.sticky_bound(),
        completion.fallback_from_sticky(),
    );

    // 创建流处理上下文
    let mut ctx = StreamContext::new_with_simulation(
        model,
        input_tokens,
        thinking_enabled,
        extract_xml_thinking,
        tool_name_map,
        credential_usage.request.simulated_usage,
        credential_usage.request.simulation_mode,
    );

    // 生成初始事件
    let initial_events = ctx.generate_initial_events();

    // 创建 SSE 流
    let response_request_id = credential_usage.request.request_id.clone();
    let stream = create_sse_stream(response, ctx, initial_events, completion, credential_usage);

    // 返回 SSE 响应
    let mut builder = envelope::sse_builder_with_id(&response_request_id);
    if let Some(warnings) = warnings_header {
        builder = builder.header("x-kiro-rs-warnings", warnings);
    }
    builder.body(Body::from_stream(stream)).unwrap()
}

/// Ping 事件间隔（25秒）
const PING_INTERVAL_SECS: u64 = 25;
/// 上游 eventstream 读空闲超时（180秒）
const UPSTREAM_IDLE_TIMEOUT_SECS: u64 = 180;

/// 创建 ping 事件的 SSE 字符串
fn create_ping_sse() -> Bytes {
    Bytes::from("event: ping\ndata: {\"type\": \"ping\"}\n\n")
}

/// 创建 SSE 事件流
fn create_sse_stream(
    response: reqwest::Response,
    ctx: StreamContext,
    initial_events: Vec<SseEvent>,
    completion: KiroStreamCompletion,
    usage_context: CredentialUsageContext,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let usage_guard = StreamUsageGuard::new(usage_context);
    // 先发送初始事件
    let initial_stream = stream::iter(
        initial_events
            .into_iter()
            .map(|e| Ok(Bytes::from(e.to_sse_string()))),
    );

    // 然后处理 Kiro 响应流，同时每25秒发送 ping 保活
    let body_stream = response.bytes_stream();

    let processing_stream = stream::unfold(
        (
            body_stream,
            ctx,
            EventStreamDecoder::new(),
            false,
            completion,
            usage_guard,
            interval(Duration::from_secs(PING_INTERVAL_SECS)),
            Instant::now() + Duration::from_secs(UPSTREAM_IDLE_TIMEOUT_SECS),
        ),
        |(
            mut body_stream,
            mut ctx,
            mut decoder,
            finished,
            completion,
            usage_guard,
            mut ping_interval,
            mut idle_deadline,
        )| async move {
            if finished {
                return None;
            }

            let idle_sleep = sleep_until(idle_deadline);
            tokio::pin!(idle_sleep);

            // 使用 select! 同时等待数据、ping 定时器和上游空闲超时
            tokio::select! {
                // 处理数据流
                chunk_result = body_stream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            idle_deadline = Instant::now() + Duration::from_secs(UPSTREAM_IDLE_TIMEOUT_SECS);
                            // 解码事件
                            if let Err(e) = decoder.feed(&chunk) {
                                tracing::warn!("缓冲区溢出: {}", e);
                            }

                            let mut events = Vec::new();
                            for result in decoder.decode_iter() {
                                match result {
                                    Ok(frame) => {
                                        if let Ok(event) = Event::from_frame(frame) {
                                            let sse_events = ctx.process_kiro_event(&event);
                                            events.extend(sse_events);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("解码事件失败: {}", e);
                                    }
                                }
                            }

                            // 转换为 SSE 字节流
                            let bytes: Vec<Result<Bytes, Infallible>> = events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();

                            Some((stream::iter(bytes), (body_stream, ctx, decoder, false, completion, usage_guard, ping_interval, idle_deadline)))
                        }
                        Some(Err(e)) => {
                            tracing::error!("读取响应流失败: {}", e);
                            completion.report_soft_failure();
                            // 读取错误：关闭已有内容块后发送 SSE error，不再发送正常 message_stop。
                            ctx.record_stream_error("api_error", format!("upstream stream read error: {}", e));
                            let error_detail = ctx.stream_error_detail();
                            let final_events = ctx.generate_final_events();
                            usage_guard.context().record_stream_failure_from_context(
                                UsageRecordStatus::StreamError,
                                ctx.final_usage(),
                                error_detail,
                                ctx.metadata_usage(),
                                ctx.metadata_usage().is_none() && ctx.context_input_tokens_seen(),
                            );
                            usage_guard.complete();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, completion, usage_guard, ping_interval, idle_deadline)))
                        }
                        None => {
                            // 流结束，发送最终事件
                            if ctx.has_stream_error() {
                                completion.report_soft_failure();
                            } else {
                                completion.report_success();
                            }
                            let had_stream_error = ctx.has_stream_error();
                            let error_detail = ctx.stream_error_detail();
                            let final_events = ctx.generate_final_events();
                            if had_stream_error {
                                usage_guard.context().record_stream_failure_from_context(
                                    UsageRecordStatus::StreamError,
                                    ctx.final_usage(),
                                    error_detail,
                                    ctx.metadata_usage(),
                                    ctx.metadata_usage().is_none() && ctx.context_input_tokens_seen(),
                                );
                            } else {
                                usage_guard.context().record_success_from_stream(&ctx);
                            }
                            usage_guard.complete();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, completion, usage_guard, ping_interval, idle_deadline)))
                        }
                    }
                }
                _ = &mut idle_sleep => {
                    tracing::error!(
                        "上游响应流超过 {} 秒未产生数据，结束流并发送错误事件",
                        UPSTREAM_IDLE_TIMEOUT_SECS
                    );
                    completion.report_soft_failure();
                    ctx.record_stream_error("api_error", "upstream stream idle timeout");
                    let error_detail = ctx.stream_error_detail();
                    let final_events = ctx.generate_final_events();
                    usage_guard.context().record_stream_failure_from_context(
                        UsageRecordStatus::UpstreamTimeout,
                        ctx.final_usage(),
                        error_detail,
                        ctx.metadata_usage(),
                        ctx.metadata_usage().is_none() && ctx.context_input_tokens_seen(),
                    );
                    usage_guard.complete();
                    let bytes: Vec<Result<Bytes, Infallible>> = final_events
                        .into_iter()
                        .map(|e| Ok(Bytes::from(e.to_sse_string())))
                        .collect();
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, true, completion, usage_guard, ping_interval, idle_deadline)))
                }
                // 发送 ping 保活
                _ = ping_interval.tick() => {
                    tracing::trace!("发送 ping 保活事件");
                    let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, false, completion, usage_guard, ping_interval, idle_deadline)))
                }
            }
        },
    )
    .flatten();

    initial_stream.chain(processing_stream)
}

use super::converter::get_context_window_size;

/// 处理非流式请求
async fn handle_non_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    input_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: HashMap<String, String>,
    usage_context: RequestUsageContext,
    warnings_header: Option<String>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let api_response = match provider.call_api_with_context(request_body).await {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            let request_id = usage_context.request_id.clone();
            usage_context
                .attach_credential(None, None, false, false)
                .record_failure(UsageRecordStatus::Error, "api_error", message);
            return map_provider_error(e, Some(&request_id));
        }
    };
    let credential_usage = prepare_credential_usage_context(
        usage_context,
        &provider,
        api_response.credential_id,
        api_response.sticky_bound,
        api_response.fallback_from_sticky,
    );

    // 读取响应体
    let body_bytes = match api_response.response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!("读取响应体失败: {}", e);
            credential_usage.record_failure(
                UsageRecordStatus::Error,
                "api_error",
                format!("读取响应失败: {}", e),
            );
            return envelope::error_response_with_id(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("读取响应失败: {}", e),
                &credential_usage.request.request_id,
            );
        }
    };

    // 解析事件流
    let mut decoder = EventStreamDecoder::new();
    if let Err(e) = decoder.feed(&body_bytes) {
        tracing::warn!("缓冲区溢出: {}", e);
    }

    let mut text_content = String::new();
    let mut tool_uses: Vec<serde_json::Value> = Vec::new();
    let mut has_tool_use = false;
    let mut stop_reason = "end_turn".to_string();
    // 从 contextUsageEvent 计算的实际输入 tokens
    let mut context_input_tokens: Option<i32> = None;
    let mut metadata_usage: Option<crate::kiro::model::events::MetadataTokenUsage> = None;
    let mut native_thinking_content = String::new();
    let mut native_thinking_signature: Option<String> = None;
    let mut redacted_thinking: Option<String> = None;

    // 收集工具调用的增量 JSON
    let mut tool_json_buffers: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for result in decoder.decode_iter() {
        match result {
            Ok(frame) => {
                if let Ok(event) = Event::from_frame(frame) {
                    match event {
                        Event::AssistantResponse(resp) => {
                            text_content.push_str(&resp.content);
                        }
                        Event::ReasoningContent(reasoning) => {
                            if let Some(redacted) = reasoning.redacted_content {
                                if !redacted.is_empty() {
                                    redacted_thinking = Some(redacted);
                                }
                            }
                            if !reasoning.text.is_empty() {
                                native_thinking_content = reasoning.text;
                            }
                            if reasoning.signature.is_some() {
                                native_thinking_signature = reasoning.signature;
                            }
                        }
                        Event::ToolUse(tool_use) => {
                            has_tool_use = true;

                            // 累积工具的 JSON 输入
                            let buffer = tool_json_buffers
                                .entry(tool_use.tool_use_id.clone())
                                .or_insert_with(String::new);
                            buffer.push_str(&tool_use.input);

                            // 如果是完整的工具调用，添加到列表
                            if tool_use.stop {
                                let input: serde_json::Value = if buffer.is_empty() {
                                    serde_json::json!({})
                                } else {
                                    serde_json::from_str(buffer).unwrap_or_else(|e| {
                                        tracing::warn!(
                                            "工具输入 JSON 解析失败: {}, tool_use_id: {}",
                                            e,
                                            tool_use.tool_use_id
                                        );
                                        serde_json::json!({})
                                    })
                                };

                                let original_name = tool_name_map
                                    .get(&tool_use.name)
                                    .cloned()
                                    .unwrap_or_else(|| tool_use.name.clone());

                                tool_uses.push(json!({
                                    "type": "tool_use",
                                    "id": tool_use.tool_use_id,
                                    "name": original_name,
                                    "input": input
                                }));
                            }
                        }
                        Event::ContextUsage(context_usage) => {
                            // 从上下文使用百分比计算实际的 input_tokens
                            let window_size = get_context_window_size(model);
                            let actual_input_tokens =
                                (context_usage.context_usage_percentage * (window_size as f64)
                                    / 100.0) as i32;
                            context_input_tokens = Some(actual_input_tokens);
                            // 上下文使用量达到 100% 时，设置 stop_reason 为 model_context_window_exceeded
                            if context_usage.context_usage_percentage >= 100.0 {
                                stop_reason = "model_context_window_exceeded".to_string();
                            }
                            tracing::debug!(
                                "收到 contextUsageEvent: {}%, 计算 input_tokens: {}",
                                context_usage.context_usage_percentage,
                                actual_input_tokens
                            );
                        }
                        Event::Metadata(metadata) => {
                            if let Some(token_usage) = metadata.token_usage {
                                metadata_usage = Some(token_usage);
                            }
                        }
                        Event::InvalidState(invalid) => {
                            let message = invalid.error_text();
                            tracing::warn!(
                                reason = %invalid.reason,
                                message = %message,
                                "非流式响应收到 invalidStateEvent"
                            );
                            credential_usage.record_failure(
                                UsageRecordStatus::Error,
                                "invalid_request_error",
                                message.clone(),
                            );
                            return envelope::error_response_with_id(
                                StatusCode::BAD_REQUEST,
                                "invalid_request_error",
                                message,
                                &credential_usage.request.request_id,
                            );
                        }
                        Event::Exception { exception_type, .. } => {
                            if exception_type == "ContentLengthExceededException" {
                                stop_reason = "max_tokens".to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::warn!("解码事件失败: {}", e);
            }
        }
    }

    // 确定 stop_reason
    if has_tool_use && stop_reason == "end_turn" {
        stop_reason = "tool_use".to_string();
    }

    // 构建响应内容
    let mut content: Vec<serde_json::Value> = Vec::new();

    if thinking_enabled && redacted_thinking.is_some() {
        content.push(json!({
            "type": "redacted_thinking",
            "data": redacted_thinking.unwrap()
        }));
    } else if thinking_enabled && !native_thinking_content.is_empty() {
        let mut thinking_block = json!({
            "type": "thinking",
            "thinking": native_thinking_content
        });
        if let Some(signature) = native_thinking_signature {
            if !signature.is_empty() {
                thinking_block["signature"] = json!(signature);
            }
        }
        content.push(thinking_block);
        if !text_content.is_empty() {
            content.push(json!({
                "type": "text",
                "text": text_content
            }));
        }
    } else if thinking_enabled {
        // 从完整文本中提取 thinking 块
        let (thinking, remaining_text) =
            super::stream::extract_thinking_from_complete_text(&text_content);

        if let Some(thinking_text) = thinking {
            content.push(json!({
                "type": "thinking",
                "thinking": thinking_text
            }));
        }

        if !remaining_text.is_empty() {
            content.push(json!({
                "type": "text",
                "text": remaining_text
            }));
        }
    } else if !text_content.is_empty() {
        content.push(json!({
            "type": "text",
            "text": text_content
        }));
    }

    content.extend(tool_uses);

    // 估算输出 tokens
    let output_tokens = metadata_usage
        .as_ref()
        .map(|usage| usage.output_tokens)
        .unwrap_or_else(|| token::estimate_output_tokens(&content));

    // 优先使用 metadataEvent 的准确 usage，其次使用 contextUsageEvent 估算值。
    let final_input_tokens = metadata_usage
        .as_ref()
        .map(|usage| usage.total_input_tokens())
        .or(context_input_tokens)
        .unwrap_or(input_tokens);

    let usage = super::cache::build_usage_with_simulation_policy(
        metadata_usage.as_ref(),
        final_input_tokens,
        output_tokens,
        credential_usage.request.simulated_usage,
        credential_usage.request.simulation_mode == PromptCacheSimulationMode::HighCache,
    );
    let has_metadata = metadata_usage.is_some();
    let context_estimated = !has_metadata && context_input_tokens.is_some();
    let usage_source =
        credential_usage.usage_source(&usage, metadata_usage.as_ref(), context_estimated);
    credential_usage.record_success(usage, usage_source, context_estimated);
    provider.report_success_for_context(
        api_response.credential_id,
        api_response.session_id.as_deref(),
    );

    // 构建 Anthropic 响应
    let response_body = json!({
        "id": envelope::message_id(),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage.to_json()
    });

    envelope::json_response_with_id(
        StatusCode::OK,
        response_body,
        &credential_usage.request.request_id,
        warnings_header,
    )
}

/// 检测模型名是否包含 "thinking" 后缀，若包含则在调用方未显式配置时注入 thinking
///
/// - 调用方已指定 `thinking` 字段：保留原值
/// - 调用方未指定：根据模型注入
///   - Opus 4.6 / 4.7：adaptive 类型
///   - 其他模型：enabled 类型
///   - budget_tokens 固定为 20000
/// - `output_config.effort` 同样仅在调用方未设置时填充
fn override_thinking_from_model_name(payload: &mut MessagesRequest) {
    let model_lower = payload.model.to_lowercase();
    if !model_lower.contains("thinking") {
        return;
    }

    let model_base = model_lower.strip_suffix("[1m]").unwrap_or(&model_lower);
    let is_opus_alias = matches!(
        model_base,
        "opus-thinking" | "opusplan-thinking" | "best-thinking" | "default-thinking"
    );
    let is_opus_4_7 = is_opus_alias
        || (model_base.contains("opus")
            && (model_base.contains("4-7")
                || model_base.contains("4.7")
                || model_base == "opus"
                || model_base == "opusplan"
                || model_base == "best"
                || model_base == "default"));
    let is_opus_4_6 =
        model_base.contains("opus") && (model_base.contains("4-6") || model_base.contains("4.6"));
    let is_adaptive_opus = is_opus_4_7 || is_opus_4_6;

    let thinking_type = if is_adaptive_opus {
        "adaptive"
    } else {
        "enabled"
    };

    if payload.thinking.is_none() {
        tracing::info!(
            model = %payload.model,
            thinking_type = thinking_type,
            "模型名包含 thinking 后缀，注入默认 thinking 配置"
        );
        payload.thinking = Some(Thinking {
            thinking_type: thinking_type.to_string(),
            budget_tokens: 20000,
        });
    } else {
        tracing::debug!(
            model = %payload.model,
            "调用方已指定 thinking 配置，保留原值"
        );
    }

    if is_adaptive_opus && payload.output_config.is_none() {
        payload.output_config = Some(OutputConfig {
            effort: if is_opus_4_7 { "xhigh" } else { "high" }.to_string(),
        });
    }
}

/// POST /v1/messages/count_tokens
///
/// 计算消息的 token 数量
pub async fn count_tokens(
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> impl IntoResponse {
    tracing::info!(
        model = %payload.model,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages/count_tokens request"
    );

    let total_tokens = token::count_all_tokens(
        payload.model,
        payload.system,
        payload.messages,
        payload.tools,
    ) as i32;

    Json(CountTokensResponse {
        input_tokens: total_tokens.max(1) as i32,
    })
}

/// POST /cc/v1/messages
///
/// Claude Code 兼容端点，与 /v1/messages 的区别在于：
/// - 流式响应实时转发 Kiro eventstream，避免 Claude Code CLI 长时间没有过程输出
/// - 最终 usage 仍会在 message_delta 和 usage records 中修正
pub async fn post_messages_cc(
    State(state): State<AppState>,
    headers: HeaderMap,
    JsonExtractor(mut payload): JsonExtractor<MessagesRequest>,
) -> Response {
    tracing::info!(
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST /cc/v1/messages request"
    );

    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return envelope::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Kiro API provider not configured",
            );
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    let caller_ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    if let Err(message) = materialize_remote_multimodal_sources(&mut payload, caller_ua).await {
        tracing::warn!("多模态远程 source 处理失败: {}", message);
        return envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message);
    }

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        if !websearch_supported_for_profile(state.compat_profile) {
            return envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "web_search server-tool synthesis is disabled in anthropic-strict profile",
            );
        }
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");

        // 估算输入 tokens
        let input_tokens = token::count_all_tokens(
            payload.model.clone(),
            payload.system.clone(),
            payload.messages.clone(),
            payload.tools.clone(),
        ) as i32;

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request_with_options(
        &payload,
        ConverterOptions {
            compat_profile: state.compat_profile,
        },
    ) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("请求转换失败: {}", e);
            return conversion_error_response(&e);
        }
    };

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };

    let request_body = match serde_json::to_string(&kiro_request) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("序列化请求失败: {}", e);
            return envelope::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                format!("序列化请求失败: {}", e),
            );
        }
    };

    tracing::debug!("Kiro request body: {}", request_body);

    // 估算输入 tokens
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system.clone(),
        payload.messages.clone(),
        payload.tools.clone(),
    ) as i32;
    let usage_context = prepare_usage_context(
        &state,
        "/cc/v1/messages",
        payload.stream,
        &payload,
        Some(kiro_request.conversation_state.conversation_id.clone()),
        extract_stable_conversation_id(&payload),
        input_tokens,
    );

    // 检查是否启用了thinking
    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);

    let tool_name_map = conversion_result.tool_name_map;
    let warnings_header = if should_expose_proxy_warnings(&state) {
        conversion_result.warnings.encode_header()
    } else {
        None
    };
    let extract_xml_thinking = state.compat_profile.allows_unsigned_thinking();

    if payload.stream {
        // 流式响应（实时模式）
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            thinking_enabled,
            extract_xml_thinking,
            tool_name_map,
            usage_context,
            warnings_header,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = should_extract_unsigned_thinking(&state, thinking_enabled);
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            extract_thinking,
            tool_name_map,
            usage_context,
            warnings_header,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::cache::{self, CacheUsage};
    use crate::anthropic::prompt_cache::PromptCacheTracker;
    use crate::anthropic::types::{Message, SystemMessage};
    use crate::anthropic::usage::UsageRecorder;
    use crate::kiro::model::events::MetadataTokenUsage;
    use serde_json::json;

    fn messages_request_for_model(model: &str) -> MessagesRequest {
        MessagesRequest {
            model: model.to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("hello"),
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
    fn thinking_suffix_opus_4_7_uses_adaptive() {
        let mut payload = messages_request_for_model("claude-opus-4-7-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(thinking.budget_tokens, 20000);
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be set")
                .effort,
            "xhigh"
        );
    }

    #[test]
    fn thinking_suffix_opus_alias_uses_opus_4_7_adaptive_defaults() {
        let mut payload = messages_request_for_model("opus-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be set")
                .effort,
            "xhigh"
        );
    }

    #[test]
    fn thinking_suffix_sonnet_stays_enabled() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "enabled");
        assert!(payload.output_config.is_none());
    }

    #[test]
    fn local_prompt_cache_updates_even_when_context_tokens_are_estimated() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10, None));
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "text",
                        "text": "cacheable prompt block ".repeat(700),
                        "cache_control": {"type": "ephemeral"}
                    }
                ]),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let profile = prompt_cache.build_profile(&payload, 4096);
        let usage_context = RequestUsageContext {
            recorder: usage_recorder,
            prompt_cache: prompt_cache.clone(),
            request_id: "req_test".to_string(),
            endpoint: "/v1/messages",
            stream: true,
            model: payload.model.clone(),
            conversation_id: Some("session-a".to_string()),
            input_tokens: 4096,
            prompt_cache_profile: profile.clone(),
            simulation_mode: PromptCacheSimulationMode::LocalPromptCache,
            prompt_cache_target_read_ratio: 0.85,
            simulated_usage: None,
            simulated_source: Some(UsageSource::LocalPromptCache),
            started_at: Instant::now(),
        }
        .attach_credential(Some(1), None, false, false);
        let usage = CacheUsage {
            total_input_tokens: 4096,
            input_tokens: 128,
            output_tokens: 1,
            cache_creation_input_tokens: 3968,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 3968,
            cache_creation_1h_input_tokens: 0,
        };

        usage_context.record_success(usage, UsageSource::LocalPromptCache, true);

        let scope = PromptCacheScope {
            credential_id: 1,
            conversation_id: "session-a".to_string(),
            model: payload.model,
        };
        let second = prompt_cache.compute(Some(scope), profile.as_ref(), 0.85);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn high_cache_zero_metadata_fallback_updates_local_prompt_cache() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10, None));
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("hello"),
            }],
            stream: true,
            system: Some(vec![SystemMessage {
                text: "cacheable prompt block ".repeat(700),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let profile = prompt_cache.build_high_cache_profile(&payload, 4096);
        let usage_context = RequestUsageContext {
            recorder: usage_recorder,
            prompt_cache: prompt_cache.clone(),
            request_id: "req_high_cache".to_string(),
            endpoint: "/v1/messages",
            stream: true,
            model: payload.model.clone(),
            conversation_id: Some("session-high-cache".to_string()),
            input_tokens: 4096,
            prompt_cache_profile: profile.clone(),
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            simulated_usage: Some(cache::CacheSimulation {
                cache_creation_input_tokens: 3968,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 3968,
                cache_creation_1h_input_tokens: 0,
                target_cache_ratio: Some(0.95),
            }),
            simulated_source: Some(UsageSource::LocalPromptCache),
            started_at: Instant::now(),
        }
        .attach_credential(Some(1), None, false, false);
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 4096,
            output_tokens: 1,
            total_tokens: 4097,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
        };
        let usage = cache::build_usage_with_simulation_policy(
            Some(&metadata),
            4096,
            1,
            usage_context.request.simulated_usage,
            true,
        );

        let source = usage_context.usage_source(&usage, Some(&metadata), false);
        assert_eq!(source, UsageSource::LocalPromptCache);
        usage_context.record_success(usage, source, false);

        let scope = PromptCacheScope {
            credential_id: 1,
            conversation_id: "session-high-cache".to_string(),
            model: payload.model,
        };
        let second = prompt_cache.compute(Some(scope), profile.as_ref(), 0.95);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn local_prompt_cache_does_not_simulate_without_stable_conversation_id() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10, None));
        let state = AppState::new(
            "test-key",
            true,
            usage_recorder,
            prompt_cache.clone(),
            PromptCacheSimulationMode::LocalPromptCache,
            0.95,
            CompatProfile::ClaudeCode,
            false,
        );
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "text",
                        "text": "cacheable prompt block ".repeat(700),
                        "cache_control": {"type": "ephemeral"}
                    }
                ]),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let profile = prompt_cache.build_profile(&payload, 4096);

        let (simulation, source) = build_simulated_usage(&state, None, profile.as_ref());

        assert!(simulation.is_none());
        assert!(source.is_none());
    }

    #[test]
    fn strict_profile_suppresses_proxy_warning_header() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10, None));
        let state = AppState::new(
            "test-key",
            true,
            usage_recorder,
            prompt_cache,
            PromptCacheSimulationMode::Disabled,
            0.85,
            CompatProfile::AnthropicStrict,
            true,
        );

        assert!(!should_expose_proxy_warnings(&state));
    }

    #[test]
    fn remote_url_safety_rejects_local_and_private_targets() {
        for url in [
            "http://localhost/image.png",
            "http://127.0.0.1/image.png",
            "http://10.0.0.5/image.png",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/image.png",
        ] {
            assert!(
                ensure_safe_remote_url(url).is_err(),
                "{url} should be blocked"
            );
        }
    }
}
