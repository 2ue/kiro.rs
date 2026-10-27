//! Model-driven native WebSearch (route switch `routeFeatures.modelDrivenWebSearch`).
//!
//! The official server tool loop is run by the proxy: the model receives `web_search` as an
//! ordinary tool and decides whether and what to search; each call is executed through Kiro MCP
//! and the results go back to the model, which writes the answer with numbered citations. The
//! client sees `server_tool_use` / `web_search_tool_result` blocks and text blocks with
//! `web_search_result_location` citations. Token usage is the sum over every model call in the
//! loop, then passes through the route's usage shaping like any other request.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use super::*;
use crate::anthropic::types::{Message, SystemMessage, Tool};
use crate::kiro::call_trace::McpCallAttributionSink;

/// Searches per request when the client sets no `max_uses`.
const DEFAULT_MAX_SEARCHES: u32 = 3;
/// Hard ceiling on searches per request, whatever `max_uses` says.
const MAX_SEARCHES_CEILING: u32 = 5;
const SNIPPET_MAX_CHARS: usize = 2000;
const CITED_TEXT_MAX_CHARS: usize = 150;
const SEARCH_TOOL_NAME: &str = "web_search";
const SEARCH_TOOL_DESCRIPTION: &str = "Search the web for current or external information. Use a concise query. Results are numbered so they can be cited.";
/// Part of every result list header: a reminder next to where the answer is generated is
/// followed more reliably than the system prompt alone. Phrased as a description of the data,
/// not an instruction, so the model does not open its answer by acknowledging it.
const CITATION_HINT: &str = "cite a result with its number in square brackets, e.g. [1]";

/// Everything the loop needs, owned: in streaming mode the loop continues in a background task
/// after the response headers and the first events have been sent.
pub(super) struct ModelDrivenWebSearchContext {
    pub(super) state: AppState,
    pub(super) provider: Arc<KiroProvider>,
    pub(super) payload: MessagesRequest,
    pub(super) endpoint: String,
    pub(super) runtime_config: RequestRuntimeConfig,
    pub(super) cache_route: ResolvedCacheRoutePolicy,
    pub(super) model_resolution: ModelResolution,
    pub(super) usage_mapper: Option<websearch::SharedWebSearchUsageMapper>,
    pub(super) inference_attempt_budget: Arc<InferenceAttemptBudget>,
    pub(super) attribution_sink: Arc<McpCallAttributionSink>,
    pub(super) identity_output_guard: bool,
    pub(super) request_id: String,
    pub(super) error_id: String,
}

/// One numbered search result, as cited by the model.
#[derive(Debug, Clone)]
struct Source {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug, Default)]
struct RoundOutput {
    text: String,
    /// (name, input) in call order, names restored to the client side.
    tool_calls: Vec<(String, Value)>,
    input_tokens: i32,
    output_tokens: i32,
}

struct CallFailure {
    response: Response,
    error_type: &'static str,
    internal_reason: &'static str,
    attribution: McpCallAttribution,
}

fn processing_failure_for(
    ctx: &ModelDrivenWebSearchContext,
    attribution: McpCallAttribution,
    internal_reason: &'static str,
) -> CallFailure {
    CallFailure {
        response: envelope::error_response_with_id(
            StatusCode::BAD_GATEWAY,
            "api_error",
            envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
            &ctx.request_id,
        ),
        error_type: "api_error",
        internal_reason,
        attribution,
    }
}

#[derive(Debug, Clone, Default)]
struct Accounting {
    input_tokens: i32,
    output_tokens: i32,
    credential_id: Option<u64>,
    attempts: Vec<KiroCredentialAttempt>,
}

impl Accounting {
    fn attribution(&self, provider: &KiroProvider) -> McpCallAttribution {
        McpCallAttribution {
            credential_id: self.credential_id,
            credential_label: self
                .credential_id
                .and_then(|id| provider.credential_label(id)),
            attempts: self.attempts.clone(),
            selection_failure: None,
        }
    }
}

/// Where the loop writes its content blocks: collected for a JSON response, or sent to the client
/// as soon as each block is complete when streaming.
struct BlockOutput {
    blocks: Vec<Value>,
    writer: Option<StreamWriter>,
}

impl BlockOutput {
    /// Returns false once the streaming client has gone away.
    async fn push(&mut self, block: Value) -> bool {
        match self.writer.as_mut() {
            Some(writer) => writer.push_block(&block).await,
            None => {
                self.blocks.push(block);
                true
            }
        }
    }
}

struct StreamWriter {
    tx: tokio::sync::mpsc::Sender<Bytes>,
    next_index: usize,
    keepalive: Duration,
}

impl StreamWriter {
    async fn send(&mut self, event: SseEvent) -> bool {
        self.tx
            .send(Bytes::from(event.to_sse_string()))
            .await
            .is_ok()
    }

    async fn ping(&mut self) -> bool {
        self.tx.send(create_ping_sse()).await.is_ok()
    }

    async fn push_block(&mut self, block: &Value) -> bool {
        let index = self.next_index;
        self.next_index += 1;
        for event in block_events(index, block) {
            if !self.send(event).await {
                return false;
            }
        }
        true
    }
}

/// Awaits `future`, sending `ping` events at the keepalive interval while streaming. `None`
/// means the client disconnected; the future is dropped so no further upstream work runs.
async fn with_keepalive<F: std::future::Future>(
    output: &mut BlockOutput,
    future: F,
) -> Option<F::Output> {
    let Some(writer) = output.writer.as_mut() else {
        return Some(future.await);
    };
    let period = writer.keepalive;
    let mut ticker = tokio::time::interval_at(Instant::now() + period, period);
    tokio::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Some(output),
            _ = ticker.tick() => {
                if !writer.ping().await {
                    return None;
                }
            }
        }
    }
}

enum DriveError {
    /// Boxed: `CallFailure` holds a whole error response.
    Model(Box<CallFailure>),
    ClientGone,
}

struct DriveResult {
    searches: u32,
    stop_reason: &'static str,
}

pub(super) async fn run(ctx: ModelDrivenWebSearchContext) -> websearch::WebSearchOutcome {
    let max_searches = websearch::native_web_search_max_uses(&ctx.payload)
        .map_or(DEFAULT_MAX_SEARCHES, |uses| uses.max(1) as u32)
        .min(MAX_SEARCHES_CEILING);
    // Each search costs one MCP send and one more model call, plus one answer-only retry.
    ctx.inference_attempt_budget.raise_max_attempts(
        ctx.inference_attempt_budget
            .max_attempts()
            .saturating_add(max_searches.saturating_mul(2))
            .saturating_add(1),
    );

    let injections = ctx
        .runtime_config
        .injections_for(&ctx.endpoint, crate::model::config::UpstreamKind::Local);
    let guidance = injections
        .allows(crate::model::config::InjectionId::WebSearchGuidance)
        .then(|| {
            injections
                .text(crate::model::prompt_injection::TextKey::WebSearchGuidance)
                .to_string()
        });
    let mut request = internal_request(&ctx.payload, guidance.as_deref());
    if websearch::is_claude_code_search_request(&ctx.payload) {
        // The Claude Code WebSearch sub-request always expects a search.
        request.tool_choice = Some(json!({"type": "tool", "name": SEARCH_TOOL_NAME}));
    }

    // The first model call runs before anything is sent, so account selection retries and an
    // HTTP error status stay available when it fails.
    let mut accounting = Accounting::default();
    let first = match call_model(&ctx, &request, &mut accounting).await {
        Ok(round) => round,
        Err(failure) => return failure_outcome(failure),
    };

    if !ctx.payload.stream {
        let mut output = BlockOutput {
            blocks: Vec::new(),
            writer: None,
        };
        let result = match drive(
            &ctx,
            request,
            first,
            &mut accounting,
            &mut output,
            max_searches,
        )
        .await
        {
            Ok(result) => result,
            Err(DriveError::Model(failure)) => return failure_outcome(*failure),
            Err(DriveError::ClientGone) => unreachable!("non-streaming output has no client"),
        };
        let raw = websearch::raw_websearch_usage(accounting.input_tokens, accounting.output_tokens);
        let reported = ctx.usage_mapper.as_ref().map_or(raw, |mapper| mapper(raw));
        let response = envelope::json_response_with_id(
            StatusCode::OK,
            json!({
                "id": envelope::message_id(),
                "type": "message",
                "role": "assistant",
                "model": ctx.payload.model,
                "content": output.blocks,
                "stop_reason": result.stop_reason,
                "stop_sequence": null,
                "usage": websearch::usage_json(reported, reported.output_tokens, Some(result.searches))
            }),
            &ctx.request_id,
            None,
        );
        log_completion(&ctx, &accounting, result.searches);
        return websearch::WebSearchOutcome::Success {
            response,
            raw_input_tokens: accounting.input_tokens,
            output_tokens: accounting.output_tokens,
            reported_usage: reported,
            attribution: accounting.attribution(&ctx.provider),
            web_search_requests: result.searches,
        };
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(32);
    let completion: websearch::WebSearchStreamCompletionSlot = Arc::new(Mutex::new(None));
    let attribution = accounting.attribution(&ctx.provider);
    let request_id = ctx.request_id.clone();
    let keepalive = Duration::from_secs(normalize_stream_keepalive_interval_secs(
        ctx.runtime_config.stream_keepalive_interval_secs,
    ));
    tokio::spawn(stream_loop(
        ctx,
        request,
        first,
        accounting,
        StreamWriter {
            tx,
            next_index: 0,
            keepalive,
        },
        completion.clone(),
        max_searches,
    ));
    let body = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|bytes| (Ok::<_, std::convert::Infallible>(bytes), rx))
    });
    websearch::WebSearchOutcome::StreamingSuccess {
        response: envelope::sse_builder_with_id(&request_id)
            .body(Body::from_stream(body))
            .unwrap(),
        completion,
        attribution,
    }
}

fn failure_outcome(failure: CallFailure) -> websearch::WebSearchOutcome {
    websearch::WebSearchOutcome::Failure {
        response: failure.response,
        error_type: failure.error_type,
        internal_reason: failure.internal_reason,
        attribution: failure.attribution,
        tool_error: None,
    }
}

fn log_completion(ctx: &ModelDrivenWebSearchContext, accounting: &Accounting, searches: u32) {
    tracing::info!(
        request_id = %ctx.request_id,
        searches,
        stream = ctx.payload.stream,
        input_tokens = accounting.input_tokens,
        output_tokens = accounting.output_tokens,
        "model-driven WebSearch completed"
    );
}

/// The streaming half: `message_start` goes out at once, then every block as soon as it is
/// complete, with `ping` keepalives while searches and model calls run.
async fn stream_loop(
    ctx: ModelDrivenWebSearchContext,
    request: MessagesRequest,
    first: RoundOutput,
    mut accounting: Accounting,
    writer: StreamWriter,
    completion: websearch::WebSearchStreamCompletionSlot,
    max_searches: u32,
) {
    let mut output = BlockOutput {
        blocks: Vec::new(),
        writer: Some(writer),
    };
    // The final usage is only known when the loop ends; message_start carries the first call's
    // input and no cache fields, and the final usage in message_delta replaces it.
    let start_usage = websearch::raw_websearch_usage(accounting.input_tokens, 0);
    let start = message_start_event(&ctx.payload.model, start_usage);
    if !output
        .writer
        .as_mut()
        .expect("streaming writer")
        .send(start)
        .await
    {
        return;
    }
    let result = drive(
        &ctx,
        request,
        first,
        &mut accounting,
        &mut output,
        max_searches,
    )
    .await;
    let raw = websearch::raw_websearch_usage(accounting.input_tokens, accounting.output_tokens);
    let writer = output.writer.as_mut().expect("streaming writer");
    match result {
        Ok(result) => {
            let reported = ctx.usage_mapper.as_ref().map_or(raw, |mapper| mapper(raw));
            *completion.lock() = Some(websearch::WebSearchStreamCompletion {
                raw_input_tokens: accounting.input_tokens,
                output_tokens: accounting.output_tokens,
                reported_usage: reported,
                attribution: accounting.attribution(&ctx.provider),
                web_search_requests: result.searches,
                stop_reason: result.stop_reason,
                failure: None,
            });
            log_completion(&ctx, &accounting, result.searches);
            for event in final_events(result.stop_reason, reported, result.searches) {
                if !writer.send(event).await {
                    return;
                }
            }
        }
        Err(DriveError::ClientGone) => {}
        Err(DriveError::Model(failure)) => {
            // Headers are already sent, so the failure ends the stream with an error event,
            // the shape the official API uses for mid-stream errors.
            *completion.lock() = Some(websearch::WebSearchStreamCompletion {
                raw_input_tokens: accounting.input_tokens,
                output_tokens: accounting.output_tokens,
                reported_usage: raw,
                attribution: failure.attribution,
                web_search_requests: 0,
                stop_reason: "error",
                failure: Some((failure.error_type, failure.internal_reason)),
            });
            let message = match failure.error_type {
                "rate_limit_error" => envelope::PUBLIC_RATE_LIMIT_MESSAGE,
                "invalid_request_error" => envelope::PUBLIC_INVALID_REQUEST_MESSAGE,
                _ => envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
            };
            let event = SseEvent::new(
                "error",
                json!({
                    "type": "error",
                    "error": {
                        "type": failure.error_type,
                        "message": envelope::public_message_with_error_id(message, &ctx.error_id)
                    }
                }),
            );
            let _ = writer.send(event).await;
        }
    }
}

/// The search loop shared by both response modes. The first round is already done.
async fn drive(
    ctx: &ModelDrivenWebSearchContext,
    mut request: MessagesRequest,
    first: RoundOutput,
    accounting: &mut Accounting,
    output: &mut BlockOutput,
    max_searches: u32,
) -> Result<DriveResult, DriveError> {
    let mut sources: Vec<Source> = Vec::new();
    let mut searches = 0_u32;
    let mut final_text = String::new();
    let mut client_tool_calls: Vec<(String, Value)> = Vec::new();
    let mut pending = Some(first);
    let mut rounds = 0_u32;

    loop {
        let round = match pending.take() {
            Some(round) => round,
            None => with_keepalive(output, call_model(ctx, &request, accounting))
                .await
                .ok_or(DriveError::ClientGone)?
                .map_err(|failure| DriveError::Model(Box::new(failure)))?,
        };
        rounds += 1;
        let searches_allowed = request
            .tool_choice
            .as_ref()
            .and_then(|choice| choice.get("type"))
            .and_then(Value::as_str)
            != Some("none");
        let (search_calls, other_calls): (Vec<_>, Vec<_>) = round
            .tool_calls
            .into_iter()
            .partition(|(name, _)| name == SEARCH_TOOL_NAME);
        let search_calls: Vec<String> = if searches_allowed {
            search_calls
                .into_iter()
                .filter_map(|(_, input)| {
                    input
                        .get("query")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|query| !query.is_empty())
                        .map(str::to_string)
                })
                .collect()
        } else {
            Vec::new()
        };

        if search_calls.is_empty() {
            final_text = round.text;
            client_tool_calls = other_calls;
            break;
        }

        let mut assistant_content = Vec::new();
        let preamble = clean_preamble(ctx, &round.text);
        if !preamble.is_empty() {
            let block = json!({"type": "text", "text": preamble});
            assistant_content.push(block.clone());
            if !output.push(block).await {
                return Err(DriveError::ClientGone);
            }
        }
        let mut tool_results = Vec::new();
        for query in search_calls {
            let server_id = envelope::server_tool_use_id();
            let internal_id = envelope::tool_use_id();
            let server_tool_use = json!({
                "type": "server_tool_use",
                "id": server_id,
                "name": SEARCH_TOOL_NAME,
                "input": {"query": query}
            });
            if !output.push(server_tool_use).await {
                return Err(DriveError::ClientGone);
            }
            assistant_content.push(json!({
                "type": "tool_use",
                "id": internal_id,
                "name": SEARCH_TOOL_NAME,
                "input": {"query": query}
            }));
            let (result_content, result_text) = if searches >= max_searches {
                (
                    websearch::web_search_error_block("max_uses_exceeded"),
                    "The search limit for this request was reached. Answer from the results you already have.".to_string(),
                )
            } else {
                searches += 1;
                let search = websearch::execute_search(
                    &ctx.provider,
                    &ctx.payload,
                    &query,
                    ctx.inference_attempt_budget.clone(),
                    ctx.attribution_sink.clone(),
                    &ctx.request_id,
                );
                match with_keepalive(output, search)
                    .await
                    .ok_or(DriveError::ClientGone)?
                {
                    Ok(results) => {
                        let first = sources.len() + 1;
                        let text = format_results(&query, &results, first);
                        sources.extend(results.iter().map(|result| Source {
                            title: result.title.clone(),
                            url: result.url.clone(),
                            snippet: result.snippet.clone().unwrap_or_default(),
                        }));
                        (
                            Value::Array(
                                results
                                    .iter()
                                    .map(websearch::web_search_result_block)
                                    .collect(),
                            ),
                            text,
                        )
                    }
                    Err(error_code) => (
                        websearch::web_search_error_block(error_code),
                        format!(
                            "The web search failed ({error_code}). Answer without live results and say that the search was unavailable."
                        ),
                    ),
                }
            };
            let result_block = json!({
                "type": "web_search_tool_result",
                "tool_use_id": server_id,
                "content": result_content
            });
            if !output.push(result_block).await {
                return Err(DriveError::ClientGone);
            }
            tool_results.push(json!({
                "type": "tool_result",
                "tool_use_id": internal_id,
                "content": result_text
            }));
        }
        request.messages.push(Message {
            role: "assistant".to_string(),
            content: Value::Array(assistant_content),
        });
        request.messages.push(Message {
            role: "user".to_string(),
            content: Value::Array(tool_results),
        });
        if rounds > max_searches {
            break;
        }
        // Later rounds may search again unless the budget is spent; then the model must answer.
        request.tool_choice = Some(if searches >= max_searches {
            json!({"type": "none"})
        } else {
            json!({"type": "auto"})
        });
    }

    // An answer with no text (the model kept calling tools after the limit, or returned
    // nothing) gets one answer-only retry; an empty text block is never sent, since the
    // official API rejects it when the history is sent back.
    if final_text.trim().is_empty() && client_tool_calls.is_empty() {
        request.tool_choice = Some(json!({"type": "none"}));
        let retry = with_keepalive(output, call_model(ctx, &request, accounting))
            .await
            .ok_or(DriveError::ClientGone)?
            .map_err(|failure| DriveError::Model(Box::new(failure)))?;
        final_text = retry.text;
    }

    let mut answer = cited_text_blocks(&final_text, &sources);
    clean_text_blocks(ctx, &mut answer);
    for block in answer {
        if !output.push(block).await {
            return Err(DriveError::ClientGone);
        }
    }
    let stop_reason = if client_tool_calls.is_empty() {
        "end_turn"
    } else {
        for (name, input) in client_tool_calls {
            let block = json!({
                "type": "tool_use",
                "id": envelope::tool_use_id(),
                "name": name,
                "input": input
            });
            if !output.push(block).await {
                return Err(DriveError::ClientGone);
            }
        }
        "tool_use"
    };
    Ok(DriveResult {
        searches,
        stop_reason,
    })
}

/// Text written before a search call, without echoed control prompts or identity leaks.
fn clean_preamble(ctx: &ModelDrivenWebSearchContext, text: &str) -> String {
    let mut text = text.to_string();
    if !super::super::control_block_cleaner::request_mentions_control_tags(&ctx.payload) {
        text = super::super::control_block_cleaner::strip_control_blocks(&text).into_owned();
    }
    if ctx.identity_output_guard
        && let Some(cleaned) = super::super::identity_guard::scrub_text(&text)
    {
        text = cleaned;
    }
    text.trim().to_string()
}

/// Removes echoed control prompts and identity leaks, then drops text blocks left empty.
fn clean_text_blocks(ctx: &ModelDrivenWebSearchContext, blocks: &mut Vec<Value>) {
    if !super::super::control_block_cleaner::request_mentions_control_tags(&ctx.payload) {
        super::super::control_block_cleaner::strip_control_blocks_in_content(blocks);
    }
    if ctx.identity_output_guard {
        super::super::identity_guard::scrub_content(blocks, IDENTITY_GUARD_FALLBACK);
    }
    blocks.retain(|block| {
        block["type"] != "text"
            || block["text"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
    });
}

/// The request sent to the model: the native tool becomes an ordinary `web_search` tool, the
/// citation guidance (injection item `webSearchGuidance`, when on) is appended to the system
/// prompt, and thinking is off for the inner calls.
pub(super) fn internal_request(
    payload: &MessagesRequest,
    guidance: Option<&str>,
) -> MessagesRequest {
    let mut request = payload.clone();
    request.stream = false;
    request.thinking = None;
    let mut tools: Vec<Tool> = request
        .tools
        .take()
        .unwrap_or_default()
        .into_iter()
        .filter(|tool| tool.name != SEARCH_TOOL_NAME)
        .collect();
    tools.push(Tool {
        tool_type: None,
        name: SEARCH_TOOL_NAME.to_string(),
        description: SEARCH_TOOL_DESCRIPTION.to_string(),
        input_schema: HashMap::from([
            ("type".to_string(), json!("object")),
            (
                "properties".to_string(),
                json!({"query": {"type": "string", "description": "The search query."}}),
            ),
            ("required".to_string(), json!(["query"])),
        ]),
        max_uses: None,
        allowed_domains: None,
        blocked_domains: None,
        cache_control: None,
    });
    request.tools = Some(tools);
    if let Some(guidance) = guidance.map(str::trim).filter(|text| !text.is_empty()) {
        request
            .system
            .get_or_insert_with(Vec::new)
            .push(SystemMessage {
                text: guidance.to_string(),
                cache_control: None,
            });
    }
    request
}

#[allow(clippy::result_large_err)]
async fn call_model(
    ctx: &ModelDrivenWebSearchContext,
    request: &MessagesRequest,
    accounting: &mut Accounting,
) -> Result<RoundOutput, CallFailure> {
    let native_reasoning_capability = ctx
        .model_resolution
        .upstream_model
        .as_deref()
        .map(|model| {
            ctx.state
                .model_capabilities
                .reasoning_capability_state_for(model, &ctx.provider.model_capability_cohort_keys())
        })
        .unwrap_or(KiroReasoningCapabilityState::Unknown);
    let prepared = local_body_pipeline::prepare(
        &ctx.endpoint,
        request,
        &ctx.runtime_config,
        &ctx.cache_route,
        &ctx.model_resolution,
        native_reasoning_capability,
    )
    .map_err(|error| CallFailure {
        response: error.response,
        error_type: "invalid_request_error",
        internal_reason: "websearch_model_request_invalid",
        attribution: accounting.attribution(&ctx.provider),
    })?;
    let upstream_model = ctx
        .model_resolution
        .upstream_model
        .as_deref()
        .unwrap_or(&request.model);
    let api_response = call_api_maybe_fail_fast(
        &ctx.provider,
        &prepared.request_body,
        Some(&prepared.kiro_request),
        Some(ctx.request_id.as_str()),
        None,
        capacity_weight_units_for_local_request(ctx.provider.as_ref(), prepared.input_tokens),
        Some(upstream_model),
        ctx.inference_attempt_budget.clone(),
    )
    .await
    .map_err(|error| {
        let attempts = KiroProvider::diagnostic_attempts_from_error(&error);
        accounting.attempts =
            merge_credential_attempts(std::mem::take(&mut accounting.attempts), attempts);
        let status_response = map_provider_error(
            error,
            Some(ctx.request_id.as_str()),
            Some(ctx.error_id.as_str()),
            Some(ctx.provider.as_ref()),
            None,
        );
        CallFailure {
            error_type: error_type_for_status(status_response.status()),
            response: status_response,
            internal_reason: "websearch_model_call_failed",
            attribution: accounting.attribution(&ctx.provider),
        }
    })?;
    accounting.credential_id = Some(api_response.credential_id());
    accounting.attempts = merge_credential_attempts(
        std::mem::take(&mut accounting.attempts),
        api_response.attempts().to_vec(),
    );
    let (response, completion) = api_response.into_parts();
    let content_type = response
        .headers()
        .get(REQWEST_CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let runtime = ctx.provider.runtime_config();
    let processing_failure = |detail: String| {
        tracing::warn!(
            request_id = %ctx.request_id,
            detail,
            "model-driven WebSearch model call failed"
        );
        processing_failure_for(
            ctx,
            accounting.attribution(&ctx.provider),
            "websearch_model_call_failed",
        )
    };
    let body = match read_non_stream_body_with_first_output_deadline(
        response,
        runtime.kiro_upstream_response_timeout_secs,
        LOCAL_NON_STREAM_RESPONSE_MAX_BYTES,
        runtime
            .stream_first_output_timeout_secs
            .min(crate::model::config::STREAM_FIRST_OUTPUT_TIMEOUT_MAX_SECS),
    )
    .await
    {
        Ok(body) => body,
        Err(error) => {
            completion.release();
            return Err(processing_failure(error.to_string()));
        }
    };
    let body = match inspect_complete_upstream_body(content_type.as_deref(), body) {
        Ok(body) => body,
        Err(error) => {
            completion.release();
            return Err(processing_failure(error.internal_detail));
        }
    };
    let events = match decode_complete_eventstream(&body) {
        Ok(events) => events,
        Err(detail) => {
            completion.release();
            return Err(processing_failure(detail));
        }
    };

    let mut output = RoundOutput::default();
    let mut tool_buffers: HashMap<String, (String, String)> = HashMap::new();
    let mut metadata = crate::kiro::model::events::MetadataTokenUsage::default();
    for event in events {
        match event {
            Event::AssistantResponse(response) => output.text.push_str(&response.content),
            Event::ToolUse(tool_use) => {
                let entry = tool_buffers
                    .entry(tool_use.tool_use_id.clone())
                    .or_insert_with(|| (tool_use.name.clone(), String::new()));
                entry.1.push_str(&tool_use.input);
                if tool_use.stop {
                    let (name, input) = tool_buffers
                        .remove(&tool_use.tool_use_id)
                        .unwrap_or_default();
                    let resolved = super::super::tool_name_restore::resolve_response_tool_name(
                        &prepared.tool_name_map,
                        &prepared.known_tool_names,
                        &name,
                    );
                    let input = serde_json::from_str(&input).unwrap_or_else(|_| json!({}));
                    let input = prepared
                        .tool_schema_key_map
                        .reverse_tool_input(&resolved.upstream, input);
                    output.tool_calls.push((resolved.original, input));
                }
            }
            Event::Metadata(event) => {
                if let Some(usage) = event.token_usage.as_ref() {
                    metadata.merge_positive_from(usage);
                }
            }
            Event::MessageMetadata(event) => {
                if let Some(usage) = event.token_usage.as_ref() {
                    metadata.merge_positive_from(usage);
                }
            }
            Event::Error { error_code, .. } => {
                completion.release();
                return Err(processing_failure(format!(
                    "upstream error event: {error_code}"
                )));
            }
            Event::Exception { exception_type, .. } => {
                completion.release();
                return Err(processing_failure(format!(
                    "upstream exception event: {exception_type}"
                )));
            }
            _ => {}
        }
    }
    completion.report_success();

    let metadata_input = metadata
        .uncached_input_tokens
        .saturating_add(metadata.cache_read_input_tokens)
        .saturating_add(metadata.cache_write_input_tokens);
    output.input_tokens = if metadata_input > 0 {
        metadata_input
    } else {
        prepared.input_tokens
    };
    output.output_tokens = if metadata.output_tokens > 0 {
        metadata.output_tokens
    } else {
        let mut counted = output.text.clone();
        for (name, input) in &output.tool_calls {
            counted.push_str(name);
            counted.push_str(&input.to_string());
        }
        (token::count_tokens(&counted) as i32).max(1)
    };
    accounting.input_tokens = accounting.input_tokens.saturating_add(output.input_tokens);
    accounting.output_tokens = accounting
        .output_tokens
        .saturating_add(output.output_tokens);
    Ok(output)
}

fn error_type_for_status(status: StatusCode) -> &'static str {
    match status {
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        StatusCode::BAD_REQUEST => "invalid_request_error",
        _ => "api_error",
    }
}

/// Tool result text handed to the model; sources are numbered across the whole request.
fn format_results(query: &str, results: &[websearch::WebSearchResult], first: usize) -> String {
    if results.is_empty() {
        return format!("No results found for \"{query}\".");
    }
    let mut text = format!("Search results for \"{query}\" ({CITATION_HINT}):\n");
    for (offset, result) in results.iter().enumerate() {
        text.push_str(&format!(
            "\n[{}] {}\nURL: {}\n",
            first + offset,
            result.title,
            result.url
        ));
        if let Some(age) = websearch::result_page_age(result) {
            text.push_str(&format!("Published: {age}\n"));
        }
        if let Some(snippet) = result.snippet.as_deref().filter(|s| !s.trim().is_empty()) {
            text.push_str(&truncate_chars(snippet.trim(), SNIPPET_MAX_CHARS));
            text.push('\n');
        }
    }
    text
}

fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((index, _)) => format!("{}...", &text[..index]),
        None => text.to_string(),
    }
}

fn citation(source: &Source, number: usize, cited_span: &str) -> Value {
    json!({
        "type": "web_search_result_location",
        "url": source.url,
        "title": source.title,
        // Proxy-issued opaque index (the official value is Anthropic-encrypted).
        "encrypted_index": BASE64_STANDARD.encode(format!("{number}:{}", source.url)),
        // At most 150 characters, like the official field.
        "cited_text": cited_text(&source.snippet, cited_span)
    })
}

/// The snippet sentence that overlaps most with the cited span (at most 150 characters);
/// the start of the snippet when nothing overlaps.
fn cited_text(snippet: &str, cited_span: &str) -> String {
    let snippet = snippet.trim();
    let span_tokens = overlap_tokens(cited_span);
    let best = split_sentences(snippet)
        .into_iter()
        .map(|sentence| {
            let tokens = overlap_tokens(sentence);
            (span_tokens.intersection(&tokens).count(), sentence)
        })
        .filter(|(shared, _)| *shared > 0)
        .max_by_key(|(shared, _)| *shared)
        .map_or(snippet, |(_, sentence)| sentence);
    best.trim().chars().take(CITED_TEXT_MAX_CHARS).collect()
}

fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        // ASCII punctuation ends a sentence only before whitespace or the end, so "1.90" and
        // "e.g." inside a sentence do not split it; CJK punctuation always ends one.
        let ascii_end = matches!(ch, '.' | '!' | '?')
            && chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if ascii_end || matches!(ch, '。' | '！' | '？' | '\n') {
            let end = index + ch.len_utf8();
            if !text[start..end].trim().is_empty() {
                sentences.push(&text[start..end]);
            }
            start = end;
        }
    }
    if !text[start..].trim().is_empty() {
        sentences.push(&text[start..]);
    }
    sentences
}

/// Tokens for overlap matching: lowercase words of 3+ characters, and character bigrams for
/// CJK text (which has no spaces).
fn overlap_tokens(text: &str) -> std::collections::HashSet<String> {
    let mut tokens = std::collections::HashSet::new();
    let mut word = String::new();
    let mut previous_cjk: Option<char> = None;
    let flush = |word: &mut String, tokens: &mut std::collections::HashSet<String>| {
        if word.chars().count() >= 3 {
            tokens.insert(std::mem::take(word));
        } else {
            word.clear();
        }
    };
    for ch in text.chars() {
        if is_cjk(ch) {
            flush(&mut word, &mut tokens);
            if let Some(previous) = previous_cjk {
                tokens.insert(format!("{previous}{ch}"));
            }
            previous_cjk = Some(ch);
        } else if ch.is_alphanumeric() {
            previous_cjk = None;
            word.extend(ch.to_lowercase());
        } else {
            previous_cjk = None;
            flush(&mut word, &mut tokens);
        }
    }
    flush(&mut word, &mut tokens);
    tokens
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af)
}

/// Citation markers the model writes: `[1]`, `[1][2]`, `[1, 2]`, `[1-3]`, `[^1]`, `【1】`, `［1］`.
static MARKER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?:\s*(?:\[\^?|【|［)\d{1,3}(?:\s*[,，、\-–~]\s*\^?\d{1,3})*(?:\]|】|］))+")
        .unwrap()
});

/// Numbers in one marker run, ranges expanded, unknown numbers dropped, in first-seen order.
fn marker_numbers(marker: &str, sources: usize) -> Vec<usize> {
    static ITEM: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(\d{1,3})(?:\s*[\-–~]\s*\^?(\d{1,3}))?").unwrap()
    });
    let mut numbers = Vec::new();
    for item in ITEM.captures_iter(marker) {
        let Some(start) = item[1].parse::<usize>().ok() else {
            continue;
        };
        let end = item
            .get(2)
            .and_then(|end| end.as_str().parse::<usize>().ok())
            .filter(|end| *end >= start && *end - start < 20)
            .unwrap_or(start);
        for number in start..=end {
            if (1..=sources).contains(&number) && !numbers.contains(&number) {
                numbers.push(number);
            }
        }
    }
    numbers
}

/// Splits the answer at citation markers into text blocks: a span that ends with markers carries
/// the cited sources; markers are removed from the text. Unknown numbers are dropped. When the
/// model wrote no usable marker, paragraphs that clearly reuse a result's wording are cited.
fn cited_text_blocks(text: &str, sources: &[Source]) -> Vec<Value> {
    const TRAILING_PUNCTUATION: &[char] = &[
        '.', ',', ';', ':', '!', '?', '。', '，', '；', '：', '！', '？', '、',
    ];

    let text = text.trim();
    let mut blocks = Vec::new();
    let mut cursor = 0;
    for found in MARKER.find_iter(text) {
        let numbers = marker_numbers(found.as_str(), sources.len());
        let mut span_end = found.start();
        let mut next = found.end();
        // "sentence [1]." keeps the period with the cited sentence.
        let rest = &text[next..];
        let punctuation: usize = rest
            .chars()
            .take_while(|ch| TRAILING_PUNCTUATION.contains(ch))
            .map(char::len_utf8)
            .sum();
        let span = if punctuation > 0 {
            next += punctuation;
            format!("{}{}", &text[cursor..span_end], &rest[..punctuation])
        } else {
            span_end = span_end.max(cursor);
            text[cursor..span_end].to_string()
        };
        cursor = next;
        if span.trim().is_empty() {
            continue;
        }
        if numbers.is_empty() {
            push_plain(&mut blocks, &span);
        } else {
            blocks.push(cited_block(&span, &numbers, sources));
        }
    }
    if cursor < text.len() {
        push_plain(&mut blocks, &text[cursor..]);
    }
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    if !sources.is_empty() && !blocks.iter().any(|block| block.get("citations").is_some()) {
        return overlap_cited_blocks(text, sources).unwrap_or(blocks);
    }
    blocks
}

fn cited_block(span: &str, numbers: &[usize], sources: &[Source]) -> Value {
    json!({
        "type": "text",
        "text": span,
        "citations": numbers
            .iter()
            .map(|n| citation(&sources[n - 1], *n, span))
            .collect::<Vec<_>>()
    })
}

/// Fallback for an answer without markers: each paragraph is cited with the result whose
/// title and snippet share the most wording with it, when the overlap is clear. `None` when
/// no paragraph qualifies.
fn overlap_cited_blocks(text: &str, sources: &[Source]) -> Option<Vec<Value>> {
    const MIN_SHARED: usize = 3;
    const MIN_RATIO: f64 = 0.25;
    let source_tokens: Vec<_> = sources
        .iter()
        .map(|source| overlap_tokens(&format!("{} {}", source.title, source.snippet)))
        .collect();
    let mut blocks = Vec::new();
    let mut cited_any = false;
    for paragraph in text.split_inclusive("\n\n") {
        let tokens = overlap_tokens(paragraph);
        let best = source_tokens
            .iter()
            .enumerate()
            .map(|(index, candidate)| (tokens.intersection(candidate).count(), index))
            .max_by_key(|(shared, index)| (*shared, std::cmp::Reverse(*index)));
        match best {
            Some((shared, index))
                if shared >= MIN_SHARED && shared as f64 >= tokens.len() as f64 * MIN_RATIO =>
            {
                cited_any = true;
                blocks.push(cited_block(paragraph, &[index + 1], sources));
            }
            _ => push_plain(&mut blocks, paragraph),
        }
    }
    cited_any.then_some(blocks)
}

fn push_plain(blocks: &mut Vec<Value>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = blocks
        .last_mut()
        .filter(|block| block.get("citations").is_none())
    {
        let joined = format!("{}{}", last["text"].as_str().unwrap_or_default(), text);
        last["text"] = json!(joined);
        return;
    }
    blocks.push(json!({"type": "text", "text": text}));
}

fn message_start_event(model: &str, usage: crate::anthropic::cache::CacheUsage) -> SseEvent {
    SseEvent::new(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": envelope::message_id(),
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": websearch::usage_json(usage, 0, None)
            }
        }),
    )
}

/// `content_block_start` / deltas / `content_block_stop` for one complete block.
fn block_events(index: usize, block: &Value) -> Vec<SseEvent> {
    let delta = |delta: Value| {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": delta}),
        )
    };
    let kind = block["type"].as_str().unwrap_or_default();
    let start_block = match kind {
        "text" => json!({"type": "text", "text": ""}),
        "server_tool_use" | "tool_use" => json!({
            "type": kind,
            "id": block["id"],
            "name": block["name"],
            "input": {}
        }),
        _ => block.clone(),
    };
    let mut events = vec![SseEvent::new(
        "content_block_start",
        json!({"type": "content_block_start", "index": index, "content_block": start_block}),
    )];
    match kind {
        "text" => {
            for citation in block["citations"].as_array().into_iter().flatten() {
                events.push(delta(
                    json!({"type": "citations_delta", "citation": citation}),
                ));
            }
            let text = block["text"].as_str().unwrap_or_default();
            let mut chars = text.chars();
            loop {
                let chunk: String = chars.by_ref().take(100).collect();
                if chunk.is_empty() {
                    break;
                }
                events.push(delta(json!({"type": "text_delta", "text": chunk})));
            }
        }
        "server_tool_use" | "tool_use" => {
            events.push(delta(
                json!({"type": "input_json_delta", "partial_json": block["input"].to_string()}),
            ));
        }
        _ => {}
    }
    events.push(SseEvent::new(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": index}),
    ));
    events
}

fn final_events(
    stop_reason: &str,
    usage: crate::anthropic::cache::CacheUsage,
    searches: u32,
) -> Vec<SseEvent> {
    vec![
        SseEvent::new(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": websearch::usage_json(usage, usage.output_tokens, Some(searches))
            }),
        ),
        SseEvent::new("message_stop", json!({"type": "message_stop"})),
    ]
}

#[cfg(test)]
fn stream_events(
    model: &str,
    blocks: &[Value],
    stop_reason: &str,
    usage: crate::anthropic::cache::CacheUsage,
    searches: u32,
) -> Vec<SseEvent> {
    let mut events = vec![message_start_event(model, usage)];
    for (index, block) in blocks.iter().enumerate() {
        events.extend(block_events(index, block));
    }
    events.extend(final_events(stop_reason, usage, searches));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<Source> {
        vec![
            Source {
                title: "Rust 1.90".to_string(),
                url: "https://blog.rust-lang.org/1.90".to_string(),
                snippet: "Rust 1.90 was released on September 18.".to_string(),
            },
            Source {
                title: "Releases".to_string(),
                url: "https://releases.rs".to_string(),
                snippet: "Release calendar".to_string(),
            },
        ]
    }

    #[test]
    fn markers_become_citations_and_are_removed_from_text() {
        let blocks = cited_text_blocks(
            "The latest stable Rust is 1.90 [1]. It shipped in September [1][2]. More soon.",
            &sources(),
        );
        let texts: Vec<&str> = blocks.iter().map(|b| b["text"].as_str().unwrap()).collect();
        assert_eq!(
            texts,
            [
                "The latest stable Rust is 1.90.",
                " It shipped in September.",
                " More soon."
            ]
        );
        assert_eq!(
            blocks[0]["citations"][0]["url"],
            "https://blog.rust-lang.org/1.90"
        );
        assert_eq!(
            blocks[0]["citations"][0]["type"],
            "web_search_result_location"
        );
        assert_eq!(blocks[1]["citations"].as_array().unwrap().len(), 2);
        assert!(blocks[2].get("citations").is_none());
        assert!(!texts.concat().contains('['));
    }

    #[test]
    fn cited_text_is_at_most_150_characters() {
        let source = Source {
            title: "t".to_string(),
            url: "https://example.com".to_string(),
            snippet: "字".repeat(400),
        };
        let cited = citation(&source, 1, "")["cited_text"]
            .as_str()
            .unwrap()
            .chars()
            .count();
        assert_eq!(cited, CITED_TEXT_MAX_CHARS);
    }

    #[test]
    fn unknown_markers_are_dropped_without_citations() {
        let blocks = cited_text_blocks("Claim [7] and more.", &sources());
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["text"], "Claim and more.");
        assert!(blocks[0].get("citations").is_none());
    }

    #[test]
    fn text_without_markers_is_one_block() {
        let blocks = cited_text_blocks("No sources were needed.", &[]);
        assert_eq!(
            blocks,
            vec![json!({"type": "text", "text": "No sources were needed."})]
        );
    }

    #[test]
    fn internal_request_turns_the_native_tool_into_an_ordinary_tool() {
        let payload: MessagesRequest = serde_json::from_value(json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 100,
            "stream": true,
            "thinking": {"type": "enabled", "budget_tokens": 2048},
            "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 2}],
            "messages": [{"role": "user", "content": "Search the web: latest Rust"}]
        }))
        .unwrap();
        let request = internal_request(
            &payload,
            Some(crate::model::prompt_injection::catalog::DEFAULT_WEB_SEARCH_GUIDANCE),
        );
        assert!(!request.stream);
        assert!(request.thinking.is_none());
        let tools = request.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].tool_type.is_none());
        assert_eq!(tools[0].input_schema["required"], json!(["query"]));
        assert!(
            request
                .system
                .unwrap()
                .last()
                .unwrap()
                .text
                .contains("web_search_guidance")
        );
        assert!(internal_request(&payload, None).system.is_none());
    }

    #[test]
    fn stream_events_follow_the_official_block_lifecycle() {
        let usage = websearch::raw_websearch_usage(1200, 80);
        let blocks = vec![
            json!({"type": "server_tool_use", "id": "srvtoolu_01a", "name": "web_search", "input": {"query": "rust"}}),
            json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01a", "content": []}),
            json!({"type": "text", "text": "Rust 1.90.", "citations": [citation(&sources()[0], 1, "Rust 1.90.")]}),
        ];
        let events = stream_events("claude-sonnet-4-5", &blocks, "end_turn", usage, 1);
        let kinds: Vec<String> = events
            .iter()
            .map(|e| match e.data["delta"]["type"].as_str() {
                Some(delta) => format!("{}:{delta}", e.event),
                None => e.event.clone(),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "message_start",
                "content_block_start",
                "content_block_delta:input_json_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_stop",
                "content_block_start",
                "content_block_delta:citations_delta",
                "content_block_delta:text_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert_eq!(events[1].data["content_block"]["input"], json!({}));
        assert!(
            events[0].data["message"]["usage"]
                .get("server_tool_use")
                .is_none()
        );
        let final_usage = &events[10].data["usage"];
        assert_eq!(final_usage["input_tokens"], 1200);
        assert_eq!(final_usage["server_tool_use"]["web_search_requests"], 1);
    }

    #[test]
    fn marker_variants_are_parsed_and_removed() {
        let cases = [
            ("Rust 1.90 shipped【1】。", vec![1]),
            ("Rust 1.90 shipped [1, 2].", vec![1, 2]),
            ("Rust 1.90 shipped [1-2].", vec![1, 2]),
            ("Rust 1.90 shipped[^1].", vec![1]),
            ("Rust 1.90 shipped［2］.", vec![2]),
        ];
        for (text, expected) in cases {
            let blocks = cited_text_blocks(text, &sources());
            let urls: Vec<String> = blocks[0]["citations"]
                .as_array()
                .unwrap_or_else(|| panic!("{text}: no citations"))
                .iter()
                .map(|c| c["url"].as_str().unwrap().to_string())
                .collect();
            let all = sources();
            let want: Vec<String> = expected.iter().map(|n| all[n - 1].url.clone()).collect();
            assert_eq!(urls, want, "{text}");
            let joined: String = blocks.iter().map(|b| b["text"].as_str().unwrap()).collect();
            assert!(!joined.contains(['[', '【', '［']), "{text}: {joined}");
        }
    }

    #[test]
    fn answer_without_markers_is_cited_by_wording_overlap() {
        let blocks = cited_text_blocks(
            "Rust 1.90 was released on September 18 with new lints.\n\nThanks for asking.",
            &sources(),
        );
        assert_eq!(
            blocks[0]["citations"][0]["url"],
            "https://blog.rust-lang.org/1.90"
        );
        assert!(blocks.last().unwrap().get("citations").is_none());
    }

    #[test]
    fn unrelated_answer_without_markers_gets_no_citations() {
        let blocks = cited_text_blocks("The weather is nice today.", &sources());
        assert!(blocks.iter().all(|b| b.get("citations").is_none()));
    }

    #[test]
    fn cited_text_picks_the_most_relevant_snippet_sentence() {
        let source = Source {
            title: "t".to_string(),
            url: "https://example.com".to_string(),
            snippet: "Intro about the site. Rust 1.90 was released on September 18. Other news."
                .to_string(),
        };
        assert_eq!(
            cited_text(&source.snippet, "Rust 1.90 was released in September"),
            "Rust 1.90 was released on September 18."
        );
    }
}
