use std::{
    collections::BTreeMap,
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{SecondsFormat, Utc};
use clap::{Parser, ValueEnum};
use crc::{CRC_32_ISO_HDLC, Crc};
use futures::{Stream, StreamExt, stream};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::Semaphore, task::JoinSet, time::sleep};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "kiro_loadtest")]
#[command(about = "Kiro proxy load/chaos test helper")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:9022")]
    base_url: String,
    #[arg(long, default_value = "/cc/v1/messages")]
    route: String,
    #[arg(long, default_value = "claude-sonnet-4-20250514")]
    model: String,
    #[arg(long, default_value_t = 10)]
    concurrency: usize,
    #[arg(long, default_value_t = 100)]
    requests: usize,
    #[arg(long)]
    duration_secs: Option<u64>,
    #[arg(long, value_enum, default_value_t = Scenario::NormalStream)]
    scenario: Scenario,
    #[arg(long, default_value_t = true, num_args = 0..=1, default_missing_value = "true")]
    stream: bool,
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    thinking: bool,
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    tool_use: bool,
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    cache_control: bool,
    #[arg(long, value_enum, default_value_t = PayloadCase::TextHistory)]
    payload_case: PayloadCase,
    #[arg(long, default_value_t = 0)]
    long_context_chars: usize,
    #[arg(long, default_value_t = 1)]
    long_context_messages: usize,
    #[arg(long, default_value_t = 0)]
    current_user_chars: usize,
    #[arg(long, default_value_t = 0)]
    system_chars: usize,
    #[arg(long, default_value_t = 0)]
    tool_result_chars: usize,
    #[arg(long, default_value_t = 0)]
    tool_result_count: usize,
    #[arg(long, default_value_t = 0)]
    tool_input_depth: usize,
    #[arg(long, default_value_t = 0)]
    tool_count: usize,
    #[arg(long)]
    dfcache_route: Option<String>,
    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    real_upstream: bool,
    #[arg(long)]
    auth_key: Option<String>,
    #[arg(long)]
    target_pid: Option<u32>,
    #[arg(long)]
    fake_listen: Option<SocketAddr>,
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    fake_only: bool,
    #[arg(long, default_value_t = false, num_args = 0..=1, default_missing_value = "true")]
    fake_kiro_eventstream: bool,
    #[arg(long, default_value_t = 1500)]
    fake_delay_ms: u64,
    #[arg(long, default_value_t = 10)]
    fake_recover_after: u64,
    #[arg(long, default_value_t = 16)]
    fake_stream_chunks: usize,
    #[arg(long, default_value_t = 250)]
    fake_stream_chunk_delay_ms: u64,
    #[arg(long)]
    fake_capture_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    NormalStream,
    NormalNonStream,
    SlowFirstByte,
    RandomSlowFirstByte,
    DenseSlowFirstByte,
    TieredSlowFirstByte,
    SlowThinkingThenText,
    StreamIdleTimeout,
    JsonException200,
    RateLimit429,
    ServerError500,
    InvalidToolFormat,
    CachePointReject,
    ToolUseStream,
    LongStream,
    MalformedSse,
    ClientDrop,
    RecoveryAfterBurst,
    MixedChaos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum PayloadCase {
    TextHistory,
    LargeToolResults,
    DeepToolInput,
    ManyTools,
    MixedPathological,
}

#[derive(Debug, Clone)]
struct RunConfig {
    base_url: String,
    route: String,
    model: String,
    concurrency: usize,
    requests: usize,
    duration_secs: Option<u64>,
    scenario: Scenario,
    stream: bool,
    thinking: bool,
    tool_use: bool,
    cache_control: bool,
    payload_case: PayloadCase,
    long_context_chars: usize,
    long_context_messages: usize,
    current_user_chars: usize,
    system_chars: usize,
    tool_result_chars: usize,
    tool_result_count: usize,
    tool_input_depth: usize,
    tool_count: usize,
    timeout_secs: u64,
    auth_key: Option<String>,
    target_pid: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadtestReport {
    scenario: Scenario,
    request_profile: RequestProfile,
    started_at: String,
    duration_ms: u128,
    requests: usize,
    success: usize,
    errors: usize,
    status_counts: BTreeMap<String, usize>,
    ttfb_ms: Percentiles,
    first_thinking_ms: Percentiles,
    first_text_ms: Percentiles,
    total_latency_ms: Percentiles,
    memory: ResourceStats,
    file_descriptors: ResourceStats,
    cpu_percent: CpuStats,
    request_ids: Vec<String>,
    error_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestProfile {
    payload_case: PayloadCase,
    stream: bool,
    thinking: bool,
    tool_use: bool,
    cache_control: bool,
    long_context_chars: usize,
    long_context_messages: usize,
    current_user_chars: usize,
    system_chars: usize,
    tool_result_chars: usize,
    tool_result_count: usize,
    tool_input_depth: usize,
    tool_count: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct Percentiles {
    p50: u128,
    p95: u128,
    p99: u128,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResourceStats {
    start: u64,
    peak: u64,
    end: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct CpuStats {
    start: f64,
    peak: f64,
    end: f64,
}

#[derive(Debug, Clone)]
struct RequestResult {
    status: Option<u16>,
    success: bool,
    ttfb_ms: Option<u128>,
    first_thinking_ms: Option<u128>,
    first_text_ms: Option<u128>,
    total_latency_ms: u128,
    request_id: Option<String>,
    error_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ResourceSample {
    rss_bytes: u64,
    fd_count: u64,
    cpu_percent: f64,
}

#[derive(Clone)]
struct FakeServerState {
    scenario: Scenario,
    delay: Duration,
    recover_after: u64,
    stream_chunks: usize,
    stream_chunk_delay: Duration,
    kiro_eventstream: bool,
    capture_dir: Option<PathBuf>,
    counter: Arc<AtomicU64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    ensure_real_upstream_guard(args.real_upstream)?;

    if let Some(addr) = args.fake_listen {
        let state = FakeServerState {
            scenario: args.scenario,
            delay: Duration::from_millis(args.fake_delay_ms),
            recover_after: args.fake_recover_after,
            stream_chunks: args.fake_stream_chunks.max(1),
            stream_chunk_delay: Duration::from_millis(args.fake_stream_chunk_delay_ms),
            kiro_eventstream: args.fake_kiro_eventstream,
            capture_dir: args.fake_capture_dir,
            counter: Arc::new(AtomicU64::new(0)),
        };
        let server = tokio::spawn(run_fake_server(addr, state));
        tracing::info!("fake Kiro server listening on http://{}", addr);
        if args.fake_only {
            return server.await.context("fake server task failed")?;
        }
        sleep(Duration::from_millis(150)).await;
    }

    let route = args
        .dfcache_route
        .clone()
        .unwrap_or_else(|| args.route.clone());
    let config = RunConfig {
        base_url: args.base_url,
        route,
        model: args.model,
        concurrency: args.concurrency.max(1),
        requests: args.requests,
        duration_secs: args.duration_secs,
        scenario: args.scenario,
        stream: if args.scenario == Scenario::NormalNonStream {
            false
        } else {
            args.stream
        },
        thinking: args.thinking,
        tool_use: args.tool_use,
        cache_control: args.cache_control,
        payload_case: args.payload_case,
        long_context_chars: args.long_context_chars,
        long_context_messages: args.long_context_messages.max(1),
        current_user_chars: args.current_user_chars,
        system_chars: args.system_chars,
        tool_result_chars: args.tool_result_chars,
        tool_result_count: args.tool_result_count,
        tool_input_depth: args.tool_input_depth,
        tool_count: args.tool_count,
        timeout_secs: args.timeout_secs,
        auth_key: args.auth_key,
        target_pid: args.target_pid.unwrap_or_else(std::process::id),
    };

    let report = run_loadtest(config).await?;
    let report_json = serde_json::to_string_pretty(&report)?;
    if let Some(path) = args.report {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, report_json.as_bytes()).await?;
        println!("wrote report to {}", path.display());
    } else {
        println!("{report_json}");
    }

    Ok(())
}

fn ensure_real_upstream_guard(real_upstream: bool) -> anyhow::Result<()> {
    if !real_upstream {
        return Ok(());
    }
    match std::env::var("KIRO_LOADTEST_ALLOW_REAL_UPSTREAM") {
        Ok(value) if value == "1" => Ok(()),
        _ => bail!(
            "--real-upstream requires KIRO_LOADTEST_ALLOW_REAL_UPSTREAM=1 to avoid accidental production traffic"
        ),
    }
}

async fn run_loadtest(config: RunConfig) -> anyhow::Result<LoadtestReport> {
    let started_at = now_rfc3339();
    let run_started = Instant::now();
    let client = Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs))
        .build()?;
    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let deadline = config
        .duration_secs
        .map(|secs| Instant::now() + Duration::from_secs(secs));
    let resource_start = sample_resources(config.target_pid);
    let resource_peak = Arc::new(tokio::sync::Mutex::new(resource_start));
    let sampler_peak = resource_peak.clone();
    let sampler_pid = config.target_pid;
    let sampler = tokio::spawn(async move {
        loop {
            let sample = sample_resources(sampler_pid);
            let mut peak = sampler_peak.lock().await;
            peak.rss_bytes = peak.rss_bytes.max(sample.rss_bytes);
            peak.fd_count = peak.fd_count.max(sample.fd_count);
            peak.cpu_percent = peak.cpu_percent.max(sample.cpu_percent);
            drop(peak);
            sleep(Duration::from_millis(500)).await;
        }
    });

    let total_requests = config.requests;
    let mut join_set = JoinSet::new();
    let mut launched = 0usize;

    while launched < total_requests {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        let permit = semaphore.clone().acquire_owned().await?;
        let client = client.clone();
        let config = config.clone();
        let index = launched;
        join_set.spawn(async move {
            let _permit = permit;
            execute_request(client, config, index).await
        });
        launched += 1;
    }

    let mut results = Vec::with_capacity(launched);
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(request)) => results.push(request),
            Ok(Err(err)) => {
                let elapsed = run_started.elapsed().as_millis();
                tracing::warn!("loadtest request failed: {err:#}");
                results.push(RequestResult {
                    status: None,
                    success: false,
                    ttfb_ms: None,
                    first_thinking_ms: None,
                    first_text_ms: None,
                    total_latency_ms: elapsed,
                    request_id: None,
                    error_id: None,
                });
            }
            Err(err) => {
                tracing::warn!("loadtest task failed: {err}");
            }
        }
    }

    sampler.abort();
    let resource_end = sample_resources(config.target_pid);
    let resource_peak = *resource_peak.lock().await;

    Ok(build_report(
        config.scenario,
        request_profile(&config),
        started_at,
        run_started.elapsed().as_millis(),
        results,
        resource_start,
        resource_peak,
        resource_end,
    ))
}

async fn execute_request(
    client: Client,
    config: RunConfig,
    index: usize,
) -> anyhow::Result<RequestResult> {
    let url = format!("{}{}", config.base_url.trim_end_matches('/'), config.route);
    let body = request_body(&config, index);
    let started = Instant::now();
    let mut request = client.post(url).json(&body);
    if let Some(key) = config.auth_key.as_deref() {
        request = request
            .header("x-api-key", key)
            .header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    let response = request.send().await?;
    let status = response.status();
    let headers = response.headers().clone();
    let request_id = header_string(&headers, "request-id")
        .or_else(|| header_string(&headers, "anthropic-request-id"))
        .or_else(|| header_string(&headers, "x-amzn-requestid"));
    let error_id = header_string(&headers, "x-kiro-rs-error-id");

    if config.stream {
        let mut byte_stream = response.bytes_stream();
        let mut parser = SseMetricsParser::new(started);
        let mut first_byte = None;
        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk?;
            if first_byte.is_none() {
                first_byte = Some(started.elapsed().as_millis());
            }
            parser.push(&chunk);
            if config.scenario == Scenario::ClientDrop && first_byte.is_some() {
                break;
            }
        }
        let metrics = parser.finish();
        let success = status.is_success() && (metrics.saw_message_stop || metrics.saw_done);
        Ok(RequestResult {
            status: Some(status.as_u16()),
            success,
            ttfb_ms: first_byte,
            first_thinking_ms: metrics.first_thinking_ms,
            first_text_ms: metrics.first_text_ms,
            total_latency_ms: started.elapsed().as_millis(),
            request_id,
            error_id: error_id.or(metrics.error_id),
        })
    } else {
        let bytes = response.bytes().await?;
        let body_error_id = error_id_from_json(&bytes);
        let json_exception = body_is_json_exception(&bytes);
        Ok(RequestResult {
            status: Some(status.as_u16()),
            success: status.is_success() && !json_exception,
            ttfb_ms: Some(started.elapsed().as_millis()),
            first_thinking_ms: None,
            first_text_ms: None,
            total_latency_ms: started.elapsed().as_millis(),
            request_id,
            error_id: error_id.or(body_error_id),
        })
    }
}

fn request_body(config: &RunConfig, index: usize) -> Value {
    let messages = payload_case_messages(config, index);
    let mut body = json!({
        "model": config.model,
        "max_tokens": 256,
        "stream": config.stream,
        "messages": messages
    });

    if config.thinking {
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": 1024
        });
    }

    if config.cache_control {
        let text = if config.system_chars > 0 {
            deterministic_long_text(index, usize::MAX - 1, config.system_chars)
        } else {
            "stable loadtest system prompt".to_string()
        };
        body["system"] = json!([{
            "type": "text",
            "text": text,
            "cache_control": {"type": "ephemeral"}
        }]);
    } else if config.system_chars > 0 {
        body["system"] = json!([{
            "type": "text",
            "text": deterministic_long_text(index, usize::MAX - 1, config.system_chars)
        }]);
    }

    if payload_case_requires_tools(config) {
        body["tools"] = Value::Array(loadtest_tools(config));
    }

    body
}

fn payload_case_messages(config: &RunConfig, index: usize) -> Value {
    match config.payload_case {
        PayloadCase::TextHistory => {
            if config.long_context_chars > 0 {
                long_context_messages(
                    index,
                    config.long_context_chars,
                    config.long_context_messages,
                )
            } else {
                json!([user_text_message(current_user_text(config, index))])
            }
        }
        PayloadCase::LargeToolResults => Value::Array(tool_result_messages(config, index)),
        PayloadCase::DeepToolInput => Value::Array(deep_tool_input_messages(config, index)),
        PayloadCase::ManyTools => {
            Value::Array(vec![user_text_message(current_user_text(config, index))])
        }
        PayloadCase::MixedPathological => Value::Array(mixed_pathological_messages(config, index)),
    }
}

fn user_text_message(text: String) -> Value {
    json!({
        "role": "user",
        "content": [{
            "type": "text",
            "text": text
        }]
    })
}

fn assistant_text_message(text: String) -> Value {
    json!({
        "role": "assistant",
        "content": [{
            "type": "text",
            "text": text
        }]
    })
}

fn current_user_text(config: &RunConfig, index: usize) -> String {
    if config.current_user_chars > 0 {
        deterministic_long_text(index, usize::MAX - 2, config.current_user_chars)
    } else {
        format!("loadtest request {index}: respond with a short sentence")
    }
}

fn payload_case_requires_tools(config: &RunConfig) -> bool {
    config.tool_use
        || matches!(
            config.payload_case,
            PayloadCase::LargeToolResults
                | PayloadCase::DeepToolInput
                | PayloadCase::ManyTools
                | PayloadCase::MixedPathological
        )
}

fn loadtest_tools(config: &RunConfig) -> Vec<Value> {
    let tool_count = effective_tool_count(config);
    (0..tool_count)
        .map(|index| {
            let mut tool = json!({
                "name": format!("loadtest_tool_{index}"),
                "description": format!("Synthetic loadtest tool {index} with realistic schema pressure."),
                "input_schema": nested_tool_schema(effective_tool_input_depth(config).min(64))
            });
            if index == 0 {
                tool["name"] = json!("echo");
                tool["description"] = json!("Return the provided text.");
            }
            if config.cache_control {
                tool["cache_control"] = json!({"type": "ephemeral"});
            }
            tool
        })
        .collect()
}

fn effective_tool_count(config: &RunConfig) -> usize {
    if config.tool_count > 0 {
        return config.tool_count;
    }
    match config.payload_case {
        PayloadCase::ManyTools | PayloadCase::MixedPathological => 64,
        _ => 1,
    }
}

fn effective_tool_result_count(config: &RunConfig) -> usize {
    if config.tool_result_count > 0 {
        return config.tool_result_count;
    }
    match config.payload_case {
        PayloadCase::LargeToolResults => 4,
        PayloadCase::MixedPathological => 6,
        _ => 1,
    }
}

fn effective_tool_result_chars(config: &RunConfig) -> usize {
    if config.tool_result_chars > 0 {
        return config.tool_result_chars;
    }
    match config.payload_case {
        PayloadCase::LargeToolResults => 128 * 1024,
        PayloadCase::MixedPathological => 96 * 1024,
        _ => 4096,
    }
}

fn effective_tool_input_depth(config: &RunConfig) -> usize {
    if config.tool_input_depth > 0 {
        return config.tool_input_depth;
    }
    match config.payload_case {
        PayloadCase::DeepToolInput => 48,
        PayloadCase::MixedPathological => 32,
        PayloadCase::ManyTools => 8,
        _ => 1,
    }
}

fn tool_result_messages(config: &RunConfig, index: usize) -> Vec<Value> {
    let count = effective_tool_result_count(config);
    let chars = effective_tool_result_chars(config);
    let mut messages = Vec::with_capacity(count.saturating_mul(3).saturating_add(1));
    messages.push(user_text_message(format!(
        "loadtest request {index}: run echo and summarize the captured command output"
    )));
    for result_index in 0..count {
        let tool_use_id = format!("toolu_loadtest_{index}_{result_index}");
        messages.push(json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": tool_use_id,
                "name": "echo",
                "input": {"text": format!("collect diagnostics batch {result_index}")}
            }]
        }));
        messages.push(json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": [{
                    "type": "text",
                    "text": deterministic_tool_result_text(index, result_index, chars)
                }]
            }]
        }));
        messages.push(assistant_text_message(format!(
            "Captured diagnostics batch {result_index}; waiting for the next instruction."
        )));
    }
    messages.push(user_text_message(current_user_text(config, index)));
    messages
}

fn deep_tool_input_messages(config: &RunConfig, index: usize) -> Vec<Value> {
    let depth = effective_tool_input_depth(config);
    let tool_use_id = format!("toolu_deep_{index}");
    vec![
        user_text_message(format!(
            "loadtest request {index}: inspect the nested plan and report the deepest action"
        )),
        json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": tool_use_id,
                "name": "echo",
                "input": nested_tool_input(depth)
            }]
        }),
        json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": [{
                    "type": "text",
                    "text": "nested plan accepted by the diagnostic collector"
                }]
            }]
        }),
        assistant_text_message("Nested diagnostic plan was inspected.".to_string()),
        user_text_message(current_user_text(config, index)),
    ]
}

fn mixed_pathological_messages(config: &RunConfig, index: usize) -> Vec<Value> {
    let mut messages = if config.long_context_chars > 0 {
        match long_context_messages(
            index,
            config.long_context_chars,
            config.long_context_messages,
        ) {
            Value::Array(items) => items,
            _ => Vec::new(),
        }
    } else {
        vec![user_text_message(format!(
            "loadtest request {index}: begin mixed payload pressure case"
        ))]
    };
    messages.extend(tool_result_messages(config, index));
    messages.extend(deep_tool_input_messages(config, index));
    messages.push(user_text_message(current_user_text(config, index)));
    messages
}

fn nested_tool_input(depth: usize) -> Value {
    let mut value = json!({
        "leafAction": "summarize",
        "path": "/workspace/src/anthropic/payload_guard.rs",
        "checks": ["serialize", "trim", "repair", "shape"]
    });
    for level in (0..depth).rev() {
        value = json!({
            "level": level,
            "operation": "diagnostic_step",
            "metadata": {
                "file": format!("src/module_{level}.rs"),
                "span": {"start": level * 17, "end": level * 17 + 13},
                "hash": format!("{:08x}", level.wrapping_mul(2_654_435_761usize))
            },
            "children": [value, {
                "level": level,
                "operation": "side_check",
                "status": "skipped"
            }]
        });
    }
    value
}

fn nested_tool_schema(depth: usize) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "text": {"type": "string"},
            "limit": {"type": "integer"}
        },
        "required": ["text"]
    });
    for level in (0..depth).rev() {
        let key = format!("step_{level}");
        let mut properties = serde_json::Map::new();
        properties.insert(key.clone(), schema);
        properties.insert("enabled".to_string(), json!({"type": "boolean"}));
        properties.insert(
            "notes".to_string(),
            json!({"type": "array", "items": {"type": "string"}}),
        );
        schema = json!({
            "type": "object",
            "properties": Value::Object(properties),
            "required": [key]
        });
    }
    schema
}

fn long_context_messages(index: usize, total_chars: usize, message_count: usize) -> Value {
    let message_count = message_count.max(1);
    let per_message = total_chars.div_ceil(message_count);
    let mut messages = Vec::with_capacity(message_count);
    for message_index in 0..message_count {
        let role = if message_index + 1 == message_count {
            "user"
        } else if message_index % 2 == 0 {
            "user"
        } else {
            "assistant"
        };
        let text = deterministic_long_text(index, message_index, per_message);
        messages.push(json!({
            "role": role,
            "content": [{
                "type": "text",
                "text": text
            }]
        }));
    }
    Value::Array(messages)
}

fn deterministic_long_text(
    request_index: usize,
    message_index: usize,
    target_chars: usize,
) -> String {
    let seed = format!(
        "loadtest request={request_index} message={message_index} long context line with mixed code, JSON, markdown, and prose. "
    );
    let mut out = String::with_capacity(target_chars.saturating_add(seed.len()));
    while out.len() < target_chars {
        out.push_str(&seed);
        out.push_str(
            "fn compute(value: usize) -> usize { value.saturating_mul(31).wrapping_add(7) } ",
        );
        out.push_str("{\"path\":\"/tmp/example\",\"status\":\"ok\",\"items\":[1,2,3,4,5]} ");
        out.push_str("这是一段用于压测长上下文本地处理路径的中文内容。 ");
    }
    let mut end = target_chars.min(out.len());
    while end > 0 && !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out
}

fn deterministic_tool_result_text(
    request_index: usize,
    result_index: usize,
    target_chars: usize,
) -> String {
    let mut out = String::with_capacity(target_chars.saturating_add(512));
    let mut line = 0usize;
    while out.len() < target_chars {
        out.push_str(&format!(
            "[2026-07-03T12:{:02}:{:02}Z] request={request_index} result={result_index} line={line} level=INFO target=loadtest.collector\n",
            line % 60,
            (line * 7) % 60
        ));
        out.push_str(
            "command: rg -n \"payload_guard|serialize_request|trim_oldest\" src/anthropic src/external_pool.rs\n",
        );
        out.push_str(
            r#"json: {"file":"src/anthropic/payload_guard.rs","phase":"trim","bytesBefore":983421,"bytesAfter":742118,"changed":true}"#,
        );
        out.push('\n');
        out.push_str(
            "diff: - old_history_entry_with_large_tool_result\n      + summarized_history_entry_with_hash_and_excerpt\n",
        );
        out.push_str(
            "stack: converter::content_block -> payload_guard::breakdown -> usage::record_batch\n\n",
        );
        line += 1;
    }
    let mut end = target_chars.min(out.len());
    while end > 0 && !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out
}

#[derive(Debug, Default)]
struct ParsedSseMetrics {
    first_thinking_ms: Option<u128>,
    first_text_ms: Option<u128>,
    error_id: Option<String>,
    saw_message_stop: bool,
    saw_done: bool,
}

struct SseMetricsParser {
    started: Instant,
    buffer: Vec<u8>,
    metrics: ParsedSseMetrics,
}

impl SseMetricsParser {
    fn new(started: Instant) -> Self {
        Self {
            started,
            buffer: Vec::new(),
            metrics: ParsedSseMetrics::default(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        while let Some((idx, delimiter_len)) = find_sse_delimiter(&self.buffer) {
            let event = self.buffer[..idx].to_vec();
            self.buffer.drain(..idx + delimiter_len);
            self.inspect_event(&event);
        }
    }

    fn finish(mut self) -> ParsedSseMetrics {
        if !self.buffer.is_empty() {
            let event = std::mem::take(&mut self.buffer);
            self.inspect_event(&event);
        }
        self.metrics
    }

    fn inspect_event(&mut self, event: &[u8]) {
        let Ok(text) = std::str::from_utf8(event) else {
            return;
        };
        let data = event_data(text);
        for item in data {
            if item == "[DONE]" {
                self.metrics.saw_done = true;
                continue;
            }
            if data_has_thinking(&item) && self.metrics.first_thinking_ms.is_none() {
                self.metrics.first_thinking_ms = Some(self.started.elapsed().as_millis());
            }
            if data_has_visible_text(&item) && self.metrics.first_text_ms.is_none() {
                self.metrics.first_text_ms = Some(self.started.elapsed().as_millis());
            }
            if data_has_message_stop(&item) {
                self.metrics.saw_message_stop = true;
            }
            if self.metrics.error_id.is_none() {
                self.metrics.error_id = error_id_from_sse_data(&item);
            }
        }
    }
}

fn find_sse_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| (idx, 4))
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|idx| (idx, 2))
        })
}

fn event_data(event: &str) -> Vec<String> {
    event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn data_has_thinking(data: &str) -> bool {
    if data.contains("thinking_delta")
        || data.contains("reasoningContentEvent")
        || data.contains("<thinking>")
    {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return false;
    };
    value
        .pointer("/delta/type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.contains("thinking"))
        || value.get("thinking").is_some()
        || value.get("reasoningContentEvent").is_some()
}

fn data_has_visible_text(data: &str) -> bool {
    if data.contains("text_delta") {
        return true;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return false;
    };
    value
        .pointer("/delta/type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "text_delta")
        || value.pointer("/delta/text").is_some()
        || value.pointer("/content/0/text").is_some()
}

fn data_has_message_stop(data: &str) -> bool {
    if data.contains("\"message_stop\"") {
        return true;
    }
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|kind| kind == "message_stop")
        })
        .unwrap_or(false)
}

fn error_id_from_sse_data(data: &str) -> Option<String> {
    if data == "[DONE]" {
        return None;
    }
    let value = serde_json::from_str::<Value>(data).ok()?;
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .and_then(extract_error_id)
        .or_else(|| {
            value
                .get("error_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            value
                .get("request_id")
                .and_then(Value::as_str)
                .filter(|_| value.get("error").is_some())
                .map(ToOwned::to_owned)
        })
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn error_id_from_json(bytes: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .and_then(extract_error_id)
        .or_else(|| {
            value
                .get("error_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn body_is_json_exception(bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    value.get("__type").is_some()
        || value.get("code").is_some()
        || value.get("errorType").is_some()
        || value
            .get("error")
            .and_then(Value::as_object)
            .is_some_and(|error| error.contains_key("type") || error.contains_key("message"))
}

fn extract_error_id(message: &str) -> Option<String> {
    let marker = "error ID:";
    let (_, tail) = message.split_once(marker)?;
    Some(
        tail.trim()
            .trim_end_matches('.')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
    )
    .filter(|value| !value.is_empty())
}

fn build_report(
    scenario: Scenario,
    request_profile: RequestProfile,
    started_at: String,
    duration_ms: u128,
    results: Vec<RequestResult>,
    resource_start: ResourceSample,
    resource_peak: ResourceSample,
    resource_end: ResourceSample,
) -> LoadtestReport {
    let mut status_counts = BTreeMap::new();
    let mut ttfb = Vec::new();
    let mut first_thinking = Vec::new();
    let mut first_text = Vec::new();
    let mut total_latency = Vec::new();
    let mut request_ids = Vec::new();
    let mut error_ids = Vec::new();
    let mut success = 0usize;

    for result in &results {
        let status = result
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "transport_error".to_string());
        *status_counts.entry(status).or_insert(0) += 1;
        if result.success {
            success += 1;
        }
        if let Some(value) = result.ttfb_ms {
            ttfb.push(value);
        }
        if let Some(value) = result.first_thinking_ms {
            first_thinking.push(value);
        }
        if let Some(value) = result.first_text_ms {
            first_text.push(value);
        }
        total_latency.push(result.total_latency_ms);
        if let Some(id) = &result.request_id {
            push_unique_limited(&mut request_ids, id);
        }
        if let Some(id) = &result.error_id {
            push_unique_limited(&mut error_ids, id);
        }
    }

    LoadtestReport {
        scenario,
        request_profile,
        started_at,
        duration_ms,
        requests: results.len(),
        success,
        errors: results.len().saturating_sub(success),
        status_counts,
        ttfb_ms: percentiles(ttfb),
        first_thinking_ms: percentiles(first_thinking),
        first_text_ms: percentiles(first_text),
        total_latency_ms: percentiles(total_latency),
        memory: ResourceStats {
            start: resource_start.rss_bytes,
            peak: resource_peak
                .rss_bytes
                .max(resource_start.rss_bytes)
                .max(resource_end.rss_bytes),
            end: resource_end.rss_bytes,
        },
        file_descriptors: ResourceStats {
            start: resource_start.fd_count,
            peak: resource_peak
                .fd_count
                .max(resource_start.fd_count)
                .max(resource_end.fd_count),
            end: resource_end.fd_count,
        },
        cpu_percent: CpuStats {
            start: resource_start.cpu_percent,
            peak: resource_peak
                .cpu_percent
                .max(resource_start.cpu_percent)
                .max(resource_end.cpu_percent),
            end: resource_end.cpu_percent,
        },
        request_ids,
        error_ids,
    }
}

fn request_profile(config: &RunConfig) -> RequestProfile {
    let uses_tool_results = matches!(
        config.payload_case,
        PayloadCase::LargeToolResults | PayloadCase::MixedPathological
    );
    let uses_deep_input = matches!(
        config.payload_case,
        PayloadCase::DeepToolInput | PayloadCase::ManyTools | PayloadCase::MixedPathological
    );
    RequestProfile {
        payload_case: config.payload_case,
        stream: config.stream,
        thinking: config.thinking,
        tool_use: config.tool_use,
        cache_control: config.cache_control,
        long_context_chars: config.long_context_chars,
        long_context_messages: config.long_context_messages,
        current_user_chars: config.current_user_chars,
        system_chars: config.system_chars,
        tool_result_chars: if uses_tool_results {
            effective_tool_result_chars(config)
        } else {
            config.tool_result_chars
        },
        tool_result_count: if uses_tool_results {
            effective_tool_result_count(config)
        } else {
            config.tool_result_count
        },
        tool_input_depth: if uses_deep_input {
            effective_tool_input_depth(config)
        } else {
            config.tool_input_depth
        },
        tool_count: if payload_case_requires_tools(config) {
            effective_tool_count(config)
        } else {
            config.tool_count
        },
    }
}

fn push_unique_limited(items: &mut Vec<String>, value: &str) {
    if items.len() >= 100 || items.iter().any(|item| item == value) {
        return;
    }
    items.push(value.to_string());
}

fn percentiles(mut values: Vec<u128>) -> Percentiles {
    if values.is_empty() {
        return Percentiles::default();
    }
    values.sort_unstable();
    Percentiles {
        p50: percentile_sorted(&values, 50),
        p95: percentile_sorted(&values, 95),
        p99: percentile_sorted(&values, 99),
    }
}

fn percentile_sorted(values: &[u128], percentile: usize) -> u128 {
    let len = values.len();
    if len == 0 {
        return 0;
    }
    let rank = ((len - 1) * percentile).div_ceil(100);
    values[rank.min(len - 1)]
}

fn sample_resources(pid: u32) -> ResourceSample {
    ResourceSample {
        rss_bytes: sample_rss_bytes(pid).unwrap_or_default(),
        fd_count: sample_fd_count(pid).unwrap_or_default(),
        cpu_percent: sample_cpu_percent(pid).unwrap_or_default(),
    }
}

fn sample_rss_bytes(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rss_kb = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(rss_kb * 1024)
}

fn sample_cpu_percent(pid: u32) -> Option<f64> {
    let output = Command::new("ps")
        .args(["-o", "%cpu=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
}

fn sample_fd_count(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .ok()
            .map(|entries| entries.count() as u64)
    }
    #[cfg(not(target_os = "linux"))]
    {
        if pid == std::process::id() {
            return std::fs::read_dir("/dev/fd")
                .ok()
                .map(|entries| entries.count() as u64);
        }
        let output = Command::new("lsof")
            .args(["-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .skip(1)
                .count() as u64,
        )
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn run_fake_server(addr: SocketAddr, state: FakeServerState) -> anyhow::Result<()> {
    let app = Router::new().fallback(any(fake_handler)).with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn fake_handler(
    State(state): State<FakeServerState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let sequence = state.counter.fetch_add(1, Ordering::Relaxed) + 1;
    let request_id = format!("fake_req_{}", sequence);
    let request = serde_json::from_slice::<Value>(&body).unwrap_or_else(|_| json!({}));
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| wants_stream(&headers) || wants_kiro_eventstream(&state, &uri));
    let scenario = effective_fake_scenario(&state, sequence);
    capture_fake_request(&state, &request_id, &uri, &headers, &request).await;
    let thinking = body_requests_thinking(&request);

    match scenario {
        Scenario::RateLimit429 => json_error(
            StatusCode::TOO_MANY_REQUESTS,
            &request_id,
            "rate_limit_error",
            "rate limited",
        ),
        Scenario::ServerError500 => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &request_id,
            "api_error",
            "server error",
        ),
        Scenario::InvalidToolFormat => json_error(
            StatusCode::BAD_REQUEST,
            &request_id,
            "invalid_request_error",
            "Invalid tool use format.",
        ),
        Scenario::CachePointReject => {
            if body_contains_cache_point(&request) {
                json_error(
                    StatusCode::BAD_REQUEST,
                    &request_id,
                    "invalid_request_error",
                    "Invalid tool use format.",
                )
            } else if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(
                        request_id,
                        normal_kiro_events(Duration::ZERO, thinking),
                    )
                } else {
                    sse_response(request_id, normal_sse_events(Duration::ZERO, thinking))
                }
            } else {
                fake_json_message(&request_id)
            }
        }
        Scenario::JsonException200 => (
            StatusCode::OK,
            [
                ("content-type", "application/json"),
                ("x-amzn-requestid", request_id.as_str()),
            ],
            Json(json!({
                "__type": "ThrottlingException",
                "message": "Rate exceeded"
            })),
        )
            .into_response(),
        Scenario::MalformedSse => sse_response(
            request_id,
            vec![(
                Duration::ZERO,
                "data: {\"type\":\"message_start\"".to_string(),
            )],
        ),
        Scenario::StreamIdleTimeout => {
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(
                        request_id,
                        vec![(
                            state.delay,
                            kiro_event_frame(
                                "assistantResponseEvent",
                                json!({"content":"partial response"}),
                            ),
                        )],
                    )
                } else {
                    sse_response(
                        request_id,
                        vec![(
                            state.delay,
                            "data: {\"type\":\"message_start\"}\n\n".to_string(),
                        )],
                    )
                }
            } else {
                sleep(state.delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::SlowFirstByte => {
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(request_id, normal_kiro_events(state.delay, false))
                } else {
                    sse_response(request_id, normal_sse_events(state.delay, false))
                }
            } else {
                sleep(state.delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::RandomSlowFirstByte => {
            let delay = random_slow_first_byte_delay(state.delay, sequence);
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(request_id, normal_kiro_events(delay, thinking))
                } else {
                    sse_response(request_id, normal_sse_events(delay, thinking))
                }
            } else {
                sleep(delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::DenseSlowFirstByte => {
            let delay = dense_slow_first_byte_delay(state.delay, sequence);
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(request_id, normal_kiro_events(delay, thinking))
                } else {
                    sse_response(request_id, normal_sse_events(delay, thinking))
                }
            } else {
                sleep(delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::TieredSlowFirstByte => {
            let delay = tiered_slow_first_byte_delay(sequence);
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(request_id, normal_kiro_events(delay, thinking))
                } else {
                    sse_response(request_id, normal_sse_events(delay, thinking))
                }
            } else {
                sleep(delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::SlowThinkingThenText => {
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(request_id, thinking_kiro_events(state.delay))
                } else {
                    sse_response(request_id, thinking_sse_events(state.delay))
                }
            } else {
                sleep(state.delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::ToolUseStream => {
            if stream {
                if state.kiro_eventstream {
                    if body_contains_tool_result(&request) {
                        kiro_eventstream_response(
                            request_id,
                            normal_kiro_events(Duration::ZERO, thinking),
                        )
                    } else {
                        kiro_eventstream_response(
                            request_id,
                            tool_use_kiro_events(select_fake_tool_name(&request)),
                        )
                    }
                } else if body_contains_tool_result(&request) {
                    sse_response(request_id, normal_sse_events(Duration::ZERO, thinking))
                } else {
                    sse_response(
                        request_id,
                        tool_use_sse_events(select_fake_tool_name(&request)),
                    )
                }
            } else {
                fake_json_message(&request_id)
            }
        }
        Scenario::LongStream => {
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(
                        request_id,
                        long_stream_kiro_events(
                            state.delay,
                            state.stream_chunks,
                            state.stream_chunk_delay,
                        ),
                    )
                } else {
                    sse_response(
                        request_id,
                        long_stream_sse_events(
                            state.delay,
                            state.stream_chunks,
                            state.stream_chunk_delay,
                        ),
                    )
                }
            } else {
                sleep(
                    state
                        .delay
                        .saturating_add(state.stream_chunk_delay * state.stream_chunks as u32),
                )
                .await;
                fake_json_message(&request_id)
            }
        }
        Scenario::NormalNonStream => fake_json_message(&request_id),
        Scenario::NormalStream | Scenario::ClientDrop | Scenario::RecoveryAfterBurst => {
            if stream {
                if state.kiro_eventstream {
                    kiro_eventstream_response(
                        request_id,
                        normal_kiro_events(Duration::ZERO, thinking),
                    )
                } else {
                    sse_response(request_id, normal_sse_events(Duration::ZERO, thinking))
                }
            } else {
                fake_json_message(&request_id)
            }
        }
        Scenario::MixedChaos => unreachable!("mixed chaos resolves to a concrete scenario"),
    }
}

fn random_slow_first_byte_delay(base: Duration, sequence: u64) -> Duration {
    let max_ms = base.as_millis().clamp(1, u64::MAX as u128) as u64;
    let mixed = sequence
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    if mixed % 4 == 0 {
        return Duration::ZERO;
    }
    let min_ms = max_ms.min(50);
    let spread = max_ms.saturating_sub(min_ms).max(1);
    Duration::from_millis(min_ms + (mixed % spread))
}

fn dense_slow_first_byte_delay(base: Duration, sequence: u64) -> Duration {
    let base_ms = base.as_millis().clamp(1, u64::MAX as u128) as u64;
    let jitter_cap = (base_ms / 5).clamp(1, 250);
    let jitter = sequence.wrapping_mul(1103515245).wrapping_add(12345) % jitter_cap;
    Duration::from_millis(base_ms.saturating_add(jitter))
}

fn tiered_slow_first_byte_delay(sequence: u64) -> Duration {
    match sequence % 3 {
        1 => Duration::from_secs(3),
        2 => Duration::from_secs(10),
        _ => Duration::from_secs(22),
    }
}

fn effective_fake_scenario(state: &FakeServerState, sequence: u64) -> Scenario {
    if state.scenario == Scenario::MixedChaos {
        return mixed_chaos_scenario(sequence);
    }
    if state.scenario != Scenario::RecoveryAfterBurst {
        return state.scenario;
    }
    let count = state.counter.load(Ordering::Relaxed);
    if count < state.recover_after {
        Scenario::ServerError500
    } else {
        Scenario::NormalStream
    }
}

fn mixed_chaos_scenario(sequence: u64) -> Scenario {
    match sequence % 12 {
        0 => Scenario::RateLimit429,
        1 => Scenario::ServerError500,
        2 | 3 => Scenario::TieredSlowFirstByte,
        4 | 5 => Scenario::LongStream,
        6 => Scenario::RandomSlowFirstByte,
        _ => Scenario::NormalStream,
    }
}

fn wants_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"))
}

fn wants_kiro_eventstream(state: &FakeServerState, uri: &Uri) -> bool {
    state.kiro_eventstream && uri.path().contains("generateAssistantResponse")
}

fn body_contains_cache_point(value: &Value) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| key == "cachePoint" || body_contains_cache_point(value)),
        Value::Array(items) => items.iter().any(body_contains_cache_point),
        _ => false,
    }
}

fn body_contains_tool_result(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "tool_result")
            {
                return true;
            }
            if map.iter().any(|(key, value)| {
                key == "toolResults" && value.as_array().is_some_and(|items| !items.is_empty())
            }) {
                return true;
            }
            map.values().any(body_contains_tool_result)
        }
        Value::Array(items) => items.iter().any(body_contains_tool_result),
        _ => false,
    }
}

fn select_fake_tool_name(value: &Value) -> String {
    let mut names = Vec::new();
    collect_tool_names(value, &mut names);
    names
        .iter()
        .find(|name| {
            let normalized = name.to_ascii_lowercase();
            normalized == "bash" || normalized.starts_with("bash")
        })
        .or_else(|| {
            names.iter().find(|name| {
                let normalized = name.to_ascii_lowercase();
                normalized == "echo" || normalized.starts_with("echo")
            })
        })
        .or_else(|| {
            names.iter().find(|name| {
                let normalized = name.to_ascii_lowercase();
                normalized == "ping"
                    || normalized.starts_with("ping")
                    || normalized.ends_with("__ping")
                    || normalized.contains("ping")
            })
        })
        .or_else(|| names.first())
        .cloned()
        .unwrap_or_else(|| "echo".to_string())
}

fn collect_tool_names(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map
                .get("toolSpecification")
                .and_then(|spec| spec.get("name"))
                .and_then(Value::as_str)
            {
                names.push(name.to_string());
            }
            for value in map.values() {
                collect_tool_names(value, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_tool_names(item, names);
            }
        }
        _ => {}
    }
}

async fn capture_fake_request(
    state: &FakeServerState,
    request_id: &str,
    uri: &Uri,
    headers: &HeaderMap,
    request: &Value,
) {
    let Some(dir) = state.capture_dir.as_ref() else {
        return;
    };
    if let Err(err) = tokio::fs::create_dir_all(dir).await {
        tracing::warn!(path = %dir.display(), error = %err, "failed to create fake capture dir");
        return;
    }

    let mut captured_headers = serde_json::Map::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_ascii_lowercase();
        let value = if matches!(
            key.as_str(),
            "authorization" | "x-api-key" | "x-amz-security-token"
        ) {
            "<redacted>".to_string()
        } else {
            value.to_str().unwrap_or("<non-utf8>").to_string()
        };
        captured_headers.insert(key, Value::String(value));
    }

    let capture = json!({
        "requestId": request_id,
        "path": uri.path(),
        "query": uri.query(),
        "headers": captured_headers,
        "thinkingDetected": body_requests_thinking(request),
        "body": request,
    });
    let path = dir.join(format!("{request_id}.json"));
    match serde_json::to_vec_pretty(&capture) {
        Ok(bytes) => {
            if let Err(err) = tokio::fs::write(&path, bytes).await {
                tracing::warn!(path = %path.display(), error = %err, "failed to write fake capture");
            }
        }
        Err(err) => {
            tracing::warn!(request_id, error = %err, "failed to serialize fake capture");
        }
    }
}

fn body_requests_thinking(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map
                .get("thinking")
                .is_some_and(|thinking| thinking_value_enabled(thinking))
            {
                return true;
            }
            map.values().any(body_requests_thinking)
        }
        Value::Array(items) => items.iter().any(body_requests_thinking),
        Value::String(text) => {
            text.contains("<thinking_mode>enabled</thinking_mode>")
                || text.contains("<thinking_mode>adaptive</thinking_mode>")
        }
        _ => false,
    }
}

fn thinking_value_enabled(value: &Value) -> bool {
    value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "enabled" | "adaptive"))
}

fn json_error(status: StatusCode, request_id: &str, error_type: &str, message: &str) -> Response {
    (
        status,
        [
            ("request-id", request_id),
            ("anthropic-request-id", request_id),
            ("x-kiro-rs-error-id", request_id),
        ],
        Json(json!({
            "type": "error",
            "error": {
                "type": error_type,
                "message": message
            },
            "request_id": request_id
        })),
    )
        .into_response()
}

fn fake_json_message(request_id: &str) -> Response {
    (
        StatusCode::OK,
        [
            ("request-id", request_id),
            ("anthropic-request-id", request_id),
        ],
        Json(json!({
            "id": "msg_fake",
            "type": "message",
            "role": "assistant",
            "model": "fake-sonnet",
            "content": [{"type": "text", "text": "fake response"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        })),
    )
        .into_response()
}

fn sse_response(request_id: String, events: Vec<(Duration, String)>) -> Response {
    let body_stream = delayed_event_stream(events);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("request-id", &request_id)
        .header("anthropic-request-id", &request_id)
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn kiro_eventstream_response(request_id: String, events: Vec<(Duration, Vec<u8>)>) -> Response {
    let body_stream = delayed_byte_stream(events);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("request-id", &request_id)
        .header("x-amzn-requestid", &request_id)
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn delayed_event_stream(
    events: Vec<(Duration, String)>,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    stream::unfold(events.into_iter(), |mut iter| async move {
        let (delay, event) = iter.next()?;
        if !delay.is_zero() {
            sleep(delay).await;
        }
        Some((Ok(Bytes::from(event)), iter))
    })
}

fn delayed_byte_stream(
    events: Vec<(Duration, Vec<u8>)>,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    stream::unfold(events.into_iter(), |mut iter| async move {
        let (delay, event) = iter.next()?;
        if !delay.is_zero() {
            sleep(delay).await;
        }
        Some((Ok(Bytes::from(event)), iter))
    })
}

fn normal_sse_events(first_delay: Duration, thinking: bool) -> Vec<(Duration, String)> {
    let mut events = vec![
        (
            first_delay,
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fake\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"fake-sonnet\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".to_string(),
        ),
    ];
    if thinking {
        events.push((
            Duration::ZERO,
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"considering\"}}\n\n".to_string(),
        ));
    }
    events.extend([
        (
            Duration::ZERO,
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"fake response\"}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
        ),
    ]);
    events
}

fn normal_kiro_events(first_delay: Duration, thinking: bool) -> Vec<(Duration, Vec<u8>)> {
    let mut events = Vec::new();
    if thinking {
        events.push((
            Duration::ZERO,
            kiro_event_frame(
                "reasoningContentEvent",
                json!({"text":"thinking through the loadtest path","signature":"fake-signature"}),
            ),
        ));
    }
    events.extend([
        (
            first_delay,
            kiro_event_frame("assistantResponseEvent", json!({"content":"fake response"})),
        ),
        (
            Duration::ZERO,
            kiro_event_frame(
                "metadataEvent",
                json!({
                    "tokenUsage": {
                        "uncachedInputTokens": 10,
                        "cacheReadInputTokens": 0,
                        "cacheWriteInputTokens": 0,
                        "outputTokens": if thinking { 9 } else { 3 },
                        "totalTokens": if thinking { 19 } else { 13 }
                    }
                }),
            ),
        ),
        (
            Duration::ZERO,
            kiro_event_frame(
                "messageMetadataEvent",
                json!({
                    "conversationId": "fake-conversation",
                    "utteranceId": "fake-utterance",
                    "tokenUsage": {
                        "uncachedInputTokens": 10,
                        "cacheReadInputTokens": 0,
                        "cacheWriteInputTokens": 0,
                        "outputTokens": if thinking { 9 } else { 3 },
                        "totalTokens": if thinking { 19 } else { 13 }
                    }
                }),
            ),
        ),
    ]);
    events
}

fn long_stream_sse_events(
    first_delay: Duration,
    chunks: usize,
    chunk_delay: Duration,
) -> Vec<(Duration, String)> {
    let chunks = chunks.max(1);
    let mut events = vec![
        (
            first_delay,
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fake\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"fake-sonnet\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".to_string(),
        ),
    ];
    for index in 0..chunks {
        events.push((
            chunk_delay,
            format!(
                "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"stream chunk {index}; \"}}}}\n\n"
            ),
        ));
    }
    events.extend([
        (
            Duration::ZERO,
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":64}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
        ),
        (Duration::ZERO, "data: [DONE]\n\n".to_string()),
    ]);
    events
}

fn long_stream_kiro_events(
    first_delay: Duration,
    chunks: usize,
    chunk_delay: Duration,
) -> Vec<(Duration, Vec<u8>)> {
    let chunks = chunks.max(1);
    let mut events = Vec::with_capacity(chunks.saturating_add(2));
    for index in 0..chunks {
        events.push((
            if index == 0 { first_delay } else { chunk_delay },
            kiro_event_frame(
                "assistantResponseEvent",
                json!({"content": format!("stream chunk {index}; ")}),
            ),
        ));
    }
    events.push((
        Duration::ZERO,
        kiro_event_frame(
            "metadataEvent",
            json!({
                "tokenUsage": {
                    "uncachedInputTokens": 10,
                    "cacheReadInputTokens": 0,
                    "cacheWriteInputTokens": 0,
                    "outputTokens": 64,
                    "totalTokens": 74
                }
            }),
        ),
    ));
    events.push((
        Duration::ZERO,
        kiro_event_frame(
            "messageMetadataEvent",
            json!({
                "conversationId": "fake-conversation",
                "utteranceId": "fake-utterance",
                "tokenUsage": {
                    "uncachedInputTokens": 10,
                    "cacheReadInputTokens": 0,
                    "cacheWriteInputTokens": 0,
                    "outputTokens": 64,
                    "totalTokens": 74
                }
            }),
        ),
    ));
    events
}

fn thinking_sse_events(text_delay: Duration) -> Vec<(Duration, String)> {
    vec![
        (
            Duration::ZERO,
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fake\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"fake-sonnet\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"real thinking chunk\"}}\n\n".to_string(),
        ),
        (
            text_delay,
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"visible text\"}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
        ),
    ]
}

fn thinking_kiro_events(text_delay: Duration) -> Vec<(Duration, Vec<u8>)> {
    vec![
        (
            Duration::ZERO,
            kiro_event_frame(
                "reasoningContentEvent",
                json!({"text":"real thinking chunk","signature":"fake-signature"}),
            ),
        ),
        (
            text_delay,
            kiro_event_frame("assistantResponseEvent", json!({"content":"visible text"})),
        ),
        (
            Duration::ZERO,
            kiro_event_frame(
                "metadataEvent",
                json!({
                    "tokenUsage": {
                        "uncachedInputTokens": 10,
                        "cacheReadInputTokens": 0,
                        "cacheWriteInputTokens": 0,
                        "outputTokens": 8,
                        "totalTokens": 18
                    }
                }),
            ),
        ),
    ]
}

fn tool_use_input_for(name: &str) -> Value {
    let normalized = name.to_ascii_lowercase();
    if normalized == "bash" || normalized.starts_with("bash") {
        json!({"command": "echo cli tool ok"})
    } else if normalized == "echo" || normalized.starts_with("echo") {
        json!({"text": "cli tool ok"})
    } else {
        json!({})
    }
}

fn tool_use_kiro_events(name: String) -> Vec<(Duration, Vec<u8>)> {
    let input = tool_use_input_for(&name).to_string();
    vec![
        (
            Duration::ZERO,
            kiro_event_frame(
                "toolUseEvent",
                json!({
                    "name": name,
                    "toolUseId": "toolu_fake_1",
                    "input": input,
                    "stop": true
                }),
            ),
        ),
        (
            Duration::ZERO,
            kiro_event_frame(
                "metadataEvent",
                json!({
                    "tokenUsage": {
                        "uncachedInputTokens": 10,
                        "cacheReadInputTokens": 0,
                        "cacheWriteInputTokens": 0,
                        "outputTokens": 6,
                        "totalTokens": 16
                    }
                }),
            ),
        ),
    ]
}

fn tool_use_sse_events(name: String) -> Vec<(Duration, String)> {
    let input = tool_use_input_for(&name).to_string();
    vec![
        (
            Duration::ZERO,
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fake\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"fake-sonnet\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            format!(
                "event: content_block_start\ndata: {}\n\n",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": "toolu_fake_1",
                        "name": name,
                        "input": {}
                    }
                })
            ),
        ),
        (
            Duration::ZERO,
            format!(
                "event: content_block_delta\ndata: {}\n\n",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": input
                    }
                })
            ),
        ),
        (
            Duration::ZERO,
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":6}}\n\n".to_string(),
        ),
        (
            Duration::ZERO,
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
        ),
    ]
}

fn kiro_event_frame(event_type: &str, payload: Value) -> Vec<u8> {
    let payload = serde_json::to_vec(&payload).expect("fake event payload serializes");
    let headers = kiro_event_headers(event_type);
    let total_length = 12usize + headers.len() + payload.len() + 4usize;
    let header_length = headers.len();

    let mut frame = Vec::with_capacity(total_length);
    frame.extend_from_slice(&(total_length as u32).to_be_bytes());
    frame.extend_from_slice(&(header_length as u32).to_be_bytes());
    let prelude_crc = crc32(&frame[..8]);
    frame.extend_from_slice(&prelude_crc.to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(&payload);
    let message_crc = crc32(&frame);
    frame.extend_from_slice(&message_crc.to_be_bytes());
    frame
}

fn kiro_event_headers(event_type: &str) -> Vec<u8> {
    let mut headers = Vec::new();
    push_eventstream_string_header(&mut headers, ":message-type", "event");
    push_eventstream_string_header(&mut headers, ":event-type", event_type);
    push_eventstream_string_header(&mut headers, ":content-type", "application/json");
    headers
}

fn push_eventstream_string_header(headers: &mut Vec<u8>, name: &str, value: &str) {
    headers.push(name.len() as u8);
    headers.extend_from_slice(name.as_bytes());
    headers.push(7);
    headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
    headers.extend_from_slice(value.as_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    static CRC: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
    CRC.checksum(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_calculation_uses_nearest_rank_ceiling() {
        let values = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let p = percentiles(values);
        assert_eq!(p.p50, 6);
        assert_eq!(p.p95, 10);
        assert_eq!(p.p99, 10);
    }

    #[test]
    fn sse_parser_detects_thinking_and_visible_text() {
        let started = Instant::now();
        let mut parser = SseMetricsParser::new(started);
        for (_, event) in thinking_sse_events(Duration::ZERO) {
            parser.push(event.as_bytes());
        }
        let metrics = parser.finish();
        assert!(metrics.first_thinking_ms.is_some());
        assert!(metrics.first_text_ms.is_some());
        assert!(metrics.saw_message_stop);
    }

    #[test]
    fn fake_server_detects_native_thinking_request() {
        let request = json!({
            "thinking": {
                "type": "enabled",
                "budget_tokens": 1024
            }
        });

        assert!(body_requests_thinking(&request));
    }

    #[test]
    fn fake_server_detects_kiro_history_thinking_tags() {
        let request = json!({
            "conversationState": {
                "history": [{
                    "userInputMessage": {
                        "content": "<thinking_mode>adaptive</thinking_mode><thinking_effort>high</thinking_effort>"
                    }
                }]
            }
        });

        assert!(body_requests_thinking(&request));
    }

    #[test]
    fn random_slow_first_byte_has_fast_and_slow_samples() {
        let base = Duration::from_millis(1_500);
        let samples: Vec<Duration> = (1..=16)
            .map(|index| random_slow_first_byte_delay(base, index))
            .collect();

        assert!(samples.iter().any(|value| value.is_zero()));
        assert!(
            samples
                .iter()
                .any(|value| *value >= Duration::from_millis(50))
        );
        assert!(samples.iter().all(|value| *value <= base));
    }

    #[test]
    fn dense_slow_first_byte_delays_every_sample() {
        let base = Duration::from_millis(1_500);
        let samples: Vec<Duration> = (1..=16)
            .map(|index| dense_slow_first_byte_delay(base, index))
            .collect();

        assert!(samples.iter().all(|value| *value >= base));
        assert!(samples.iter().any(|value| *value > base));
    }

    #[test]
    fn tiered_slow_first_byte_covers_seconds_ten_and_twenty_plus_seconds() {
        let samples: Vec<Duration> = (1..=6).map(tiered_slow_first_byte_delay).collect();

        assert!(samples.contains(&Duration::from_secs(3)));
        assert!(samples.contains(&Duration::from_secs(10)));
        assert!(samples.contains(&Duration::from_secs(22)));
    }

    #[test]
    fn mixed_chaos_includes_success_slow_long_and_errors() {
        let scenarios: Vec<Scenario> = (1..=24).map(mixed_chaos_scenario).collect();

        assert!(scenarios.contains(&Scenario::RateLimit429));
        assert!(scenarios.contains(&Scenario::ServerError500));
        assert!(scenarios.contains(&Scenario::TieredSlowFirstByte));
        assert!(scenarios.contains(&Scenario::RandomSlowFirstByte));
        assert!(scenarios.contains(&Scenario::LongStream));
        assert!(scenarios.contains(&Scenario::NormalStream));
    }

    #[test]
    fn fake_server_detects_tool_result_and_prefers_bash_tool() {
        let request = json!({
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "userInputMessageContext": {
                            "tools": [
                                {"toolSpecification": {"name": "echo"}},
                                {"toolSpecification": {"name": "Bash"}}
                            ],
                            "toolResults": [{
                                "toolUseId": "toolu_fake_1",
                                "content": [{"text": "ok"}]
                            }]
                        }
                    }
                }
            }
        });

        assert!(body_contains_tool_result(&request));
        assert_eq!(select_fake_tool_name(&request), "Bash");
    }

    fn test_config(payload_case: PayloadCase) -> RunConfig {
        RunConfig {
            base_url: "http://127.0.0.1:19022".to_string(),
            route: "/cc/v1/messages".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            concurrency: 1,
            requests: 1,
            duration_secs: None,
            scenario: Scenario::NormalStream,
            stream: true,
            thinking: false,
            tool_use: false,
            cache_control: false,
            payload_case,
            long_context_chars: 0,
            long_context_messages: 1,
            current_user_chars: 0,
            system_chars: 0,
            tool_result_chars: 1024,
            tool_result_count: 2,
            tool_input_depth: 4,
            tool_count: 3,
            timeout_secs: 60,
            auth_key: None,
            target_pid: std::process::id(),
        }
    }

    #[test]
    fn large_tool_result_case_builds_real_tool_result_blocks() {
        let body = request_body(&test_config(PayloadCase::LargeToolResults), 7);
        assert!(body_contains_tool_result(&body));
        let serialized = serde_json::to_string(&body).expect("serialize body");
        assert!(serialized.contains("payload_guard.rs"));
        assert!(serialized.contains("tool_result"));
        assert!(serialized.len() > 2000);
    }

    #[test]
    fn deep_tool_input_case_builds_nested_input() {
        let body = request_body(&test_config(PayloadCase::DeepToolInput), 3);
        let serialized = serde_json::to_string(&body).expect("serialize body");
        assert!(serialized.contains("\"level\":0"));
        assert!(serialized.contains("\"level\":3"));
        assert!(serialized.contains("side_check"));
    }

    #[test]
    fn many_tools_case_builds_requested_tool_count() {
        let body = request_body(&test_config(PayloadCase::ManyTools), 1);
        let tools = body.get("tools").and_then(Value::as_array).expect("tools");
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn long_stream_sse_events_hold_stream_and_finish_cleanly() {
        let started = Instant::now();
        let mut parser = SseMetricsParser::new(started);
        let events = long_stream_sse_events(Duration::ZERO, 4, Duration::ZERO);
        assert!(events.len() >= 8);
        for (_, event) in events {
            parser.push(event.as_bytes());
        }
        let metrics = parser.finish();
        assert!(metrics.first_text_ms.is_some());
        assert!(metrics.saw_message_stop);
        assert!(metrics.saw_done);
    }

    #[test]
    fn fake_server_prefers_mcp_ping_over_generic_first_tool() {
        let request = json!({
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "userInputMessageContext": {
                            "tools": [
                                {"toolSpecification": {"name": "mcp__kiro-local-test__fail"}},
                                {"toolSpecification": {"name": "mcp__kiro-local-test__ping"}}
                            ]
                        }
                    }
                }
            }
        });

        assert_eq!(
            select_fake_tool_name(&request),
            "mcp__kiro-local-test__ping"
        );
    }

    #[test]
    fn fake_server_prefers_sanitized_mcp_ping_tool_name() {
        let request = json!({
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "userInputMessageContext": {
                            "tools": [
                                {"toolSpecification": {"name": "mcpKiroLocalTestFailHash29c36f63"}},
                                {"toolSpecification": {"name": "mcpKiroLocalTestPingHash62ce4ea1"}}
                            ]
                        }
                    }
                }
            }
        });

        assert_eq!(
            select_fake_tool_name(&request),
            "mcpKiroLocalTestPingHash62ce4ea1"
        );
    }

    #[test]
    fn real_upstream_guard_requires_env() {
        unsafe {
            std::env::remove_var("KIRO_LOADTEST_ALLOW_REAL_UPSTREAM");
        }
        assert!(ensure_real_upstream_guard(true).is_err());
        assert!(ensure_real_upstream_guard(false).is_ok());
    }

    #[test]
    fn error_id_is_extracted_from_public_message() {
        let id = extract_error_id(
            "The request could not be completed. If this continues, contact the administrator with error ID: err_01abc.",
        );
        assert_eq!(id.as_deref(), Some("err_01abc"));
    }

    #[test]
    fn sse_error_event_exposes_error_id_to_report() {
        let id = error_id_from_sse_data(
            r#"{"type":"error","error":{"type":"api_error","message":"The request could not be completed. If this continues, contact the administrator with error ID: req_01abc."}}"#,
        );
        assert_eq!(id.as_deref(), Some("req_01abc"));
    }

    #[test]
    fn json_exception_body_is_classified_as_error() {
        assert!(body_is_json_exception(
            br#"{"__type":"ThrottlingException","message":"Rate exceeded"}"#
        ));
        assert!(body_is_json_exception(
            br#"{"error":{"type":"api_error","message":"failed"}}"#
        ));
        assert!(!body_is_json_exception(
            br#"{"type":"message","content":[{"type":"text","text":"ok"}]}"#
        ));
    }
}
