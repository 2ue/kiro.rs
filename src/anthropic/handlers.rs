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
use crate::model::config::PromptCacheSimulationMode;
use crate::token;
use anyhow::Error;
use axum::{
    Json as JsonExtractor,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use chrono::Utc;
use futures::{Stream, StreamExt, stream};
use serde_json::json;
use std::time::Duration;
use tokio::time::{Instant, interval, sleep_until};
use uuid::Uuid;

use super::converter::{ConversionError, convert_request, extract_stable_conversation_id};
use super::middleware::AppState;
use super::prompt_cache::{PromptCacheProfile, PromptCacheScope};
use super::stream::{SseEvent, StreamContext};
use super::types::{
    CountTokensRequest, CountTokensResponse, ErrorResponse, MessagesRequest, Model, ModelsResponse,
    OutputConfig, Thinking,
};
use super::usage::{UsageRecord, UsageRecordStatus, UsageSource};
use super::websearch;
use crate::kiro::provider::KiroStreamCompletion;

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
        has_metadata: bool,
        context_estimated: bool,
    ) -> UsageSource {
        if has_metadata {
            UsageSource::UpstreamMetadata
        } else if self.request.simulated_source.is_some()
            && (usage.cache_read_input_tokens > 0 || usage.cache_creation_input_tokens > 0)
        {
            self.request.simulated_source.unwrap()
        } else if context_estimated {
            UsageSource::ContextEstimate
        } else {
            UsageSource::RequestEstimate
        }
    }

    fn record_success_from_stream(&self, ctx: &StreamContext) {
        let Some(usage) = ctx.final_usage() else {
            return;
        };
        let has_metadata = ctx.metadata_usage_seen();
        let context_estimated = !has_metadata && ctx.context_input_tokens_seen();
        let usage_source = self.usage_source(&usage, has_metadata, context_estimated);
        self.record_success(usage, usage_source, context_estimated);
    }

    fn record_stream_failure_from_context(
        &self,
        status: UsageRecordStatus,
        usage: Option<super::cache::CacheUsage>,
        error_detail: Option<(String, String)>,
        has_metadata: bool,
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
        let source = self.usage_source(&usage, has_metadata, context_estimated);
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

fn request_id() -> String {
    format!("req_{}", Uuid::new_v4().to_string().replace('-', ""))
}

fn credential_label(provider: &crate::kiro::provider::KiroProvider, id: u64) -> Option<String> {
    provider.credential_label(id)
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
    let prompt_cache_profile = state.prompt_cache.build_profile(payload, input_tokens);
    let (simulated_usage, simulated_source) = build_simulated_usage(
        state,
        stable_conversation_id.as_deref(),
        prompt_cache_profile.as_ref(),
    );

    RequestUsageContext {
        recorder: state.usage_recorder.clone(),
        prompt_cache: state.prompt_cache.clone(),
        request_id: request_id(),
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
        PromptCacheSimulationMode::LocalPromptCache => {
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
    if usage_context.simulation_mode == PromptCacheSimulationMode::LocalPromptCache {
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
fn map_provider_error(err: Error) -> Response {
    let err_str = err.to_string();

    // 上下文窗口满了（对话历史累积超出模型上下文窗口限制）
    if err_str.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") {
        tracing::warn!(error = %err, "上游拒绝请求：上下文窗口已满（不应重试）");
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                "Context window is full. Reduce conversation history, system prompt, or tools.",
            )),
        )
            .into_response();
    }

    // 单次输入太长（请求体本身超出上游限制）
    if err_str.contains("Input is too long") {
        tracing::warn!(error = %err, "上游拒绝请求：输入过长（不应重试）");
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "invalid_request_error",
                "Input is too long. Reduce the size of your messages.",
            )),
        )
            .into_response();
    }
    tracing::error!("Kiro API 调用失败: {}", err);
    (
        StatusCode::BAD_GATEWAY,
        Json(ErrorResponse::new(
            "api_error",
            format!("上游 API 调用失败: {}", err),
        )),
    )
        .into_response()
}

/// GET /v1/models
///
/// 返回可用的模型列表
pub async fn get_models() -> impl IntoResponse {
    tracing::info!("Received GET /v1/models request");

    let models = vec![
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
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::new(
                    "service_unavailable",
                    "Kiro API provider not configured",
                )),
            )
                .into_response();
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
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
    let conversion_result = match convert_request(&payload) {
        Ok(result) => result,
        Err(e) => {
            let (error_type, message) = match &e {
                ConversionError::UnsupportedModel(model) => {
                    ("invalid_request_error", format!("模型不支持: {}", model))
                }
                ConversionError::EmptyMessages => {
                    ("invalid_request_error", "消息列表为空".to_string())
                }
            };
            tracing::warn!("请求转换失败: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(error_type, message)),
            )
                .into_response();
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
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    "internal_error",
                    format!("序列化请求失败: {}", e),
                )),
            )
                .into_response();
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

    if payload.stream {
        // 流式响应
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            thinking_enabled,
            tool_name_map,
            usage_context,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = state.extract_thinking && thinking_enabled;
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            extract_thinking,
            tool_name_map,
            usage_context,
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
    tool_name_map: HashMap<String, String>,
    usage_context: RequestUsageContext,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let response = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            usage_context
                .attach_credential(None, None, false, false)
                .record_failure(UsageRecordStatus::Error, "api_error", message);
            return map_provider_error(e);
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
        tool_name_map,
        credential_usage.request.simulated_usage,
    );

    // 生成初始事件
    let initial_events = ctx.generate_initial_events();

    // 创建 SSE 流
    let stream = create_sse_stream(response, ctx, initial_events, completion, credential_usage);

    // 返回 SSE 响应
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(stream))
        .unwrap()
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
                                ctx.metadata_usage_seen(),
                                !ctx.metadata_usage_seen() && ctx.context_input_tokens_seen(),
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
                                    ctx.metadata_usage_seen(),
                                    !ctx.metadata_usage_seen() && ctx.context_input_tokens_seen(),
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
                        ctx.metadata_usage_seen(),
                        !ctx.metadata_usage_seen() && ctx.context_input_tokens_seen(),
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
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let api_response = match provider.call_api_with_context(request_body).await {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            usage_context
                .attach_credential(None, None, false, false)
                .record_failure(UsageRecordStatus::Error, "api_error", message);
            return map_provider_error(e);
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
            return (
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse::new(
                    "api_error",
                    format!("读取响应失败: {}", e),
                )),
            )
                .into_response();
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
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(ErrorResponse::new("invalid_request_error", message)),
                            )
                                .into_response();
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

    let usage = super::cache::build_usage_with_simulation(
        metadata_usage.as_ref(),
        final_input_tokens,
        output_tokens,
        credential_usage.request.simulated_usage,
    );
    let has_metadata = metadata_usage.is_some();
    let context_estimated = !has_metadata && context_input_tokens.is_some();
    let usage_source = credential_usage.usage_source(&usage, has_metadata, context_estimated);
    credential_usage.record_success(usage, usage_source, context_estimated);
    provider.report_success_for_context(
        api_response.credential_id,
        api_response.session_id.as_deref(),
    );

    // 构建 Anthropic 响应
    let response_body = json!({
        "id": format!("msg_{}", Uuid::new_v4().to_string().replace('-', "")),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage.to_json()
    });

    (StatusCode::OK, Json(response_body)).into_response()
}

/// 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
///
/// - Opus 4.6：覆写为 adaptive 类型
/// - 其他模型：覆写为 enabled 类型
/// - budget_tokens 固定为 20000
fn override_thinking_from_model_name(payload: &mut MessagesRequest) {
    let model_lower = payload.model.to_lowercase();
    if !model_lower.contains("thinking") {
        return;
    }

    let is_opus_4_6 = model_lower.contains("opus")
        && (model_lower.contains("4-6") || model_lower.contains("4.6"));

    let thinking_type = if is_opus_4_6 { "adaptive" } else { "enabled" };

    tracing::info!(
        model = %payload.model,
        thinking_type = thinking_type,
        "模型名包含 thinking 后缀，覆写 thinking 配置"
    );

    payload.thinking = Some(Thinking {
        thinking_type: thinking_type.to_string(),
        budget_tokens: 20000,
    });

    if is_opus_4_6 {
        payload.output_config = Some(OutputConfig {
            effort: "high".to_string(),
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
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::new(
                    "service_unavailable",
                    "Kiro API provider not configured",
                )),
            )
                .into_response();
        }
    };

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
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
    let conversion_result = match convert_request(&payload) {
        Ok(result) => result,
        Err(e) => {
            let (error_type, message) = match &e {
                ConversionError::UnsupportedModel(model) => {
                    ("invalid_request_error", format!("模型不支持: {}", model))
                }
                ConversionError::EmptyMessages => {
                    ("invalid_request_error", "消息列表为空".to_string())
                }
            };
            tracing::warn!("请求转换失败: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(error_type, message)),
            )
                .into_response();
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
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    "internal_error",
                    format!("序列化请求失败: {}", e),
                )),
            )
                .into_response();
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

    if payload.stream {
        // 流式响应（实时模式）
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            thinking_enabled,
            tool_name_map,
            usage_context,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = state.extract_thinking && thinking_enabled;
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            input_tokens,
            extract_thinking,
            tool_name_map,
            usage_context,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::cache::CacheUsage;
    use crate::anthropic::prompt_cache::PromptCacheTracker;
    use crate::anthropic::types::Message;
    use crate::anthropic::usage::UsageRecorder;
    use serde_json::json;

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
}
