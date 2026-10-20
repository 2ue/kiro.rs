//! Model-driven native WebSearch (route switch `bodyConversion.modelDrivenWebSearch`).
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
const GUIDANCE: &str = "<web_search_guidance>You can search the web with the `web_search` tool. Call it with a concise query when the answer needs current or external information; you may search again with a refined query. After the results arrive, answer the user from them. When a sentence uses information from a search result, put that result's number in square brackets right after the sentence, for example [1] or [1][3]. Only cite numbers that appear in the results.</web_search_guidance>";

pub(super) struct ModelDrivenWebSearchContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) provider: Arc<KiroProvider>,
    pub(super) payload: &'a MessagesRequest,
    pub(super) endpoint: &'a str,
    pub(super) runtime_config: &'a RequestRuntimeConfig,
    pub(super) cache_route: ResolvedCacheRoutePolicy,
    pub(super) model_resolution: &'a ModelResolution,
    pub(super) usage_mapper: Option<websearch::WebSearchUsageMapper<'a>>,
    pub(super) inference_attempt_budget: Arc<InferenceAttemptBudget>,
    pub(super) attribution_sink: Arc<McpCallAttributionSink>,
    pub(super) identity_output_guard: bool,
    pub(super) request_id: &'a str,
    pub(super) error_id: &'a str,
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
    attribution: McpCallAttribution,
}

#[derive(Default)]
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

pub(super) async fn run(ctx: ModelDrivenWebSearchContext<'_>) -> websearch::WebSearchOutcome {
    let max_searches = websearch::native_web_search_max_uses(ctx.payload)
        .map_or(DEFAULT_MAX_SEARCHES, |uses| uses.max(1) as u32)
        .min(MAX_SEARCHES_CEILING);
    // Each search costs one MCP send and one more model call.
    ctx.inference_attempt_budget.raise_max_attempts(
        ctx.inference_attempt_budget
            .max_attempts()
            .saturating_add(max_searches.saturating_mul(2)),
    );

    let mut request = internal_request(ctx.payload);
    if websearch::is_claude_code_search_request(ctx.payload) {
        // The Claude Code WebSearch sub-request always expects a search.
        request.tool_choice = Some(json!({"type": "tool", "name": SEARCH_TOOL_NAME}));
    }

    let mut accounting = Accounting::default();
    let mut blocks: Vec<Value> = Vec::new();
    let mut sources: Vec<Source> = Vec::new();
    let mut searches = 0_u32;
    let mut final_text = String::new();
    let mut client_tool_calls: Vec<(String, Value)> = Vec::new();

    for _ in 0..=max_searches {
        let round = match call_model(&ctx, &request, &mut accounting).await {
            Ok(round) => round,
            Err(failure) => {
                return websearch::WebSearchOutcome::Failure {
                    response: failure.response,
                    error_type: failure.error_type,
                    internal_reason: "websearch_model_call_failed",
                    attribution: failure.attribution,
                    tool_error: None,
                };
            }
        };
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

        let preamble = round.text.trim();
        if !preamble.is_empty() {
            blocks.push(json!({"type": "text", "text": preamble}));
        }
        let mut assistant_content = Vec::new();
        if !preamble.is_empty() {
            assistant_content.push(json!({"type": "text", "text": preamble}));
        }
        let mut tool_results = Vec::new();
        for query in search_calls {
            let server_id = envelope::server_tool_use_id();
            let internal_id = envelope::tool_use_id();
            blocks.push(json!({
                "type": "server_tool_use",
                "id": server_id,
                "name": SEARCH_TOOL_NAME,
                "input": {"query": query}
            }));
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
                match websearch::execute_search(
                    &ctx.provider,
                    ctx.payload,
                    &query,
                    ctx.inference_attempt_budget.clone(),
                    ctx.attribution_sink.clone(),
                    ctx.request_id,
                )
                .await
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
            blocks.push(json!({
                "type": "web_search_tool_result",
                "tool_use_id": server_id,
                "content": result_content
            }));
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
        // Later rounds may search again unless the budget is spent; then the model must answer.
        request.tool_choice = Some(if searches >= max_searches {
            json!({"type": "none"})
        } else {
            json!({"type": "auto"})
        });
    }

    let mut answer = cited_text_blocks(&final_text, &sources);
    if !super::super::control_block_cleaner::request_mentions_control_tags(ctx.payload) {
        super::super::control_block_cleaner::strip_control_blocks_in_content(&mut answer);
    }
    if ctx.identity_output_guard {
        super::super::identity_guard::scrub_content(&mut answer, IDENTITY_GUARD_FALLBACK);
    }
    blocks.extend(answer);
    let stop_reason = if client_tool_calls.is_empty() {
        "end_turn"
    } else {
        for (name, input) in client_tool_calls {
            blocks.push(json!({
                "type": "tool_use",
                "id": envelope::tool_use_id(),
                "name": name,
                "input": input
            }));
        }
        "tool_use"
    };

    let raw = websearch::raw_websearch_usage(accounting.input_tokens, accounting.output_tokens);
    let reported = ctx.usage_mapper.map_or(raw, |mapper| mapper(raw));
    let response = if ctx.payload.stream {
        let events = stream_events(&ctx.payload.model, &blocks, stop_reason, reported, searches);
        envelope::sse_builder_with_id(ctx.request_id)
            .body(Body::from_stream(futures::stream::iter(
                events.into_iter().map(|event| {
                    Ok::<_, std::convert::Infallible>(Bytes::from(event.to_sse_string()))
                }),
            )))
            .unwrap()
    } else {
        envelope::json_response_with_id(
            StatusCode::OK,
            json!({
                "id": envelope::message_id(),
                "type": "message",
                "role": "assistant",
                "model": ctx.payload.model,
                "content": blocks,
                "stop_reason": stop_reason,
                "stop_sequence": null,
                "usage": websearch::usage_json(reported, reported.output_tokens, Some(searches))
            }),
            ctx.request_id,
            None,
        )
    };
    tracing::info!(
        request_id = ctx.request_id,
        searches,
        sources = sources.len(),
        input_tokens = accounting.input_tokens,
        output_tokens = accounting.output_tokens,
        "model-driven WebSearch completed"
    );
    websearch::WebSearchOutcome::Success {
        response,
        raw_input_tokens: accounting.input_tokens,
        output_tokens: accounting.output_tokens,
        reported_usage: reported,
        attribution: accounting.attribution(&ctx.provider),
    }
}

/// The request sent to the model: the native tool becomes an ordinary `web_search` tool, the
/// citation guidance is appended to the system prompt, and thinking is off for the inner calls.
fn internal_request(payload: &MessagesRequest) -> MessagesRequest {
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
    request
        .system
        .get_or_insert_with(Vec::new)
        .push(SystemMessage {
            text: GUIDANCE.to_string(),
            cache_control: None,
        });
    request
}

#[allow(clippy::result_large_err)]
async fn call_model(
    ctx: &ModelDrivenWebSearchContext<'_>,
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
        ctx.endpoint,
        request,
        ctx.runtime_config,
        &ctx.cache_route,
        ctx.model_resolution,
        native_reasoning_capability,
    )
    .map_err(|error| CallFailure {
        response: error.response,
        error_type: "invalid_request_error",
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
        Some(ctx.request_id),
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
            Some(ctx.request_id),
            Some(ctx.error_id),
            Some(ctx.provider.as_ref()),
            None,
        );
        CallFailure {
            error_type: error_type_for_status(status_response.status()),
            response: status_response,
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
            request_id = ctx.request_id,
            detail,
            "model-driven WebSearch model call failed"
        );
        CallFailure {
            response: envelope::error_response_with_id(
                StatusCode::BAD_GATEWAY,
                "api_error",
                envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
                ctx.request_id,
            ),
            error_type: "api_error",
            attribution: accounting.attribution(&ctx.provider),
        }
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
    let mut text = format!("Search results for \"{query}\":\n");
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

fn citation(source: &Source, number: usize) -> Value {
    json!({
        "type": "web_search_result_location",
        "url": source.url,
        "title": source.title,
        // Proxy-issued opaque index (the official value is Anthropic-encrypted).
        "encrypted_index": BASE64_STANDARD.encode(format!("{number}:{}", source.url)),
        "cited_text": truncate_chars(source.snippet.trim(), CITED_TEXT_MAX_CHARS)
    })
}

/// Splits the answer at `[n]` markers into text blocks: a span that ends with markers carries
/// the cited sources; markers are removed from the text. Unknown numbers are dropped.
fn cited_text_blocks(text: &str, sources: &[Source]) -> Vec<Value> {
    static MARKER: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?:\s*\[\d{1,3}\])+").unwrap());
    static NUMBER: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\d{1,3}").unwrap());
    const TRAILING_PUNCTUATION: &[char] = &[
        '.', ',', ';', ':', '!', '?', '。', '，', '；', '：', '！', '？', '、',
    ];

    let text = text.trim();
    let mut blocks = Vec::new();
    let mut cursor = 0;
    for found in MARKER.find_iter(text) {
        let numbers: Vec<usize> = NUMBER
            .find_iter(found.as_str())
            .filter_map(|n| n.as_str().parse::<usize>().ok())
            .filter(|n| (1..=sources.len()).contains(n))
            .fold(Vec::new(), |mut acc, n| {
                if !acc.contains(&n) {
                    acc.push(n);
                }
                acc
            });
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
            blocks.push(json!({
                "type": "text",
                "text": span,
                "citations": numbers
                    .iter()
                    .map(|n| citation(&sources[n - 1], *n))
                    .collect::<Vec<_>>()
            }));
        }
    }
    if cursor < text.len() {
        push_plain(&mut blocks, &text[cursor..]);
    }
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    blocks
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

fn stream_events(
    model: &str,
    blocks: &[Value],
    stop_reason: &str,
    usage: crate::anthropic::cache::CacheUsage,
    searches: u32,
) -> Vec<SseEvent> {
    let mut events = vec![SseEvent::new(
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
    )];
    let delta = |index: usize, delta: Value| {
        SseEvent::new(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": delta}),
        )
    };
    for (index, block) in blocks.iter().enumerate() {
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
        events.push(SseEvent::new(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": start_block}),
        ));
        match kind {
            "text" => {
                for citation in block["citations"].as_array().into_iter().flatten() {
                    events.push(delta(
                        index,
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
                    events.push(delta(index, json!({"type": "text_delta", "text": chunk})));
                }
            }
            "server_tool_use" | "tool_use" => {
                events.push(delta(
                    index,
                    json!({"type": "input_json_delta", "partial_json": block["input"].to_string()}),
                ));
            }
            _ => {}
        }
        events.push(SseEvent::new(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));
    }
    events.push(SseEvent::new(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": websearch::usage_json(usage, usage.output_tokens, Some(searches))
        }),
    ));
    events.push(SseEvent::new(
        "message_stop",
        json!({"type": "message_stop"}),
    ));
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
        let request = internal_request(&payload);
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
    }

    #[test]
    fn stream_events_follow_the_official_block_lifecycle() {
        let usage = websearch::raw_websearch_usage(1200, 80);
        let blocks = vec![
            json!({"type": "server_tool_use", "id": "srvtoolu_01a", "name": "web_search", "input": {"query": "rust"}}),
            json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01a", "content": []}),
            json!({"type": "text", "text": "Rust 1.90.", "citations": [citation(&sources()[0], 1)]}),
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
}
