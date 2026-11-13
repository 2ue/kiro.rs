//! 外部池流式“提前提交首包”模式。
//!
//! 普通的首输出前重试会一直缓冲到第一个内容事件才提交下游，期间下游收不到任何字节。
//! 提前提交模式下，下游响应头和第一次尝试的 message_start 立即发出，之后每隔保活间隔发
//! ping；首个内容到达前上游出错时仍在外部池内换池重放，新一次尝试的 message_start 被丢弃，
//! 下游看到的始终是同一条消息。
//!
//! 计费约束：发出去的 message_start 里 usage 全部清零。真实用量只在最后的 message_delta
//! 里给出；请求最终失败时以 SSE `error` 事件收尾，下游按“未完成”处理，不会按 message_start
//! 里的输入 token 计费。

use super::*;
use tokio::sync::{mpsc, oneshot};

/// 下游 SSE 通道容量：只承载 message_start、ping 和终止错误这类小事件。
const EARLY_COMMIT_CHANNEL_CAPACITY: usize = 16;

/// 提前提交模式下已打开的下游 SSE 通道。
pub struct ExternalEarlyCommitDownstream {
    sender: mpsc::Sender<Bytes>,
    ready_sender: SyncMutex<Option<oneshot::Sender<EarlyCommitResponseReady>>>,
    /// 已向下游发出 message_start；之后的尝试只能丢弃自己的 message_start。
    message_start_sent: AtomicBool,
    response_committed: AtomicBool,
    /// 上游真实 message_start 中的 usage（未清零），用于补全 message_delta 缺失的输入侧字段。
    upstream_start_usage: SyncMutex<Option<serde_json::Value>>,
    started_at: Instant,
    latency_trace: Arc<ExternalLatencyTraceState>,
}

impl std::fmt::Debug for ExternalEarlyCommitDownstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalEarlyCommitDownstream")
            .field(
                "message_start_sent",
                &self.message_start_sent.load(Ordering::Acquire),
            )
            .finish()
    }
}

impl ExternalEarlyCommitDownstream {
    pub(super) fn new(
        started_at: Instant,
        latency_trace: Arc<ExternalLatencyTraceState>,
        ready_sender: oneshot::Sender<EarlyCommitResponseReady>,
    ) -> (Arc<Self>, mpsc::Receiver<Bytes>) {
        let (sender, receiver) = mpsc::channel(EARLY_COMMIT_CHANNEL_CAPACITY);
        (
            Arc::new(Self {
                sender,
                ready_sender: SyncMutex::new(Some(ready_sender)),
                message_start_sent: AtomicBool::new(false),
                response_committed: AtomicBool::new(false),
                upstream_start_usage: SyncMutex::new(None),
                started_at,
                latency_trace,
            }),
            receiver,
        )
    }

    pub(super) fn message_start_sent(&self) -> bool {
        self.message_start_sent.load(Ordering::Acquire)
    }

    pub(super) fn response_committed(&self) -> bool {
        self.response_committed.load(Ordering::Acquire)
    }

    pub(super) fn mark_response_committed(&self) {
        self.response_committed.store(true, Ordering::Release);
        if let Some(sender) = self.ready_sender.lock().take() {
            let _ = sender.send(EarlyCommitResponseReady::Committed);
        }
    }

    pub(super) fn fail_before_response_commit(&self, error: ExternalPoolFinalError) -> bool {
        self.ready_sender
            .lock()
            .take()
            .is_some_and(|sender| sender.send(EarlyCommitResponseReady::Failed(error)).is_ok())
    }

    /// 清除上一轮上游尝试的 usage，避免最终尝试缺少 message_start usage 时误用旧值。
    pub(super) fn reset_attempt_usage(&self) {
        *self.upstream_start_usage.lock() = None;
    }

    /// 发送一段字节给下游；下游已断开时返回 false。
    pub(super) async fn send(&self, bytes: Bytes) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let sent = self.sender.send(bytes).await.is_ok();
        if sent {
            self.latency_trace
                .mark_downstream_first_byte(self.started_at);
        }
        sent
    }

    pub(super) async fn send_ping(&self) -> bool {
        self.send(Bytes::from_static(EARLY_COMMIT_PING_EVENT)).await
    }

    /// 下游客户端已断开（接收端被丢弃）。
    pub(super) fn downstream_closed(&self) -> bool {
        self.sender.is_closed()
    }

    pub(super) async fn wait_downstream_closed(&self) {
        self.sender.closed().await;
    }

    /// 处理一次尝试的 message_start：第一次发给下游（usage 清零），之后的直接丢弃。
    /// 返回值表示下游是否仍在线。
    pub(super) async fn forward_message_start(&self, event: &[u8]) -> bool {
        if let Some(usage) = sse_event_message_start_usage(event) {
            // 每次尝试都覆盖：message_delta 只来自最终成功的那次尝试，应按它的 message_start 补全。
            *self.upstream_start_usage.lock() = Some(usage);
        }
        if self
            .message_start_sent
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return !self.downstream_closed();
        }
        let sent = self
            .send(Bytes::from(zero_message_start_usage(event)))
            .await;
        if sent {
            self.mark_response_committed();
        }
        sent
    }

    /// 发送终止错误事件。下游已断开时静默忽略。
    pub(super) async fn send_terminal_error(&self, error_id: &str, request_id: &str) {
        let message = envelope::public_message_with_error_id(
            envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
            error_id,
        );
        let body = json!({
            "type": "error",
            "error": {
                "type": "api_error",
                "message": message,
            },
            "request_id": request_id,
        });
        let event = format!(
            "event: error\ndata: {}\n\n",
            serde_json::to_string(&body).unwrap_or_default()
        );
        let _ = self.send(Bytes::from(event)).await;
    }

    pub(super) fn upstream_start_usage(&self) -> Option<serde_json::Value> {
        self.upstream_start_usage.lock().clone()
    }
}

const EARLY_COMMIT_PING_EVENT: &[u8] = b"event: ping\ndata: {\"type\": \"ping\"}\n\n";

pub(super) fn sse_event_is_message_start(event: &[u8]) -> bool {
    if external_sse_event_name(event).as_deref() == Some("message_start") {
        return true;
    }
    sse_event_data_json(event)
        .is_some_and(|value| value.get("type").and_then(|v| v.as_str()) == Some("message_start"))
}

fn sse_event_data_json(event: &[u8]) -> Option<serde_json::Value> {
    let text = std::str::from_utf8(event).ok()?;
    text.lines().find_map(|line| {
        let data = line.trim_end_matches('\r').strip_prefix("data:")?.trim();
        (!data.is_empty() && data != "[DONE]")
            .then(|| serde_json::from_str::<serde_json::Value>(data).ok())
            .flatten()
    })
}

fn sse_event_message_start_usage(event: &[u8]) -> Option<serde_json::Value> {
    sse_event_data_json(event)?
        .get("message")?
        .get("usage")
        .filter(|usage| usage.is_object())
        .cloned()
}

/// 把 message_start 中所有数值型 usage 字段（含 cache_creation 明细）清零，保留其他字段原样。
/// 无法解析时原样返回：上游 message_start 没有 usage 时也就没有需要清零的计费数据。
pub(super) fn zero_message_start_usage(event: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(event) else {
        return event.to_vec();
    };
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        let Some(data) = body.strip_prefix("data:") else {
            out.push_str(line);
            continue;
        };
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
            out.push_str(line);
            continue;
        };
        let Some(usage) = value
            .get_mut("message")
            .and_then(|message| message.get_mut("usage"))
        else {
            out.push_str(line);
            continue;
        };
        zero_usage_numbers(usage);
        changed = true;
        out.push_str("data: ");
        out.push_str(&serde_json::to_string(&value).unwrap_or_else(|_| data.trim().to_string()));
        out.push_str(ending);
    }
    if changed {
        out.into_bytes()
    } else {
        event.to_vec()
    }
}

fn zero_usage_numbers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(_) => *value = json!(0),
        serde_json::Value::Object(map) => map.values_mut().for_each(zero_usage_numbers),
        _ => {}
    }
}
/// message_delta 的 usage 中缺失的输入侧字段用上游真实 message_start 的值补全。
///
/// 下游（如 sub2api）只用 message_delta 里大于 0 的字段覆盖 message_start 的值；
/// message_start 已被清零，若 message_delta 不带输入侧字段，下游会把输入按 0 计费。
/// 只补“缺失”的字段，上游 message_delta 自己给出的值（包括 0）原样保留。
pub(super) fn backfill_message_delta_usage(
    event: &[u8],
    start_usage: &serde_json::Value,
) -> Option<Vec<u8>> {
    const INPUT_SIDE_FIELDS: [&str; 4] = [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "cache_creation",
    ];
    let start = start_usage.as_object()?;
    let text = std::str::from_utf8(event).ok()?;
    let mut changed = false;
    let mut out = String::with_capacity(text.len() + 128);
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\r', '\n']);
        let ending = &line[body.len()..];
        let parsed = body
            .strip_prefix("data:")
            .and_then(|data| serde_json::from_str::<serde_json::Value>(data.trim()).ok())
            .filter(|value| value.get("type").and_then(|v| v.as_str()) == Some("message_delta"));
        let Some(mut value) = parsed else {
            out.push_str(line);
            continue;
        };
        let usage = value
            .as_object_mut()
            .map(|object| object.entry("usage").or_insert_with(|| json!({})));
        let Some(usage) = usage.and_then(|usage| usage.as_object_mut()) else {
            out.push_str(line);
            continue;
        };
        let mut line_changed = false;
        for field in INPUT_SIDE_FIELDS {
            if usage.contains_key(field) {
                continue;
            }
            if let Some(start_value) = start.get(field) {
                usage.insert(field.to_string(), start_value.clone());
                line_changed = true;
            }
        }
        if !line_changed {
            out.push_str(line);
            continue;
        }
        changed = true;
        out.push_str("data: ");
        out.push_str(&serde_json::to_string(&value).ok()?);
        out.push_str(ending);
    }
    changed.then(|| out.into_bytes())
}

/// 提前提交模式下首个内容前的读取结果。
pub(super) enum EarlyCommitPreRead {
    /// 已遇到第一个内容事件（或终止事件），`prefix` 中是需要随后转发的字节。
    Committed(Instant),
    /// 下游已断开，不再继续读上游。
    DownstreamGone,
}

pub(super) enum EarlyCommitResponseReady {
    Committed,
    Failed(ExternalPoolFinalError),
}

pub(super) struct EarlyCommitTaskGuard(Option<tokio::task::JoinHandle<()>>);

impl EarlyCommitTaskGuard {
    pub(super) fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(task))
    }

    pub(super) fn detach(&mut self) {
        self.0.take();
    }
}

impl Drop for EarlyCommitTaskGuard {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// 提前提交模式下首个内容前的读取：message_start 交给下游通道（清零或丢弃），ping 由本函数
/// 按保活间隔补发；首个内容前的错误事件、读错误、EOF、空闲超时都返回可换池的错误。
/// 与普通模式不同，这里不设“首个内容最长等待”：只要上游还在发 ping 或数据就一直等待，
/// 避免放弃已在生成的尝试后上游仍按完整请求计费。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::result_large_err)]
pub(super) async fn pre_read_early_commit<S>(
    downstream: &ExternalEarlyCommitDownstream,
    body_stream: &mut S,
    buffer: &mut Vec<u8>,
    prefix: &mut Vec<u8>,
    lease: &ExternalPoolLease,
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    config: &ExternalPoolsConfig,
    projection_context: Option<&ExternalUsageProjectionContext>,
    usage_capture: &Arc<SyncMutex<ExternalUsageCapture>>,
    stream_plan: ExternalStreamProcessingPlan,
    stream_error_mask: &ExternalStreamErrorMask,
    transcript_state: &Arc<SyncMutex<ExternalAnthropicTranscriptState>>,
    stream_idle_timeout: Option<Duration>,
    outbound_model: &Option<String>,
    upstream_chunk_index: Option<&AtomicU64>,
) -> Result<EarlyCommitPreRead, ExternalForwardError>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    let mut last_chunk_at = Instant::now();
    loop {
        while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
            let end = idx + delimiter_len;
            let event = buffer.drain(..end).collect::<Vec<u8>>();
            if external_sse_event_is_error_event(&event) {
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    "external pool early-commit stream emitted error event before semantic output; failing over"
                );
                return Err(external_pre_output_stream_protocol_error(
                    SAFE_EXTERNAL_STREAM_ERROR_EVENT,
                    "stream_pre_output_error_event",
                    config,
                    outbound_model.clone(),
                ));
            }
            if sse_event_is_message_start(&event) {
                // 走一遍常规处理以更新用量捕获和转写状态，但发给下游的是清零版本。
                let processed = process_sse_event_with_plan_and_transcript(
                    &event,
                    projection_context,
                    Some(usage_capture),
                    Some(stream_error_mask),
                    stream_plan,
                    Some(&mut transcript_state.lock()),
                );
                if !downstream.forward_message_start(&processed).await {
                    return Ok(EarlyCommitPreRead::DownstreamGone);
                }
                continue;
            }
            if external_sse_event_name(&event).as_deref() == Some("ping") {
                // 上游 ping 只说明连接活着；下游保活由本函数按固定间隔发送，避免重复。
                continue;
            }
            let commits_downstream = external_sse_event_commits_pre_output_stream(&event);
            let processed = process_sse_event_with_plan_and_transcript(
                &event,
                projection_context,
                Some(usage_capture),
                Some(stream_error_mask),
                stream_plan,
                Some(&mut transcript_state.lock()),
            );
            prefix.extend(
                downstream
                    .upstream_start_usage()
                    .and_then(|start| backfill_message_delta_usage(&processed, &start))
                    .unwrap_or(processed),
            );
            if prefix.len() > EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES {
                return Err(external_pre_output_stream_protocol_error(
                    "external pre-output stream buffer exceeded limit",
                    "stream_pre_output_buffer_limit",
                    config,
                    outbound_model.clone(),
                ));
            }
            if commits_downstream {
                downstream.mark_response_committed();
                return Ok(EarlyCommitPreRead::Committed(last_chunk_at));
            }
        }

        if buffer.len() > EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES {
            return Err(external_pre_output_stream_protocol_error(
                "external pre-output stream event exceeded buffer limit",
                "stream_pre_output_buffer_limit",
                config,
                outbound_model.clone(),
            ));
        }

        tokio::select! {
            chunk = body_stream.next() => {
                if let Some(chunk) = early_commit_upstream_chunk(chunk, route, pool, config, outbound_model)? {
                    record_external_upstream_chunk(Some(&route.request_id), pool.id, upstream_chunk_index, &chunk);
                    route.latency_trace.mark_first_upstream_chunk(route.started_at);
                    last_chunk_at = Instant::now();
                    buffer.extend_from_slice(&chunk);
                }
            }
            _ = lease.wait_until_lost() => {
                return Err(external_pool_lease_lost_forward_error(outbound_model.clone()));
            }
            _ = downstream.wait_downstream_closed() => {
                return Ok(EarlyCommitPreRead::DownstreamGone);
            }
            _ = external_pool_stream_idle_deadline(last_chunk_at, stream_idle_timeout) => {
                return Err(early_commit_idle_error(route, pool, config, stream_idle_timeout, outbound_model));
            }
        }
    }
}

/// 读取一次上游分片：成功返回字节，读错误或提前结束都转成可换池的错误。
#[allow(clippy::result_large_err)]
fn early_commit_upstream_chunk(
    chunk: Option<Result<Bytes, reqwest::Error>>,
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    config: &ExternalPoolsConfig,
    outbound_model: &Option<String>,
) -> Result<Option<Bytes>, ExternalForwardError> {
    match chunk {
        Some(Ok(chunk)) => Ok(Some(chunk)),
        Some(Err(err)) => {
            let message = sanitized_external_network_error("stream pre-output read failed", &err);
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                pool_id = pool.id,
                pool_name = %pool.name,
                error_class = %message,
                "external pool early-commit stream read failed before semantic output; failing over"
            );
            Err(external_pre_output_stream_network_error(
                message,
                config,
                outbound_model.clone(),
            ))
        }
        None => {
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                pool_id = pool.id,
                pool_name = %pool.name,
                "external pool early-commit stream ended before semantic output; failing over"
            );
            Err(external_pre_output_stream_protocol_error(
                "external upstream ended stream before semantic output",
                "stream_pre_output_eof",
                config,
                outbound_model.clone(),
            ))
        }
    }
}

fn early_commit_idle_error(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    config: &ExternalPoolsConfig,
    stream_idle_timeout: Option<Duration>,
    outbound_model: &Option<String>,
) -> ExternalForwardError {
    let seconds = stream_idle_timeout
        .map(|timeout| timeout.as_secs())
        .unwrap_or_default();
    tracing::warn!(
        request_id = %route.request_id,
        error_id = %route.error_id,
        pool_id = pool.id,
        pool_name = %pool.name,
        idle_timeout_secs = seconds,
        "external pool early-commit stream idled before semantic output; failing over"
    );
    external_pre_output_stream_network_error(
        format!(
            "model endpoint stream idle timeout before semantic output after {seconds} seconds"
        ),
        config,
        outbound_model.clone(),
    )
}

/// 后台换池任务的结果：成功时是提交后的上游流，失败时终止错误事件已写入通道。
pub(super) enum EarlyCommitTail {
    Stream(futures::stream::BoxStream<'static, Result<Bytes, axum::Error>>),
    Done,
}

/// 下游响应：先转发通道里的 message_start / ping，后台任务交回上游流后接着转发它。
/// 响应头只带 SSE 必需的字段，上游的限流类响应头此时还不存在。
pub(super) fn early_commit_response(
    request_id: &str,
    receiver: mpsc::Receiver<Bytes>,
    tail: tokio::sync::oneshot::Receiver<EarlyCommitTail>,
) -> Response {
    enum Phase {
        Channel(
            mpsc::Receiver<Bytes>,
            tokio::sync::oneshot::Receiver<EarlyCommitTail>,
        ),
        Tail(futures::stream::BoxStream<'static, Result<Bytes, axum::Error>>),
        Finished,
    }
    let stream = futures::stream::unfold(Phase::Channel(receiver, tail), |phase| async move {
        let mut phase = phase;
        loop {
            phase = match phase {
                Phase::Channel(mut receiver, mut tail) => {
                    tokio::select! {
                        biased;
                        bytes = receiver.recv() => match bytes {
                            Some(bytes) => {
                                return Some((Ok::<Bytes, axum::Error>(bytes), Phase::Channel(receiver, tail)));
                            }
                            // 发送端全部释放：后台任务已结束，余下内容（若有）在 tail 里。
                            None => match tail.await {
                                Ok(EarlyCommitTail::Stream(stream)) => Phase::Tail(stream),
                                Ok(EarlyCommitTail::Done) | Err(_) => Phase::Finished,
                            },
                        },
                        next = &mut tail => {
                            // 先把通道里已排队的 message_start / ping / 错误事件发完，再接上游流，
                            // 保证下游看到的事件顺序与产生顺序一致。
                            let mut pending = Vec::new();
                            while let Ok(bytes) = receiver.try_recv() {
                                pending.extend_from_slice(&bytes);
                            }
                            let next_phase = match next {
                                Ok(EarlyCommitTail::Stream(stream)) => Phase::Tail(stream),
                                Ok(EarlyCommitTail::Done) | Err(_) => Phase::Finished,
                            };
                            if !pending.is_empty() {
                                return Some((Ok(Bytes::from(pending)), next_phase));
                            }
                            next_phase
                        }
                    }
                }
                Phase::Tail(mut stream) => {
                    return stream.next().await.map(|item| (item, Phase::Tail(stream)));
                }
                Phase::Finished => return None,
            };
        }
    });
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache");
    if let Some(headers) = builder.headers_mut() {
        envelope::insert_request_id_headers(headers, request_id);
        headers.insert(
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        );
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn sse(event: &str, data: serde_json::Value) -> Vec<u8> {
        format!("event: {event}\ndata: {}\n\n", data).into_bytes()
    }

    #[test]
    fn zero_message_start_usage_zeroes_every_number_and_keeps_the_rest() {
        let event = sse(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_1",
                    "model": "claude-sonnet-4-6",
                    "usage": {
                        "input_tokens": 1234,
                        "output_tokens": 1,
                        "cache_read_input_tokens": 50000,
                        "cache_creation_input_tokens": 700,
                        "cache_creation": {
                            "ephemeral_5m_input_tokens": 700,
                            "ephemeral_1h_input_tokens": 0
                        },
                        "service_tier": "standard"
                    }
                }
            }),
        );
        let zeroed = zero_message_start_usage(&event);
        let text = String::from_utf8(zeroed).unwrap();
        assert!(text.starts_with("event: message_start\n"));
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(data).unwrap();
        let usage = &value["message"]["usage"];
        assert_eq!(usage["input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 0);
        assert_eq!(usage["cache_read_input_tokens"], 0);
        assert_eq!(usage["cache_creation_input_tokens"], 0);
        assert_eq!(usage["cache_creation"]["ephemeral_5m_input_tokens"], 0);
        assert_eq!(usage["service_tier"], "standard");
        assert_eq!(value["message"]["id"], "msg_1");
        assert!(text.ends_with("\n\n"));
    }

    #[test]
    fn message_delta_backfill_only_fills_missing_input_side_fields() {
        let start = json!({
            "input_tokens": 1234,
            "cache_read_input_tokens": 50000,
            "cache_creation_input_tokens": 700,
            "output_tokens": 1
        });
        let delta = sse(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": 42, "cache_read_input_tokens": 0}
            }),
        );
        let filled = backfill_message_delta_usage(&delta, &start).expect("backfilled");
        let text = String::from_utf8(filled).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(data).unwrap();
        let usage = &value["usage"];
        assert_eq!(usage["input_tokens"], 1234);
        assert_eq!(usage["cache_creation_input_tokens"], 700);
        // 上游 message_delta 自己给出的值保持不变，即使是 0。
        assert_eq!(usage["cache_read_input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 42);
    }

    #[test]
    fn message_delta_backfill_leaves_complete_usage_untouched() {
        let start = json!({"input_tokens": 9, "cache_read_input_tokens": 3});
        let delta = sse(
            "message_delta",
            json!({
                "type": "message_delta",
                "usage": {"input_tokens": 9, "cache_read_input_tokens": 3, "output_tokens": 2}
            }),
        );
        assert!(backfill_message_delta_usage(&delta, &start).is_none());
        let other = sse("content_block_stop", json!({"type": "content_block_stop"}));
        assert!(backfill_message_delta_usage(&other, &start).is_none());
    }

    #[test]
    fn message_start_detection_accepts_named_and_unnamed_events() {
        assert!(sse_event_is_message_start(&sse(
            "message_start",
            json!({"type": "message_start"})
        )));
        assert!(sse_event_is_message_start(
            b"data: {\"type\":\"message_start\",\"message\":{}}\n\n"
        ));
        assert!(!sse_event_is_message_start(&sse(
            "ping",
            json!({"type": "ping"})
        )));
    }

    #[tokio::test]
    async fn only_the_first_message_start_reaches_downstream() {
        let latency = Arc::new(ExternalLatencyTraceState::default());
        let (ready_sender, _ready_receiver) = tokio::sync::oneshot::channel();
        let (downstream, mut receiver) =
            ExternalEarlyCommitDownstream::new(Instant::now(), latency, ready_sender);
        let first = sse(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_a", "usage": {"input_tokens": 10}}}),
        );
        let second = sse(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_b", "usage": {"input_tokens": 20}}}),
        );
        assert!(downstream.forward_message_start(&first).await);
        assert!(downstream.forward_message_start(&second).await);
        let sent = receiver.recv().await.expect("first message_start");
        let text = String::from_utf8(sent.to_vec()).unwrap();
        assert!(text.contains("msg_a"));
        assert!(text.contains("\"input_tokens\":0"));
        assert!(
            receiver.try_recv().is_err(),
            "second message_start must be dropped"
        );
        // 补全 message_delta 时用最后一次尝试上游的真实值（成功的那次）。
        assert_eq!(
            downstream.upstream_start_usage().unwrap()["input_tokens"],
            20
        );
        downstream.reset_attempt_usage();
        assert!(downstream.upstream_start_usage().is_none());
    }
}
