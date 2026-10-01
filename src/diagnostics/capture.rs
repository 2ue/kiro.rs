//! 进程内 LLM 流量明文采集。
//!
//! 设计约束：
//! - 未开启采集时，请求路径只付出一次原子读（`is_active`），不做任何序列化。
//! - 开启后事件在调用方线程序列化，经有界通道交给独立写线程落盘；
//!   通道满、超出字节/事件预算时只丢弃事件并计数，绝不阻塞请求路径。
//! - 同一时刻最多一个采集会话；会话到期由后台定时器停止并打包 ZIP。
//! - 凭据类请求头（Authorization / x-api-key / Cookie 等）默认脱敏，
//!   仅在会话显式开启 `includeSecrets` 时保留原值。

use std::{
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::Stream;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const DEFAULT_MAX_DURATION_SECS: u64 = 15 * 60;
const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_EVENTS: u64 = 100_000;
const CHANNEL_CAPACITY: usize = 4096;
const HISTORY_LIMIT: usize = 20;
const EVENTS_FILE: &str = "events.jsonl";
const MANIFEST_FILE: &str = "manifest.json";
const ARCHIVE_FILE: &str = "capture.zip";
const REDACTED: &str = "[redacted]";

/// 默认采集目录（相对工作目录，Docker 下即 `/app/logs/llm-capture`）。
pub const DEFAULT_CAPTURE_DIR: &str = "logs/llm-capture";

/// 被视为凭据的请求/响应头（小写）。
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "cookie",
    "set-cookie",
    "x-amz-security-token",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureLimits {
    pub max_duration_secs: u64,
    pub max_bytes: u64,
    pub max_events: u64,
    /// 保留凭据类请求头原值。默认 false。
    pub include_secrets: bool,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            max_duration_secs: DEFAULT_MAX_DURATION_SECS,
            max_bytes: DEFAULT_MAX_BYTES,
            max_events: DEFAULT_MAX_EVENTS,
            include_secrets: false,
        }
    }
}

impl CaptureLimits {
    fn normalized(self) -> Self {
        Self {
            max_duration_secs: self.max_duration_secs.clamp(1, 24 * 60 * 60),
            max_bytes: self.max_bytes.clamp(4 * 1024, 4 * 1024 * 1024 * 1024),
            max_events: self.max_events.clamp(1, 10_000_000),
            include_secrets: self.include_secrets,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    /// `idle` | `running` | `stopped` | `failed`
    pub state: String,
    pub session_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub limits: Option<CaptureLimits>,
    pub event_count: u64,
    pub bytes_written: u64,
    pub dropped_events: u64,
    pub archive_bytes: Option<u64>,
    pub stop_reason: Option<String>,
    pub error: Option<String>,
}

impl CaptureStatus {
    fn idle() -> Self {
        Self {
            state: "idle".to_string(),
            session_id: None,
            started_at: None,
            stopped_at: None,
            expires_at: None,
            limits: None,
            event_count: 0,
            bytes_written: 0,
            dropped_events: 0,
            archive_bytes: None,
            stop_reason: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureOverview {
    pub dir: String,
    pub current: CaptureStatus,
    pub history: Vec<CaptureStatus>,
}

#[derive(Debug)]
pub enum CaptureError {
    AlreadyRunning,
    NotRunning,
    Io(std::io::Error),
}

impl std::error::Error for CaptureError {}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "capture session already running"),
            Self::NotRunning => write!(f, "no capture session running"),
            Self::Io(error) => write!(f, "capture io error: {error}"),
        }
    }
}

impl From<std::io::Error> for CaptureError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Default)]
struct Counters {
    seq: AtomicU64,
    events: AtomicU64,
    bytes: AtomicU64,
    dropped: AtomicU64,
}

struct Runtime {
    id: String,
    dir: PathBuf,
    started_at: DateTime<Utc>,
    started_instant: Instant,
    limits: CaptureLimits,
    counters: Arc<Counters>,
    tx: SyncSender<Vec<u8>>,
    writer: JoinHandle<std::io::Result<()>>,
}

struct State {
    runtime: Option<Runtime>,
    current: CaptureStatus,
    history: Vec<CaptureStatus>,
}

pub struct CaptureManager {
    root: PathBuf,
    active: AtomicBool,
    include_secrets: AtomicBool,
    /// 当前会话共享计数器的快照指针；写入路径不持有 state 锁。
    hot: Mutex<Option<HotPath>>,
    state: Mutex<State>,
}

#[derive(Clone)]
struct HotPath {
    session_id: Arc<str>,
    limits: CaptureLimits,
    counters: Arc<Counters>,
    tx: SyncSender<Vec<u8>>,
}

static GLOBAL: OnceLock<Arc<CaptureManager>> = OnceLock::new();

/// 初始化全局采集管理器；重复调用返回首次实例。
pub fn init_global(root: impl Into<PathBuf>) -> Arc<CaptureManager> {
    GLOBAL.get_or_init(|| CaptureManager::new(root)).clone()
}

/// 全局采集管理器；未初始化时返回 None（例如单元测试）。
pub fn global() -> Option<&'static Arc<CaptureManager>> {
    GLOBAL.get()
}

/// 是否正在采集。请求路径应先判断该值再构造事件。
#[inline]
pub fn is_active() -> bool {
    GLOBAL
        .get()
        .is_some_and(|manager| manager.active.load(Ordering::Relaxed))
}

/// 记录一条事件。`build` 仅在采集开启时调用。
#[inline]
pub fn record(event_type: &'static str, request_id: Option<&str>, build: impl FnOnce() -> Value) {
    let Some(manager) = GLOBAL.get() else {
        return;
    };
    if !manager.active.load(Ordering::Relaxed) {
        return;
    }
    manager.record(event_type, request_id, build());
}

/// 当前会话是否保留凭据原值。
pub fn include_secrets() -> bool {
    GLOBAL
        .get()
        .is_some_and(|manager| manager.include_secrets.load(Ordering::Relaxed))
}

impl CaptureManager {
    pub fn new(root: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            root: root.into(),
            active: AtomicBool::new(false),
            include_secrets: AtomicBool::new(false),
            hot: Mutex::new(None),
            state: Mutex::new(State {
                runtime: None,
                current: CaptureStatus::idle(),
                history: Vec::new(),
            }),
        })
    }

    pub fn overview(&self) -> CaptureOverview {
        let state = self.state.lock();
        let mut current = state.current.clone();
        if let Some(runtime) = &state.runtime {
            fill_counters(&mut current, &runtime.counters);
        }
        CaptureOverview {
            dir: self.root.display().to_string(),
            current,
            history: state.history.clone(),
        }
    }

    /// 启动采集。成功后会派生一个到期自动停止的 tokio 任务（若在 runtime 内）。
    pub fn start(self: &Arc<Self>, limits: CaptureLimits) -> Result<CaptureStatus, CaptureError> {
        let limits = limits.normalized();
        let mut state = self.state.lock();
        if state.runtime.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        let started_at = Utc::now();
        let id = format!(
            "capture-{}-{}",
            started_at.format("%Y%m%dT%H%M%SZ"),
            &Uuid::new_v4().simple().to_string()[..8]
        );
        let dir = self.root.join(&id);
        create_private_dir(&dir)?;
        let file = open_private_file(&dir.join(EVENTS_FILE))?;

        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(CHANNEL_CAPACITY);
        let writer = std::thread::Builder::new()
            .name("llm-capture-writer".to_string())
            .spawn(move || write_loop(file, rx))?;
        let counters = Arc::new(Counters::default());
        let expires_at = started_at + chrono::Duration::seconds(limits.max_duration_secs as i64);

        state.current = CaptureStatus {
            state: "running".to_string(),
            session_id: Some(id.clone()),
            started_at: Some(started_at),
            expires_at: Some(expires_at),
            limits: Some(limits),
            ..CaptureStatus::idle()
        };
        *self.hot.lock() = Some(HotPath {
            session_id: Arc::from(id.as_str()),
            limits,
            counters: Arc::clone(&counters),
            tx: tx.clone(),
        });
        state.runtime = Some(Runtime {
            id: id.clone(),
            dir,
            started_at,
            started_instant: Instant::now(),
            limits,
            counters,
            tx,
            writer,
        });
        self.include_secrets
            .store(limits.include_secrets, Ordering::Relaxed);
        self.active.store(true, Ordering::Release);
        let status = state.current.clone();
        drop(state);

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let manager = Arc::clone(self);
            handle.spawn(async move {
                tokio::time::sleep(Duration::from_secs(limits.max_duration_secs)).await;
                let _ =
                    tokio::task::spawn_blocking(move || manager.stop_session(&id, "max_duration"))
                        .await;
            });
        }
        tracing::info!(session_id = ?status.session_id, "LLM capture started");
        Ok(status)
    }

    /// 停止当前会话并打包。会阻塞等待写线程落盘，异步上下文请用 `spawn_blocking`。
    pub fn stop(&self, reason: &str) -> Result<CaptureStatus, CaptureError> {
        let id = {
            let state = self.state.lock();
            match &state.runtime {
                Some(runtime) => runtime.id.clone(),
                None => return Err(CaptureError::NotRunning),
            }
        };
        self.stop_session(&id, reason)
    }

    fn stop_session(&self, id: &str, reason: &str) -> Result<CaptureStatus, CaptureError> {
        let runtime = {
            let mut state = self.state.lock();
            if state
                .runtime
                .as_ref()
                .is_none_or(|runtime| runtime.id != id)
            {
                return Err(CaptureError::NotRunning);
            }
            self.active.store(false, Ordering::Release);
            self.include_secrets.store(false, Ordering::Relaxed);
            *self.hot.lock() = None;
            state.runtime.take().expect("checked above")
        };

        let Runtime {
            id,
            dir,
            started_at,
            started_instant,
            limits,
            counters,
            tx,
            writer,
        } = runtime;
        // 关闭发送端；仍在途的克隆随 hot 清空后自然释放。
        drop(tx);
        let write_result = writer
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("capture writer panicked")));

        let stopped_at = Utc::now();
        let mut status = CaptureStatus {
            state: "stopped".to_string(),
            session_id: Some(id.clone()),
            started_at: Some(started_at),
            stopped_at: Some(stopped_at),
            expires_at: None,
            limits: Some(limits),
            stop_reason: Some(reason.to_string()),
            ..CaptureStatus::idle()
        };
        fill_counters(&mut status, &counters);

        let manifest = json!({
            "sessionId": id,
            "startedAt": started_at,
            "stoppedAt": stopped_at,
            "durationMs": started_instant.elapsed().as_millis() as u64,
            "stopReason": reason,
            "limits": limits,
            "eventCount": status.event_count,
            "bytesWritten": status.bytes_written,
            "droppedEvents": status.dropped_events,
            "version": env!("CARGO_PKG_VERSION"),
            "eventTypes": EVENT_TYPE_DOCS,
        });
        let archive_result = write_result.and_then(|()| {
            let manifest_bytes =
                serde_json::to_vec_pretty(&manifest).map_err(std::io::Error::other)?;
            std::fs::write(dir.join(MANIFEST_FILE), &manifest_bytes)?;
            create_zip_archive(&dir, &dir.join(ARCHIVE_FILE))
        });
        match archive_result {
            Ok(bytes) => status.archive_bytes = Some(bytes),
            Err(error) => {
                status.state = "failed".to_string();
                status.error = Some(error.to_string());
            }
        }

        let mut state = self.state.lock();
        state.current = status.clone();
        state.history.insert(0, status.clone());
        state.history.truncate(HISTORY_LIMIT);
        tracing::info!(session_id = %id, reason, events = status.event_count, "LLM capture stopped");
        Ok(status)
    }

    /// 返回已完成会话的归档路径。只接受内存历史中的会话 ID，杜绝路径穿越。
    pub fn archive_path(&self, session_id: &str) -> Option<PathBuf> {
        let state = self.state.lock();
        state
            .history
            .iter()
            .find(|status| {
                status.session_id.as_deref() == Some(session_id) && status.archive_bytes.is_some()
            })
            .map(|_| self.root.join(session_id).join(ARCHIVE_FILE))
    }

    fn record(&self, event_type: &'static str, request_id: Option<&str>, data: Value) {
        let Some(hot) = self.hot.lock().clone() else {
            return;
        };
        let counters = &hot.counters;
        if counters.events.load(Ordering::Relaxed) >= hot.limits.max_events {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let seq = counters.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let line = json!({
            "seq": seq,
            "ts": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            "session": &*hot.session_id,
            "type": event_type,
            "requestId": request_id,
            "data": data,
        });
        let Ok(mut line) = serde_json::to_vec(&line) else {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        line.push(b'\n');
        let len = line.len() as u64;
        let previous = counters.bytes.fetch_add(len, Ordering::Relaxed);
        if previous.saturating_add(len) > hot.limits.max_bytes {
            counters.bytes.fetch_sub(len, Ordering::Relaxed);
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match hot.tx.try_send(line) {
            Ok(()) => {
                counters.events.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                counters.bytes.fetch_sub(len, Ordering::Relaxed);
                counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// manifest 中附带的事件类型说明，方便离线阅读。
const EVENT_TYPE_DOCS: &[(&str, &str)] = &[
    (
        "client_request",
        "客户端原始请求：endpoint、请求头、完整请求体（system/tools/messages）",
    ),
    (
        "upstream_request",
        "每次发往 Kiro 上游的请求：attempt、凭据 ID、URL、请求头、完整请求体",
    ),
    ("upstream_response", "上游响应状态与响应头"),
    ("upstream_error_body", "上游非 2xx 响应体"),
    ("upstream_frame", "上游 AWS EventStream 帧：帧头与 payload"),
    (
        "upstream_body",
        "非流式上游响应体中无法按 EventStream 解码的原始内容",
    ),
    (
        "upstream_stream_json_error",
        "流式上游返回 2xx JSON 错误体的分类结果",
    ),
    (
        "upstream_decode_error",
        "EventStream 解码失败：错误与当次原始分块（base64）",
    ),
    (
        "client_sse",
        "返回客户端的 SSE 原始文本块（含 ping/keepalive）",
    ),
    ("client_response", "返回客户端的非流式 JSON 响应或错误响应"),
    (
        "client_stream_end",
        "客户端流结束：completed 或 dropped（客户端断开）",
    ),
    (
        "external_upstream_request",
        "外部号池转发请求：URL、请求头、请求体",
    ),
    ("external_upstream_response", "外部号池响应状态与响应头"),
];

fn fill_counters(status: &mut CaptureStatus, counters: &Counters) {
    status.event_count = counters.events.load(Ordering::Relaxed);
    status.bytes_written = counters.bytes.load(Ordering::Relaxed);
    status.dropped_events = counters.dropped.load(Ordering::Relaxed);
}

fn write_loop(file: std::fs::File, rx: Receiver<Vec<u8>>) -> std::io::Result<()> {
    let mut writer = BufWriter::with_capacity(256 * 1024, file);
    let mut last_flush = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(line) => writer.write_all(&line)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        // 定期刷盘，便于运行中直接 tail 文件。
        if last_flush.elapsed() >= Duration::from_secs(1) {
            writer.flush()?;
            last_flush = Instant::now();
        }
    }
    writer.flush()?;
    writer.get_ref().sync_all()
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

fn open_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn create_zip_archive(dir: &Path, output: &Path) -> std::io::Result<u64> {
    let file = {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(output)?
    };
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    for name in [MANIFEST_FILE, EVENTS_FILE] {
        zip.start_file(name, options)
            .map_err(std::io::Error::other)?;
        let mut input = std::fs::File::open(dir.join(name))?;
        std::io::copy(&mut input, &mut zip)?;
    }
    zip.finish().map_err(std::io::Error::other)?;
    Ok(std::fs::metadata(output)?.len())
}

// ---------------------------------------------------------------------------
// 事件构造辅助
// ---------------------------------------------------------------------------

/// HTTP 头转 JSON（同名多值合并为数组），凭据头按会话配置脱敏。
pub fn http_headers_json(headers: &http::HeaderMap) -> Value {
    let keep_secrets = include_secrets();
    let mut object = serde_json::Map::new();
    for name in headers.keys() {
        let key = name.as_str();
        let values: Vec<Value> = headers
            .get_all(name)
            .iter()
            .map(|value| {
                if !keep_secrets && SECRET_HEADERS.contains(&key) {
                    Value::String(redact_secret(value.as_bytes()))
                } else {
                    Value::String(String::from_utf8_lossy(value.as_bytes()).into_owned())
                }
            })
            .collect();
        let value = if values.len() == 1 {
            values.into_iter().next().unwrap_or(Value::Null)
        } else {
            Value::Array(values)
        };
        object.insert(key.to_string(), value);
    }
    Value::Object(object)
}

/// 保留凭据的类型前缀与长度，便于排查而不泄露值。
fn redact_secret(value: &[u8]) -> String {
    let text = String::from_utf8_lossy(value);
    let scheme = text
        .split_once(' ')
        .map(|(scheme, _)| format!("{scheme} "))
        .unwrap_or_default();
    format!("{scheme}{REDACTED} (len={})", value.len())
}

/// 请求/响应体：合法 JSON 原样嵌入，否则按 UTF-8 文本，非 UTF-8 按 base64。
pub fn body_json(bytes: &[u8]) -> Value {
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        return json!({ "encoding": "json", "bytes": bytes.len(), "json": value });
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({ "encoding": "text", "bytes": bytes.len(), "text": text }),
        Err(_) => {
            json!({ "encoding": "base64", "bytes": bytes.len(), "base64": base64_encode(bytes) })
        }
    }
}

pub fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 记录一个 AWS EventStream 帧。
pub fn record_upstream_frame(
    request_id: Option<&str>,
    credential_id: Option<u64>,
    attempt: u32,
    frame: &crate::kiro::parser::frame::Frame,
) {
    record("upstream_frame", request_id, || {
        json!({
            "credentialId": credential_id,
            "attempt": attempt,
            "messageType": frame.message_type(),
            "eventType": frame.event_type(),
            "headers": frame.headers.to_json(),
            "payload": body_json(&frame.payload),
        })
    });
}

/// 包装发往客户端的 SSE 流：逐块记录 `client_sse`，结束或客户端断开时记录 `client_stream_end`。
pub fn tap_client_stream<S, E>(
    request_id: String,
    route: &'static str,
    inner: S,
) -> impl Stream<Item = Result<Bytes, E>> + Send + 'static
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: 'static,
{
    use futures::StreamExt;

    struct EndGuard {
        request_id: String,
        route: &'static str,
        started: Instant,
        chunks: u64,
        bytes: u64,
        completed: bool,
    }
    impl Drop for EndGuard {
        fn drop(&mut self) {
            let outcome = if self.completed {
                "completed"
            } else {
                "dropped"
            };
            record("client_stream_end", Some(&self.request_id), || {
                json!({
                    "route": self.route,
                    "outcome": outcome,
                    "chunks": self.chunks,
                    "bytes": self.bytes,
                    "elapsedMs": self.started.elapsed().as_millis() as u64,
                })
            });
        }
    }

    let mut guard = EndGuard {
        request_id,
        route,
        started: Instant::now(),
        chunks: 0,
        bytes: 0,
        completed: false,
    };
    let mut inner = Box::pin(inner);
    futures::stream::poll_fn(move |cx| {
        let polled = inner.poll_next_unpin(cx);
        match &polled {
            std::task::Poll::Ready(Some(Ok(chunk))) => {
                guard.chunks += 1;
                guard.bytes += chunk.len() as u64;
                let index = guard.chunks;
                record("client_sse", Some(&guard.request_id), || {
                    json!({
                        "route": guard.route,
                        "index": index,
                        "text": String::from_utf8_lossy(chunk),
                    })
                });
            }
            std::task::Poll::Ready(None) => guard.completed = true,
            _ => {}
        }
        polled
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("kiro-capture-{}", Uuid::new_v4().simple()))
    }

    fn read_events(manager: &CaptureManager, id: &str) -> Vec<Value> {
        std::fs::read_to_string(manager.root.join(id).join(EVENTS_FILE))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn start_record_stop_creates_archive() {
        let root = temp_root();
        let manager = CaptureManager::new(&root);
        let status = manager.start(CaptureLimits::default()).unwrap();
        let id = status.session_id.unwrap();
        assert!(manager.active.load(Ordering::Relaxed));
        manager.record("client_request", Some("req_1"), json!({"body": 1}));
        let manager2 = Arc::clone(&manager);
        let stopped = tokio::task::spawn_blocking(move || manager2.stop("test"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stopped.state, "stopped");
        assert_eq!(stopped.event_count, 1);
        assert!(!manager.active.load(Ordering::Relaxed));
        let events = read_events(&manager, &id);
        assert_eq!(events[0]["type"], "client_request");
        assert_eq!(events[0]["requestId"], "req_1");
        assert_eq!(events[0]["seq"], 1);
        let archive = manager.archive_path(&id).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
        assert!(zip.by_name(EVENTS_FILE).is_ok());
        assert!(zip.by_name(MANIFEST_FILE).is_ok());
        assert!(manager.archive_path("../etc").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn concurrent_start_is_rejected_and_budget_drops_events() {
        let root = temp_root();
        let manager = CaptureManager::new(&root);
        manager
            .start(CaptureLimits {
                max_events: 2,
                ..CaptureLimits::default()
            })
            .unwrap();
        assert!(matches!(
            manager.start(CaptureLimits::default()),
            Err(CaptureError::AlreadyRunning)
        ));
        for _ in 0..5 {
            manager.record("x", None, Value::Null);
        }
        let overview = manager.overview();
        assert_eq!(overview.current.event_count, 2);
        assert_eq!(overview.current.dropped_events, 3);
        let manager2 = Arc::clone(&manager);
        tokio::task::spawn_blocking(move || manager2.stop("test"))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            manager.stop("again"),
            Err(CaptureError::NotRunning)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn secret_headers_are_redacted_by_default() {
        let mut headers = http::HeaderMap::new();
        headers.insert("authorization", "Bearer abc123".parse().unwrap());
        headers.insert("x-api-key", "sk-xyz".parse().unwrap());
        headers.insert("user-agent", "claude-cli/2.1".parse().unwrap());
        let value = http_headers_json(&headers);
        assert_eq!(value["authorization"], "Bearer [redacted] (len=13)");
        assert_eq!(value["x-api-key"], "[redacted] (len=6)");
        assert_eq!(value["user-agent"], "claude-cli/2.1");
    }

    #[test]
    fn body_json_handles_json_text_and_binary() {
        assert_eq!(body_json(br#"{"a":1}"#)["json"]["a"], 1);
        assert_eq!(body_json(b"hello")["text"], "hello");
        assert_eq!(body_json(&[0xff, 0x00])["encoding"], "base64");
    }

    #[tokio::test]
    async fn tapped_stream_passes_chunks_through() {
        use futures::StreamExt;
        let inner = futures::stream::iter(vec![
            Ok::<_, std::convert::Infallible>(Bytes::from_static(b"event: a\n\n")),
            Ok(Bytes::from_static(b"event: b\n\n")),
        ]);
        let collected: Vec<_> = tap_client_stream("req".to_string(), "local", inner)
            .collect()
            .await;
        assert_eq!(collected.len(), 2);
    }
}
