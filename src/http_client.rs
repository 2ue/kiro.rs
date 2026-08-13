//! HTTP Client 构建模块
//!
//! 提供统一的 HTTP Client 构建功能，支持代理配置

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use reqwest::{Client, Proxy, Request, RequestBuilder, Response};
use serde::Deserialize;
use std::fmt;
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

use crate::model::config::TlsBackend;

#[cfg(test)]
pub(crate) mod allocation_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct CountingAllocator;

    static ENABLED: AtomicBool = AtomicBool::new(false);
    static ALLOCATION_OPS: AtomicUsize = AtomicUsize::new(0);
    static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
    static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
    static PEAK_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
    static MEASUREMENT_LOCK: Mutex<()> = Mutex::new(());

    #[global_allocator]
    static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() && ENABLED.load(Ordering::Relaxed) {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() && ENABLED.load(Ordering::Relaxed) {
                record_allocation(layout.size());
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            if ENABLED.load(Ordering::Relaxed) {
                LIVE_BYTES
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                        Some(live.saturating_sub(layout.size()))
                    })
                    .ok();
            }
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
            if !new_pointer.is_null() && ENABLED.load(Ordering::Relaxed) {
                ALLOCATION_OPS.fetch_add(1, Ordering::Relaxed);
                ALLOCATED_BYTES.fetch_add(new_size, Ordering::Relaxed);
                let live = if new_size >= layout.size() {
                    LIVE_BYTES.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size
                        - layout.size()
                } else {
                    LIVE_BYTES
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                            Some(live.saturating_sub(layout.size() - new_size))
                        })
                        .unwrap_or_default()
                        .saturating_sub(layout.size() - new_size)
                };
                PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
            }
            new_pointer
        }
    }

    fn record_allocation(size: usize) {
        ALLOCATION_OPS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
        let live = LIVE_BYTES.fetch_add(size, Ordering::Relaxed) + size;
        PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub(crate) struct AllocationStats {
        pub(crate) allocation_ops: usize,
        pub(crate) allocated_bytes: usize,
        pub(crate) peak_live_bytes: usize,
        pub(crate) end_live_bytes: usize,
    }

    struct MeasurementGuard;

    impl Drop for MeasurementGuard {
        fn drop(&mut self) {
            ENABLED.store(false, Ordering::Release);
        }
    }

    pub(crate) fn measure<T>(operation: impl FnOnce() -> T) -> (T, AllocationStats) {
        let _lock = MEASUREMENT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ENABLED.store(false, Ordering::Release);
        ALLOCATION_OPS.store(0, Ordering::Relaxed);
        ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        LIVE_BYTES.store(0, Ordering::Relaxed);
        PEAK_LIVE_BYTES.store(0, Ordering::Relaxed);
        assert!(
            !ENABLED.swap(true, Ordering::AcqRel),
            "allocation measurement cannot be nested"
        );
        let guard = MeasurementGuard;
        let result = operation();
        drop(guard);
        (
            result,
            AllocationStats {
                allocation_ops: ALLOCATION_OPS.load(Ordering::Relaxed),
                allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
                peak_live_bytes: PEAK_LIVE_BYTES.load(Ordering::Relaxed),
                end_live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
            },
        )
    }
}

/// 代理配置
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct ProxyConfig {
    /// 代理地址，支持 http/https/socks5/socks5h
    pub url: String,
    /// 代理认证用户名
    pub username: Option<String>,
    /// 代理认证密码
    pub password: Option<String>,
}

impl fmt::Debug for ProxyConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxyConfig")
            .field("url_configured", &!self.url.trim().is_empty())
            .field("scheme", &self.scheme_for_log())
            .field("username_present", &self.username.is_some())
            .field("password_present", &self.password.is_some())
            .finish()
    }
}

pub fn maybe_compress_json_whitespace(body: String, enabled: bool) -> String {
    if !enabled {
        return body;
    }

    let mut in_string = false;
    let mut escaped = false;
    let mut has_removable_whitespace = false;

    for byte in body.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }

        match byte {
            b'"' => in_string = true,
            b' ' | b'\t' | b'\r' | b'\n' => {
                has_removable_whitespace = true;
                break;
            }
            _ => {}
        }
    }

    if !has_removable_whitespace {
        return body;
    }

    // Validate without materializing a Value. This keeps invalid payloads unchanged while
    // preserving duplicate keys, number spellings, escapes, and object order byte-for-byte.
    let mut deserializer = serde_json::Deserializer::from_str(&body);
    if serde::de::IgnoredAny::deserialize(&mut deserializer).is_err() || deserializer.end().is_err()
    {
        return body;
    }

    let mut compacted = body.into_bytes();
    let mut write_index = 0;
    in_string = false;
    escaped = false;
    for read_index in 0..compacted.len() {
        let byte = compacted[read_index];
        let keep = if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            true
        } else {
            match byte {
                b'"' => {
                    in_string = true;
                    true
                }
                b' ' | b'\t' | b'\r' | b'\n' => false,
                _ => true,
            }
        };
        if keep {
            compacted[write_index] = byte;
            write_index += 1;
        }
    }
    compacted.truncate(write_index);

    String::from_utf8(compacted)
        .expect("removing ASCII JSON whitespace from valid UTF-8 must preserve UTF-8")
}

#[derive(Debug)]
pub enum HttpSendError {
    Request(reqwest::Error),
    ResponseHeaderTimeout { timeout_secs: u64 },
    ResponseBodyTimeout { timeout_secs: u64 },
    ResponseBodyTooLarge { max_bytes: usize },
}

impl HttpSendError {
    #[cfg(test)]
    pub fn is_response_header_timeout(&self) -> bool {
        matches!(self, Self::ResponseHeaderTimeout { .. })
    }

    #[cfg(test)]
    pub fn is_response_body_timeout(&self) -> bool {
        matches!(self, Self::ResponseBodyTimeout { .. })
    }

    #[cfg(test)]
    pub fn is_response_body_too_large(&self) -> bool {
        matches!(self, Self::ResponseBodyTooLarge { .. })
    }
}

impl fmt::Display for HttpSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(err) => write!(f, "{}", err),
            Self::ResponseHeaderTimeout { timeout_secs } => {
                write!(
                    f,
                    "upstream response header timeout after {}s",
                    timeout_secs
                )
            }
            Self::ResponseBodyTimeout { timeout_secs } => {
                write!(f, "upstream response body timeout after {}s", timeout_secs)
            }
            Self::ResponseBodyTooLarge { max_bytes } => {
                write!(f, "upstream response body exceeds {} bytes", max_bytes)
            }
        }
    }
}

impl std::error::Error for HttpSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(err) => Some(err),
            Self::ResponseHeaderTimeout { .. }
            | Self::ResponseBodyTimeout { .. }
            | Self::ResponseBodyTooLarge { .. } => None,
        }
    }
}

async fn deadline_first_timeout<F>(duration: Duration, future: F) -> Result<F::Output, ()>
where
    F: Future,
{
    deadline_first_timeout_at(Instant::now() + duration, future).await
}

async fn deadline_first_timeout_at<F>(deadline: Instant, future: F) -> Result<F::Output, ()>
where
    F: Future,
{
    let timer = tokio::time::sleep_until(deadline);
    tokio::pin!(timer);
    tokio::pin!(future);
    tokio::select! {
        biased;
        _ = &mut timer => Err(()),
        output = &mut future => Ok(output),
    }
}

pub async fn send_with_response_header_timeout(
    request: RequestBuilder,
    timeout_secs: u64,
) -> Result<Response, HttpSendError> {
    if timeout_secs == 0 {
        return request.send().await.map_err(HttpSendError::Request);
    }

    match deadline_first_timeout(Duration::from_secs(timeout_secs), request.send()).await {
        Ok(result) => result.map_err(HttpSendError::Request),
        Err(_) => Err(HttpSendError::ResponseHeaderTimeout { timeout_secs }),
    }
}

/// Execute an already-built request with a response-header deadline.
///
/// Callers that account real HTTP sends can build first, then reserve their send budget, and
/// finally execute here. A request-builder failure therefore cannot consume a send attempt.
pub async fn execute_with_response_header_timeout(
    client: &Client,
    request: Request,
    timeout_secs: u64,
) -> Result<Response, HttpSendError> {
    if timeout_secs == 0 {
        return client
            .execute(request)
            .await
            .map_err(HttpSendError::Request);
    }

    match deadline_first_timeout(Duration::from_secs(timeout_secs), client.execute(request)).await {
        Ok(result) => result.map_err(HttpSendError::Request),
        Err(_) => Err(HttpSendError::ResponseHeaderTimeout { timeout_secs }),
    }
}

#[cfg(test)]
pub async fn response_text_with_body_timeout(
    response: Response,
    timeout_secs: u64,
) -> Result<String, HttpSendError> {
    if timeout_secs == 0 {
        return response.text().await.map_err(HttpSendError::Request);
    }

    match deadline_first_timeout(Duration::from_secs(timeout_secs), response.text()).await {
        Ok(result) => result.map_err(HttpSendError::Request),
        Err(_) => Err(HttpSendError::ResponseBodyTimeout { timeout_secs }),
    }
}

pub async fn response_bytes_with_limit_and_body_timeout(
    response: Response,
    timeout_secs: u64,
    max_bytes: usize,
) -> Result<Bytes, HttpSendError> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > max_bytes as u64)
    {
        return Err(HttpSendError::ResponseBodyTooLarge { max_bytes });
    }

    let read = async move {
        let mut stream = response.bytes_stream();
        let mut body = BytesMut::with_capacity(max_bytes.min(16 * 1024));
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(HttpSendError::Request)?;
            if chunk.len() > max_bytes.saturating_sub(body.len()) {
                return Err(HttpSendError::ResponseBodyTooLarge { max_bytes });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    };

    if timeout_secs == 0 {
        return read.await;
    }
    match deadline_first_timeout(Duration::from_secs(timeout_secs), read).await {
        Ok(result) => result,
        Err(_) => Err(HttpSendError::ResponseBodyTimeout { timeout_secs }),
    }
}

pub async fn response_text_with_limit_and_body_timeout(
    response: Response,
    timeout_secs: u64,
    max_bytes: usize,
) -> Result<String, HttpSendError> {
    let body =
        response_bytes_with_limit_and_body_timeout(response, timeout_secs, max_bytes).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

impl ProxyConfig {
    /// 从 url 创建代理配置
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            username: None,
            password: None,
        }
    }

    /// 设置认证信息
    pub fn with_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    fn scheme_for_log(&self) -> &str {
        url::Url::parse(&self.url)
            .ok()
            .map(|url| match url.scheme() {
                "http" => "http",
                "https" => "https",
                "socks5" => "socks5",
                "socks5h" => "socks5h",
                _ => "other",
            })
            .unwrap_or("invalid")
    }
}

/// 构建 HTTP Client
///
/// # Arguments
/// * `proxy` - 可选的代理配置
/// * `timeout_secs` - 超时时间（秒）
///
/// # Returns
/// 配置好的 reqwest::Client
pub fn build_client(
    proxy: Option<&ProxyConfig>,
    timeout_secs: u64,
    tls_backend: TlsBackend,
) -> anyhow::Result<Client> {
    let mut builder = Client::builder();
    if timeout_secs > 0 {
        builder = builder.timeout(Duration::from_secs(timeout_secs));
    }

    match tls_backend {
        TlsBackend::Rustls => {
            builder = builder.use_rustls_tls();
        }
        TlsBackend::NativeTls => {
            #[cfg(feature = "native-tls")]
            {
                builder = builder.use_native_tls();
            }
            #[cfg(not(feature = "native-tls"))]
            {
                anyhow::bail!("此构建版本未包含 native-tls 后端，请在配置中改用 rustls");
            }
        }
    }

    if let Some(proxy_config) = proxy {
        let mut proxy = Proxy::all(&proxy_config.url)?;

        // 设置代理认证
        if let (Some(username), Some(password)) = (&proxy_config.username, &proxy_config.password) {
            proxy = proxy.basic_auth(username, password);
        }

        builder = builder.proxy(proxy);
        tracing::debug!(
            scheme = proxy_config.scheme_for_log(),
            "HTTP Client 使用代理"
        );
    }

    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn sized_json_body(target_size: usize, mode: &str) -> String {
        let (prefix, suffix) = match mode {
            "compact" => (r#"{"payload":""#, r#"","z":1e+02}"#),
            _ => (r#" { "payload" : ""#, "\" , \"z\" : 1e+02 } \n"),
        };
        let payload_len = target_size.saturating_sub(prefix.len() + suffix.len());
        let mut body = String::with_capacity(prefix.len() + payload_len + suffix.len() + 1);
        body.push_str(prefix);
        body.extend(std::iter::repeat_n('x', payload_len));
        body.push_str(suffix);
        if mode == "invalid" {
            body.push('x');
        }
        body
    }

    fn percentile(sorted: &[u128], percentile: usize) -> u128 {
        let index = ((sorted.len() * percentile).saturating_sub(1) / 100).min(sorted.len() - 1);
        sorted[index]
    }

    #[test]
    fn json_whitespace_compression_preserves_token_bytes_for_five_rounds() {
        let fixtures = [
            (
                r#" { "z" : 1.0 , "a" : 1e+02 , "z" : 18446744073709551616 } "#,
                r#"{"z":1.0,"a":1e+02,"z":18446744073709551616}"#,
            ),
            (
                "{\n  \"literal\": \"é/中\",\n  \"escaped\": \"\\u00e9\\/\\n  x\\\\\\\"\",\n  \"nested\": [ true, null, { \"unknown\" : false } ]\n}",
                "{\"literal\":\"é/中\",\"escaped\":\"\\u00e9\\/\\n  x\\\\\\\"\",\"nested\":[true,null,{\"unknown\":false}]}",
            ),
            (
                " \r\n\t[ -0, 0.000, 9E-9, \"a b\\t c\" ] \n",
                "[-0,0.000,9E-9,\"a b\\t c\"]",
            ),
        ];

        for round in 0..5 {
            for (input, expected) in fixtures {
                assert_eq!(
                    maybe_compress_json_whitespace(input.to_owned(), true),
                    expected,
                    "round {round}: JSON tokens must remain byte-for-byte intact"
                );
            }
        }
    }

    #[test]
    fn json_whitespace_compression_preserves_invalid_or_disabled_bodies_for_five_rounds() {
        let invalid = [
            r#"{ "a" : 1 2 }"#,
            "{\"unterminated\": \"value}",
            "{\"unicode-space\":1}\u{00a0}",
            "not json \n at all",
        ];
        let valid = " { \"a\" : [ 1, 2, 3 ], \"b\" : \"x y\" } \n";

        for round in 0..5 {
            assert_eq!(
                maybe_compress_json_whitespace(valid.to_owned(), false),
                valid,
                "round {round}: disabled compression must be exact identity"
            );
            for body in invalid {
                assert_eq!(
                    maybe_compress_json_whitespace(body.to_owned(), true),
                    body,
                    "round {round}: invalid input must not be rewritten"
                );
            }
        }
    }

    #[test]
    fn json_whitespace_raw_and_noop_paths_are_byte_identical_for_one_hundred_rounds() {
        const SIZES: [usize; 4] = [1 << 10, 100 << 10, 1 << 20, 5 << 20];

        for target_size in SIZES {
            for mode in ["disabled", "compact"] {
                let body = sized_json_body(
                    target_size,
                    if mode == "compact" {
                        "compact"
                    } else {
                        "valid"
                    },
                );
                let enabled = mode == "compact";

                for round in 0..100 {
                    let input = body.clone();
                    let input_pointer = input.as_ptr();
                    let input_capacity = input.capacity();
                    let output = maybe_compress_json_whitespace(input, enabled);

                    assert_eq!(
                        output, body,
                        "mode={mode}, target_size={target_size}, round={round}"
                    );
                    assert_eq!(
                        output.as_ptr(),
                        input_pointer,
                        "identity path allocated a replacement body: mode={mode}, target_size={target_size}, round={round}"
                    );
                    assert_eq!(
                        output.capacity(),
                        input_capacity,
                        "mode={mode}, target_size={target_size}, round={round}"
                    );
                }
            }
        }
    }

    #[test]
    fn json_whitespace_compression_reuses_the_input_allocation_for_five_rounds() {
        for round in 0..5 {
            for (body, enabled) in [
                (r#"{ "a" : [ 1, 2, 3 ], "b" : "x y" }"#.to_owned(), true),
                (r#"{"a":[1,2,3],"b":"x y"}"#.to_owned(), true),
                (r#"{ "a" : [ 1, 2, 3 ] }"#.to_owned(), false),
            ] {
                let input_pointer = body.as_ptr();
                let input_capacity = body.capacity();
                let compacted = maybe_compress_json_whitespace(body, enabled);
                assert_eq!(
                    compacted.as_ptr(),
                    input_pointer,
                    "round {round}: compression must not allocate a second body-sized buffer"
                );
                assert_eq!(compacted.capacity(), input_capacity, "round {round}");
            }
        }
    }

    #[test]
    fn json_whitespace_compression_scales_to_five_mib_for_five_rounds() {
        const SIZES: [usize; 4] = [1 << 10, 100 << 10, 1 << 20, 5 << 20];

        for target_size in SIZES {
            let element = " { \"z\" : 1.0, \"a\" : 1e+02, \"s\" : \"keep  spaces\" },\n";
            let repeats = target_size.saturating_sub(2) / element.len();
            let mut body = String::with_capacity(repeats * element.len() + 2);
            body.push('[');
            for index in 0..repeats {
                if index > 0 {
                    body.push(' ');
                }
                body.push_str(element.trim_end_matches([',', '\n']));
                if index + 1 < repeats {
                    body.push(',');
                }
            }
            body.push(']');

            for round in 0..5 {
                let compacted = maybe_compress_json_whitespace(body.clone(), true);
                assert!(
                    compacted.len() < body.len(),
                    "size {target_size}, round {round}"
                );
                assert!(
                    compacted.contains(r#"{"z":1.0,"a":1e+02,"s":"keep  spaces"}"#),
                    "size {target_size}, round {round}"
                );
                assert_eq!(
                    compacted
                        .matches(r#"{"z":1.0,"a":1e+02,"s":"keep  spaces"}"#)
                        .count(),
                    repeats,
                    "size {target_size}, round {round}"
                );
            }
        }
    }

    #[test]
    fn json_whitespace_compression_deep_iterative_validation_and_recovery_for_five_rounds() {
        let recovery = r#" { "ok" : [ 1, 2, 3 ], "text" : "keep  spaces" } "#;
        let expected_recovery = r#"{"ok":[1,2,3],"text":"keep  spaces"}"#;

        for depth in [127, 128, 256, 4096] {
            let deep = format!(" {}0{} ", "[".repeat(depth), "]".repeat(depth));
            let compact = deep.trim().to_string();
            let malformed = format!(" {}0{} ", "[".repeat(depth), "]".repeat(depth - 1));
            for round in 0..5 {
                assert_eq!(
                    maybe_compress_json_whitespace(deep.clone(), true),
                    compact,
                    "depth {depth}, round {round}: iterative validation must preserve deep JSON tokens"
                );
                assert_eq!(
                    maybe_compress_json_whitespace(malformed.clone(), true),
                    malformed,
                    "depth {depth}, round {round}: malformed deep JSON must remain exact"
                );
                assert_eq!(
                    maybe_compress_json_whitespace(recovery.to_string(), true),
                    expected_recovery,
                    "depth {depth}, round {round}: deep input must not poison the next request"
                );
            }
        }
    }

    #[test]
    #[ignore = "run in release as an isolated allocation/latency/RSS body-size probe"]
    fn json_whitespace_compression_release_perf_probe() {
        let target_size = std::env::var("KIRO_BODY_PERF_SIZE_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1 << 20);
        let rounds = std::env::var("KIRO_BODY_PERF_ROUNDS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(5);
        let mode = std::env::var("KIRO_BODY_PERF_MODE").unwrap_or_else(|_| "valid".to_string());
        assert!(matches!(
            mode.as_str(),
            "valid" | "invalid" | "disabled" | "compact"
        ));
        assert!(
            rounds >= 5,
            "release evidence requires at least five rounds"
        );
        let body = sized_json_body(target_size, &mode);
        let enabled = mode != "disabled";
        let mut latencies_us = Vec::with_capacity(rounds);
        let mut allocation_ops = Vec::with_capacity(rounds);
        let mut allocated_bytes = Vec::with_capacity(rounds);
        let mut peak_live_bytes = Vec::with_capacity(rounds);

        for round in 0..rounds {
            let input = body.clone();
            let input_pointer = input.as_ptr();
            let input_capacity = input.capacity();
            let started = Instant::now();
            let (output, stats) = allocation_probe::measure(|| {
                maybe_compress_json_whitespace(std::hint::black_box(input), enabled)
            });
            let elapsed = started.elapsed().as_micros();
            assert_eq!(output.as_ptr(), input_pointer, "round {round}");
            assert_eq!(output.capacity(), input_capacity, "round {round}");
            match mode.as_str() {
                "valid" => assert!(output.len() < body.len(), "round {round}"),
                "invalid" | "disabled" | "compact" => {
                    assert_eq!(output, body, "round {round}: identity mode changed bytes")
                }
                _ => unreachable!(),
            }
            latencies_us.push(elapsed);
            allocation_ops.push(stats.allocation_ops);
            allocated_bytes.push(stats.allocated_bytes);
            peak_live_bytes.push(stats.peak_live_bytes);
            assert_eq!(
                stats.end_live_bytes, 0,
                "round {round}: transform leaked allocator-tracked live bytes"
            );
        }
        latencies_us.sort_unstable();
        println!(
            "BODY_PERF mode={} input_bytes={} rounds={} latency_us_p50={} latency_us_p95={} latency_us_p99={} allocation_ops={:?} allocated_bytes={:?} peak_live_bytes={:?}",
            mode,
            body.len(),
            rounds,
            percentile(&latencies_us, 50),
            percentile(&latencies_us, 95),
            percentile(&latencies_us, 99),
            allocation_ops,
            allocated_bytes,
            peak_live_bytes,
        );
    }

    #[test]
    #[ignore = "run in release as an isolated concurrent body transform/RSS probe"]
    fn json_whitespace_compression_burst_and_recovery_probe() {
        let target_size = std::env::var("KIRO_BODY_PERF_SIZE_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(5 << 20);
        let concurrency = std::env::var("KIRO_BODY_PERF_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8);
        let rounds = std::env::var("KIRO_BODY_PERF_ROUNDS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(5);
        assert!(rounds >= 5);
        let body = Arc::new(sized_json_body(target_size, "valid"));
        let started = Instant::now();

        for round in 0..rounds {
            std::thread::scope(|scope| {
                let barrier = Arc::new(std::sync::Barrier::new(concurrency));
                for worker in 0..concurrency {
                    let body = body.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        let input = body.as_str().to_string();
                        let pointer = input.as_ptr();
                        barrier.wait();
                        let output = maybe_compress_json_whitespace(input, true);
                        assert_eq!(output.as_ptr(), pointer, "round {round}, worker {worker}");
                        assert!(output.len() < body.len(), "round {round}, worker {worker}");
                    });
                }
            });
        }

        for recovery_round in 0..5 {
            assert_eq!(
                maybe_compress_json_whitespace(r#" { "recovered" : true } "#.to_string(), true),
                r#"{"recovered":true}"#,
                "recovery round {recovery_round}"
            );
        }
        println!(
            "BODY_BURST input_bytes={} concurrency={} rounds={} calls={} total_ms={}",
            body.len(),
            concurrency,
            rounds,
            concurrency * rounds,
            started.elapsed().as_millis(),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "run in isolation to characterize abort latency of the synchronous CPU transform"]
    async fn json_whitespace_compression_abort_and_recovery_probe() {
        let target_size = std::env::var("KIRO_BODY_PERF_SIZE_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(5 << 20);
        let rounds = std::env::var("KIRO_BODY_PERF_ROUNDS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(5);
        assert!(rounds >= 5);
        let mut abort_latencies_us = Vec::with_capacity(rounds);
        let mut completed_before_abort_observed = 0;
        let mut cancelled_after_cpu_poll = 0;

        for round in 0..rounds {
            let input = sized_json_body(target_size, "valid");
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let task_completed = completed.clone();
            let task = tokio::spawn(async move {
                let _ = started_tx.send(());
                let output = maybe_compress_json_whitespace(input, true);
                task_completed.store(true, std::sync::atomic::Ordering::Release);
                output.len()
            });
            started_rx
                .await
                .unwrap_or_else(|_| panic!("round {round}: transform task never started"));
            let abort_started = Instant::now();
            task.abort();
            match task.await {
                Ok(output_len) => {
                    assert!(output_len < target_size, "round {round}");
                    completed_before_abort_observed += 1;
                }
                Err(error) if error.is_cancelled() => {
                    cancelled_after_cpu_poll += 1;
                }
                Err(error) => panic!("round {round}: transform task failed: {error}"),
            }
            let abort_latency = abort_started.elapsed().as_micros();
            assert!(
                completed.load(std::sync::atomic::Ordering::Acquire),
                "round {round}: abort may cancel the async task only after its synchronous transform poll returns"
            );
            abort_latencies_us.push(abort_latency);

            assert_eq!(
                maybe_compress_json_whitespace(r#" { "recovered" : true } "#.to_string(), true),
                r#"{"recovered":true}"#,
                "round {round}: abort observation must not poison later transforms"
            );
        }
        abort_latencies_us.sort_unstable();
        println!(
            "BODY_ABORT input_bytes={} rounds={} completed_before_abort_observed={} cancelled_after_cpu_poll={} abort_latency_us_p50={} abort_latency_us_p95={} abort_latency_us_p99={}",
            target_size,
            rounds,
            completed_before_abort_observed,
            cancelled_after_cpu_poll,
            percentile(&abort_latencies_us, 50),
            percentile(&abort_latencies_us, 95),
            percentile(&abort_latencies_us, 99),
        );
    }

    fn local_test_client() -> Client {
        Client::builder()
            .no_proxy()
            .build()
            .expect("local test client")
    }

    #[test]
    fn test_proxy_config_new() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        assert_eq!(config.url, "http://127.0.0.1:7890");
        assert!(config.username.is_none());
        assert!(config.password.is_none());
    }

    #[test]
    fn test_proxy_config_with_auth() {
        let config = ProxyConfig::new("socks5://127.0.0.1:1080").with_auth("user", "pass");
        assert_eq!(config.url, "socks5://127.0.0.1:1080");
        assert_eq!(config.username, Some("user".to_string()));
        assert_eq!(config.password, Some("pass".to_string()));
    }

    #[test]
    fn proxy_config_debug_redacts_url_and_authentication() {
        let config = ProxyConfig::new("http://url-user:url-pass@proxy.example.invalid:8080/path")
            .with_auth("configured-user", "configured-pass");

        let output = format!("{config:?}");
        for secret in [
            config.url.as_str(),
            config.username.as_deref().unwrap(),
            config.password.as_deref().unwrap(),
            "url-user",
            "url-pass",
            "proxy.example.invalid",
        ] {
            assert!(!output.contains(secret), "Debug output leaked {secret:?}");
        }
        assert!(output.contains("scheme: \"http\""));
        assert!(output.contains("username_present: true"));
    }

    #[test]
    fn test_build_client_without_proxy() {
        let client = build_client(None, 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn test_build_client_without_total_timeout() {
        let client = build_client(None, 0, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[test]
    fn test_build_client_with_proxy() {
        let config = ProxyConfig::new("http://127.0.0.1:7890");
        let client = build_client(Some(&config), 30, TlsBackend::Rustls);
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn deadline_first_timeout_rejects_ready_future_after_elapsed_deadline_for_five_rounds() {
        for round in 0..5 {
            let deadline = tokio::time::Instant::now() - Duration::from_millis(1);
            let result = deadline_first_timeout_at(deadline, std::future::ready(round)).await;
            assert_eq!(result, Err(()), "round {round}");
        }
    }

    #[tokio::test]
    async fn deadline_first_timeout_accepts_ready_future_before_deadline_for_five_rounds() {
        for round in 0..5 {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            let result = deadline_first_timeout_at(deadline, std::future::ready(round)).await;
            assert_eq!(result, Ok(round), "round {round}");
        }
    }

    #[tokio::test]
    async fn send_with_response_header_timeout_expires_before_response_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let Ok((_stream, _peer)) = listener.accept().await else {
                return;
            };
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let client = local_test_client();
        let err = send_with_response_header_timeout(client.get(format!("http://{}", addr)), 1)
            .await
            .expect_err("server accepts connection but never sends response headers");

        assert!(err.is_response_header_timeout());
        server.abort();
    }

    #[tokio::test]
    async fn response_text_with_body_timeout_expires_after_response_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let Ok((mut stream, _peer)) = listener.accept().await else {
                return;
            };
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
                .await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let client = local_test_client();
        let response = send_with_response_header_timeout(client.get(format!("http://{}", addr)), 5)
            .await
            .expect("response headers should arrive before timeout");
        let err = response_text_with_body_timeout(response, 1)
            .await
            .expect_err("server sends headers but stalls before declared body completes");

        assert!(err.is_response_body_timeout());
        server.abort();
    }

    #[tokio::test]
    async fn bounded_response_body_rejects_content_length_and_chunked_overflow_for_five_rounds() {
        for _ in 0..5 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef")
                    .await
                    .unwrap();
            });
            let response = local_test_client()
                .get(format!("http://{addr}"))
                .send()
                .await
                .unwrap();
            let error = response_bytes_with_limit_and_body_timeout(response, 5, 5)
                .await
                .expect_err("declared body exceeds the hard limit");
            assert!(error.is_response_body_too_large());
            server.await.unwrap();

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
                    )
                    .await
                    .unwrap();
            });
            let response = local_test_client()
                .get(format!("http://{addr}"))
                .send()
                .await
                .unwrap();
            let error = response_bytes_with_limit_and_body_timeout(response, 5, 5)
                .await
                .expect_err("chunked body exceeds the hard limit while streaming");
            assert!(error.is_response_body_too_large());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn bounded_response_body_accepts_exact_limit_and_recovers_for_five_rounds() {
        for round in 0..5 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                for response in [
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef"
                        .as_slice(),
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde"
                        .as_slice(),
                ] {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request).await;
                    stream.write_all(response).await.unwrap();
                }
            });
            let client = local_test_client();

            let oversized = client.get(format!("http://{addr}")).send().await.unwrap();
            let error = response_bytes_with_limit_and_body_timeout(oversized, 5, 5)
                .await
                .expect_err("first response is oversized");
            assert!(error.is_response_body_too_large(), "round {round}");

            let exact = client.get(format!("http://{addr}")).send().await.unwrap();
            let body = response_bytes_with_limit_and_body_timeout(exact, 5, 5)
                .await
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            assert_eq!(body.as_ref(), b"abcde", "round {round}");
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn bounded_response_body_timeout_is_total_and_recovers_for_five_rounds() {
        for round in 0..5 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stalled, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _ = stalled.read(&mut request).await;
                stalled
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\na")
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(1_200)).await;
                drop(stalled);

                let (mut recovered, _) = listener.accept().await.unwrap();
                let _ = recovered.read(&mut request).await;
                recovered
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .unwrap();
            });
            let client = local_test_client();

            let stalled = client.get(format!("http://{addr}")).send().await.unwrap();
            let error = response_bytes_with_limit_and_body_timeout(stalled, 1, 5)
                .await
                .expect_err("body must time out before the declared length arrives");
            assert!(error.is_response_body_timeout(), "round {round}");

            let recovered = client.get(format!("http://{addr}")).send().await.unwrap();
            let body = response_bytes_with_limit_and_body_timeout(recovered, 2, 5)
                .await
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            assert_eq!(body.as_ref(), b"ok", "round {round}");
            server.await.unwrap();
        }
    }
}
