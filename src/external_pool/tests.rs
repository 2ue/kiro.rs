use super::*;
use crate::anthropic::types::{Message, Metadata, OutputConfig, SystemMessage, Thinking};
use crate::anthropic::usage::UsageRecordQuery;
use crate::kiro::endpoint::{IdeEndpoint, KiroEndpoint};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::{AcquireMode, MultiTokenManager};
use crate::model::config::Config;
use crate::model::config::{ReportedUsageFieldPolicy, ReportedUsagePathPolicy};
use base64::Engine as _;

#[test]
fn persisted_external_pool_enum_parsers_reject_unknown_values_for_five_rounds() {
    for round in 1..=5 {
        assert_eq!(
            ExternalPoolAuthType::parse_known("bearer"),
            Some(ExternalPoolAuthType::Bearer),
            "round {round}"
        );
        assert!(ExternalPoolAuthType::parse_known("corrupt").is_none());
        assert_eq!(
            ExternalPoolUsageProjectionMode::parse_known("pass_through"),
            Some(ExternalPoolUsageProjectionMode::PassThrough)
        );
        assert!(ExternalPoolUsageProjectionMode::parse_known("corrupt").is_none());
        assert_eq!(
            ExternalPoolRequestBodyMode::parse_known("normalized"),
            Some(ExternalPoolRequestBodyMode::Normalized)
        );
        assert!(ExternalPoolRequestBodyMode::parse_known("corrupt").is_none());
        assert_eq!(
            ExternalPoolRawModelMode::parse_known("none"),
            Some(ExternalPoolRawModelMode::None)
        );
        assert!(ExternalPoolRawModelMode::parse_known("corrupt").is_none());
        assert_eq!(
            ExternalPoolAutoDisablePolicy::parse_known("inherit"),
            Some(ExternalPoolAutoDisablePolicy::Inherit)
        );
        assert!(ExternalPoolAutoDisablePolicy::parse_known("corrupt").is_none());
        assert_eq!(
            ExternalPoolModelMappingMode::parse_known("processed_mapping"),
            Some(ExternalPoolModelMappingMode::ProcessedMapping)
        );
        assert!(ExternalPoolModelMappingMode::parse_known("corrupt").is_none());
        assert_eq!(
            ExternalPoolStreamResponseMode::parse_known("event_passthrough"),
            Some(ExternalPoolStreamResponseMode::EventPassthrough)
        );
        assert!(ExternalPoolStreamResponseMode::parse_known("corrupt").is_none());
        assert_eq!(
            ExternalPoolStreamRetryMode::parse_known("inherit"),
            Some(ExternalPoolStreamRetryMode::Inherit)
        );
        assert_eq!(
            ExternalPoolStreamRetryMode::parse_known("enabled"),
            Some(ExternalPoolStreamRetryMode::Enabled)
        );
        assert_eq!(
            ExternalPoolStreamRetryMode::parse_known("disabled"),
            Some(ExternalPoolStreamRetryMode::Disabled)
        );
        assert!(ExternalPoolStreamRetryMode::parse_known("corrupt").is_none());
    }
}

#[test]
fn finite_external_queue_lease_covers_wait_without_periodic_renewal() {
    for round in 1..=5 {
        let default_wait = ExternalPoolsConfig::default().effective_dispatch_max_wait_secs();
        let default_policy =
            external_pool_queue_lease_policy(Some(Duration::from_secs(default_wait)));
        assert_eq!(default_wait, 5, "round {round}");
        assert_eq!(default_policy.ttl_secs, 65, "round {round}");
        assert!(!default_policy.renewal_required, "round {round}");

        let long_policy = external_pool_queue_lease_policy(Some(Duration::from_secs(120)));
        assert_eq!(long_policy.ttl_secs, 180, "round {round}");
        assert!(!long_policy.renewal_required, "round {round}");

        let fractional_policy = external_pool_queue_lease_policy(Some(Duration::from_millis(1501)));
        assert_eq!(fractional_policy.ttl_secs, 62, "round {round}");
        assert!(!fractional_policy.renewal_required, "round {round}");

        let unlimited_policy = external_pool_queue_lease_policy(None);
        assert_eq!(unlimited_policy.ttl_secs, 60, "round {round}");
        assert!(unlimited_policy.renewal_required, "round {round}");
    }
}

fn test_postgres_config() -> Option<Config> {
    let url = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")?;
    let mut config = Config::default();
    config.postgres.url = Some(url);
    config.postgres.max_connections = 2;
    Some(config)
}

fn test_redis_config() -> Option<Config> {
    let url = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL")?;
    let mut config = Config::default();
    config.redis.url = Some(url);
    config.redis.key_prefix = format!("kiro_rs:test:external_pool:{}", uuid::Uuid::new_v4());
    Some(config)
}

async fn test_external_pool_manager() -> Option<(ExternalPoolManager, Arc<PostgresStore>)> {
    let Some(postgres_config) = test_postgres_config() else {
        eprintln!("跳过外部备用池集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return None;
    };
    let Some(redis_config) = test_redis_config() else {
        eprintln!("跳过外部备用池集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return None;
    };
    let postgres = Arc::new(PostgresStore::connect_test(&postgres_config).await.unwrap());
    let redis = Arc::new(RedisStore::connect(&redis_config).await.unwrap());
    Some((ExternalPoolManager::new(postgres.clone(), redis), postgres))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_local_mutations_keep_all_pools_when_own_event_is_observed() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };

    for (index, name) in ["local-one", "local-two", "local-three"].iter().enumerate() {
        let mut request = create_pool_request(name, (index + 1) as i32, true);
        request.supported_models = vec!["claude-sonnet-4".to_string()];
        let pool = postgres.create_external_pool(request).await.unwrap();
        manager.notify_external_pool_data_changed_with_local_pool("test_local_create", &pool);

        let generation = index as u64 + 1;
        let own_event = serde_json::json!({
            "generation": generation,
            "reason": "test_local_create",
            "poolId": pool.id,
            "origin": manager.instance_id,
        })
        .to_string();
        assert!(
            !manager.observe_external_pool_data_event(&own_event),
            "an event emitted by this process must not invalidate the just-merged local snapshot"
        );
        let snapshot = manager.load_authoritative_pool_snapshot().await.unwrap();
        assert_eq!(
            snapshot.len(),
            index + 1,
            "local mutation {name} must retain all previously created pools"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

enum TestRawHttpBody {
    DeclaredOnly(usize),
    Chunked(Vec<u8>),
    StallAfterPrefix,
    Fixed(Vec<u8>),
}

async fn spawn_test_raw_http_response(
    status: StatusCode,
    body: TestRawHttpBody,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let reason = status.canonical_reason().unwrap_or("Test");
        match body {
            TestRawHttpBody::DeclaredOnly(length) => {
                let headers = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    status.as_u16(),
                    reason,
                    length
                );
                let _ = socket.write_all(headers.as_bytes()).await;
            }
            TestRawHttpBody::Chunked(bytes) => {
                let headers = format!(
                    "HTTP/1.1 {} {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                    status.as_u16(),
                    reason
                );
                if socket.write_all(headers.as_bytes()).await.is_ok()
                    && socket
                        .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                        .await
                        .is_ok()
                    && socket.write_all(&bytes).await.is_ok()
                {
                    let _ = socket.write_all(b"\r\n0\r\n\r\n").await;
                }
            }
            TestRawHttpBody::StallAfterPrefix => {
                let headers = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nx",
                    status.as_u16(),
                    reason
                );
                let _ = socket.write_all(headers.as_bytes()).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            TestRawHttpBody::Fixed(bytes) => {
                let headers = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    status.as_u16(),
                    reason,
                    bytes.len()
                );
                if socket.write_all(headers.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(&bytes).await;
                }
            }
        }
    });
    (format!("http://{address}"), task)
}

#[derive(Clone, Default)]
struct AuxiliaryFallbackFakeHits {
    refresh: Arc<AtomicU64>,
    profile: Arc<AtomicU64>,
    local_inference: Arc<AtomicU64>,
    external_inference: Arc<AtomicU64>,
}

struct AuxiliaryFallbackFakeServer {
    base_url: String,
    hits: AuxiliaryFallbackFakeHits,
    task: tokio::task::JoinHandle<()>,
}

impl AuxiliaryFallbackFakeServer {
    async fn start() -> Self {
        async fn refresh(
            axum::extract::State(hits): axum::extract::State<AuxiliaryFallbackFakeHits>,
        ) -> impl axum::response::IntoResponse {
            hits.refresh.fetch_add(1, Ordering::Relaxed);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": "server_error"})),
            )
        }

        async fn profile(
            axum::extract::State(hits): axum::extract::State<AuxiliaryFallbackFakeHits>,
        ) -> impl axum::response::IntoResponse {
            hits.profile.fetch_add(1, Ordering::Relaxed);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"message": "controlled profile failure"})),
            )
        }

        async fn local_inference(
            axum::extract::State(hits): axum::extract::State<AuxiliaryFallbackFakeHits>,
        ) -> impl axum::response::IntoResponse {
            hits.local_inference.fetch_add(1, Ordering::Relaxed);
            (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({"message": "controlled local rejection"})),
            )
        }

        async fn external_inference(
            axum::extract::State(hits): axum::extract::State<AuxiliaryFallbackFakeHits>,
        ) -> impl axum::response::IntoResponse {
            hits.external_inference.fetch_add(1, Ordering::Relaxed);
            axum::Json(serde_json::json!({
                "id": "msg_external_auxiliary",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-6",
                "content": [{"type": "text", "text": "external-ok"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 7, "output_tokens": 1}
            }))
        }

        let hits = AuxiliaryFallbackFakeHits::default();
        let app = axum::Router::new()
            .route("/token", axum::routing::post(refresh))
            .route("/ListAvailableProfiles", axum::routing::post(profile))
            .route(
                "/generateAssistantResponse",
                axum::routing::post(local_inference),
            )
            .route("/cc/v1/messages", axum::routing::post(external_inference))
            .route("/v1/messages", axum::routing::post(external_inference))
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind auxiliary fallback fake server");
        let address = listener.local_addr().expect("auxiliary fallback address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve auxiliary fallback fake server");
        });
        Self {
            base_url: format!("http://{address}"),
            hits,
            task,
        }
    }

    fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.hits.refresh.load(Ordering::Acquire),
            self.hits.profile.load(Ordering::Acquire),
            self.hits.local_inference.load(Ordering::Acquire),
            self.hits.external_inference.load(Ordering::Acquire),
        )
    }
}

impl Drop for AuxiliaryFallbackFakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct ExternalMessagesFakeState {
    hits: Arc<AtomicU64>,
    status: StatusCode,
    headers: HeaderMap,
    body: serde_json::Value,
}

struct ExternalMessagesFakeServer {
    base_url: String,
    hits: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl ExternalMessagesFakeServer {
    async fn start(status: StatusCode, body: serde_json::Value) -> Self {
        Self::start_with_headers(status, HeaderMap::new(), body).await
    }

    async fn start_with_headers(
        status: StatusCode,
        headers: HeaderMap,
        body: serde_json::Value,
    ) -> Self {
        async fn messages(
            axum::extract::State(state): axum::extract::State<ExternalMessagesFakeState>,
        ) -> impl axum::response::IntoResponse {
            state.hits.fetch_add(1, Ordering::Relaxed);
            (state.status, state.headers.clone(), axum::Json(state.body))
        }

        let hits = Arc::new(AtomicU64::new(0));
        let state = ExternalMessagesFakeState {
            hits: hits.clone(),
            status,
            headers,
            body,
        };
        let app = axum::Router::new()
            .route("/v1/messages", axum::routing::post(messages))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind external messages fake server");
        let address = listener.local_addr().expect("external messages address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve external messages fake server");
        });
        Self {
            base_url: format!("http://{address}"),
            hits,
            task,
        }
    }

    fn snapshot(&self) -> u64 {
        self.hits.load(Ordering::Acquire)
    }
}

impl Drop for ExternalMessagesFakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
enum ExternalStreamFakeStep {
    Chunk(Vec<u8>),
    Delay(Duration),
    Error(String),
}

impl ExternalStreamFakeStep {
    fn chunk(data: impl Into<Vec<u8>>) -> Self {
        Self::Chunk(data.into())
    }

    fn delay(duration: Duration) -> Self {
        Self::Delay(duration)
    }

    fn error(message: impl Into<String>) -> Self {
        Self::Error(message.into())
    }
}

#[derive(Clone)]
struct ExternalStreamFakeState {
    hits: Arc<AtomicU64>,
    status: StatusCode,
    steps: Arc<Vec<ExternalStreamFakeStep>>,
}

struct ExternalStreamFakeServer {
    base_url: String,
    hits: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl ExternalStreamFakeServer {
    async fn start(steps: Vec<ExternalStreamFakeStep>) -> Self {
        Self::start_with_status(StatusCode::OK, steps).await
    }

    async fn start_with_status(status: StatusCode, steps: Vec<ExternalStreamFakeStep>) -> Self {
        async fn messages(
            axum::extract::State(state): axum::extract::State<ExternalStreamFakeState>,
        ) -> Response {
            state.hits.fetch_add(1, Ordering::Relaxed);
            let steps = state.steps.clone();
            let stream =
                futures::stream::unfold((steps, 0usize), |(steps, mut index)| async move {
                    loop {
                        let step = steps.get(index).cloned()?;
                        index = index.saturating_add(1);
                        match step {
                            ExternalStreamFakeStep::Chunk(bytes) => {
                                return Some((
                                    Ok::<Bytes, std::io::Error>(Bytes::from(bytes)),
                                    (steps, index),
                                ));
                            }
                            ExternalStreamFakeStep::Delay(duration) => {
                                tokio::time::sleep(duration).await;
                            }
                            ExternalStreamFakeStep::Error(message) => {
                                return Some((Err(std::io::Error::other(message)), (steps, index)));
                            }
                        }
                    }
                });
            Response::builder()
                .status(state.status)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(stream))
                .expect("build external stream fake response")
        }

        let hits = Arc::new(AtomicU64::new(0));
        let state = ExternalStreamFakeState {
            hits: hits.clone(),
            status,
            steps: Arc::new(steps),
        };
        let app = axum::Router::new()
            .route("/v1/messages", axum::routing::post(messages))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind external stream fake server");
        let address = listener.local_addr().expect("external stream address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve external stream fake server");
        });
        Self {
            base_url: format!("http://{address}"),
            hits,
            task,
        }
    }

    fn snapshot(&self) -> u64 {
        self.hits.load(Ordering::Acquire)
    }
}

impl Drop for ExternalStreamFakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct FlakyExternalMessagesFakeState {
    hits: Arc<AtomicU64>,
    remaining_failures: Arc<AtomicU64>,
    failure_status: StatusCode,
    failure_body: serde_json::Value,
    success_body: serde_json::Value,
}

struct FlakyExternalMessagesFakeServer {
    base_url: String,
    hits: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl FlakyExternalMessagesFakeServer {
    async fn start_failures_then_success(
        failure_count: u64,
        failure_status: StatusCode,
        failure_body: serde_json::Value,
        success_body: serde_json::Value,
    ) -> Self {
        async fn messages(
            axum::extract::State(state): axum::extract::State<FlakyExternalMessagesFakeState>,
        ) -> impl axum::response::IntoResponse {
            state.hits.fetch_add(1, Ordering::Relaxed);
            let previous = state.remaining_failures.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |current| (current > 0).then(|| current - 1),
            );
            if previous.is_ok() {
                (state.failure_status, axum::Json(state.failure_body.clone()))
            } else {
                (StatusCode::OK, axum::Json(state.success_body.clone()))
            }
        }

        let hits = Arc::new(AtomicU64::new(0));
        let state = FlakyExternalMessagesFakeState {
            hits: hits.clone(),
            remaining_failures: Arc::new(AtomicU64::new(failure_count)),
            failure_status,
            failure_body,
            success_body,
        };
        let app = axum::Router::new()
            .route("/v1/messages", axum::routing::post(messages))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind flaky external messages fake server");
        let address = listener
            .local_addr()
            .expect("flaky external messages address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve flaky external messages fake server");
        });
        Self {
            base_url: format!("http://{address}"),
            hits,
            task,
        }
    }

    fn snapshot(&self) -> u64 {
        self.hits.load(Ordering::Acquire)
    }
}

impl Drop for FlakyExternalMessagesFakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct TurbulentExternalMessagesFakeState {
    hits: Arc<AtomicU64>,
    failures: Arc<AtomicU64>,
    fail_until_hit: u64,
    fail_percent: u8,
    seed: u64,
    statuses: Arc<Vec<(StatusCode, String)>>,
    success_body: serde_json::Value,
}

struct TurbulentExternalMessagesFakeServer {
    base_url: String,
    hits: Arc<AtomicU64>,
    failures: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl TurbulentExternalMessagesFakeServer {
    async fn start(
        fail_until_hit: u64,
        fail_percent: u8,
        seed: u64,
        statuses: Vec<(StatusCode, &str)>,
        success_body: serde_json::Value,
    ) -> Self {
        assert!(!statuses.is_empty(), "turbulent statuses must not be empty");
        async fn messages(
            axum::extract::State(state): axum::extract::State<TurbulentExternalMessagesFakeState>,
        ) -> impl axum::response::IntoResponse {
            let hit = state.hits.fetch_add(1, Ordering::Relaxed) + 1;
            let should_fail = hit <= state.fail_until_hit
                && deterministic_failure_percent(hit, state.seed) < state.fail_percent as u64;
            if should_fail {
                state.failures.fetch_add(1, Ordering::Relaxed);
                let index = deterministic_failure_percent(hit, state.seed ^ 0x51d7_34a3) as usize
                    % state.statuses.len();
                let (status, message) = &state.statuses[index];
                return (*status, axum::Json(fake_external_error_body(message)));
            }
            (StatusCode::OK, axum::Json(state.success_body.clone()))
        }

        let hits = Arc::new(AtomicU64::new(0));
        let failures = Arc::new(AtomicU64::new(0));
        let state = TurbulentExternalMessagesFakeState {
            hits: hits.clone(),
            failures: failures.clone(),
            fail_until_hit,
            fail_percent: fail_percent.min(100),
            seed,
            statuses: Arc::new(
                statuses
                    .into_iter()
                    .map(|(status, message)| (status, message.to_string()))
                    .collect(),
            ),
            success_body,
        };
        let app = axum::Router::new()
            .route("/v1/messages", axum::routing::post(messages))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind turbulent external messages fake server");
        let address = listener
            .local_addr()
            .expect("turbulent external messages address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve turbulent external messages fake server");
        });
        Self {
            base_url: format!("http://{address}"),
            hits,
            failures,
            task,
        }
    }

    fn snapshot(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Acquire),
            self.failures.load(Ordering::Acquire),
        )
    }
}

impl Drop for TurbulentExternalMessagesFakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn deterministic_failure_percent(index: u64, seed: u64) -> u64 {
    let mut mixed = index.wrapping_add(seed).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xff51_afd7_ed55_8ccd);
    mixed ^= mixed >> 33;
    mixed % 100
}

fn fake_external_success_body(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "msg_external_retry",
        "type": "message",
        "role": "assistant",
        "model": "claude-sonnet-4-6",
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 7, "output_tokens": 1}
    })
}

fn fake_external_error_body(message: &str) -> serde_json::Value {
    serde_json::json!({
        "error": {"type": "api_error", "message": message},
        "type": "error"
    })
}

fn external_sse_event(name: &str, value: serde_json::Value) -> String {
    format!("event: {name}\ndata: {value}\n\n")
}

fn external_sse_message_start(id: &str, input_tokens: i32) -> String {
    external_sse_event(
        "message_start",
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-sonnet-4-6",
                "stop_reason": null,
                "usage": {"input_tokens": input_tokens, "output_tokens": 0}
            }
        }),
    )
}

fn external_sse_ping() -> String {
    external_sse_event("ping", serde_json::json!({"type": "ping"}))
}

fn external_sse_error(message: &str) -> String {
    external_sse_event(
        "error",
        serde_json::json!({
            "type": "error",
            "error": {"type": "api_error", "message": message}
        }),
    )
}

fn external_sse_text_start() -> String {
    external_sse_event(
        "content_block_start",
        serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
    )
}

fn external_sse_text_delta(text: &str) -> String {
    external_sse_event(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text}
        }),
    )
}

fn external_sse_text_stop() -> String {
    external_sse_event(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": 0}),
    )
}

fn external_sse_message_delta(input_tokens: i32, output_tokens: i32) -> String {
    external_sse_event(
        "message_delta",
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
        }),
    )
}

fn external_sse_message_stop() -> String {
    external_sse_event("message_stop", serde_json::json!({"type": "message_stop"}))
}

fn external_stream_success_steps(
    message_id: &str,
    text: &str,
    input_tokens: i32,
    output_tokens: i32,
) -> Vec<ExternalStreamFakeStep> {
    vec![
        ExternalStreamFakeStep::chunk(external_sse_message_start(message_id, input_tokens)),
        ExternalStreamFakeStep::chunk(external_sse_text_start()),
        ExternalStreamFakeStep::chunk(external_sse_text_delta(text)),
        ExternalStreamFakeStep::chunk(external_sse_text_stop()),
        ExternalStreamFakeStep::chunk(external_sse_message_delta(input_tokens, output_tokens)),
        ExternalStreamFakeStep::chunk(external_sse_message_stop()),
    ]
}

fn external_stream_config_for_test() -> ExternalPoolsConfig {
    ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 8,
        external_pool_retry_max_attempts: 2,
        external_pool_same_pool_retry_count: 0,
        external_pool_stream_idle_timeout_secs: 1,
        external_pool_protocol_error_cooldown_secs: 1,
        external_pool_network_error_cooldown_secs: 1,
        external_pool_transient_failure_priority_penalty: 20,
        ..ExternalPoolsConfig::default()
    }
}

fn external_stream_route(
    request_id: impl Into<String>,
    error_id: impl Into<String>,
) -> (
    ExternalRouteRequest,
    Arc<crate::anthropic::usage::UsageRecorder>,
) {
    let recorder = Arc::new(crate::anthropic::usage::UsageRecorder::new(16));
    let mut route = test_route("claude-sonnet-4-6");
    payload_mut(&mut route).stream = true;
    refresh_test_route_derived_state(&mut route);
    route.request_id = request_id.into();
    route.error_id = error_id.into();
    route.recorder = recorder.clone();
    route.direct_policy_reason = Some(EXPLICIT_DIRECT_POLICY_REASON.to_string());
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
    (route, recorder)
}

async fn read_response_body_text_allow_error(response: Response) -> (String, Option<String>) {
    let mut stream = response.into_body().into_data_stream();
    let mut body = Vec::new();
    let mut error = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => body.extend_from_slice(&chunk),
            Err(err) => {
                error = Some(err.to_string());
                break;
            }
        }
    }
    (String::from_utf8_lossy(&body).into_owned(), error)
}

fn usage_record_for_request(
    recorder: &crate::anthropic::usage::UsageRecorder,
    request_id: &str,
) -> UsageRecord {
    let result = recorder.query(UsageRecordQuery {
        request_id: Some(request_id.to_string()),
        ..UsageRecordQuery::default()
    });
    assert_eq!(result.records.len(), 1, "usage record for {request_id}");
    result.records[0].clone()
}

async fn create_messages_pool(
    postgres: &PostgresStore,
    name: &str,
    priority: i32,
    base_url: &str,
) -> ExternalPool {
    let mut request = create_pool_request(name, priority, true);
    request.base_url = base_url.to_string();
    request.max_concurrent_requests = 4;
    request.supported_models = vec!["claude-sonnet-4-6".to_string()];
    postgres.create_external_pool(request).await.unwrap()
}

async fn create_messages_pool_with_concurrency(
    postgres: &PostgresStore,
    name: &str,
    priority: i32,
    base_url: &str,
    max_concurrent_requests: u32,
) -> ExternalPool {
    let mut request = create_pool_request(name, priority, true);
    request.base_url = base_url.to_string();
    request.max_concurrent_requests = max_concurrent_requests;
    request.supported_models = vec!["claude-sonnet-4-6".to_string()];
    postgres.create_external_pool(request).await.unwrap()
}

#[test]
fn external_pool_pre_output_stream_retry_effective_mode_respects_overrides() {
    let mut config = ExternalPoolsConfig {
        external_pool_stream_pre_output_retry_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let mut pool = test_pool("https://example.test/v1", true);

    pool.pre_output_stream_retry_mode = ExternalPoolStreamRetryMode::Inherit;
    assert!(effective_external_pool_pre_output_stream_retry_enabled(
        &pool, &config
    ));

    config.external_pool_stream_pre_output_retry_enabled = false;
    assert!(!effective_external_pool_pre_output_stream_retry_enabled(
        &pool, &config
    ));

    pool.pre_output_stream_retry_mode = ExternalPoolStreamRetryMode::Enabled;
    assert!(effective_external_pool_pre_output_stream_retry_enabled(
        &pool, &config
    ));

    config.external_pool_stream_pre_output_retry_enabled = true;
    pool.pre_output_stream_retry_mode = ExternalPoolStreamRetryMode::Disabled;
    assert!(!effective_external_pool_pre_output_stream_retry_enabled(
        &pool, &config
    ));
}

#[test]
fn external_pool_pre_output_stream_commit_classifier_is_conservative() {
    let message_start = external_sse_message_start("msg_classifier_start", 10);
    assert!(
        !external_sse_event_commits_pre_output_stream(message_start.as_bytes()),
        "message_start is protocol-only and can be buffered before retry"
    );

    let ping = external_sse_ping();
    assert!(
        !external_sse_event_commits_pre_output_stream(ping.as_bytes()),
        "ping is protocol-only and can be discarded with a failed attempt"
    );

    let text_start = external_sse_text_start();
    assert!(
        external_sse_event_commits_pre_output_stream(text_start.as_bytes()),
        "content_block_start commits downstream protocol state even when text is empty"
    );

    let stop = external_sse_message_stop();
    assert!(
        external_sse_event_commits_pre_output_stream(stop.as_bytes()),
        "legal terminal message_stop is not retried by this pre-output failure feature"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_stream_pre_output_error_event_fails_over_and_keeps_success_usage_clean() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalStreamFakeServer::start(vec![
        ExternalStreamFakeStep::chunk(external_sse_message_start("msg_a", 111)),
        ExternalStreamFakeStep::chunk(external_sse_error("raw upstream failure secret")),
    ])
    .await;
    let succeeding = ExternalStreamFakeServer::start(external_stream_success_steps(
        "msg_b",
        "ok-from-b",
        222,
        5,
    ))
    .await;
    let pool_a =
        create_messages_pool(&postgres, "stream-pre-output-error-a", 1, &failing.base_url).await;
    let pool_b = create_messages_pool(
        &postgres,
        "stream-pre-output-error-b",
        10,
        &succeeding.base_url,
    )
    .await;

    let request_id = "req_stream_pre_output_error_failover";
    let (route, recorder) = external_stream_route(request_id, "err_stream_pre_output_error");
    let response = match timeout(
        Duration::from_secs(5),
        manager.forward_with_failover_result(external_stream_config_for_test(), route),
    )
    .await
    .expect("pre-output error event failover should finish")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("pre-output error should fail over to healthy pool: {error:?}")
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    let (body, stream_error) = read_response_body_text_allow_error(response).await;
    assert_eq!(stream_error, None);
    assert!(body.contains("ok-from-b"));
    assert!(body.contains("msg_b"));
    assert!(!body.contains("msg_a"));
    assert!(!body.contains("raw upstream failure secret"));
    assert_eq!(failing.snapshot(), 1);
    assert_eq!(succeeding.snapshot(), 1);

    let record = usage_record_for_request(&recorder, request_id);
    assert_eq!(record.status, UsageRecordStatus::Success);
    assert_eq!(record.external_pool_id, Some(pool_b.id));
    assert_eq!(record.local_attempted, Some(false));
    assert!(record.credential_attempts.is_empty());
    assert_eq!(record.external_attempts.len(), 2);
    assert_eq!(record.external_attempts[0].pool_id, pool_a.id);
    assert_eq!(record.external_attempts[0].action, "retry_next");
    assert_eq!(
        record.external_attempts[0].error_type.as_deref(),
        Some("protocol_error")
    );
    assert_eq!(record.external_attempts[1].pool_id, pool_b.id);
    assert_eq!(record.external_attempts[1].action, "success");
    let billing = record
        .external_pool_billing
        .as_ref()
        .expect("successful stream billing");
    assert_eq!(billing.raw_usage.input_tokens, 222);
    assert_eq!(billing.raw_usage.output_tokens, 5);
    assert_eq!(billing.reported_usage.input_tokens, 222);
    assert_eq!(billing.reported_usage.output_tokens, 5);
    assert_ne!(billing.raw_usage.input_tokens, 111);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_stream_protocol_only_then_error_event_fails_over_without_leaking_prefix() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalStreamFakeServer::start(vec![
        ExternalStreamFakeStep::chunk(external_sse_message_start("msg_protocol_only_a", 111)),
        ExternalStreamFakeStep::chunk(external_sse_ping()),
        ExternalStreamFakeStep::chunk(external_sse_error("raw protocol-only failure")),
    ])
    .await;
    let succeeding = ExternalStreamFakeServer::start(external_stream_success_steps(
        "msg_protocol_only_b",
        "ok-after-ping",
        222,
        6,
    ))
    .await;
    create_messages_pool(
        &postgres,
        "stream-protocol-only-error-a",
        1,
        &failing.base_url,
    )
    .await;
    create_messages_pool(
        &postgres,
        "stream-protocol-only-error-b",
        10,
        &succeeding.base_url,
    )
    .await;

    let request_id = "req_stream_protocol_only_error_failover";
    let (route, recorder) = external_stream_route(request_id, "err_stream_protocol_only_error");
    let response = match timeout(
        Duration::from_secs(5),
        manager.forward_with_failover_result(external_stream_config_for_test(), route),
    )
    .await
    .expect("protocol-only pre-output error failover should finish")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("protocol-only pre-output error should fail over: {error:?}")
        }
    };
    let (body, stream_error) = read_response_body_text_allow_error(response).await;
    assert_eq!(stream_error, None);
    assert!(body.contains("ok-after-ping"));
    assert!(body.contains("msg_protocol_only_b"));
    assert!(!body.contains("msg_protocol_only_a"));
    assert!(!body.contains("raw protocol-only failure"));
    assert_eq!(failing.snapshot(), 1);
    assert_eq!(succeeding.snapshot(), 1);

    let record = usage_record_for_request(&recorder, request_id);
    assert_eq!(record.status, UsageRecordStatus::Success);
    assert_eq!(record.external_attempts.len(), 2);
    assert_eq!(record.external_attempts[0].action, "retry_next");
    assert_eq!(record.external_attempts[1].action, "success");

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_stream_pre_output_eof_read_error_and_idle_fail_over_to_next_pool() {
    for (case, steps) in [
        (
            "eof",
            vec![ExternalStreamFakeStep::chunk(external_sse_message_start(
                "msg_eof_a",
                111,
            ))],
        ),
        (
            "read_error",
            vec![
                ExternalStreamFakeStep::chunk(external_sse_message_start("msg_read_a", 111)),
                ExternalStreamFakeStep::error("controlled fake upstream body error"),
            ],
        ),
        (
            "idle",
            vec![
                ExternalStreamFakeStep::chunk(external_sse_message_start("msg_idle_a", 111)),
                ExternalStreamFakeStep::delay(Duration::from_millis(1_500)),
                ExternalStreamFakeStep::chunk(external_sse_error("late idle failure")),
            ],
        ),
    ] {
        let Some((manager, postgres)) = test_external_pool_manager().await else {
            return;
        };
        let failing = ExternalStreamFakeServer::start(steps).await;
        let succeeding = ExternalStreamFakeServer::start(external_stream_success_steps(
            &format!("msg_{case}_b"),
            &format!("ok-{case}-from-b"),
            222,
            7,
        ))
        .await;
        create_messages_pool(
            &postgres,
            &format!("stream-pre-output-{case}-a"),
            1,
            &failing.base_url,
        )
        .await;
        create_messages_pool(
            &postgres,
            &format!("stream-pre-output-{case}-b"),
            10,
            &succeeding.base_url,
        )
        .await;

        let request_id = format!("req_stream_pre_output_{case}_failover");
        let (route, recorder) =
            external_stream_route(request_id.clone(), format!("err_stream_pre_output_{case}"));
        let response = match timeout(
            Duration::from_secs(6),
            manager.forward_with_failover_result(external_stream_config_for_test(), route),
        )
        .await
        .unwrap_or_else(|_| panic!("{case}: failover should finish"))
        {
            ExternalPoolForwardOutcome::Response(response) => response,
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("{case}: pre-output stream failure should fail over: {error:?}")
            }
        };
        let (body, stream_error) = read_response_body_text_allow_error(response).await;
        assert_eq!(stream_error, None, "{case}");
        assert!(body.contains(&format!("ok-{case}-from-b")), "{case}");
        assert!(!body.contains("msg_eof_a"), "{case}");
        assert!(!body.contains("msg_read_a"), "{case}");
        assert!(!body.contains("msg_idle_a"), "{case}");
        assert!(
            !body.contains("controlled fake upstream body error"),
            "{case}"
        );
        assert_eq!(failing.snapshot(), 1, "{case}");
        assert_eq!(succeeding.snapshot(), 1, "{case}");

        let record = usage_record_for_request(&recorder, &request_id);
        assert_eq!(record.status, UsageRecordStatus::Success, "{case}");
        assert_eq!(record.external_attempts.len(), 2, "{case}");
        assert_eq!(record.external_attempts[0].action, "retry_next", "{case}");
        assert_eq!(record.external_attempts[1].action, "success", "{case}");
        assert_eq!(record.output_tokens, 7, "{case}");

        postgres.drop_test_schema().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_stream_pre_output_retry_disabled_returns_original_stream_error_without_failover()
 {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalStreamFakeServer::start(vec![
        ExternalStreamFakeStep::chunk(external_sse_message_start("msg_disabled_a", 111)),
        ExternalStreamFakeStep::chunk(external_sse_error("raw disabled retry secret")),
    ])
    .await;
    let succeeding = ExternalStreamFakeServer::start(external_stream_success_steps(
        "msg_disabled_b",
        "should-not-run",
        222,
        8,
    ))
    .await;
    let mut disabled_pool_request = create_pool_request("stream-pre-output-disabled-a", 1, true);
    disabled_pool_request.base_url = failing.base_url.clone();
    disabled_pool_request.max_concurrent_requests = 4;
    disabled_pool_request.supported_models = vec!["claude-sonnet-4-6".to_string()];
    disabled_pool_request.pre_output_stream_retry_mode = ExternalPoolStreamRetryMode::Disabled;
    postgres
        .create_external_pool(disabled_pool_request)
        .await
        .unwrap();
    create_messages_pool(
        &postgres,
        "stream-pre-output-disabled-b",
        10,
        &succeeding.base_url,
    )
    .await;

    let request_id = "req_stream_pre_output_retry_disabled";
    let (route, recorder) = external_stream_route(request_id, "err_stream_pre_output_disabled");
    let response = match timeout(
        Duration::from_secs(5),
        manager.forward_with_failover_result(external_stream_config_for_test(), route),
    )
    .await
    .expect("disabled retry stream should return response")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!(
                "disabled retry keeps current stream behavior, not final dispatch error: {error:?}"
            )
        }
    };
    let (body, stream_error) = read_response_body_text_allow_error(response).await;
    assert_eq!(stream_error, None);
    assert!(body.contains("msg_disabled_a"));
    assert!(body.contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE));
    assert!(!body.contains("raw disabled retry secret"));
    assert!(!body.contains("should-not-run"));
    assert_eq!(failing.snapshot(), 1);
    assert_eq!(succeeding.snapshot(), 0);

    let record = usage_record_for_request(&recorder, request_id);
    assert_eq!(record.status, UsageRecordStatus::StreamError);
    assert_eq!(record.error_type.as_deref(), Some("stream_error"));
    assert_eq!(
        record.error_message.as_deref(),
        Some("external upstream emitted an error event")
    );
    assert_eq!(record.external_attempts.len(), 1);
    assert_eq!(record.external_attempts[0].action, "success");

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_stream_error_after_content_start_does_not_replay_request() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalStreamFakeServer::start(vec![
        ExternalStreamFakeStep::chunk(external_sse_message_start("msg_post_commit_a", 111)),
        ExternalStreamFakeStep::chunk(external_sse_text_start()),
        ExternalStreamFakeStep::chunk(external_sse_error("raw post-commit secret")),
    ])
    .await;
    let succeeding = ExternalStreamFakeServer::start(external_stream_success_steps(
        "msg_post_commit_b",
        "should-not-replay",
        222,
        9,
    ))
    .await;
    create_messages_pool(&postgres, "stream-post-commit-a", 1, &failing.base_url).await;
    create_messages_pool(&postgres, "stream-post-commit-b", 10, &succeeding.base_url).await;

    let request_id = "req_stream_post_commit_error_no_replay";
    let (route, recorder) = external_stream_route(request_id, "err_stream_post_commit_error");
    let response = match timeout(
        Duration::from_secs(5),
        manager.forward_with_failover_result(external_stream_config_for_test(), route),
    )
    .await
    .expect("post-commit stream should return original response")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("post-commit stream error must not become pre-response failover: {error:?}")
        }
    };
    let (body, stream_error) = read_response_body_text_allow_error(response).await;
    assert_eq!(stream_error, None);
    assert!(body.contains("msg_post_commit_a"));
    assert!(body.contains("content_block_start"));
    assert!(body.contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE));
    assert!(!body.contains("raw post-commit secret"));
    assert!(!body.contains("should-not-replay"));
    assert!(!body.contains("msg_post_commit_b"));
    assert_eq!(failing.snapshot(), 1);
    assert_eq!(succeeding.snapshot(), 0);

    let record = usage_record_for_request(&recorder, request_id);
    assert_eq!(record.status, UsageRecordStatus::StreamError);
    assert_eq!(record.error_type.as_deref(), Some("stream_error"));
    assert_eq!(record.external_attempts.len(), 1);
    assert_eq!(record.external_attempts[0].action, "success");
    assert_eq!(
        record.external_attempts[0].pool_name,
        "stream-post-commit-a"
    );

    postgres.drop_test_schema().await.unwrap();
}

async fn unused_loopback_base_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind unused loopback port");
    let address = listener.local_addr().expect("unused loopback address");
    drop(listener);
    format!("http://{address}")
}

fn auxiliary_fallback_local_provider(base_url: &str, expired_for_refresh: bool) -> KiroProvider {
    let mut config = Config::default();
    config.kiro_upstream_base_url = Some(base_url.to_string());
    config.kiro_upstream_response_timeout_secs = 2;
    config.credential_retry_max_attempts = 1;
    config.credential_prompt_logic_retry_enabled = false;
    let credentials = KiroCredentials {
        id: Some(1),
        access_token: Some("fake-local-access-token".to_string()),
        refresh_token: Some(format!("refresh-{}", "x".repeat(256))),
        expires_at: Some(
            if expired_for_refresh {
                Utc::now() - chrono::Duration::hours(1)
            } else {
                Utc::now() + chrono::Duration::hours(1)
            }
            .to_rfc3339(),
        ),
        auth_method: Some("external_idp".to_string()),
        client_id: Some("fake-client".to_string()),
        token_endpoint: Some(format!("{base_url}/token")),
        ..Default::default()
    };
    let manager = Arc::new(
        MultiTokenManager::new(config, vec![credentials], None, None, false)
            .expect("construct auxiliary fallback local manager"),
    );
    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
    KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string())
}

async fn assert_external_bounded_body_recovery(
    client: &reqwest::Client,
    status: StatusCode,
    max_bytes: usize,
    round: usize,
    stage: &str,
) {
    let (url, server) =
        spawn_test_raw_http_response(status, TestRawHttpBody::Fixed(b"ok".to_vec())).await;
    let response = client.get(url).send().await.unwrap();
    let body = response_bytes_with_limit_and_body_timeout(response, 2, max_bytes)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"ok", "{stage} recovery round {round}");
    server.await.unwrap();
}

async fn run_external_coordinator_perf_round(
    manager: Arc<ExternalPoolManager>,
    direct_redis: Arc<RedisStore>,
    config: Arc<ExternalPoolsConfig>,
    route: Arc<ExternalRouteRequest>,
    requests: usize,
    concurrency: usize,
) -> Vec<Result<Duration, String>> {
    let authoritative_pools = match manager.load_authoritative_pool_snapshot().await {
        Ok(pools) => pools,
        Err(unavailable) => {
            return vec![
                Err(format!(
                    "PostgreSQL selection unavailable before Redis perf round: {}",
                    unavailable.kind.as_str()
                ));
                requests
            ];
        }
    };
    futures::stream::iter(0..requests)
        .map(|_| {
            let manager = manager.clone();
            let authoritative_pools = authoritative_pools.clone();
            let direct_redis = direct_redis.clone();
            let config = config.clone();
            let route = route.clone();
            async move {
                let started = Instant::now();
                let selection = manager
                    .select_pool_for_route_from_snapshot(
                        &authoritative_pools,
                        &HashSet::new(),
                        &config,
                        &route,
                    )
                    .await;
                let pool = selection.selected_pool.ok_or_else(|| {
                    format!(
                        "selection unavailable: coordinator={}, available={}, temporary={}",
                        selection.availability.coordinator_unavailable,
                        selection.availability.available_pools,
                        selection.availability.temporary_unavailable_pools
                    )
                })?;
                let lease = match manager.acquire_pool_for_route(&pool, &config, &route).await {
                    PoolAcquireResult::Acquired(lease) => lease,
                    PoolAcquireResult::Unavailable(unavailable) => {
                        return Err(format!("acquire unavailable: {}", unavailable.detail));
                    }
                };
                let lease_id = lease.lease_id.clone();
                lease.disarm();
                let released = direct_redis
                    .release_external_pool_confirmed_lease(pool.id, &lease_id)
                    .await
                    .map_err(|err| format!("direct release failed: {err}"))?;
                if !released {
                    return Err("direct release did not find the confirmed lease".to_string());
                }
                Ok(started.elapsed())
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await
}

fn external_perf_percentile_micros(sorted: &[u128], percentile: usize) -> u128 {
    let index = sorted
        .len()
        .saturating_sub(1)
        .saturating_mul(percentile.min(100))
        .saturating_add(99)
        / 100;
    sorted[index.min(sorted.len().saturating_sub(1))]
}

fn external_perf_process_rss_kib() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn external_perf_open_fd_count() -> Option<usize> {
    std::fs::read_dir("/dev/fd")
        .ok()
        .map(|entries| entries.count())
}

fn create_pool_request(name: &str, priority: i32, enabled: bool) -> CreateExternalPoolRequest {
    CreateExternalPoolRequest {
        name: name.to_string(),
        base_url: format!("https://{}.example.test", name),
        api_key: format!("sk-{}", name),
        auth_type: ExternalPoolAuthType::Bearer,
        enabled,
        priority,
        max_concurrent_requests: 1,
        usage_projection_mode: ExternalPoolUsageProjectionMode::PassThrough,
        stream_response_mode: None,
        request_body_mode: ExternalPoolRequestBodyMode::Normalized,
        raw_model_mode: ExternalPoolRawModelMode::None,
        auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
        pre_output_stream_retry_mode: ExternalPoolStreamRetryMode::Inherit,
        preserve_path: true,
        normalize_model_version_dots: false,
        model_mapping_mode: ExternalPoolModelMappingMode::ProcessedMapping,
        model_mapping_require_match: false,
        model_mapping_rules: Vec::new(),
        supported_models: Vec::new(),
        route_mode: ExternalPoolRouteMode::AllowAll,
        route_rules: Vec::new(),
        notes: None,
    }
}

async fn lock_external_pool_table(
    postgres: &PostgresStore,
) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    let mut connection = postgres.pool().acquire().await.unwrap();
    sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("LOCK TABLE external_upstream_pools IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *connection)
        .await
        .unwrap();
    connection
}

async fn unlock_external_pool_table(mut connection: sqlx::pool::PoolConnection<sqlx::Postgres>) {
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
}

async fn restore_dispatch_pool_configuration(
    postgres: &PostgresStore,
    pool_id: u64,
    base_url: &str,
    api_key: &str,
    supported_model: &str,
) {
    sqlx::query(
        r#"
        UPDATE external_upstream_pools
        SET base_url = $2,
            api_key = $3,
            auth_type = 'bearer',
            max_concurrent_requests = 1,
            usage_projection_mode = 'pass_through',
            stream_response_mode = NULL,
            request_body_mode = 'normalized',
            raw_model_mode = 'none',
            auto_disable_policy = 'inherit',
            model_mapping_mode = 'processed_mapping',
            model_mapping_require_match = false,
            model_mapping_rules = '[]'::jsonb,
            supported_models = $4,
            revision = revision + 1,
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(pool_id as i64)
    .bind(base_url)
    .bind(api_key)
    .bind(json!([supported_model]))
    .execute(postgres.pool())
    .await
    .unwrap();
}

async fn corrupt_dispatch_pool_configuration(postgres: &PostgresStore, pool_id: u64, case: &str) {
    let query = match case {
        "base_url" => {
            "UPDATE external_upstream_pools SET base_url = 'ftp://invalid.example', revision = revision + 1 WHERE id = $1"
        }
        "base_url_without_host" => {
            "UPDATE external_upstream_pools SET base_url = 'http://', revision = revision + 1 WHERE id = $1"
        }
        "api_key" => {
            "UPDATE external_upstream_pools SET api_key = '', revision = revision + 1 WHERE id = $1"
        }
        "auth_type" => {
            "UPDATE external_upstream_pools SET auth_type = 'corrupt', revision = revision + 1 WHERE id = $1"
        }
        "max_concurrent_requests" => {
            "UPDATE external_upstream_pools SET max_concurrent_requests = 0, revision = revision + 1 WHERE id = $1"
        }
        "usage_projection_mode" => {
            "UPDATE external_upstream_pools SET usage_projection_mode = 'corrupt', revision = revision + 1 WHERE id = $1"
        }
        "stream_response_mode" => {
            "UPDATE external_upstream_pools SET stream_response_mode = 'corrupt', revision = revision + 1 WHERE id = $1"
        }
        "request_body_mode" => {
            "UPDATE external_upstream_pools SET request_body_mode = 'corrupt', revision = revision + 1 WHERE id = $1"
        }
        "raw_model_mode" => {
            "UPDATE external_upstream_pools SET raw_model_mode = 'corrupt', revision = revision + 1 WHERE id = $1"
        }
        "auto_disable_policy" => {
            "UPDATE external_upstream_pools SET auto_disable_policy = 'corrupt', revision = revision + 1 WHERE id = $1"
        }
        "model_mapping_mode" => {
            "UPDATE external_upstream_pools SET model_mapping_mode = 'corrupt', revision = revision + 1 WHERE id = $1"
        }
        "model_mapping_rules" => {
            "UPDATE external_upstream_pools SET model_mapping_rules = '{\"invalid\":true}'::jsonb, revision = revision + 1 WHERE id = $1"
        }
        "model_mapping_rules_blank" => {
            "UPDATE external_upstream_pools SET model_mapping_rules = '[{\"enabled\":true,\"source\":\"\",\"target\":\"\"}]'::jsonb, revision = revision + 1 WHERE id = $1"
        }
        "supported_models" => {
            "UPDATE external_upstream_pools SET supported_models = '{\"invalid\":true}'::jsonb, revision = revision + 1 WHERE id = $1"
        }
        "supported_models_blank" => {
            "UPDATE external_upstream_pools SET supported_models = '[\"\"]'::jsonb, revision = revision + 1 WHERE id = $1"
        }
        _ => panic!("unknown corrupt dispatch pool case: {case}"),
    };
    sqlx::query(query)
        .bind(pool_id as i64)
        .execute(postgres.pool())
        .await
        .unwrap();
}

async fn wait_for_static_pool_background_idle(manager: &ExternalPoolManager, wait_for: Duration) {
    timeout(wait_for, async {
        loop {
            if manager.static_pool_snapshot_background_in_flight_for_test() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("static pool background refresh must become idle");
}

async fn wait_for_static_pool_pg_loads(
    manager: &ExternalPoolManager,
    expected: u64,
    wait_for: Duration,
) {
    timeout(wait_for, async {
        loop {
            if manager.static_pool_snapshot_pg_loads_for_test() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("static pool PostgreSQL load counter must reach expected value");
}

#[test]
fn stream_response_headers_disable_proxy_buffering() {
    let mut upstream_headers = HeaderMap::new();
    upstream_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );

    let mut builder = Response::builder().status(StatusCode::OK);
    apply_forwarded_response_headers(&mut builder, &upstream_headers, "req_01abc");
    disable_proxy_buffering_for_stream_response(&mut builder);
    let response = builder.body(()).expect("response should build");

    assert_eq!(response.headers()["x-accel-buffering"], "no");
    assert_eq!(response.headers()["request-id"], "req_01abc");
}

#[test]
fn external_pool_coordinator_breaker_uses_bounded_exponential_backoff() {
    let breaker = ExternalPoolCoordinatorBreaker::default();
    let expected = [1, 2, 4, 8, 16, 30, 30, 30];
    for (failure_count, expected_secs) in expected.into_iter().enumerate() {
        assert_eq!(
            breaker.backoff_for_failure(failure_count + 1),
            Duration::from_secs(expected_secs)
        );
    }
}

#[tokio::test]
async fn external_pool_coordinator_breaker_single_probe_and_cancellation_recover_for_five_rounds() {
    for round in 0..5 {
        let breaker = Arc::new(ExternalPoolCoordinatorBreaker::new(vec![
            Duration::from_millis(15),
            Duration::from_millis(30),
            Duration::from_millis(60),
        ]));
        let stale_success = breaker.try_begin().expect("closed breaker must admit");
        breaker
            .try_begin()
            .expect("closed breaker must admit concurrent operation")
            .failure(ExternalPoolCoordinatorFailureKind::Timeout);
        stale_success.success();

        for _ in 0..64 {
            assert!(
                breaker.try_begin().is_err(),
                "round {round}: stale success must not close a newer failure generation"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;

        let cancelled_probe = breaker
            .try_begin()
            .expect("round must admit exactly one recovery probe");
        for _ in 0..64 {
            assert!(
                breaker.try_begin().is_err(),
                "round {round}: concurrent recovery probes must fail fast"
            );
        }
        drop(cancelled_probe);
        assert!(
            breaker.try_begin().is_err(),
            "round {round}: cancelled probe must re-arm a bounded retry window"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;

        breaker
            .try_begin()
            .expect("cancelled probe must not permanently occupy recovery")
            .success();
        for _ in 0..5 {
            breaker
                .try_begin()
                .expect("successful probe must close the breaker")
                .success();
        }

        assert_eq!(breaker.stats.failures.load(Ordering::Relaxed), 1);
        assert_eq!(breaker.stats.recovery_probes.load(Ordering::Relaxed), 2);
        assert_eq!(breaker.stats.cancelled_probes.load(Ordering::Relaxed), 1);
        assert_eq!(breaker.stats.recoveries.load(Ordering::Relaxed), 1);
        assert!(breaker.stats.fail_fast.load(Ordering::Relaxed) >= 129);
    }
}

#[test]
fn external_pool_coordinator_first_failure_wave_has_a_hard_in_flight_cap_for_five_rounds() {
    for round in 0..5 {
        let breaker = Arc::new(ExternalPoolCoordinatorBreaker::default());
        let mut admitted = Vec::with_capacity(EXTERNAL_POOL_COORDINATOR_MAX_IN_FLIGHT);
        for _ in 0..EXTERNAL_POOL_COORDINATOR_MAX_IN_FLIGHT {
            admitted.push(
                breaker
                    .try_begin()
                    .expect("operations up to the hard cap must be admitted"),
            );
        }
        for _ in 0..10_000 {
            assert!(
                breaker.try_begin().is_err(),
                "round {round}: operations above the hard cap must not create waiting tasks"
            );
        }
        assert_eq!(
            breaker.stats.saturated.load(Ordering::Relaxed),
            10_000,
            "round {round}"
        );
        assert_eq!(breaker.operation_semaphore.available_permits(), 0);
        for permit in admitted {
            permit.success();
        }
        assert_eq!(
            breaker.operation_semaphore.available_permits(),
            EXTERNAL_POOL_COORDINATOR_MAX_IN_FLIGHT
        );
    }
}

#[test]
fn external_pool_selection_admission_has_a_low_hard_in_flight_cap_for_five_rounds() {
    for round in 0..5 {
        let breaker = Arc::new(ExternalPoolSelectionBreaker::default());
        let admitted = (0..EXTERNAL_POOL_SELECTION_MAX_IN_FLIGHT)
            .map(|_| {
                breaker
                    .try_begin()
                    .expect("selection operations up to the low hard cap must be admitted")
            })
            .collect::<Vec<_>>();
        for _ in 0..10_000 {
            assert!(
                breaker.try_begin().is_err(),
                "round {round}: selection work above the hard cap must fail before spawning PG work"
            );
        }
        assert_eq!(
            breaker.stats.saturated.load(Ordering::Relaxed),
            10_000,
            "round {round}"
        );
        assert_eq!(breaker.operation_semaphore.available_permits(), 0);
        for permit in admitted {
            permit.success();
        }
        assert_eq!(
            breaker.operation_semaphore.available_permits(),
            EXTERNAL_POOL_SELECTION_MAX_IN_FLIGHT
        );
    }
}

#[tokio::test]
async fn external_pool_selection_breaker_generation_fence_and_single_probe_recover_for_five_rounds()
{
    for round in 0..5 {
        let breaker = Arc::new(ExternalPoolSelectionBreaker::new(
            4,
            vec![Duration::from_millis(120), Duration::from_millis(240)],
            0,
        ));
        let stale_success = breaker.try_begin().expect("closed breaker must admit");
        breaker
            .try_begin()
            .expect("closed breaker must admit concurrent PG work")
            .failure(ExternalPoolSelectionFailureKind::PostgresTimeout);
        stale_success.success();

        for _ in 0..64 {
            assert!(
                breaker.try_begin().is_err(),
                "round {round}: a stale success must not close the failed generation"
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        let cancelled_probe = breaker
            .try_begin()
            .expect("one recovery probe must be admitted");
        for _ in 0..64 {
            assert!(
                breaker.try_begin().is_err(),
                "round {round}: concurrent recovery probes must fail fast"
            );
        }
        drop(cancelled_probe);
        assert!(
            breaker.try_begin().is_err(),
            "round {round}: a cancelled probe must re-arm backoff"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        breaker
            .try_begin()
            .expect("a later recovery probe must be admitted")
            .success();
        breaker
            .try_begin()
            .expect("successful recovery must close the breaker")
            .success();

        assert_eq!(breaker.stats.failures.load(Ordering::Relaxed), 1);
        assert_eq!(breaker.stats.recovery_probes.load(Ordering::Relaxed), 2);
        assert_eq!(breaker.stats.cancelled_probes.load(Ordering::Relaxed), 1);
        assert_eq!(breaker.stats.recoveries.load(Ordering::Relaxed), 1);
        assert!(breaker.stats.fail_fast.load(Ordering::Relaxed) >= 129);
    }
}

#[test]
fn external_release_ready_queue_bypasses_more_than_one_batch_of_poison_for_five_rounds() {
    for round in 0..5u64 {
        let semaphore = Arc::new(Semaphore::new(EXTERNAL_POOL_RELEASE_BATCH_SIZE + 2));
        let mut state = ExternalPoolReleaseDispatcherState::default();
        let now = Instant::now();
        for index in 0..=EXTERNAL_POOL_RELEASE_BATCH_SIZE {
            let intent = ExternalPoolReleaseIntent {
                pool_id: round + 1,
                lease_id: format!("poison-{round}-{index}"),
                release_kind: ExternalPoolLeaseReleaseKind::Confirmed,
            };
            state.order.push_back(intent.clone());
            state.pending.insert(
                intent,
                ExternalPoolPendingRelease {
                    _permit: semaphore.clone().try_acquire_owned().unwrap(),
                    failures: 20,
                    next_attempt_at: now + Duration::from_secs(30),
                },
            );
        }
        let healthy = ExternalPoolReleaseIntent {
            pool_id: round + 10_000,
            lease_id: format!("healthy-{round}"),
            release_kind: ExternalPoolLeaseReleaseKind::Confirmed,
        };
        state.order.push_back(healthy.clone());
        state.pending.insert(
            healthy.clone(),
            ExternalPoolPendingRelease {
                _permit: semaphore.clone().try_acquire_owned().unwrap(),
                failures: 0,
                next_attempt_at: now,
            },
        );

        let (batch, next_attempt_at) =
            state.next_ready_batch(now, EXTERNAL_POOL_RELEASE_BATCH_SIZE);
        assert_eq!(batch, vec![healthy], "round {round}");
        assert_eq!(next_attempt_at, Some(now + Duration::from_secs(30)));
    }
}

#[test]
fn external_release_transport_backoff_retains_but_does_not_send_new_work() {
    let semaphore = Arc::new(Semaphore::new(1));
    let now = Instant::now();
    let retry_at = now + Duration::from_secs(30);
    let intent = ExternalPoolReleaseIntent {
        pool_id: 1,
        lease_id: "queued-during-outage".to_string(),
        release_kind: ExternalPoolLeaseReleaseKind::Confirmed,
    };
    let mut state = ExternalPoolReleaseDispatcherState {
        system_failures: 10,
        system_retry_at: Some(retry_at),
        ..Default::default()
    };
    state.order.push_back(intent.clone());
    state.pending.insert(
        intent,
        ExternalPoolPendingRelease {
            _permit: semaphore.try_acquire_owned().unwrap(),
            failures: 0,
            next_attempt_at: now,
        },
    );
    assert_eq!(
        state.next_ready_batch(now, EXTERNAL_POOL_RELEASE_BATCH_SIZE),
        (Vec::new(), Some(retry_at))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_redis_external_poison_does_not_block_or_emit_false_capacity_for_five_rounds() {
    let Some(url) = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL") else {
        eprintln!("跳过 Redis external release poison 测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };

    for round in 0..5u64 {
        let mut config = Config::default();
        config.redis.url = Some(url.clone());
        config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
        let redis = Arc::new(RedisStore::connect(&config).await.unwrap());
        let poison_pool = 20_000 + round;
        redis
            .set_external_release_wrongtype_for_test(poison_pool, true)
            .await
            .unwrap();
        let capacity_signal = Arc::new(CapacitySignal::default());
        let selection_runtime_snapshot = Arc::new(SyncMutex::new(None));
        let dispatcher = ExternalPoolReleaseDispatcher::new(
            redis.clone(),
            capacity_signal.clone(),
            selection_runtime_snapshot,
        );
        let poison_permit = dispatcher.try_reserve().unwrap();
        dispatcher.enqueue(
            ExternalPoolReleaseIntent {
                pool_id: poison_pool,
                lease_id: "poison".to_string(),
                release_kind: ExternalPoolLeaseReleaseKind::Confirmed,
            },
            poison_permit,
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while dispatcher.stats.retries.load(Ordering::Acquire) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: poison intent did not enter retry"));

        let completed_before = dispatcher.stats.completed.load(Ordering::Acquire);
        let mut capacity_waiter = capacity_signal.register();
        let healthy_permit = dispatcher.try_reserve().unwrap();
        dispatcher.enqueue(
            ExternalPoolReleaseIntent {
                pool_id: poison_pool + 100,
                lease_id: "already-absent".to_string(),
                release_kind: ExternalPoolLeaseReleaseKind::Confirmed,
            },
            healthy_permit,
        );
        tokio::time::timeout(Duration::from_millis(500), async {
            while dispatcher.stats.completed.load(Ordering::Acquire) == completed_before {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: healthy release was blocked by poison"));
        assert_eq!(dispatcher.pending_len(), 1, "round {round}");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), capacity_waiter.wait_for_change())
                .await
                .is_err(),
            "round {round}: removed=false must not create a capacity wake"
        );

        redis
            .set_external_release_wrongtype_for_test(poison_pool, false)
            .await
            .unwrap();
        assert!(
            dispatcher.drain(Duration::from_secs(2)).await.drained,
            "round {round}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_release_fallback_storm_is_bounded_for_five_rounds() {
    for round in 0..5 {
        tokio::time::timeout(Duration::from_secs(5), async {
            while external_pool_release_fallback_semaphore().available_permits()
                != EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("previous fallback tasks must drain");

        let accepted_before = EXTERNAL_POOL_RELEASE_FALLBACK_ACCEPTED.load(Ordering::Acquire);
        let rejected_before = EXTERNAL_POOL_RELEASE_FALLBACK_REJECTED.load(Ordering::Acquire);
        let finished_before = EXTERNAL_POOL_RELEASE_FALLBACK_FINISHED.load(Ordering::Acquire);
        let gate = Arc::new(Semaphore::new(0));
        let mut accepted = 0usize;

        for _ in 0..10_000 {
            let gate = gate.clone();
            accepted += usize::from(spawn_external_release_fallback(
                "test external release fallback bound",
                async move {
                    let _permit = gate.acquire().await.expect("test gate remains open");
                    Ok(())
                },
            ));
        }

        assert_eq!(
            accepted, EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT,
            "round {round}: fallback must never create waiting tasks beyond the hard cap"
        );
        assert_eq!(
            EXTERNAL_POOL_RELEASE_FALLBACK_ACCEPTED
                .load(Ordering::Acquire)
                .saturating_sub(accepted_before),
            EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT as u64
        );
        assert_eq!(
            EXTERNAL_POOL_RELEASE_FALLBACK_REJECTED
                .load(Ordering::Acquire)
                .saturating_sub(rejected_before),
            (10_000 - EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT) as u64
        );
        assert_eq!(
            external_pool_release_fallback_semaphore().available_permits(),
            0
        );

        gate.add_permits(accepted);
        tokio::time::timeout(Duration::from_secs(5), async {
            while EXTERNAL_POOL_RELEASE_FALLBACK_FINISHED
                .load(Ordering::Acquire)
                .saturating_sub(finished_before)
                < accepted as u64
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("accepted fallback tasks must finish");
        assert_eq!(
            external_pool_release_fallback_semaphore().available_permits(),
            EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_release_dispatcher_drains_10k_real_leases_after_commit_unknown_for_five_rounds()
 {
    let Ok(direct_redis_url) = std::env::var("KIRO_RS_TEST_REDIS_DIRECT_URL") else {
        eprintln!("跳过 10k external release 测试：未设置 KIRO_RS_TEST_REDIS_DIRECT_URL");
        return;
    };
    let Ok(toxiproxy_api) = std::env::var("KIRO_RS_TEST_TOXIPROXY_API") else {
        eprintln!("跳过 10k external release 测试：未设置 KIRO_RS_TEST_TOXIPROXY_API");
        return;
    };
    let proxy_name =
        std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").unwrap_or_else(|_| "redis".to_string());
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let mut direct_config = Config::default();
    direct_config.redis.url = Some(direct_redis_url.clone());
    direct_config.redis.key_prefix = manager.redis.key_prefix_for_test();
    let direct_redis = Arc::new(RedisStore::connect(&direct_config).await.unwrap());
    let raw_client = redis::Client::open(direct_redis_url).unwrap();
    let mut raw_redis = raw_client.get_multiplexed_async_connection().await.unwrap();
    let coordination_epoch = manager.external_pool_coordination_epoch().await.unwrap();
    let client = reqwest::Client::new();
    let toxic_base = format!(
        "{}/proxies/{}/toxics",
        toxiproxy_api.trim_end_matches('/'),
        proxy_name
    );
    let rss_start = external_perf_process_rss_kib();
    let fd_start = external_perf_open_fd_count();
    let mut rss_peak = rss_start;
    let mut fd_peak = fd_start;
    let worker_starts_before = manager
        .release_dispatcher
        .stats
        .worker_starts
        .load(Ordering::Acquire);

    for round in 0..5 {
        let pool_id = 90_000 + round as u64;
        let cooldown_key = format!("external_pool:{pool_id}:cooldown");
        let mut leases = Vec::with_capacity(10_000);
        for index in 0..10_000 {
            let lease_id = format!("release-storm-{round}-{index}");
            let acquired = direct_redis
                .acquire_external_pool_lease(
                    pool_id,
                    &lease_id,
                    &coordination_epoch,
                    10_000,
                    10_000,
                    Some(Duration::from_secs(60)),
                    std::slice::from_ref(&cooldown_key),
                )
                .await
                .unwrap();
            assert!(
                matches!(
                    acquired,
                    RedisExternalPoolLeaseAcquireResult::Acquired { .. }
                ),
                "round {round}, lease {index}: seed acquire failed: {acquired:?}"
            );
            let release_permit = manager
                .release_dispatcher
                .try_reserve()
                .expect("10k leases must fit within the explicit release capacity");
            leases.push(ExternalPoolLease {
                manager: manager.clone(),
                pool_id,
                lease_id,
                coordination_epoch: coordination_epoch.clone(),
                state: ExternalPoolLeaseState::Confirmed,
                max_age: Duration::from_secs(60),
                heartbeat: None,
                release_permit: Some(release_permit),
            });
        }
        let seeded = direct_redis
            .external_pool_coordinator_snapshot(
                pool_id,
                Some(Duration::from_secs(60)),
                std::slice::from_ref(&cooldown_key),
                &coordination_epoch,
            )
            .await
            .unwrap();
        assert_eq!(seeded.capacity.pool_in_flight_requests, 10_000);
        assert_eq!(seeded.capacity.global_in_flight_requests, 10_000);

        let toxic_name = format!("external-release-reset-peer-{round}");
        let _ = client
            .delete(format!("{toxic_base}/{toxic_name}"))
            .send()
            .await;
        let install = client
            .post(&toxic_base)
            .json(&json!({
                "name": toxic_name.clone(),
                "type": "reset_peer",
                "stream": "downstream",
                "toxicity": 1.0,
                "attributes": { "timeout": 0 },
            }))
            .send()
            .await
            .unwrap();
        assert!(install.status().is_success(), "round {round}: {install:?}");

        let enqueued_before = manager
            .release_dispatcher
            .stats
            .enqueued
            .load(Ordering::Acquire);
        let completed_before = manager
            .release_dispatcher
            .stats
            .completed
            .load(Ordering::Acquire);
        let deduplicated_before = manager
            .release_dispatcher
            .stats
            .deduplicated
            .load(Ordering::Acquire);
        let retries_before = manager
            .release_dispatcher
            .stats
            .retries
            .load(Ordering::Acquire);
        let worker_starts_round = manager
            .release_dispatcher
            .stats
            .worker_starts
            .load(Ordering::Acquire);
        let drop_started = Instant::now();
        drop(leases);
        let drop_elapsed = drop_started.elapsed();
        assert!(
            drop_elapsed < Duration::from_secs(2),
            "round {round}: 10k O(1) release registrations took {drop_elapsed:?}"
        );
        assert_eq!(manager.release_dispatcher.pending_len(), 10_000);

        let duplicate_permit = manager
            .release_dispatcher
            .try_reserve()
            .expect("duplicate test intent must reserve temporary capacity");
        manager.release_dispatcher.enqueue(
            ExternalPoolReleaseIntent {
                pool_id,
                lease_id: format!("release-storm-{round}-0"),
                release_kind: ExternalPoolLeaseReleaseKind::Confirmed,
            },
            duplicate_permit,
        );
        assert_eq!(manager.release_dispatcher.pending_len(), 10_000);
        assert_eq!(
            manager
                .release_dispatcher
                .stats
                .deduplicated
                .load(Ordering::Acquire),
            deduplicated_before + 1
        );

        let blocked = manager
            .drain_release_intents(Duration::from_millis(250))
            .await;
        assert!(!blocked.drained, "round {round}: fault must retain intents");
        assert_eq!(blocked.pending, 10_000, "round {round}");

        let remove = client
            .delete(format!("{toxic_base}/{toxic_name}"))
            .send()
            .await
            .unwrap();
        assert!(remove.status().is_success(), "round {round}: {remove:?}");
        let recovery_started = Instant::now();
        let drained = manager.drain_release_intents(Duration::from_secs(20)).await;
        let recovery_elapsed = recovery_started.elapsed();
        assert!(drained.drained, "round {round}: {drained:?}");
        assert_eq!(drained.pending, 0, "round {round}");
        assert_eq!(
            drained.enqueued.saturating_sub(enqueued_before),
            10_000,
            "round {round}: duplicate registration must not add an intent"
        );
        assert_eq!(
            drained.completed.saturating_sub(completed_before),
            10_000,
            "round {round}: removed=false retries must still complete every intent"
        );
        assert!(drained.retries > retries_before, "round {round}");
        assert_eq!(
            drained.worker_starts.saturating_sub(worker_starts_round),
            1,
            "round {round}: a 10k burst must use one fixed worker"
        );
        assert_eq!(
            manager.release_dispatcher.capacity.available_permits(),
            EXTERNAL_POOL_RELEASE_CAPACITY,
            "round {round}: every release reservation must return"
        );

        let final_snapshot = direct_redis
            .external_pool_coordinator_snapshot(
                pool_id,
                Some(Duration::from_secs(60)),
                std::slice::from_ref(&cooldown_key),
                &coordination_epoch,
            )
            .await
            .unwrap();
        assert_eq!(
            final_snapshot.capacity,
            crate::storage::redis_cache::ExternalPoolCapacityState::default(),
            "round {round}: pool/global leases must be zero after recovery"
        );
        let tombstones: i64 = redis::cmd("ZCARD")
            .arg(direct_redis.key(format!("external_pool:inflight:{pool_id}:released")))
            .query_async(&mut raw_redis)
            .await
            .unwrap();
        assert_eq!(tombstones, 0, "round {round}: confirmed release tombstones");

        rss_peak = rss_peak.max(external_perf_process_rss_kib());
        fd_peak = fd_peak.max(external_perf_open_fd_count());
        eprintln!(
            "external_release_10k round={round} drop_ms={} recovery_ms={} retries={} rss_kib={:?} fd={:?}",
            drop_elapsed.as_millis(),
            recovery_elapsed.as_millis(),
            drained.retries.saturating_sub(retries_before),
            external_perf_process_rss_kib(),
            external_perf_open_fd_count(),
        );
    }

    assert_eq!(
        manager
            .release_dispatcher
            .stats
            .worker_starts
            .load(Ordering::Acquire)
            .saturating_sub(worker_starts_before),
        5,
        "five 10k bursts must start exactly five fixed workers"
    );
    let all_reservations = (0..EXTERNAL_POOL_RELEASE_CAPACITY)
        .map(|_| {
            manager
                .release_dispatcher
                .try_reserve()
                .expect("release capacity must be fully restored")
        })
        .collect::<Vec<_>>();
    assert!(
        manager.release_dispatcher.try_reserve().is_none(),
        "release capacity must have a hard upper bound"
    );
    drop(all_reservations);
    assert_eq!(
        manager.release_dispatcher.capacity.available_permits(),
        EXTERNAL_POOL_RELEASE_CAPACITY
    );

    let rss_end = external_perf_process_rss_kib();
    let fd_end = external_perf_open_fd_count();
    if let (Some(start), Some(peak)) = (rss_start, rss_peak) {
        assert!(
            peak.saturating_sub(start) <= 128 * 1024,
            "10k release RSS growth exceeded 128 MiB: start={start}, peak={peak}"
        );
    }
    if let (Some(start), Some(end)) = (fd_start, fd_end) {
        assert!(
            end <= start.saturating_add(8),
            "release recovery leaked file descriptors: start={start}, peak={fd_peak:?}, end={end}"
        );
    }
    eprintln!(
        "external_release_10k resources rss_start={rss_start:?} rss_peak={rss_peak:?} rss_end={rss_end:?} fd_start={fd_start:?} fd_peak={fd_peak:?} fd_end={fd_end:?}"
    );
    postgres.drop_test_schema().await.unwrap();
}

#[test]
fn inference_attempt_rejection_never_creates_pool_cooldown_for_five_rounds() {
    for _ in 0..5 {
        for rejection in [
            InferenceAttemptRejection::Exhausted,
            InferenceAttemptRejection::ReservedForFallback,
            InferenceAttemptRejection::DownstreamCommitted,
        ] {
            let error = ExternalForwardError::dispatch_rejected(
                ExternalPoolError {
                    status: Some(StatusCode::SERVICE_UNAVAILABLE),
                    message: "dispatch unavailable".to_string(),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: None,
                    protocol_error: None,
                },
                Some("claude-sonnet-4-5".to_string()),
                rejection,
            );

            assert_eq!(error.attempt_rejection, Some(rejection));
            assert!(!error.err.retryable);
            assert!(error.err.cooldown.is_none());
            assert!(error.err.auto_disable_reason.is_none());
        }
    }
}

#[test]
fn inference_attempt_rejection_keeps_attempt_list_empty_and_public_error_masked() {
    for round in 0..5 {
        let mut route = test_route("claude-sonnet-4-5");
        route.error_id = format!("req_01mask{round}");
        let error = external_attempt_rejection_final_error(&route, Vec::new());

        assert!(error.attempts.is_empty());
        assert_eq!(error.pool_id, None);
        assert_eq!(error.pool_name, None);
        assert_eq!(error.route_error_type, "inference_attempt_policy");
        let public_message = error.public_message(&route.error_id);
        assert_public_message_hides_internal_routing(&public_message);
        assert!(!public_message.contains("attempt"));
        assert!(!public_message.contains("pool"));
    }
}

#[test]
fn pool_auto_disable_policy_can_override_global_switch() {
    let mut config = ExternalPoolsConfig::default();
    config.external_pool_auto_disable_enabled = false;

    assert!(!pool_auto_disable_policy_enabled(
        ExternalPoolAutoDisablePolicy::Inherit,
        &config
    ));
    assert!(pool_auto_disable_policy_enabled(
        ExternalPoolAutoDisablePolicy::Enabled,
        &config
    ));
    assert!(!pool_auto_disable_policy_enabled(
        ExternalPoolAutoDisablePolicy::Disabled,
        &config
    ));

    config.external_pool_auto_disable_enabled = true;
    assert!(pool_auto_disable_policy_enabled(
        ExternalPoolAutoDisablePolicy::Inherit,
        &config
    ));
}

#[test]
fn external_pool_default_retry_attempts_cover_eligible_pools_and_payload_guard_retry() {
    assert_eq!(
        PoolAvailabilitySnapshot {
            eligible_pools: 0,
            ..PoolAvailabilitySnapshot::default()
        }
        .default_retry_attempts(false),
        1
    );
    assert_eq!(
        PoolAvailabilitySnapshot {
            eligible_pools: 2,
            ..PoolAvailabilitySnapshot::default()
        }
        .default_retry_attempts(false),
        2
    );
    assert_eq!(
        PoolAvailabilitySnapshot {
            eligible_pools: 2,
            ..PoolAvailabilitySnapshot::default()
        }
        .default_retry_attempts(true),
        3
    );
}

#[test]
fn external_pool_skip_reason_respects_enabled_switches_and_capacity() {
    let mut config = ExternalPoolsConfig::default();
    config.external_pools_enabled = false;
    let mut pool = test_pool("https://pool.example.test", true);

    assert_eq!(
        ExternalPoolManager::skip_reason(&pool, 0, 0, 0, &config).as_deref(),
        Some("external_pools_disabled")
    );

    config.external_pools_enabled = true;
    pool.enabled = false;
    assert_eq!(
        ExternalPoolManager::skip_reason(&pool, 0, 0, 0, &config).as_deref(),
        Some("disabled")
    );

    pool.enabled = true;
    assert_eq!(
        ExternalPoolManager::skip_reason(&pool, 0, 0, 3, &config).as_deref(),
        Some("cooldown")
    );

    pool.max_concurrent_requests = 2;
    assert_eq!(
        ExternalPoolManager::skip_reason(&pool, 2, 0, 0, &config).as_deref(),
        Some("pool_concurrency_full")
    );

    config.external_pool_global_max_concurrent_requests = 4;
    assert_eq!(
        ExternalPoolManager::skip_reason(&pool, 0, 4, 0, &config).as_deref(),
        Some("global_concurrency_full")
    );

    assert!(ExternalPoolManager::skip_reason(&pool, 0, 3, 0, &config).is_none());
}

#[test]
fn external_pool_candidate_selection_handles_multiple_backup_pools() {
    let config = ExternalPoolsConfig::default();
    let mut primary = test_pool("https://primary.example.test", true);
    primary.id = 11;
    primary.priority = 1;
    primary.max_concurrent_requests = 1;
    let mut secondary = test_pool("https://secondary.example.test", true);
    secondary.id = 22;
    secondary.priority = 2;
    secondary.max_concurrent_requests = 1;
    let mut tertiary = test_pool("https://tertiary.example.test", true);
    tertiary.id = 33;
    tertiary.priority = 3;
    tertiary.max_concurrent_requests = 1;

    let selected = select_external_pool_candidate(
        vec![
            (secondary.clone(), 0, 0),
            (tertiary.clone(), 0, 0),
            (primary.clone(), 0, 0),
        ],
        &config,
    )
    .expect("candidate should be selected");
    assert_eq!(selected.id, primary.id);

    let selected = select_external_pool_candidate(
        vec![(secondary.clone(), 0, 0), (tertiary.clone(), 0, 0)],
        &config,
    )
    .expect("fallback candidate should be selected when primary is excluded/full");
    assert_eq!(selected.id, secondary.id);

    primary.priority = 1;
    secondary.priority = 1;
    primary.max_concurrent_requests = 2;
    secondary.max_concurrent_requests = 4;
    let selected = select_external_pool_candidate(
        vec![(primary.clone(), 1, 0), (secondary.clone(), 1, 0)],
        &config,
    )
    .expect("lower same-priority load should be selected");
    assert_eq!(selected.id, secondary.id);
}

#[test]
fn external_pool_candidate_selection_penalizes_transient_failures() {
    let config = ExternalPoolsConfig::default();
    let mut failing_primary = test_pool("https://failing-primary.example.test", true);
    failing_primary.id = 41;
    failing_primary.priority = 1;
    failing_primary.max_concurrent_requests = 4;
    let mut healthy_backup = test_pool("https://healthy-backup.example.test", true);
    healthy_backup.id = 42;
    healthy_backup.priority = 10;
    healthy_backup.max_concurrent_requests = 4;
    let selected = select_external_pool_candidate(
        vec![
            (failing_primary.clone(), 0, 1),
            (healthy_backup.clone(), 0, 0),
        ],
        &config,
    )
    .expect("healthy backup should be selected once the primary accumulated a transient failure");
    assert_eq!(selected.id, healthy_backup.id);
}

#[tokio::test]
async fn external_pool_manager_respects_disabled_switch_and_disabled_pools() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let mut config = ExternalPoolsConfig::default();
    config.external_pools_enabled = false;

    let disabled = postgres
        .create_external_pool(create_pool_request("external-disabled", 1, false))
        .await
        .unwrap();
    let enabled = postgres
        .create_external_pool(create_pool_request("external-enabled", 2, true))
        .await
        .unwrap();

    assert!(!manager.has_available_pool(&config).await);
    let disabled_outcome = manager
        .forward_with_failover_result(config.clone(), test_route("claude-sonnet-4-5"))
        .await;
    let ExternalPoolForwardOutcome::FinalError(disabled_error) = disabled_outcome else {
        panic!("the global external-pool switch must reject without forwarding");
    };
    assert_eq!(disabled_error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(disabled_error.route_error_type, "external_pool_unavailable");
    assert!(!disabled_error.retryable);
    assert!(disabled_error.attempts.is_empty());

    config.external_pools_enabled = true;
    let selected = manager
        .select_pool(&HashSet::new(), &config)
        .await
        .expect("enabled external pool should be selected");
    assert_eq!(selected.id, enabled.id);
    assert_ne!(selected.id, disabled.id);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_manager_selects_multiple_pools_by_priority_and_capacity() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 0,
        ..ExternalPoolsConfig::default()
    };

    let primary = postgres
        .create_external_pool(create_pool_request("external-primary", 1, true))
        .await
        .unwrap();
    let secondary = postgres
        .create_external_pool(create_pool_request("external-secondary", 2, true))
        .await
        .unwrap();
    let tertiary = postgres
        .create_external_pool(create_pool_request("external-tertiary", 3, true))
        .await
        .unwrap();

    let first = manager
        .select_pool(&HashSet::new(), &config)
        .await
        .expect("primary pool should be selected first");
    assert_eq!(first.id, primary.id);

    let first_lease = match manager.acquire_pool(&primary, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(_) => panic!("primary pool lease should be acquired"),
    };
    let second = manager
        .select_pool(&HashSet::new(), &config)
        .await
        .expect("secondary pool should be selected when primary is full");
    assert_eq!(second.id, secondary.id);

    let second_lease = match manager.acquire_pool(&secondary, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(_) => panic!("secondary pool lease should be acquired"),
    };
    let third = manager
        .select_pool(&HashSet::new(), &config)
        .await
        .expect("tertiary pool should be selected when higher-priority pools are full");
    assert_eq!(third.id, tertiary.id);

    drop(first_lease);
    drop(second_lease);
    let mut after_release = None;
    for _ in 0..20 {
        if let Some(pool) = manager.select_pool(&HashSet::new(), &config).await {
            if pool.id == primary.id {
                after_release = Some(pool);
                break;
            }
            after_release = Some(pool);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let after_release = after_release.expect("primary should be selected again after release");
    assert_eq!(after_release.id, primary.id);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_manager_distinguishes_global_capacity_from_no_pool() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let mut config = ExternalPoolsConfig::default();
    config.external_pools_enabled = true;
    config.external_pool_global_max_concurrent_requests = 1;

    let primary = postgres
        .create_external_pool(create_pool_request("external-global-a", 1, true))
        .await
        .unwrap();
    let secondary = postgres
        .create_external_pool(create_pool_request("external-global-b", 2, true))
        .await
        .unwrap();

    let lease = match manager.acquire_pool(&primary, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(_) => panic!("primary pool lease should be acquired"),
    };

    assert!(
        manager
            .select_pool(&HashSet::new(), &config)
            .await
            .is_none()
    );
    assert!(manager.has_eligible_pool(&config).await);
    let snapshot = manager
        .pool_availability_snapshot(&HashSet::new(), &config)
        .await;
    assert_eq!(snapshot.eligible_pools, 2);
    assert_eq!(snapshot.available_pools, 0);
    assert_eq!(snapshot.temporary_unavailable_pools, 2);
    assert_eq!(snapshot.wait_reason, Some(PoolCapacityWaitReason::Full));

    drop(lease);
    let mut selected = None;
    for _ in 0..20 {
        if let Some(pool) = manager.select_pool(&HashSet::new(), &config).await {
            selected = Some(pool);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let selected = selected.expect("pool should become selectable after global lease release");
    assert!(selected.id == primary.id || selected.id == secondary.id);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_manager_uncached_snapshot_detects_full_pool_after_available_cache() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 0,
        ..ExternalPoolsConfig::default()
    };

    let pool = postgres
        .create_external_pool(create_pool_request("external-stale-cache", 1, true))
        .await
        .unwrap();

    let cached_available = manager
        .pool_availability_snapshot(&HashSet::new(), &config)
        .await;
    assert_eq!(cached_available.eligible_pools, 1);
    assert_eq!(cached_available.available_pools, 1);

    let lease = match manager.acquire_pool(&pool, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(_) => panic!("pool lease should be acquired"),
    };
    let selection = manager
        .scan_pool_availability_uncached(&HashSet::new(), &config, true, None, None, None)
        .await;
    assert!(selection.selected_pool.is_none());
    let uncached_full = selection.availability;
    assert_eq!(uncached_full.eligible_pools, 1);
    assert_eq!(uncached_full.available_pools, 0);
    assert_eq!(uncached_full.temporary_unavailable_pools, 1);
    assert_eq!(
        uncached_full.wait_reason,
        Some(PoolCapacityWaitReason::Full)
    );

    drop(lease);
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_immediate_availability_requires_current_capacity_and_recovers() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 0,
        ..ExternalPoolsConfig::default()
    };

    let pool = postgres
        .create_external_pool(create_pool_request("external-immediate-capacity", 1, true))
        .await
        .unwrap();

    assert!(
        manager
            .has_immediately_available_pool_for_model(
                &config,
                "claude-sonnet-4",
                Duration::from_secs(2),
            )
            .await,
        "a healthy pool with an open lease slot can immediately take local fallback traffic"
    );

    let lease = match manager.acquire_pool(&pool, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(unavailable) => {
            panic!(
                "pool lease should be acquired, got {:?}",
                unavailable.reason
            )
        }
    };
    assert!(
        !manager
            .has_immediately_available_pool_for_model(
                &config,
                "claude-sonnet-4",
                Duration::from_secs(2),
            )
            .await,
        "a full external pool must not change local credential dispatch into fail-fast fallback"
    );

    drop(lease);
    let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
    assert!(drained.drained, "external release should drain");

    assert!(
        manager
            .has_immediately_available_pool_for_model(
                &config,
                "claude-sonnet-4",
                Duration::from_secs(2),
            )
            .await,
        "external immediate availability must recover after the external lease is released"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_cached_immediate_availability_is_no_wait_under_pg_lock_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 0,
        ..ExternalPoolsConfig::default()
    };

    postgres
        .create_external_pool(create_pool_request("external-cached-no-wait", 1, true))
        .await
        .unwrap();

    for round in 1..=5 {
        manager.invalidate_static_pool_snapshot();
        let blocker = lock_external_pool_table(&postgres).await;
        let loads_before = manager.authoritative_pool_snapshot_pg_loads_for_test();
        let wave_manager = manager.clone();
        let wave_config = config.clone();
        let started = Instant::now();
        let wave = tokio::spawn(async move {
            futures::future::join_all((0..128).map(|_| {
                let manager = wave_manager.clone();
                let config = wave_config.clone();
                async move {
                    manager
                        .has_cached_immediately_available_pool_for_model(&config, "claude-sonnet-4")
                }
            }))
            .await
        });
        let results = timeout(Duration::from_millis(250), wave)
            .await
            .unwrap_or_else(|_| {
                panic!("round {round}: cached local gate waited for locked Postgres")
            })
            .unwrap();
        assert!(
            results.iter().all(|available| !*available),
            "round {round}: cold cached gate must preserve local semantics until a snapshot is ready"
        );
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "round {round}: cached local gate must not wait for authoritative refresh"
        );
        assert!(
            manager.authoritative_pool_snapshot_pg_loads_for_test() <= loads_before + 1,
            "round {round}: c128 cached local gate may start at most one background PG refresh"
        );

        unlock_external_pool_table(blocker).await;
        let snapshot = timeout(
            Duration::from_secs(2),
            manager.load_authoritative_pool_snapshot(),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: background refresh did not recover"))
        .expect("authoritative snapshot should recover after table unlock");
        assert!(
            !snapshot.is_empty(),
            "round {round}: recovered authoritative snapshot should contain the pool"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_cached_immediate_availability_uses_cached_runtime_capacity() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 0,
        ..ExternalPoolsConfig::default()
    };

    let pool = postgres
        .create_external_pool(create_pool_request("external-cached-runtime", 1, true))
        .await
        .unwrap();

    assert!(
        !manager.has_cached_immediately_available_pool_for_model(&config, "claude-sonnet-4"),
        "cold cached route gate must not synchronously read Redis/PgSQL"
    );
    assert!(
        manager
            .has_immediately_available_pool_for_model(
                &config,
                "claude-sonnet-4",
                Duration::from_secs(2),
            )
            .await,
        "authoritative availability should populate the runtime cache"
    );
    assert!(
        manager.has_cached_immediately_available_pool_for_model(&config, "claude-sonnet-4"),
        "cached route gate should use the warmed authoritative/runtime snapshots"
    );

    let lease = match manager.acquire_pool(&pool, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(unavailable) => {
            panic!("pool lease should be acquired: {}", unavailable.detail)
        }
    };
    assert!(
        !manager
            .has_immediately_available_pool_for_model(
                &config,
                "claude-sonnet-4",
                Duration::from_secs(2),
            )
            .await,
        "authoritative availability should observe the full external pool"
    );
    assert!(
        !manager.has_cached_immediately_available_pool_for_model(&config, "claude-sonnet-4"),
        "cached route gate must not steer local traffic to a full external pool"
    );

    drop(lease);
    let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
    assert!(drained.drained, "external release should drain");
    assert!(
        manager
            .has_immediately_available_pool_for_model(
                &config,
                "claude-sonnet-4",
                Duration::from_secs(2),
            )
            .await,
        "authoritative availability should recover after release"
    );
    assert!(
        manager.has_cached_immediately_available_pool_for_model(&config, "claude-sonnet-4"),
        "cached route gate should recover after release and runtime cache refresh"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_fallback_eligibility_bypasses_stale_empty_cache() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let mut config = ExternalPoolsConfig::default();
    config.external_pools_enabled = true;

    let cached_empty = manager
        .pool_availability_snapshot(&HashSet::new(), &config)
        .await;
    assert_eq!(cached_empty.eligible_pools, 0);
    assert_eq!(cached_empty.available_pools, 0);

    let mut request = create_pool_request("external-after-empty-cache", 1, true);
    request.supported_models = vec!["claude-sonnet-4".to_string()];
    postgres.create_external_pool(request).await.unwrap();

    assert!(
        manager
            .has_eligible_pool_for_model(&config, "claude-sonnet-4")
            .await,
        "fallback eligibility must observe a newly created model-limited pool without waiting for cache expiry"
    );
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "claude-opus-4")
            .await,
        "model-limited pools must not be eligible for a different model"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_static_eligibility_snapshot_singleflights_models_and_body_modes() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_static_pool_snapshot_ttl(Duration::from_secs(30));
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };

    let mut normalized = create_pool_request("static-normalized-sonnet", 1, true);
    normalized.supported_models = vec!["claude-sonnet-4".to_string()];
    postgres.create_external_pool(normalized).await.unwrap();

    let mut raw = create_pool_request("static-raw-opus", 2, true);
    raw.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    raw.supported_models = vec!["claude-opus-4".to_string()];
    postgres.create_external_pool(raw).await.unwrap();

    postgres
        .create_external_pool(create_pool_request("static-disabled", 3, false))
        .await
        .unwrap();

    for round in 0..3 {
        for concurrency in [1usize, 8, 32] {
            manager.invalidate_static_pool_snapshot();
            manager.redis.reset_external_pool_hot_path_round_trips();
            let loads_before = manager.static_pool_snapshot_pg_loads_for_test();
            let checks = futures::future::join_all((0..concurrency).map(|index| {
                let manager = manager.clone();
                let config = config.clone();
                async move {
                    match index % 5 {
                        0 => {
                            manager
                                .has_eligible_pool_for_model(&config, "claude-sonnet-4")
                                .await
                        }
                        1 => {
                            manager
                                .has_eligible_pool_for_body_mode_and_model(
                                    &config,
                                    ExternalPoolRequestBodyMode::Normalized,
                                    Some("claude-sonnet-4"),
                                )
                                .await
                        }
                        2 => {
                            manager
                                .has_eligible_pool_for_body_mode_and_model(
                                    &config,
                                    ExternalPoolRequestBodyMode::RawPassthrough,
                                    Some("claude-opus-4"),
                                )
                                .await
                        }
                        3 => {
                            manager
                                .has_eligible_pool_for_body_mode_and_model(
                                    &config,
                                    ExternalPoolRequestBodyMode::Normalized,
                                    Some("claude-opus-4"),
                                )
                                .await
                        }
                        _ => {
                            !manager
                                .has_eligible_pool_for_model(&config, "claude-future-9")
                                .await
                        }
                    }
                }
            }))
            .await;

            assert!(
                checks.iter().all(|passed| *passed),
                "round {round}, concurrency {concurrency}: mixed eligibility mismatch"
            );
            assert_eq!(
                manager
                    .static_pool_snapshot_pg_loads_for_test()
                    .saturating_sub(loads_before),
                1,
                "round {round}, concurrency {concurrency}: one generation must issue exactly one PostgreSQL pool-list query"
            );
            assert_eq!(
                manager.static_pool_snapshot_pool_count_for_test(),
                Some(3),
                "the cache must contain the complete pool list, not a model-filtered entry"
            );
            assert_eq!(
                manager.redis.external_pool_hot_path_round_trips(),
                0,
                "round {round}, concurrency {concurrency}: static eligibility must not read Redis"
            );

            let second_wave = futures::future::join_all((0..32).map(|index| {
                let manager = manager.clone();
                let config = config.clone();
                async move {
                    manager
                        .has_eligible_pool_for_model(
                            &config,
                            if index % 2 == 0 {
                                "claude-sonnet-4"
                            } else {
                                "claude-opus-4"
                            },
                        )
                        .await
                }
            }))
            .await;
            assert!(second_wave.iter().all(|eligible| *eligible));
            assert_eq!(
                manager.static_pool_snapshot_pg_loads_for_test(),
                loads_before + 1,
                "different models must share the same complete static snapshot"
            );
            assert_eq!(manager.redis.external_pool_hot_path_round_trips(), 0);
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_fallback_eligibility_uses_model_not_body_mode() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let mut raw = create_pool_request("body-parity-raw", 1, true);
    raw.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    raw.supported_models = vec!["model-raw-only".to_string()];
    postgres.create_external_pool(raw).await.unwrap();
    let mut normalized = create_pool_request("body-parity-normalized", 2, true);
    normalized.supported_models = vec!["model-normalized-only".to_string()];
    postgres.create_external_pool(normalized).await.unwrap();

    for round in 0..5 {
        manager.invalidate_static_pool_snapshot();
        manager.redis.reset_external_pool_hot_path_round_trips();
        assert!(
            manager
                .has_eligible_pool_for_body_mode_and_model(
                    &config,
                    ExternalPoolRequestBodyMode::RawPassthrough,
                    Some("model-raw-only"),
                )
                .await,
            "round {round}: raw-only model must match by supported model"
        );
        assert!(
            manager
                .has_eligible_pool_for_body_mode_and_model(
                    &config,
                    ExternalPoolRequestBodyMode::Normalized,
                    Some("model-raw-only"),
                )
                .await,
            "round {round}: body mode must not hide a model-supported raw pool"
        );
        assert!(
            manager
                .has_eligible_pool_for_body_mode_and_model(
                    &config,
                    ExternalPoolRequestBodyMode::Normalized,
                    Some("model-normalized-only"),
                )
                .await,
            "round {round}: normalized-only model must match by supported model"
        );
        assert!(
            manager
                .has_eligible_pool_for_body_mode_and_model(
                    &config,
                    ExternalPoolRequestBodyMode::RawPassthrough,
                    Some("model-normalized-only"),
                )
                .await,
            "round {round}: body mode must not hide a model-supported normalized pool"
        );
        assert!(
            manager
                .has_eligible_pool_for_model(&config, "model-raw-only")
                .await
        );
        assert!(
            manager
                .has_eligible_pool_for_model(&config, "model-normalized-only")
                .await
        );
        assert_eq!(manager.redis.external_pool_hot_path_round_trips(), 0);
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn external_pool_static_snapshot_swr_avoids_pg_lock_hol_c32_c128_for_three_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_static_pool_snapshot_timing(
        Duration::from_millis(20),
        Duration::from_secs(5),
        Duration::from_secs(1),
        Duration::from_millis(500),
    );
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let mut request = create_pool_request("static-swr-lock", 1, true);
    request.supported_models = vec!["model-static-swr".to_string()];
    postgres.create_external_pool(request).await.unwrap();
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "model-static-swr")
            .await
    );

    for round in 0..3 {
        for concurrency in [32usize, 128] {
            wait_for_static_pool_background_idle(&manager, Duration::from_secs(1)).await;
            tokio::time::sleep(Duration::from_millis(30)).await;
            let blocker = lock_external_pool_table(&postgres).await;
            manager.redis.reset_external_pool_hot_path_round_trips();
            let loads_before = manager.static_pool_snapshot_pg_loads_for_test();
            let refreshes_before = manager.static_pool_snapshot_background_refreshes_for_test();
            let barrier = Arc::new(tokio::sync::Barrier::new(concurrency + 1));
            let handles = (0..concurrency)
                .map(|_| {
                    let manager = manager.clone();
                    let config = config.clone();
                    let barrier = barrier.clone();
                    tokio::spawn(async move {
                        barrier.wait().await;
                        let started = Instant::now();
                        let eligible = manager
                            .has_eligible_pool_for_model(&config, "model-static-swr")
                            .await;
                        (eligible, started.elapsed().as_micros())
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait().await;
            let completed = timeout(
                Duration::from_millis(250),
                futures::future::join_all(handles),
            )
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "round {round}, concurrency {concurrency}: same-generation stale readers blocked behind PostgreSQL refresh"
                )
            });
            let mut latency_micros = Vec::with_capacity(concurrency);
            for result in completed {
                let (eligible, elapsed_micros) = result.unwrap();
                assert!(eligible, "round {round}, concurrency {concurrency}");
                latency_micros.push(elapsed_micros);
            }
            latency_micros.sort_unstable();
            let p95 = external_perf_percentile_micros(&latency_micros, 95);
            let p99 = external_perf_percentile_micros(&latency_micros, 99);
            assert!(
                p99 < 250_000,
                "round {round}, concurrency {concurrency}: p99 {p99}us must remain below the held PG lock interval"
            );
            wait_for_static_pool_pg_loads(&manager, loads_before + 1, Duration::from_millis(100))
                .await;
            assert_eq!(
                manager.static_pool_snapshot_pg_loads_for_test(),
                loads_before + 1,
                "round {round}, concurrency {concurrency}: one stale generation may start one PG refresh"
            );
            assert_eq!(
                manager.static_pool_snapshot_background_refreshes_for_test(),
                refreshes_before + 1,
                "round {round}, concurrency {concurrency}: one background task must own the refresh"
            );
            assert_eq!(
                manager.static_pool_snapshot_background_in_flight_for_test(),
                1,
                "round {round}, concurrency {concurrency}: the sole refresh remains blocked on the table lock"
            );
            assert_eq!(manager.redis.external_pool_hot_path_round_trips(), 0);
            eprintln!(
                "static_pool_swr round={round} concurrency={concurrency} p50_us={} p95_us={p95} p99_us={p99} pg_loads=1 background_tasks=1 redis_rtt=0",
                external_perf_percentile_micros(&latency_micros, 50),
            );

            unlock_external_pool_table(blocker).await;
            wait_for_static_pool_background_idle(&manager, Duration::from_millis(750)).await;
            assert_eq!(
                manager.static_pool_snapshot_cached_generation_for_test(),
                Some(manager.static_pool_snapshot_generation_for_test())
            );
        }
    }

    assert_eq!(
        manager.static_pool_snapshot_background_in_flight_for_test(),
        0
    );
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_static_eligibility_ttl_recovers_cross_instance_changes_for_three_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_static_pool_snapshot_timing(
        Duration::from_millis(250),
        Duration::from_secs(5),
        Duration::from_millis(100),
        Duration::from_millis(500),
    );
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };

    for round in 0..3 {
        let model = format!("claude-cross-instance-{round}");
        manager.invalidate_static_pool_snapshot();
        assert!(!manager.has_eligible_pool_for_model(&config, &model).await);

        let mut request = create_pool_request(&format!("cross-instance-{round}"), 1, true);
        request.supported_models = vec![model.clone()];
        let pool = postgres.create_external_pool(request).await.unwrap();
        assert!(
            !manager.has_eligible_pool_for_model(&config, &model).await,
            "round {round}: a peer mutation remains boundedly stale before TTL"
        );

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !manager.has_eligible_pool_for_model(&config, &model).await,
            "round {round}: the first expired read returns same-generation stale while refreshing"
        );
        wait_for_static_pool_background_idle(&manager, Duration::from_millis(750)).await;
        assert!(
            manager.has_eligible_pool_for_model(&config, &model).await,
            "round {round}: background TTL refresh must discover a peer-created pool"
        );

        postgres.soft_delete_external_pool(pool.id).await.unwrap();
        assert!(
            manager.has_eligible_pool_for_model(&config, &model).await,
            "round {round}: the cached snapshot remains stable inside its TTL"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            manager.has_eligible_pool_for_model(&config, &model).await,
            "round {round}: the first expired read returns same-generation stale while refreshing"
        );
        wait_for_static_pool_background_idle(&manager, Duration::from_millis(750)).await;
        assert!(
            !manager.has_eligible_pool_for_model(&config, &model).await,
            "round {round}: background TTL refresh must drop a peer-deleted pool"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_static_eligibility_invalidation_observes_every_pool_mutation() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_static_pool_snapshot_ttl(Duration::from_secs(30));
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };

    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "claude-sonnet-4")
            .await
    );
    let mut request = create_pool_request("static-invalidation", 1, true);
    request.supported_models = vec!["claude-sonnet-4".to_string()];
    let pool = postgres.create_external_pool(request).await.unwrap();
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "claude-sonnet-4")
            .await,
        "a cached generation must remain stable until explicit invalidation"
    );

    let mut expected_generation = manager.static_pool_snapshot_generation_for_test();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    expected_generation = manager.static_pool_snapshot_generation_for_test();
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "claude-sonnet-4")
            .await
    );

    postgres
        .set_external_pool_supported_models(pool.id, vec!["claude-opus-4".to_string()])
        .await
        .unwrap();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    expected_generation = manager.static_pool_snapshot_generation_for_test();
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "claude-sonnet-4")
            .await
    );
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "claude-opus-4")
            .await
    );

    postgres
        .set_external_pool_enabled(pool.id, false)
        .await
        .unwrap();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    expected_generation = manager.static_pool_snapshot_generation_for_test();
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "claude-opus-4")
            .await
    );

    postgres
        .set_external_pool_enabled(pool.id, true)
        .await
        .unwrap();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    expected_generation = manager.static_pool_snapshot_generation_for_test();
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "claude-opus-4")
            .await
    );

    postgres
        .update_external_pool(
            pool.id,
            UpdateExternalPoolRequest {
                request_body_mode: Some(ExternalPoolRequestBodyMode::RawPassthrough),
                ..UpdateExternalPoolRequest::default()
            },
        )
        .await
        .unwrap();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    expected_generation = manager.static_pool_snapshot_generation_for_test();
    assert!(
        manager
            .has_eligible_pool_for_body_mode_and_model(
                &config,
                ExternalPoolRequestBodyMode::RawPassthrough,
                Some("claude-opus-4"),
            )
            .await
    );
    assert!(
        manager
            .has_eligible_pool_for_body_mode_and_model(
                &config,
                ExternalPoolRequestBodyMode::Normalized,
                Some("claude-opus-4"),
            )
            .await
    );

    postgres
        .auto_disable_external_pool(pool.id, "auth_error", "fixture", 60)
        .await
        .unwrap();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    expected_generation = manager.static_pool_snapshot_generation_for_test();
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "claude-opus-4")
            .await
    );

    postgres
        .clear_external_pool_auto_disabled(pool.id)
        .await
        .unwrap();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    expected_generation = manager.static_pool_snapshot_generation_for_test();
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "claude-opus-4")
            .await
    );

    postgres.soft_delete_external_pool(pool.id).await.unwrap();
    manager.invalidate_static_pool_snapshot();
    assert!(manager.static_pool_snapshot_generation_for_test() > expected_generation);
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "claude-opus-4")
            .await
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn external_pool_static_snapshot_invalidation_never_serves_old_generation_under_pg_lock() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_static_pool_snapshot_timing(
        Duration::from_millis(20),
        Duration::from_secs(5),
        Duration::from_secs(1),
        Duration::from_millis(500),
    );
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let mut request = create_pool_request("static-invalidation-lock", 1, true);
    request.supported_models = vec!["model-generation-old".to_string()];
    let pool = postgres.create_external_pool(request).await.unwrap();
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "model-generation-old")
            .await
    );

    postgres
        .set_external_pool_supported_models(pool.id, vec!["model-generation-new".to_string()])
        .await
        .unwrap();
    manager.invalidate_static_pool_snapshot();
    let invalidated_generation = manager.static_pool_snapshot_generation_for_test();
    let blocker = lock_external_pool_table(&postgres).await;
    manager.redis.reset_external_pool_hot_path_round_trips();
    let loads_before = manager.static_pool_snapshot_pg_loads_for_test();
    let refreshes_before = manager.static_pool_snapshot_background_refreshes_for_test();
    let completed_count = Arc::new(AtomicU64::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(129));
    let handles = (0..128usize)
        .map(|index| {
            let manager = manager.clone();
            let config = config.clone();
            let completed_count = completed_count.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                let started = Instant::now();
                let eligible = manager
                    .has_eligible_pool_for_model(
                        &config,
                        if index % 2 == 0 {
                            "model-generation-old"
                        } else {
                            "model-generation-new"
                        },
                    )
                    .await;
                completed_count.fetch_add(1, Ordering::AcqRel);
                (eligible, started.elapsed().as_micros())
            })
        })
        .collect::<Vec<_>>();
    barrier.wait().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        completed_count.load(Ordering::Acquire),
        0,
        "generation mismatch must not return stale data while authoritative refresh is blocked"
    );
    wait_for_static_pool_pg_loads(&manager, loads_before + 1, Duration::from_millis(100)).await;
    let completed = timeout(
        Duration::from_millis(1_500),
        futures::future::join_all(handles),
    )
    .await
    .expect("cold invalidated generation must fail closed within the 500ms PG timeout");
    let mut latency_micros = Vec::with_capacity(128);
    for result in completed {
        let (eligible, elapsed_micros) = result.unwrap();
        assert!(
            !eligible,
            "negative cache must fail closed for both old and new models"
        );
        latency_micros.push(elapsed_micros);
    }
    latency_micros.sort_unstable();
    let p95 = external_perf_percentile_micros(&latency_micros, 95);
    let p99 = external_perf_percentile_micros(&latency_micros, 99);
    assert!(
        p95 >= 350_000,
        "p95 {p95}us did not observe the held PG lock"
    );
    assert!(
        p99 < 1_200_000,
        "p99 {p99}us exceeded the bounded refresh window"
    );
    assert_eq!(
        manager.static_pool_snapshot_pg_loads_for_test(),
        loads_before + 1
    );
    assert_eq!(
        manager.static_pool_snapshot_background_refreshes_for_test(),
        refreshes_before,
        "cold/invalidation refresh must remain foreground singleflight"
    );
    assert_eq!(
        manager.static_pool_snapshot_cached_generation_for_test(),
        Some(invalidated_generation)
    );
    assert_eq!(manager.static_pool_snapshot_pool_count_for_test(), Some(0));
    assert_eq!(manager.redis.external_pool_hot_path_round_trips(), 0);

    unlock_external_pool_table(blocker).await;
    tokio::time::sleep(Duration::from_millis(1_050)).await;
    let recovery_loads_before = manager.static_pool_snapshot_pg_loads_for_test();
    let recovery_refreshes_before = manager.static_pool_snapshot_background_refreshes_for_test();
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "model-generation-new")
            .await,
        "the retry-triggering read still returns the fail-closed negative snapshot"
    );
    wait_for_static_pool_background_idle(&manager, Duration::from_millis(750)).await;
    assert_eq!(
        manager.static_pool_snapshot_pg_loads_for_test(),
        recovery_loads_before + 1
    );
    assert_eq!(
        manager.static_pool_snapshot_background_refreshes_for_test(),
        recovery_refreshes_before + 1
    );
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "model-generation-new")
            .await
    );
    assert!(
        !manager
            .has_eligible_pool_for_model(&config, "model-generation-old")
            .await
    );

    tokio::time::sleep(Duration::from_millis(30)).await;
    let blocker = lock_external_pool_table(&postgres).await;
    let timeout_loads_before = manager.static_pool_snapshot_pg_loads_for_test();
    let timeout_refreshes_before = manager.static_pool_snapshot_background_refreshes_for_test();
    let timeout_started = Instant::now();
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "model-generation-new")
            .await,
        "same-generation last-good data must remain immediately available"
    );
    wait_for_static_pool_pg_loads(
        &manager,
        timeout_loads_before + 1,
        Duration::from_millis(100),
    )
    .await;
    wait_for_static_pool_background_idle(&manager, Duration::from_millis(750)).await;
    assert!(
        timeout_started.elapsed() < Duration::from_millis(750),
        "the sole background task must terminate after its 500ms PG timeout"
    );
    assert_eq!(
        manager.static_pool_snapshot_background_refreshes_for_test(),
        timeout_refreshes_before + 1
    );
    assert_eq!(
        manager.static_pool_snapshot_background_in_flight_for_test(),
        0
    );

    let second_wave = futures::future::join_all((0..128).map(|_| {
        let manager = manager.clone();
        let config = config.clone();
        async move {
            manager
                .has_eligible_pool_for_model(&config, "model-generation-new")
                .await
        }
    }))
    .await;
    assert!(second_wave.iter().all(|eligible| *eligible));
    assert_eq!(
        manager.static_pool_snapshot_pg_loads_for_test(),
        timeout_loads_before + 1,
        "the one-second failure retry window must suppress PG RPM fanout"
    );
    assert_eq!(
        manager.static_pool_snapshot_background_refreshes_for_test(),
        timeout_refreshes_before + 1,
        "the one-second failure retry window must not spawn more tasks"
    );
    assert_eq!(manager.redis.external_pool_hot_path_round_trips(), 0);
    unlock_external_pool_table(blocker).await;

    tokio::time::sleep(Duration::from_millis(1_050)).await;
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "model-generation-new")
            .await
    );
    wait_for_static_pool_background_idle(&manager, Duration::from_millis(750)).await;
    assert!(
        manager
            .has_eligible_pool_for_model(&config, "model-generation-new")
            .await
    );
    assert_eq!(
        manager.static_pool_snapshot_background_in_flight_for_test(),
        0
    );
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_authoritative_selection_pg_lock_is_typed_bounded_and_recovers() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("selection-pg-timeout", 1, true))
        .await
        .unwrap();
    let route = test_route("claude-sonnet-4-6");
    let blocker = lock_external_pool_table(&postgres).await;
    manager.redis.reset_external_pool_hot_path_round_trips();
    let started = Instant::now();
    let blocked = manager
        .select_pool_for_route(&HashSet::new(), &config, &route)
        .await;
    let elapsed = started.elapsed();
    assert!(blocked.selected_pool.is_none());
    assert!(blocked.availability.coordinator_unavailable);
    assert_eq!(
        blocked.availability.coordinator_unavailable_kind,
        Some(PoolCoordinatorUnavailableKind::PostgresTimeout)
    );
    assert_eq!(
        blocked.availability.wait_reason,
        Some(PoolCapacityWaitReason::CoordinatorUnavailable)
    );
    assert!(
        elapsed >= Duration::from_millis(1_800),
        "elapsed={elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(3_000),
        "elapsed={elapsed:?}"
    );
    assert_eq!(
        manager.redis.external_pool_hot_path_round_trips(),
        0,
        "PG selection timeout must fail before Redis selection/acquire"
    );

    unlock_external_pool_table(blocker).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let recovered = manager
        .select_pool_for_route(&HashSet::new(), &config, &route)
        .await;
    let selected = recovered
        .selected_pool
        .expect("authoritative selection must recover after the PG lock is released");
    assert_eq!(selected.id, pool.id);
    let cold_bootstrap_rtts = manager.redis.external_pool_hot_path_round_trips();
    assert!(
        (1..=5).contains(&cold_bootstrap_rtts),
        "the first successful selection after a coordinator-cold PG timeout may bootstrap Redis coordination, but must stay bounded; rtts={cold_bootstrap_rtts}"
    );

    manager.redis.reset_external_pool_hot_path_round_trips();
    *manager.selection_runtime_snapshot.lock() = None;
    let warm = manager
        .select_pool_for_route(&HashSet::new(), &config, &route)
        .await;
    let warm_selected = warm
        .selected_pool
        .expect("warm authoritative selection must keep working after bootstrap");
    assert_eq!(warm_selected.id, pool.id);
    assert_eq!(
        manager.redis.external_pool_hot_path_round_trips(),
        1,
        "warm authoritative selection must use one batched Redis runtime snapshot RTT"
    );
    let lease = match manager
        .acquire_pool_for_route(&warm_selected, &config, &route)
        .await
    {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(unavailable) => {
            panic!("atomic lease must recover: {}", unavailable.detail)
        }
    };
    assert_eq!(
        manager.redis.external_pool_hot_path_round_trips(),
        2,
        "warm authoritative selection plus atomic acquire must use two Redis RTTs"
    );
    drop(lease);
    let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
    assert!(drained.drained);
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_authoritative_snapshot_singleflights_c32_c128_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request("authoritative-singleflight", 1, true))
        .await
        .unwrap();

    for concurrency in [32usize, 128] {
        for round in 1..=5 {
            manager.invalidate_static_pool_snapshot();
            let blocker = lock_external_pool_table(&postgres).await;
            let loads_before = manager.authoritative_pool_snapshot_pg_loads_for_test();
            let wave_manager = manager.clone();
            let wave = tokio::spawn(async move {
                futures::future::join_all(
                    (0..concurrency).map(|_| wave_manager.load_authoritative_pool_snapshot()),
                )
                .await
            });
            timeout(Duration::from_millis(500), async {
                loop {
                    if manager.authoritative_pool_snapshot_pg_loads_for_test() > loads_before {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("round {round}, c{concurrency}: authoritative PG query did not start")
            });
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert_eq!(
                manager.authoritative_pool_snapshot_pg_loads_for_test(),
                loads_before + 1,
                "round {round}, c{concurrency}: one cold generation must issue one PG query"
            );
            unlock_external_pool_table(blocker).await;
            let results = timeout(Duration::from_secs(2), wave)
                .await
                .unwrap_or_else(|_| panic!("round {round}, c{concurrency}: wave timed out"))
                .unwrap();
            assert_eq!(results.len(), concurrency);
            assert!(results.iter().all(|result| {
                result
                    .as_ref()
                    .is_ok_and(|pools| pools.iter().any(|candidate| candidate.id == pool.id))
            }));
            assert_eq!(
                manager.authoritative_pool_snapshot_pg_loads_for_test(),
                loads_before + 1,
                "round {round}, c{concurrency}: waiters must reuse the same completed wave"
            );
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_authoritative_refresh_survives_leader_cancellation_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request("authoritative-cancel-safe", 1, true))
        .await
        .unwrap();

    for round in 1..=5 {
        manager.invalidate_static_pool_snapshot();
        let blocker = lock_external_pool_table(&postgres).await;
        let loads_before = manager.authoritative_pool_snapshot_pg_loads_for_test();
        let caller_manager = manager.clone();
        let caller =
            tokio::spawn(async move { caller_manager.load_authoritative_pool_snapshot().await });
        timeout(Duration::from_millis(500), async {
            loop {
                if manager.authoritative_pool_snapshot_pg_loads_for_test() > loads_before {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: manager-owned PG refresh did not start"));
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled(), "round {round}");
        unlock_external_pool_table(blocker).await;

        let snapshot = timeout(
            Duration::from_secs(1),
            manager.load_authoritative_pool_snapshot(),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: surviving refresh did not publish"))
        .expect("manager-owned refresh must survive caller cancellation");
        assert!(snapshot.iter().any(|candidate| candidate.id == pool.id));
        assert_eq!(
            manager.authoritative_pool_snapshot_pg_loads_for_test(),
            loads_before + 1,
            "round {round}: cancellation must not start a replacement PG query"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_authoritative_pg_timeout_c128_is_one_query_and_recovers_for_three_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request("authoritative-timeout-wave", 1, true))
        .await
        .unwrap();

    for round in 1..=3 {
        manager.invalidate_static_pool_snapshot();
        let blocker = lock_external_pool_table(&postgres).await;
        let loads_before = manager.authoritative_pool_snapshot_pg_loads_for_test();
        let saturated_before = manager.selection_saturated.load(Ordering::Acquire);
        let first_wave = timeout(
            Duration::from_secs(3),
            futures::future::join_all((0..128).map(|_| manager.load_authoritative_pool_snapshot())),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: first timeout wave exceeded hard deadline"));
        assert!(first_wave.iter().all(|result| {
            result
                .as_ref()
                .is_err_and(|error| error.kind == ExternalPoolSelectionFailureKind::PostgresTimeout)
        }));
        assert_eq!(
            manager.authoritative_pool_snapshot_pg_loads_for_test(),
            loads_before + 1,
            "round {round}: c128 timeout wave must issue one PG list"
        );
        assert_eq!(
            manager.selection_saturated.load(Ordering::Acquire),
            saturated_before,
            "round {round}: singleflight must not shed callers at the 32-query admission cap"
        );

        let negative_wave =
            futures::future::join_all((0..128).map(|_| manager.load_authoritative_pool_snapshot()))
                .await;
        assert!(negative_wave.iter().all(Result::is_err));
        assert_eq!(
            manager.authoritative_pool_snapshot_pg_loads_for_test(),
            loads_before + 1,
            "round {round}: failure cache must suppress immediate retry fanout"
        );
        unlock_external_pool_table(blocker).await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        let recovered = timeout(
            Duration::from_secs(1),
            futures::future::join_all((0..128).map(|_| manager.load_authoritative_pool_snapshot())),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: recovery wave timed out"));
        assert!(recovered.iter().all(|result| {
            result
                .as_ref()
                .is_ok_and(|pools| pools.iter().any(|candidate| candidate.id == pool.id))
        }));
        assert_eq!(
            manager.authoritative_pool_snapshot_pg_loads_for_test(),
            loads_before + 2,
            "round {round}: recovery c128 must issue exactly one new PG list"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_local_mutation_seed_survives_pg_lock_for_scheduler_degraded_fallback() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let fake = AuxiliaryFallbackFakeServer::start().await;
    let mut request = create_pool_request("local-mutation-seed", 1, true);
    request.base_url = fake.base_url.clone();
    request.supported_models = vec!["claude-sonnet-4-6".to_string()];
    let pool = postgres
        .create_external_pool_unmasked(request)
        .await
        .expect("create unmasked pool for manager-local seed");
    assert!(
        pool.api_key.is_some(),
        "local manager seed must retain secret"
    );

    let blocker = lock_external_pool_table(&postgres).await;
    manager.notify_external_pool_data_changed_with_local_pool("test_local_seed", &pool);
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 4,
        external_pool_retry_max_attempts: 1,
        external_pool_capacity_mode: ExternalPoolCapacityMode::FailFast,
        ..ExternalPoolsConfig::default()
    };

    let eligible = timeout(
        Duration::from_millis(250),
        manager.has_eligible_pool_for_model(&config, "claude-sonnet-4-6"),
    )
    .await
    .expect("local static eligibility seed must not wait for locked PostgreSQL");
    assert!(eligible);

    let selected = timeout(
        Duration::from_secs(1),
        manager.select_pool_for_route(&HashSet::new(), &config, &test_route("claude-sonnet-4-6")),
    )
    .await
    .expect("local authoritative seed must not wait for locked PostgreSQL")
    .selected_pool
    .expect("local authoritative seed should select the newly-created pool");
    assert_eq!(selected.id, pool.id);
    assert_eq!(selected.revision, pool.revision);
    assert!(
        selected.api_key.is_some(),
        "authoritative seed must retain secret"
    );

    let mut route = test_route("claude-sonnet-4-6");
    route.request_id = "req_local_mutation_seed_degraded".to_string();
    route.error_id = "err_local_mutation_seed_degraded".to_string();
    route.route_subtype = UsageRouteSubtype::ExternalFallbackAfterLocalAttempts;
    route.fallback_reason = Some("local_scheduler_redis_degraded".to_string());
    route.local_attempted = true;
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(1));
    let hits_before = fake.snapshot().3;
    let response = timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("scheduler degraded fallback should finish with local mutation seed under PG lock");
    let response = match response {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("scheduler degraded fallback must use seeded external pool: {error:?}")
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read seeded external fallback body");
    assert!(
        body.windows(b"external-ok".len())
            .any(|window| window == b"external-ok")
    );
    assert_eq!(fake.snapshot().3, hits_before + 1);

    unlock_external_pool_table(blocker).await;
    wait_for_static_pool_background_idle(&manager, Duration::from_secs(1)).await;
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_dispatch_fence_coalesces_only_in_flight_c32_c128_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request("dispatch-fence-singleflight", 1, true))
        .await
        .unwrap();

    for concurrency in [32usize, 128] {
        for round in 1..=5 {
            let blocker = lock_external_pool_table(&postgres).await;
            let loads_before = manager.dispatch_fence_pg_loads_for_test();
            let wave_manager = manager.clone();
            let wave_pool = pool.clone();
            let wave = tokio::spawn(async move {
                futures::future::join_all(
                    (0..concurrency).map(|_| wave_manager.validate_pool_dispatch_fence(&wave_pool)),
                )
                .await
            });
            timeout(Duration::from_millis(250), async {
                loop {
                    if manager.dispatch_fence_pg_loads_for_test() > loads_before {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!("round {round}, c{concurrency}: dispatch fence query did not start")
            });
            tokio::time::sleep(Duration::from_millis(25)).await;
            assert_eq!(
                manager.dispatch_fence_pg_loads_for_test(),
                loads_before + 1,
                "round {round}, c{concurrency}: concurrent same-revision fences must singleflight"
            );
            unlock_external_pool_table(blocker).await;
            let results = timeout(Duration::from_secs(1), wave)
                .await
                .unwrap_or_else(|_| panic!("round {round}, c{concurrency}: fence wave timed out"))
                .unwrap();
            assert!(
                results
                    .iter()
                    .all(|result| *result == PoolDispatchFenceResult::Current)
            );

            let sequential_before = manager.dispatch_fence_pg_loads_for_test();
            assert_eq!(
                manager.validate_pool_dispatch_fence(&pool).await,
                PoolDispatchFenceResult::Current
            );
            assert_eq!(
                manager.validate_pool_dispatch_fence(&pool).await,
                PoolDispatchFenceResult::Current
            );
            assert_eq!(
                manager.dispatch_fence_pg_loads_for_test(),
                sequential_before + 2,
                "round {round}, c{concurrency}: completed results must never become a TTL cache"
            );
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_dispatch_fence_pg_timeout_c128_is_one_query_and_recovers_for_three_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request("dispatch-fence-timeout-wave", 1, true))
        .await
        .unwrap();

    for round in 1..=3 {
        let blocker = lock_external_pool_table(&postgres).await;
        let loads_before = manager.dispatch_fence_pg_loads_for_test();
        let saturated_before = manager.selection_saturated.load(Ordering::Acquire);
        let first_wave = timeout(
            Duration::from_secs(1),
            futures::future::join_all(
                (0..128).map(|_| manager.validate_pool_dispatch_fence(&pool)),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: fence timeout wave exceeded deadline"));
        assert!(first_wave.iter().all(|result| {
            matches!(
                result,
                PoolDispatchFenceResult::CoordinatorUnavailable(unavailable)
                    if unavailable.kind == ExternalPoolSelectionFailureKind::PostgresTimeout
            )
        }));
        assert_eq!(
            manager.dispatch_fence_pg_loads_for_test(),
            loads_before + 1,
            "round {round}: c128 fence timeout must issue one PG query"
        );
        assert_eq!(
            manager.selection_saturated.load(Ordering::Acquire),
            saturated_before,
            "round {round}: same-revision c128 must not hit selection admission"
        );

        let negative_wave = futures::future::join_all(
            (0..128).map(|_| manager.validate_pool_dispatch_fence(&pool)),
        )
        .await;
        assert!(negative_wave.iter().all(|result| {
            matches!(result, PoolDispatchFenceResult::CoordinatorUnavailable(_))
        }));
        assert_eq!(
            manager.dispatch_fence_pg_loads_for_test(),
            loads_before + 1,
            "round {round}: open breaker must suppress immediate fence query fanout"
        );
        unlock_external_pool_table(blocker).await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        let recovered = timeout(
            Duration::from_secs(1),
            futures::future::join_all(
                (0..128).map(|_| manager.validate_pool_dispatch_fence(&pool)),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: fence recovery timed out"));
        assert!(
            recovered
                .iter()
                .all(|result| *result == PoolDispatchFenceResult::Current)
        );
        assert_eq!(
            manager.dispatch_fence_pg_loads_for_test(),
            loads_before + 2,
            "round {round}: recovery c128 must issue one new fence query"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_request_scoped_snapshot_is_reused_across_reselection() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    for index in 0..3 {
        postgres
            .create_external_pool(create_pool_request(
                &format!("request-snapshot-{index}"),
                index + 1,
                true,
            ))
            .await
            .unwrap();
    }
    let route = test_route("claude-sonnet-4-6");
    let attempts_before = manager
        .selection_breaker
        .stats
        .postgres_attempts
        .load(Ordering::Acquire);
    let authoritative_pools = manager
        .load_authoritative_pool_snapshot()
        .await
        .expect("one request-scoped PostgreSQL snapshot");
    let mut excluded = HashSet::new();
    for expected_priority in 1..=3 {
        let selection = manager
            .select_pool_for_route_from_snapshot(&authoritative_pools, &excluded, &config, &route)
            .await;
        let pool = selection
            .selected_pool
            .expect("each reselection must use another pool from the same snapshot");
        assert_eq!(pool.priority, expected_priority);
        excluded.insert(pool.id);
    }
    assert_eq!(
        manager
            .selection_breaker
            .stats
            .postgres_attempts
            .load(Ordering::Acquire)
            .saturating_sub(attempts_before),
        1,
        "reselection must not reload PostgreSQL inside one request"
    );
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_post_lease_revision_fence_rejects_disable_and_update_toctou() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let mut pool = postgres
        .create_external_pool(create_pool_request("dispatch-revision-fence", 1, true))
        .await
        .unwrap();

    for round in 0..3 {
        let lease = match manager.acquire_pool(&pool, &config).await {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(unavailable) => {
                panic!("round {round}: lease unavailable: {}", unavailable.detail)
            }
        };
        let disabled = postgres
            .set_external_pool_enabled(pool.id, false)
            .await
            .unwrap()
            .expect("pool remains present");
        assert_eq!(disabled.revision, pool.revision + 1);
        assert!(matches!(
            manager.validate_pool_dispatch_fence(&pool).await,
            PoolDispatchFenceResult::Changed
        ));
        drop(lease);
        assert!(
            manager
                .drain_release_intents(Duration::from_secs(5))
                .await
                .drained
        );

        let enabled = postgres
            .set_external_pool_enabled(pool.id, true)
            .await
            .unwrap()
            .expect("pool remains present");
        assert_eq!(enabled.revision, disabled.revision + 1);
        assert!(matches!(
            manager.validate_pool_dispatch_fence(&enabled).await,
            PoolDispatchFenceResult::Current
        ));
        pool = postgres
            .update_external_pool(
                pool.id,
                UpdateExternalPoolRequest {
                    notes: Some(format!("revision-fence-round-{round}")),
                    ..UpdateExternalPoolRequest::default()
                },
            )
            .await
            .unwrap()
            .expect("pool remains present");
        assert_eq!(pool.revision, enabled.revision + 1);
    }
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_dispatch_prepares_then_fences_before_attempt_and_http_send_for_five_rounds()
{
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let fake = AuxiliaryFallbackFakeServer::start().await;
    let mut request = create_pool_request("prepare-before-fence", 1, true);
    request.base_url = fake.base_url.clone();
    request.max_concurrent_requests = 4;
    let mut pool = postgres.create_external_pool(request).await.unwrap();
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 4,
        external_pool_retry_max_attempts: 1,
        ..ExternalPoolsConfig::default()
    };

    for round in 1..=5 {
        if round > 1 {
            pool = postgres
                .set_external_pool_enabled(pool.id, true)
                .await
                .unwrap()
                .expect("pool remains present");
            manager.invalidate_static_pool_snapshot();
        }
        let gate = TestDispatchAfterPrepareGate::new();
        manager.set_dispatch_after_prepare_gate(gate.clone());
        let budget = Arc::new(InferenceAttemptBudget::new(2));
        let mut route = test_route("claude-sonnet-4-6");
        route.inference_attempt_budget = budget.clone();
        route.request_id = format!("req_prepare_fence_{round}");
        route.error_id = format!("err_prepare_fence_{round}");
        let hits_before = fake.snapshot().3;
        let task_manager = manager.clone();
        let task_config = config.clone();
        let task = tokio::spawn(async move {
            task_manager
                .forward_with_failover_result(task_config, route)
                .await
        });

        timeout(Duration::from_secs(1), gate.prepared.wait())
            .await
            .unwrap_or_else(|_| panic!("round {round}: request preparation gate timed out"));
        let disabled = postgres
            .set_external_pool_enabled(pool.id, false)
            .await
            .unwrap()
            .expect("pool remains present");
        assert!(disabled.revision > pool.revision, "round {round}");
        gate.resume.wait().await;
        let outcome = timeout(Duration::from_secs(2), task)
            .await
            .unwrap_or_else(|_| panic!("round {round}: fenced dispatch did not finish"))
            .unwrap();
        assert!(
            matches!(outcome, ExternalPoolForwardOutcome::FinalError(_)),
            "round {round}: changed pool must not send"
        );
        assert_eq!(
            fake.snapshot().3,
            hits_before,
            "round {round}: stale URL/key/body snapshot reached the HTTP server"
        );
        assert_eq!(
            budget.snapshot().consumed,
            0,
            "round {round}: rejected stale dispatch must not consume inference attempt budget"
        );
        assert!(
            manager
                .drain_release_intents(Duration::from_secs(5))
                .await
                .drained,
            "round {round}: rejected stale lease must release"
        );
        pool = disabled;
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_external_pool_rows_are_isolated_and_fail_closed_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let mut healthy_request = create_pool_request("malformed-row-healthy", 1, true);
    healthy_request.supported_models = vec!["model-healthy-only".to_string()];
    let healthy = postgres
        .create_external_pool(healthy_request)
        .await
        .unwrap();
    let mut candidate_request = create_pool_request("malformed-row-candidate", 2, true);
    candidate_request.supported_models = vec!["model-corrupt-only".to_string()];
    let candidate_base_url = candidate_request.base_url.clone();
    let candidate_api_key = candidate_request.api_key.clone();
    let candidate = postgres
        .create_external_pool(candidate_request)
        .await
        .unwrap();
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let corrupt_cases = [
        "base_url",
        "base_url_without_host",
        "api_key",
        "auth_type",
        "max_concurrent_requests",
        "usage_projection_mode",
        "stream_response_mode",
        "request_body_mode",
        "raw_model_mode",
        "auto_disable_policy",
        "model_mapping_mode",
        "model_mapping_rules",
        "model_mapping_rules_blank",
        "supported_models",
        "supported_models_blank",
    ];

    for round in 1..=5 {
        for case in corrupt_cases {
            restore_dispatch_pool_configuration(
                &postgres,
                candidate.id,
                &candidate_base_url,
                &candidate_api_key,
                "model-corrupt-only",
            )
            .await;
            corrupt_dispatch_pool_configuration(&postgres, candidate.id, case).await;
            manager.invalidate_static_pool_snapshot();

            assert!(
                !manager
                    .has_eligible_pool_for_model(&config, "model-corrupt-only")
                    .await,
                "round {round}, case {case}: malformed row must not authorize static fallback"
            );
            assert!(
                manager
                    .has_eligible_pool_for_model(&config, "model-healthy-only")
                    .await,
                "round {round}, case {case}: healthy row must remain eligible"
            );
            let authoritative = manager
                .load_authoritative_pool_snapshot()
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "round {round}, case {case}: healthy rows must survive malformed peer: {}",
                        error.kind.as_str()
                    )
                });
            assert!(authoritative.iter().any(|pool| pool.id == healthy.id));
            assert!(
                authoritative.iter().all(|pool| pool.id != candidate.id),
                "round {round}, case {case}: malformed row entered authoritative dispatch snapshot"
            );
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_auto_disable_two_managers_claim_one_revision_transition_per_burst() {
    let Some((manager_a, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager_b = ExternalPoolManager::new(postgres.clone(), manager_a.redis.clone());
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_auto_disable_enabled: true,
        external_pool_auto_disable_on_auth_error: true,
        external_pool_auto_disable_failure_threshold: 3,
        external_pool_auto_disable_window_secs: 60,
        external_pool_auto_disable_duration_secs: 60,
        ..ExternalPoolsConfig::default()
    };
    let mut pool = postgres
        .create_external_pool(create_pool_request("auto-disable-two-manager", 1, true))
        .await
        .unwrap();

    for round in 0..3 {
        let revision_before = pool.revision;
        futures::future::join_all((0..128).map(|index| {
            let manager = if index % 2 == 0 {
                manager_a.clone()
            } else {
                manager_b.clone()
            };
            let config = config.clone();
            let pool = pool.clone();
            async move {
                manager
                    .auto_disable_pool_if_configured(
                        &pool,
                        &config,
                        "auth_error",
                        "test authentication failure",
                    )
                    .await;
            }
        }))
        .await;
        let disabled = postgres
            .get_external_pool(pool.id, true)
            .await
            .unwrap()
            .expect("pool remains present");
        assert!(disabled.auto_disabled, "round {round}");
        assert_eq!(
            disabled.revision,
            revision_before + 1,
            "round {round}: distributed claim and PG CAS permit one transition"
        );
        pool = postgres
            .clear_external_pool_auto_disabled(pool.id)
            .await
            .unwrap()
            .expect("pool remains present");
        assert_eq!(pool.revision, disabled.revision + 1);
    }
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_success_reset_coalesces_auto_disable_keys_and_obeys_task_cap() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool_id = 42;
    for reason in EXTERNAL_POOL_AUTO_DISABLE_REASONS {
        manager
            .redis
            .incr_with_ttl(
                format!("external_pool:{pool_id}:auto_disable_failures:{reason}"),
                60,
            )
            .await
            .unwrap();
    }
    manager
        .redis
        .incr_with_ttl(external_pool_transient_failure_key(pool_id), 60)
        .await
        .unwrap();
    let started_before = manager.success_reset_tasks_started.load(Ordering::Acquire);
    for _ in 0..10_000 {
        manager.reset_pool_auto_disable_failure_counts(pool_id);
    }
    timeout(Duration::from_secs(2), async {
        while manager
            .success_reset_tasks_in_flight
            .load(Ordering::Acquire)
            != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the one coalesced reset task must finish");
    assert_eq!(
        manager
            .success_reset_tasks_started
            .load(Ordering::Acquire)
            .saturating_sub(started_before),
        1
    );
    for reason in EXTERNAL_POOL_AUTO_DISABLE_REASONS {
        let value = manager
            .redis
            .get_json::<u64>(format!(
                "external_pool:{pool_id}:auto_disable_failures:{reason}"
            ))
            .await
            .unwrap();
        assert!(value.is_none(), "reason {reason} must be cleared");
    }
    let transient = manager
        .redis
        .get_json::<u64>(external_pool_transient_failure_key(pool_id))
        .await
        .unwrap();
    assert_eq!(
        transient,
        Some(1),
        "single successes must not erase soft failure health evidence during intermittent turbulence"
    );

    let held_permits = futures::future::join_all(
        (0..EXTERNAL_POOL_SUCCESS_RESET_MAX_TASKS)
            .map(|_| manager.success_reset_semaphore.clone().acquire_owned()),
    )
    .await
    .into_iter()
    .map(|result| result.unwrap())
    .collect::<Vec<_>>();
    let capped_before = manager.success_reset_tasks_started.load(Ordering::Acquire);
    for id in 10_000..10_128 {
        manager.reset_pool_auto_disable_failure_counts(id);
    }
    assert_eq!(
        manager.success_reset_tasks_started.load(Ordering::Acquire),
        capped_before,
        "no task may be spawned after the hard task cap is exhausted"
    );
    drop(held_permits);
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_soft_failure_streak_accumulates_and_requires_manual_clear_or_ttl() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool_id = 43;

    manager
        .record_external_pool_soft_failure(pool_id, "server_error")
        .await;
    manager
        .record_external_pool_soft_failure(pool_id, "server_error")
        .await;
    let streak = manager
        .redis
        .get_json::<u64>(external_pool_transient_failure_key(pool_id))
        .await
        .unwrap()
        .expect("soft failure streak should exist");
    assert_eq!(streak, 2);

    manager.reset_pool_auto_disable_failure_counts(pool_id);
    timeout(Duration::from_secs(2), async {
        while manager
            .success_reset_tasks_in_flight
            .load(Ordering::Acquire)
            != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("success reset should finish without clearing soft failure streak");

    let after_reset = manager
        .redis
        .get_json::<u64>(external_pool_transient_failure_key(pool_id))
        .await
        .unwrap();
    assert_eq!(
        after_reset,
        Some(2),
        "one later success should not reset accumulated soft failure streak"
    );

    let cleared = manager.clear_pool_cooldowns(pool_id).await.unwrap();
    assert!(
        cleared >= 1,
        "manual clear should remove soft failure streak"
    );
    let after_clear = manager
        .redis
        .get_json::<u64>(external_pool_transient_failure_key(pool_id))
        .await
        .unwrap();
    assert!(
        after_clear.is_none(),
        "manual clear should reset the soft failure streak"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_data_generation_invalidates_peer_without_clearing_on_policy_only_change() {
    let Some((manager_a, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager_b = ExternalPoolManager::new(postgres.clone(), manager_a.redis.clone())
        .with_static_pool_snapshot_ttl(Duration::from_secs(30));
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let mut request = create_pool_request("cross-instance-generation", 1, true);
    request.supported_models = vec!["model-before-event".to_string()];
    let pool = postgres.create_external_pool(request).await.unwrap();
    assert!(
        manager_b
            .has_eligible_pool_for_model(&config, "model-before-event")
            .await
    );
    let cached_generation = manager_b.static_pool_snapshot_generation_for_test();
    manager_b.invalidate_external_pool_policy_state();
    assert_eq!(
        manager_b.static_pool_snapshot_generation_for_test(),
        cached_generation,
        "policy-only config changes must not clear static pool data"
    );

    postgres
        .set_external_pool_supported_models(pool.id, vec!["model-after-event".to_string()])
        .await
        .unwrap();
    let distributed_generation = manager_a
        .redis
        .publish_external_pool_data_changed("test_peer_update", Some(pool.id))
        .await
        .unwrap();
    assert!(manager_b.observe_external_pool_data_event(
        &json!({ "generation": distributed_generation }).to_string()
    ));
    assert!(manager_b.static_pool_snapshot_generation_for_test() > cached_generation);
    assert!(
        manager_b
            .has_eligible_pool_for_model(&config, "model-after-event")
            .await
    );
    assert!(
        !manager_b
            .has_eligible_pool_for_model(&config, "model-before-event")
            .await
    );
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_static_eligibility_pg_failure_is_negative_cached_without_rpm_fanout() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_static_pool_snapshot_ttl(Duration::from_secs(30));
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    postgres.drop_test_schema().await.unwrap();

    for round in 0..3 {
        for concurrency in [1usize, 8, 32] {
            manager.invalidate_static_pool_snapshot();
            manager.redis.reset_external_pool_hot_path_round_trips();
            let loads_before = manager.static_pool_snapshot_pg_loads_for_test();
            let first_wave = futures::future::join_all((0..concurrency).map(|_| {
                let manager = manager.clone();
                let config = config.clone();
                async move {
                    manager
                        .has_eligible_pool_for_model(&config, "claude-sonnet-4")
                        .await
                }
            }))
            .await;
            assert!(first_wave.iter().all(|eligible| !eligible));
            assert_eq!(
                manager.static_pool_snapshot_pg_loads_for_test(),
                loads_before + 1,
                "round {round}, concurrency {concurrency}: a failed generation must singleflight one PostgreSQL attempt"
            );

            let second_wave = futures::future::join_all((0..concurrency).map(|_| {
                let manager = manager.clone();
                let config = config.clone();
                async move {
                    manager
                        .has_eligible_pool_for_model(&config, "claude-opus-4")
                        .await
                }
            }))
            .await;
            assert!(second_wave.iter().all(|eligible| !eligible));
            assert_eq!(
                manager.static_pool_snapshot_pg_loads_for_test(),
                loads_before + 1,
                "round {round}, concurrency {concurrency}: a cached load failure must not fan out retries"
            );
            assert_eq!(
                manager.redis.external_pool_hot_path_round_trips(),
                0,
                "static eligibility failure must remain independent of Redis"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_lease_touch_and_drop_release_are_accepted_and_drained() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 1,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-lease-drain", 1, true))
        .await
        .unwrap();
    let lease = match manager.acquire_pool(&pool, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(unavailable) => panic!(
            "external pool lease should be acquired: {}",
            unavailable.detail
        ),
    };

    assert!(
        manager
            .touch_pool(
                pool.id,
                &lease.lease_id,
                EXTERNAL_POOL_LEASE_MAX_AGE_SECS as usize * 2,
                &lease.coordination_epoch,
            )
            .await
            .unwrap(),
        "confirmed lease heartbeat must receive direct success feedback"
    );
    let (pool_in_flight, global_in_flight, _, _) =
        manager.pool_runtime_snapshot(pool.id).await.unwrap();
    assert_eq!(pool_in_flight, 1);
    assert_eq!(global_in_flight, 1);

    let enqueued_before = manager
        .release_dispatcher
        .stats
        .enqueued
        .load(Ordering::Acquire);
    drop(lease);
    let release_drain = manager.drain_release_intents(Duration::from_secs(5)).await;
    assert!(release_drain.drained, "accepted release task should drain");
    assert_eq!(release_drain.pending, 0);
    assert!(release_drain.enqueued >= enqueued_before.saturating_add(1));
    assert!(release_drain.completed >= release_drain.enqueued);
    let (pool_in_flight, global_in_flight, _, _) =
        manager.pool_runtime_snapshot(pool.id).await.unwrap();
    assert_eq!(
        pool_in_flight, 0,
        "drop release must not wait for lease TTL"
    );
    assert_eq!(
        global_in_flight, 0,
        "global release must not wait for lease TTL"
    );

    manager
        .redis
        .del("external_pool:inflight:lease_sequence")
        .await
        .unwrap();
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_confirmed_heartbeat_keeps_lease_alive_past_max_age_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 1,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-heartbeat-max-age", 1, true))
        .await
        .unwrap();
    let max_age = Duration::from_millis(900);
    let cooldown_keys = vec![format!("external_pool:{}:cooldown", pool.id)];

    for round in 0..5 {
        let lease = match manager
            .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
            .await
        {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(unavailable) => {
                panic!("round {round}: {}", unavailable.detail)
            }
        };
        tokio::time::sleep(Duration::from_millis(1_350)).await;
        let snapshot = manager
            .redis
            .external_pool_coordinator_snapshot(
                pool.id,
                Some(max_age),
                &cooldown_keys,
                &lease.coordination_epoch,
            )
            .await
            .unwrap();
        assert_eq!(
            snapshot.capacity,
            crate::storage::redis_cache::ExternalPoolCapacityState {
                pool_in_flight_requests: 1,
                global_in_flight_requests: 1,
            },
            "round {round}: heartbeat must keep the confirmed lease alive past max_age"
        );
        assert!(
            lease
                .heartbeat
                .as_ref()
                .unwrap()
                .state
                .attempts
                .load(Ordering::Relaxed)
                >= 2,
            "round {round}: expected repeated direct heartbeat feedback"
        );
        drop(lease);
        let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
        assert!(drained.drained, "round {round}: release must drain");
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_heartbeat_touch_false_marks_lease_lost_and_recovers_five_of_five() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 1,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-heartbeat-lost", 1, true))
        .await
        .unwrap();
    let max_age = Duration::from_millis(900);

    for round in 0..5 {
        let lease = match manager
            .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
            .await
        {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(unavailable) => {
                panic!("round {round}: {}", unavailable.detail)
            }
        };
        assert!(
            manager
                .redis
                .release_external_pool_confirmed_lease(pool.id, &lease.lease_id)
                .await
                .unwrap(),
            "round {round}: test must remove the active lease"
        );
        tokio::time::timeout(Duration::from_secs(2), lease.wait_until_lost())
            .await
            .expect("touch=false must be surfaced before the prune deadline");
        assert!(
            lease
                .heartbeat
                .as_ref()
                .unwrap()
                .state
                .lost
                .load(Ordering::Acquire),
            "round {round}: heartbeat loss feedback missing"
        );
        drop(lease);
        let recovered = match manager
            .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
            .await
        {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(unavailable) => {
                panic!("round {round}: recovery failed: {}", unavailable.detail)
            }
        };
        drop(recovered);
        let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
        assert!(
            drained.drained,
            "round {round}: recovery release must drain"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_cancelled_waiter_releases_redis_queue_lease() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = Arc::new(manager);
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_capacity_mode: ExternalPoolCapacityMode::Wait,
        external_pool_max_queued_requests: 1,
        external_pool_dispatch_max_wait_secs: 60,
        ..ExternalPoolsConfig::default()
    };

    let pool = postgres
        .create_external_pool(create_pool_request("external-cancelled-waiter", 1, true))
        .await
        .unwrap();
    let held_lease = match manager.acquire_pool(&pool, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(unavailable) => panic!(
            "external pool lease should be acquired before queueing: {}",
            unavailable.detail
        ),
    };

    let waiting_manager = manager.clone();
    let waiting = tokio::spawn(async move {
        waiting_manager
            .forward_with_failover_result(config, test_route("claude-sonnet-4-5"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager
                .redis
                .external_pool_dispatch_queue_size()
                .await
                .unwrap()
                == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("external waiter should acquire a Redis queue lease");

    waiting.abort();
    let join_error = match waiting.await {
        Err(error) => error,
        Ok(_) => panic!("aborted external queue waiter should not finish normally"),
    };
    assert!(join_error.is_cancelled());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager
                .redis
                .external_pool_dispatch_queue_size()
                .await
                .unwrap()
                == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled external waiter should release its queue lease before TTL");

    drop(held_lease);
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_dispatch_deadline_is_shared_with_capacity_wait() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_capacity_mode: ExternalPoolCapacityMode::Wait,
        external_pool_max_queued_requests: 1,
        external_pool_dispatch_max_wait_secs: 60,
        external_pool_request_timeout_secs: 1,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-shared-deadline", 1, true))
        .await
        .unwrap();
    let held_lease = match manager.acquire_pool(&pool, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(unavailable) => {
            panic!(
                "external pool lease should be held before waiting: {}",
                unavailable.detail
            )
        }
    };

    let mut route = test_route("claude-sonnet-4-5");
    route.started_at = Instant::now() - Duration::from_secs(2);
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("expired dispatch deadline should terminate promptly");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "capacity wait must use the request's remaining deadline"
    );
    match outcome {
        ExternalPoolForwardOutcome::FinalError(error) => {
            assert_eq!(error.route_error_type, "external_pool_deadline_exceeded");
        }
        ExternalPoolForwardOutcome::Response(_) => {
            panic!("a held pool must not produce a response after the deadline")
        }
    }

    drop(held_lease);
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_dispatch_uses_shared_request_deadline_before_pool_timeout() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_capacity_mode: ExternalPoolCapacityMode::Wait,
        external_pool_max_queued_requests: 1,
        external_pool_dispatch_max_wait_secs: 60,
        external_pool_request_timeout_secs: 60,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request(
            "external-shared-request-deadline",
            1,
            true,
        ))
        .await
        .unwrap();
    let held_lease = match manager.acquire_pool(&pool, &config).await {
        PoolAcquireResult::Acquired(lease) => lease,
        PoolAcquireResult::Unavailable(unavailable) => {
            panic!(
                "external pool lease should be held before waiting: {}",
                unavailable.detail
            )
        }
    };

    let route = test_route("claude-sonnet-4-5");
    route
        .inference_attempt_budget
        .set_dispatch_deadline_after(Duration::from_millis(50));
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("shared request deadline should terminate capacity wait");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shared deadline should win over the configured 60 second pool timeout"
    );
    match outcome {
        ExternalPoolForwardOutcome::FinalError(error) => {
            assert_eq!(error.route_error_type, "external_pool_deadline_exceeded");
        }
        ExternalPoolForwardOutcome::Response(_) => {
            panic!("a held pool must not produce a response after the shared deadline")
        }
    }

    drop(held_lease);
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_coordinator_failure_fails_closed_without_queue_admission() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_capacity_mode: ExternalPoolCapacityMode::Wait,
        external_pool_max_queued_requests: 1,
        external_pool_dispatch_max_wait_secs: 0,
        ..ExternalPoolsConfig::default()
    };
    let route = test_route("claude-sonnet-4-5");
    let mut queue_guard = None;
    let mut wait_started_at = None;
    let mut capacity_waiter = manager.capacity_signal.register();

    let decision = manager
        .handle_capacity_unavailable(
            &route,
            Vec::new(),
            &config,
            PoolCapacityWaitContext {
                reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                wait_for: None,
                cooldown_reason: None,
                cooldown_scope: None,
                cooldown_remaining_secs: None,
                eligible_pools: 0,
                available_pools: 0,
                temporary_unavailable_pools: 0,
                coordinator_unavailable_kind: Some(PoolCoordinatorUnavailableKind::RedisError),
            },
            &mut queue_guard,
            &mut wait_started_at,
            &mut capacity_waiter,
            None,
        )
        .await;

    let ExternalCapacityDecision::FinalError(error) = decision else {
        panic!("coordinator failure must fail closed without waiting");
    };
    assert_eq!(
        error.route_error_type,
        "external_pool_coordinator_unavailable"
    );
    assert!(queue_guard.is_none());
    assert_eq!(
        manager
            .redis
            .external_pool_dispatch_queue_size()
            .await
            .unwrap(),
        0
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn legacy_zero_external_wait_reaches_a_bounded_final_error() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_capacity_mode: ExternalPoolCapacityMode::Wait,
        external_pool_max_queued_requests: 1,
        external_pool_dispatch_max_wait_secs: 0,
        ..ExternalPoolsConfig::default()
    };
    assert_eq!(config.effective_dispatch_max_wait_secs(), 5);

    let route = test_route("claude-sonnet-4-5");
    let mut queue_guard = None;
    let mut wait_started_at = Some(Instant::now() - Duration::from_secs(31));
    let mut capacity_waiter = manager.capacity_signal.register();
    let decision = manager
        .handle_capacity_unavailable(
            &route,
            Vec::new(),
            &config,
            PoolCapacityWaitContext {
                reason: PoolCapacityWaitReason::Full,
                wait_for: None,
                cooldown_reason: None,
                cooldown_scope: None,
                cooldown_remaining_secs: None,
                eligible_pools: 1,
                available_pools: 0,
                temporary_unavailable_pools: 1,
                coordinator_unavailable_kind: None,
            },
            &mut queue_guard,
            &mut wait_started_at,
            &mut capacity_waiter,
            None,
        )
        .await;

    let ExternalCapacityDecision::FinalError(error) = decision else {
        panic!("legacy zero wait must terminate at the safe default deadline");
    };
    assert_eq!(error.route_error_type, "external_pool_wait_timeout");
    assert!(!error.retryable);
    assert!(queue_guard.is_none());

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test]
async fn external_pool_model_unavailable_cooldown_is_model_scoped_and_does_not_queue() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_capacity_mode: ExternalPoolCapacityMode::Wait,
        external_pool_max_queued_requests: 1,
        external_pool_model_unavailable_cooldown_mode:
            ExternalPoolModelUnavailableCooldownMode::Model,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-model-cooldown", 1, true))
        .await
        .unwrap();
    let route_a = test_route("claude-opus-4-8");
    let route_b = test_route("claude-sonnet-4-6");
    manager
        .mark_pool_model_cooldowns(
            pool.id,
            Duration::from_secs(30),
            "model_unavailable".to_string(),
            &route_a.model_cooldown_candidates(),
        )
        .await;

    let unavailable_for_a = manager
        .select_pool_for_route(&HashSet::new(), &config, &route_a)
        .await;
    assert!(unavailable_for_a.selected_pool.is_none());
    assert_eq!(
        unavailable_for_a.availability.wait_reason,
        Some(PoolCapacityWaitReason::ModelUnavailable)
    );
    assert_eq!(
        unavailable_for_a.availability.cooldown_scope,
        Some(PoolCooldownScope::Model)
    );
    assert_eq!(
        unavailable_for_a.availability.cooldown_reason.as_deref(),
        Some("model_unavailable")
    );

    let mut queue_guard = None;
    let mut wait_started_at = None;
    let mut capacity_waiter = manager.capacity_signal.register();
    let decision = manager
        .handle_capacity_unavailable(
            &route_a,
            Vec::new(),
            &config,
            unavailable_for_a.availability.capacity_context(),
            &mut queue_guard,
            &mut wait_started_at,
            &mut capacity_waiter,
            None,
        )
        .await;
    let ExternalCapacityDecision::FinalError(error) = decision else {
        panic!("model_unavailable cooldown must fail fast instead of queueing");
    };
    assert_eq!(error.route_error_type, "model_unavailable");
    assert!(queue_guard.is_none());
    assert_eq!(
        manager
            .redis
            .external_pool_dispatch_queue_size()
            .await
            .unwrap(),
        0
    );

    let available_for_b = manager
        .select_pool_for_route(&HashSet::new(), &config, &route_b)
        .await;
    assert!(available_for_b.selected_pool.is_some());
    assert_eq!(available_for_b.availability.available_pools, 1);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_route_mode_is_applied_per_pool_for_selection_and_eligibility() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let model = "claude-sonnet-4-6".to_string();

    let mut cc_request = create_pool_request("route-allow-cc", 1, true);
    cc_request.supported_models = vec![model.clone()];
    cc_request.route_mode = ExternalPoolRouteMode::AllowList;
    cc_request.route_rules = vec!["/cc".to_string()];
    let cc_pool = postgres.create_external_pool(cc_request).await.unwrap();

    let mut v1_request = create_pool_request("route-allow-v1", 2, true);
    v1_request.supported_models = vec![model.clone()];
    v1_request.route_mode = ExternalPoolRouteMode::AllowList;
    v1_request.route_rules = vec!["/v1".to_string()];
    let v1_pool = postgres.create_external_pool(v1_request).await.unwrap();

    let mut ha_request = create_pool_request("route-deny-cc-v1", 3, true);
    ha_request.supported_models = vec![model.clone()];
    ha_request.route_mode = ExternalPoolRouteMode::DenyList;
    ha_request.route_rules = vec!["/cc".to_string(), "/v1".to_string()];
    let ha_pool = postgres.create_external_pool(ha_request).await.unwrap();

    manager.invalidate_static_pool_snapshot();

    let mut cc_route = test_route(&model);
    cc_route.endpoint = "/cc/v1/messages".to_string();
    let cc_selection = manager
        .select_pool_for_route(&HashSet::new(), &config, &cc_route)
        .await;
    assert_eq!(
        cc_selection.selected_pool.as_ref().map(|pool| pool.id),
        Some(cc_pool.id)
    );
    assert!(
        manager
            .has_eligible_pool_for_route_and_model(&config, &cc_route.endpoint, &model)
            .await
    );

    let mut v1_route = test_route(&model);
    v1_route.endpoint = "/v1/messages".to_string();
    let v1_selection = manager
        .select_pool_for_route(&HashSet::new(), &config, &v1_route)
        .await;
    assert_eq!(
        v1_selection.selected_pool.as_ref().map(|pool| pool.id),
        Some(v1_pool.id)
    );
    assert!(
        manager
            .has_eligible_pool_for_route_and_model(&config, &v1_route.endpoint, &model)
            .await
    );

    let mut ha_route = test_route(&model);
    ha_route.endpoint = "/ha/v1/messages".to_string();
    let ha_selection = manager
        .select_pool_for_route(&HashSet::new(), &config, &ha_route)
        .await;
    assert_eq!(
        ha_selection.selected_pool.as_ref().map(|pool| pool.id),
        Some(ha_pool.id)
    );
    assert!(
        manager
            .has_eligible_pool_for_route_and_model(&config, &ha_route.endpoint, &model)
            .await
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_redis_hot_path_is_repeatable_across_selection_and_acquire() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_static_pool_snapshot_ttl(Duration::from_secs(30));
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_model_unavailable_cooldown_mode:
            ExternalPoolModelUnavailableCooldownMode::Model,
        ..ExternalPoolsConfig::default()
    };
    let mut pool_ids = Vec::new();
    for index in 0..8 {
        let pool = postgres
            .create_external_pool(create_pool_request(
                &format!("external-redis-hot-path-{index}"),
                1,
                true,
            ))
            .await
            .unwrap();
        pool_ids.push(pool.id);
    }
    let route = test_route("claude-sonnet-4-6");

    for round in 0..5 {
        manager.redis.reset_external_pool_hot_path_round_trips();
        let started = Instant::now();
        assert!(
            manager
                .has_eligible_pool_for_model(&config, &route.requested_model())
                .await,
            "round {round}: configured pool should remain eligible"
        );
        assert_eq!(
            manager.redis.external_pool_hot_path_round_trips(),
            0,
            "round {round}: healthy local eligibility must not perform an external Redis selection snapshot"
        );
        let after_eligibility = Instant::now();
        let selection = manager
            .select_pool_for_route(&HashSet::new(), &config, &route)
            .await;
        let after_selection = Instant::now();
        let selected = selection
            .selected_pool
            .expect("configured pool should remain selectable");
        assert!(
            pool_ids.contains(&selected.id),
            "round {round}: selected pool must come from the eligible batch"
        );
        let lease = match manager
            .acquire_pool_for_route(&selected, &config, &route)
            .await
        {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(unavailable) => {
                panic!(
                    "round {round}: lease acquire failed: {}",
                    unavailable.detail
                )
            }
        };
        let after_acquire = Instant::now();
        let redis_round_trips = manager.redis.external_pool_hot_path_round_trips();
        if round == 0 {
            assert!(
                (2..=6).contains(&redis_round_trips),
                "round {round}: cold coordinator bootstrap may add bounded Redis run_id/epoch probes, got {redis_round_trips}"
            );
        } else {
            assert_eq!(
                redis_round_trips, 2,
                "round {round}: hot eligibility must not access Redis, while batched selection and atomic acquire use one round trip each regardless of pool count"
            );
        }
        eprintln!(
            "external_pool_redis_hot_path round={round} pools={} eligibility_ms={} selection_ms={} acquire_ms={} total_ms={}",
            pool_ids.len(),
            after_eligibility.duration_since(started).as_millis(),
            after_selection
                .duration_since(after_eligibility)
                .as_millis(),
            after_acquire.duration_since(after_selection).as_millis(),
            after_acquire.duration_since(started).as_millis(),
        );

        drop(lease);
        let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
        assert!(drained.drained, "round {round}: release task should drain");
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn external_pool_redis_rtt_and_concurrency_matrix_five_outer_rounds() {
    if !matches!(
        std::env::var("KIRO_RS_RUN_EXTERNAL_REDIS_RTT_MATRIX"),
        Ok(value) if value == "1"
    ) {
        eprintln!("跳过 external Redis RTT 矩阵：未设置 KIRO_RS_RUN_EXTERNAL_REDIS_RTT_MATRIX=1");
        return;
    }
    let Ok(proxy_redis_url) = std::env::var("KIRO_RS_TEST_REDIS_URL") else {
        eprintln!("跳过 external Redis RTT 矩阵：未设置 KIRO_RS_TEST_REDIS_URL");
        return;
    };
    let Ok(direct_redis_url) = std::env::var("KIRO_RS_TEST_REDIS_DIRECT_URL") else {
        eprintln!("跳过 external Redis RTT 矩阵：未设置 KIRO_RS_TEST_REDIS_DIRECT_URL");
        return;
    };
    let Ok(toxiproxy_api) = std::env::var("KIRO_RS_TEST_TOXIPROXY_API") else {
        eprintln!("跳过 external Redis RTT 矩阵：未设置 KIRO_RS_TEST_TOXIPROXY_API");
        return;
    };
    let proxy_name =
        std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").unwrap_or_else(|_| "redis".to_string());
    let Some(mut postgres_config) = test_postgres_config() else {
        eprintln!("跳过 external Redis RTT 矩阵：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return;
    };
    postgres_config.postgres.max_connections = 64;
    let postgres = Arc::new(PostgresStore::connect_test(&postgres_config).await.unwrap());

    for index in 0..60 {
        let mut request = create_pool_request(&format!("external-rtt-matrix-{index}"), index, true);
        request.max_concurrent_requests = 1_024;
        postgres.create_external_pool(request).await.unwrap();
    }
    let config = Arc::new(ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 0,
        ..ExternalPoolsConfig::default()
    });
    let route = Arc::new(test_route("claude-sonnet-4-6"));
    let mut managers = Vec::with_capacity(5);
    let mut direct_stores = Vec::with_capacity(5);
    let key_prefix = format!("kiro_rs:test:external_rtt:{}", uuid::Uuid::new_v4());
    for _ in 0..5 {
        let mut proxy_config = Config::default();
        proxy_config.redis.url = Some(proxy_redis_url.clone());
        proxy_config.redis.key_prefix = key_prefix.clone();
        let proxy_store = Arc::new(RedisStore::connect(&proxy_config).await.unwrap());
        managers.push(Arc::new(ExternalPoolManager::new(
            postgres.clone(),
            proxy_store,
        )));

        let mut direct_config = Config::default();
        direct_config.redis.url = Some(direct_redis_url.clone());
        direct_config.redis.key_prefix = key_prefix.clone();
        direct_stores.push(Arc::new(RedisStore::connect(&direct_config).await.unwrap()));
    }

    let client = reqwest::Client::new();
    let toxic_base = format!(
        "{}/proxies/{}/toxics",
        toxiproxy_api.trim_end_matches('/'),
        proxy_name
    );
    let rss_start = external_perf_process_rss_kib();
    let fd_start = external_perf_open_fd_count();
    let mut rss_peak = rss_start;
    let mut fd_peak = fd_start;

    for latency_ms in [0u64, 50, 74, 75, 90, 150, 500] {
        for concurrency in [64usize, 16, 1] {
            let toxic_name = format!("external-rtt-{latency_ms}-c{concurrency}");
            if latency_ms > 0 {
                let _ = client
                    .delete(format!("{toxic_base}/{toxic_name}"))
                    .send()
                    .await;
                let install = client
                    .post(&toxic_base)
                    .json(&json!({
                        "name": toxic_name.clone(),
                        "type": "latency",
                        "stream": "downstream",
                        "toxicity": 1.0,
                        "attributes": { "latency": latency_ms, "jitter": 0 },
                    }))
                    .send()
                    .await
                    .unwrap();
                assert!(
                    install.status().is_success(),
                    "latency={latency_ms}, concurrency={concurrency}: {install:?}"
                );
            }

            let warmups = futures::future::join_all((0..5).map(|round| {
                run_external_coordinator_perf_round(
                    managers[round].clone(),
                    direct_stores[round].clone(),
                    config.clone(),
                    route.clone(),
                    40,
                    concurrency,
                )
            }))
            .await;
            for manager in &managers {
                manager.redis.reset_external_pool_hot_path_round_trips();
            }
            let saturated_before = managers
                .iter()
                .map(|manager| manager.selection_saturated.load(Ordering::Acquire))
                .collect::<Vec<_>>();
            let measured_started = Instant::now();
            let measured = futures::future::join_all((0..5).map(|round| {
                run_external_coordinator_perf_round(
                    managers[round].clone(),
                    direct_stores[round].clone(),
                    config.clone(),
                    route.clone(),
                    200,
                    concurrency,
                )
            }))
            .await;
            let measured_wall = measured_started.elapsed();

            if latency_ms > 0 {
                let remove = client
                    .delete(format!("{toxic_base}/{toxic_name}"))
                    .send()
                    .await
                    .unwrap();
                assert!(
                    remove.status().is_success(),
                    "latency={latency_ms}, concurrency={concurrency}: {remove:?}"
                );
            }

            let warmup_failures = warmups
                .iter()
                .flat_map(|round| round.iter())
                .filter(|result| result.is_err())
                .count();
            let measured_failures = measured
                .iter()
                .flat_map(|round| round.iter())
                .filter(|result| result.is_err())
                .count();
            assert_eq!(
                warmup_failures, 0,
                "latency={latency_ms}, concurrency={concurrency}: warmup failures"
            );
            assert_eq!(
                measured_failures, 0,
                "latency={latency_ms}, concurrency={concurrency}: measured failures"
            );

            let redis_attempts = managers
                .iter()
                .map(|manager| manager.redis.external_pool_hot_path_round_trips())
                .sum::<u64>();
            let request_round_trips = 2_000u64;
            let probe_interval_ms =
                Duration::from_secs(EXTERNAL_POOL_COORDINATOR_RUN_ID_PROBE_INTERVAL_SECS)
                    .as_millis()
                    .max(1);
            let probe_windows = measured_wall
                .as_millis()
                .saturating_add(probe_interval_ms - 1)
                / probe_interval_ms;
            let max_probe_round_trips = probe_windows
                .saturating_add(2)
                .saturating_mul(managers.len() as u128)
                .min(u64::MAX as u128) as u64;
            assert!(
                redis_attempts >= request_round_trips
                    && redis_attempts <= request_round_trips.saturating_add(max_probe_round_trips),
                "latency={latency_ms}, concurrency={concurrency}: measured Redis RTTs {redis_attempts} exceeded request RTTs {request_round_trips} plus amortized probe bound {max_probe_round_trips}"
            );
            let probe_round_trips = redis_attempts.saturating_sub(request_round_trips);
            let admission_rejections = managers
                .iter()
                .zip(&saturated_before)
                .map(|(manager, before)| {
                    manager
                        .selection_saturated
                        .load(Ordering::Acquire)
                        .saturating_sub(*before)
                })
                .sum::<u64>();
            assert_eq!(
                admission_rejections, 0,
                "latency={latency_ms}, concurrency={concurrency}: nominal matrix load must not be shed"
            );

            let mut latency_micros = measured
                .into_iter()
                .flatten()
                .map(|result| result.unwrap().as_micros())
                .collect::<Vec<_>>();
            latency_micros.sort_unstable();
            assert_eq!(latency_micros.len(), 1_000);
            let recovery = run_external_coordinator_perf_round(
                managers[0].clone(),
                direct_stores[0].clone(),
                config.clone(),
                route.clone(),
                5,
                1,
            )
            .await;
            assert!(
                recovery.iter().all(Result::is_ok),
                "latency={latency_ms}, concurrency={concurrency}: recovery must be 5/5"
            );

            let rss_now = external_perf_process_rss_kib();
            let fd_now = external_perf_open_fd_count();
            rss_peak = match (rss_peak, rss_now) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (left, right) => left.or(right),
            };
            fd_peak = match (fd_peak, fd_now) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (left, right) => left.or(right),
            };
            eprintln!(
                "external_redis_rtt latency_ms={latency_ms} round_count=5 pools=60 per_round_concurrency={concurrency} aggregate_concurrency={} warmup=200 measured=1000 request_round_trips={request_round_trips} probe_round_trips={probe_round_trips} probe_round_trip_bound={max_probe_round_trips} admission_rejections={admission_rejections} success=1000 recovery=5/5 p50_us={} p95_us={} p99_us={} wall_ms={} rss_kib={:?} fd={:?}",
                concurrency * 5,
                external_perf_percentile_micros(&latency_micros, 50),
                external_perf_percentile_micros(&latency_micros, 95),
                external_perf_percentile_micros(&latency_micros, 99),
                measured_wall.as_millis(),
                rss_now,
                fd_now,
            );
        }
    }

    let rss_end = external_perf_process_rss_kib();
    let fd_end = external_perf_open_fd_count();
    eprintln!(
        "external_redis_rtt_resources rss_start_kib={rss_start:?} rss_peak_kib={rss_peak:?} rss_end_kib={rss_end:?} fd_start={fd_start:?} fd_peak={fd_peak:?} fd_end={fd_end:?}"
    );
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_atomic_acquire_honors_pool_cooldown_and_fails_closed_on_bad_state() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_model_unavailable_cooldown_mode:
            ExternalPoolModelUnavailableCooldownMode::Model,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-atomic-cooldown", 1, true))
        .await
        .unwrap();
    let cooldown_key = format!("external_pool:{}:cooldown", pool.id);
    let route = test_route("claude-sonnet-4-6");
    let model_cooldown_candidates = route.model_cooldown_candidates();

    for round in 0..5 {
        manager
            .mark_pool_cooldown(pool.id, Duration::from_secs(30), "rate_limit".to_string())
            .await;
        let blocked = manager.acquire_pool(&pool, &config).await;
        let PoolAcquireResult::Unavailable(blocked) = blocked else {
            panic!("round {round}: active pool cooldown must block atomic acquire");
        };
        assert_eq!(blocked.reason, PoolCapacityWaitReason::Cooldown);
        assert_eq!(blocked.detail, "cooldown_during_atomic_acquire");
        assert!(blocked.wait_for.is_some());
        let cleared = manager.clear_pool_cooldowns(pool.id).await.unwrap();
        assert!(
            cleared >= 1,
            "round {round}: clearing pool cooldown should delete at least one Redis key"
        );
        let cleared_snapshot = manager
            .load_pool_runtime_snapshot(pool.id, &model_cooldown_candidates)
            .await
            .unwrap();
        assert_eq!(
            cleared_snapshot.pool_cooldown_remaining_secs, 0,
            "round {round}: cleared pool cooldown must be absent from the runtime snapshot"
        );
        assert!(
            cleared_snapshot.model_cooldown.is_none(),
            "round {round}: clearing pool cooldown should also clear model cooldown state"
        );

        let before_model_race = manager
            .load_pool_runtime_snapshot(pool.id, &model_cooldown_candidates)
            .await
            .unwrap();
        assert!(
            before_model_race.model_cooldown.is_none(),
            "round {round}: selection snapshot should precede the model cooldown write"
        );
        manager
            .mark_pool_model_cooldowns(
                pool.id,
                Duration::from_secs(30),
                "model_unavailable".to_string(),
                &model_cooldown_candidates,
            )
            .await;
        let model_blocked = manager
            .acquire_pool_with_model_cooldowns(&pool, &config, &model_cooldown_candidates)
            .await;
        let PoolAcquireResult::Unavailable(model_blocked) = model_blocked else {
            panic!("round {round}: model cooldown race must block atomic acquire");
        };
        assert_eq!(
            model_blocked.reason,
            PoolCapacityWaitReason::ModelUnavailable
        );
        assert_eq!(model_blocked.detail, "model_cooldown_during_atomic_acquire");
        assert_eq!(
            manager.pool_runtime_snapshot(pool.id).await.unwrap().0,
            0,
            "round {round}: rejected model cooldown race must not occupy a lease"
        );
        let cleared = manager.clear_pool_cooldowns(pool.id).await.unwrap();
        assert!(
            cleared >= 1,
            "round {round}: clearing model cooldown should delete at least one Redis key"
        );
        let cleared_snapshot = manager
            .load_pool_runtime_snapshot(pool.id, &model_cooldown_candidates)
            .await
            .unwrap();
        assert_eq!(cleared_snapshot.pool_cooldown_remaining_secs, 0);
        assert!(
            cleared_snapshot.model_cooldown.is_none(),
            "round {round}: cleared model cooldown must be absent from the runtime snapshot"
        );

        manager
            .redis
            .set_json(&cooldown_key, &"not-a-cooldown-state", 30)
            .await
            .unwrap();
        let malformed = manager
            .scan_pool_availability_uncached(&HashSet::new(), &config, true, None, None, None)
            .await;
        assert!(malformed.selected_pool.is_none());
        assert!(malformed.availability.coordinator_unavailable);
        assert_eq!(
            malformed.availability.wait_reason,
            Some(PoolCapacityWaitReason::CoordinatorUnavailable)
        );
        manager.redis.del(&cooldown_key).await.unwrap();

        let lease = match manager.acquire_pool(&pool, &config).await {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(unavailable) => {
                panic!(
                    "round {round}: acquire should recover: {}",
                    unavailable.detail
                )
            }
        };
        drop(lease);
        let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
        assert!(drained.drained, "round {round}: release task should drain");
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_same_pool_retry_runs_even_when_pool_attempt_budget_is_one_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalMessagesFakeServer::start(
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
    )
    .await;
    create_messages_pool(&postgres, "same-pool-budget-primary", 1, &failing.base_url).await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 4,
        external_pool_retry_max_attempts: 1,
        external_pool_same_pool_retry_count: 1,
        external_pool_same_pool_retry_status_codes: vec![StatusCode::BAD_GATEWAY.as_u16()],
        external_pool_same_pool_retry_delay_ms: 1,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4-6");
    route.request_id = "req_same_pool_retry_budget".to_string();
    route.error_id = "err_same_pool_retry_budget".to_string();
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));

    let outcome = timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("same-pool retry should finish");
    assert!(
        matches!(outcome, ExternalPoolForwardOutcome::FinalError(_)),
        "single permanently failing pool should still end with a final error"
    );
    assert_eq!(
        failing.snapshot(),
        2,
        "same-pool retry must not be blocked by the one-pool cross-pool attempt budget"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_same_pool_retry_precedes_cross_pool_failover_for_configured_status() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalMessagesFakeServer::start(
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
    )
    .await;
    let succeeding =
        ExternalMessagesFakeServer::start(StatusCode::OK, fake_external_success_body("retry-ok"))
            .await;
    create_messages_pool(
        &postgres,
        "same-pool-before-failover-primary",
        1,
        &failing.base_url,
    )
    .await;
    create_messages_pool(
        &postgres,
        "same-pool-before-failover-secondary",
        2,
        &succeeding.base_url,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 8,
        external_pool_retry_max_attempts: 2,
        external_pool_same_pool_retry_count: 1,
        external_pool_same_pool_retry_status_codes: vec![StatusCode::BAD_GATEWAY.as_u16()],
        external_pool_same_pool_retry_delay_ms: 1,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4-6");
    route.request_id = "req_same_pool_before_failover".to_string();
    route.error_id = "err_same_pool_before_failover".to_string();
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));

    let response = match timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("same-pool retry failover should finish")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("configured retryable status should reach secondary pool: {error:?}")
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read retry success body");
    assert!(
        body.windows(b"retry-ok".len())
            .any(|window| window == b"retry-ok")
    );
    assert_eq!(
        failing.snapshot(),
        2,
        "configured status should retry the selected pool before switching"
    );
    assert_eq!(
        succeeding.snapshot(),
        1,
        "secondary pool should be tried after same-pool retry is exhausted"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_same_pool_retry_skips_statuses_not_in_config() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalMessagesFakeServer::start(
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
    )
    .await;
    let succeeding = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("no-same-pool-retry-ok"),
    )
    .await;
    create_messages_pool(
        &postgres,
        "same-pool-status-filter-primary",
        1,
        &failing.base_url,
    )
    .await;
    create_messages_pool(
        &postgres,
        "same-pool-status-filter-secondary",
        2,
        &succeeding.base_url,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 8,
        external_pool_retry_max_attempts: 2,
        external_pool_same_pool_retry_count: 3,
        external_pool_same_pool_retry_status_codes: vec![StatusCode::TOO_MANY_REQUESTS.as_u16()],
        external_pool_same_pool_retry_delay_ms: 1,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4-6");
    route.request_id = "req_same_pool_status_filter".to_string();
    route.error_id = "err_same_pool_status_filter".to_string();
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));

    let response = match timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("status-filtered failover should finish")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("unconfigured status should switch pools without same-pool retry: {error:?}")
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        failing.snapshot(),
        1,
        "status not listed in 同池重试状态码 must not retry the same pool"
    );
    assert_eq!(succeeding.snapshot(), 1);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_cross_pool_retry_status_codes_can_stop_failover() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalMessagesFakeServer::start(
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
    )
    .await;
    let succeeding =
        ExternalMessagesFakeServer::start(StatusCode::OK, fake_external_success_body("unused"))
            .await;
    create_messages_pool(
        &postgres,
        "cross-pool-status-filter-primary",
        1,
        &failing.base_url,
    )
    .await;
    create_messages_pool(
        &postgres,
        "cross-pool-status-filter-secondary",
        2,
        &succeeding.base_url,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 8,
        external_pool_retry_max_attempts: 2,
        external_pool_retry_status_codes: vec![StatusCode::TOO_MANY_REQUESTS.as_u16()],
        external_pool_same_pool_retry_count: 0,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4-6");
    route.request_id = "req_cross_pool_status_filter".to_string();
    route.error_id = "err_cross_pool_status_filter".to_string();
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));

    let outcome = timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("cross-pool status filtering should finish");
    assert!(
        matches!(outcome, ExternalPoolForwardOutcome::FinalError(_)),
        "a status excluded from cross-pool retry must fail without switching pools"
    );
    assert_eq!(failing.snapshot(), 1);
    assert_eq!(succeeding.snapshot(), 0);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_terminal_account_error_skips_same_pool_retry_and_fails_over() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing = ExternalMessagesFakeServer::start(
        StatusCode::UNAUTHORIZED,
        fake_external_error_body("invalid token"),
    )
    .await;
    let succeeding =
        ExternalMessagesFakeServer::start(StatusCode::OK, fake_external_success_body("auth-ok"))
            .await;
    create_messages_pool(
        &postgres,
        "terminal-account-error-primary",
        1,
        &failing.base_url,
    )
    .await;
    create_messages_pool(
        &postgres,
        "terminal-account-error-secondary",
        2,
        &succeeding.base_url,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 8,
        external_pool_retry_max_attempts: 2,
        external_pool_same_pool_retry_count: 3,
        external_pool_same_pool_retry_status_codes: vec![StatusCode::UNAUTHORIZED.as_u16()],
        external_pool_same_pool_retry_delay_ms: 1,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4-6");
    route.request_id = "req_terminal_account_error".to_string();
    route.error_id = "err_terminal_account_error".to_string();
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));

    let response = match timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("terminal account failover should finish")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("terminal account error should fail over to the next pool: {error:?}")
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        failing.snapshot(),
        1,
        "auth errors should cool down and fail over instead of retrying the same account"
    );
    assert_eq!(succeeding.snapshot(), 1);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_retry_after_header_records_soft_failure_without_pool_cooldown() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("4"));
    let failing = ExternalMessagesFakeServer::start_with_headers(
        StatusCode::TOO_MANY_REQUESTS,
        headers,
        fake_external_error_body("rate limited"),
    )
    .await;
    let pool =
        create_messages_pool(&postgres, "retry-after-real-cooldown", 1, &failing.base_url).await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 4,
        external_pool_retry_max_attempts: 1,
        external_pool_same_pool_retry_count: 0,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4-6");
    route.request_id = "req_retry_after_real_cooldown".to_string();
    route.error_id = "err_retry_after_real_cooldown".to_string();
    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(2));
    let model_cooldown_candidates = route.model_cooldown_candidates();

    let outcome = timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config, route),
    )
    .await
    .expect("rate-limit fake upstream request should finish");
    assert!(
        matches!(outcome, ExternalPoolForwardOutcome::FinalError(_)),
        "single rate-limited pool should return a final error"
    );
    assert_eq!(failing.snapshot(), 1);

    let runtime = manager
        .load_pool_runtime_snapshot(pool.id, &model_cooldown_candidates)
        .await
        .expect("read pool runtime snapshot");
    assert_eq!(
        runtime.pool_cooldown_remaining_secs, 0,
        "ordinary 429 must not hard-cool the whole external pool"
    );
    assert_eq!(runtime.pool_cooldown_reason.as_deref(), None);
    assert_eq!(
        runtime.transient_failure_streak, 1,
        "ordinary 429 should only leave a short-lived health penalty"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_transient_failure_penalty_moves_sustained_traffic_to_healthy_backup() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing_primary = ExternalMessagesFakeServer::start(
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
    )
    .await;
    let healthy_secondary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-secondary-ok"),
    )
    .await;
    let healthy_tertiary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-tertiary-ok"),
    )
    .await;
    let primary = create_messages_pool_with_concurrency(
        &postgres,
        "transient-priority-primary",
        1,
        &failing_primary.base_url,
        64,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "transient-priority-secondary",
        10,
        &healthy_secondary.base_url,
        64,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "transient-priority-tertiary",
        20,
        &healthy_tertiary.base_url,
        64,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 128,
        external_pool_retry_max_attempts: 3,
        external_pool_retry_status_codes: vec![StatusCode::BAD_GATEWAY.as_u16()],
        external_pool_same_pool_retry_count: 0,
        external_pool_server_error_cooldown_secs: 1,
        external_pool_transient_failure_priority_penalty: 20,
        ..ExternalPoolsConfig::default()
    };

    let mut first_route = test_route("claude-sonnet-4-6");
    first_route.request_id = "req_transient_priority_first".to_string();
    first_route.error_id = "err_transient_priority_first".to_string();
    first_route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
    let response = match timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config.clone(), first_route),
    )
    .await
    .expect("first failover request should finish")
    {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("primary 502 should fail over to a healthy backup: {error:?}")
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(failing_primary.snapshot(), 1);
    assert_eq!(healthy_secondary.snapshot(), 1);
    assert_eq!(healthy_tertiary.snapshot(), 0);

    let runtime_after_failure = manager
        .load_pool_runtime_snapshot(primary.id, &[])
        .await
        .expect("read primary runtime after failure");
    assert_eq!(runtime_after_failure.transient_failure_streak, 1);
    assert!(
        runtime_after_failure.transient_failure_ttl.is_some(),
        "primary transient failure window should remain after the failed send"
    );

    tokio::time::sleep(Duration::from_millis(1_250)).await;

    let statuses = manager
        .status(&config)
        .await
        .expect("read external pool status");
    let primary_status = statuses
        .iter()
        .find(|status| status.pool.id == primary.id)
        .expect("primary status");
    assert_eq!(primary_status.transient_failure_streak, 1);
    assert!(
        primary_status.transient_failure_ttl_secs > 0,
        "status should expose the remaining transient failure window"
    );

    for index in 0..5 {
        let mut route = test_route("claude-sonnet-4-6");
        route.request_id = format!("req_transient_priority_seq_{index}");
        route.error_id = format!("err_transient_priority_seq_{index}");
        route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
        let response = match timeout(
            Duration::from_secs(3),
            manager.forward_with_failover_result(config.clone(), route),
        )
        .await
        .expect("sequential backup request should finish")
        {
            ExternalPoolForwardOutcome::Response(response) => response,
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("sequential traffic should stay on a healthy backup: {error:?}")
            }
        };
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(
        failing_primary.snapshot(),
        1,
        "primary must not be retried immediately after its short cooldown expires"
    );
    assert_eq!(healthy_secondary.snapshot(), 6);

    let concurrent = futures::future::join_all((0..24).map(|index| {
        let manager = manager.clone();
        let config = config.clone();
        async move {
            let mut route = test_route("claude-sonnet-4-6");
            route.request_id = format!("req_transient_priority_concurrent_{index}");
            route.error_id = format!("err_transient_priority_concurrent_{index}");
            route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
            timeout(
                Duration::from_secs(3),
                manager.forward_with_failover_result(config, route),
            )
            .await
            .expect("concurrent backup request should finish")
        }
    }))
    .await;
    for outcome in concurrent {
        match outcome {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK);
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("concurrent traffic should stay on healthy backup pools: {error:?}")
            }
        }
    }
    assert_eq!(
        failing_primary.snapshot(),
        1,
        "concurrent traffic must not be trapped by the failed priority-1 pool"
    );
    assert_eq!(
        healthy_secondary.snapshot(),
        30,
        "healthy priority-10 backup should carry the redirected traffic"
    );
    assert_eq!(healthy_tertiary.snapshot(), 0);

    let cleared = manager
        .clear_pool_cooldowns(primary.id)
        .await
        .expect("clear primary cooldowns and transient failure window");
    assert!(
        cleared >= 1,
        "clear cooldown should also remove the transient failure window"
    );
    let selected_after_clear = manager
        .select_pool_for_route(&HashSet::new(), &config, &test_route("claude-sonnet-4-6"))
        .await
        .selected_pool
        .expect("primary should be eligible again after manual clear");
    assert_eq!(selected_after_clear.id, primary.id);

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_one_wave_all_pools_transient_502_does_not_blackout_recovery() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool_a_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
        1,
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
        fake_external_success_body("pool-a-recovered"),
    )
    .await;
    let pool_b_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
        1,
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
        fake_external_success_body("pool-b-recovered"),
    )
    .await;
    let pool_c_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
        1,
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
        fake_external_success_body("pool-c-recovered"),
    )
    .await;
    let pool_a = create_messages_pool_with_concurrency(
        &postgres,
        "one-wave-primary",
        1,
        &pool_a_server.base_url,
        64,
    )
    .await;
    let pool_b = create_messages_pool_with_concurrency(
        &postgres,
        "one-wave-secondary",
        10,
        &pool_b_server.base_url,
        64,
    )
    .await;
    let pool_c = create_messages_pool_with_concurrency(
        &postgres,
        "one-wave-tertiary",
        20,
        &pool_c_server.base_url,
        64,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 128,
        external_pool_retry_max_attempts: 3,
        external_pool_retry_status_codes: vec![StatusCode::BAD_GATEWAY.as_u16()],
        external_pool_same_pool_retry_count: 0,
        external_pool_server_error_cooldown_secs: 30,
        external_pool_transient_failure_priority_penalty: 20,
        ..ExternalPoolsConfig::default()
    };

    let mut first_route = test_route("claude-sonnet-4-6");
    first_route.request_id = "req_one_wave_first".to_string();
    first_route.error_id = "err_one_wave_first".to_string();
    first_route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
    let first = timeout(
        Duration::from_secs(3),
        manager.forward_with_failover_result(config.clone(), first_route),
    )
    .await
    .expect("first wave should finish");
    assert!(
        matches!(first, ExternalPoolForwardOutcome::FinalError(_)),
        "when every configured pool fails once, the first request can fail"
    );
    assert_eq!(pool_a_server.snapshot(), 1);
    assert_eq!(pool_b_server.snapshot(), 1);
    assert_eq!(pool_c_server.snapshot(), 1);

    for pool in [&pool_a, &pool_b, &pool_c] {
        let runtime = manager
            .load_pool_runtime_snapshot(pool.id, &[])
            .await
            .expect("read runtime after one transient wave");
        assert_eq!(
            runtime.pool_cooldown_remaining_secs, 0,
            "ordinary one-wave 502 must not hard-cool pool {}",
            pool.name
        );
        assert_eq!(
            runtime.transient_failure_streak, 1,
            "ordinary one-wave 502 should only soft-penalize pool {}",
            pool.name
        );
    }

    let mut recovered_route = test_route("claude-sonnet-4-6");
    recovered_route.request_id = "req_one_wave_recovered".to_string();
    recovered_route.error_id = "err_one_wave_recovered".to_string();
    recovered_route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
    let recovered = timeout(
        Duration::from_millis(750),
        manager.forward_with_failover_result(config, recovered_route),
    )
    .await
    .expect("recovered pools must be callable immediately after the transient wave");
    match recovered {
        ExternalPoolForwardOutcome::Response(response) => {
            assert_eq!(response.status(), StatusCode::OK);
        }
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("recovered transient pools should not remain blacked out: {error:?}")
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn external_pool_high_concurrency_sustained_primary_502_transfers_to_backup_without_cooldown()
{
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let failing_primary = ExternalMessagesFakeServer::start(
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
    )
    .await;
    let healthy_secondary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-secondary-ok"),
    )
    .await;
    let healthy_tertiary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-tertiary-ok"),
    )
    .await;
    let primary = create_messages_pool_with_concurrency(
        &postgres,
        "concurrent-sustained-primary",
        1,
        &failing_primary.base_url,
        128,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "concurrent-sustained-secondary",
        10,
        &healthy_secondary.base_url,
        128,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "concurrent-sustained-tertiary",
        20,
        &healthy_tertiary.base_url,
        128,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 384,
        external_pool_retry_max_attempts: 3,
        external_pool_retry_status_codes: vec![StatusCode::BAD_GATEWAY.as_u16()],
        external_pool_same_pool_retry_count: 0,
        external_pool_transient_failure_priority_penalty: 20,
        ..ExternalPoolsConfig::default()
    };

    let run_batch = |batch: &'static str| {
        let manager = manager.clone();
        let config = config.clone();
        async move {
            futures::future::join_all((0..64).map(|index| {
                let manager = manager.clone();
                let config = config.clone();
                async move {
                    let mut route = test_route("claude-sonnet-4-6");
                    route.request_id = format!("req_{batch}_{index}");
                    route.error_id = format!("err_{batch}_{index}");
                    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
                    timeout(
                        Duration::from_secs(5),
                        manager.forward_with_failover_result(config, route),
                    )
                    .await
                    .expect("concurrent request should finish")
                }
            }))
            .await
        }
    };

    for outcome in run_batch("concurrent_sustained_wave1").await {
        match outcome {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK);
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("healthy backup should absorb concurrent primary failures: {error:?}")
            }
        }
    }
    let primary_after_first_wave = failing_primary.snapshot();
    assert!(
        primary_after_first_wave > 0,
        "first wave must actually hit the failing priority-1 pool"
    );
    let runtime = manager
        .load_pool_runtime_snapshot(primary.id, &[])
        .await
        .expect("read primary runtime after high-concurrency wave");
    assert_eq!(
        runtime.pool_cooldown_remaining_secs, 0,
        "high-concurrency 502 wave must not hard-cool the primary pool"
    );
    assert!(
        runtime.transient_failure_streak > 0,
        "high-concurrency 502 wave should leave a soft health penalty"
    );

    tokio::time::sleep(Duration::from_millis(150)).await;

    for outcome in run_batch("concurrent_sustained_wave2").await {
        match outcome {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK);
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("healthy backup should keep absorbing traffic after soft penalty: {error:?}")
            }
        }
    }
    let primary_second_wave_hits = failing_primary
        .snapshot()
        .saturating_sub(primary_after_first_wave);
    assert!(
        primary_second_wave_hits <= 2,
        "soft penalty should prevent the failing priority-1 pool from monopolizing the next high-concurrency wave; got {primary_second_wave_hits} extra hits"
    );
    assert_eq!(
        healthy_secondary
            .snapshot()
            .saturating_add(healthy_tertiary.snapshot()),
        128,
        "healthy backup pools should receive every successful request"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn external_pool_high_concurrency_random_mixed_status_turbulence_transfers_to_healthy_pools()
{
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let primary = TurbulentExternalMessagesFakeServer::start(
        10_000,
        85,
        0x5eed_2026,
        vec![
            (StatusCode::UNAUTHORIZED, "invalid token"),
            (StatusCode::PAYMENT_REQUIRED, "no credit available"),
            (StatusCode::FORBIDDEN, "security precaution"),
            (StatusCode::TOO_MANY_REQUESTS, "rate limit"),
            (StatusCode::BAD_GATEWAY, "temporary upstream failure"),
            (
                StatusCode::from_u16(523).expect("523 is a valid HTTP status"),
                "origin unreachable",
            ),
        ],
        fake_external_success_body("primary-rare-ok"),
    )
    .await;
    let secondary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-secondary-ok"),
    )
    .await;
    let tertiary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-tertiary-ok"),
    )
    .await;
    let primary_pool = create_messages_pool_with_concurrency(
        &postgres,
        "random-mixed-primary",
        1,
        &primary.base_url,
        256,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "random-mixed-secondary",
        10,
        &secondary.base_url,
        256,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "random-mixed-tertiary",
        20,
        &tertiary.base_url,
        256,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 512,
        external_pool_retry_max_attempts: 4,
        external_pool_same_pool_retry_count: 0,
        external_pool_transient_failure_priority_penalty: 20,
        ..ExternalPoolsConfig::default()
    };

    let run_batch = |batch: &'static str, size: usize| {
        let manager = manager.clone();
        let config = config.clone();
        async move {
            futures::future::join_all((0..size).map(|index| {
                let manager = manager.clone();
                let config = config.clone();
                async move {
                    let mut route = test_route("claude-sonnet-4-6");
                    route.request_id = format!("req_{batch}_{index}");
                    route.error_id = format!("err_{batch}_{index}");
                    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(5));
                    timeout(
                        Duration::from_secs(5),
                        manager.forward_with_failover_result(config, route),
                    )
                    .await
                    .expect("random mixed turbulence request should finish")
                }
            }))
            .await
        }
    };

    for outcome in run_batch("random_mixed_wave1", 128).await {
        match outcome {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK);
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("healthy external pools should absorb mixed temporary failures: {error:?}")
            }
        }
    }
    let (primary_wave1_hits, primary_wave1_failures) = primary.snapshot();
    assert!(
        primary_wave1_failures > 20,
        "the first wave must exercise sustained mixed failures, got {primary_wave1_failures}"
    );
    let runtime = manager
        .load_pool_runtime_snapshot(primary_pool.id, &[])
        .await
        .expect("read primary runtime after random mixed wave");
    assert_eq!(
        runtime.pool_cooldown_remaining_secs, 0,
        "mixed 401/402/403/429/5xx turbulence must not hard-cool the primary pool"
    );
    assert!(
        runtime.transient_failure_streak >= primary_wave1_failures as u32,
        "mixed turbulence should accumulate soft evidence instead of being erased by rare successes"
    );

    tokio::time::sleep(Duration::from_millis(150)).await;

    for outcome in run_batch("random_mixed_wave2", 128).await {
        match outcome {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK);
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!(
                    "next traffic wave should stay on healthy pools while the primary is turbulent: {error:?}"
                )
            }
        }
    }
    let (primary_total_hits, _) = primary.snapshot();
    let primary_wave2_hits = primary_total_hits.saturating_sub(primary_wave1_hits);
    assert!(
        primary_wave2_hits <= 4,
        "soft health penalty should prevent a turbulent priority-1 pool from reclaiming sustained traffic; got {primary_wave2_hits} extra hits"
    );
    assert!(
        secondary.snapshot().saturating_add(tertiary.snapshot()) >= 220,
        "healthy backup pools should carry most successful traffic under sustained mixed turbulence"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn external_pool_high_concurrency_network_turbulence_transfers_to_healthy_pools() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let primary_base_url = unused_loopback_base_url().await;
    let secondary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-secondary-network-ok"),
    )
    .await;
    let tertiary = ExternalMessagesFakeServer::start(
        StatusCode::OK,
        fake_external_success_body("healthy-tertiary-network-ok"),
    )
    .await;
    let primary_pool = create_messages_pool_with_concurrency(
        &postgres,
        "network-turbulence-primary",
        1,
        &primary_base_url,
        128,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "network-turbulence-secondary",
        10,
        &secondary.base_url,
        128,
    )
    .await;
    create_messages_pool_with_concurrency(
        &postgres,
        "network-turbulence-tertiary",
        20,
        &tertiary.base_url,
        128,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 384,
        external_pool_retry_max_attempts: 3,
        external_pool_retry_on_network_error: true,
        external_pool_same_pool_retry_count: 0,
        external_pool_transient_failure_priority_penalty: 20,
        ..ExternalPoolsConfig::default()
    };

    let run_batch = |batch: &'static str| {
        let manager = manager.clone();
        let config = config.clone();
        async move {
            futures::future::join_all((0..64).map(|index| {
                let manager = manager.clone();
                let config = config.clone();
                async move {
                    let mut route = test_route("claude-sonnet-4-6");
                    route.request_id = format!("req_{batch}_{index}");
                    route.error_id = format!("err_{batch}_{index}");
                    route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
                    timeout(
                        Duration::from_secs(5),
                        manager.forward_with_failover_result(config, route),
                    )
                    .await
                    .expect("network turbulence request should finish")
                }
            }))
            .await
        }
    };

    for outcome in run_batch("network_turbulence_wave1").await {
        match outcome {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK);
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("healthy pools should absorb network failures: {error:?}")
            }
        }
    }
    let runtime = manager
        .load_pool_runtime_snapshot(primary_pool.id, &[])
        .await
        .expect("read primary runtime after network wave");
    assert_eq!(
        runtime.pool_cooldown_remaining_secs, 0,
        "connection-level turbulence must not hard-cool the primary pool"
    );
    assert!(
        runtime.transient_failure_streak > 0,
        "network failures should leave soft health evidence"
    );
    let first_streak = runtime.transient_failure_streak;

    tokio::time::sleep(Duration::from_millis(150)).await;

    for outcome in run_batch("network_turbulence_wave2").await {
        match outcome {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK);
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("subsequent traffic should stay on healthy pools: {error:?}")
            }
        }
    }
    let after_second = manager
        .load_pool_runtime_snapshot(primary_pool.id, &[])
        .await
        .expect("read primary runtime after second network wave");
    assert!(
        after_second.transient_failure_streak <= first_streak.saturating_add(2),
        "soft penalty should stop repeated high-concurrency traffic from pounding the bad network pool"
    );
    assert!(
        secondary.snapshot().saturating_add(tertiary.snapshot()) >= 120,
        "healthy backup pools should carry sustained network turbulence traffic"
    );

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_all_pools_sustained_502_recovers_without_long_blackout() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool_a_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
        20,
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
        fake_external_success_body("pool-a-recovered"),
    )
    .await;
    let pool_b_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
        20,
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
        fake_external_success_body("pool-b-recovered"),
    )
    .await;
    let pool_c_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
        20,
        StatusCode::BAD_GATEWAY,
        fake_external_error_body("temporary upstream failure"),
        fake_external_success_body("pool-c-recovered"),
    )
    .await;
    let pool_a = create_messages_pool_with_concurrency(
        &postgres,
        "sustained-all-primary",
        1,
        &pool_a_server.base_url,
        64,
    )
    .await;
    let pool_b = create_messages_pool_with_concurrency(
        &postgres,
        "sustained-all-secondary",
        10,
        &pool_b_server.base_url,
        64,
    )
    .await;
    let pool_c = create_messages_pool_with_concurrency(
        &postgres,
        "sustained-all-tertiary",
        20,
        &pool_c_server.base_url,
        64,
    )
    .await;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 192,
        external_pool_retry_max_attempts: 3,
        external_pool_retry_status_codes: vec![StatusCode::BAD_GATEWAY.as_u16()],
        external_pool_same_pool_retry_count: 0,
        external_pool_transient_failure_priority_penalty: 20,
        ..ExternalPoolsConfig::default()
    };

    for index in 0..20 {
        let mut route = test_route("claude-sonnet-4-6");
        route.request_id = format!("req_sustained_all_fail_{index}");
        route.error_id = format!("err_sustained_all_fail_{index}");
        route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
        let outcome = timeout(
            Duration::from_secs(3),
            manager.forward_with_failover_result(config.clone(), route),
        )
        .await
        .expect("sustained all-pool failure request should finish");
        assert!(
            matches!(outcome, ExternalPoolForwardOutcome::FinalError(_)),
            "all pools still failing should produce bounded final errors"
        );
    }
    assert_eq!(pool_a_server.snapshot(), 20);
    assert_eq!(pool_b_server.snapshot(), 20);
    assert_eq!(pool_c_server.snapshot(), 20);

    for pool in [&pool_a, &pool_b, &pool_c] {
        let runtime = manager
            .load_pool_runtime_snapshot(pool.id, &[])
            .await
            .expect("read runtime after sustained all-pool failure");
        assert_eq!(
            runtime.pool_cooldown_remaining_secs, 0,
            "sustained ordinary failures must not create long pool cooldown for {}",
            pool.name
        );
        assert!(
            runtime.transient_failure_streak >= 20,
            "sustained ordinary failures should accumulate soft evidence for {}",
            pool.name
        );
    }

    let mut recovered_route = test_route("claude-sonnet-4-6");
    recovered_route.request_id = "req_sustained_all_recovered".to_string();
    recovered_route.error_id = "err_sustained_all_recovered".to_string();
    recovered_route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
    let recovered = timeout(
        Duration::from_millis(750),
        manager.forward_with_failover_result(config, recovered_route),
    )
    .await
    .expect("recovered pools should be callable without waiting for long cooldown");
    match recovered {
        ExternalPoolForwardOutcome::Response(response) => {
            assert_eq!(response.status(), StatusCode::OK);
        }
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("recovered sustained-failure pools should not remain blacked out: {error:?}")
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_one_wave_account_capacity_and_quota_errors_soft_recover() {
    for (case, status, message) in [
        ("auth_401", StatusCode::UNAUTHORIZED, "invalid token"),
        (
            "security_403",
            StatusCode::FORBIDDEN,
            "account locked temporarily",
        ),
        (
            "quota_402",
            StatusCode::PAYMENT_REQUIRED,
            "no credit available",
        ),
        ("rate_429", StatusCode::TOO_MANY_REQUESTS, "rate limit"),
    ] {
        let Some((manager, postgres)) = test_external_pool_manager().await else {
            return;
        };
        let first_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
            1,
            status,
            fake_external_error_body(message),
            fake_external_success_body(&format!("{case}-first-recovered")),
        )
        .await;
        let second_server = FlakyExternalMessagesFakeServer::start_failures_then_success(
            1,
            status,
            fake_external_error_body(message),
            fake_external_success_body(&format!("{case}-second-recovered")),
        )
        .await;
        let first_pool = create_messages_pool_with_concurrency(
            &postgres,
            &format!("{case}-primary"),
            1,
            &first_server.base_url,
            32,
        )
        .await;
        let second_pool = create_messages_pool_with_concurrency(
            &postgres,
            &format!("{case}-secondary"),
            10,
            &second_server.base_url,
            32,
        )
        .await;

        let config = ExternalPoolsConfig {
            external_pools_enabled: true,
            external_pool_global_max_concurrent_requests: 64,
            external_pool_retry_max_attempts: 2,
            external_pool_retry_status_codes: vec![status.as_u16()],
            external_pool_same_pool_retry_count: 0,
            external_pool_rate_limit_cooldown_secs: 30,
            external_pool_protocol_error_cooldown_secs: 30,
            external_pool_transient_failure_priority_penalty: 20,
            ..ExternalPoolsConfig::default()
        };

        let mut first_route = test_route("claude-sonnet-4-6");
        first_route.request_id = format!("req_one_wave_{case}_first");
        first_route.error_id = format!("err_one_wave_{case}_first");
        first_route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
        let first = timeout(
            Duration::from_secs(3),
            manager.forward_with_failover_result(config.clone(), first_route),
        )
        .await
        .expect("first account/capacity/quota wave should finish");
        assert!(
            matches!(first, ExternalPoolForwardOutcome::FinalError(_)),
            "{case}: first request can fail when all pools fail once"
        );

        for pool in [&first_pool, &second_pool] {
            let runtime = manager
                .load_pool_runtime_snapshot(pool.id, &[])
                .await
                .expect("read runtime after one account/capacity/quota wave");
            assert_eq!(
                runtime.pool_cooldown_remaining_secs, 0,
                "{case}: one temporary status must not hard-cool pool {}",
                pool.name
            );
            assert_eq!(
                runtime.transient_failure_streak, 1,
                "{case}: one temporary status should only soft-penalize pool {}",
                pool.name
            );
        }

        let mut recovered_route = test_route("claude-sonnet-4-6");
        recovered_route.request_id = format!("req_one_wave_{case}_recovered");
        recovered_route.error_id = format!("err_one_wave_{case}_recovered");
        recovered_route.inference_attempt_budget = Arc::new(InferenceAttemptBudget::new(4));
        let recovered = timeout(
            Duration::from_millis(750),
            manager.forward_with_failover_result(config, recovered_route),
        )
        .await
        .expect("recovered account/capacity/quota pools must be callable immediately");
        match recovered {
            ExternalPoolForwardOutcome::Response(response) => {
                assert_eq!(response.status(), StatusCode::OK, "{case}");
            }
            ExternalPoolForwardOutcome::FinalError(error) => {
                panic!("{case}: recovered temporary pools should not remain blacked out: {error:?}")
            }
        }

        postgres.drop_test_schema().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_one_malformed_runtime_isolated_from_fifty_nine_healthy_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 120,
        ..ExternalPoolsConfig::default()
    };
    let mut pool_ids = Vec::with_capacity(60);
    for index in 0..60 {
        let pool = postgres
            .create_external_pool(create_pool_request(
                &format!("external-malformed-isolation-{index}"),
                index,
                true,
            ))
            .await
            .unwrap();
        pool_ids.push(pool.id);
    }
    let malformed_pool_id = pool_ids[0];
    let route = test_route("claude-sonnet-4-6");
    let redis_url = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL").unwrap();
    let redis_client = redis::Client::open(redis_url).unwrap();
    let mut raw_redis = redis_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let cooldown_key = manager
        .redis
        .key(format!("external_pool:{malformed_pool_id}:cooldown"));

    for malformed_kind in ["invalid_json", "list", "hash", "set"] {
        let _: i64 = redis::cmd("DEL")
            .arg(&cooldown_key)
            .query_async(&mut raw_redis)
            .await
            .unwrap();
        match malformed_kind {
            "invalid_json" => {
                let _: () = redis::cmd("SET")
                    .arg(&cooldown_key)
                    .arg("not-a-cooldown-state")
                    .arg("EX")
                    .arg(60)
                    .query_async(&mut raw_redis)
                    .await
                    .unwrap();
            }
            "list" => {
                let _: i64 = redis::cmd("LPUSH")
                    .arg(&cooldown_key)
                    .arg("wrong-type")
                    .query_async(&mut raw_redis)
                    .await
                    .unwrap();
            }
            "hash" => {
                let _: i64 = redis::cmd("HSET")
                    .arg(&cooldown_key)
                    .arg("wrong")
                    .arg("type")
                    .query_async(&mut raw_redis)
                    .await
                    .unwrap();
            }
            "set" => {
                let _: i64 = redis::cmd("SADD")
                    .arg(&cooldown_key)
                    .arg("wrong-type")
                    .query_async(&mut raw_redis)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }

        for round in 0..5 {
            let selection = manager
                .select_pool_for_route(&HashSet::new(), &config, &route)
                .await;
            let selected = selection.selected_pool.unwrap_or_else(|| {
                panic!("{malformed_kind} round {round}: healthy pools must remain selectable")
            });
            assert_ne!(
                selected.id, malformed_pool_id,
                "{malformed_kind} round {round}"
            );
            assert_eq!(
                selection.availability.eligible_pools, 60,
                "{malformed_kind} round {round}"
            );
            assert_eq!(
                selection.availability.available_pools, 59,
                "{malformed_kind} round {round}"
            );
            assert_eq!(
                selection.availability.invalid_runtime_pools, 1,
                "{malformed_kind} round {round}"
            );
            assert!(
                !selection.availability.coordinator_unavailable,
                "{malformed_kind} round {round}: one malformed pool must not fail the batch"
            );

            let statuses = manager
                .status(&config)
                .await
                .expect("status must not be 500");
            assert_eq!(statuses.len(), 60, "{malformed_kind} round {round}");
            let malformed = statuses
                .iter()
                .find(|status| status.pool.id == malformed_pool_id)
                .expect("malformed pool status must be returned");
            assert!(!malformed.dispatchable, "{malformed_kind} round {round}");
            assert_eq!(
                malformed.skipped_reason.as_deref(),
                Some("coordinator_state_invalid"),
                "{malformed_kind} round {round}"
            );
            assert_eq!(
                statuses.iter().filter(|status| status.dispatchable).count(),
                59,
                "{malformed_kind} round {round}"
            );
        }
    }

    let _: i64 = redis::cmd("DEL")
        .arg(&cooldown_key)
        .query_async(&mut raw_redis)
        .await
        .unwrap();

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_heartbeat_redis_faults_fail_closed_before_prune_and_recover_five_of_five() {
    let Ok(toxiproxy_api) = std::env::var("KIRO_RS_TEST_TOXIPROXY_API") else {
        eprintln!("跳过 heartbeat Redis 故障测试：未设置 KIRO_RS_TEST_TOXIPROXY_API");
        return;
    };
    let Ok(direct_redis_url) = std::env::var("KIRO_RS_TEST_REDIS_DIRECT_URL") else {
        eprintln!("跳过 heartbeat Redis 故障测试：未设置 KIRO_RS_TEST_REDIS_DIRECT_URL");
        return;
    };
    let proxy_name =
        std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").unwrap_or_else(|_| "redis".to_string());
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let mut direct_redis_config = Config::default();
    direct_redis_config.redis.url = Some(direct_redis_url);
    direct_redis_config.redis.key_prefix = manager.redis.key_prefix_for_test();
    let direct_redis = RedisStore::connect(&direct_redis_config).await.unwrap();
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 1,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-heartbeat-faults", 1, true))
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let toxic_base = format!(
        "{}/proxies/{}/toxics",
        toxiproxy_api.trim_end_matches('/'),
        proxy_name
    );
    let max_age = Duration::from_millis(900);
    let cooldown_keys = vec![format!("external_pool:{}:cooldown", pool.id)];

    for fault in ["latency", "reset_peer"] {
        for round in 0..5 {
            let lease = match manager
                .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
                .await
            {
                PoolAcquireResult::Acquired(lease) => lease,
                PoolAcquireResult::Unavailable(unavailable) => {
                    panic!(
                        "{fault} round {round}: initial acquire: {}",
                        unavailable.detail
                    )
                }
            };
            let lease_acquired_at = Instant::now();
            let toxic_name = format!("external-heartbeat-{fault}-{round}");
            let toxic = if fault == "latency" {
                json!({
                    "name": toxic_name.clone(),
                    "type": "latency",
                    "stream": "downstream",
                    "toxicity": 1.0,
                    "attributes": { "latency": 500, "jitter": 0 },
                })
            } else {
                json!({
                    "name": toxic_name.clone(),
                    "type": "reset_peer",
                    "stream": "downstream",
                    "toxicity": 1.0,
                    "attributes": { "timeout": 0 },
                })
            };
            let install = client.post(&toxic_base).json(&toxic).send().await.unwrap();
            assert!(
                install.status().is_success(),
                "{fault} round {round}: {install:?}"
            );

            let before_lost = direct_redis
                .acquire_external_pool_lease(
                    pool.id,
                    &format!("pre-loss-probe-{fault}-{round}"),
                    &lease.coordination_epoch,
                    1,
                    1,
                    Some(max_age),
                    &cooldown_keys,
                )
                .await
                .unwrap();
            assert!(
                matches!(
                    before_lost,
                    RedisExternalPoolLeaseAcquireResult::PoolCapacityFull { .. }
                        | RedisExternalPoolLeaseAcquireResult::GlobalCapacityFull { .. }
                ),
                "{fault} round {round}: a second manager must not acquire before heartbeat loss: {before_lost:?}"
            );
            let lost = tokio::time::timeout(Duration::from_secs(2), lease.wait_until_lost()).await;
            let lost_elapsed = lease_acquired_at.elapsed();
            let remove = client
                .delete(format!("{toxic_base}/{toxic_name}"))
                .send()
                .await
                .unwrap();
            assert!(
                remove.status().is_success(),
                "{fault} round {round}: {remove:?}"
            );
            lost.expect("heartbeat must fail closed before the Redis prune deadline");
            assert!(
                lost_elapsed < max_age,
                "{fault} round {round}: heartbeat loss {lost_elapsed:?} must precede max-age {max_age:?}"
            );
            let heartbeat = lease.heartbeat.as_ref().unwrap();
            assert!(heartbeat.state.lost.load(Ordering::Acquire));
            assert!(
                heartbeat.state.attempts.load(Ordering::Relaxed) <= 2,
                "{fault} round {round}: heartbeat retry attempts must remain bounded"
            );

            drop(lease);
            let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
            assert!(drained.drained, "{fault} round {round}: release must drain");
            let recovered = match manager
                .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
                .await
            {
                PoolAcquireResult::Acquired(lease) => lease,
                PoolAcquireResult::Unavailable(unavailable) => {
                    panic!("{fault} round {round}: recovery: {}", unavailable.detail)
                }
            };
            drop(recovered);
            let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
            assert!(
                drained.drained,
                "{fault} round {round}: recovery release must drain"
            );
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_coordinator_breaker_bounds_10k_failures_and_single_recovery_probe_for_five_rounds()
 {
    let Ok(toxiproxy_api) = std::env::var("KIRO_RS_TEST_TOXIPROXY_API") else {
        eprintln!("跳过 coordinator breaker 压力测试：未设置 KIRO_RS_TEST_TOXIPROXY_API");
        return;
    };
    let proxy_name =
        std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").unwrap_or_else(|_| "redis".to_string());
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-breaker-burst", 1, true))
        .await
        .unwrap();
    let pool_ids = vec![pool.id];
    let client = reqwest::Client::new();
    let toxic_base = format!(
        "{}/proxies/{}/toxics",
        toxiproxy_api.trim_end_matches('/'),
        proxy_name
    );

    for round in 0..5 {
        manager.redis.reset_external_pool_hot_path_round_trips();
        let toxic_name = format!("external-breaker-burst-{round}");
        let install = client
            .post(&toxic_base)
            .json(&json!({
                "name": toxic_name.clone(),
                "type": "reset_peer",
                "stream": "downstream",
                "toxicity": 1.0,
                "attributes": { "timeout": 0 },
            }))
            .send()
            .await
            .unwrap();
        assert!(install.status().is_success(), "round {round}: {install:?}");

        assert!(
            manager
                .load_pool_runtime_snapshots(&pool_ids, &[])
                .await
                .is_err(),
            "round {round}: injected Redis failure must open the breaker"
        );
        let remove = client
            .delete(format!("{toxic_base}/{toxic_name}"))
            .send()
            .await
            .unwrap();
        assert!(remove.status().is_success(), "round {round}: {remove:?}");
        assert_eq!(manager.redis.external_pool_hot_path_round_trips(), 1);

        let fail_fast = futures::future::join_all(
            (0..10_000).map(|_| manager.load_pool_runtime_snapshots(&pool_ids, &[])),
        )
        .await;
        assert!(
            fail_fast.iter().all(|result| result
                .as_ref()
                .is_err_and(|err| err.to_string().contains("coordinator breaker is open"))),
            "round {round}: every request inside the open window must fail fast"
        );
        assert_eq!(
            manager.redis.external_pool_hot_path_round_trips(),
            1,
            "round {round}: 10k fail-fast requests must not amplify Redis operations"
        );

        tokio::time::sleep(Duration::from_millis(1_050)).await;
        let recovery_race = futures::future::join_all(
            (0..128).map(|_| manager.load_pool_runtime_snapshots(&pool_ids, &[])),
        )
        .await;
        assert_eq!(
            recovery_race.iter().filter(|result| result.is_ok()).count(),
            1,
            "round {round}: exactly one recovery probe may reach Redis"
        );
        assert_eq!(
            manager.redis.external_pool_hot_path_round_trips(),
            2,
            "round {round}: recovery race must add exactly one Redis RTT"
        );

        for recovery_probe in 0..5 {
            let recovered = manager
                .load_pool_runtime_snapshots(&pool_ids, &[])
                .await
                .unwrap();
            assert_eq!(recovered.len(), 1, "round {round}, {recovery_probe}");
            assert!(recovered[0].is_ok(), "round {round}, {recovery_probe}");
        }
        assert_eq!(
            manager.redis.external_pool_hot_path_round_trips(),
            7,
            "round {round}: five healthy probes must each use one batch RTT"
        );
    }

    assert!(
        manager
            .coordinator_breaker
            .stats
            .fail_fast
            .load(Ordering::Relaxed)
            >= 5 * (10_000 + 127)
    );
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_selection_runtime_snapshot_coalesces_128_waiters_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request(
            "external-selection-singleflight",
            1,
            true,
        ))
        .await
        .unwrap();
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let authoritative = manager.load_authoritative_pool_snapshot().await.unwrap();
    let excluded = HashSet::new();

    manager.redis.reset_external_pool_hot_path_round_trips();
    let bootstrap = manager
        .scan_pool_availability_from_snapshot(
            &authoritative,
            &excluded,
            &config,
            true,
            None,
            None,
            None,
            None,
        )
        .await;
    assert_eq!(
        bootstrap.selected_pool.as_ref().map(|pool| pool.id),
        Some(pool.id),
        "coordinator bootstrap must observe the available pool before measuring warm coalescing"
    );
    let bootstrap_rtts = manager.redis.external_pool_hot_path_round_trips();
    assert!(
        (1..=5).contains(&bootstrap_rtts),
        "coordinator bootstrap must stay bounded before warm coalescing measurement; rtts={bootstrap_rtts}"
    );
    *manager.selection_runtime_snapshot.lock() = None;

    for round in 0..5 {
        if round > 0 {
            tokio::time::sleep(
                EXTERNAL_POOL_SELECTION_RUNTIME_SNAPSHOT_TTL + Duration::from_millis(20),
            )
            .await;
        }
        manager.redis.reset_external_pool_hot_path_round_trips();
        let selections = futures::future::join_all((0..128).map(|_| {
            manager.scan_pool_availability_from_snapshot(
                &authoritative,
                &excluded,
                &config,
                true,
                None,
                None,
                None,
                None,
            )
        }))
        .await;
        assert!(
            selections.iter().all(|selection| {
                selection.selected_pool.as_ref().map(|pool| pool.id) == Some(pool.id)
            }),
            "round {round}: every waiter must observe the available pool"
        );
        assert_eq!(
            manager.redis.external_pool_hot_path_round_trips(),
            1,
            "round {round}: 128 simultaneous waiters must share one Redis runtime snapshot"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_coordinator_admission_bounds_10k_simultaneous_first_timeout_wave_for_five_rounds()
 {
    let Ok(toxiproxy_api) = std::env::var("KIRO_RS_TEST_TOXIPROXY_API") else {
        eprintln!("跳过 coordinator 首波压力测试：未设置 KIRO_RS_TEST_TOXIPROXY_API");
        return;
    };
    let proxy_name =
        std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").unwrap_or_else(|_| "redis".to_string());
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-breaker-first-wave", 1, true))
        .await
        .unwrap();
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let pool_ids = vec![pool.id];
    let excluded = HashSet::new();
    let client = reqwest::Client::new();
    let toxic_base = format!(
        "{}/proxies/{}/toxics",
        toxiproxy_api.trim_end_matches('/'),
        proxy_name
    );

    for round in 0..5 {
        manager.redis.reset_external_pool_hot_path_round_trips();
        let saturated_before = manager.selection_saturated.load(Ordering::Acquire);
        let toxic_name = format!("external-breaker-first-wave-{round}");
        let install = client
            .post(&toxic_base)
            .json(&json!({
                "name": toxic_name.clone(),
                "type": "latency",
                "stream": "downstream",
                "toxicity": 1.0,
                "attributes": { "latency": 2200, "jitter": 0 },
            }))
            .send()
            .await
            .unwrap();
        assert!(install.status().is_success(), "round {round}: {install:?}");

        let first_wave = futures::future::join_all((0..10_000).map(|_| {
            manager.scan_pool_availability_uncached(&excluded, &config, true, None, None, None)
        }))
        .await;
        let remove = client
            .delete(format!("{toxic_base}/{toxic_name}"))
            .send()
            .await
            .unwrap();
        assert!(remove.status().is_success(), "round {round}: {remove:?}");
        assert!(
            first_wave.iter().all(|selection| {
                selection.selected_pool.is_none() && selection.availability.coordinator_unavailable
            }),
            "round {round}: the injected first timeout wave must fail closed"
        );
        let redis_attempts = manager.redis.external_pool_hot_path_round_trips();
        assert!(redis_attempts > 0, "round {round}");
        assert!(
            redis_attempts <= EXTERNAL_POOL_COORDINATOR_MAX_IN_FLIGHT as u64,
            "round {round}: first-wave Redis attempts {redis_attempts} exceeded the hard cap"
        );
        assert!(
            manager
                .selection_saturated
                .load(Ordering::Acquire)
                .saturating_sub(saturated_before)
                >= (10_000 - EXTERNAL_POOL_COORDINATOR_MAX_IN_FLIGHT) as u64,
            "round {round}: excess requests must be rejected before spawning Redis work"
        );

        tokio::time::sleep(Duration::from_millis(1_250)).await;
        let attempts_before_recovery = manager.redis.external_pool_hot_path_round_trips();
        let recovery_race = futures::future::join_all(
            (0..128).map(|_| manager.load_pool_runtime_snapshots(&pool_ids, &[])),
        )
        .await;
        assert_eq!(
            recovery_race.iter().filter(|result| result.is_ok()).count(),
            1,
            "round {round}: exactly one recovery probe must succeed"
        );
        assert_eq!(
            manager.redis.external_pool_hot_path_round_trips(),
            attempts_before_recovery + 1,
            "round {round}: the recovery race must add exactly one Redis attempt"
        );
        for recovery_probe in 0..5 {
            let recovered = manager
                .scan_pool_availability_uncached(&excluded, &config, true, None, None, None)
                .await;
            assert_eq!(
                recovered.selected_pool.map(|pool| pool.id),
                Some(pool.id),
                "round {round}, recovery probe {recovery_probe}"
            );
        }
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_coordinator_clean_startup_has_no_recovery_barrier_for_five_rounds() {
    for round in 0..5 {
        let Some((manager, postgres)) = test_external_pool_manager().await else {
            return;
        };
        let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_config WHERE id = $1")
            .bind(EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID)
            .fetch_one(postgres.pool())
            .await
            .unwrap();
        assert_eq!(existing, 0, "round {round}: test schema must start clean");

        let started = Instant::now();
        let epoch = manager.external_pool_coordination_epoch().await.unwrap();
        let first_start_elapsed = started.elapsed();
        assert!(!epoch.is_empty(), "round {round}");
        assert!(
            first_start_elapsed < Duration::from_secs(2),
            "round {round}: clean startup unexpectedly waited for recovery: {first_start_elapsed:?}"
        );
        assert_eq!(
            manager
                .redis
                .external_pool_coordinator_guard_state_for_epoch(&epoch)
                .await
                .unwrap(),
            ExternalPoolCoordinatorGuardState::Ready {
                coordination_epoch: epoch.clone(),
            },
            "round {round}: clean startup must not install a recovery barrier"
        );

        let authority: serde_json::Value =
            sqlx::query_scalar("SELECT config FROM runtime_config WHERE id = $1")
                .bind(EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID)
                .fetch_one(postgres.pool())
                .await
                .unwrap();
        assert_eq!(
            authority
                .get("coordinationEpoch")
                .and_then(serde_json::Value::as_str),
            Some(epoch.as_str()),
            "round {round}"
        );
        assert!(
            authority
                .get("redisRunId")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|run_id| !run_id.is_empty()),
            "round {round}: authority must persist the Redis run id"
        );

        let peer = ExternalPoolManager::new(postgres.clone(), manager.redis.clone());
        let peer_started = Instant::now();
        let peer_epoch = peer.external_pool_coordination_epoch().await.unwrap();
        assert_eq!(
            peer_epoch, epoch,
            "round {round}: peer rotated a clean epoch"
        );
        assert!(
            peer_started.elapsed() < Duration::from_secs(2),
            "round {round}: peer startup unexpectedly waited for recovery"
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runtime_config WHERE id = $1")
            .bind(EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID)
            .fetch_one(postgres.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1, "round {round}");
        postgres.drop_test_schema().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_redis_disconnect_fails_closed_and_recovers() {
    let Ok(toxiproxy_api) = std::env::var("KIRO_RS_TEST_TOXIPROXY_API") else {
        eprintln!("跳过 Redis 断连集成测试：未设置 KIRO_RS_TEST_TOXIPROXY_API");
        return;
    };
    let proxy_name =
        std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").unwrap_or_else(|_| "redis".to_string());
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-redis-disconnect", 1, true))
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let toxic_base = format!(
        "{}/proxies/{}/toxics",
        toxiproxy_api.trim_end_matches('/'),
        proxy_name
    );

    for round in 0..5 {
        let healthy = manager
            .scan_pool_availability_uncached(&HashSet::new(), &config, true, None, None, None)
            .await;
        assert_eq!(
            healthy.selected_pool.as_ref().map(|selected| selected.id),
            Some(pool.id),
            "round {round}: coordinator should start healthy"
        );
        let epoch_before = manager
            .coordinator_epoch
            .lock()
            .clone()
            .expect("healthy coordinator must cache an epoch");
        let run_id_before = manager
            .coordinator_run_id
            .lock()
            .clone()
            .expect("healthy coordinator must cache a Redis run id");
        let authority_version_before: i64 =
            sqlx::query_scalar("SELECT version FROM runtime_config WHERE id = $1")
                .bind(EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID)
                .fetch_one(postgres.pool())
                .await
                .unwrap();

        let toxic_name = format!("external-reset-peer-{round}");
        let install = client
            .post(&toxic_base)
            .json(&json!({
                "name": toxic_name.clone(),
                "type": "reset_peer",
                "stream": "downstream",
                "toxicity": 1.0,
                "attributes": { "timeout": 0 },
            }))
            .send()
            .await
            .unwrap();
        assert!(install.status().is_success(), "round {round}: {install:?}");

        let disrupted = tokio::time::timeout(
            Duration::from_secs(3),
            manager.scan_pool_availability_uncached(
                &HashSet::new(),
                &config,
                true,
                None,
                None,
                None,
            ),
        )
        .await;
        let remove = client
            .delete(format!("{toxic_base}/{toxic_name}"))
            .send()
            .await
            .unwrap();
        assert!(remove.status().is_success(), "round {round}: {remove:?}");

        let disrupted = disrupted.expect("Redis disconnect must fail closed within 3 seconds");
        assert!(disrupted.selected_pool.is_none(), "round {round}");
        assert!(
            disrupted.availability.coordinator_unavailable,
            "round {round}"
        );
        assert_eq!(
            disrupted.availability.wait_reason,
            Some(PoolCapacityWaitReason::CoordinatorUnavailable),
            "round {round}"
        );

        let recovered = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let selection = manager
                    .scan_pool_availability_uncached(
                        &HashSet::new(),
                        &config,
                        true,
                        None,
                        None,
                        None,
                    )
                    .await;
                if selection.selected_pool.is_some() {
                    break selection;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("Redis coordinator should recover after reset_peer is removed");
        assert_eq!(
            recovered.selected_pool.map(|selected| selected.id),
            Some(pool.id),
            "round {round}"
        );
        for recovery_probe in 0..5 {
            let recovered = manager
                .scan_pool_availability_uncached(&HashSet::new(), &config, true, None, None, None)
                .await;
            assert_eq!(
                recovered.selected_pool.map(|selected| selected.id),
                Some(pool.id),
                "round {round}, recovery probe {recovery_probe}: coordinator must remain healthy"
            );
        }
        assert_eq!(
            manager.external_pool_coordination_epoch().await.unwrap(),
            epoch_before,
            "round {round}: a transport disconnect must not rotate the epoch"
        );
        assert_eq!(
            manager.coordinator_run_id.lock().as_deref(),
            Some(run_id_before.as_str()),
            "round {round}: a transport disconnect must not change Redis run_id"
        );
        assert_eq!(
            manager
                .redis
                .external_pool_coordinator_guard_state_for_epoch(&epoch_before)
                .await
                .unwrap(),
            ExternalPoolCoordinatorGuardState::Ready {
                coordination_epoch: epoch_before.clone(),
            },
            "round {round}: disconnect recovery must not install a grace barrier"
        );
        let authority_version_after: i64 =
            sqlx::query_scalar("SELECT version FROM runtime_config WHERE id = $1")
                .bind(EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID)
                .fetch_one(postgres.pool())
                .await
                .unwrap();
        assert_eq!(
            authority_version_after, authority_version_before,
            "round {round}: disconnect recovery must not rewrite authority"
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_redis_restart_fails_closed_and_recovers_five_of_five() {
    let Ok(container) = std::env::var("KIRO_RS_TEST_REDIS_RESTART_CONTAINER") else {
        eprintln!("跳过 Redis restart 集成测试：未设置 KIRO_RS_TEST_REDIS_RESTART_CONTAINER");
        return;
    };
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = manager.with_coordinator_recovery_grace(Duration::from_millis(600));
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request("external-redis-restart", 1, true))
        .await
        .unwrap();

    for round in 0..5 {
        let healthy = manager
            .scan_pool_availability_uncached(&HashSet::new(), &config, true, None, None, None)
            .await;
        assert_eq!(
            healthy.selected_pool.map(|selected| selected.id),
            Some(pool.id),
            "round {round}: coordinator must be healthy before restart"
        );

        let stop_container = container.clone();
        let stop = tokio::task::spawn_blocking(move || {
            std::process::Command::new("docker")
                .args(["stop", "--timeout", "1", &stop_container])
                .status()
        })
        .await
        .unwrap();
        let disrupted = tokio::time::timeout(
            Duration::from_secs(3),
            manager.scan_pool_availability_uncached(
                &HashSet::new(),
                &config,
                true,
                None,
                None,
                None,
            ),
        )
        .await;
        let start_container = container.clone();
        let start = tokio::task::spawn_blocking(move || {
            std::process::Command::new("docker")
                .args(["start", &start_container])
                .status()
        })
        .await
        .unwrap();

        assert!(stop.unwrap().success(), "round {round}: docker stop failed");
        assert!(
            start.unwrap().success(),
            "round {round}: docker start failed"
        );
        let disrupted = disrupted.expect("Redis restart failure must be bounded within 3 seconds");
        assert!(disrupted.selected_pool.is_none(), "round {round}");
        assert!(
            disrupted.availability.coordinator_unavailable,
            "round {round}: Redis downtime must fail closed"
        );

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let recovered = manager
                    .scan_pool_availability_uncached(
                        &HashSet::new(),
                        &config,
                        true,
                        None,
                        None,
                        None,
                    )
                    .await;
                if recovered.selected_pool.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("Redis coordinator must reconnect after restart");

        for recovery_probe in 0..5 {
            let recovered = manager
                .scan_pool_availability_uncached(&HashSet::new(), &config, true, None, None, None)
                .await;
            assert_eq!(
                recovered.selected_pool.map(|selected| selected.id),
                Some(pool.id),
                "round {round}, recovery probe {recovery_probe}: coordinator must remain healthy"
            );
        }
        eprintln!("external_pool_redis_restart round={round} recovery=5/5");
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_redis_data_loss_fences_active_confirmed_lease_before_reacquire() {
    let Ok(container) = std::env::var("KIRO_RS_TEST_REDIS_RESTART_CONTAINER") else {
        eprintln!(
            "跳过 active lease Redis restart 测试：未设置 KIRO_RS_TEST_REDIS_RESTART_CONTAINER"
        );
        return;
    };
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    // Keep the restart setup below the heartbeat's first 5s probe while leaving enough
    // recovery time for that probe to fence the old epoch before a fresh lease is admitted.
    let recovery_grace = Duration::from_secs(8);
    let mut manager = manager.with_coordinator_recovery_grace(recovery_grace);
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 1,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request(
            "external-redis-active-lease-restart",
            1,
            true,
        ))
        .await
        .unwrap();
    let max_age = Duration::from_secs(15);
    for round in 0..5 {
        let old_lease = match manager
            .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
            .await
        {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(unavailable) => {
                panic!(
                    "round {round}: initial active lease unavailable: {}",
                    unavailable.detail
                )
            }
        };
        let old_epoch = old_lease.coordination_epoch.clone();

        let kill_container = container.clone();
        let killed = tokio::task::spawn_blocking(move || {
            std::process::Command::new("docker")
                .args(["kill", "--signal", "KILL", &kill_container])
                .status()
        })
        .await
        .unwrap()
        .unwrap();
        assert!(killed.success(), "round {round}: docker kill failed");
        let start_container = container.clone();
        let started = tokio::task::spawn_blocking(move || {
            std::process::Command::new("docker")
                .args(["start", &start_container])
                .status()
        })
        .await
        .unwrap()
        .unwrap();
        assert!(started.success(), "round {round}: docker start failed");

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if manager
                    .redis
                    .del("external_pool:restart-readiness")
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Redis must reconnect before the old heartbeat deadline");
        for suffix in [
            format!("external_pool:inflight:{}:last_seen", pool.id),
            format!("external_pool:inflight:{}:acquired", pool.id),
            "external_pool:global:inflight:last_seen".to_string(),
            "external_pool:global:inflight:acquired".to_string(),
        ] {
            manager.redis.del(suffix).await.unwrap();
        }

        let mut fresh_redis_config = Config::default();
        fresh_redis_config.redis.url =
            Some(crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL").unwrap());
        fresh_redis_config.redis.key_prefix = manager.redis.key_prefix_for_test();
        let fresh_manager = ExternalPoolManager::new(
            postgres.clone(),
            Arc::new(RedisStore::connect(&fresh_redis_config).await.unwrap()),
        )
        .with_coordinator_recovery_grace(recovery_grace);
        assert!(
            !old_lease
                .heartbeat
                .as_ref()
                .unwrap()
                .state
                .lost
                .load(Ordering::Acquire),
            "round {round}: reacquire must be attempted before old heartbeat fencing"
        );

        for barrier_probe in 0..5 {
            let reacquire = fresh_manager
                .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
                .await;
            let PoolAcquireResult::Unavailable(unavailable) = reacquire else {
                panic!(
                    "round {round}, barrier probe {barrier_probe}: second lease admitted before fencing"
                );
            };
            assert_eq!(
                unavailable.reason,
                PoolCapacityWaitReason::CoordinatorUnavailable
            );
            assert_eq!(unavailable.detail, "coordinator_restart_recovery");
            assert!(unavailable.wait_for.is_some());
        }

        tokio::time::timeout(Duration::from_secs(5), old_lease.wait_until_lost())
            .await
            .unwrap_or_else(|_| panic!("round {round}: old epoch heartbeat was not fenced"));
        assert_ne!(
            fresh_manager.coordinator_epoch.lock().as_deref(),
            Some(old_epoch.as_str()),
            "round {round}: Redis restart must rotate the shared coordinator epoch"
        );

        let recovered = tokio::time::timeout(recovery_grace + Duration::from_secs(3), async {
            loop {
                match fresh_manager
                    .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
                    .await
                {
                    PoolAcquireResult::Acquired(lease) => break lease,
                    PoolAcquireResult::Unavailable(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: coordinator did not recover after barrier"));
        assert!(
            old_lease
                .heartbeat
                .as_ref()
                .unwrap()
                .state
                .lost
                .load(Ordering::Acquire),
            "round {round}: new lease must not precede old upstream fencing"
        );
        drop(recovered);
        drop(old_lease);
        let (old_drained, fresh_drained) = tokio::join!(
            manager.drain_release_intents(Duration::from_secs(5)),
            fresh_manager.drain_release_intents(Duration::from_secs(5)),
        );
        assert!(old_drained.drained, "round {round}: old release must drain");
        assert!(
            fresh_drained.drained,
            "round {round}: fresh release must drain"
        );
        manager = fresh_manager;
    }
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_pool_redis_restart_fences_multiple_active_leases_across_managers_for_five_rounds()
{
    let Ok(container) = std::env::var("KIRO_RS_TEST_REDIS_RESTART_CONTAINER") else {
        eprintln!("跳过多 active lease Redis restart 测试：未设置 restart container");
        return;
    };
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let recovery_grace = Duration::from_secs(8);
    let mut manager = manager.with_coordinator_recovery_grace(recovery_grace);
    let mut peer_redis_config = Config::default();
    peer_redis_config.redis.url =
        Some(crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL").unwrap());
    peer_redis_config.redis.key_prefix = manager.redis.key_prefix_for_test();
    let peer = ExternalPoolManager::new(
        postgres.clone(),
        Arc::new(RedisStore::connect(&peer_redis_config).await.unwrap()),
    )
    .with_coordinator_recovery_grace(recovery_grace);
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 4,
        ..ExternalPoolsConfig::default()
    };
    let mut request = create_pool_request("external-redis-multi-active-restart", 1, true);
    request.max_concurrent_requests = 4;
    let pool = postgres.create_external_pool(request).await.unwrap();
    let max_age = Duration::from_secs(15);

    for round in 0..5 {
        let mut old_leases = Vec::with_capacity(4);
        for owner in [&manager, &manager, &peer, &peer] {
            match owner
                .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
                .await
            {
                PoolAcquireResult::Acquired(lease) => old_leases.push(lease),
                PoolAcquireResult::Unavailable(unavailable) => panic!(
                    "round {round}: failed to seed four active leases: {}",
                    unavailable.detail
                ),
            }
        }
        let old_epoch = old_leases[0].coordination_epoch.clone();
        assert!(
            old_leases
                .iter()
                .all(|lease| lease.coordination_epoch == old_epoch),
            "round {round}: managers did not share one epoch"
        );

        let kill_container = container.clone();
        let killed = tokio::task::spawn_blocking(move || {
            std::process::Command::new("docker")
                .args(["kill", "--signal", "KILL", &kill_container])
                .status()
        })
        .await
        .unwrap()
        .unwrap();
        assert!(killed.success(), "round {round}: docker kill failed");
        let start_container = container.clone();
        let started = tokio::task::spawn_blocking(move || {
            std::process::Command::new("docker")
                .args(["start", &start_container])
                .status()
        })
        .await
        .unwrap()
        .unwrap();
        assert!(started.success(), "round {round}: docker start failed");

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if manager
                    .redis
                    .del("external_pool:multi-restart-readiness")
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Redis must reconnect before the old heartbeat probe");
        for suffix in [
            format!("external_pool:inflight:{}:last_seen", pool.id),
            format!("external_pool:inflight:{}:acquired", pool.id),
            "external_pool:global:inflight:last_seen".to_string(),
            "external_pool:global:inflight:acquired".to_string(),
        ] {
            manager.redis.del(suffix).await.unwrap();
        }

        let fresh = ExternalPoolManager::new(
            postgres.clone(),
            Arc::new(RedisStore::connect(&peer_redis_config).await.unwrap()),
        )
        .with_coordinator_recovery_grace(recovery_grace);
        assert!(
            old_leases.iter().all(|lease| !lease
                .heartbeat
                .as_ref()
                .unwrap()
                .state
                .lost
                .load(Ordering::Acquire)),
            "round {round}: fresh manager must race before old heartbeat fencing"
        );
        for probe in 0..5 {
            let PoolAcquireResult::Unavailable(unavailable) = fresh
                .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
                .await
            else {
                panic!("round {round}, probe {probe}: lease admitted inside recovery barrier");
            };
            assert_eq!(unavailable.detail, "coordinator_restart_recovery");
        }

        tokio::time::timeout(
            Duration::from_secs(6),
            futures::future::join_all(old_leases.iter().map(ExternalPoolLease::wait_until_lost)),
        )
        .await
        .unwrap_or_else(|_| panic!("round {round}: all four old heartbeats were not fenced"));
        assert!(
            old_leases.iter().all(|lease| lease
                .heartbeat
                .as_ref()
                .unwrap()
                .state
                .lost
                .load(Ordering::Acquire)),
            "round {round}: at least one old upstream remained active"
        );
        assert_ne!(
            fresh.coordinator_epoch.lock().as_deref(),
            Some(old_epoch.as_str()),
            "round {round}: restart must rotate the shared epoch"
        );

        let recovered = tokio::time::timeout(recovery_grace + Duration::from_secs(3), async {
            loop {
                match fresh
                    .acquire_pool_with_model_cooldowns_and_max_age(&pool, &config, &[], max_age)
                    .await
                {
                    PoolAcquireResult::Acquired(lease) => break lease,
                    PoolAcquireResult::Unavailable(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("round {round}: fresh manager did not recover"));
        drop(recovered);
        drop(old_leases);
        let (manager_drain, peer_drain, fresh_drain) = tokio::join!(
            manager.drain_release_intents(Duration::from_secs(5)),
            peer.drain_release_intents(Duration::from_secs(5)),
            fresh.drain_release_intents(Duration::from_secs(5)),
        );
        assert!(manager_drain.drained, "round {round}: manager drain");
        assert!(peer_drain.drained, "round {round}: peer drain");
        assert!(fresh_drain.drained, "round {round}: fresh drain");
        manager = fresh;
    }
    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_redis_acquire_timeout_reclaims_commit_unknown_and_recovers() {
    let Ok(toxiproxy_api) = std::env::var("KIRO_RS_TEST_TOXIPROXY_API") else {
        eprintln!("跳过 Redis commit-unknown 集成测试：未设置 KIRO_RS_TEST_TOXIPROXY_API");
        return;
    };
    let proxy_name =
        std::env::var("KIRO_RS_TEST_TOXIPROXY_NAME").unwrap_or_else(|_| "redis".to_string());
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        ..ExternalPoolsConfig::default()
    };
    let pool = postgres
        .create_external_pool(create_pool_request(
            "external-redis-commit-unknown",
            1,
            true,
        ))
        .await
        .unwrap();
    let client = reqwest::Client::new();
    let toxic_base = format!(
        "{}/proxies/{}/toxics",
        toxiproxy_api.trim_end_matches('/'),
        proxy_name
    );

    for round in 0..5 {
        let toxic_name = format!("external-commit-unknown-{round}");
        let install = client
            .post(&toxic_base)
            .json(&json!({
                "name": toxic_name.clone(),
                "type": "latency",
                "stream": "downstream",
                "toxicity": 1.0,
                "attributes": { "latency": 2200, "jitter": 0 },
            }))
            .send()
            .await
            .unwrap();
        assert!(install.status().is_success(), "round {round}: {install:?}");

        let started = Instant::now();
        let delayed = manager.acquire_pool(&pool, &config).await;
        let elapsed = started.elapsed();
        let remove = client
            .delete(format!("{toxic_base}/{toxic_name}"))
            .send()
            .await
            .unwrap();
        assert!(remove.status().is_success(), "round {round}: {remove:?}");

        let PoolAcquireResult::Unavailable(unavailable) = delayed else {
            panic!("round {round}: delayed acquire response must not be treated as committed");
        };
        assert_eq!(
            unavailable.reason,
            PoolCapacityWaitReason::CoordinatorUnavailable
        );
        assert_eq!(unavailable.detail, "lease_acquire_timeout");
        assert!(
            elapsed >= EXTERNAL_POOL_COORDINATOR_REDIS_OPERATION_TIMEOUT,
            "round {round}: timeout returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(2600),
            "round {round}: timeout was not bounded: {elapsed:?}"
        );

        let drained = manager.drain_release_intents(Duration::from_secs(8)).await;
        assert!(
            drained.drained,
            "round {round}: commit-unknown cleanup must drain"
        );

        let fail_fast = manager.pool_runtime_snapshot(pool.id).await;
        assert!(
            fail_fast
                .as_ref()
                .is_err_and(|err| err.to_string().contains("coordinator breaker is open")),
            "round {round}: requests inside the breaker window must fail fast: {fail_fast:?}"
        );
        let recovered_snapshot = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match manager.pool_runtime_snapshot(pool.id).await {
                    Ok(snapshot) => break snapshot,
                    Err(err) if err.to_string().contains("coordinator breaker is open") => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(err) => panic!("round {round}: unexpected recovery error: {err}"),
                }
            }
        })
        .await
        .expect("coordinator breaker must admit one successful recovery probe");
        assert_eq!(
            (recovered_snapshot.0, recovered_snapshot.1),
            (0, 0),
            "round {round}: pending cleanup must remove any commit-unknown lease"
        );

        for recovery_probe in 0..5 {
            let snapshot = manager.pool_runtime_snapshot(pool.id).await.unwrap();
            assert_eq!(
                (snapshot.0, snapshot.1),
                (0, 0),
                "round {round}, recovery probe {recovery_probe}: timed-out lease must not retain capacity"
            );
            let lease = match manager.acquire_pool(&pool, &config).await {
                PoolAcquireResult::Acquired(lease) => lease,
                PoolAcquireResult::Unavailable(unavailable) => panic!(
                    "round {round}, recovery probe {recovery_probe}: {}",
                    unavailable.detail
                ),
            };
            drop(lease);
            let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
            assert!(
                drained.drained,
                "round {round}, recovery probe {recovery_probe}: release must drain"
            );
        }
        eprintln!(
            "external_pool_commit_unknown round={round} timeout_ms={} recovery=5/5",
            elapsed.as_millis()
        );
    }

    postgres.drop_test_schema().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_pool_error_and_non_stream_bodies_are_bounded_and_recover_for_five_rounds() {
    let client = reqwest::Client::builder().build().unwrap();
    for (case, status, max_bytes) in [
        (
            "error",
            StatusCode::BAD_GATEWAY,
            EXTERNAL_POOL_ERROR_RESPONSE_MAX_BYTES,
        ),
        (
            "non_stream_success",
            StatusCode::OK,
            EXTERNAL_POOL_NON_STREAM_RESPONSE_MAX_BYTES,
        ),
    ] {
        for round in 0..5 {
            let (url, server) = spawn_test_raw_http_response(
                status,
                TestRawHttpBody::DeclaredOnly(max_bytes.saturating_add(1)),
            )
            .await;
            let response = client.get(url).send().await.unwrap();
            let error = response_bytes_with_limit_and_body_timeout(response, 5, max_bytes)
                .await
                .expect_err("declared Content-Length above the external limit must be rejected");
            let error = external_response_body_read_error(error, status, max_bytes, None);
            assert!(!error.err.retryable, "{case} Content-Length round {round}");
            assert!(
                error.err.message.contains("exceeds"),
                "{case} Content-Length round {round}: {}",
                error.err.message
            );
            server.await.unwrap();
            assert_external_bounded_body_recovery(
                &client,
                status,
                max_bytes,
                round,
                &format!("{case} Content-Length"),
            )
            .await;

            let (url, server) = spawn_test_raw_http_response(
                status,
                TestRawHttpBody::Chunked(vec![b'x'; max_bytes.saturating_add(1)]),
            )
            .await;
            let response = client.get(url).send().await.unwrap();
            let error = response_bytes_with_limit_and_body_timeout(response, 5, max_bytes)
                .await
                .expect_err("chunked external body above the limit must be rejected");
            let error = external_response_body_read_error(error, status, max_bytes, None);
            assert!(!error.err.retryable, "{case} chunked round {round}");
            assert!(
                error.err.message.contains("exceeds"),
                "{case} chunked round {round}: {}",
                error.err.message
            );
            server.await.unwrap();
            assert_external_bounded_body_recovery(
                &client,
                status,
                max_bytes,
                round,
                &format!("{case} chunked"),
            )
            .await;

            let (url, server) =
                spawn_test_raw_http_response(status, TestRawHttpBody::StallAfterPrefix).await;
            let response = client.get(url).send().await.unwrap();
            let error = response_bytes_with_limit_and_body_timeout(response, 1, max_bytes)
                .await
                .expect_err("stalled external body must hit the total body timeout");
            let error = external_response_body_read_error(error, status, max_bytes, None);
            assert!(error.err.retryable, "{case} stalled body round {round}");
            assert!(
                error.err.message.contains("timeout"),
                "{case} stalled body round {round}: {}",
                error.err.message
            );
            server.abort();
            let _ = server.await;
            assert_external_bounded_body_recovery(
                &client,
                status,
                max_bytes,
                round,
                &format!("{case} stalled body"),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn external_pool_error_response_masks_raw_error_body_with_trace_id() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        HeaderName::from_static("anthropic-request-id"),
        HeaderValue::from_static("req_upstream"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("999"));
    let body = Bytes::from_static(
        br#"{"error":{"type":"invalid_request_error","message":"bad input"},"type":"error"}"#,
    );

    let err = classify_external_error(
        StatusCode::BAD_REQUEST,
        body.clone(),
        headers,
        &ExternalPoolsConfig::default(),
    );
    let response = external_final_error_from_error(None, Vec::new(), &err, "req_01gatewayerror")
        .into_response("req_gateway");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get(HeaderName::from_static("request-id"))
            .unwrap(),
        "req_gateway"
    );
    assert_eq!(
        response
            .headers()
            .get(HeaderName::from_static("request-id"))
            .unwrap(),
        "req_gateway"
    );
    assert!(
        response
            .headers()
            .get(HeaderName::from_static("x-error-id"))
            .and_then(|value| value.to_str().ok())
            .is_some_and(|error_id| error_id.starts_with("req_01"))
    );
    assert!(response.headers().get(header::CONTENT_LENGTH).is_none());

    let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read external error body");
    let value: serde_json::Value = serde_json::from_slice(&actual).expect("json envelope");
    assert_eq!(value["error"]["type"], "invalid_request_error");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains(envelope::PUBLIC_INVALID_REQUEST_MESSAGE));
    assert!(message.contains("error ID: req_01"));
    assert!(!message.contains("bad input"));
    assert!(!message.contains("invalid_request_error"));
    assert_public_message_hides_internal_routing(message);
    assert_eq!(value["request_id"], "req_gateway");
}

#[test]
fn external_public_error_masks_raw_message() {
    let public_error = external_public_error_from_parts(
        StatusCode::BAD_GATEWAY,
        "server_error",
        true,
        "provider says buy credits at https://example.invalid",
        "req_01public",
    );

    assert_eq!(public_error.status_code, StatusCode::BAD_GATEWAY.as_u16());
    assert_eq!(public_error.error_type, "api_error");
    assert!(
        public_error
            .message
            .contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE)
    );
    assert!(public_error.message.contains("error ID: req_01public"));
    assert!(!public_error.message.contains("buy credits"));
}

#[test]
fn external_public_error_reports_prompt_too_long_without_raw_pool_message() {
    let public_error = external_public_error_from_parts(
        StatusCode::BAD_REQUEST,
        "bad_request",
        false,
        "prompt is too long: > 1000000 maximum; pool banner buy credits",
        "req_01long",
    );

    assert_eq!(public_error.status_code, StatusCode::BAD_REQUEST.as_u16());
    assert_eq!(public_error.error_type, "invalid_request_error");
    assert!(public_error.message.contains("Prompt is too long"));
    assert!(public_error.message.contains("error ID: req_01long"));
    assert!(!public_error.message.contains("1000000 maximum"));
    assert!(!public_error.message.contains("buy credits"));
}

#[tokio::test]
async fn external_pool_retryable_final_error_uses_gateway_error_envelope() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = Bytes::from_static(
        br#"{"error":{"type":"rate_limit_error","message":"slow down"},"type":"error"}"#,
    );

    let err = classify_external_error(
        StatusCode::TOO_MANY_REQUESTS,
        body.clone(),
        headers,
        &ExternalPoolsConfig::default(),
    );
    assert!(err.retryable);
    assert_eq!(error_type_for_external_error(&err), "rate_limit");

    let response = external_final_error_from_error(None, Vec::new(), &err, "req_01gatewayerror")
        .into_response("req_gateway");

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response
            .headers()
            .get(HeaderName::from_static("x-error-id"))
            .and_then(|value| value.to_str().ok())
            .is_some_and(|error_id| error_id.starts_with("req_01"))
    );
    let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read external final retryable body");
    let value: serde_json::Value = serde_json::from_slice(&actual).expect("json envelope");
    assert_eq!(value["error"]["type"], "rate_limit_error");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains(envelope::PUBLIC_RATE_LIMIT_MESSAGE));
    assert!(message.contains("error ID: req_01"));
    assert!(!message.contains("slow down"));
    assert!(!message.contains("rate_limit_error"));
    assert_public_message_hides_internal_routing(message);
    assert_eq!(value["request_id"], "req_gateway");
}

#[test]
fn external_pool_retry_after_header_classifies_rate_limit_as_soft_failure_hint() {
    let mut headers = HeaderMap::new();
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("4"));
    let err = classify_external_error(
        StatusCode::TOO_MANY_REQUESTS,
        Bytes::from_static(
            br#"{"error":{"type":"rate_limit_error","message":"slow down"},"type":"error"}"#,
        ),
        headers,
        &ExternalPoolsConfig::default(),
    );

    assert_eq!(
        err.cooldown,
        Some((Duration::from_secs(4), "rate_limit".to_string()))
    );

    let mut route = test_route("claude-sonnet-4-6");
    route.error_id = "req_01retryafter".to_string();
    let diagnostics = external_error_diagnostics(
        &route,
        &err,
        anthropic_error_type_for_external_error(&err),
        false,
    );
    let metadata = diagnostics.metadata.expect("rate-limit metadata");
    assert_eq!(metadata["softFailureReason"], "rate_limit");
    assert_eq!(
        metadata["softFailureWindowSecs"],
        EXTERNAL_POOL_TRANSIENT_FAILURE_WINDOW_SECS as u64
    );
    assert!(metadata.get("cooldownReason").is_none());
    assert!(metadata.get("cooldownMs").is_none());
}

#[test]
fn external_pool_retry_after_http_date_is_bounded() {
    let retry_at = (Utc::now() + chrono::Duration::days(30)).to_rfc2822();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&retry_at).expect("retry-after date header"),
    );
    let err = classify_external_error(
        StatusCode::SERVICE_UNAVAILABLE,
        Bytes::from_static(br#"{"error":{"message":"temporarily unavailable"}}"#),
        headers,
        &ExternalPoolsConfig::default(),
    );

    let (cooldown, reason) = err.cooldown.expect("server cooldown");
    assert_eq!(reason, "server_error");
    assert_eq!(
        cooldown,
        Duration::from_secs(EXTERNAL_POOL_RETRY_AFTER_MAX_SECS)
    );
}

#[test]
fn external_pool_error_classifies_nested_rate_limit_body() {
    let err = classify_external_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        Bytes::from_static(br#"{"error":"SERVICE_REQUEST_RATE_EXCEEDED: Too many requests"}"#),
        HeaderMap::new(),
        &ExternalPoolsConfig::default(),
    );

    assert!(err.retryable);
    assert_eq!(error_type_for_external_error(&err), "rate_limit");
    assert!(err.auto_disable_reason.is_none());
}

#[test]
fn external_error_diagnostics_records_status_and_non_duplicate_metadata() {
    let mut route = test_route("claude-sonnet-4-6");
    route.error_id = "req_01diagnostic".to_string();
    let err = classify_external_error(
        StatusCode::TOO_MANY_REQUESTS,
        Bytes::from_static(br#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#),
        HeaderMap::new(),
        &ExternalPoolsConfig::default(),
    );
    let response_error_type = anthropic_error_type_for_external_error(&err);
    let (record_message, message_truncated) = external_error_record_message(&err);
    let diagnostics =
        external_error_diagnostics(&route, &err, response_error_type, message_truncated);

    assert_eq!(record_message, err.message);
    assert_eq!(diagnostics.status_code, Some(429));
    assert_eq!(diagnostics.source.as_deref(), Some("external_account"));
    assert_eq!(diagnostics.error_id.as_deref(), Some("req_01diagnostic"));
    let metadata = diagnostics.metadata.unwrap();
    assert_eq!(metadata["responseErrorType"], "rate_limit_error");
    assert_eq!(metadata["retryable"], true);
    assert_eq!(metadata["softFailureReason"], "rate_limit");
    assert!(metadata.get("cooldownReason").is_none());
    for duplicate_key in [
        "message",
        "rawMessage",
        "attempts",
        "poolId",
        "poolName",
        "requestId",
        "errorId",
        "statusCode",
    ] {
        assert!(
            metadata.get(duplicate_key).is_none(),
            "metadata duplicated {duplicate_key}: {metadata}"
        );
    }
}

#[test]
fn external_failure_standard_usage_fields_are_zeroed_for_all_non_success_statuses() {
    let huge_usage = ExternalPoolUsageSnapshot {
        total_input_tokens: 2_648_439,
        input_tokens: 1_100_000,
        billable_input_tokens: 1_100_000,
        output_tokens: 77,
        cache_creation_input_tokens: 1_300_180,
        cache_read_input_tokens: 1_200_000,
        cache_creation_5m_input_tokens: 700_000,
        cache_creation_1h_input_tokens: 600_180,
    };

    for status in [
        UsageRecordStatus::Error,
        UsageRecordStatus::StreamError,
        UsageRecordStatus::UpstreamTimeout,
        UsageRecordStatus::ClientDropped,
    ] {
        let standard_usage =
            external_standard_usage_for_status(status, 2_648_439, Some(huge_usage));
        assert_eq!(standard_usage.total_input_tokens, 0, "status={status:?}");
        assert_eq!(standard_usage.input_tokens, 0, "status={status:?}");
        assert_eq!(standard_usage.billable_input_tokens, 0, "status={status:?}");
        assert_eq!(standard_usage.output_tokens, 0, "status={status:?}");
        assert_eq!(
            standard_usage.cache_creation_input_tokens, 0,
            "status={status:?}"
        );
        assert_eq!(
            standard_usage.cache_read_input_tokens, 0,
            "status={status:?}"
        );
    }

    let success_missing_usage =
        external_standard_usage_for_status(UsageRecordStatus::Success, 2_648_439, None);
    assert_eq!(success_missing_usage.total_input_tokens, 2_648_439);
    assert_eq!(success_missing_usage.input_tokens, 2_648_439);
    assert_eq!(success_missing_usage.billable_input_tokens, 2_648_439);
    assert_eq!(success_missing_usage.output_tokens, 0);

    let success_reported_usage =
        external_standard_usage_for_status(UsageRecordStatus::Success, 2_648_439, Some(huge_usage));
    assert_eq!(success_reported_usage, huge_usage);
}

#[test]
fn external_error_classification_attempt_usage_and_final_error_never_retain_raw_bodies() {
    for round in 0..5 {
        for (status, body) in [
            (
                StatusCode::BAD_REQUEST,
                format!(
                    r#"{{"error":{{"message":"prompt is too long PRIVATE_EXTERNAL_{round}_400"}}}}"#
                ),
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                format!(r#"{{"error":"PRIVATE_EXTERNAL_{round}_429"}}"#),
            ),
            (
                StatusCode::FORBIDDEN,
                format!(r#"{{"error":"PRIVATE_EXTERNAL_{round}_403"}}"#),
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(r#"{{"error":"PRIVATE_EXTERNAL_{round}_500"}}"#),
            ),
        ] {
            let marker = format!("PRIVATE_EXTERNAL_{round}_{}", status.as_u16());
            let err = classify_external_error(
                status,
                Bytes::from(body),
                HeaderMap::new(),
                &ExternalPoolsConfig::default(),
            );
            let (record_message, truncated) = external_error_record_message(&err);
            let route = test_route("claude-sonnet-4-6");
            let diagnostics = external_error_diagnostics(
                &route,
                &err,
                anthropic_error_type_for_external_error(&err),
                truncated,
            );
            let final_error = external_final_error_from_error(
                None,
                vec![ExternalPoolAttempt {
                    attempt: 1,
                    pool_id: 7,
                    pool_name: "pool-a".to_string(),
                    outbound_model: Some("model-a".to_string()),
                    status: Some(status.as_u16()),
                    action: "fail".to_string(),
                    duration_ms: 1,
                    error_type: Some(error_type_for_external_error(&err)),
                    error_message: Some(err.message.clone()),
                }],
                &err,
                "req_01safe",
            );
            let retained = format!("{err:?} {record_message} {diagnostics:?} {final_error:?}");
            assert!(
                !retained.contains(&marker),
                "retained raw marker: {retained}"
            );
        }
    }
}

#[test]
fn external_pool_error_classifies_database_busy_without_auto_disable() {
    let err = classify_external_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        Bytes::from_static(br#"database is locked (SQLITE_BUSY)"#),
        HeaderMap::new(),
        &ExternalPoolsConfig::default(),
    );

    assert!(err.retryable);
    assert_eq!(error_type_for_external_error(&err), "database_busy");
    assert!(err.auto_disable_reason.is_none());
}

#[test]
fn external_pool_error_classifies_channel_disabled_for_optional_auto_disable() {
    let config = ExternalPoolsConfig::default();
    let err = classify_external_error(
        StatusCode::BAD_GATEWAY,
        Bytes::from_static(br#"channel affinity has been disabled"#),
        HeaderMap::new(),
        &config,
    );

    assert!(err.retryable);
    assert_eq!(err.auto_disable_reason.as_deref(), Some("channel_disabled"));
    assert_eq!(error_type_for_external_error(&err), "channel_disabled");
    assert!(auto_disable_reason_enabled(&config, "channel_disabled"));
}

#[test]
fn external_pool_error_classifies_model_unavailable_as_retryable() {
    let err = classify_external_error(
        StatusCode::BAD_REQUEST,
        Bytes::from_static(br#"{"error":{"code":"model_not_found"}}"#),
        HeaderMap::new(),
        &ExternalPoolsConfig::default(),
    );

    assert!(err.retryable);
    assert_eq!(error_type_for_external_error(&err), "model_unavailable");
    assert!(err.auto_disable_reason.is_none());
    assert_eq!(
        err.cooldown.as_ref().map(|(_, reason)| reason.as_str()),
        Some("model_unavailable")
    );
}

#[test]
fn external_pool_error_classifies_model_unavailable_without_cooldown_when_disabled() {
    let config = ExternalPoolsConfig {
        external_pool_model_unavailable_cooldown_mode:
            ExternalPoolModelUnavailableCooldownMode::Disabled,
        ..ExternalPoolsConfig::default()
    };
    let err = classify_external_error(
        StatusCode::BAD_REQUEST,
        Bytes::from_static(br#"{"error":{"message":"No available channel for model x"}}"#),
        HeaderMap::new(),
        &config,
    );

    assert!(err.retryable);
    assert_eq!(error_type_for_external_error(&err), "model_unavailable");
    assert!(err.cooldown.is_none());
}

#[test]
fn external_payload_guard_retry_route_trims_and_disables_second_retry() {
    for round in 1..=5 {
        let mut route = test_route("claude-sonnet-4-6");
        let mut messages = Vec::new();
        for idx in 0..32 {
            messages.push(Message {
                role: "user".to_string(),
                content: serde_json::json!(format!(
                    "round {round} history {} {}",
                    idx,
                    "x".repeat(700)
                )),
            });
            messages.push(Message {
                role: "assistant".to_string(),
                content: serde_json::json!([{
                    "type": "text",
                    "text": format!("round {round} answer {} {}", idx, "y".repeat(500)),
                }]),
            });
        }
        messages.push(Message {
            role: "user".to_string(),
            content: serde_json::json!(format!("current question round {round}")),
        });
        route.payload.as_mut().unwrap().messages = messages;
        refresh_test_route_derived_state(&mut route);
        let original_input_tokens = route.request_input_tokens;
        let body = serde_json::to_string(route.payload.as_ref().unwrap())
            .expect("serialize route payload");
        route.raw_body = Bytes::from(body);
        route.payload_guard_retry_config = Some(PayloadGuardConfig {
            enabled: true,
            max_bytes: 8_000,
            trim_history: true,
            shaping: crate::model::config::PayloadShapingConfig::default(),
        });
        let err = classify_external_error(
            StatusCode::BAD_REQUEST,
            Bytes::from_static(br#"{"error":{"message":"Context window is full"}}"#),
            HeaderMap::new(),
            &ExternalPoolsConfig::default(),
        );

        assert!(should_retry_external_payload_guard(&route, &err));
        let retry_route = external_payload_guard_retry_route(&route).expect("retry route");

        assert_eq!(
            retry_route.body_mode_filter,
            Some(ExternalPoolRequestBodyMode::Normalized),
            "round {round}"
        );
        assert!(retry_route.raw_body.len() <= 8_000, "round {round}");
        assert!(
            retry_route.payload_guard_retry_config.is_none(),
            "round {round}"
        );
        assert!(
            retry_route
                .payload_guard_report
                .as_ref()
                .is_some_and(|report| report.trimmed_history_entries > 0),
            "round {round}"
        );
        assert_eq!(
            retry_route
                .payload
                .as_ref()
                .unwrap()
                .messages
                .last()
                .unwrap()
                .content,
            serde_json::json!(format!("current question round {round}"))
        );

        let retry_payload = retry_route.payload.as_ref().expect("retry payload");
        let retry_input_tokens = count_external_route_input_tokens(retry_payload);
        assert_eq!(
            retry_route.request_input_tokens, retry_input_tokens,
            "round {round}: retry route token estimate must match its trimmed payload"
        );
        assert!(
            retry_input_tokens < original_input_tokens,
            "round {round}: retry tokens {retry_input_tokens} must be below original {original_input_tokens}"
        );
        let body_payload: MessagesRequest = serde_json::from_slice(&retry_route.raw_body)
            .expect("retry body remains a Messages request");
        assert_eq!(
            serde_json::to_value(body_payload).unwrap(),
            serde_json::to_value(retry_payload).unwrap(),
            "round {round}: retry raw body and typed payload must remain identical"
        );

        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&retry_route, &pool, 0)
            .unwrap_or_else(|| panic!("round {round}: retry usage projection"));
        assert_eq!(
            projection.raw_input_tokens, retry_input_tokens,
            "round {round}: usage projection must use the trimmed request estimate"
        );
        assert!(
            projection.prompt_cache_profile.is_some(),
            "round {round}: retry payload should build a prompt-cache profile"
        );
        let projected = maybe_project_non_stream_usage(
            Bytes::from_static(
                br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
            ),
            Some(&projection),
        );
        assert_eq!(
            projected.usage_capture.request_input_tokens,
            Some(retry_input_tokens),
            "round {round}: projected usage must carry the trimmed request estimate"
        );
    }
}

#[tokio::test]
async fn external_capacity_scheduler_error_uses_request_id_and_error_type() {
    let route = ExternalRouteRequest {
        effective_raw_body: Bytes::new(),
        effective_raw_probe: None,
        preparation_cache: Arc::new(ExternalRouteRequestPreparationCache::default()),
        raw_body: Bytes::new(),
        headers: HeaderMap::new(),
        endpoint: "/v1/messages".to_string(),
        payload: Some(MessagesRequest {
            model: "claude-sonnet-4-5-20250929".to_string(),
            max_tokens: 8,
            messages: Vec::new(),
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }),
        body_mode_filter: Some(ExternalPoolRequestBodyMode::Normalized),
        model_hint: None,
        stream_hint: None,
        request_input_tokens: 1,
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        route_subtype: UsageRouteSubtype::ExternalDirectPolicy,
        fallback_reason: None,
        direct_policy_reason: None,
        local_attempted: false,
        local_preflight: None,
        local_attempts: Vec::new(),
        reported_usage: ReportedUsageConfig::default(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_route_namespace: None,
        prompt_cache_target_read_ratio: 0.98,
        prompt_cache_token_scale: 1.6,
        prompt_cache_max_simulated_input_tokens: 300_000,
        prompt_cache_cap_jitter_min_tokens: 12_000,
        prompt_cache_cap_jitter_max_tokens: 24_000,
        prompt_cache_scale_min_input_tokens: 20_000,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        kiro_rs_tool_cache_policy: KiroRsToolCachePolicy::default(),
        model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_external_capacity".to_string(),
        error_id: "req_01capacity".to_string(),
        recorder: Arc::new(crate::anthropic::usage::UsageRecorder::new(1)),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        latency_trace: Arc::new(ExternalLatencyTraceState::default()),
        payload_breakdown: None,
        payload_guard_report: None,
        payload_guard_external_enabled: true,
        payload_guard_initial_config: PayloadGuardConfig {
            enabled: true,
            max_bytes: 0,
            trim_history: false,
            shaping: crate::model::config::PayloadShapingConfig::default(),
        },
        payload_guard_retry_config: None,
        inference_attempt_budget: Arc::new(
            crate::anthropic::inference_attempt_budget::InferenceAttemptBudget::new(4),
        ),
        request_api_key_id: None,
    };

    let (error_type, message) = external_capacity_error(PoolCapacityWaitReason::Full);
    let err = external_capacity_final_error(
        StatusCode::SERVICE_UNAVAILABLE,
        error_type,
        message,
        &route.error_id,
    );
    assert!(err.retryable);
    assert!(err.is_capacity_like());
    assert_eq!(err.route_error_type, "external_pool_capacity_full");
    assert!(should_defer_synthetic_capacity_error_to_last_pool_error(
        &err
    ));

    let wait_timeout = external_capacity_final_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "external_pool_wait_timeout",
        "Request capacity wait timed out after 1 seconds",
        &route.error_id,
    );
    assert!(wait_timeout.retryable);
    assert!(wait_timeout.is_capacity_like());
    assert!(should_defer_synthetic_capacity_error_to_last_pool_error(
        &wait_timeout
    ));

    let deadline = external_capacity_final_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "external_pool_deadline_exceeded",
        "Request dispatch deadline exceeded",
        &route.error_id,
    );
    assert!(!deadline.retryable);
    assert!(!should_defer_synthetic_capacity_error_to_last_pool_error(
        &deadline
    ));
    let response = err.into_response(&route.request_id);

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers().get("request-id").unwrap(),
        "req_external_capacity"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read scheduler error body");
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["type"], "api_error");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE));
    assert!(message.contains("error ID: req_01"));
    assert!(!message.contains("Request capacity is full"));
    assert_public_message_hides_internal_routing(message);
}

#[test]
fn external_coordinator_failure_is_not_classified_as_capacity_or_queue_full() {
    let (error_type, message) =
        external_capacity_error(PoolCapacityWaitReason::CoordinatorUnavailable);

    assert_eq!(error_type, "external_pool_coordinator_unavailable");
    assert!(!error_type.contains("full"));
    assert!(message.contains("coordinator"));
}

#[test]
fn degraded_fallback_local_lease_is_limited_to_scheduler_degraded_fallback_route() {
    let mut direct_route = test_route("claude-sonnet-4-5");
    direct_route.fallback_reason = Some("local_scheduler_redis_degraded".to_string());
    assert!(
        !route_allows_degraded_fallback_local_lease(&direct_route),
        "direct external route must not bypass external coordinator"
    );

    let mut fallback_route = test_route("claude-sonnet-4-5");
    fallback_route.route_subtype = UsageRouteSubtype::ExternalFallbackAfterLocalAttempts;
    fallback_route.fallback_reason = Some("local_scheduler_redis_degraded".to_string());
    assert!(route_allows_degraded_fallback_local_lease(&fallback_route));

    let mut preflight_route = test_route("claude-sonnet-4-5");
    preflight_route.route_subtype = UsageRouteSubtype::ExternalFallbackPreflight;
    preflight_route.fallback_reason = Some("local_scheduler_redis_degraded".to_string());
    assert!(route_allows_degraded_fallback_local_lease(&preflight_route));

    fallback_route.fallback_reason = Some("local_capacity_exhausted".to_string());
    assert!(
        !route_allows_degraded_fallback_local_lease(&fallback_route),
        "ordinary capacity fallback must still use the external coordinator"
    );
}

#[test]
fn successful_external_html_response_is_protocol_error() {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
    let body = Bytes::from_static(br#"<!doctype html><html><body>admin</body></html>"#);

    assert!(success_response_looks_like_html(&headers, &body));
    let err = success_protocol_error(
        &headers,
        Some(&body),
        &ExternalPoolsConfig::default(),
        "model endpoint returned an HTML response",
    );

    assert!(err.retryable);
    assert_eq!(err.status, Some(StatusCode::OK));
    assert_eq!(
        err.auto_disable_reason.as_deref(),
        Some("misconfigured_endpoint")
    );
    assert_eq!(
        error_type_for_external_error(&err),
        "misconfigured_endpoint"
    );
}

#[test]
fn successful_external_error_body_is_treated_as_protocol_error() {
    let body = Bytes::from_static(
        br#"{"type":"error","error":{"type":"api_error","message":"raw pool failure"}}"#,
    );

    assert!(success_response_looks_like_error_body(&body));
    let err = success_error_body_protocol_error(&body, &ExternalPoolsConfig::default());

    assert!(err.retryable);
    assert_eq!(err.status, Some(StatusCode::OK));
    assert_eq!(err.protocol_error, Some("success_error_envelope"));
    assert!(!format!("{err:?}").contains("raw pool failure"));
    assert!(err.message.contains("success status"));
}

#[test]
fn external_stream_error_event_is_masked_and_only_safe_classification_is_recorded() {
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let mask = ExternalStreamErrorMask {
        request_id: "req_stream_mask".to_string(),
        error_id: "req_01streammask".to_string(),
        pool_id: 7,
        pool_name: "pool-a".to_string(),
    };
    let event = br#"event: error
data: {"type":"error","error":{"type":"api_error","message":"raw external promo text"}}

"#;

    let masked = process_sse_event_with_plan(
        event,
        None,
        Some(&capture),
        Some(&mask),
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );
    let text = std::str::from_utf8(&masked).expect("masked event utf8");

    assert!(text.contains("event: error"));
    assert!(text.contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE));
    assert!(text.contains("error ID: req_01streammask"));
    assert!(text.contains("req_stream_mask"));
    assert!(!text.contains("raw external promo text"));
    assert!(!text.contains("pool-a"));
    assert_public_message_hides_internal_routing(text);

    let recorded = capture
        .lock()
        .stream_error_message
        .clone()
        .expect("safe stream error classification recorded");
    assert_eq!(recorded, "external upstream emitted an error event");
    assert!(!recorded.contains("raw external promo text"));
    assert!(!recorded.contains("req_01streammask"));
}

fn assert_public_message_hides_internal_routing(message: &str) {
    let lower = message.to_ascii_lowercase();
    for forbidden in [
        "credential",
        "external pool",
        "external_pool",
        "fallback",
        "preflight",
        "备用池",
        "外部池",
        "凭据",
    ] {
        assert!(
            !lower.contains(forbidden),
            "public message leaked internal term {forbidden:?}: {message}"
        );
    }
}

#[test]
fn external_latency_trace_records_stream_markers_without_changing_first_output_semantics() {
    let trace = ExternalLatencyTraceState::default();
    let started_at = Instant::now() - Duration::from_millis(25);

    trace.mark_upstream_header(started_at);
    trace.mark_first_upstream_chunk(started_at);

    let text_start = Bytes::from_static(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"text\"}}\n\n",
        );
    assert!(!external_stream_chunk_has_first_output(&text_start));

    let output = Bytes::from_static(
            b"event: ping\ndata: {}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
        );
    assert!(external_stream_chunk_has_first_output(&output));
    assert_eq!(count_external_stream_events_before_first_output(&output), 1);

    trace.mark_first_output(50, 1, 2);
    let snapshot = trace.snapshot().expect("latency trace snapshot");
    assert!(snapshot.upstream_header_ms.is_some());
    assert!(snapshot.first_upstream_chunk_ms.is_some());
    assert_eq!(snapshot.first_output_delta_ms, Some(50));
    assert_eq!(snapshot.chunks_before_first_output, Some(1));
    assert_eq!(snapshot.events_before_first_output, Some(2));
    assert!(snapshot.stream_gap_to_first_output_ms.is_some());
}

#[test]
fn external_usage_trace_preserves_local_auxiliary_attempts_for_five_rounds() {
    use crate::anthropic::inference_attempt_budget::AuxiliaryAttemptKind;

    for round in 1..=5 {
        let route = test_route("claude-sonnet-4-6");
        route
            .inference_attempt_budget
            .auxiliary_budget()
            .reserve(AuxiliaryAttemptKind::TokenRefresh)
            .expect("token refresh auxiliary attempt");
        route
            .inference_attempt_budget
            .auxiliary_budget()
            .reserve(AuxiliaryAttemptKind::ProfileDiscovery)
            .expect("profile discovery auxiliary attempt");
        route
            .inference_attempt_budget
            .reserve(InferenceAttemptKind::ExternalPool, 0)
            .expect("external inference attempt");

        let trace = external_usage_latency_trace(&route);
        let inference = trace.inference_attempts.expect("inference snapshot");
        assert_eq!(inference.consumed, 1, "round {round}");
        assert_eq!(inference.external_attempts, 1, "round {round}");
        let auxiliary = trace.auxiliary_attempts.expect("auxiliary snapshot");
        assert_eq!(auxiliary.consumed, 2, "round {round}");
        assert_eq!(auxiliary.token_refresh_attempts, 1, "round {round}");
        assert_eq!(auxiliary.profile_discovery_attempts, 1, "round {round}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_fallback_usage_matches_real_refresh_profile_and_inference_hits_for_five_rounds() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let fake = AuxiliaryFallbackFakeServer::start().await;
    let mut pool_request = create_pool_request("external-auxiliary-attribution", 1, true);
    pool_request.base_url = fake.base_url.clone();
    pool_request.max_concurrent_requests = 4;
    let pool = postgres
        .create_external_pool(pool_request)
        .await
        .expect("create auxiliary attribution external pool");
    let external_config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_global_max_concurrent_requests: 4,
        external_pool_retry_max_attempts: 1,
        ..ExternalPoolsConfig::default()
    };
    let local_request = serde_json::json!({
        "conversationState": {
            "conversationId": "auxiliary-fallback-test",
            "currentMessage": {
                "userInputMessage": {
                    "content": "test",
                    "modelId": "claude-sonnet-4"
                }
            }
        }
    })
    .to_string();

    for (case, expired_for_refresh) in [("refresh", true), ("profile", false)] {
        for round in 1..=5 {
            let hits_before = fake.snapshot();
            let budget = Arc::new(InferenceAttemptBudget::new(4));
            let provider = auxiliary_fallback_local_provider(&fake.base_url, expired_for_refresh);
            let local_result = provider
                .call_api_with_context_with_request_id_and_attempt_budget_max_sends(
                    &local_request,
                    Some(&format!("req_local_{case}_{round}")),
                    AcquireMode::FailFastOnCapacity,
                    1,
                    Some("claude-sonnet-4"),
                    budget.clone(),
                    true,
                    Some(1),
                )
                .await;
            if local_result.is_ok() {
                panic!("case={case} round={round}: controlled local path unexpectedly succeeded");
            }

            let hits_after_local = fake.snapshot();
            let local_delta = (
                hits_after_local.0 - hits_before.0,
                hits_after_local.1 - hits_before.1,
                hits_after_local.2 - hits_before.2,
            );
            let expected_local_delta = if expired_for_refresh {
                (1, 0, 0)
            } else {
                (0, 1, 1)
            };
            assert_eq!(
                local_delta, expected_local_delta,
                "case={case} round={round}"
            );

            let recorder = Arc::new(crate::anthropic::usage::UsageRecorder::new(4));
            let request_id = format!("req_external_{case}_{round}");
            let mut route = test_route("claude-sonnet-4-6");
            route.request_id = request_id.clone();
            route.error_id = format!("err_external_{case}_{round}");
            route.recorder = recorder.clone();
            route.inference_attempt_budget = budget.clone();
            route.local_attempted = true;
            route.route_subtype = UsageRouteSubtype::ExternalFallbackAfterLocalAttempts;
            route.fallback_reason = Some(format!("local_{case}_failure"));

            let response = match manager
                .forward_with_failover_result(external_config.clone(), route)
                .await
            {
                ExternalPoolForwardOutcome::Response(response) => response,
                ExternalPoolForwardOutcome::FinalError(error) => {
                    panic!(
                        "case={case} round={round}: external fallback failed: {}",
                        error.message
                    )
                }
            };
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "case={case} round={round}"
            );
            let response_body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("read external fallback body");
            assert!(
                response_body
                    .windows(b"external-ok".len())
                    .any(|window| window == b"external-ok"),
                "case={case} round={round}"
            );

            let hits_after_external = fake.snapshot();
            assert_eq!(
                hits_after_external.3 - hits_after_local.3,
                1,
                "case={case} round={round}"
            );
            let result = recorder.query(UsageRecordQuery {
                request_id: Some(request_id.clone()),
                ..UsageRecordQuery::default()
            });
            assert_eq!(result.records.len(), 1, "case={case} round={round}");
            let record = &result.records[0];
            assert_eq!(record.status, UsageRecordStatus::Success);
            assert_eq!(record.external_pool_id, Some(pool.id));
            let trace = record
                .latency_trace
                .as_ref()
                .expect("external fallback usage latency trace");
            let inference = trace
                .inference_attempts
                .expect("external fallback inference attempts");
            let auxiliary = trace
                .auxiliary_attempts
                .expect("external fallback auxiliary attempts");
            assert_eq!(
                inference.consumed as u64,
                hits_after_external
                    .2
                    .saturating_sub(hits_before.2)
                    .saturating_add(hits_after_external.3.saturating_sub(hits_before.3)),
                "case={case} round={round}"
            );
            assert_eq!(inference.external_attempts, 1, "case={case} round={round}");
            assert_eq!(
                auxiliary.token_refresh_attempts as u64,
                hits_after_external.0 - hits_before.0,
                "case={case} round={round}"
            );
            assert_eq!(
                auxiliary.profile_discovery_attempts as u64,
                hits_after_external.1 - hits_before.1,
                "case={case} round={round}"
            );
            assert_eq!(
                auxiliary.consumed,
                auxiliary.token_refresh_attempts + auxiliary.profile_discovery_attempts,
                "case={case} round={round}"
            );
        }
    }

    let drained = manager.drain_release_intents(Duration::from_secs(5)).await;
    assert!(drained.drained, "external release intents must drain");
    postgres.drop_test_schema().await.unwrap();
}

#[test]
fn external_first_output_parser_uses_sse_json_semantics() {
    let empty_delta = Bytes::from_static(
        br#"event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":""}}

"#,
    );
    assert!(!external_stream_chunk_has_first_output(&empty_delta));

    let text_then_tool_start = Bytes::from_static(
            br#"event: content_block_start
data: {"type":"content_block_start","content_block":{"type":"text"}}

event: content_block_start
data: {"type":"content_block_start","content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}

"#,
        );
    assert!(external_stream_chunk_has_first_output(
        &text_then_tool_start
    ));
    assert_eq!(
        count_external_stream_events_before_first_output(&text_then_tool_start),
        1
    );

    let content_in_payload_string = Bytes::from_static(
        br#"event: message_delta
data: {"type":"message_delta","note":"content_block_delta"}

"#,
    );
    assert!(!external_stream_chunk_has_first_output(
        &content_in_payload_string
    ));
}

fn test_pool(base_url: &str, preserve_path: bool) -> ExternalPool {
    let now = Utc::now();
    ExternalPool {
        id: 1,
        revision: 1,
        name: "test".to_string(),
        base_url: base_url.to_string(),
        api_key: Some("sk-test".to_string()),
        masked_api_key: None,
        auth_type: ExternalPoolAuthType::Bearer,
        enabled: true,
        priority: 10,
        max_concurrent_requests: 10,
        usage_projection_mode: ExternalPoolUsageProjectionMode::PassThrough,
        stream_response_mode: None,
        request_body_mode: ExternalPoolRequestBodyMode::Normalized,
        raw_model_mode: ExternalPoolRawModelMode::None,
        auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
        pre_output_stream_retry_mode: ExternalPoolStreamRetryMode::Inherit,
        auto_disabled: false,
        auto_disabled_reason: None,
        auto_disabled_at: None,
        auto_disabled_until: None,
        auto_disabled_last_error: None,
        preserve_path,
        normalize_model_version_dots: false,
        model_mapping_mode: ExternalPoolModelMappingMode::ProcessedMapping,
        model_mapping_require_match: false,
        model_mapping_rules: Vec::new(),
        supported_models: Vec::new(),
        route_mode: ExternalPoolRouteMode::AllowAll,
        route_rules: Vec::new(),
        notes: None,
        created_at: now,
        updated_at: now,
    }
}

fn model_rule(source: &str, target: &str) -> ModelMappingRule {
    ModelMappingRule {
        enabled: true,
        source: source.to_string(),
        target: target.to_string(),
        kind: Default::default(),
        note: None,
    }
}

fn test_pool_with_model_dot_normalization() -> ExternalPool {
    let mut pool = test_pool("https://example.com/v1", true);
    pool.normalize_model_version_dots = true;
    pool
}

#[test]
fn supported_model_filter_allows_empty_and_matches_route_candidates() {
    let mut pool = test_pool("https://example.com/v1", true);
    let route = test_route("claude-sonnet-4.5");

    assert!(external_pool_matches_supported_models(
        &pool,
        Some(&route.model_candidates_for_support())
    ));

    pool.supported_models = vec!["claude-haiku-4.5".to_string()];
    assert!(!external_pool_matches_supported_models(
        &pool,
        Some(&route.model_candidates_for_support())
    ));

    pool.supported_models = vec!["claude-sonnet-4.5".to_string()];
    assert!(external_pool_matches_supported_models(
        &pool,
        Some(&route.model_candidates_for_support())
    ));
}

#[test]
fn supported_model_filter_uses_original_payload_and_raw_model_candidates() {
    let mut pool = test_pool("https://example.com/v1", true);
    let mut route = test_route("client-alias");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());

    pool.supported_models = vec!["claude-sonnet-4.5".to_string()];
    assert!(!external_pool_matches_supported_models(
        &pool,
        Some(&route.model_candidates_for_support())
    ));

    pool.supported_models = vec!["client-alias".to_string()];
    assert!(external_pool_matches_supported_models(
        &pool,
        Some(&route.model_candidates_for_support())
    ));

    let raw_route = raw_test_route(
            br#"{"model":"raw-client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
        );
    pool.supported_models = vec!["raw-client-model".to_string()];
    assert!(external_pool_matches_supported_models(
        &pool,
        Some(&raw_route.model_candidates_for_support())
    ));

    pool.supported_models = vec!["other-model".to_string()];
    assert!(!external_pool_matches_supported_models(
        &pool,
        Some(&raw_route.model_candidates_for_support())
    ));
}

#[tokio::test]
async fn external_pool_max_input_tokens_does_not_short_circuit_dispatch() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let fake = AuxiliaryFallbackFakeServer::start().await;
    let mut pool_request = create_pool_request("external-no-input-preflight", 1, true);
    pool_request.base_url = fake.base_url.clone();
    postgres.create_external_pool(pool_request).await.unwrap();
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_max_input_tokens: 1,
        external_pool_retry_max_attempts: 0,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4-6");
    route.request_input_tokens = 1_500_000;
    let hits_before = fake.snapshot().3;

    let response = match manager.forward_with_failover_result(config, route).await {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!(
                "external max-input compatibility field must not reject before dispatch: {}",
                error.message
            )
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fake.snapshot().3, hits_before + 1);
    postgres.drop_test_schema().await.unwrap();
}

#[test]
fn external_pool_max_input_tokens_has_no_dispatch_preflight_stage() {
    let source = include_str!("../external_pool.rs");

    assert!(
        !source.contains("external_prompt_too_long_preflight"),
        "external pool must not synthesize a prompt-too-long preflight stage"
    );
    assert!(
        !source.contains("external_pool_max_input_tokens_for_route"),
        "externalPoolMaxInputTokens is a compatibility field, not a dispatch preflight gate"
    );
}

#[test]
fn supported_model_filter_empty_list_allows_future_model_without_fallback() {
    let mut pool = test_pool("https://example.com/v1", true);
    let mut route = test_route("claude-sonnet-5");
    route.upstream_model = Some("claude-sonnet-4.6".to_string());

    pool.supported_models = vec!["claude-sonnet-4.6".to_string()];
    assert!(!external_pool_matches_supported_models(
        &pool,
        Some(&route.model_candidates_for_support())
    ));

    pool.supported_models = Vec::new();
    assert!(external_pool_matches_supported_models(
        &pool,
        Some(&route.model_candidates_for_support())
    ));
}

#[test]
fn supported_model_filter_requires_a_candidate_when_list_is_restricted() {
    let mut pool = test_pool("https://example.com/v1", true);
    pool.supported_models = vec!["claude-sonnet-4.5".to_string()];

    assert!(!external_pool_matches_supported_models(&pool, None));
    assert!(!external_pool_matches_supported_models(
        &pool,
        Some(&[None, None, None])
    ));
}

#[test]
fn forward_headers_adds_default_anthropic_version_for_external_auth_modes() {
    let headers = HeaderMap::new();
    let mut bearer_pool = test_pool("https://example.com/v1", true);
    bearer_pool.auth_type = ExternalPoolAuthType::Bearer;
    bearer_pool.api_key = Some("sk-bearer".to_string());

    let bearer = forward_headers(&headers, &bearer_pool).expect("bearer headers");
    assert_eq!(
        bearer
            .get(HeaderName::from_static("anthropic-version"))
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );
    assert_eq!(
        bearer
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer sk-bearer")
    );
    assert!(bearer.get(HeaderName::from_static("x-api-key")).is_none());

    let mut x_api_key_pool = bearer_pool;
    x_api_key_pool.auth_type = ExternalPoolAuthType::XApiKey;
    x_api_key_pool.api_key = Some("sk-x-api-key".to_string());
    let x_api_key = forward_headers(&headers, &x_api_key_pool).expect("x-api-key headers");
    assert_eq!(
        x_api_key
            .get(HeaderName::from_static("anthropic-version"))
            .and_then(|value| value.to_str().ok()),
        Some("2023-06-01")
    );
    assert_eq!(
        x_api_key
            .get(HeaderName::from_static("x-api-key"))
            .and_then(|value| value.to_str().ok()),
        Some("sk-x-api-key")
    );
    assert!(x_api_key.get(header::AUTHORIZATION).is_none());
}

#[test]
fn forward_headers_preserves_client_anthropic_version() {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2024-02-29"),
    );
    let pool = test_pool("https://example.com/v1", true);

    let forwarded = forward_headers(&headers, &pool).expect("headers");
    assert_eq!(
        forwarded
            .get(HeaderName::from_static("anthropic-version"))
            .and_then(|value| value.to_str().ok()),
        Some("2024-02-29")
    );
}

fn test_external_pool_outbound_body(route: &ExternalRouteRequest, pool: &ExternalPool) -> Bytes {
    external_pool_outbound_body(route, pool).expect("build external outbound body")
}

fn payload_ref(route: &ExternalRouteRequest) -> &MessagesRequest {
    route.payload.as_ref().expect("typed test route payload")
}

fn payload_mut(route: &mut ExternalRouteRequest) -> &mut MessagesRequest {
    route.payload.as_mut().expect("typed test route payload")
}

fn refresh_test_route_derived_state(route: &mut ExternalRouteRequest) {
    let request_input_tokens = count_external_route_input_tokens(payload_ref(route));
    route.request_input_tokens = request_input_tokens;
    route.reset_preparation_cache();
}

fn test_route(model: &str) -> ExternalRouteRequest {
    let payload = test_payload(model);
    let request_input_tokens = count_external_route_input_tokens(&payload);
    ExternalRouteRequest {
        effective_raw_body: Bytes::new(),
        effective_raw_probe: None,
        preparation_cache: Arc::new(ExternalRouteRequestPreparationCache::default()),
        raw_body: Bytes::new(),
        headers: HeaderMap::new(),
        endpoint: "/cc/v1/messages".to_string(),
        payload: Some(payload),
        body_mode_filter: Some(ExternalPoolRequestBodyMode::Normalized),
        model_hint: None,
        stream_hint: None,
        request_input_tokens,
        upstream_model: Some(model.to_string()),
        model_resolution_source: Some("exact_upstream".to_string()),
        model_resolution_note: None,
        route_subtype: UsageRouteSubtype::ExternalDirectPolicy,
        fallback_reason: None,
        direct_policy_reason: None,
        local_attempted: false,
        local_preflight: None,
        local_attempts: Vec::new(),
        reported_usage: ReportedUsageConfig::default(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_route_namespace: None,
        prompt_cache_target_read_ratio: 0.98,
        prompt_cache_token_scale: 1.6,
        prompt_cache_max_simulated_input_tokens: 300_000,
        prompt_cache_cap_jitter_min_tokens: 12_000,
        prompt_cache_cap_jitter_max_tokens: 24_000,
        prompt_cache_scale_min_input_tokens: 20_000,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        kiro_rs_tool_cache_policy: KiroRsToolCachePolicy::default(),
        model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_external_billing".to_string(),
        error_id: "req_error_external_billing".to_string(),
        recorder: Arc::new(crate::anthropic::usage::UsageRecorder::new(1)),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        latency_trace: Arc::new(ExternalLatencyTraceState::default()),
        payload_breakdown: None,
        payload_guard_report: None,
        payload_guard_external_enabled: true,
        payload_guard_initial_config: PayloadGuardConfig {
            enabled: true,
            max_bytes: 0,
            trim_history: false,
            shaping: crate::model::config::PayloadShapingConfig::default(),
        },
        payload_guard_retry_config: None,
        inference_attempt_budget: Arc::new(
            crate::anthropic::inference_attempt_budget::InferenceAttemptBudget::new(4),
        ),
        request_api_key_id: None,
    }
}

fn raw_test_route(raw_body: &[u8]) -> ExternalRouteRequest {
    let mut route = test_route("raw-placeholder");
    route.effective_raw_body = Bytes::copy_from_slice(raw_body);
    route.raw_body = route.effective_raw_body.clone();
    let raw_probe = Arc::new(probe_raw_messages_body(&route.effective_raw_body));
    let model_hint = raw_probe.model.clone();
    let stream_hint = raw_probe.stream;
    route.effective_raw_probe = Some(raw_probe);
    route.payload = None;
    route.body_mode_filter = Some(ExternalPoolRequestBodyMode::RawPassthrough);
    route.model_hint = model_hint;
    route.stream_hint = stream_hint;
    route.request_input_tokens = 0;
    route.upstream_model = None;
    route.model_resolution_source = None;
    route
}

fn test_payload(model: &str) -> MessagesRequest {
    MessagesRequest {
        model: model.to_string(),
        max_tokens: 8,
        messages: vec![Message {
            role: "user".to_string(),
            content: serde_json::json!("hello"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "You are a careful coding assistant. ".repeat(180),
            cache_control: None,
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: Some(Metadata {
            user_id: Some("user_test_account__session_external-projection-session".to_string()),
        }),
    }
}

#[test]
fn direct_external_policy_enabled_is_global_direct_reason() {
    let mut config = ExternalPoolsConfig::default();

    assert_eq!(
        direct_external_policy_static_reason(&config, "/cc/v1/messages", "claude-custom"),
        None
    );

    config.external_pools_enabled = true;
    config.external_direct_policy_enabled = true;
    assert_eq!(
        direct_external_policy_static_reason(&config, "/cc/v1/messages", "claude-custom")
            .as_deref(),
        Some("explicit_direct")
    );

    config.direct_external_model_rules = vec!["sonnet".to_string()];
    assert_eq!(
        direct_external_policy_static_reason(&config, "/cc/v1/messages", "claude-sonnet-4-5")
            .as_deref(),
        Some("model_rule:claude-sonnet-4-5")
    );

    config.direct_external_model_rules.clear();
    config.direct_external_path_rules = vec!["/ha/".to_string()];
    assert_eq!(
        direct_external_policy_static_reason(&config, "/ha/v1/messages", "custom-model").as_deref(),
        Some("path_rule:/ha/v1/messages")
    );
}

#[test]
fn direct_external_policy_respects_external_pool_route_policy() {
    let mut config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_direct_policy_enabled: true,
        external_pool_route_mode: crate::model::config::ExternalPoolRouteMode::DenyList,
        external_pool_route_rules: vec!["/cc".to_string()],
        ..ExternalPoolsConfig::default()
    };

    assert_eq!(
        direct_external_policy_static_reason(&config, "/cc/v1/messages", "claude-custom"),
        None
    );
    assert_eq!(
        direct_external_policy_static_reason(&config, "/v1/messages", "claude-custom").as_deref(),
        Some("explicit_direct")
    );

    config.external_pool_route_mode = crate::model::config::ExternalPoolRouteMode::AllowList;
    config.external_pool_route_rules = vec!["/ha".to_string()];
    assert_eq!(
        direct_external_policy_static_reason(&config, "/v1/messages", "claude-custom"),
        None
    );
    assert_eq!(
        direct_external_policy_static_reason(&config, "/ha/v1/messages", "claude-custom")
            .as_deref(),
        Some("explicit_direct")
    );
}

#[test]
fn external_pool_route_policy_applies_per_pool_rules() {
    let mut pool = test_pool("https://example.com/v1", true);

    assert!(external_pool_route_allowed(&pool, Some("/cc/v1/messages")));
    assert!(external_pool_route_allowed(&pool, None));

    pool.route_mode = crate::model::config::ExternalPoolRouteMode::AllowList;
    pool.route_rules = vec!["/cc".to_string(), "/dfcache/team".to_string()];
    assert!(external_pool_route_allowed(&pool, Some("/cc/v1/messages")));
    assert!(external_pool_route_allowed(
        &pool,
        Some("/dfcache/team/v1/messages")
    ));
    assert!(!external_pool_route_allowed(
        &pool,
        Some("/dfcache/team-a/v1/messages")
    ));
    assert!(!external_pool_route_allowed(&pool, Some("/v1/messages")));

    pool.route_rules = vec!["*".to_string()];
    assert!(external_pool_route_allowed(&pool, Some("/v1/messages")));
    assert!(external_pool_route_allowed(&pool, Some("/ha/v1/messages")));

    pool.route_mode = crate::model::config::ExternalPoolRouteMode::DenyList;
    pool.route_rules = vec!["/cc".to_string()];
    assert!(!external_pool_route_allowed(&pool, Some("/cc/v1/messages")));
    assert!(external_pool_route_allowed(&pool, Some("/v1/messages")));

    pool.route_rules = vec!["*".to_string()];
    assert!(!external_pool_route_allowed(&pool, Some("/v1/messages")));
}

#[test]
fn fallback_body_mode_filter_does_not_ignore_raw_passthrough_pools() {
    let normalized_pool = test_pool("https://normalized.example.com/v1", true);
    let mut raw_pool = test_pool("https://raw.example.com/v1", true);
    raw_pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;

    assert!(external_pool_matches_body_mode_filter(
        &normalized_pool,
        None
    ));
    assert!(external_pool_matches_body_mode_filter(&raw_pool, None));
    assert!(external_pool_matches_body_mode_filter(
        &raw_pool,
        Some(ExternalPoolRequestBodyMode::RawPassthrough)
    ));
    assert!(!external_pool_matches_body_mode_filter(
        &raw_pool,
        Some(ExternalPoolRequestBodyMode::Normalized)
    ));
    assert!(external_pool_matches_body_mode_filter(
        &normalized_pool,
        Some(ExternalPoolRequestBodyMode::Normalized)
    ));
}

#[test]
fn external_pool_outbound_body_strips_budget_tokens_for_adaptive_thinking() {
    let mut route = test_route("claude-opus-4-7-thinking");
    payload_mut(&mut route).thinking = Some(Thinking {
        thinking_type: "adaptive".to_string(),
        budget_tokens: 20000,
    });
    payload_mut(&mut route).output_config = Some(OutputConfig {
        effort: Some("xhigh".to_string()),
    });
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-7-thinking","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"adaptive","budget_tokens":20000},"output_config":{"effort":"xhigh"}}"#,
        );

    let pool = test_pool("https://example.com/v1", true);
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["thinking"]["type"], "adaptive");
    assert!(value["thinking"].get("budget_tokens").is_none());
    assert_eq!(value["output_config"]["effort"], "xhigh");
}

#[test]
fn external_pool_normalized_wire_preserves_omitted_output_effort_for_five_rounds() {
    let mut route = test_route("claude-opus-4-7-thinking");
    payload_mut(&mut route).thinking = Some(Thinking {
        thinking_type: "adaptive".to_string(),
        budget_tokens: 0,
    });
    payload_mut(&mut route).output_config = Some(OutputConfig::default());
    route.raw_body = Bytes::from_static(
        br#"{"model":"claude-opus-4-7-thinking","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"adaptive"},"output_config":{}}"#,
    );
    route.effective_raw_body = route.raw_body.clone();

    let pool = test_pool("https://example.com/v1", true);
    for round in 0..5 {
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse normalized outbound body");

        assert_eq!(value["thinking"]["type"], "adaptive", "round {round}");
        assert_eq!(
            value["output_config"],
            serde_json::json!({}),
            "round {round}: normalized forwarding must not invent an effort"
        );
        assert!(
            value.pointer("/output_config/effort").is_none(),
            "round {round}"
        );
    }
}

#[test]
fn external_pool_outbound_body_applies_resolved_upstream_model() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.model_resolution_source = Some("alias".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );

    let pool = test_pool_with_model_dot_normalization();
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let prepared = external_pool_prepare_request(&route, &pool).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-sonnet-4-5");
    assert_eq!(
        prepared.outbound_model.as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(payload_ref(&route).model, "claude-sonnet-4-5-20250929");
}

#[test]
fn external_pool_outbound_body_uses_normalized_payload_not_stale_raw_body() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.model_resolution_source = Some("alias".to_string());
    payload_mut(&mut route).messages = vec![Message {
        role: "user".to_string(),
        content: serde_json::json!([{
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/jpeg",
                "data": "/9j/normalized"
            }
        }]),
    }];
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"/9j/stale"}}]}],"stream":true}"#,
        );

    let pool = test_pool_with_model_dot_normalization();
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-sonnet-4-5");
    assert_eq!(
        value["messages"][0]["content"][0]["source"]["media_type"],
        "image/jpeg"
    );
    assert_eq!(
        value["messages"][0]["content"][0]["source"]["data"],
        "/9j/normalized"
    );
}

#[test]
fn external_pool_outbound_body_applies_model_mapping_and_thinking_normalization() {
    let mut route = test_route("claude-opus-4-5-20251101");
    route.upstream_model = Some("claude-opus-4.5".to_string());
    route.model_resolution_source = Some("alias".to_string());
    payload_mut(&mut route).thinking = Some(Thinking {
        thinking_type: "adaptive".to_string(),
        budget_tokens: 20000,
    });
    payload_mut(&mut route).output_config = Some(OutputConfig {
        effort: Some("xhigh".to_string()),
    });
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-5-20251101","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"adaptive","budget_tokens":20000},"output_config":{"effort":"xhigh"}}"#,
        );

    let pool = test_pool_with_model_dot_normalization();
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-opus-4-5");
    assert_eq!(value["thinking"]["type"], "adaptive");
    assert!(value["thinking"].get("budget_tokens").is_none());
    assert_eq!(value["output_config"]["effort"], "xhigh");
}

#[test]
fn external_pool_outbound_body_normalizes_payload_claude_model_without_mapping() {
    let route = test_route("claude-haiku-4.5");

    let pool = test_pool_with_model_dot_normalization();
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-haiku-4-5");
}

#[test]
fn external_pool_outbound_body_preserves_dot_model_when_pool_normalization_disabled() {
    let route = test_route("claude-haiku-4.5");
    let pool = test_pool("https://example.com/v1", true);

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-haiku-4.5");
}

#[test]
fn external_pool_raw_passthrough_keeps_body_byte_for_byte() {
    let raw = br#" { "model":"client-model","stream":false,"messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}] } "#;
    let route = raw_test_route(raw);
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::None;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;

    let prepared = external_pool_prepare_request(&route, &pool).unwrap();

    assert_eq!(prepared.body, Bytes::from_static(raw));
    assert!(prepared.outbound_model.is_none());
}

#[test]
fn raw_route_can_build_normalized_body_for_selected_normalized_pool() {
    let raw = br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}],"max_tokens":8}"#;
    let route = raw_test_route(raw);
    assert!(route.payload.is_none());
    assert_eq!(
        route.body_mode_filter,
        Some(ExternalPoolRequestBodyMode::RawPassthrough)
    );

    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::Normalized;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;
    pool.supported_models = vec!["client-model".to_string()];

    let prepared = external_pool_prepare_request(&route, &pool)
        .expect("raw route should lazily parse for normalized pool");
    let value: serde_json::Value =
        serde_json::from_slice(&prepared.body).expect("normalized body remains JSON");

    assert_eq!(value["model"], "client-model");
    assert_eq!(value["messages"][0]["content"], "hello");
    assert_eq!(value["max_tokens"], 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_route_failover_reselects_normalized_pool_by_model_after_raw_pool_502() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let (bad_gateway_url, bad_gateway_task) = spawn_test_raw_http_response(
        StatusCode::BAD_GATEWAY,
        TestRawHttpBody::Fixed(
            br#"{"error":{"message":"upstream temporarily unavailable"}}"#.to_vec(),
        ),
    )
    .await;
    let normalized_fake = AuxiliaryFallbackFakeServer::start().await;

    let mut raw_pool = create_pool_request("raw-first-502", 1, true);
    raw_pool.base_url = bad_gateway_url;
    raw_pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    raw_pool.raw_model_mode = ExternalPoolRawModelMode::None;
    raw_pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;
    raw_pool.supported_models = vec!["client-model".to_string()];
    postgres.create_external_pool(raw_pool).await.unwrap();

    let mut normalized_pool = create_pool_request("normalized-second-ok", 10, true);
    normalized_pool.base_url = normalized_fake.base_url.clone();
    normalized_pool.request_body_mode = ExternalPoolRequestBodyMode::Normalized;
    normalized_pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;
    normalized_pool.supported_models = vec!["client-model".to_string()];
    postgres
        .create_external_pool(normalized_pool)
        .await
        .unwrap();

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_retry_max_attempts: 2,
        ..ExternalPoolsConfig::default()
    };
    let route = raw_test_route(
        br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}],"max_tokens":8}"#,
    );
    let hits_before = normalized_fake.snapshot().3;

    let response = match manager.forward_with_failover_result(config, route).await {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(error) => {
            panic!("raw 502 should retry a model-supported normalized pool: {error:?}");
        }
    };

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(normalized_fake.snapshot().3, hits_before + 1);
    timeout(Duration::from_secs(2), bad_gateway_task)
        .await
        .expect("raw failing pool should have been tried")
        .expect("raw failing pool task should finish");

    postgres.drop_test_schema().await.unwrap();
}

#[test]
fn raw_direct_preflight_fallback_and_failover_use_effective_original_sha_for_five_rounds() {
    let effective = Bytes::from_static(
        br#" {"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}],"future":{"keep":true},"max_tokens":20480} "#,
    );
    let effective_sha = hex::encode(Sha256::digest(&effective));
    let working = Bytes::from_static(
        br#"{"model":"working-model","max_tokens":7,"messages":[{"role":"user","content":"working serialization must not escape"}]}"#,
    );
    let paths = [
        ("direct", UsageRouteSubtype::ExternalDirectPolicy),
        ("preflight", UsageRouteSubtype::ExternalFallbackPreflight),
        (
            "fallback",
            UsageRouteSubtype::ExternalFallbackAfterLocalAttempts,
        ),
    ];

    for round in 1..=5 {
        for (path, route_subtype) in paths {
            let mut route = raw_test_route(effective.as_ref());
            route.route_subtype = route_subtype;
            route.raw_body = working.clone();
            if path == "fallback" {
                route.payload = Some(test_payload("working-model"));
                route.body_mode_filter = None;
                route.local_attempted = true;
            }

            for failover_attempt in 1..=2 {
                for raw_model_mode in [
                    ExternalPoolRawModelMode::None,
                    ExternalPoolRawModelMode::ProbeOnly,
                ] {
                    let mut pool = test_pool("https://example.com/v1", true);
                    pool.id = failover_attempt;
                    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
                    pool.raw_model_mode = raw_model_mode;
                    pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;

                    let prepared = external_pool_prepare_request(&route, &pool)
                        .unwrap_or_else(|err| panic!("round {round} {path}: {}", err.message));
                    assert_eq!(
                        hex::encode(Sha256::digest(&prepared.body)),
                        effective_sha,
                        "round {round} {path} failover attempt {failover_attempt} mode {raw_model_mode:?}"
                    );
                    assert_eq!(prepared.body, effective);
                    assert!(
                        !prepared
                            .body
                            .windows(13)
                            .any(|window| window == b"working-model")
                    );
                }
            }
        }
    }
}

#[test]
fn raw_rewrite_only_changes_effective_top_level_model_for_five_rounds() {
    let effective = Bytes::from_static(
        br#" {"messages":[{"role":"user","content":{"model":"nested-model","future":true}}],"model":"client-model","max_tokens":20480,"future_top":"keep"} "#,
    );
    let expected =
        rewrite_raw_top_level_model(&effective, "mapped-model").expect("expected rewrite");
    let expected_sha = hex::encode(Sha256::digest(&expected));

    for round in 1..=5 {
        let mut route = raw_test_route(effective.as_ref());
        route.raw_body =
            Bytes::from_static(br#"{"model":"working-model","max_tokens":1,"messages":[]}"#);
        route.route_subtype = UsageRouteSubtype::ExternalFallbackAfterLocalAttempts;
        route.payload = Some(test_payload("working-model"));
        route.body_mode_filter = None;

        for failover_attempt in 1..=2 {
            let mut pool = test_pool("https://example.com/v1", true);
            pool.id = failover_attempt;
            pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
            pool.raw_model_mode = ExternalPoolRawModelMode::RewriteTopLevel;
            pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
            pool.model_mapping_rules = vec![model_rule("client-model", "mapped-model")];

            let prepared = external_pool_prepare_request(&route, &pool).expect("raw rewrite");
            assert_eq!(hex::encode(Sha256::digest(&prepared.body)), expected_sha);
            assert_eq!(
                prepared.body, expected,
                "round {round} attempt {failover_attempt}"
            );
            let value: serde_json::Value = serde_json::from_slice(&prepared.body).expect("JSON");
            assert_eq!(value["model"], "mapped-model");
            assert_eq!(value["messages"][0]["content"]["model"], "nested-model");
            assert_eq!(value["future_top"], "keep");
        }
    }
}

#[test]
fn normalized_failover_builds_and_serializes_the_full_body_once_per_request() {
    let model = "claude-sonnet-4-5";
    let mut route = test_route(model);
    let mut original = serde_json::to_value(payload_ref(&route)).expect("typed request value");
    original["future_top"] = json!({"preserved": true});
    route.effective_raw_body =
        Bytes::from(serde_json::to_vec(&original).expect("effective request serialization"));
    route.raw_body = Bytes::from(
        serde_json::to_vec(payload_ref(&route)).expect("working request serialization"),
    );
    route.payload_guard_external_enabled = false;
    let probe_count_before = raw_body_probe_invocations_for_current_thread();

    for pool_id in 1..=8 {
        let mapped_model = format!("mapped-model-{pool_id}");
        let mut pool = test_pool("https://example.com/v1", true);
        pool.id = pool_id;
        pool.request_body_mode = ExternalPoolRequestBodyMode::Normalized;
        pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
        pool.model_mapping_rules = vec![model_rule(model, &mapped_model)];

        let prepared = external_pool_prepare_request(&route, &pool).expect("normalized request");
        let value: serde_json::Value =
            serde_json::from_slice(&prepared.body).expect("normalized JSON");
        assert_eq!(value["model"], mapped_model);
        assert_eq!(value["future_top"]["preserved"], true);
    }

    let counts = route.preparation_operation_counts();
    assert_eq!(counts.normalized_base_builds, 1);
    assert_eq!(counts.normalized_original_value_parses, 1);
    assert_eq!(counts.normalized_json_serializations, 2);
    assert_eq!(counts.payload_guard_serializations, 0);
    assert_eq!(counts.raw_payload_parses, 0);
    assert_eq!(
        raw_body_probe_invocations_for_current_thread() - probe_count_before,
        1,
        "only the request-scoped normalized base may be probed"
    );
}

#[test]
fn raw_failover_shares_tool_and_usage_projection_parsing_across_pools() {
    let raw = br#"{
        "model":"claude-sonnet-4-5",
        "max_tokens":128,
        "messages":[
            {"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}
        ],
        "tools":[{"name":"Bash","description":"run","input_schema":{"type":"object"}}],
        "stream":false,
        "metadata":{"user_id":"raw-cache-session"}
    }"#;
    let route = raw_test_route(raw);

    for pool_id in 1..=8 {
        let mut pool = test_pool("https://example.com/v1", true);
        pool.id = pool_id;
        pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let names = external_route_known_tool_names(&route);
        assert!(names.iter().any(|name| name == "Bash"));
        let projection = projection_context(&route, &pool, 0).expect("usage projection");
        let expected_credential_key = format!("external_pool:{pool_id}");
        assert_eq!(
            projection.credential_key.as_deref(),
            Some(expected_credential_key.as_str())
        );
    }

    let counts = route.preparation_operation_counts();
    assert_eq!(counts.raw_payload_parses, 1);
    assert_eq!(counts.known_tool_name_builds, 1);
    assert_eq!(counts.usage_projection_builds, 1);
    assert_eq!(counts.normalized_base_builds, 0);
    assert_eq!(counts.normalized_original_value_parses, 0);
    assert_eq!(counts.normalized_json_serializations, 0);
}

#[test]
fn normalized_overlay_preserves_future_fields_after_sanitize_image_and_steering_for_five_rounds() {
    let jpeg = base64::engine::general_purpose::STANDARD
        .encode([0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00]);
    let original_value = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 128,
        "stream": false,
        "service_tier": "auto",
        "context_management": {"edits": [{"type": "clear_tool_uses_20250919"}]},
        "future_top": {"keep": true},
        "system": [{
            "type": "text",
            "text": "original system",
            "cache_control": {"type": "ephemeral"},
            "future_system": "system-extra"
        }],
        "messages": [
            {
                "role": "user",
                "future_message_field": "user-extra",
                "content": [{
                    "type": "image",
                    "future_block": "image-block-extra",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": jpeg,
                        "future_source": "source-extra"
                    }
                }]
            },
            {
                "role": "assistant",
                "future_message_field": "assistant-extra",
                "content": [{
                    "type": "text",
                    "text": "safe prefix\nuser Continue\n\nBash: hidden transcript",
                    "caller": {"kind": "future-caller"},
                    "future_block": "assistant-block-extra"
                }]
            },
            {
                "role": "user",
                "future_message_field": "current-extra",
                "content": "current question"
            }
        ],
        "tools": [{
            "name": "Bash",
            "description": "run a command",
            "input_schema": {"type": "object", "future_schema": true},
            "future_tool_field": "tool-extra"
        }],
        "metadata": {"user_id": "user_test", "future_metadata": "metadata-extra"},
        "thinking": {"type": "adaptive", "future_thinking": "thinking-extra"},
        "output_config": {"effort": "high", "future_output": "output-extra"}
    });
    let original = Bytes::from(serde_json::to_vec(&original_value).expect("original JSON"));

    for round in 1..=5 {
        let (sanitized, report) =
            crate::anthropic::transcript_sanitizer::sanitize_raw_request_assistant_history(
                &original,
            )
            .expect("assistant history inspection succeeds")
            .expect("polluted assistant history is sanitized");
        assert_eq!(report.blocks, 1, "round {round}");
        let mut payload: MessagesRequest =
            serde_json::from_slice(&sanitized).expect("sanitized typed payload");
        assert_eq!(
            crate::anthropic::body_processing::normalize_base64_image_media_types(&mut payload),
            1,
            "round {round}"
        );
        assert!(
            crate::anthropic::prompt_steering::apply_to_messages_request(
                "/cc/v1/messages",
                crate::model::config::CompatProfile::ClaudeCode,
                &crate::model::config::PromptSteeringConfig::default(),
                &mut payload,
            )
        );

        let mut route = test_route("claude-sonnet-4-5");
        route.effective_raw_body = original.clone();
        route.raw_body = Bytes::from(sanitized);
        route.payload = Some(payload);
        route.body_mode_filter = Some(ExternalPoolRequestBodyMode::Normalized);
        let mut pool = test_pool("https://example.com/v1", true);
        pool.request_body_mode = ExternalPoolRequestBodyMode::Normalized;

        let prepared = external_pool_prepare_request(&route, &pool).expect("normalized body");
        let value: serde_json::Value =
            serde_json::from_slice(&prepared.body).expect("normalized JSON");

        assert_eq!(value["service_tier"], "auto", "round {round}");
        assert_eq!(
            value["context_management"],
            original_value["context_management"]
        );
        assert_eq!(value["future_top"], json!({"keep": true}));
        assert_eq!(value["messages"][0]["future_message_field"], "user-extra");
        assert_eq!(
            value["messages"][0]["content"][0]["future_block"],
            "image-block-extra"
        );
        assert_eq!(
            value["messages"][0]["content"][0]["source"]["future_source"],
            "source-extra"
        );
        assert_eq!(
            value["messages"][0]["content"][0]["source"]["media_type"],
            "image/jpeg"
        );
        assert_eq!(
            value["messages"][1]["future_message_field"],
            "assistant-extra"
        );
        assert_eq!(
            value["messages"][1]["content"][0]["caller"]["kind"],
            "future-caller"
        );
        assert_eq!(
            value["messages"][1]["content"][0]["future_block"],
            "assistant-block-extra"
        );
        assert!(
            !prepared
                .body
                .windows(13)
                .any(|window| window == b"user Continue")
        );
        assert!(
            !prepared
                .body
                .windows(17)
                .any(|window| window == b"hidden transcript")
        );
        assert_eq!(
            value["messages"][2]["future_message_field"],
            "current-extra"
        );
        assert_eq!(value["tools"][0]["future_tool_field"], "tool-extra");
        assert_eq!(value["tools"][0]["input_schema"]["future_schema"], true);
        assert_eq!(value["metadata"]["future_metadata"], "metadata-extra");
        assert_eq!(value["thinking"]["future_thinking"], "thinking-extra");
        assert_eq!(value["output_config"]["future_output"], "output-extra");

        let systems = value["system"].as_array().expect("system array");
        assert_eq!(systems.len(), 2);
        assert!(
            systems[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("<prompt_steering"))
        );
        assert!(systems[0].get("future_system").is_none());
        assert_eq!(systems[1]["text"], "original system");
        assert_eq!(systems[1]["future_system"], "system-extra");
    }
}

#[test]
fn normalized_overlay_large_history_and_tool_sets_preserve_tail_identity_for_five_rounds() {
    const HISTORY_MESSAGES: usize = 4_097;
    const RETAINED_MESSAGES: usize = 257;
    const TOOLS: usize = 2_048;

    let messages = (0..HISTORY_MESSAGES)
        .map(|index| {
            json!({
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "content": format!("turn-{index}"),
                "future_message_index": index,
            })
        })
        .collect::<Vec<_>>();
    let tools = (0..TOOLS)
        .map(|index| {
            json!({
                "name": format!("tool_{index}"),
                "description": "large overlay fixture",
                "input_schema": {"type": "object"},
                "future_tool_index": index,
            })
        })
        .collect::<Vec<_>>();
    let original_value = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 128,
        "messages": messages,
        "tools": tools,
        "future_top": "large-overlay-extra",
    });
    let effective_raw_body =
        Bytes::from(serde_json::to_vec(&original_value).expect("large original JSON"));
    let mut payload: MessagesRequest =
        serde_json::from_value(original_value).expect("large typed payload");
    payload.messages = payload
        .messages
        .split_off(HISTORY_MESSAGES - RETAINED_MESSAGES);

    for round in 1..=5 {
        let mut route = test_route("claude-sonnet-4-5");
        route.effective_raw_body = effective_raw_body.clone();
        route.raw_body =
            Bytes::from(serde_json::to_vec(&payload).expect("large working payload serialization"));
        route.payload = Some(payload.clone());
        route.body_mode_filter = Some(ExternalPoolRequestBodyMode::Normalized);
        route.payload_guard_external_enabled = false;
        let mut pool = test_pool("https://example.com/v1", true);
        pool.request_body_mode = ExternalPoolRequestBodyMode::Normalized;

        let prepared = external_pool_prepare_request(&route, &pool).expect("large normalized body");
        let value: serde_json::Value =
            serde_json::from_slice(&prepared.body).expect("large normalized JSON");
        let messages = value["messages"].as_array().expect("messages array");
        let tools = value["tools"].as_array().expect("tools array");

        assert_eq!(messages.len(), RETAINED_MESSAGES, "round {round}");
        assert_eq!(
            messages[0]["future_message_index"],
            HISTORY_MESSAGES - RETAINED_MESSAGES,
            "round {round} first retained message"
        );
        assert_eq!(
            messages.last().unwrap()["future_message_index"],
            HISTORY_MESSAGES - 1,
            "round {round} last retained message"
        );
        assert_eq!(tools.len(), TOOLS, "round {round}");
        assert_eq!(tools[0]["future_tool_index"], 0);
        assert_eq!(tools[TOOLS - 1]["future_tool_index"], TOOLS - 1);
        assert_eq!(value["future_top"], "large-overlay-extra");
    }
}

#[test]
fn raw_body_none_model_mode_ignores_mapping_settings_and_keeps_body() {
    let raw = br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}]}"#;
    let route = raw_test_route(raw);
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::None;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::DirectMapping;
    pool.model_mapping_require_match = true;
    pool.normalize_model_version_dots = true;
    pool.model_mapping_rules = vec![model_rule("other-model", "mapped-model")];

    let prepared = external_pool_prepare_request(&route, &pool).unwrap();

    assert_eq!(prepared.body, Bytes::from_static(raw));
    assert!(prepared.outbound_model.is_none());
}

#[test]
fn external_pool_raw_body_mode_does_not_apply_payload_guard() {
    let raw = br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"keep raw body even when guard config is enabled"}]}"#;
    let mut route = test_route("client-model");
    route.effective_raw_body = Bytes::from_static(raw);
    route.raw_body = Bytes::from_static(raw);
    route.payload_guard_external_enabled = true;
    route.payload_guard_initial_config = PayloadGuardConfig {
        enabled: true,
        max_bytes: 32,
        trim_history: true,
        shaping: crate::model::config::PayloadShapingConfig::default(),
    };
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::None;

    let prepared = external_pool_prepare_request(&route, &pool).unwrap();

    assert_eq!(prepared.body, Bytes::from_static(raw));
}

#[test]
fn external_pool_normalized_body_mode_applies_payload_guard() {
    let mut route = test_route("client-model");
    let mut messages = Vec::new();
    for idx in 0..24 {
        messages.push(Message {
            role: "user".to_string(),
            content: serde_json::json!(format!("old history {idx} {}", "x".repeat(240))),
        });
        messages.push(Message {
            role: "assistant".to_string(),
            content: serde_json::json!(format!("old answer {idx} {}", "y".repeat(180))),
        });
    }
    messages.push(Message {
        role: "user".to_string(),
        content: serde_json::json!("current question"),
    });
    payload_mut(&mut route).messages = messages;
    route.raw_body =
        Bytes::from(serde_json::to_vec(payload_ref(&route)).expect("serialize raw body for route"));
    let original_len = route.raw_body.len();
    route.payload_guard_external_enabled = true;
    route.payload_guard_initial_config = PayloadGuardConfig {
        enabled: true,
        max_bytes: 2_000,
        trim_history: true,
        shaping: crate::model::config::PayloadShapingConfig::default(),
    };
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::Normalized;

    let prepared = external_pool_prepare_request(&route, &pool).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&prepared.body).expect("normalized body remains json");

    assert!(prepared.body.len() < original_len);
    assert_eq!(
        value["messages"].as_array().unwrap().last().unwrap()["content"],
        serde_json::json!("current question")
    );
}

#[test]
fn external_pool_raw_probe_only_maps_model_without_mutating_body() {
    let raw =
        br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let route = raw_test_route(raw);
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::ProbeOnly;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
    pool.model_mapping_rules = vec![model_rule("client-model", "mapped-model")];

    let prepared = external_pool_prepare_request(&route, &pool).unwrap();

    assert_eq!(prepared.body, Bytes::from_static(raw));
    assert_eq!(prepared.outbound_model.as_deref(), Some("mapped-model"));
    assert_eq!(route.stream_hint, Some(true));
    assert_eq!(route.model_hint.as_deref(), Some("client-model"));
}

#[test]
fn external_pool_raw_probe_is_reused_across_modes_and_failover_for_five_rounds() {
    let raw = br#"{"model":"client-model","max_tokens":8,"stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let before_route = raw_body_probe_invocations_for_current_thread();
    let route = raw_test_route(raw);
    let after_route = raw_body_probe_invocations_for_current_thread();
    assert_eq!(
        after_route,
        before_route + 1,
        "route construction probes once"
    );

    for round in 0..5 {
        for pool_id in 1..=5 {
            for mode in [
                ExternalPoolRawModelMode::None,
                ExternalPoolRawModelMode::ProbeOnly,
                ExternalPoolRawModelMode::RewriteTopLevel,
            ] {
                let mut pool = test_pool("https://example.com/v1", true);
                pool.id = pool_id;
                pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
                pool.raw_model_mode = mode;
                pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;

                let prepared =
                    external_pool_prepare_request(&route, &pool).unwrap_or_else(|error| {
                        panic!("round {round}, pool {pool_id}: {}", error.message)
                    });
                assert_eq!(prepared.body, route.effective_raw_body);
                assert_eq!(
                    prepared.body.as_ptr(),
                    route.effective_raw_body.as_ptr(),
                    "round {round}, pool {pool_id}, mode {mode:?}"
                );
            }
        }
    }

    assert_eq!(
        raw_body_probe_invocations_for_current_thread(),
        after_route,
        "all failover candidates must reuse the entry-bound probe"
    );
}

#[test]
fn external_pool_raw_probe_only_require_mapping_match_rejects_miss_without_mutating_body() {
    let raw =
        br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#;
    let route = raw_test_route(raw);
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::ProbeOnly;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
    pool.model_mapping_require_match = true;
    pool.model_mapping_rules = vec![model_rule("other-model", "mapped-model")];

    let err = match external_pool_prepare_request(&route, &pool) {
        Ok(_) => panic!("raw probe should reject mapping miss"),
        Err(err) => err,
    };

    assert!(err.retryable);
    assert_eq!(err.status, Some(StatusCode::BAD_GATEWAY));
    assert_eq!(error_type_for_external_error(&err), "model_mapping_miss");
}

#[test]
fn external_pool_raw_rewrite_changes_only_top_level_model() {
    let raw = br#"{"messages":[{"role":"user","content":[{"type":"tool_result","content":{"model":"nested-model"}}]}],"model":"client-model","stream":false}"#;
    let route = raw_test_route(raw);
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::RewriteTopLevel;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
    pool.model_mapping_rules = vec![model_rule("client-model", "mapped-model")];

    let prepared = external_pool_prepare_request(&route, &pool).unwrap();
    let text = std::str::from_utf8(&prepared.body).unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&prepared.body).expect("rewritten body remains json");

    assert_eq!(value["model"], "mapped-model");
    assert_eq!(
        value["messages"][0]["content"][0]["content"]["model"],
        "nested-model"
    );
    assert!(text.contains(r#""model":"nested-model""#));
    assert_eq!(prepared.outbound_model.as_deref(), Some("mapped-model"));
}

#[test]
fn external_pool_raw_rewrite_require_mapping_match_rejects_miss() {
    let raw = br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}]}"#;
    let route = raw_test_route(raw);
    let mut pool = test_pool("https://example.com/v1", true);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::RewriteTopLevel;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
    pool.model_mapping_require_match = true;
    pool.model_mapping_rules = vec![model_rule("other-model", "mapped-model")];

    let err = match external_pool_prepare_request(&route, &pool) {
        Ok(_) => panic!("raw rewrite should reject mapping miss"),
        Err(err) => err,
    };

    assert!(err.retryable);
    assert_eq!(err.status, Some(StatusCode::BAD_GATEWAY));
    assert_eq!(error_type_for_external_error(&err), "model_mapping_miss");
}

#[test]
fn raw_messages_body_hints_ignore_nested_model_without_top_level_model() {
    let raw = Bytes::from_static(
            br#"{"messages":[{"role":"user","content":[{"type":"text","model":"nested-model","text":"hello"}]}],"stream":true}"#,
        );

    let (model, stream) = raw_messages_body_hints(&raw);

    assert_eq!(model, None);
    assert_eq!(stream, Some(true));
}

#[test]
fn external_pool_outbound_body_passthrough_uses_original_request_model() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;
    pool.model_mapping_rules = vec![model_rule("claude-sonnet-4-5-20250929", "custom-sonnet")];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let prepared = external_pool_prepare_request(&route, &pool).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-sonnet-4-5-20250929");
    assert_eq!(
        prepared.outbound_model.as_deref(),
        Some("claude-sonnet-4-5-20250929")
    );
}

#[test]
fn external_pool_outbound_body_passthrough_mapping_maps_hit_and_keeps_original_on_miss() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
    pool.model_mapping_rules = vec![model_rule("claude-sonnet-4-5-20250929", "external-sonnet")];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");
    assert_eq!(value["model"], "external-sonnet");

    pool.model_mapping_rules = vec![model_rule("claude-opus-4-8", "external-opus")];
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");
    assert_eq!(value["model"], "claude-sonnet-4-5-20250929");
}

#[test]
fn external_pool_outbound_body_require_mapping_match_rejects_miss_before_send() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
    pool.model_mapping_require_match = true;
    pool.model_mapping_rules = vec![model_rule("claude-opus-4-8", "external-opus")];

    let err = external_pool_outbound_body(&route, &pool).unwrap_err();

    assert!(err.retryable);
    assert_eq!(err.status, Some(StatusCode::BAD_GATEWAY));
    assert_eq!(error_type_for_external_error(&err), "model_mapping_miss");
    assert!(err.message.contains("requires model mapping match"));
}

#[test]
fn external_pool_outbound_body_direct_mapping_uses_original_model() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::DirectMapping;
    pool.model_mapping_rules = vec![model_rule("claude-sonnet-4-5-20250929", "external-sonnet")];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let prepared = external_pool_prepare_request(&route, &pool).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "external-sonnet");
    assert_eq!(prepared.outbound_model.as_deref(), Some("external-sonnet"));
}

#[test]
fn external_pool_outbound_body_processed_mapping_uses_upstream_model() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
    pool.model_mapping_rules = vec![model_rule("claude-sonnet-4.5", "external-sonnet")];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "external-sonnet");
}

#[test]
fn external_pool_outbound_body_mapping_miss_falls_back_to_existing_conversion() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::DirectMapping;
    pool.model_mapping_rules = vec![model_rule("claude-opus-4.8", "external-opus")];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-sonnet-4-5");
}

#[test]
fn external_pool_outbound_body_mapping_target_is_final() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
    pool.model_mapping_rules = vec![model_rule("claude-sonnet-4.5", "claude-sonnet-4.5")];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-sonnet-4.5");
}

#[test]
fn external_pool_mapping_rules_normalize_and_match_on_call_path() {
    let mut route = test_route("claude-sonnet-4-5-20250929");
    route.upstream_model = Some("claude-sonnet-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
    pool.model_mapping_rules = normalize_external_pool_model_mapping_rules(vec![
        model_rule("  CLAUDE-SONNET-4.5  ", "  CLAUDE-SONNET-4-5  "),
        model_rule("", "ignored-target"),
        model_rule("ignored-source", ""),
    ]);

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(pool.model_mapping_rules.len(), 1);
    assert_eq!(pool.model_mapping_rules[0].target, "CLAUDE-SONNET-4-5");
    assert_eq!(value["model"], "CLAUDE-SONNET-4-5");
}

#[test]
fn external_pool_mapping_supports_common_direct_date_to_dot_rule() {
    let mut route = test_route("claude-opus-4-5-20251101");
    route.upstream_model = Some("claude-opus-4.5".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-5-20251101","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool_with_model_dot_normalization();
    pool.model_mapping_mode = ExternalPoolModelMappingMode::DirectMapping;
    pool.model_mapping_rules = vec![model_rule("claude-opus-4-5-20251101", "claude-opus-4.5")];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-opus-4.5");
}

#[test]
fn external_pool_mapping_supports_common_processed_thinking_to_dash_rule() {
    let mut route = test_route("claude-opus-4-8-thinking");
    route.upstream_model = Some("claude-opus-4.8-thinking".to_string());
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-8-thinking","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
    let mut pool = test_pool("https://example.com/v1", true);
    pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
    pool.model_mapping_rules = vec![model_rule(
        "claude-opus-4.8-thinking",
        "claude-opus-4-8-thinking",
    )];

    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["model"], "claude-opus-4-8-thinking");
}

#[test]
fn external_pool_outbound_model_normalization_only_changes_claude_numeric_versions() {
    assert_eq!(
        normalize_external_pool_outbound_model("claude-opus-4.8"),
        "claude-opus-4-8"
    );
    assert_eq!(
        normalize_external_pool_outbound_model("claude-opus-4.8-thinking"),
        "claude-opus-4-8-thinking"
    );
    assert_eq!(
        normalize_external_pool_outbound_model(" claude-sonnet-4.5[1m] "),
        "claude-sonnet-4-5[1m]"
    );
    assert_eq!(
        normalize_external_pool_outbound_model("deepseek-3.2"),
        "deepseek-3.2"
    );
}

#[test]
fn external_pool_outbound_body_strips_budget_tokens_for_disabled_thinking() {
    let mut route = test_route("claude-opus-4-7");
    payload_mut(&mut route).thinking = Some(Thinking {
        thinking_type: "disabled".to_string(),
        budget_tokens: 20000,
    });
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-7","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"disabled","budget_tokens":20000}}"#,
        );

    let pool = test_pool("https://example.com/v1", true);
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["thinking"]["type"], "disabled");
    assert!(value["thinking"].get("budget_tokens").is_none());
}

#[test]
fn external_pool_outbound_body_preserves_enabled_budget_tokens() {
    let mut route = test_route("claude-sonnet-4-6-thinking");
    payload_mut(&mut route).thinking = Some(Thinking {
        thinking_type: "enabled".to_string(),
        budget_tokens: 12345,
    });
    route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-6-thinking","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"enabled","budget_tokens":12345}}"#,
        );

    let pool = test_pool("https://example.com/v1", true);
    let outbound = test_external_pool_outbound_body(&route, &pool);
    let value: serde_json::Value = serde_json::from_slice(&outbound).expect("parse outbound body");

    assert_eq!(value["thinking"]["type"], "enabled");
    assert_eq!(value["thinking"]["budget_tokens"], 12345);
}

fn projection_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    projection_context_with_output_uplift(route, pool, uplift_percent, 0, 0)
}

fn projection_context_with_output_uplift(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    build_external_usage_projection_context(
        route,
        pool,
        uplift_percent,
        output_uplift_min_tokens,
        output_uplift_percent,
    )
}

fn disable_path_output_postprocess(route: &mut ExternalRouteRequest) {
    route.reported_usage.default.final_output_guard_enabled = false;
    for policy in route.reported_usage.path_overrides.values_mut() {
        policy.final_output_guard_enabled = false;
    }
}

fn event_usage_i64(event: &str, key: &str) -> i64 {
    event
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("data:"))
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json.trim()).ok())
        .and_then(|value| value.get("usage").and_then(|usage| usage.get(key)).cloned())
        .and_then(|value| value.as_i64())
        .expect("usage field")
}

fn event_data_value(event: &[u8]) -> serde_json::Value {
    let text = std::str::from_utf8(event).expect("event utf8");
    text.lines()
        .find_map(|line| line.trim_start().strip_prefix("data:"))
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json.trim()).ok())
        .expect("event data json")
}

fn assert_projected_cache_creation_consistent(usage: &serde_json::Value) {
    let aggregate = usage["cache_creation_input_tokens"]
        .as_i64()
        .expect("cache_creation_input_tokens");
    let five_min = usage["cache_creation"]["ephemeral_5m_input_tokens"]
        .as_i64()
        .expect("ephemeral_5m_input_tokens");
    let one_hour = usage["cache_creation"]["ephemeral_1h_input_tokens"]
        .as_i64()
        .expect("ephemeral_1h_input_tokens");

    assert_eq!(aggregate, five_min + one_hour);
}

#[test]
fn external_pool_url_adds_single_v1_for_standard_message_path() {
    let config = ExternalPoolsConfig::default();
    let cases = [
        (
            "http://pool.example.com",
            "http://pool.example.com/v1/messages",
        ),
        (
            "http://pool.example.com/",
            "http://pool.example.com/v1/messages",
        ),
        (
            "http://pool.example.com/v1",
            "http://pool.example.com/v1/messages",
        ),
        (
            "http://pool.example.com/v1/",
            "http://pool.example.com/v1/messages",
        ),
        (
            "http://pool.example.com/api",
            "http://pool.example.com/api/v1/messages",
        ),
        (
            "http://pool.example.com/api/v1",
            "http://pool.example.com/api/v1/messages",
        ),
    ];

    for (base_url, expected) in cases {
        let actual = external_pool_url(&test_pool(base_url, false), "/cc/v1/messages", &config)
            .expect("valid external pool url");
        assert_eq!(actual.as_str(), expected);
    }
}

#[test]
fn external_pool_url_uses_pool_messages_path_even_when_preserve_path_is_true() {
    let config = ExternalPoolsConfig::default();
    let base_v1 = external_pool_url(
        &test_pool("http://pool.example.com/v1", true),
        "/v1/messages",
        &config,
    )
    .expect("valid external pool url");
    assert_eq!(base_v1.as_str(), "http://pool.example.com/v1/messages");

    let cc_path = external_pool_url(
        &test_pool("http://pool.example.com", true),
        "/cc/v1/messages",
        &config,
    )
    .expect("valid external pool url");
    assert_eq!(cc_path.as_str(), "http://pool.example.com/v1/messages");
}

#[test]
fn external_pool_models_url_adds_single_v1() {
    let cases = [
        (
            "http://pool.example.com",
            "http://pool.example.com/v1/models",
        ),
        (
            "http://pool.example.com/",
            "http://pool.example.com/v1/models",
        ),
        (
            "http://pool.example.com/v1",
            "http://pool.example.com/v1/models",
        ),
        (
            "http://pool.example.com/v1/",
            "http://pool.example.com/v1/models",
        ),
        (
            "http://pool.example.com/api",
            "http://pool.example.com/api/v1/models",
        ),
        (
            "http://pool.example.com/api/v1",
            "http://pool.example.com/api/v1/models",
        ),
    ];

    for (base_url, expected) in cases {
        let actual = external_pool_models_url(base_url).expect("valid models url");
        assert_eq!(actual.as_str(), expected);
    }
}

#[test]
fn external_pool_auto_disable_window_has_own_default() {
    let config = ExternalPoolsConfig::default();

    assert_eq!(config.external_pool_auto_disable_window_secs, 60);
    assert_eq!(config.local_pool_circuit_window_secs, 60);
}

#[test]
fn usage_projection_pass_through_keeps_body_unchanged() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let projected = maybe_project_non_stream_usage(body.clone(), None);

    assert_eq!(projected.body, body);
    assert_eq!(
        projected.usage_capture.raw,
        projected.usage_capture.reported
    );
    assert!(!projected.protocol_contamination);
}

#[test]
fn openai_usage_is_normalized_for_non_stream_external_pool_body() {
    let body = Bytes::from_static(
        br#"{"type":"message","content":[{"type":"text","text":"OK"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}"#,
    );
    let route = test_route("claude-opus-4-6");
    let pool = test_pool("http://pool.example.com", false);

    let projected = maybe_project_non_stream_usage(body, None);
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");

    assert_eq!(usage["input_tokens"], 11);
    assert_eq!(usage["output_tokens"], 3);
    assert_eq!(
        projected.usage_capture.raw.map(|usage| usage.input_tokens),
        Some(11)
    );
    assert_eq!(
        projected.usage_capture.usage_candidate_path.as_deref(),
        Some("$.usage")
    );

    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing should be captured");
    assert_eq!(billing.raw_usage.input_tokens, 11);
    assert_eq!(billing.reported_usage.output_tokens, 3);
    assert_eq!(billing.usage_candidate_path.as_deref(), Some("$.usage"));
    assert!(!billing.usage_estimated);
}

#[test]
fn openai_usage_is_captured_for_stream_external_pool_billing() {
    let route = test_route("claude-opus-4-6");
    let pool = test_pool("http://pool.example.com", false);
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
        ..ExternalUsageCapture::default()
    }));

    capture_sse_event_usage(
        br#"event: message_delta
data: {"type":"message_delta","usage":{"prompt_tokens":1234,"completion_tokens":56,"total_tokens":1290}}

"#,
        None,
        Some(&capture),
    );

    let billing = external_pool_billing_from_capture_ref(&route, &pool, &capture)
        .expect("stream usage should produce billing");
    assert_eq!(billing.raw_usage.input_tokens, 1234);
    assert_eq!(billing.raw_usage.output_tokens, 56);
    assert_eq!(billing.reported_usage.input_tokens, 1234);
    assert_eq!(billing.reported_usage.output_tokens, 56);
    assert!(!billing.usage_estimated);
    assert_eq!(
        billing.usage_candidate_path, None,
        "stream captures do not assign a non-stream candidate path"
    );
}

#[test]
fn openai_stream_usage_keeps_local_shaping_separate_from_raw_billing() {
    let route = test_route("claude-opus-4-6");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
        ..ExternalUsageCapture::default()
    }));
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"prompt_tokens":1234,"completion_tokens":56,"total_tokens":1290}}

"#;

    let rewritten = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let value = event_data_value(&rewritten);
    assert_eq!(value["usage"]["output_tokens"], 56);

    let billing = external_pool_billing_from_capture_ref(&route, &pool, &capture)
        .expect("stream usage should produce billing");
    assert_eq!(billing.raw_usage.input_tokens, 1234);
    assert_eq!(billing.raw_usage.output_tokens, 56);
    assert_eq!(billing.shaped_usage.output_tokens, 56);
    assert_eq!(
        value["usage"]["input_tokens"].as_i64(),
        Some(billing.reported_usage.input_tokens as i64)
    );
    assert!(billing.usage_projection_applied);
    assert!(!billing.usage_estimated);
}

#[test]
fn non_stream_missing_usage_injects_estimated_billing_body() {
    let body = Bytes::from_static(
        br#"{"type":"message","content":[{"type":"text","text":"OK"}],"stop_reason":"end_turn"}"#,
    );
    let route = test_route("claude-opus-4-6");
    let pool = test_pool("http://pool.example.com", false);

    let projected =
        process_non_stream_response_usage(body, Some(&route), None, std::iter::empty::<String>());
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("estimated usage");

    assert!(projected.usage_capture.usage_estimated);
    assert_eq!(
        projected.usage_capture.usage_estimate_reason.as_deref(),
        Some("missing_upstream_usage")
    );
    assert!(usage["input_tokens"].as_i64().unwrap() > 0);
    assert!(usage["output_tokens"].as_i64().unwrap() >= 0);

    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing should be captured");
    assert!(billing.usage_estimated);
    assert_eq!(
        billing.usage_estimate_reason.as_deref(),
        Some("missing_upstream_usage")
    );
    assert_eq!(billing.usage_candidate_path, None);
    assert!(billing.reported_usage.input_tokens > 0);
}

#[test]
fn external_pool_billing_matches_dashed_opus_request_to_dotted_pricing_model() {
    let body = Bytes::from_static(
        br#"{"type":"message","content":[{"type":"text","text":"OK"}],"usage":{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
    );
    let route = test_route("claude-opus-4-8");
    route.pricing_catalog.upsert_manual_price(
        "claude-opus-4.8",
        crate::anthropic::pricing::ModelPricing {
            input_cost_per_token: 0.000007,
            output_cost_per_token: 0.000031,
            cache_creation_input_token_cost: 0.000008,
            cache_read_input_token_cost: 0.0000007,
        },
    );
    let pool = test_pool("http://pool.example.com", false);

    let projected =
        process_non_stream_response_usage(body, Some(&route), None, std::iter::empty::<String>());
    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing should be captured");

    assert!(billing.pricing_available);
    assert_eq!(billing.pricing_model.as_deref(), Some("claude-opus-4.8"));
    assert!(billing.billable_cost_usd > 0.0);
    assert!((billing.billable_cost_usd - 0.00101).abs() < f64::EPSILON);
}

#[test]
fn non_stream_unknown_json_without_usage_injects_estimated_usage_and_billing() {
    let body = Bytes::from_static(
        br#"{"id":"chatcmpl_fake","choices":[{"message":{"role":"assistant","content":"OK"}}],"model":"claude-opus-4-6"}"#,
    );
    let route = test_route("claude-opus-4-6");
    let pool = test_pool("http://pool.example.com", false);

    let projected =
        process_non_stream_response_usage(body, Some(&route), None, std::iter::empty::<String>());
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("estimated usage");

    assert!(projected.usage_capture.usage_estimated);
    assert_eq!(
        projected.usage_capture.usage_estimate_reason.as_deref(),
        Some("unrecognized_success_body")
    );
    assert!(usage["input_tokens"].as_i64().unwrap() > 0);
    assert!(usage["output_tokens"].as_i64().unwrap() > 0);

    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing should be captured");
    assert!(billing.usage_estimated);
    assert_eq!(
        billing.usage_estimate_reason.as_deref(),
        Some("unrecognized_success_body")
    );
    assert_eq!(billing.usage_candidate_path, None);
    assert!(billing.reported_usage.input_tokens > 0);
    assert!(billing.reported_usage.output_tokens > 0);
}

#[test]
fn non_stream_unknown_text_without_usage_records_estimated_billing_without_rewriting_body() {
    let body = Bytes::from_static(b"OK");
    let route = test_route("claude-opus-4-6");
    let pool = test_pool("http://pool.example.com", false);

    let projected = process_non_stream_response_usage(
        body.clone(),
        Some(&route),
        None,
        std::iter::empty::<String>(),
    );

    assert_eq!(projected.body, body);
    assert!(projected.usage_capture.usage_estimated);
    assert_eq!(
        projected.usage_capture.usage_estimate_reason.as_deref(),
        Some("unrecognized_success_body")
    );

    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing should be captured");
    assert!(billing.usage_estimated);
    assert_eq!(
        billing.usage_estimate_reason.as_deref(),
        Some("unrecognized_success_body")
    );
    assert!(billing.reported_usage.input_tokens > 0);
    assert_eq!(billing.reported_usage.output_tokens, 0);
}

#[test]
fn stream_output_token_estimator_counts_text_thinking_and_tool_events() {
    let chunk = Bytes::from_static(
        br#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hello world"}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"thinking_delta","thinking":"reasoning text"}}

"#,
    );

    assert!(estimate_external_stream_output_tokens(&chunk) >= 3);
}

#[test]
fn stream_missing_usage_builds_estimated_billable_external_pool_billing() {
    let route = test_route("claude-opus-4-6");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    pool.stream_response_mode = Some(ExternalPoolStreamResponseMode::EventPassthrough);
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        request_input_tokens: Some(route.request_input_tokens),
        stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
        ..ExternalUsageCapture::default()
    }));
    let chunk = Bytes::from_static(
        br#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello world"}}

"#,
    );
    let output_tokens = estimate_external_stream_output_tokens(&chunk);

    let billing = external_pool_billing_from_stream_estimate(
        &route,
        &pool,
        &capture,
        Some(&projection),
        output_tokens,
    )
    .expect("estimated stream billing");

    assert!(billing.usage_estimated);
    assert_eq!(
        billing.usage_estimate_reason.as_deref(),
        Some("missing_stream_usage")
    );
    assert_eq!(
        billing.usage_candidate_path.as_deref(),
        Some("$stream.estimated")
    );
    assert_eq!(
        billing.stream_response_mode.as_deref(),
        Some(ExternalPoolStreamResponseMode::EventPassthrough.as_str())
    );
    assert!(billing.pricing_available);
    assert!(billing.billable_cost_usd > 0.0);
    assert!(billing.reported_usage.input_tokens > 0);
    assert!(billing.reported_usage.output_tokens > 0);
}

#[test]
fn external_record_usage_source_prefers_request_estimate_for_estimated_billing() {
    let route = test_route("claude-opus-4-6");
    let pool = test_pool("http://pool.example.com", false);

    let estimated = process_non_stream_response_usage(
        Bytes::from_static(
            br#"{"id":"chatcmpl_fake","choices":[{"message":{"role":"assistant","content":"OK"}}]}"#,
        ),
        Some(&route),
        None,
        std::iter::empty::<String>(),
    );
    let estimated_billing =
        external_pool_billing_from_capture(&route, &pool, estimated.usage_capture)
            .expect("estimated billing");
    assert!(estimated_billing.usage_estimated);
    assert_eq!(
        external_record_usage_source(Some(&estimated_billing), true),
        UsageSource::RequestEstimate
    );

    let projected_route = test_route("claude-opus-4-6");
    let mut projected_pool = test_pool("http://pool.example.com", false);
    projected_pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&projected_route, &projected_pool, 0).expect("projection");
    let projected = maybe_project_non_stream_usage(
        Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":1000,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        ),
        Some(&projection),
    );
    let projected_billing = external_pool_billing_from_capture(
        &projected_route,
        &projected_pool,
        projected.usage_capture,
    )
    .expect("projected billing");
    assert!(projected_billing.usage_projection_applied);
    assert_eq!(
        external_record_usage_source(Some(&projected_billing), true),
        UsageSource::LocalPromptCache
    );

    let passthrough = maybe_project_non_stream_usage(
        Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":1000,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        ),
        None,
    );
    let passthrough_billing =
        external_pool_billing_from_capture(&route, &pool, passthrough.usage_capture)
            .expect("passthrough billing");
    assert_eq!(
        external_record_usage_source(Some(&passthrough_billing), true),
        UsageSource::UpstreamMetadata
    );
}

#[test]
fn external_non_stream_response_contamination_is_retryable_not_partial_success() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    let cases = [
        json!({"type":"text","text":polluted}),
        json!({"type":"thinking","thinking":polluted}),
        json!({"type":"thinking","thinking":polluted,"signature":"opaque-signature"}),
        json!({"type":"redacted_thinking","data":polluted}),
    ];
    for _round in 0..5 {
        for content in &cases {
            let body = Bytes::from(
                serde_json::to_vec(&json!({
                    "type": "message",
                    "content": [content.clone()],
                    "usage": {"input_tokens": 12, "output_tokens": 7},
                    "future_field": {"preserved": true}
                }))
                .unwrap(),
            );
            let projected =
                maybe_project_non_stream_usage_with_tools(body, None, ["Bash".to_string()]);
            assert!(projected.protocol_contamination);
            let wire = String::from_utf8(projected.body.to_vec()).unwrap();
            assert!(!wire.contains("user Continue"));
            assert!(!wire.contains("Bash: hidden"));
            let value: serde_json::Value = serde_json::from_str(&wire).unwrap();
            assert_eq!(value["usage"]["input_tokens"], 12);
            assert_eq!(value["usage"]["output_tokens"], 7);
            assert_eq!(value["future_field"]["preserved"], true);
        }
    }

    let error = external_protocol_contamination_error(&ExternalPoolsConfig::default());
    assert_eq!(error.status, Some(StatusCode::OK));
    assert_eq!(error.message, RESPONSE_PROTOCOL_CONTAMINATION_DETAIL);
    assert!(error.retryable);
    assert!(error.auto_disable_reason.is_none());
    assert_eq!(
        error.cooldown.as_ref().map(|(_, reason)| reason.as_str()),
        Some("protocol_contamination")
    );
    assert!(error.protocol_error.is_none());
}

#[test]
fn external_non_stream_clean_response_remains_byte_identical() {
    for _round in 0..5 {
        let body = Bytes::from_static(
            br#"{"type":"message","content":[{"type":"thinking","thinking":"ordinary reasoning","signature":"opaque-signature"},{"type":"text","text":"visible answer"}],"usage":{"input_tokens":12,"output_tokens":7},"future_field":{"preserved":true}}"#,
        );
        let projected =
            maybe_project_non_stream_usage_with_tools(body.clone(), None, ["Bash".to_string()]);
        assert!(!projected.protocol_contamination);
        assert_eq!(projected.body, body);
    }
}

#[test]
fn usage_projection_applies_current_path_policy_to_json_body() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let projected = maybe_project_non_stream_usage(body.clone(), Some(&projection));

    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    assert!(
        usage
            .get("input_tokens")
            .and_then(|value| value.as_i64())
            .is_some_and(|tokens| (1..=96).contains(&tokens))
    );
    assert!(
        usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default()
            == 0
    );
    assert!(
        usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default()
            > 0
    );
    let reported = projected.usage_capture.reported.expect("reported usage");
    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn raw_passthrough_keeps_body_but_still_applies_usage_projection() {
    let raw = br#"{"model":"claude-sonnet-4-5","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"system":[{"type":"text","text":"You are a careful coding assistant. You are a careful coding assistant. You are a careful coding assistant. "}],"stream":false,"metadata":{"user_id":"raw-projection-session"}}"#;
    let route = raw_test_route(raw);
    assert!(route.payload.is_none());
    assert_eq!(route.request_input_tokens, 0);

    let mut pool = test_pool("http://pool.example.com", false);
    pool.request_body_mode = ExternalPoolRequestBodyMode::RawPassthrough;
    pool.raw_model_mode = ExternalPoolRawModelMode::None;
    pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let prepared = external_pool_prepare_request(&route, &pool).unwrap();
    assert_eq!(prepared.body, Bytes::from_static(raw));

    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":5,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let projection = projection_context(&route, &pool, 0).expect("raw usage projection");
    assert!(projection.raw_input_tokens > 0);

    let projected = maybe_project_non_stream_usage(body.clone(), Some(&projection));
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");

    assert!(projected.usage_capture.projected);
    assert_eq!(
        projected.usage_capture.request_input_tokens,
        Some(projection.raw_input_tokens)
    );
    assert_ne!(projected.body, body);
    assert!(
        usage
            .get("input_tokens")
            .and_then(|value| value.as_i64())
            .is_some_and(|tokens| (1..=96).contains(&tokens))
    );
}

#[test]
fn usage_projection_path_skip_non_stream_blocks_external_projection() {
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            skip_non_stream_usage_projection: true,
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0);
    assert!(projection.is_none());
}

#[test]
fn usage_projection_shapes_uncached_non_stream_usage_by_path_policy() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":4165,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-opus-4-6");
    route.request_input_tokens = 4165;
    route.prompt_cache_target_read_ratio = 0.5;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0).expect("projection");
    let projected = maybe_project_non_stream_usage(body.clone(), Some(&projection));

    assert_ne!(projected.body, body);
    assert!(projected.usage_capture.projected);
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    assert_eq!(usage["input_tokens"].as_i64().unwrap(), 1);
    assert_eq!(usage["output_tokens"].as_i64().unwrap(), 2);
    assert_eq!(usage["cache_read_input_tokens"].as_i64().unwrap(), 0);
    assert_eq!(usage["cache_creation_input_tokens"].as_i64().unwrap(), 4164);
    assert_projected_cache_creation_consistent(usage);
    assert_eq!(
        projected.usage_capture.raw.map(|usage| usage.input_tokens),
        Some(4165)
    );
    let reported = projected.usage_capture.reported.expect("reported usage");
    assert_eq!(reported.input_tokens, 1);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(reported.cache_creation_input_tokens, 4164);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_final_output_guard_caps_after_external_output_uplift() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":4165,"output_tokens":80,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-opus-4-6");
    route.request_input_tokens = 4165;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            final_output_max_tokens: 80,
            final_output_jitter_min_tokens: 10,
            final_output_jitter_max_tokens: 10,
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection =
        projection_context_with_output_uplift(&route, &pool, 0, 1, 100).expect("projection");
    let projected = maybe_project_non_stream_usage(body.clone(), Some(&projection));

    assert_ne!(projected.body, body);
    assert!(projected.usage_capture.projected);
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    assert_eq!(usage["output_tokens"].as_i64().unwrap(), 70);
    let reported = projected.usage_capture.reported.expect("reported usage");
    assert_eq!(reported.output_tokens, 70);
}

#[test]
fn usage_projection_path_skip_non_stream_keeps_external_usage_raw() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":4165,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-opus-4-6");
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            skip_non_stream_usage_projection: true,
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0);
    assert!(projection.is_none());
    let projected = maybe_project_non_stream_usage(body.clone(), projection.as_ref());

    assert_eq!(projected.body, body);
    assert!(!projected.usage_capture.projected);
    assert_eq!(
        projected.usage_capture.raw.map(|usage| usage.input_tokens),
        Some(4165)
    );
    assert_eq!(
        projected
            .usage_capture
            .reported
            .map(|usage| usage.input_tokens),
        Some(4165)
    );

    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing should be captured");
    assert!(!billing.usage_projection_applied);
    assert_eq!(billing.raw_usage.input_tokens, 4165);
    assert_eq!(billing.shaped_usage.input_tokens, 4165);
    assert_eq!(billing.reported_usage.input_tokens, 4165);
    assert_eq!(billing.reported_usage.output_tokens, 2);
}

#[test]
fn usage_projection_disabled_reported_usage_blocks_non_stream_projection() {
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            enabled: false,
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0);
    assert!(projection.is_none());
}

#[test]
fn usage_projection_disabled_reported_usage_blocks_stream_projection() {
    let mut route = test_route("claude-sonnet-4-5");
    payload_mut(&mut route).stream = true;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            enabled: false,
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0);
    assert!(projection.is_none());
}

#[test]
fn usage_projection_path_skip_non_stream_keeps_stream_projection_enabled() {
    let mut route = test_route("claude-sonnet-4-5");
    payload_mut(&mut route).stream = true;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            skip_non_stream_usage_projection: true,
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0);
    assert!(projection.is_some());
}

#[test]
fn usage_projection_ignores_external_cache_when_local_policy_has_no_cache() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":7,"cache_creation_input_tokens":50000,"cache_read_input_tokens":25000}}"#,
        );
    let route = test_route("deepseek-3.2");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 25).expect("projection");
    let projected = maybe_project_non_stream_usage(body.clone(), Some(&projection));

    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    assert_eq!(
        usage
            .get("input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        count_external_route_input_tokens(payload_ref(&route)) as i64
    );
    assert_eq!(
        usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        0
    );
    assert_eq!(
        usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        0
    );
    assert!(projected.usage_capture.projected);
    assert_eq!(
        projected
            .usage_capture
            .raw
            .expect("raw")
            .cache_creation_input_tokens,
        50_000
    );
    assert_eq!(
        projected
            .usage_capture
            .reported
            .expect("reported")
            .input_tokens,
        count_external_route_input_tokens(payload_ref(&route))
    );
    assert_eq!(
        projected
            .usage_capture
            .reported
            .expect("reported")
            .cache_creation_input_tokens,
        0
    );
}

#[test]
fn usage_projection_no_cache_route_removes_external_cache_usage() {
    let mut route = test_route("claude-sonnet-4-5");
    route.prompt_cache_strategy_type = PromptCacheStrategyType::NoCache;
    route.prompt_cache_simulation_mode = PromptCacheSimulationMode::Disabled;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(64),
            output: ReportedUsageFieldPolicy::sample_max(5),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0);
    assert!(projection.is_some());

    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":9,"cache_creation_input_tokens":50000,"cache_read_input_tokens":25000,"cache_creation":{"ephemeral_5m_input_tokens":50000,"ephemeral_1h_input_tokens":0}}}"#,
        );
    let projected = maybe_project_non_stream_usage(body, projection.as_ref());
    assert!(projected.usage_capture.projected);
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    assert_eq!(
        usage
            .get("input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        count_external_route_input_tokens(payload_ref(&route)) as i64
    );
    assert_eq!(
        usage
            .get("output_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        9
    );
    assert_eq!(
        usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        0
    );
    assert_eq!(
        usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        0
    );
    assert!(usage.get("cache_creation").is_none());
    assert_eq!(
        projected
            .usage_capture
            .raw
            .expect("raw")
            .cache_creation_input_tokens,
        50_000
    );
    assert_eq!(
        projected
            .usage_capture
            .reported
            .expect("reported")
            .cache_creation_input_tokens,
        0
    );
    assert_eq!(
        projected
            .usage_capture
            .reported
            .expect("reported")
            .cache_read_input_tokens,
        0
    );
}

#[test]
fn usage_projection_applies_external_pool_uplift_after_path_policy() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/v1/messages".to_string();
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let no_uplift_projection = projection_context(&route, &pool, 0).expect("projection");
    let no_uplift = maybe_project_non_stream_usage(body.clone(), Some(&no_uplift_projection));
    let with_uplift_projection = projection_context(&route, &pool, 25).expect("projection");
    let with_uplift = maybe_project_non_stream_usage(body, Some(&with_uplift_projection));

    let no_uplift_usage = no_uplift.usage_capture.reported.expect("no uplift usage");
    let with_uplift_shaped = with_uplift
        .usage_capture
        .shaped
        .expect("with uplift shaped usage");
    let with_uplift_usage = with_uplift.usage_capture.reported.expect("uplift usage");
    assert_eq!(
        with_uplift_shaped.total_input_tokens,
        no_uplift_usage.total_input_tokens
    );
    assert_eq!(
        with_uplift_shaped.input_tokens,
        no_uplift_usage.input_tokens
    );
    assert_eq!(
        with_uplift_shaped.output_tokens,
        no_uplift_usage.output_tokens
    );
    assert_eq!(
        with_uplift_shaped.cache_creation_input_tokens,
        no_uplift_usage.cache_creation_input_tokens
    );
    assert_eq!(
        with_uplift_shaped.cache_read_input_tokens,
        no_uplift_usage.cache_read_input_tokens
    );
    assert_eq!(with_uplift_usage.input_tokens, no_uplift_usage.input_tokens);
    assert_eq!(
        with_uplift_usage.cache_creation_input_tokens,
        uplift_tokens(no_uplift_usage.cache_creation_input_tokens, 25)
    );
    assert_eq!(
        with_uplift_usage.cache_read_input_tokens,
        uplift_tokens(no_uplift_usage.cache_read_input_tokens, 25)
    );
}

#[test]
fn usage_projection_final_cache_read_guard_runs_after_external_pool_uplift() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            final_cache_read_max_tokens: 100,
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let warmup_projection = projection_context(&route, &pool, 0).expect("warmup projection");
    let _warmup = maybe_project_non_stream_usage(body.clone(), Some(&warmup_projection));
    warmup_projection.record_success();

    payload_mut(&mut route).messages.extend([
        Message {
            role: "assistant".to_string(),
            content: serde_json::json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: serde_json::json!("continue external projection session"),
        },
    ]);
    refresh_test_route_derived_state(&mut route);
    let projection = projection_context(&route, &pool, 200).expect("projection");
    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let reported = projected.usage_capture.reported.expect("reported usage");

    assert_eq!(reported.cache_read_input_tokens, 100);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_final_cache_creation_guard_runs_after_external_pool_uplift() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.request_input_tokens = 200_000;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            final_cache_creation_max_tokens: 100,
            final_cache_creation_jitter_min_tokens: 0,
            final_cache_creation_jitter_max_tokens: 0,
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 200).expect("projection");
    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    let reported = projected.usage_capture.reported.expect("reported usage");

    assert_eq!(reported.input_tokens, 1);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(reported.cache_creation_input_tokens, 100);
    assert_eq!(
        usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        100
    );
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_final_input_guard_samples_input_without_cache_read_after_uplift() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/v1".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 200).expect("projection");
    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let reported = projected.usage_capture.reported.expect("reported usage");

    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert!(reported.cache_creation_input_tokens > 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_final_input_guard_leaves_compliant_input_unchanged() {
    let policy = ReportedCacheUsagePolicy::from_path_policy(
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
        42,
    )
    .expect("policy");
    let usage = CacheUsage {
        total_input_tokens: 50_000,
        input_tokens: 42,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 49_958,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    let guarded = policy.apply_final_input_guard(usage);

    assert_eq!(guarded.input_tokens, 42);
    assert_eq!(guarded.cache_read_input_tokens, 49_958);
    assert_eq!(guarded.total_input_tokens, 50_000);
}

#[test]
fn usage_projection_stream_capture_uses_latest_projected_reported_usage() {
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/v1".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        reported: Some(CacheUsage {
            total_input_tokens: 120_000,
            input_tokens: 10_000,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 110_000,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        }),
        ..ExternalUsageCapture::default()
    }));

    let event =
            br#"data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":120000}}

"#;

    let out = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let text = std::str::from_utf8(&out).expect("event text");
    let event_input = event_usage_i64(text, "input_tokens");
    let reported = capture.lock().reported.expect("reported usage");

    assert!((1..=96).contains(&event_input));
    assert_eq!(reported.input_tokens as i64, event_input);
    assert!(reported.input_tokens < 10_000);
    assert!(reported.cache_read_input_tokens > 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_treats_upstream_cache_read_as_evidence_not_value() {
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/v1".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    assert!(projection.raw_input_tokens > 96);

    let small_read = maybe_project_non_stream_usage(
        Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":1}}"#,
        ),
        Some(&projection),
    );
    let sentinel_read = maybe_project_non_stream_usage(
        Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":120000}}"#,
        ),
        Some(&projection),
    );

    assert_eq!(small_read.body, sentinel_read.body);
    assert_eq!(
        small_read.usage_capture.reported,
        sentinel_read.usage_capture.reported
    );
    assert_eq!(
        small_read
            .usage_capture
            .raw
            .expect("small raw usage")
            .cache_read_input_tokens,
        1
    );
    assert_eq!(
        sentinel_read
            .usage_capture
            .raw
            .expect("sentinel raw usage")
            .cache_read_input_tokens,
        120_000
    );
    let reported = sentinel_read
        .usage_capture
        .reported
        .expect("reported usage");
    assert!((1..=96).contains(&reported.input_tokens));
    assert!(reported.cache_read_input_tokens > 0);
    assert_ne!(reported.cache_read_input_tokens, 120_000);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_carries_cache_read_evidence_across_split_sse_events() {
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/v1".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let start = br#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":100000,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":120000}}}

"#;
    let delta = br#"event: message_delta
data: {"type":"message_delta","usage":{"output_tokens":2}}

"#;

    let _ = rewrite_sse_event_usage(start, Some(&projection), Some(&capture));
    let final_event = rewrite_sse_event_usage(delta, Some(&projection), Some(&capture));
    let final_value = event_data_value(&final_event);
    let final_usage = &final_value["usage"];
    let reported = capture.lock().reported.expect("reported usage");

    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.input_tokens as i64, final_usage["input_tokens"]);
    assert_eq!(reported.output_tokens, 2);
    assert!(reported.cache_read_input_tokens > 0);
    assert_ne!(reported.cache_read_input_tokens, 120_000);
    assert_eq!(
        reported.cache_read_input_tokens as i64,
        final_usage["cache_read_input_tokens"]
    );
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_does_not_leak_uncommitted_read_evidence_to_next_request() {
    let mut route = test_route("claude-sonnet-4-5");
    route.reported_usage.path_overrides.insert(
        "/v1".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let first_projection = projection_context(&route, &pool, 0).expect("first projection");
    let first = maybe_project_non_stream_usage(
        Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":120000}}"#,
        ),
        Some(&first_projection),
    );
    assert!(
        first
            .usage_capture
            .reported
            .expect("first reported usage")
            .cache_read_input_tokens
            > 0
    );

    let next_projection = projection_context(&route, &pool, 0).expect("next projection");
    let next = maybe_project_non_stream_usage(
        Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        ),
        Some(&next_projection),
    );
    let next_reported = next.usage_capture.reported.expect("next reported usage");

    assert!((1..=96).contains(&next_reported.input_tokens));
    assert_eq!(next_reported.cache_read_input_tokens, 0);
    assert_eq!(
        next_reported.total_input_tokens,
        next_reported
            .input_tokens
            .saturating_add(next_reported.cache_read_input_tokens)
            .saturating_add(next_reported.cache_creation_input_tokens)
    );
}

#[test]
fn usage_projection_output_uplift_only_applies_above_threshold() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":800,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/v1/messages".to_string();
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection =
        projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let shaped = projected.usage_capture.shaped.expect("shaped usage");
    let reported = projected.usage_capture.reported.expect("reported usage");

    assert_eq!(shaped.output_tokens, 800);
    assert_eq!(reported.output_tokens, 800);
}

#[test]
fn usage_projection_output_uplift_changes_only_final_reported_usage() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1200,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/v1/messages".to_string();
    disable_path_output_postprocess(&mut route);
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection =
        projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    let shaped = projected.usage_capture.shaped.expect("shaped usage");
    let reported = projected.usage_capture.reported.expect("reported usage");

    assert_eq!(shaped.output_tokens, 1200);
    assert_eq!(reported.output_tokens, uplift_tokens(1200, 50));
    assert_eq!(
        usage
            .get("output_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        uplift_tokens(1200, 50) as i64
    );
    assert_eq!(reported.input_tokens, shaped.input_tokens);
    assert_eq!(
        reported.cache_read_input_tokens,
        shaped.cache_read_input_tokens
    );
    assert_eq!(
        reported.cache_creation_input_tokens,
        shaped.cache_creation_input_tokens
    );
}

#[test]
fn usage_projection_uses_resolved_model_without_mutating_payload_model() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("sonnet");
    route.endpoint = "/v1/messages".to_string();
    route.upstream_model = Some("claude-sonnet-4-5".to_string());
    route.model_resolution_source = Some("alias".to_string());
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0).expect("projection");
    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing");

    assert_eq!(payload_ref(&route).model, "sonnet");
    assert_eq!(billing.pricing_model.as_deref(), Some("claude-sonnet-4-5"));
    assert!(billing.pricing_available);
}

#[test]
fn usage_projection_updates_external_pool_cache_after_success() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/v1/messages".to_string();
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let first_projection = projection_context(&route, &pool, 0).expect("first projection");
    let first = maybe_project_non_stream_usage(body.clone(), Some(&first_projection));
    let first_value: serde_json::Value =
        serde_json::from_slice(&first.body).expect("first projected json");
    let first_usage = first_value.get("usage").expect("first usage");
    assert_eq!(
        first_usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default(),
        0
    );
    assert!(
        first_usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default()
            > 0
    );
    first_projection.record_success();

    payload_mut(&mut route).messages.extend([
        Message {
            role: "assistant".to_string(),
            content: serde_json::json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: serde_json::json!("continue external projection session"),
        },
    ]);
    refresh_test_route_derived_state(&mut route);
    let second_projection = projection_context(&route, &pool, 0).expect("second projection");
    let second = maybe_project_non_stream_usage(body, Some(&second_projection));
    let second_value: serde_json::Value =
        serde_json::from_slice(&second.body).expect("second projected json");
    let second_usage = second_value.get("usage").expect("second usage");
    assert!(
        second_usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default()
            > 0
    );
}

#[test]
fn kiro_rs_tool_usage_projection_commits_external_pool_cache_only_after_success() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/kiro/v1/messages".to_string();
    route.prompt_cache_strategy_type = PromptCacheStrategyType::KiroRsTool;
    route.prompt_cache_simulation_mode = PromptCacheSimulationMode::Disabled;
    route.reported_usage = ReportedUsageConfig {
        default: ReportedUsagePathPolicy::disabled(),
        path_overrides: Default::default(),
    };
    payload_mut(&mut route).metadata = Some(Metadata {
        user_id: Some(
            "user_test_account__session_8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string(),
        ),
    });
    payload_mut(&mut route).system = Some(vec![SystemMessage {
        text: "stable external kiro strategy prompt ".repeat(700),
        cache_control: Some(serde_json::json!({"type": "ephemeral"})),
    }]);
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let failed_projection = projection_context(&route, &pool, 0).expect("failed projection");
    let failed = maybe_project_non_stream_usage(body.clone(), Some(&failed_projection));
    let failed_value: serde_json::Value =
        serde_json::from_slice(&failed.body).expect("failed projected json");
    assert_eq!(
        failed_value["usage"]["cache_read_input_tokens"]
            .as_i64()
            .unwrap_or_default(),
        0
    );
    assert!(
        failed_value["usage"]["cache_creation_input_tokens"]
            .as_i64()
            .unwrap_or_default()
            > 0
    );

    let retry_projection = projection_context(&route, &pool, 0).expect("retry projection");
    let retry = maybe_project_non_stream_usage(body.clone(), Some(&retry_projection));
    let retry_value: serde_json::Value =
        serde_json::from_slice(&retry.body).expect("retry projected json");
    assert_eq!(
        retry_value["usage"]["cache_read_input_tokens"]
            .as_i64()
            .unwrap_or_default(),
        0
    );
    assert!(
        retry_value["usage"]["cache_creation_input_tokens"]
            .as_i64()
            .unwrap_or_default()
            > 0
    );
    retry_projection.record_success();

    payload_mut(&mut route).messages.extend([
        Message {
            role: "assistant".to_string(),
            content: serde_json::json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: serde_json::json!("continue external kiro strategy session"),
        },
    ]);
    refresh_test_route_derived_state(&mut route);
    let second_projection = projection_context(&route, &pool, 0).expect("second projection");
    let second = maybe_project_non_stream_usage(body, Some(&second_projection));
    let second_value: serde_json::Value =
        serde_json::from_slice(&second.body).expect("second projected json");
    assert!(
        second_value["usage"]["cache_read_input_tokens"]
            .as_i64()
            .unwrap_or_default()
            > 0
    );
    let raw = second.usage_capture.raw.expect("raw usage");
    let reported = second.usage_capture.reported.expect("reported usage");
    assert_eq!(raw.input_tokens, 100000);
    assert_eq!(raw.cache_read_input_tokens, 0);
    assert!(reported.cache_read_input_tokens > 0);
    assert!(
        (32..=4_096).contains(&reported.input_tokens),
        "Kiro-RS Tool reported input range must remain authoritative, got {}",
        reported.input_tokens
    );
    assert_eq!(
        reported.input_tokens
            + reported.cache_creation_input_tokens
            + reported.cache_read_input_tokens,
        reported.total_input_tokens
    );
}

#[test]
fn kiro_rs_tool_usage_projection_applies_path_cache_creation_policy() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":0,"output_tokens":62,"cache_creation_input_tokens":1300180,"cache_read_input_tokens":37,"cache_creation":{"ephemeral_5m_input_tokens":1300180,"ephemeral_1h_input_tokens":0}}}"#,
        );
    let mut route = test_route("claude-opus-4-8");
    route.endpoint = "/cc/v1/messages".to_string();
    route.prompt_cache_strategy_type = PromptCacheStrategyType::KiroRsTool;
    route.prompt_cache_simulation_mode = PromptCacheSimulationMode::Disabled;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            cache_creation: ReportedUsageFieldPolicy::sample_max(4_000),
            ..ReportedUsagePathPolicy::default()
        },
    );
    payload_mut(&mut route).metadata = Some(Metadata {
        user_id: Some(
            "user_test_account__session_57f3e60f-2cc6-4e8f-ae7e-e43753320a09".to_string(),
        ),
    });
    payload_mut(&mut route).system = Some(vec![SystemMessage {
        text: "stable external kiro strategy prompt ".repeat(8_000),
        cache_control: Some(serde_json::json!({"type": "ephemeral"})),
    }]);
    route.request_input_tokens = count_external_route_input_tokens(payload_ref(&route));
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let projection = projection_context(&route, &pool, 0).expect("projection");
    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let value: serde_json::Value = serde_json::from_slice(&projected.body).expect("projected json");
    let usage = value.get("usage").expect("usage object");
    assert_projected_cache_creation_consistent(usage);
    let reported_creation = usage["cache_creation_input_tokens"]
        .as_i64()
        .expect("reported cache creation");
    assert!(
        (1..=4_000).contains(&reported_creation),
        "reported cache creation should follow path policy, got {reported_creation}"
    );
    assert!(
        !std::str::from_utf8(&projected.body)
            .unwrap()
            .contains("1300180")
    );

    let raw = projected.usage_capture.raw.expect("raw usage");
    assert_eq!(raw.cache_creation_input_tokens, 1_300_180);
    let reported = projected.usage_capture.reported.expect("reported usage");
    assert!((1..=4_000).contains(&reported.cache_creation_input_tokens));
    assert!(projected.usage_capture.projected);
}

#[test]
fn usage_projection_ignores_external_raw_cache_when_local_policy_reads() {
    let raw_creation_body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":80000,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/v1/messages".to_string();
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

    let first_projection = projection_context(&route, &pool, 0).expect("first projection");
    let first = maybe_project_non_stream_usage(raw_creation_body.clone(), Some(&first_projection));
    let first_value: serde_json::Value =
        serde_json::from_slice(&first.body).expect("first projected json");
    let first_usage = first_value.get("usage").expect("first usage");
    assert!(
        first_usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default()
            > 0
    );
    first_projection.record_success();

    payload_mut(&mut route).messages.extend([
        Message {
            role: "assistant".to_string(),
            content: serde_json::json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: serde_json::json!("continue external projection session"),
        },
    ]);
    refresh_test_route_derived_state(&mut route);
    let second_projection = projection_context(&route, &pool, 0).expect("second projection");
    let second = maybe_project_non_stream_usage(raw_creation_body, Some(&second_projection));
    let second_value: serde_json::Value =
        serde_json::from_slice(&second.body).expect("second projected json");
    let second_usage = second_value.get("usage").expect("second usage");
    let second_creation = second_usage
        .get("cache_creation_input_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_default();
    let second_read = second_usage
        .get("cache_read_input_tokens")
        .and_then(|value| value.as_i64())
        .unwrap_or_default();

    assert_eq!(second_creation, 0);
    assert!(second_read > 0);
    assert_ne!(second_creation, 80_000);
}

#[test]
fn external_pool_billing_pass_through_uses_reported_cost_without_floor() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":1000,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let projected = maybe_project_non_stream_usage(body, None);
    let route = test_route("claude-sonnet-4-5");
    let pool = test_pool("http://pool.example.com", false);
    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing");

    assert!(billing.pricing_available);
    assert!(!billing.cost_floor_applied);
    assert!((billing.raw_cost_usd - billing.shaped_cost_usd).abs() < f64::EPSILON);
    assert!((billing.raw_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
    assert!(billing.profit_usd.abs() < f64::EPSILON);
    assert!((billing.raw_cost_usd - billing.reported_cost_usd).abs() < f64::EPSILON);
    assert!((billing.billable_cost_usd - billing.reported_cost_usd).abs() < f64::EPSILON);
}

#[test]
fn external_pool_billing_tracks_raw_shaped_uplifted_costs() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 25);
    let projected = maybe_project_non_stream_usage(body, projection.as_ref());
    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing");

    assert!(billing.pricing_available);
    assert!(billing.raw_cost_usd > billing.shaped_cost_usd);
    assert!(billing.uplifted_cost_usd > billing.shaped_cost_usd);
    assert!((billing.reported_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
    assert!((billing.billable_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
    assert!(
        (billing.profit_usd - (billing.uplifted_cost_usd - billing.raw_cost_usd)).abs()
            < 0.000000001
    );
}

#[test]
fn external_pool_billing_uses_output_uplift_as_final_reported_cost() {
    let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":1000,"output_tokens":1200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/v1/messages".to_string();
    disable_path_output_postprocess(&mut route);
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection =
        projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");

    let projected = maybe_project_non_stream_usage(body, Some(&projection));
    let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
        .expect("billing");

    assert!(billing.pricing_available);
    assert_eq!(billing.raw_usage.output_tokens, 1200);
    assert_eq!(billing.shaped_usage.output_tokens, 1200);
    assert_eq!(
        billing.reported_usage.output_tokens,
        uplift_tokens(1200, 50)
    );
    assert!(billing.uplifted_cost_usd > billing.shaped_cost_usd);
    assert!((billing.reported_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
    assert!((billing.billable_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
}

#[test]
fn sse_usage_projection_preserves_delimiters_and_done_events() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

data: [DONE]

"#;
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0);
    let projected = rewrite_sse_event_usage(event, projection.as_ref(), None);
    let text = String::from_utf8(projected).expect("utf8");

    assert!(text.contains("data: [DONE]"));
    assert!(text.contains("\n\n"));
    assert!(!text.contains(r#""input_tokens":100000"#));
}

#[test]
fn sse_usage_projection_shapes_uncached_stream_usage_by_path_policy() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":4165,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}

"#;
    let mut route = test_route("claude-opus-4-6");
    payload_mut(&mut route).stream = true;
    route.request_input_tokens = 4165;
    route.prompt_cache_target_read_ratio = 0.5;
    route.reported_usage.path_overrides.insert(
        "/cc".to_string(),
        ReportedUsagePathPolicy {
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        },
    );
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));

    let projected = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));

    assert_ne!(projected, event);
    let value = event_data_value(&projected);
    let usage = &value["usage"];
    assert_eq!(usage["input_tokens"].as_i64().expect("projected input"), 1);
    assert_eq!(
        usage["output_tokens"].as_i64().expect("projected output"),
        2
    );
    assert_eq!(
        usage["cache_read_input_tokens"]
            .as_i64()
            .expect("projected cache read"),
        0
    );
    assert_eq!(
        usage["cache_creation_input_tokens"]
            .as_i64()
            .expect("projected cache creation"),
        4164
    );
    assert_projected_cache_creation_consistent(usage);
    let capture = capture.lock().clone();
    assert!(capture.projected);
    assert_eq!(capture.raw.map(|usage| usage.input_tokens), Some(4165));
    let reported = capture.reported.expect("reported usage");
    assert_eq!(reported.input_tokens, 1);
    assert_eq!(reported.cache_creation_input_tokens, 4164);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn sse_usage_projection_captures_raw_and_reported_usage() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let _projected = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let capture = capture.lock().clone();
    let raw = capture.raw.expect("raw usage");
    let reported = capture.reported.expect("reported usage");

    assert_eq!(raw.input_tokens, 100000);
    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert!(reported.cache_creation_input_tokens > 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn sse_usage_projection_rewrites_nested_5m_cache_creation_split() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":0,"output_tokens":62,"cache_creation_input_tokens":1300180,"cache_read_input_tokens":37,"cache_creation":{"ephemeral_5m_input_tokens":1300180,"ephemeral_1h_input_tokens":0}}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let route = test_route("claude-opus-4-8");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");

    let projected = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let text = std::str::from_utf8(&projected).expect("projected utf8");
    assert!(!text.contains("1300180"));

    let value = event_data_value(&projected);
    let usage = &value["usage"];
    assert_projected_cache_creation_consistent(usage);
    assert!(
        usage["cache_creation_input_tokens"]
            .as_i64()
            .expect("projected aggregate")
            < 1_300_180
    );

    let capture = capture.lock().clone();
    let raw = capture.raw.expect("raw usage");
    assert_eq!(raw.cache_creation_input_tokens, 1_300_180);
    assert_eq!(raw.cache_creation_5m_input_tokens, 1_300_180);
    assert_eq!(raw.cache_creation_1h_input_tokens, 0);
    let reported = capture.reported.expect("reported usage");
    assert_eq!(
        reported.cache_creation_input_tokens,
        usage["cache_creation_input_tokens"].as_i64().unwrap() as i32
    );
}

#[test]
fn sse_usage_projection_rewrites_nested_1h_cache_creation_split() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":0,"output_tokens":135,"cache_creation_input_tokens":1998336,"cache_read_input_tokens":17,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":1998336}}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let mut route = test_route("claude-opus-4-8");
    payload_mut(&mut route).stream = true;
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");

    let projected = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let text = std::str::from_utf8(&projected).expect("projected utf8");
    assert!(!text.contains("1998336"));

    let value = event_data_value(&projected);
    let usage = &value["usage"];
    assert_projected_cache_creation_consistent(usage);
    assert!(
        usage["cache_creation_input_tokens"]
            .as_i64()
            .expect("projected aggregate")
            < 1_998_336
    );
    assert_eq!(
        usage["cache_creation"]["ephemeral_1h_input_tokens"]
            .as_i64()
            .expect("projected 1h cache creation"),
        0,
        "external upstream 1h must not leak when the request did not ask for ttl=1h"
    );
    assert_eq!(
        usage["cache_creation"]["ephemeral_5m_input_tokens"]
            .as_i64()
            .expect("projected 5m cache creation"),
        usage["cache_creation_input_tokens"]
            .as_i64()
            .expect("projected aggregate"),
        "default projected cache creation should stay in the 5m bucket"
    );

    let capture = capture.lock().clone();
    let raw = capture.raw.expect("raw usage");
    assert_eq!(raw.cache_creation_input_tokens, 1_998_336);
    assert_eq!(raw.cache_creation_5m_input_tokens, 0);
    assert_eq!(raw.cache_creation_1h_input_tokens, 1_998_336);
}

#[test]
fn sse_usage_projection_uses_request_ttl_for_1h_cache_creation_split() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":0,"output_tokens":135,"cache_creation_input_tokens":1300180,"cache_read_input_tokens":17,"cache_creation":{"ephemeral_5m_input_tokens":1300180,"ephemeral_1h_input_tokens":0}}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let mut route = test_route("claude-opus-4-8");
    payload_mut(&mut route).stream = true;
    payload_mut(&mut route).system = Some(vec![SystemMessage {
        text: "stable external ttl one hour prompt ".repeat(8_000),
        cache_control: Some(serde_json::json!({"type": "ephemeral", "ttl": "1h"})),
    }]);
    route.request_input_tokens = count_external_route_input_tokens(payload_ref(&route));
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");

    let projected = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let text = std::str::from_utf8(&projected).expect("projected utf8");
    assert!(!text.contains("1300180"));

    let value = event_data_value(&projected);
    let usage = &value["usage"];
    assert_projected_cache_creation_consistent(usage);
    let aggregate = usage["cache_creation_input_tokens"]
        .as_i64()
        .expect("projected aggregate");
    assert!(aggregate > 0);
    assert!(
        aggregate < 1_300_180,
        "projected cache creation should follow the local path policy"
    );
    assert_eq!(
        usage["cache_creation"]["ephemeral_1h_input_tokens"]
            .as_i64()
            .expect("projected 1h cache creation"),
        aggregate,
        "explicit request ttl=1h should put projected creation in the 1h bucket"
    );
    assert_eq!(
        usage["cache_creation"]["ephemeral_5m_input_tokens"]
            .as_i64()
            .expect("projected 5m cache creation"),
        0
    );

    let capture = capture.lock().clone();
    let raw = capture.raw.expect("raw usage");
    assert_eq!(raw.cache_creation_input_tokens, 1_300_180);
    assert_eq!(raw.cache_creation_5m_input_tokens, 1_300_180);
    assert_eq!(raw.cache_creation_1h_input_tokens, 0);
    let reported = capture.reported.expect("reported usage");
    assert_eq!(reported.cache_creation_input_tokens as i64, aggregate);
    assert_eq!(reported.cache_creation_1h_input_tokens as i64, aggregate);
    assert_eq!(reported.cache_creation_5m_input_tokens, 0);
    assert!(capture.projected);
}

#[test]
fn sse_usage_projection_handles_nested_only_cache_creation_split() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":0,"output_tokens":62,"cache_creation_input_tokens":0,"cache_read_input_tokens":37,"cache_creation":{"ephemeral_5m_input_tokens":1300180,"ephemeral_1h_input_tokens":0}}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let route = test_route("claude-opus-4-8");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");

    let projected = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let text = std::str::from_utf8(&projected).expect("projected utf8");
    assert!(!text.contains("1300180"));
    assert_ne!(projected, event);

    let value = event_data_value(&projected);
    assert_projected_cache_creation_consistent(&value["usage"]);

    let capture = capture.lock().clone();
    let raw = capture.raw.expect("raw usage");
    assert_eq!(raw.cache_creation_input_tokens, 1_300_180);
    assert_eq!(raw.cache_creation_5m_input_tokens, 1_300_180);
}

#[test]
fn sse_event_passthrough_keeps_nested_usage_when_projection_disabled() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":0,"output_tokens":62,"cache_creation_input_tokens":1300180,"cache_read_input_tokens":37,"cache_creation":{"ephemeral_5m_input_tokens":1300180,"ephemeral_1h_input_tokens":0}}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
        ..ExternalUsageCapture::default()
    }));

    let passthrough = process_sse_event_with_plan(
        event,
        None,
        Some(&capture),
        None,
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );

    assert_eq!(passthrough, event);
    let capture = capture.lock().clone();
    assert!(!capture.projected);
    let raw = capture.raw.expect("raw usage");
    assert_eq!(raw.cache_creation_input_tokens, 1_300_180);
    assert_eq!(raw.cache_creation_5m_input_tokens, 1_300_180);
}

#[test]
fn sse_message_start_usage_is_rewritten_without_committing_cache_state() {
    let event = br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_fake","type":"message","role":"assistant","content":[],"model":"fake-sonnet","stop_reason":null,"usage":{"input_tokens":100000,"output_tokens":0,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}}

"#;
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");
    let projected = rewrite_sse_event_usage(event, Some(&projection), None);
    let text = std::str::from_utf8(&projected).expect("projected utf8");

    assert!(text.contains(r#""type":"message_start""#));
    assert!(!text.contains(r#""input_tokens":100000"#));
    assert!(text.contains(r#""cache_read_input_tokens":0"#));
    assert!(text.contains(r#""output_tokens":0"#));

    let mut second_route = route.clone();
    payload_mut(&mut second_route).messages.extend([
        Message {
            role: "assistant".to_string(),
            content: serde_json::json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: serde_json::json!("continue after start event only"),
        },
    ]);
    let second_projection = projection_context(&second_route, &pool, 0).expect("second projection");
    let mut final_usage = serde_json::json!({
        "input_tokens": 100000,
        "output_tokens": 1,
        "cache_creation_input_tokens": 50000,
        "cache_read_input_tokens": 0
    });
    let final_projected = project_usage_value(&mut final_usage, Some(&second_projection), true)
        .expect("final projected usage");
    assert_eq!(final_projected.reported.cache_read_input_tokens, 0);
}

#[test]
fn sse_event_passthrough_rewrites_message_start_usage_when_projection_enabled() {
    let event = br#"event: message_start
data: {"type":"message_start","message":{"id":"msg_fake","type":"message","role":"assistant","content":[],"model":"fake-sonnet","stop_reason":null,"usage":{"input_tokens":100000,"output_tokens":0,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
        ..ExternalUsageCapture::default()
    }));
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");

    let passthrough = process_sse_event_with_plan(
        event,
        Some(&projection),
        Some(&capture),
        None,
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );

    assert_ne!(passthrough, event);
    let text = std::str::from_utf8(&passthrough).expect("rewritten utf8");
    assert!(!text.contains(r#""input_tokens":100000"#));
    let capture = capture.lock().clone();
    assert!(capture.projected);
    assert_eq!(capture.raw.expect("raw").input_tokens, 100000);
    let reported = capture.reported.expect("reported usage");
    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.output_tokens, 0);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn sse_event_passthrough_rewrites_usage_when_projection_enabled() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
        ..ExternalUsageCapture::default()
    }));
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");

    let passthrough = process_sse_event_with_plan(
        event,
        Some(&projection),
        Some(&capture),
        None,
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );

    assert_ne!(passthrough, event);
    let text = std::str::from_utf8(&passthrough).expect("rewritten utf8");
    assert!(!text.contains(r#""input_tokens":100000"#));
    let capture = capture.lock().clone();
    assert!(capture.projected);
    assert_eq!(
        capture.stream_response_mode,
        Some(ExternalPoolStreamResponseMode::EventPassthrough)
    );
    assert_eq!(capture.raw.expect("raw").input_tokens, 100000);
    let reported = capture.reported.expect("reported usage");
    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert!(reported.cache_creation_input_tokens > 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn sse_event_passthrough_keeps_usage_body_when_projection_disabled() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
        stream_response_mode: Some(ExternalPoolStreamResponseMode::EventPassthrough),
        ..ExternalUsageCapture::default()
    }));

    let passthrough = process_sse_event_with_plan(
        event,
        None,
        Some(&capture),
        None,
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );

    assert_eq!(passthrough, event);
    let capture = capture.lock().clone();
    assert!(!capture.projected);
    assert_eq!(capture.raw.expect("raw").input_tokens, 100000);
    assert_eq!(capture.reported.expect("reported").input_tokens, 100000);
}

#[test]
fn stream_processing_plan_inherits_global_and_allows_pool_override() {
    let config = ExternalPoolsConfig {
        external_pool_stream_response_mode: ExternalPoolStreamResponseMode::EventPassthrough,
        ..ExternalPoolsConfig::default()
    };

    let inherited = test_pool("http://pool.example.com", false);
    assert_eq!(
        ExternalStreamProcessingPlan::for_pool(&inherited, &config).response_mode,
        ExternalPoolStreamResponseMode::EventPassthrough
    );

    let mut overridden = inherited.clone();
    overridden.stream_response_mode = Some(ExternalPoolStreamResponseMode::EventPassthrough);
    let plan = ExternalStreamProcessingPlan::for_pool(&overridden, &config);
    assert_eq!(
        plan.response_mode,
        ExternalPoolStreamResponseMode::EventPassthrough
    );
    assert!(plan.capture_usage);
}

#[test]
fn stream_passthrough_does_not_rewrite_usage_when_projection_is_disabled() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let out = process_sse_event_with_plan(
        event,
        None,
        Some(&capture),
        None,
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );

    assert_eq!(out, event);
    let capture = capture.lock().clone();
    assert!(!capture.projected);
    assert_eq!(capture.raw.expect("raw").input_tokens, 100000);
    assert_eq!(capture.reported.expect("reported").input_tokens, 100000);
}

#[test]
fn drain_sse_events_respects_processing_plan() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
    let route = test_route("claude-sonnet-4-5");
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection = projection_context(&route, &pool, 0).expect("projection");

    let mut rewrite_buffer = event.to_vec();
    let rewritten = drain_sse_events(
        &mut rewrite_buffer,
        Some(&projection),
        None,
        None,
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );
    assert!(rewrite_buffer.is_empty());
    assert_ne!(rewritten, event);
    let rewritten_text = std::str::from_utf8(&rewritten).expect("rewritten utf8");
    assert!(!rewritten_text.contains(r#""input_tokens":100000"#));

    let mut passthrough_buffer = event.to_vec();
    let rewritten_capture_mode = drain_sse_events(
        &mut passthrough_buffer,
        Some(&projection),
        None,
        None,
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough),
    );
    assert!(passthrough_buffer.is_empty());
    assert_ne!(rewritten_capture_mode, event);
    let capture_text = std::str::from_utf8(&rewritten_capture_mode).expect("rewritten utf8");
    assert!(!capture_text.contains(r#""input_tokens":100000"#));
}

#[test]
fn sse_usage_projection_applies_output_uplift_to_reported_usage() {
    let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1200,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let mut route = test_route("claude-sonnet-4-5");
    route.endpoint = "/v1/messages".to_string();
    disable_path_output_postprocess(&mut route);
    let mut pool = test_pool("http://pool.example.com", false);
    pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
    let projection =
        projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
    let projected = rewrite_sse_event_usage(event, Some(&projection), Some(&capture));
    let text = std::str::from_utf8(&projected).expect("projected sse");
    assert!(text.contains(r#""output_tokens":1800"#));

    let capture = capture.lock().clone();
    let shaped = capture.shaped.expect("shaped usage");
    let reported = capture.reported.expect("reported usage");
    assert_eq!(shaped.output_tokens, 1200);
    assert_eq!(reported.output_tokens, uplift_tokens(1200, 50));
}

#[test]
fn finds_sse_event_delimiters_for_lf_and_crlf() {
    assert_eq!(find_sse_event_delimiter(b"data: {}\n\nrest"), Some((8, 2)));
    assert_eq!(
        find_sse_event_delimiter(b"data: {}\r\n\r\nrest"),
        Some((8, 4))
    );
    assert_eq!(find_sse_event_delimiter(b"data: {}"), None);
}

fn transcript_sse_event(event: &str, value: serde_json::Value) -> Vec<u8> {
    format!("event: {event}\ndata: {value}\n\n").into_bytes()
}

fn transcript_multiline_crlf_sse_event(event: &str, value: serde_json::Value) -> Vec<u8> {
    let pretty = serde_json::to_string_pretty(&value).unwrap();
    let mut output = format!("event: {event}\r\n");
    for line in pretty.lines() {
        output.push_str("data: ");
        output.push_str(line);
        output.push_str("\r\n");
    }
    output.push_str("\r\n");
    output.into_bytes()
}

fn process_external_transcript_events(events: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let mut state = ExternalAnthropicTranscriptState::new(["Bash".to_string()]);
    let mut output = Vec::new();
    for event in events {
        output.extend(state.process(&event));
    }
    output.extend(state.finish());
    output
}

fn process_external_transcript_stream_one_byte_at_a_time(stream: &[u8]) -> Vec<u8> {
    let mut state = ExternalAnthropicTranscriptState::new(["Bash".to_string()]);
    let mut buffer = Vec::new();
    let mut output = Vec::new();
    let plan =
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough);
    for byte in stream {
        buffer.push(*byte);
        output.extend(drain_sse_events_with_transcript(
            &mut buffer,
            None,
            None,
            None,
            plan,
            Some(&mut state),
        ));
    }
    assert!(buffer.is_empty());
    output.extend(state.finish());
    output
}

#[test]
fn external_sse_text_pollution_fails_closed_before_or_after_visible_output() {
    let polluted = "user Continue\n\nBash: hidden";
    for _round in 0..5 {
        let same_event = vec![
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":format!("same-event prefix\n{polluted}")}}),
            ),
            transcript_sse_event(
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            ),
            transcript_sse_event("message_stop", json!({"type":"message_stop"})),
        ];
        let same_event = String::from_utf8(process_external_transcript_events(same_event)).unwrap();
        assert!(same_event.contains(r#""type":"error""#));
        assert!(!same_event.contains("same-event prefix"));
        assert!(!same_event.contains("user Continue"));
        assert!(!same_event.contains("hidden"));
        assert!(!same_event.contains("message_stop"));
        assert!(!same_event.contains("stop_reason"));

        let after_visible = vec![
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"already visible\n"}}),
            ),
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":polluted}}),
            ),
            transcript_sse_event("message_stop", json!({"type":"message_stop"})),
        ];
        let after_visible =
            String::from_utf8(process_external_transcript_events(after_visible)).unwrap();
        assert!(after_visible.contains("already visible"));
        assert!(after_visible.contains(r#""type":"error""#));
        assert!(!after_visible.contains("user Continue"));
        assert!(!after_visible.contains("hidden"));
        assert!(!after_visible.contains("message_stop"));

        let embedded_start = vec![
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":polluted}}),
            ),
            transcript_sse_event("message_stop", json!({"type":"message_stop"})),
        ];
        let embedded_start =
            String::from_utf8(process_external_transcript_events(embedded_start)).unwrap();
        assert!(embedded_start.contains(r#""type":"error""#));
        assert!(!embedded_start.contains("user Continue"));
        assert!(!embedded_start.contains("hidden"));
        assert!(!embedded_start.contains("message_stop"));
    }
}

#[test]
fn external_sse_partial_transcript_at_tool_boundary_fails_before_tool_forwarding() {
    for _round in 0..5 {
        let events = vec![
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ),
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"user Continue\n\nBa"}}),
            ),
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}}),
            ),
            transcript_sse_event("message_stop", json!({"type":"message_stop"})),
        ];
        let output = String::from_utf8(process_external_transcript_events(events)).unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains("toolu_1"));
        assert!(!output.contains(r#""type":"tool_use""#));
        assert!(!output.contains("message_stop"));
    }
}

#[test]
fn external_sse_eof_contamination_emits_error_and_records_stream_failure() {
    for _round in 0..5 {
        let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
        let mut state = ExternalAnthropicTranscriptState::new_with_error_context(
            ["Bash".to_string()],
            Some("req_eof_contamination".to_string()),
            Some("err_eof_contamination".to_string()),
        );
        let start = transcript_sse_event(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        );
        let partial = transcript_sse_event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"user Continue\n\nBa"}}),
        );
        let mut output = process_external_transcript_state(&mut state, &start, Some(&capture));
        output.extend(process_external_transcript_state(
            &mut state,
            &partial,
            Some(&capture),
        ));
        output.extend(finish_external_transcript_state(&mut state, Some(&capture)));

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(output.contains("req_eof_contamination"));
        assert!(output.contains("err_eof_contamination"));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains(RESPONSE_PROTOCOL_CONTAMINATION_DETAIL));
        assert!(!output.contains("message_stop"));
        assert_eq!(
            capture.lock().stream_error_message.as_deref(),
            Some(RESPONSE_PROTOCOL_CONTAMINATION_DETAIL)
        );
    }
}

#[test]
fn external_sse_signed_thinking_is_atomic_across_character_deltas() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let mut events = vec![transcript_sse_event(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        )];
        events.extend(polluted.chars().map(|ch| {
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":ch.to_string()}}),
            )
        }));
        events.push(transcript_sse_event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-signature"}}),
        ));
        events.push(transcript_sse_event(
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ));
        events.push(transcript_sse_event(
            "content_block_start",
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        ));
        events.push(transcript_sse_event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"visible"}}),
        ));
        events.push(transcript_sse_event(
            "content_block_stop",
            json!({"type":"content_block_stop","index":1}),
        ));
        events.push(transcript_sse_event(
            "message_delta",
            json!({"type":"message_delta","usage":{"output_tokens":20,"output_tokens_details":{"thinking_tokens":12}}}),
        ));

        let output = String::from_utf8(process_external_transcript_events(events)).unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains("opaque-signature"));
        assert!(!output.contains(r#""type":"thinking""#));
        assert!(!output.contains("thinking_tokens"));
        assert!(!output.contains("visible"));
        assert!(!output.contains("message_stop"));
    }
}

#[test]
fn external_sse_multiline_crlf_json_is_sanitized_across_one_byte_transport_chunks() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let stream = [
            transcript_multiline_crlf_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            transcript_multiline_crlf_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":polluted}}),
            ),
            transcript_multiline_crlf_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-signature"}}),
            ),
            transcript_multiline_crlf_sse_event(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
            transcript_multiline_crlf_sse_event(
                "message_delta",
                json!({"type":"message_delta","usage":{"output_tokens":20,"output_tokens_details":{"thinking_tokens":12}}}),
            ),
        ]
        .concat();
        let output = String::from_utf8(process_external_transcript_stream_one_byte_at_a_time(
            &stream,
        ))
        .unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains("Bash: hidden"));
        assert!(!output.contains("opaque-signature"));
        assert!(!output.contains(r#""type":"thinking""#));
        assert!(!output.contains("thinking_tokens"));
        assert!(!output.contains("message_stop"));
    }
}

#[test]
fn external_sse_multiline_crlf_unsigned_pollution_fails_closed() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let stream = [
            transcript_multiline_crlf_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            transcript_multiline_crlf_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":polluted}}),
            ),
            transcript_multiline_crlf_sse_event(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
        ]
        .concat();
        let output = String::from_utf8(process_external_transcript_stream_one_byte_at_a_time(
            &stream,
        ))
        .unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(!output.contains(r#""type":"thinking""#));
        assert!(!output.contains(r#""type":"thinking_delta""#));
        assert!(!output.contains("safe prefix"));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains("Bash: hidden"));
        assert!(!output.contains("message_stop"));
    }
}

#[test]
fn external_sse_unsigned_thinking_pollution_fails_closed() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let mut events = vec![transcript_sse_event(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        )];
        events.extend(polluted.chars().map(|ch| {
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":ch.to_string()}}),
            )
        }));
        events.push(transcript_sse_event(
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ));

        let output = String::from_utf8(process_external_transcript_events(events)).unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(!output.contains(r#""type":"thinking""#));
        assert!(!output.contains(r#""type":"thinking_delta""#));
        assert!(!output.contains("safe prefix"));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains("Bash: hidden"));
        assert!(!output.contains(r#""type":"text_delta""#));
        assert!(!output.contains("message_stop"));
    }
}

#[test]
fn external_sse_start_embedded_thinking_cannot_bypass_sanitization() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let events = vec![
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":polluted}}),
            ),
            transcript_sse_event(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
        ];
        let output = String::from_utf8(process_external_transcript_events(events)).unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(!output.contains(r#""type":"thinking""#));
        assert!(!output.contains("safe prefix"));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains("Bash: hidden"));
        assert!(!output.contains("message_stop"));
    }
}

#[test]
fn external_sse_orphan_thinking_and_signature_deltas_fail_closed_on_pollution() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let events = vec![
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":polluted}}),
            ),
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":polluted}}),
            ),
            transcript_sse_event(
                "message_delta",
                json!({"type":"message_delta","usage":{"output_tokens":20,"output_tokens_details":{"thinking_tokens":12}}}),
            ),
        ];
        let output = String::from_utf8(process_external_transcript_events(events)).unwrap();
        assert!(output.contains(r#""type":"error""#));
        assert!(!output.contains("safe prefix"));
        assert!(!output.contains("user Continue"));
        assert!(!output.contains("Bash: hidden"));
        assert!(!output.contains("signature_delta"));
        assert!(!output.contains("thinking_tokens"));
        assert!(!output.contains("message_stop"));
    }
}

#[test]
fn external_sse_redacted_leak_is_suppressed_and_clean_signed_block_is_identical() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let redacted = vec![
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":polluted}}),
            ),
            transcript_sse_event(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
        ];
        let redacted = String::from_utf8(process_external_transcript_events(redacted)).unwrap();
        assert!(redacted.contains(r#""type":"error""#));
        assert!(!redacted.contains("redacted_thinking"));
        assert!(!redacted.contains("user Continue"));
        assert!(!redacted.contains("safe prefix"));
        assert!(!redacted.contains("message_stop"));

        let clean = vec![
            transcript_sse_event(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            ),
            transcript_sse_event("ping", json!({"type":"ping"})),
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"ordinary reasoning"}}),
            ),
            transcript_sse_event(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque-signature"}}),
            ),
            transcript_sse_event(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ),
        ];
        let mut expected = clean[1].clone();
        expected.extend(clean[0].clone());
        expected.extend(clean[2..].concat());
        assert_eq!(process_external_transcript_events(clean), expected);
    }
}

#[test]
fn external_sse_atomic_thinking_buffer_is_bounded() {
    let oversized = "x".repeat(EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES + 1);
    let events = vec![
        transcript_sse_event(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        ),
        transcript_sse_event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":oversized}}),
        ),
        transcript_sse_event(
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
    ];
    let output = String::from_utf8(process_external_transcript_events(events)).unwrap();
    assert!(!output.contains(&"x".repeat(1024)));
    assert!(output.contains(r#""type":"error""#));
    assert!(!output.contains("message_stop"));
}

#[test]
fn external_sse_atomic_overflow_records_stream_failure_and_stops_terminal_success() {
    let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
    let mut state = ExternalAnthropicTranscriptState::new_with_error_context(
        ["Bash".to_string()],
        Some("req_test".to_string()),
        Some("err_test".to_string()),
    );
    let plan =
        ExternalStreamProcessingPlan::from_mode(ExternalPoolStreamResponseMode::EventPassthrough);
    let events = vec![
        transcript_sse_event(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        ),
        transcript_sse_event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"x".repeat(EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES + 1)}}),
        ),
        transcript_sse_event(
            "message_delta",
            json!({"type":"message_delta","usage":{"output_tokens":20,"output_tokens_details":{"thinking_tokens":12}}}),
        ),
        transcript_sse_event("message_stop", json!({"type":"message_stop"})),
    ];
    let mut output = Vec::new();
    for event in events {
        output.extend(process_sse_event_with_plan_and_transcript(
            &event,
            None,
            Some(&capture),
            None,
            plan,
            Some(&mut state),
        ));
    }
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains(r#""type":"error""#));
    assert!(output.contains("req_test"));
    assert!(output.contains("err_test"));
    assert!(!output.contains("thinking_tokens"));
    assert!(!output.contains("message_stop"));
    assert_eq!(
        capture.lock().stream_error_message.as_deref(),
        Some("external thinking block exceeded bounded atomic buffer")
    );
}
