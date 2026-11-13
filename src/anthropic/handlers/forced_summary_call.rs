//! 强制模型收敛的摘要调用：用目标模型把早期对话浓缩成工作记录（分析文档 12 篇）。
//!
//! 走与普通请求相同的转换、调度和准入；用独立的尝试预算，不占主请求的重试次数。摘要只计入
//! 内部成本，不进用户 usage。任何失败都返回 `None`，由调用方回退到硬裁剪。

use futures::FutureExt;
use futures::future::BoxFuture;

use super::*;
use crate::anthropic::forced_summary;

/// 一次摘要所需的请求上下文（全部拥有所有权：生成在后台任务里继续，不随请求结束而取消）。
#[derive(Clone)]
pub(super) struct SummaryContext {
    pub(super) state: AppState,
    pub(super) provider: Arc<KiroProvider>,
    pub(super) endpoint: String,
    pub(super) runtime_config: Arc<RequestRuntimeConfig>,
    pub(super) cache_route: ResolvedCacheRoutePolicy,
    /// 目标模型的解析结果（`forced` 已清空：摘要请求不注入身份卡和知识）。
    pub(super) model_resolution: ModelResolution,
    /// 外显模型的显示名与标准 ID，用于改写摘要里的自述。
    pub(super) display_name: String,
    pub(super) display_id: String,
}

/// 按顺序滚动摘要各段文字记录：每一段都在上一份记录的基础上更新。
pub(super) fn summarize(
    context: SummaryContext,
    previous: Option<String>,
    chunks: Vec<String>,
) -> BoxFuture<'static, Option<String>> {
    summarize_with(context, previous, chunks, None)
}

pub(super) fn summarize_with(
    context: SummaryContext,
    previous: Option<String>,
    chunks: Vec<String>,
    instructions: Option<String>,
) -> BoxFuture<'static, Option<String>> {
    async move {
        let started = std::time::Instant::now();
        let mut record = previous;
        let chunk_count = chunks.len();
        for chunk in &chunks {
            record = Some(
                summarize_once(&context, record.as_deref(), chunk, instructions.as_deref()).await?,
            );
        }
        let record = record.map(|record| forced_summary::with_verbatim_findings(record, &chunks));
        tracing::info!(
        endpoint = %context.endpoint,
        chunks = chunk_count,
        elapsed_ms = started.elapsed().as_millis() as u64,
        summary_chars = record.as_deref().map_or(0, |text| text.chars().count()),
        "forced model mapping wrote a context checkpoint summary"
           );
        record
    }
    .boxed()
}

async fn summarize_once(
    context: &SummaryContext,
    previous: Option<&str>,
    transcript: &str,
    instructions: Option<&str>,
) -> Option<String> {
    let model = context
        .model_resolution
        .upstream_model
        .clone()
        .unwrap_or_default();
    let request = forced_summary::summary_request(
        context.model_resolution.requested_model.as_str(),
        previous,
        transcript,
        instructions,
    );
    let native_reasoning_capability = context
        .state
        .model_capabilities
        .reasoning_capability_state_for(&model, &context.provider.model_capability_cohort_keys());
    let prepared = match local_body_pipeline::prepare(
        &context.endpoint,
        &request,
        &context.runtime_config,
        &context.cache_route,
        &context.model_resolution,
        native_reasoning_capability,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(
            status = %error.response.status(),
            "forced model mapping summary request could not be prepared"
               );
            return None;
        }
    };
    let budget = Arc::new(InferenceAttemptBudget::with_auxiliary_max_attempts(
        context.runtime_config.inference_upstream_max_attempts,
        context.runtime_config.auxiliary_upstream_max_attempts,
    ));
    budget.set_dispatch_route(&context.endpoint);
    budget.set_dispatch_deadline_after(std::time::Duration::from_secs(30));
    let request_id = envelope::request_id();
    let api_response = match call_api_maybe_fail_fast(
        &context.provider,
        &prepared.request_body,
        Some(&prepared.kiro_request),
        Some(request_id.as_str()),
        None,
        capacity_weight_units_for_local_request(context.provider.as_ref(), prepared.input_tokens),
        Some(model.as_str()),
        budget,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "forced model mapping summary call failed");
            return None;
        }
    };
    let (response, completion) = api_response.into_parts();
    let runtime = context.provider.runtime_config_snapshot();
    let content_type = response
        .headers()
        .get(REQWEST_CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = match read_non_stream_body_with_first_output_deadline(
        response,
        runtime.kiro_upstream_stream_idle_timeout_secs,
        LOCAL_NON_STREAM_RESPONSE_MAX_BYTES,
        runtime
            .stream_first_output_timeout_secs
            .min(crate::model::config::STREAM_FIRST_OUTPUT_TIMEOUT_MAX_SECS),
        runtime
            .kiro_upstream_stream_max_duration_secs
            .min(crate::model::config::KIRO_UPSTREAM_STREAM_MAX_DURATION_MAX_SECS),
        || completion.touch(),
    )
    .await
    {
        Ok(body) => body,
        Err(error) => {
            completion.release();
            tracing::warn!(%error, "forced model mapping summary response could not be read");
            return None;
        }
    };
    if crate::diagnostics::capture::is_active() {
        capture_complete_upstream_body(&request_id, None, &body);
    }
    let events = match inspect_complete_upstream_body(content_type.as_deref(), body)
        .map_err(|error| error.internal_detail)
        .and_then(|body| decode_complete_eventstream(&body))
    {
        Ok(events) => events,
        Err(detail) => {
            completion.release();
            tracing::warn!(
                detail,
                "forced model mapping summary response was malformed"
            );
            return None;
        }
    };
    let mut text = String::new();
    let mut refusal = None;
    for event in events {
        match event {
            Event::AssistantResponse(response) => text.push_str(&response.content),
            Event::Metadata(metadata) => {
                refusal = refusal.or_else(|| metadata.content_filter_refusal());
            }
            Event::Error { .. } | Event::Exception { .. } => {
                completion.release();
                tracing::warn!("forced model mapping summary response carried an upstream error");
                return None;
            }
            _ => {}
        }
    }
    completion.report_success();
    if let Some(refusal) = refusal {
        tracing::warn!(
            request_id = %request_id,
            refusal,
            transcript_chars = transcript.chars().count(),
            "forced model mapping summary was refused by the upstream content filter"
        );
        return None;
    }
    if text.trim().is_empty() {
        tracing::warn!(request_id = %request_id, "forced model mapping summary came back empty");
    }
    let text = if super::super::control_block_cleaner::request_mentions_control_tags(&request) {
        text
    } else {
        super::super::control_block_cleaner::strip_control_blocks(&text).into_owned()
    };
    let text = strip_thinking_tags(&text);
    // The summary is replayed as context on later turns: a self-description naming the serving
    // model or the channel would leak into every answer after it.
    let text = super::super::identity_guard::scrub_summary_text(
        &text,
        &context.display_name,
        &context.display_id,
    );
    forced_summary::clamp_summary(&text)
}

/// 提示词思考模式下模型可能先写 `<thinking>…</thinking>`；摘要只要正文。
fn strip_thinking_tags(text: &str) -> String {
    match (text.find("<thinking>"), text.find("</thinking>")) {
        (Some(start), Some(end)) if start < end => {
            format!("{}{}", &text[..start], &text[end + "</thinking>".len()..])
        }
        _ => text.to_string(),
    }
}

/// 拼装发给上游的消息：先用已有的检查点；超出预算时生成（或等待）下一个检查点；越过预压缩线时
/// 在后台预先生成。返回是否改写了消息。
pub(super) async fn apply_summary_checkpoint(
    payload: &mut MessagesRequest,
    context: SummaryContext,
    budget_tokens: i32,
    backoff: &crate::model::forced_model_mapping::ForcedContextBackoffConfig,
) -> bool {
    if !backoff.summary_enabled || payload.messages.len() < 4 {
        return false;
    }
    let messages = payload.messages.clone();
    let hashes = forced_summary::prefix_hashes(&messages);
    let tokens = forced_summary::message_tokens(&messages);
    let fixed_tokens = crate::token::count_all_tokens_local(
        payload.system.as_deref(),
        &[],
        payload.tools.as_deref(),
    )
    .min(i32::MAX as u64) as i32;
    let total = |messages: &[crate::anthropic::types::Message]| {
        fixed_tokens.saturating_add(
            forced_summary::message_tokens(messages)
                .iter()
                .fold(0i32, |sum, count| sum.saturating_add(*count)),
        )
    };
    let existing = forced_summary::latest_checkpoint(&messages, &hashes);
    let mut current = existing
        .as_ref()
        .map(|checkpoint| forced_summary::apply_checkpoint(&messages, checkpoint))
        .unwrap_or_else(|| messages.clone());
    let estimate = total(&current);
    let precompact_line = (f64::from(budget_tokens) * backoff.summary_precompact_ratio) as i32;
    if estimate <= precompact_line {
        let changed = existing.is_some();
        payload.messages = current;
        return changed;
    }

    // Plan the next checkpoint: keep a tail that, with the first message and a summary, lands
    // under the target line.
    let target_line = (f64::from(budget_tokens) * backoff.summary_target_ratio) as i32;
    let first_tokens = tokens.first().copied().unwrap_or(0);
    let tail_budget = target_line
        .saturating_sub(fixed_tokens)
        .saturating_sub(first_tokens)
        .saturating_sub(forced_summary::SUMMARY_RESERVE_TOKENS);
    let after = existing.as_ref().map_or(0, |checkpoint| checkpoint.cut);
    let Some(cut) = forced_summary::next_cut(&messages, &tokens, tail_budget, after) else {
        payload.messages = current;
        return existing.is_some();
    };
    let key = hashes[cut - 1];
    let previous = existing
        .as_ref()
        .map(|checkpoint| checkpoint.summary.clone());
    let start = existing.as_ref().map_or(1, |checkpoint| checkpoint.cut);
    let chunks = forced_summary::transcript_chunks(&messages[start..cut], &tokens[start..cut]);
    if chunks.is_empty() {
        payload.messages = current;
        return existing.is_some();
    }
    let pending = forced_summary::start_or_join(key, move || summarize(context, previous, chunks));
    if estimate <= budget_tokens {
        // Fits as is: the next checkpoint is prepared in the background for a later request.
        payload.messages = current;
        return existing.is_some();
    }
    let Some(pending) = pending else {
        payload.messages = current;
        return existing.is_some();
    };
    let timeout = std::time::Duration::from_secs(backoff.summary_timeout_secs);
    match tokio::time::timeout(timeout, pending).await {
        Ok(Some(summary)) => {
            current = forced_summary::apply_checkpoint(
                &messages,
                &forced_summary::Checkpoint { cut, summary },
            );
        }
        Ok(None) => {
            tracing::warn!("forced model mapping summary failed; falling back to trimming");
        }
        Err(_) => {
            tracing::warn!(
                timeout_secs = backoff.summary_timeout_secs,
                "forced model mapping summary timed out; trimming this request, the summary keeps running"
            );
        }
    }
    let changed = current.len() != messages.len() || existing.is_some();
    payload.messages = current;
    changed
}

/// 按需压缩（顶层 `compaction: {type: "summarize"}`，beta `compact-2026-09-04`）：对整段对话写一份
/// 摘要，以 `compaction` 块和 `stop_reason: "compaction"` 返回，usage 为 0。摘要失败时返回
/// `stop_reason: "end_turn"`、不带内容，客户端按官方语义判定"没有拿到摘要"。
pub(super) async fn on_demand_compaction_response(
    payload: &MessagesRequest,
    context: SummaryContext,
    display_model: &str,
    instructions: Option<String>,
) -> Response {
    let tokens = forced_summary::message_tokens(&payload.messages);
    let chunks = forced_summary::transcript_chunks(&payload.messages, &tokens);
    let summary = if chunks.is_empty() {
        None
    } else {
        summarize_with(context, None, chunks, instructions).await
    };
    let (content, stop_reason) = match summary {
        Some(summary) => {
            let signature = super::super::signature::synthetic_thinking_signature(&summary);
            (
                vec![serde_json::json!({
                    "type": "compaction",
                    "content": summary,
                    "signature": signature,
                })],
                "compaction",
            )
        }
        None => (Vec::new(), "end_turn"),
    };
    let message = serde_json::json!({
    "id": envelope::message_id(),
           "type": "message",
    "role": "assistant",
    "model": display_model,
           "content": content,
           "stop_reason": stop_reason,
    "stop_sequence": null,
    "usage": {
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
    },
       });
    if !payload.stream {
        return Json(message).into_response();
    }
    let request_id = envelope::request_id();
    let mut start = message.clone();
    start["content"] = serde_json::json!([]);
    start["stop_reason"] = serde_json::Value::Null;
    let mut events = vec![super::super::stream::SseEvent::new(
        "message_start",
        serde_json::json!({"type": "message_start", "message": start}),
    )];
    for (index, block) in message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        events.push(super::super::stream::SseEvent::new(
     "content_block_start",
     serde_json::json!({"type": "content_block_start", "index": index, "content_block": block}),
 ));
        events.push(super::super::stream::SseEvent::new(
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": index}),
        ));
    }
    events.push(super::super::stream::SseEvent::new(
        "message_delta",
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": message["usage"],
        }),
    ));
    events.push(super::super::stream::SseEvent::new(
        "message_stop",
        serde_json::json!({"type": "message_stop"}),
    ));
    let body: String = events.iter().map(|event| event.to_sse_string()).collect();
    envelope::sse_builder_with_id(&request_id)
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| Json(message).into_response())
}
