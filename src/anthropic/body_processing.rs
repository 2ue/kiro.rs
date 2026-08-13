use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::StreamExt;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{CONTENT_TYPE as REQWEST_CONTENT_TYPE, LOCATION as REQWEST_LOCATION};
use serde_json::{Value, json};
use std::{
    io,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::model::config::{ImageProcessingConfig, ImageProcessingMode};

use super::{
    converter::{infer_document_media_type_from_url, infer_image_format_from_url},
    files::{self, AnthropicFileStore},
    types::{Message, MessagesRequest},
};

const MAX_REMOTE_MULTIMODAL_BYTES: usize = 20 * 1024 * 1024;
const MAX_REMOTE_MULTIMODAL_SOURCES: usize = 20;
const MAX_REMOTE_MULTIMODAL_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_REMOTE_MULTIMODAL_MATERIALIZED_BYTES: usize = 44 * 1024 * 1024;
const MAX_REMOTE_MULTIMODAL_HTTP_ATTEMPTS: usize = 32;
const MAX_REMOTE_MULTIMODAL_REDIRECTS: usize = 5;
const MAX_CONCURRENT_REMOTE_MULTIMODAL_WORKFLOWS: usize = 4;
const REMOTE_MULTIMODAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const REMOTE_MULTIMODAL_WORKFLOW_TIMEOUT: Duration = Duration::from_secs(45);
const REMOTE_MULTIMODAL_CAPACITY_ERROR: &str =
    "remote image/document materialization is temporarily at capacity";

#[derive(Debug, Default)]
pub(crate) struct BodyProcessingReport {
    pub mode: ImageProcessingMode,
    pub materialized_file_sources: usize,
    pub materialized_remote_sources: usize,
    pub remote_downloaded_bytes: usize,
    pub remote_materialized_bytes: usize,
    pub remote_http_attempts: usize,
    pub normalized_image_media_types: usize,
    remote_workflow_permit: Option<OwnedSemaphorePermit>,
}

impl BodyProcessingReport {
    pub(crate) fn was_modified(&self) -> bool {
        self.materialized_file_sources > 0
            || self.materialized_remote_sources > 0
            || self.normalized_image_media_types > 0
    }
}

#[derive(Debug)]
struct RemoteMaterializationOutcome {
    materialized_sources: usize,
    downloaded_bytes: usize,
    materialized_bytes: usize,
    http_attempts: usize,
    workflow_permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy)]
struct RemoteMaterializationLimits {
    max_downloaded_bytes: usize,
    max_materialized_bytes: usize,
    max_http_attempts: usize,
}

impl Default for RemoteMaterializationLimits {
    fn default() -> Self {
        Self {
            max_downloaded_bytes: MAX_REMOTE_MULTIMODAL_TOTAL_BYTES,
            max_materialized_bytes: MAX_REMOTE_MULTIMODAL_MATERIALIZED_BYTES,
            max_http_attempts: MAX_REMOTE_MULTIMODAL_HTTP_ATTEMPTS,
        }
    }
}

#[derive(Debug)]
struct RemoteMaterializationBudget {
    downloaded_bytes: usize,
    materialized_bytes: usize,
    http_attempts: usize,
    limits: RemoteMaterializationLimits,
}

impl Default for RemoteMaterializationBudget {
    fn default() -> Self {
        Self {
            downloaded_bytes: 0,
            materialized_bytes: 0,
            http_attempts: 0,
            limits: RemoteMaterializationLimits::default(),
        }
    }
}

impl RemoteMaterializationBudget {
    #[cfg(test)]
    fn with_limits(limits: RemoteMaterializationLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    fn reserve_http_attempt(&mut self) -> Result<(), String> {
        if self.http_attempts >= self.limits.max_http_attempts {
            return Err(format!(
                "remote image/document sources exceed the request HTTP attempt limit of {}",
                self.limits.max_http_attempts
            ));
        }
        self.http_attempts += 1;
        Ok(())
    }

    fn reserve_downloaded_bytes(&mut self, block_type: &str, amount: usize) -> Result<(), String> {
        let next = self
            .downloaded_bytes
            .checked_add(amount)
            .ok_or_else(|| "remote image/document source byte accounting overflowed".to_string())?;
        if next > self.limits.max_downloaded_bytes {
            return Err(format!(
                "{} URL sources exceed the request aggregate download limit of {} bytes",
                block_type, self.limits.max_downloaded_bytes
            ));
        }
        self.downloaded_bytes = next;
        Ok(())
    }

    fn reserve_materialized_bytes(
        &mut self,
        block_type: &str,
        amount: usize,
    ) -> Result<(), String> {
        let next = self.materialized_bytes.checked_add(amount).ok_or_else(|| {
            "remote image/document materialized byte accounting overflowed".to_string()
        })?;
        if next > self.limits.max_materialized_bytes {
            return Err(format!(
                "{} URL sources exceed the request aggregate materialized limit of {} bytes",
                block_type, self.limits.max_materialized_bytes
            ));
        }
        self.materialized_bytes = next;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct TokioDnsResolver;

impl Resolve for TokioDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host, 0))
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(addrs) as Addrs)
        })
    }
}

#[derive(Clone)]
struct SafeRemoteDnsResolver {
    inner: Arc<dyn Resolve>,
}

impl std::fmt::Debug for SafeRemoteDnsResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("SafeRemoteDnsResolver").finish()
    }
}

impl Default for SafeRemoteDnsResolver {
    fn default() -> Self {
        Self {
            inner: Arc::new(TokioDnsResolver),
        }
    }
}

impl SafeRemoteDnsResolver {
    #[cfg(test)]
    fn with_inner(inner: Arc<dyn Resolve>) -> Self {
        Self { inner }
    }
}

impl Resolve for SafeRemoteDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolving = self.inner.resolve(name);
        Box::pin(async move {
            let resolved = resolving.await?;
            let addrs = resolved.collect::<Vec<_>>();
            if addrs.is_empty() {
                return Err(Box::new(io::Error::other(
                    "remote source DNS lookup returned no addresses",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            if addrs.iter().any(|addr| is_blocked_ip(&addr.ip())) {
                return Err(Box::new(io::Error::other(
                    "remote source DNS lookup returned a blocked address",
                ))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

fn remote_multimodal_workflow_semaphore() -> Arc<Semaphore> {
    static SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_REMOTE_MULTIMODAL_WORKFLOWS)))
        .clone()
}

fn try_acquire_remote_multimodal_workflow_permit(
    semaphore: Arc<Semaphore>,
) -> Result<OwnedSemaphorePermit, String> {
    semaphore
        .try_acquire_owned()
        .map_err(|_| REMOTE_MULTIMODAL_CAPACITY_ERROR.to_string())
}

pub(crate) fn is_remote_multimodal_capacity_error(message: &str) -> bool {
    message == REMOTE_MULTIMODAL_CAPACITY_ERROR
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
                if let Some(outcome) =
                    materialize_remote_multimodal_sources(messages, caller_user_agent).await?
                {
                    report.materialized_remote_sources = outcome.materialized_sources;
                    report.remote_downloaded_bytes = outcome.downloaded_bytes;
                    report.remote_materialized_bytes = outcome.materialized_bytes;
                    report.remote_http_attempts = outcome.http_attempts;
                    report.remote_workflow_permit = Some(outcome.workflow_permit);
                }
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
            remote_downloaded_bytes = report.remote_downloaded_bytes,
            remote_materialized_bytes = report.remote_materialized_bytes,
            remote_http_attempts = report.remote_http_attempts,
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
) -> Result<Option<RemoteMaterializationOutcome>, String> {
    let source_count = count_remote_multimodal_sources(messages);
    if source_count == 0 {
        return Ok(None);
    }
    if source_count > MAX_REMOTE_MULTIMODAL_SOURCES {
        return Err(format!(
            "remote image/document source count {} exceeds the request limit of {}",
            source_count, MAX_REMOTE_MULTIMODAL_SOURCES
        ));
    }

    let workflow = async {
        let workflow_permit =
            try_acquire_remote_multimodal_workflow_permit(remote_multimodal_workflow_semaphore())?;

        let mut budget = RemoteMaterializationBudget::default();
        let client = build_remote_multimodal_client(caller_user_agent)?;
        let mut materialized = 0usize;
        for message in messages {
            materialized +=
                materialize_content_sources(&client, &mut message.content, &mut budget).await?;
        }

        Ok(RemoteMaterializationOutcome {
            materialized_sources: materialized,
            downloaded_bytes: budget.downloaded_bytes,
            materialized_bytes: budget.materialized_bytes,
            http_attempts: budget.http_attempts,
            workflow_permit,
        })
    };

    run_remote_materialization_with_deadline(REMOTE_MULTIMODAL_WORKFLOW_TIMEOUT, workflow)
        .await
        .map(Some)
}

async fn run_remote_materialization_with_deadline<F, T>(
    deadline: Duration,
    workflow: F,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    tokio::time::timeout(deadline, workflow)
        .await
        .map_err(|_| {
            format!(
                "remote image/document materialization exceeded the {} millisecond request deadline",
                deadline.as_millis()
            )
        })?
}

fn build_remote_multimodal_client(
    caller_user_agent: Option<&str>,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(REMOTE_MULTIMODAL_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .dns_resolver(Arc::new(SafeRemoteDnsResolver::default()));
    if let Some(ua) = caller_user_agent {
        if !ua.is_empty() {
            builder = builder.user_agent(ua);
        }
    }
    builder
        .build()
        .map_err(|e| format!("failed to create remote source client: {}", e))
}

fn count_remote_multimodal_sources(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| count_remote_content_sources(&message.content))
        .sum()
}

fn count_remote_content_sources(content: &Value) -> usize {
    let Value::Array(items) = content else {
        return 0;
    };
    items
        .iter()
        .filter_map(remote_source_info)
        .filter(|(_, url, _)| !url.starts_with("data:"))
        .count()
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
    budget: &mut RemoteMaterializationBudget,
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

        let (media_type, data) = download_remote_multimodal_source(
            client,
            &block_type,
            &url,
            provided_media_type,
            budget,
        )
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
    budget: &mut RemoteMaterializationBudget,
) -> Result<(String, String), String> {
    let mut current_url = url.to_string();
    let mut response = None;

    for redirect_count in 0..=MAX_REMOTE_MULTIMODAL_REDIRECTS {
        ensure_safe_remote_url(&current_url)
            .map_err(|reason| format!("{} URL rejected: {}", block_type, reason))?;

        budget.reserve_http_attempt()?;
        let candidate = client
            .get(&current_url)
            .send()
            .await
            .map_err(|e| format!("failed to download {} URL source: {}", block_type, e))?;

        if candidate.status().is_redirection() {
            if redirect_count >= MAX_REMOTE_MULTIMODAL_REDIRECTS {
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
    ensure_safe_remote_url(&final_url)
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
    if let Some(length) = response.content_length() {
        let length = usize::try_from(length).map_err(|_| {
            format!(
                "{} URL source exceeds the supported response size",
                block_type
            )
        })?;
        if budget.downloaded_bytes.saturating_add(length) > budget.limits.max_downloaded_bytes {
            return Err(format!(
                "{} URL sources exceed the request aggregate download limit of {} bytes",
                block_type, budget.limits.max_downloaded_bytes
            ));
        }
    }

    let response_media_type = response
        .headers()
        .get(REQWEST_CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(normalize_media_type);
    let bytes = read_limited_response_body(response, block_type, budget).await?;

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

    let encoded_len = base64_encoded_len(bytes.len())?;
    budget.reserve_materialized_bytes(block_type, encoded_len)?;
    Ok((media_type, BASE64_STANDARD.encode(bytes.as_slice())))
}

async fn read_limited_response_body(
    response: reqwest::Response,
    block_type: &str,
    budget: &mut RemoteMaterializationBudget,
) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| format!("failed to read {} URL source: {}", block_type, e))?;
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|next| next > MAX_REMOTE_MULTIMODAL_BYTES)
        {
            return Err(format!(
                "{} URL source exceeds {} bytes",
                block_type, MAX_REMOTE_MULTIMODAL_BYTES
            ));
        }
        budget.reserve_downloaded_bytes(block_type, chunk.len())?;
        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
}

fn base64_encoded_len(decoded_len: usize) -> Result<usize, String> {
    decoded_len
        .checked_add(2)
        .map(|value| value / 3)
        .and_then(|groups| groups.checked_mul(4))
        .ok_or_else(|| "remote source base64 size accounting overflowed".to_string())
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
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("URL source must use http or https".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URL source credentials are not allowed".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL missing host".to_string())?;
    if parsed.port() == Some(0) {
        return Err("URL port 0 is not allowed".to_string());
    }

    let lower = host.to_ascii_lowercase();
    let normalized_host = lower.trim_end_matches('.');
    const BLOCKED_HOSTS: &[&str] = &[
        "localhost",
        "ip6-localhost",
        "ip6-loopback",
        "metadata.google.internal",
        "metadata",
        "instance-data",
    ];
    if BLOCKED_HOSTS.contains(&normalized_host) || normalized_host.ends_with(".localhost") {
        return Err("URL host is blocked".to_string());
    }

    let parsed_host_ip = match parsed.host() {
        Some(::url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
        Some(::url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
        _ => host.parse::<std::net::IpAddr>().ok(),
    };
    if let Some(addr) = parsed_host_ip {
        if is_blocked_ip(&addr) {
            return Err("URL IP is in a blocked range".to_string());
        }
    }

    Ok(())
}

fn is_blocked_ip(addr: &std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    match addr {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                || octets[0] == 0
                || (octets[0] == 100 && (octets[1] & 0xc0) == 64)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
                || (octets[0] == 198 && (octets[1] & 0xfe) == 18)
                || octets[0] >= 240
                || *v4 == Ipv4Addr::new(0, 0, 0, 0)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (segments[0] & 0xffc0) == 0xfec0
                || (segments[0] & 0xe000) != 0x2000
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || (segments[0] == 0x2001 && segments[1] == 0)
                || segments[0] == 0x2002
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
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{Request, Response, StatusCode, header},
        routing::any,
    };
    use std::{
        collections::VecDeque,
        convert::Infallible,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    #[derive(Clone)]
    struct TestMediaState {
        hits: Arc<AtomicUsize>,
        port: u16,
    }

    struct TestMediaServer {
        address: SocketAddr,
        hits: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestMediaServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn test_media_handler(
        State(state): State<TestMediaState>,
        request: Request<Body>,
    ) -> Response<Body> {
        state.hits.fetch_add(1, Ordering::SeqCst);
        match request.uri().path() {
            "/png" => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "image/png")
                .body(Body::from(vec![
                    0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 1, 2, 3, 4,
                ]))
                .expect("PNG response"),
            "/chunked" => {
                let chunks = futures::stream::iter([
                    Ok::<_, Infallible>(Bytes::from_static(&[
                        0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n',
                    ])),
                    Ok::<_, Infallible>(Bytes::from_static(&[1, 2, 3, 4, 5, 6, 7, 8])),
                ]);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "image/png")
                    .body(Body::from_stream(chunks))
                    .expect("chunked response")
            }
            "/redirect-private" => Response::builder()
                .status(StatusCode::FOUND)
                .header(
                    header::LOCATION,
                    format!("http://127.0.0.1:{}/png", state.port),
                )
                .body(Body::empty())
                .expect("private redirect response"),
            "/redirect-1" => Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, "/redirect-2")
                .body(Body::empty())
                .expect("redirect response"),
            "/redirect-2" => Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, "/redirect-3")
                .body(Body::empty())
                .expect("redirect response"),
            "/redirect-3" => Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, "/png")
                .body(Body::empty())
                .expect("redirect response"),
            "/slow" => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "image/png")
                    .body(Body::from(vec![
                        0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n',
                    ]))
                    .expect("slow response")
            }
            "/slow-body" => {
                let chunks = futures::stream::once(async {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok::<_, Infallible>(Bytes::from_static(&[
                        0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n',
                    ]))
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "image/png")
                    .body(Body::from_stream(chunks))
                    .expect("slow body response")
            }
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .expect("not found response"),
        }
    }

    async fn spawn_test_media_server() -> TestMediaServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind media server");
        let address = listener.local_addr().expect("media server address");
        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .fallback(any(test_media_handler))
            .with_state(TestMediaState {
                hits: hits.clone(),
                port: address.port(),
            });
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test media");
        });
        TestMediaServer {
            address,
            hits,
            task,
        }
    }

    fn local_test_client(server: &TestMediaServer) -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .resolve("media.test", server.address)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()
            .expect("local media client")
    }

    fn local_test_url(server: &TestMediaServer, path: &str) -> String {
        format!("http://media.test:{}{path}", server.address.port())
    }

    #[derive(Debug)]
    struct SequenceResolver {
        answers: Mutex<VecDeque<Vec<SocketAddr>>>,
        calls: AtomicUsize,
    }

    impl SequenceResolver {
        fn new(answers: impl IntoIterator<Item = Vec<SocketAddr>>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Resolve for SequenceResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let answer = self
                .answers
                .lock()
                .expect("sequence resolver lock")
                .pop_front()
                .unwrap_or_default();
            Box::pin(async move { Ok(Box::new(answer.into_iter()) as Addrs) })
        }
    }

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

    #[tokio::test]
    async fn remote_source_count_is_rejected_before_dns_or_http() {
        for _round in 0..5 {
            let content = (0..=MAX_REMOTE_MULTIMODAL_SOURCES)
                .map(|index| {
                    json!({
                        "type": if index % 2 == 0 { "image" } else { "document" },
                        "source": {
                            "type": "url",
                            "url": format!("https://source-{index}.invalid/item")
                        }
                    })
                })
                .collect::<Vec<_>>();
            let mut payload = MessagesRequest {
                model: "claude-sonnet-4".to_string(),
                max_tokens: 128,
                messages: vec![AnthropicMessage {
                    role: "user".to_string(),
                    content: Value::Array(content),
                }],
                stream: false,
                system: None,
                tools: None,
                tool_choice: None,
                thinking: None,
                output_config: None,
                metadata: None,
            };

            let error = materialize_remote_multimodal_sources(&mut payload.messages, None)
                .await
                .expect_err("source count must be rejected before any lookup");
            assert!(error.contains("source count"), "{error}");
            assert!(error.contains(&MAX_REMOTE_MULTIMODAL_SOURCES.to_string()));
        }
    }

    #[test]
    fn remote_source_preflight_ignores_inline_data_urls() {
        let payload = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 128,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "image",
                        "source": {"type": "url", "url": "data:image/png;base64,iVBORw0KGgo="}
                    },
                    {
                        "type": "document",
                        "source": {"type": "url", "url": "https://example.com/a.pdf"}
                    }
                ]),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        assert_eq!(count_remote_multimodal_sources(&payload.messages), 1);
    }

    #[tokio::test]
    async fn clean_text_multimodal_path_is_value_identical_and_skips_remote_admission() {
        let store = AnthropicFileStore::default();
        for size in [1024usize, 100 * 1024, 1024 * 1024, 5 * 1024 * 1024] {
            let mut payload = MessagesRequest {
                model: "claude-sonnet-4".to_string(),
                max_tokens: 128,
                messages: vec![AnthropicMessage {
                    role: "user".to_string(),
                    content: json!([{"type": "text", "text": "x".repeat(size)}]),
                }],
                stream: false,
                system: None,
                tools: None,
                tool_choice: None,
                thinking: None,
                output_config: None,
                metadata: None,
            };
            let expected = serde_json::to_value(&payload.messages).expect("serialize fixture");
            let mut samples = Vec::with_capacity(100);

            for _ in 0..100 {
                let started = std::time::Instant::now();
                let report = prepare_multimodal_sources(
                    &store,
                    &mut payload,
                    None,
                    ImageProcessingConfig::default(),
                )
                .await
                .expect("clean text processing");
                samples.push(started.elapsed());
                assert!(!report.was_modified());
                assert_eq!(report.materialized_remote_sources, 0);
                assert_eq!(report.remote_http_attempts, 0);
                assert!(
                    report.remote_workflow_permit.is_none(),
                    "clean text unexpectedly acquired remote workflow admission"
                );
            }

            samples.sort_unstable();
            let p50 = samples[49];
            let p95 = samples[94];
            let p99 = samples[98];
            eprintln!(
                "clean-text body_processing size={size} rounds=100 p50_us={} p95_us={} p99_us={}",
                p50.as_micros(),
                p95.as_micros(),
                p99.as_micros()
            );
            assert_eq!(
                serde_json::to_value(&payload.messages).expect("serialize result"),
                expected
            );
        }
    }

    #[test]
    fn remote_request_budget_bounds_attempts_downloads_and_materialization() {
        for _round in 0..5 {
            let mut attempts = RemoteMaterializationBudget::default();
            for _ in 0..MAX_REMOTE_MULTIMODAL_HTTP_ATTEMPTS {
                attempts
                    .reserve_http_attempt()
                    .expect("attempt inside budget");
            }
            assert!(attempts.reserve_http_attempt().is_err());
            assert_eq!(attempts.http_attempts, MAX_REMOTE_MULTIMODAL_HTTP_ATTEMPTS);

            let mut bytes = RemoteMaterializationBudget::default();
            bytes
                .reserve_downloaded_bytes("image", MAX_REMOTE_MULTIMODAL_TOTAL_BYTES - 1)
                .expect("aggregate bytes inside budget");
            bytes
                .reserve_downloaded_bytes("image", 1)
                .expect("aggregate boundary is accepted");
            assert!(bytes.reserve_downloaded_bytes("image", 1).is_err());
            assert_eq!(bytes.downloaded_bytes, MAX_REMOTE_MULTIMODAL_TOTAL_BYTES);

            let mut materialized = RemoteMaterializationBudget::default();
            materialized
                .reserve_materialized_bytes("document", MAX_REMOTE_MULTIMODAL_MATERIALIZED_BYTES)
                .expect("materialized boundary is accepted");
            assert!(
                materialized
                    .reserve_materialized_bytes("document", 1)
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn remote_materialization_deadline_cancels_slow_work() {
        for _round in 0..5 {
            let result =
                run_remote_materialization_with_deadline(Duration::from_millis(5), async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok::<_, String>(())
                })
                .await;
            let error = result.expect_err("slow remote work must hit the workflow deadline");
            assert!(error.contains("request deadline"), "{error}");
        }
    }

    #[tokio::test]
    async fn global_remote_workflow_admission_is_bounded_and_recovers() {
        for _round in 0..5 {
            let semaphore = remote_multimodal_workflow_semaphore();
            let mut permits = Vec::new();
            for _ in 0..MAX_CONCURRENT_REMOTE_MULTIMODAL_WORKFLOWS {
                permits.push(
                    semaphore
                        .clone()
                        .acquire_owned()
                        .await
                        .expect("workflow permit inside global bound"),
                );
            }
            assert!(
                semaphore.clone().try_acquire_owned().is_err(),
                "global remote workflow admission exceeded its hard bound"
            );
            permits.pop();
            let recovered = semaphore
                .clone()
                .try_acquire_owned()
                .expect("released workflow permit must recover immediately");
            drop(recovered);
            drop(permits);
            assert_eq!(
                semaphore.available_permits(),
                MAX_CONCURRENT_REMOTE_MULTIMODAL_WORKFLOWS
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn remote_workflow_saturation_has_zero_waiters_and_recovers_after_bursts() {
        for burst in [1_usize, 5, 50, 500] {
            for round in 1..=5 {
                let semaphore =
                    Arc::new(Semaphore::new(MAX_CONCURRENT_REMOTE_MULTIMODAL_WORKFLOWS));
                let held = (0..MAX_CONCURRENT_REMOTE_MULTIMODAL_WORKFLOWS)
                    .map(|_| semaphore.clone().try_acquire_owned().unwrap())
                    .collect::<Vec<_>>();
                let barrier = Arc::new(tokio::sync::Barrier::new(burst));
                let started = std::time::Instant::now();
                let tasks = (0..burst)
                    .map(|_| {
                        let semaphore = semaphore.clone();
                        let barrier = barrier.clone();
                        tokio::spawn(async move {
                            barrier.wait().await;
                            try_acquire_remote_multimodal_workflow_permit(semaphore)
                        })
                    })
                    .collect::<Vec<_>>();
                for task in tasks {
                    let error = task
                        .await
                        .expect("saturation task joins")
                        .expect_err("saturated workflow must reject without queueing");
                    assert!(is_remote_multimodal_capacity_error(&error));
                }
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "burst {burst}, round {round}: fail-fast admission queued work"
                );
                assert_eq!(semaphore.available_permits(), 0);

                drop(held);
                for recovery_round in 1..=5 {
                    let permit = try_acquire_remote_multimodal_workflow_permit(semaphore.clone())
                        .unwrap_or_else(|error| {
                            panic!(
                                "burst {burst}, round {round}, recovery {recovery_round}: {error}"
                            )
                        });
                    drop(permit);
                }
                assert_eq!(
                    semaphore.available_permits(),
                    MAX_CONCURRENT_REMOTE_MULTIMODAL_WORKFLOWS
                );
            }
        }
    }

    #[tokio::test]
    async fn safe_dns_resolver_filters_every_connection_lookup() {
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 0);
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let inner = Arc::new(SequenceResolver::new([vec![public], vec![loopback]]));
        let resolver = SafeRemoteDnsResolver::with_inner(inner.clone());

        let first = resolver
            .resolve("rebind.example".parse().expect("DNS name"))
            .await
            .expect("public address should pass")
            .collect::<Vec<_>>();
        assert_eq!(first, vec![public]);

        let second = resolver
            .resolve("rebind.example".parse().expect("DNS name"))
            .await;
        assert!(
            second.is_err(),
            "a later private DNS answer must be rejected"
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn reqwest_transport_cannot_connect_to_resolver_blocked_address() {
        for _round in 0..5 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind test listener");
            let port = listener.local_addr().expect("listener address").port();
            let inner = Arc::new(SequenceResolver::new([vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                port,
            )]]));
            let resolver = SafeRemoteDnsResolver::with_inner(inner.clone());
            let client = reqwest::Client::builder()
                .no_proxy()
                .dns_resolver(Arc::new(resolver))
                .timeout(Duration::from_secs(1))
                .build()
                .expect("test client");

            let accepted = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_millis(250), listener.accept())
                    .await
                    .is_ok()
            });
            let result = client
                .get(format!("http://blocked.example:{port}/image.png"))
                .send()
                .await;

            assert!(
                result.is_err(),
                "blocked resolver result must fail the request"
            );
            assert!(
                !accepted.await.expect("accept task"),
                "server was contacted"
            );
            assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn remote_download_preserves_supported_single_source_behavior() {
        for _round in 0..5 {
            let server = spawn_test_media_server().await;
            let client = local_test_client(&server);
            let mut budget = RemoteMaterializationBudget::default();
            let (media_type, data) = download_remote_multimodal_source(
                &client,
                "image",
                &local_test_url(&server, "/png"),
                None,
                &mut budget,
            )
            .await
            .expect("supported remote PNG");

            assert_eq!(media_type, "image/png");
            assert_eq!(
                BASE64_STANDARD.decode(data).expect("base64 PNG"),
                vec![
                    0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 1, 2, 3, 4
                ]
            );
            assert_eq!(budget.http_attempts, 1);
            assert_eq!(budget.downloaded_bytes, 12);
            assert_eq!(budget.materialized_bytes, 16);
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn chunked_remote_body_stops_at_aggregate_limit() {
        for _round in 0..5 {
            let server = spawn_test_media_server().await;
            let client = local_test_client(&server);
            let mut budget =
                RemoteMaterializationBudget::with_limits(RemoteMaterializationLimits {
                    max_downloaded_bytes: 12,
                    max_materialized_bytes: 64,
                    max_http_attempts: 4,
                });
            let error = download_remote_multimodal_source(
                &client,
                "image",
                &local_test_url(&server, "/chunked"),
                None,
                &mut budget,
            )
            .await
            .expect_err("chunked response must stop at the request aggregate bound");

            assert!(error.contains("aggregate download limit"), "{error}");
            assert!(budget.downloaded_bytes <= 12);
            assert_eq!(budget.materialized_bytes, 0);
            assert_eq!(budget.http_attempts, 1);
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn redirect_to_private_address_is_rejected_before_second_http_attempt() {
        for _round in 0..5 {
            let server = spawn_test_media_server().await;
            let client = local_test_client(&server);
            let mut budget = RemoteMaterializationBudget::default();
            let error = download_remote_multimodal_source(
                &client,
                "image",
                &local_test_url(&server, "/redirect-private"),
                None,
                &mut budget,
            )
            .await
            .expect_err("redirect to loopback must be rejected");

            assert!(error.contains("blocked range"), "{error}");
            assert_eq!(budget.http_attempts, 1);
            assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn redirects_share_the_request_http_attempt_budget() {
        for _round in 0..5 {
            let server = spawn_test_media_server().await;
            let client = local_test_client(&server);
            let mut budget =
                RemoteMaterializationBudget::with_limits(RemoteMaterializationLimits {
                    max_downloaded_bytes: 64,
                    max_materialized_bytes: 128,
                    max_http_attempts: 2,
                });
            let error = download_remote_multimodal_source(
                &client,
                "image",
                &local_test_url(&server, "/redirect-1"),
                None,
                &mut budget,
            )
            .await
            .expect_err("redirect chain must share the request attempt budget");

            assert!(error.contains("HTTP attempt limit"), "{error}");
            assert_eq!(budget.http_attempts, 2);
            assert_eq!(server.hits.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn slow_remote_fetch_is_cancelled_and_followed_by_normal_recovery() {
        for _round in 0..5 {
            let server = spawn_test_media_server().await;
            let client = local_test_client(&server);
            let slow_url = local_test_url(&server, "/slow-body");
            let slow =
                run_remote_materialization_with_deadline(Duration::from_millis(500), async {
                    let mut budget = RemoteMaterializationBudget::default();
                    download_remote_multimodal_source(
                        &client,
                        "image",
                        &slow_url,
                        None,
                        &mut budget,
                    )
                    .await
                })
                .await;
            assert!(
                slow.is_err(),
                "slow fetch must be cancelled by workflow deadline"
            );

            let mut recovery_budget = RemoteMaterializationBudget::default();
            download_remote_multimodal_source(
                &client,
                "image",
                &local_test_url(&server, "/png"),
                None,
                &mut recovery_budget,
            )
            .await
            .expect("normal fetch must recover after cancelled slow source");
            assert_eq!(recovery_budget.http_attempts, 1);
            assert!(server.hits.load(Ordering::SeqCst) >= 2);
        }
    }

    #[test]
    fn remote_url_validation_rejects_credentials_and_non_global_targets() {
        let rejected = [
            "ftp://example.com/image.png",
            "http://user:pass@example.com/image.png",
            "http://localhost./image.png",
            "http://127.1/image.png",
            "http://[::1]/image.png",
            "http://198.18.0.1/image.png",
            "http://[2001:db8::1]/image.png",
            "http://example.com:0/image.png",
        ];
        for url in rejected {
            assert!(
                ensure_safe_remote_url(url).is_err(),
                "accepted unsafe URL {url}"
            );
        }

        for url in [
            "https://example.com/image.png",
            "https://93.184.216.34/image.png",
            "https://[2606:4700:4700::1111]/image.png",
        ] {
            ensure_safe_remote_url(url).unwrap_or_else(|error| {
                panic!("rejected public URL {url}: {error}");
            });
        }
    }
}
