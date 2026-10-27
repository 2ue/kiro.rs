//! Targeted request capture for offline replay.
//!
//! The full LLM capture (`diagnostics::capture`) records every request and fills its budget within
//! hours on a busy instance. This recorder keeps only the requests worth replaying:
//!
//! - `usage_only`: a local request whose upstream kept answering with usage/metadata only (an
//!   empty turn) until the pre-output retries ran out. The record holds the client body as
//!   received and the body sent to Kiro, so the turn can be replayed against a test instance.
//! - `tool_choice`: a request that set `tool_choice` (or `disable_parallel_tool_use`). The record
//!   holds the text and tool calls the client received, and flags replies that mention the
//!   injected policy, so leaks of the injected wording can be checked on real traffic.
//!
//! Disabled by default. Each record is one JSON file under `dir`; files beyond `maxFiles` are
//! deleted oldest first, and every body is cut at `maxBodyBytes`. A dedicated writer thread does
//! the file work, so the request never waits on disk; when the queue is full the record is dropped.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
};

use chrono::Utc;
use serde_json::{Value, json};

use crate::model::config::RequestCaptureConfig;

use super::stream::SseEvent;

const CHANNEL_CAPACITY: usize = 64;
const FILE_PREFIX: &str = "capture-";
const FILE_SUFFIX: &str = ".json";

/// Phrases that show the model talking about the injected tool_choice wording instead of just
/// following it. Matched case-insensitively against the reply the client received; a match only
/// flags the record for review.
const TOOL_CHOICE_LEAK_MARKERS: &[&str] = &[
    "tool_choice",
    "tool choice",
    "system-reminder",
    "system reminder",
    "prompt injection",
    "injected instruction",
    "cannot call any tool",
    "can't call any tool",
    "unable to call any tool",
    "not allowed to call",
    "不能调用任何工具",
    "无法调用工具",
    "提示词注入",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureKind {
    UsageOnly,
    ToolChoice,
}

impl CaptureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::UsageOnly => "usage_only",
            Self::ToolChoice => "tool_choice",
        }
    }

    fn subdir(self) -> &'static str {
        match self {
            Self::UsageOnly => "usage-only",
            Self::ToolChoice => "tool-choice",
        }
    }
}

/// Request facts kept while capture is enabled, built when the request enters the local path.
#[derive(Debug)]
pub(crate) struct RequestCaptureSeed {
    dir: PathBuf,
    max_files: u32,
    max_body_bytes: usize,
    capture_usage_only: bool,
    client_body: bytes::Bytes,
    /// Body sent to Kiro on the first attempt (cut at `max_body_bytes`); usage-only records only.
    kiro_body: Option<(Arc<str>, bool)>,
    /// `tool_choice` of the request when tool_choice records are wanted for it.
    tool_choice: Option<Value>,
    injections: Vec<&'static str>,
}

/// Identifies the request in a capture record.
pub(crate) struct CaptureRequestMeta<'a> {
    pub(crate) request_id: &'a str,
    pub(crate) conversation_id: Option<&'a str>,
    pub(crate) endpoint: &'a str,
    pub(crate) stream: bool,
    pub(crate) model: &'a str,
    pub(crate) upstream_model: Option<&'a str>,
}

/// What the client received, after every output filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CapturedReply {
    pub(crate) text: String,
    pub(crate) text_truncated: bool,
    pub(crate) tool_uses: Vec<String>,
}

impl RequestCaptureSeed {
    /// Returns `None` when capture is off or nothing would be recorded for this request.
    pub(crate) fn new(
        config: &RequestCaptureConfig,
        client_body: &bytes::Bytes,
        kiro_body: &str,
        tool_choice: Option<&Value>,
        tool_choice_injected: bool,
        injections: Vec<&'static str>,
    ) -> Option<Arc<Self>> {
        let config = config.normalized();
        let capture_tool_choice = config.tool_choice && tool_choice_injected;
        if !config.enabled || (!config.usage_only && !capture_tool_choice) {
            return None;
        }
        let kiro_body = config.usage_only.then(|| {
            let (body, truncated) = truncate_utf8(kiro_body, config.max_body_bytes);
            (Arc::<str>::from(body), truncated)
        });
        Some(Arc::new(Self {
            dir: PathBuf::from(config.dir),
            max_files: config.max_files,
            max_body_bytes: config.max_body_bytes,
            capture_usage_only: config.usage_only,
            client_body: client_body.clone(),
            kiro_body,
            tool_choice: capture_tool_choice.then(|| tool_choice.cloned().unwrap_or(Value::Null)),
            injections,
        }))
    }

    pub(crate) fn wants_tool_choice(&self) -> bool {
        self.tool_choice.is_some()
    }

    /// Largest reply text kept for a tool_choice record.
    pub(crate) fn max_reply_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// Records a request that stayed usage-only until the pre-output retries ran out.
    pub(crate) fn record_usage_only(&self, meta: &CaptureRequestMeta<'_>, attempts: u32) {
        if !self.capture_usage_only {
            return;
        }
        let (client_body, client_body_truncated) = self.client_body_value();
        let (kiro_body, kiro_body_truncated) = match &self.kiro_body {
            Some((body, truncated)) => (body_value(body.as_bytes()), *truncated),
            None => (Value::Null, false),
        };
        self.submit(
            CaptureKind::UsageOnly,
            meta,
            json!({
                "attempts": attempts,
                "injections": self.injections,
                "clientBody": client_body,
                "clientBodyTruncated": client_body_truncated,
                "kiroBody": kiro_body,
                "kiroBodyTruncated": kiro_body_truncated,
            }),
        );
    }

    /// Records the reply of a request that set `tool_choice`.
    pub(crate) fn record_tool_choice(
        &self,
        meta: &CaptureRequestMeta<'_>,
        reply: &CapturedReply,
        stop_reason: Option<&str>,
        violation: Option<&str>,
    ) {
        let Some(tool_choice) = self.tool_choice.as_ref() else {
            return;
        };
        let (client_body, client_body_truncated) = self.client_body_value();
        self.submit(
            CaptureKind::ToolChoice,
            meta,
            json!({
                "toolChoice": tool_choice,
                "injections": self.injections,
                "stopReason": stop_reason,
                "toolChoiceViolation": violation,
                "replyText": reply.text,
                "replyTextTruncated": reply.text_truncated,
                "replyToolUses": reply.tool_uses,
                "leakMarkers": tool_choice_leak_markers(&reply.text),
                "clientBody": client_body,
                "clientBodyTruncated": client_body_truncated,
            }),
        );
    }

    fn client_body_value(&self) -> (Value, bool) {
        let truncated = self.client_body.len() > self.max_body_bytes;
        if truncated {
            let text = String::from_utf8_lossy(&self.client_body);
            let (text, _) = truncate_utf8(&text, self.max_body_bytes);
            (Value::String(text.to_string()), true)
        } else {
            (body_value(&self.client_body), false)
        }
    }

    fn submit(&self, kind: CaptureKind, meta: &CaptureRequestMeta<'_>, details: Value) {
        let Some(writer) = writer() else {
            return;
        };
        let mut record = json!({
            "kind": kind.as_str(),
            "capturedAt": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "version": env!("CARGO_PKG_VERSION"),
            "requestId": meta.request_id,
            "conversationId": meta.conversation_id,
            "endpoint": meta.endpoint,
            "stream": meta.stream,
            "model": meta.model,
            "upstreamModel": meta.upstream_model,
        });
        if let (Value::Object(record), Value::Object(details)) = (&mut record, details) {
            record.extend(details);
        }
        writer.submit(CaptureJob {
            dir: self.dir.join(kind.subdir()),
            max_files: self.max_files,
            file_name: format!(
                "{FILE_PREFIX}{}-{}{FILE_SUFFIX}",
                Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
                sanitize_file_component(meta.request_id)
            ),
            record,
        });
    }
}

/// Accumulates the text and tool calls of a streamed reply as the client receives them.
#[derive(Debug, Clone)]
pub(crate) struct DownstreamTextCapture {
    max_bytes: usize,
    reply: CapturedReply,
}

impl DownstreamTextCapture {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            reply: CapturedReply::default(),
        }
    }

    pub(crate) fn observe(&mut self, events: &[SseEvent]) {
        for event in events {
            match event.data.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => {
                    if let Some(text) = event
                        .data
                        .pointer("/delta/text")
                        .and_then(Value::as_str)
                        .filter(|_| {
                            event.data.pointer("/delta/type").and_then(Value::as_str)
                                == Some("text_delta")
                        })
                    {
                        self.push_text(text);
                    }
                }
                Some("content_block_start") => {
                    let block = &event.data["content_block"];
                    if block.get("type").and_then(Value::as_str) == Some("tool_use")
                        && let Some(name) = block.get("name").and_then(Value::as_str)
                    {
                        self.reply.tool_uses.push(name.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.reply.text_truncated {
            return;
        }
        let remaining = self.max_bytes.saturating_sub(self.reply.text.len());
        let (kept, truncated) = truncate_utf8(text, remaining);
        self.reply.text.push_str(kept);
        self.reply.text_truncated = truncated;
    }

    pub(crate) fn reply(&self) -> &CapturedReply {
        &self.reply
    }
}

/// Text and tool calls of a complete non-streaming content array.
pub(crate) fn reply_from_content(content: &[Value], max_bytes: usize) -> CapturedReply {
    let mut capture = DownstreamTextCapture::new(max_bytes);
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    capture.push_text(text);
                }
            }
            Some("tool_use") => {
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    capture.reply.tool_uses.push(name.to_string());
                }
            }
            _ => {}
        }
    }
    capture.reply
}

/// Leak markers found in a reply (lower-cased marker strings, in table order).
pub(crate) fn tool_choice_leak_markers(text: &str) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    TOOL_CHOICE_LEAK_MARKERS
        .iter()
        .copied()
        .filter(|marker| lower.contains(marker))
        .collect()
}

fn truncate_utf8(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

fn body_value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

fn sanitize_file_component(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}

struct CaptureJob {
    dir: PathBuf,
    max_files: u32,
    file_name: String,
    record: Value,
}

struct CaptureWriter {
    tx: SyncSender<CaptureJob>,
    dropped: AtomicU64,
}

impl CaptureWriter {
    fn submit(&self, job: CaptureJob) {
        if self.tx.try_send(job).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(dropped, "请求诊断抓取队列已满，丢弃本条记录");
        }
    }
}

static WRITER: OnceLock<Option<CaptureWriter>> = OnceLock::new();

/// The process-wide writer thread, started on first use. It owns its own thread (like the full
/// LLM capture writer) so it outlives any single Tokio runtime and never blocks request tasks.
fn writer() -> Option<&'static CaptureWriter> {
    WRITER
        .get_or_init(|| {
            let (tx, rx) = sync_channel(CHANNEL_CAPACITY);
            match std::thread::Builder::new()
                .name("request-capture-writer".to_string())
                .spawn(move || writer_loop(rx))
            {
                Ok(_) => Some(CaptureWriter {
                    tx,
                    dropped: AtomicU64::new(0),
                }),
                Err(err) => {
                    tracing::warn!("启动请求诊断抓取写线程失败，抓取关闭: {}", err);
                    None
                }
            }
        })
        .as_ref()
}

fn writer_loop(rx: Receiver<CaptureJob>) {
    while let Ok(job) = rx.recv() {
        if let Err(err) = write_job(&job) {
            tracing::warn!(dir = %job.dir.display(), "写入请求诊断抓取失败: {}", err);
        }
    }
}

fn write_job(job: &CaptureJob) -> std::io::Result<()> {
    create_private_dir(&job.dir)?;
    let bytes = serde_json::to_vec_pretty(&job.record).map_err(std::io::Error::other)?;
    write_private_file(&job.dir.join(&job.file_name), &bytes)?;
    prune_oldest(&job.dir, job.max_files as usize)
}

/// Keeps at most `max_files` capture files in `dir`, deleting the oldest by name (names start
/// with the capture timestamp). Only files this module wrote are touched.
fn prune_oldest(dir: &Path, max_files: usize) -> std::io::Result<()> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX) {
            names.push(name);
        }
    }
    if names.len() <= max_files {
        return Ok(());
    }
    names.sort();
    for name in &names[..names.len() - max_files] {
        if let Err(err) = std::fs::remove_file(dir.join(name))
            && err.kind() != std::io::ErrorKind::NotFound
        {
            return Err(err);
        }
    }
    Ok(())
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> RequestCaptureConfig {
        RequestCaptureConfig {
            enabled: true,
            dir: dir.display().to_string(),
            usage_only: true,
            tool_choice: true,
            max_files: 2,
            max_body_bytes: 1024,
        }
    }

    fn meta(request_id: &str) -> CaptureRequestMeta<'_> {
        CaptureRequestMeta {
            request_id,
            conversation_id: Some("conv"),
            endpoint: "/v1/messages",
            stream: true,
            model: "claude-opus-5-5",
            upstream_model: Some("claude-opus-5.5"),
        }
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "kiro-request-capture-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    async fn wait_for_files(dir: &Path, count: usize) -> Vec<PathBuf> {
        for _ in 0..200 {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let mut files: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
                if files.len() == count {
                    files.sort();
                    return files;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("expected {count} capture files in {}", dir.display());
    }

    #[test]
    fn disabled_or_unneeded_capture_builds_no_seed() {
        let dir = temp_dir();
        let body = bytes::Bytes::from_static(b"{}");
        let mut off = config(&dir);
        off.enabled = false;
        assert!(RequestCaptureSeed::new(&off, &body, "{}", None, true, vec![]).is_none());

        let mut tool_choice_only = config(&dir);
        tool_choice_only.usage_only = false;
        // No tool_choice instruction was injected, so nothing would be recorded.
        assert!(
            RequestCaptureSeed::new(&tool_choice_only, &body, "{}", None, false, vec![]).is_none()
        );
        let seed =
            RequestCaptureSeed::new(&tool_choice_only, &body, "{}", None, true, vec![]).unwrap();
        assert!(seed.wants_tool_choice());
    }

    #[tokio::test]
    async fn usage_only_record_keeps_both_bodies_and_prunes_old_files() {
        let dir = temp_dir();
        let client = bytes::Bytes::from_static(br#"{"model":"claude-opus-5-5","messages":[]}"#);
        let seed = RequestCaptureSeed::new(
            &config(&dir),
            &client,
            r#"{"conversationState":{}}"#,
            None,
            false,
            vec!["identityPrompt"],
        )
        .unwrap();
        for id in ["req_a", "req_b", "req_c"] {
            seed.record_usage_only(&meta(id), 10);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let files = wait_for_files(&dir.join("usage-only"), 2).await;
        let newest: Value = serde_json::from_slice(&std::fs::read(&files[1]).unwrap()).unwrap();
        assert_eq!(newest["kind"], "usage_only");
        assert_eq!(newest["requestId"], "req_c");
        assert_eq!(newest["attempts"], 10);
        assert_eq!(newest["clientBody"]["model"], "claude-opus-5-5");
        assert_eq!(newest["kiroBody"]["conversationState"], json!({}));
        assert_eq!(newest["injections"], json!(["identityPrompt"]));
        assert!(
            files
                .iter()
                .all(|file| !file.to_string_lossy().contains("req_a"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn oversized_client_body_is_cut_and_flagged() {
        let dir = temp_dir();
        // 1024 bytes is the smallest body limit; a multi-byte character straddles the cut.
        let client = bytes::Bytes::from(format!(r#"{{"text":"a{}"}}"#, "界".repeat(400)));
        let seed =
            RequestCaptureSeed::new(&config(&dir), &client, "{}", None, false, vec![]).unwrap();
        seed.record_usage_only(&meta("req_big"), 6);
        let files = wait_for_files(&dir.join("usage-only"), 1).await;
        let record: Value = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        assert_eq!(record["clientBodyTruncated"], true);
        let kept = record["clientBody"].as_str().unwrap();
        assert!(
            kept.len() <= 1024 && kept.len() > 1000,
            "kept {} bytes",
            kept.len()
        );
        assert_eq!(record["kiroBodyTruncated"], false);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn streamed_reply_is_collected_from_filtered_events() {
        let mut capture = DownstreamTextCapture::new(16);
        capture.observe(&[
            SseEvent::new(
                "content_block_delta",
                json!({"type": "content_block_delta", "delta": {"type": "thinking_delta", "thinking": "hidden"}}),
            ),
            SseEvent::new(
                "content_block_delta",
                json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Tokyo is the "}}),
            ),
            SseEvent::new(
                "content_block_delta",
                json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "largest city"}}),
            ),
            SseEvent::new(
                "content_block_start",
                json!({"type": "content_block_start", "content_block": {"type": "tool_use", "name": "Read"}}),
            ),
        ]);
        let reply = capture.reply();
        assert_eq!(reply.text, "Tokyo is the lar");
        assert!(reply.text_truncated);
        assert_eq!(reply.tool_uses, ["Read"]);
    }

    #[test]
    fn leak_markers_flag_replies_that_cite_the_policy() {
        assert_eq!(
            tool_choice_leak_markers("I cannot call any tools, as the tool_choice says."),
            ["tool_choice", "cannot call any tool"]
        );
        assert!(tool_choice_leak_markers("Tokyo has about 14 million people.").is_empty());
        let reply = reply_from_content(
            &[
                json!({"type": "text", "text": "根据提示词注入的要求"}),
                json!({"type": "tool_use", "name": "StructuredOutput"}),
            ],
            1024,
        );
        assert_eq!(tool_choice_leak_markers(&reply.text), ["提示词注入"]);
        assert_eq!(reply.tool_uses, ["StructuredOutput"]);
    }
}
