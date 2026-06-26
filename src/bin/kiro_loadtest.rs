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
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{SecondsFormat, Utc};
use clap::{Parser, ValueEnum};
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
    #[arg(long, default_value_t = 1500)]
    fake_delay_ms: u64,
    #[arg(long, default_value_t = 10)]
    fake_recover_after: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    NormalStream,
    NormalNonStream,
    SlowFirstByte,
    SlowThinkingThenText,
    StreamIdleTimeout,
    JsonException200,
    RateLimit429,
    ServerError500,
    InvalidToolFormat,
    MalformedSse,
    ClientDrop,
    RecoveryAfterBurst,
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
    timeout_secs: u64,
    auth_key: Option<String>,
    target_pid: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoadtestReport {
    scenario: Scenario,
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
    request_ids: Vec<String>,
    error_ids: Vec<String>,
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
}

#[derive(Clone)]
struct FakeServerState {
    scenario: Scenario,
    delay: Duration,
    recover_after: u64,
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
            error_id,
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
    let mut body = json!({
        "model": config.model,
        "max_tokens": 256,
        "stream": config.stream,
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": format!("loadtest request {index}: respond with a short sentence")
            }]
        }]
    });

    if config.thinking {
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": 1024
        });
    }

    if config.cache_control {
        body["system"] = json!([{
            "type": "text",
            "text": "stable loadtest system prompt",
            "cache_control": {"type": "ephemeral"}
        }]);
    }

    if config.tool_use {
        body["tools"] = json!([{
            "name": "echo",
            "description": "Return the provided text.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"}
                },
                "required": ["text"]
            }
        }]);
    }

    body
}

#[derive(Debug, Default)]
struct ParsedSseMetrics {
    first_thinking_ms: Option<u128>,
    first_text_ms: Option<u128>,
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
        request_ids,
        error_ids,
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
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = format!(
        "fake_req_{}",
        state.counter.fetch_add(1, Ordering::Relaxed) + 1
    );
    let request = serde_json::from_slice::<Value>(&body).unwrap_or_else(|_| json!({}));
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| wants_stream(&headers));
    let scenario = effective_fake_scenario(&state);

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
                sse_response(
                    request_id,
                    vec![(
                        state.delay,
                        "data: {\"type\":\"message_start\"}\n\n".to_string(),
                    )],
                )
            } else {
                sleep(state.delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::SlowFirstByte => {
            if stream {
                sse_response(request_id, normal_sse_events(state.delay, false))
            } else {
                sleep(state.delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::SlowThinkingThenText => {
            if stream {
                sse_response(request_id, thinking_sse_events(state.delay))
            } else {
                sleep(state.delay).await;
                fake_json_message(&request_id)
            }
        }
        Scenario::NormalNonStream => fake_json_message(&request_id),
        Scenario::NormalStream | Scenario::ClientDrop | Scenario::RecoveryAfterBurst => {
            if stream {
                sse_response(request_id, normal_sse_events(Duration::ZERO, false))
            } else {
                fake_json_message(&request_id)
            }
        }
    }
}

fn effective_fake_scenario(state: &FakeServerState) -> Scenario {
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

fn wants_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"))
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
