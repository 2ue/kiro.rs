use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::Response,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::{
    anthropic::{
        cache::{CacheUsage, RawUsage, ReportedCacheUsagePolicy},
        envelope,
        types::MessagesRequest,
        usage::{
            ExternalPoolAttempt, UsageRecord, UsageRecordStatus, UsageRouteKind, UsageRouteSubtype,
            UsageSource,
        },
    },
    model::config::{ExternalPoolsConfig, ReportedUsageConfig},
    storage::{postgres::PostgresStore, redis_cache::RedisStore},
    token,
};

const DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS: u64 = 180;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolAuthType {
    Bearer,
    XApiKey,
}

impl Default for ExternalPoolAuthType {
    fn default() -> Self {
        Self::Bearer
    }
}

impl ExternalPoolAuthType {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "x_api_key" | "x-api-key" | "xapikey" => Self::XApiKey,
            _ => Self::Bearer,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::XApiKey => "x_api_key",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolUsageProjectionMode {
    PassThrough,
    CurrentPathPolicy,
}

impl Default for ExternalPoolUsageProjectionMode {
    fn default() -> Self {
        Self::PassThrough
    }
}

impl ExternalPoolUsageProjectionMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "current_path_policy" => Self::CurrentPathPolicy,
            _ => Self::PassThrough,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PassThrough => "pass_through",
            Self::CurrentPathPolicy => "current_path_policy",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolAutoDisablePolicy {
    Inherit,
    Disabled,
    Enabled,
}

impl Default for ExternalPoolAutoDisablePolicy {
    fn default() -> Self {
        Self::Inherit
    }
}

impl ExternalPoolAutoDisablePolicy {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Self::Disabled,
            "enabled" => Self::Enabled,
            _ => Self::Inherit,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPool {
    pub id: u64,
    pub name: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masked_api_key: Option<String>,
    pub auth_type: ExternalPoolAuthType,
    pub enabled: bool,
    pub priority: i32,
    pub max_concurrent_requests: u32,
    pub usage_projection_mode: ExternalPoolUsageProjectionMode,
    pub auto_disable_policy: ExternalPoolAutoDisablePolicy,
    pub auto_disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_last_error: Option<String>,
    pub preserve_path: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExternalPool {
    pub fn is_auto_disabled_now(&self) -> bool {
        if !self.auto_disabled {
            return false;
        }
        self.auto_disabled_until
            .map(|until| until > Utc::now())
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateExternalPoolRequest {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub auth_type: ExternalPoolAuthType,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_external_pool_concurrency")]
    pub max_concurrent_requests: u32,
    #[serde(default)]
    pub usage_projection_mode: ExternalPoolUsageProjectionMode,
    #[serde(default)]
    pub auto_disable_policy: ExternalPoolAutoDisablePolicy,
    #[serde(default = "default_true")]
    pub preserve_path: bool,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateExternalPoolRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_type: Option<ExternalPoolAuthType>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub usage_projection_mode: Option<ExternalPoolUsageProjectionMode>,
    #[serde(default)]
    pub auto_disable_policy: Option<ExternalPoolAutoDisablePolicy>,
    #[serde(default)]
    pub preserve_path: Option<bool>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetExternalPoolEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolStatus {
    pub pool: ExternalPool,
    pub in_flight: u32,
    pub cooldown_remaining_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
    pub dispatchable: bool,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolsListResponse {
    pub pools: Vec<ExternalPool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolsStatusResponse {
    pub pools: Vec<ExternalPoolStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolTestResponse {
    pub ok: bool,
    pub status: Option<u16>,
    pub message: String,
}

#[derive(Clone)]
pub struct ExternalRouteRequest {
    pub raw_body: Bytes,
    pub headers: HeaderMap,
    pub endpoint: &'static str,
    pub payload: MessagesRequest,
    pub route_subtype: UsageRouteSubtype,
    pub fallback_reason: Option<String>,
    pub direct_policy_reason: Option<String>,
    pub local_attempted: bool,
    pub local_preflight: Option<serde_json::Value>,
    pub local_attempts: Vec<crate::kiro::call_trace::KiroCredentialAttempt>,
    pub reported_usage: ReportedUsageConfig,
    pub request_id: String,
    pub recorder: Arc<crate::anthropic::usage::UsageRecorder>,
    pub started_at: Instant,
}

#[derive(Debug, Clone)]
struct PoolRuntimeState {
    cooldown_until: Option<DateTime<Utc>>,
    cooldown_reason: Option<String>,
}

#[derive(Clone)]
pub struct ExternalPoolManager {
    postgres: Arc<PostgresStore>,
    redis: Arc<RedisStore>,
    client: reqwest::Client,
    local_state: Arc<Mutex<HashMap<u64, PoolRuntimeState>>>,
}

struct ExternalPoolLease {
    manager: ExternalPoolManager,
    pool_id: u64,
    lease_id: u64,
}

impl Drop for ExternalPoolLease {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let pool_id = self.pool_id;
        let lease_id = self.lease_id;
        tokio::spawn(async move {
            manager.release_pool(pool_id, lease_id).await;
        });
    }
}

#[derive(Debug, Clone)]
struct ExternalPoolError {
    status: Option<StatusCode>,
    message: String,
    retryable: bool,
    auto_disable_reason: Option<String>,
    cooldown: Option<(Duration, String)>,
    response_body: Option<Bytes>,
    response_headers: HeaderMap,
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalPoolCooldownState {
    until: DateTime<Utc>,
    reason: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_priority() -> i32 {
    100
}

fn default_external_pool_concurrency() -> u32 {
    10
}

pub fn mask_external_pool_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= 10 {
        return "***".to_string();
    }
    format!(
        "{}...{}",
        &trimmed[..6],
        &trimmed[trimmed.len().saturating_sub(4)..]
    )
}

impl ExternalPoolManager {
    pub fn new(postgres: Arc<PostgresStore>, redis: Arc<RedisStore>) -> Self {
        Self {
            postgres,
            redis,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(
                    DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS,
                ))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            local_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn status(
        &self,
        config: &ExternalPoolsConfig,
    ) -> anyhow::Result<Vec<ExternalPoolStatus>> {
        let pools = self.postgres.list_external_pools(true).await?;
        let mut statuses = Vec::with_capacity(pools.len());
        for pool in pools {
            let (in_flight, cooldown_remaining_secs, cooldown_reason) =
                self.pool_runtime_snapshot(pool.id).await;
            let skipped_reason =
                self.skip_reason(&pool, in_flight, cooldown_remaining_secs, config);
            statuses.push(ExternalPoolStatus {
                dispatchable: skipped_reason.is_none(),
                pool,
                in_flight,
                cooldown_remaining_secs,
                cooldown_reason,
                skipped_reason,
            });
        }
        Ok(statuses)
    }

    pub async fn has_available_pool(&self, config: &ExternalPoolsConfig) -> bool {
        if !config.external_pools_enabled {
            return false;
        }
        self.select_pool(&HashSet::new(), config).await.is_some()
    }

    pub fn direct_policy_reason(
        &self,
        config: &ExternalPoolsConfig,
        endpoint: &str,
        model: &str,
    ) -> Option<String> {
        if !config.external_pools_enabled || !config.external_direct_policy_enabled {
            return None;
        }
        if config.direct_external_on_local_maintenance {
            return Some("local_maintenance".to_string());
        }
        if config
            .direct_external_model_rules
            .iter()
            .any(|rule| rule_matches(rule, model))
        {
            return Some(format!("model_rule:{}", model));
        }
        if config
            .direct_external_path_rules
            .iter()
            .any(|rule| rule_matches(rule, endpoint))
        {
            return Some(format!("path_rule:{}", endpoint));
        }
        None
    }

    pub async fn forward_with_failover(
        &self,
        config: ExternalPoolsConfig,
        route: ExternalRouteRequest,
    ) -> Response {
        if !config.external_pools_enabled {
            self.record_external_failure(
                &route,
                None,
                Vec::new(),
                "external_pool_disabled",
                "external pools are disabled",
            );
            return envelope::error_response_with_id(
                StatusCode::SERVICE_UNAVAILABLE,
                "external_pool_unavailable",
                "external pools are disabled",
                &route.request_id,
            );
        }

        let enabled_count = self
            .postgres
            .list_external_pools(false)
            .await
            .map(|pools| {
                pools
                    .iter()
                    .filter(|pool| pool.enabled && !pool.is_auto_disabled_now())
                    .count()
            })
            .unwrap_or(0);
        let max_attempts = if config.external_pool_retry_max_attempts == 0 {
            enabled_count.max(1)
        } else {
            config.external_pool_retry_max_attempts as usize
        };

        let mut excluded = HashSet::new();
        let mut attempts = Vec::new();
        let mut last_error: Option<(ExternalPool, ExternalPoolError)> = None;

        for attempt_index in 0..max_attempts {
            let Some(pool) = self.select_pool(&excluded, &config).await else {
                break;
            };
            let pool_id = pool.id;
            let lease = match self.acquire_pool(&pool, &config).await {
                Some(lease) => lease,
                None => {
                    excluded.insert(pool_id);
                    continue;
                }
            };
            let started = std::time::Instant::now();
            let result = self.forward_once(&pool, &route, lease, &config).await;
            match result {
                Ok(response) => {
                    attempts.push(ExternalPoolAttempt {
                        attempt: attempt_index.saturating_add(1) as u32,
                        pool_id,
                        pool_name: pool.name.clone(),
                        status: Some(response.status().as_u16()),
                        action: "success".to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: None,
                        error_message: None,
                    });
                    self.record_external_success(&route, &pool, attempts.clone());
                    return response;
                }
                Err(err) => {
                    let action = if err.retryable { "retry_next" } else { "fail" };
                    attempts.push(ExternalPoolAttempt {
                        attempt: attempt_index.saturating_add(1) as u32,
                        pool_id,
                        pool_name: pool.name.clone(),
                        status: err.status.map(|status| status.as_u16()),
                        action: action.to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: Some(error_type_for_external_error(&err).to_string()),
                        error_message: Some(err.message.clone()),
                    });
                    if let Some((duration, reason)) = &err.cooldown {
                        self.mark_pool_cooldown(pool_id, *duration, reason.clone())
                            .await;
                    }
                    if let Some(reason) = &err.auto_disable_reason {
                        self.auto_disable_pool_if_configured(&pool, &config, reason, &err.message)
                            .await;
                    }
                    if err.retryable {
                        excluded.insert(pool_id);
                        last_error = Some((pool, err));
                        continue;
                    }
                    let error_type = error_type_for_external_error(&err);
                    self.record_external_failure(
                        &route,
                        Some(&pool),
                        attempts,
                        &error_type,
                        &err.message,
                    );
                    return external_pool_error_response(&route, &err);
                }
            }
        }

        if let Some((pool, err)) = last_error {
            let error_type = error_type_for_external_error(&err);
            self.record_external_failure(&route, Some(&pool), attempts, &error_type, &err.message);
            return external_pool_error_response(&route, &err);
        }

        self.record_external_failure(
            &route,
            None,
            attempts,
            "external_pool_unavailable",
            "No available external fallback pools",
        );
        envelope::error_response_with_id(
            StatusCode::SERVICE_UNAVAILABLE,
            "external_pool_unavailable",
            "No available external fallback pools",
            &route.request_id,
        )
    }

    async fn forward_once(
        &self,
        pool: &ExternalPool,
        route: &ExternalRouteRequest,
        lease: ExternalPoolLease,
        config: &ExternalPoolsConfig,
    ) -> Result<Response, ExternalPoolError> {
        let url = external_pool_url(pool, route.endpoint, config)?;
        let mut headers = forward_headers(&route.headers, pool)?;
        if !headers.contains_key(header::CONTENT_TYPE) {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        let request = self
            .client
            .post(url)
            .headers(headers)
            .body(route.raw_body.clone());
        let response = request.send().await.map_err(|err| ExternalPoolError {
            status: None,
            message: format!("external pool request send failed: {}", err),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_network_error_cooldown_secs.max(1)),
                format!("network_error {}", err),
            )),
            response_body: None,
            response_headers: HeaderMap::new(),
        })?;

        let status = response.status();
        if !status.is_success() {
            let headers = response.headers().clone();
            let body = response.bytes().await.unwrap_or_default();
            return Err(classify_external_error(status, body, headers, config));
        }
        let response_headers = response.headers().clone();
        let status = response.status();
        if route.payload.stream {
            let body_stream = response.bytes_stream();
            let usage_projection_mode = pool.usage_projection_mode;
            let reported_usage = route.reported_usage.clone();
            let endpoint = route.endpoint;
            let stream = futures::stream::unfold(
                (body_stream, Vec::<u8>::new(), Some(lease), false),
                move |(mut body_stream, mut buffer, lease, finished)| {
                    let reported_usage = reported_usage.clone();
                    async move {
                        if finished {
                            return None;
                        }
                        loop {
                            match body_stream.next().await {
                                Some(Ok(chunk)) => {
                                    buffer.extend_from_slice(&chunk);
                                    let projected = drain_projected_sse_events(
                                        &mut buffer,
                                        usage_projection_mode,
                                        &reported_usage,
                                        endpoint,
                                    );
                                    if !projected.is_empty() {
                                        return Some((
                                            Ok(Bytes::from(projected)),
                                            (body_stream, buffer, lease, false),
                                        ));
                                    }
                                }
                                Some(Err(err)) => {
                                    return Some((
                                        Err(std::io::Error::new(
                                            std::io::ErrorKind::Other,
                                            format!("external stream read error: {}", err),
                                        )),
                                        (body_stream, buffer, lease, false),
                                    ));
                                }
                                None => {
                                    let tail = if buffer.is_empty() {
                                        Vec::new()
                                    } else {
                                        maybe_project_sse_event(
                                            &buffer,
                                            usage_projection_mode,
                                            &reported_usage,
                                            endpoint,
                                        )
                                    };
                                    drop(lease);
                                    if tail.is_empty() {
                                        return None;
                                    }
                                    return Some((
                                        Ok(Bytes::from(tail)),
                                        (body_stream, Vec::new(), None, true),
                                    ));
                                }
                            }
                        }
                    }
                },
            );
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            builder
                .body(Body::from_stream(stream))
                .map_err(|err| ExternalPoolError {
                    status: None,
                    message: format!("build external stream response failed: {}", err),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: None,
                    response_body: None,
                    response_headers: HeaderMap::new(),
                })
        } else {
            let bytes = response.bytes().await.map_err(|err| ExternalPoolError {
                status: None,
                message: format!("external pool response read failed: {}", err),
                retryable: true,
                auto_disable_reason: None,
                cooldown: Some((
                    Duration::from_secs(config.external_pool_network_error_cooldown_secs.max(1)),
                    format!("network_error {}", err),
                )),
                response_body: None,
                response_headers: HeaderMap::new(),
            })?;
            drop(lease);
            let body = maybe_project_non_stream_usage(
                bytes,
                pool.usage_projection_mode,
                &route.reported_usage,
                route.endpoint,
            );
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            builder
                .body(Body::from(body))
                .map_err(|err| ExternalPoolError {
                    status: None,
                    message: format!("build external response failed: {}", err),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: None,
                    response_body: None,
                    response_headers: HeaderMap::new(),
                })
        }
    }

    async fn select_pool(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
    ) -> Option<ExternalPool> {
        let pools = self.postgres.list_external_pools(false).await.ok()?;
        let mut candidates = Vec::new();
        for pool in pools {
            if excluded.contains(&pool.id) {
                continue;
            }
            let (in_flight, cooldown_remaining_secs, _) = self.pool_runtime_snapshot(pool.id).await;
            if self
                .skip_reason(&pool, in_flight, cooldown_remaining_secs, config)
                .is_none()
            {
                candidates.push((pool, in_flight));
            }
        }
        candidates.sort_by(|(a, a_in_flight), (b, b_in_flight)| {
            let a_load = load_score(*a_in_flight, a.max_concurrent_requests);
            let b_load = load_score(*b_in_flight, b.max_concurrent_requests);
            a.priority
                .cmp(&b.priority)
                .then_with(|| a_load.cmp(&b_load))
                .then_with(|| a.id.cmp(&b.id))
        });
        let (best_priority, best_load) = candidates.first().map(|(pool, in_flight)| {
            (
                pool.priority,
                load_score(*in_flight, pool.max_concurrent_requests),
            )
        })?;
        let best: Vec<_> = candidates
            .into_iter()
            .filter(|(pool, in_flight)| {
                pool.priority == best_priority
                    && load_score(*in_flight, pool.max_concurrent_requests) == best_load
            })
            .collect();
        if best.is_empty() {
            return None;
        }
        let idx = fastrand::usize(..best.len());
        best.into_iter().nth(idx).map(|(pool, _)| pool)
    }

    fn skip_reason(
        &self,
        pool: &ExternalPool,
        in_flight: u32,
        cooldown_remaining_secs: u64,
        config: &ExternalPoolsConfig,
    ) -> Option<String> {
        if !config.external_pools_enabled {
            return Some("external_pools_disabled".to_string());
        }
        if !pool.enabled {
            return Some("disabled".to_string());
        }
        if pool.is_auto_disabled_now() {
            return Some("auto_disabled".to_string());
        }
        if cooldown_remaining_secs > 0 {
            return Some("cooldown".to_string());
        }
        let per_pool_max = pool.max_concurrent_requests.max(1);
        if in_flight >= per_pool_max {
            return Some("pool_concurrency_full".to_string());
        }
        None
    }

    async fn acquire_pool(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
    ) -> Option<ExternalPoolLease> {
        let (_, cooldown_remaining_secs, _) = self.pool_runtime_snapshot(pool.id).await;
        if cooldown_remaining_secs > 0 {
            return None;
        }
        let lease_id = match self.redis.next_external_pool_lease_id().await {
            Ok(lease_id) => lease_id,
            Err(err) => {
                tracing::warn!(pool_id = pool.id, "生成外部池 Redis lease ID 失败: {}", err);
                return None;
            }
        };
        let max_age = Some(Duration::from_secs(
            DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS.saturating_mul(2),
        ));
        match self
            .redis
            .acquire_external_pool_lease(
                pool.id,
                lease_id,
                pool.max_concurrent_requests.max(1),
                config.external_pool_global_max_concurrent_requests,
                max_age,
            )
            .await
        {
            Ok(Some(_)) => Some(ExternalPoolLease {
                manager: self.clone(),
                pool_id: pool.id,
                lease_id,
            }),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(pool_id = pool.id, "占用外部池 Redis 并发槽失败: {}", err);
                None
            }
        }
    }

    async fn release_pool(&self, pool_id: u64, lease_id: u64) {
        match self
            .redis
            .release_external_pool_lease(pool_id, lease_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                pool_id,
                lease_id,
                "外部池 Redis 并发 lease 已不存在或已释放"
            ),
            Err(err) => tracing::warn!(
                pool_id,
                lease_id,
                "释放外部池 Redis 并发 lease 失败: {}",
                err
            ),
        }
    }

    async fn mark_pool_cooldown(&self, pool_id: u64, duration: Duration, reason: String) {
        let until = Utc::now() + chrono::Duration::from_std(duration).unwrap_or_default();
        {
            let mut state = self.local_state.lock().await;
            let entry = state.entry(pool_id).or_insert(PoolRuntimeState {
                cooldown_until: None,
                cooldown_reason: None,
            });
            entry.cooldown_until = Some(until);
            entry.cooldown_reason = Some(reason.clone());
        }
        if let Err(err) = self
            .redis
            .set_json(
                format!("external_pool:{}:cooldown", pool_id),
                &json!({ "until": until.to_rfc3339(), "reason": reason }),
                duration.as_secs().max(1) as usize,
            )
            .await
        {
            tracing::warn!(pool_id, "写入外部池 Redis cooldown 失败: {}", err);
        }
    }

    async fn pool_runtime_snapshot(&self, pool_id: u64) -> (u32, u64, Option<String>) {
        let in_flight = self
            .redis
            .external_pool_capacity_state(
                pool_id,
                Some(Duration::from_secs(
                    DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS.saturating_mul(2),
                )),
            )
            .await
            .map(|state| state.pool_in_flight_requests)
            .unwrap_or_else(|err| {
                tracing::warn!(pool_id, "读取外部池 Redis 并发状态失败: {}", err);
                0
            });
        if let Ok(Some(cooldown)) = self
            .redis
            .get_json::<ExternalPoolCooldownState>(format!("external_pool:{}:cooldown", pool_id))
            .await
        {
            let now = Utc::now();
            if cooldown.until > now {
                return (
                    in_flight,
                    (cooldown.until - now).num_seconds().max(1) as u64,
                    cooldown.reason,
                );
            }
            let _ = self
                .redis
                .del(format!("external_pool:{}:cooldown", pool_id))
                .await;
        }
        let mut state = self.local_state.lock().await;
        let entry = state.entry(pool_id).or_insert(PoolRuntimeState {
            cooldown_until: None,
            cooldown_reason: None,
        });
        let now = Utc::now();
        let remaining = entry
            .cooldown_until
            .filter(|until| *until > now)
            .map(|until| (until - now).num_seconds().max(1) as u64)
            .unwrap_or(0);
        if remaining == 0 {
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
        }
        (in_flight, remaining, entry.cooldown_reason.clone())
    }

    async fn auto_disable_pool_if_configured(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
        reason: &str,
        last_error: &str,
    ) {
        if !pool_auto_disable_policy_enabled(pool.auto_disable_policy, config) {
            return;
        }
        if !auto_disable_reason_enabled(config, reason) {
            return;
        }
        let threshold = config.external_pool_auto_disable_failure_threshold.max(1) as u64;
        if threshold > 1 {
            let key = format!("external_pool:{}:auto_disable_failures:{}", pool.id, reason);
            let count = self
                .redis
                .incr_with_ttl(key, config.local_pool_circuit_window_secs.max(1) as usize)
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(
                        pool_id = pool.id,
                        reason,
                        "记录外部池自动禁用失败计数失败: {}",
                        err
                    );
                    threshold
                });
            if count < threshold {
                return;
            }
        }
        if let Err(err) = self
            .postgres
            .auto_disable_external_pool(
                pool.id,
                reason,
                last_error,
                config.external_pool_auto_disable_duration_secs,
            )
            .await
        {
            tracing::warn!(pool_id = pool.id, "自动禁用外部池失败: {}", err);
        }
    }

    fn record_external_success(
        &self,
        route: &ExternalRouteRequest,
        pool: &ExternalPool,
        attempts: Vec<ExternalPoolAttempt>,
    ) {
        self.record_external(
            route,
            Some(pool),
            attempts,
            UsageRecordStatus::Success,
            None,
            None,
            None,
        );
    }

    fn record_external_failure(
        &self,
        route: &ExternalRouteRequest,
        pool: Option<&ExternalPool>,
        attempts: Vec<ExternalPoolAttempt>,
        error_type: &str,
        error_message: &str,
    ) {
        let error_detail = format!("{}: {}", error_type, error_message);
        self.record_external(
            route,
            pool,
            attempts,
            UsageRecordStatus::Error,
            Some(error_type.to_string()),
            Some(error_message.to_string()),
            Some(error_detail),
        );
    }

    fn record_external(
        &self,
        route: &ExternalRouteRequest,
        pool: Option<&ExternalPool>,
        attempts: Vec<ExternalPoolAttempt>,
        status: UsageRecordStatus,
        error_type: Option<String>,
        error_message: Option<String>,
        error_detail: Option<String>,
    ) {
        let input_tokens = token::count_all_tokens(
            route.payload.model.clone(),
            route.payload.system.clone(),
            route.payload.messages.clone(),
            route.payload.tools.clone(),
        ) as i32;
        route.recorder.record(UsageRecord {
            id: route.request_id.clone(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: route.endpoint.to_string(),
            stream: route.payload.stream,
            model: route.payload.model.clone(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: crate::anthropic::converter::extract_stable_conversation_id(
                &route.payload,
            ),
            credential_id: None,
            credential_label: None,
            status,
            usage_source: UsageSource::RequestEstimate,
            total_input_tokens: input_tokens,
            compat_input_tokens: input_tokens,
            billable_input_tokens: input_tokens,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            estimated_cost_usd: 0.0,
            pricing_available: false,
            pricing_model: None,
            duration_ms: route.started_at.elapsed().as_millis() as u64,
            simulated: false,
            sticky_bound: false,
            fallback_from_sticky: false,
            credential_attempts: route.local_attempts.clone(),
            route_kind: Some(UsageRouteKind::ExternalPool),
            route_subtype: Some(route.route_subtype),
            fallback_reason: route.fallback_reason.clone(),
            direct_policy_reason: route.direct_policy_reason.clone(),
            local_attempted: Some(route.local_attempted),
            local_preflight: route.local_preflight.clone(),
            external_pool_id: pool.map(|pool| pool.id),
            external_pool_name: pool.map(|pool| pool.name.clone()),
            external_attempts: attempts,
            usage_projection_applied: Some(pool.is_some_and(|pool| {
                pool.usage_projection_mode == ExternalPoolUsageProjectionMode::CurrentPathPolicy
            })),
            error_type,
            error_message,
            error_detail,
            payload_breakdown: None,
            payload_guard_report: None,
        });
    }
}

fn pool_auto_disable_policy_enabled(
    policy: ExternalPoolAutoDisablePolicy,
    config: &ExternalPoolsConfig,
) -> bool {
    match policy {
        ExternalPoolAutoDisablePolicy::Disabled => false,
        ExternalPoolAutoDisablePolicy::Enabled => true,
        ExternalPoolAutoDisablePolicy::Inherit => config.external_pool_auto_disable_enabled,
    }
}

fn rule_matches(rule: &str, value: &str) -> bool {
    let rule = rule.trim();
    if rule.is_empty() {
        return false;
    }
    if rule == "*" {
        return true;
    }
    value.eq_ignore_ascii_case(rule)
        || value
            .to_ascii_lowercase()
            .contains(&rule.to_ascii_lowercase())
}

fn load_score(in_flight: u32, max: u32) -> u64 {
    let max = max.max(1) as u64;
    ((in_flight as u64) * 1_000_000) / max
}

fn external_pool_url(
    pool: &ExternalPool,
    endpoint: &str,
    config: &ExternalPoolsConfig,
) -> Result<Url, ExternalPoolError> {
    let mut base = pool.base_url.trim().trim_end_matches('/').to_string();
    let path = if pool.preserve_path {
        endpoint
    } else {
        "/v1/messages"
    };
    base.push_str(path);
    Url::parse(&base).map_err(|err| ExternalPoolError {
        status: None,
        message: format!("external pool URL is invalid: {}", err),
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        response_body: None,
        response_headers: HeaderMap::new(),
    })
}

fn forward_headers(
    headers: &HeaderMap,
    pool: &ExternalPool,
) -> Result<HeaderMap, ExternalPoolError> {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if should_forward_header(name) {
            out.insert(name.clone(), value.clone());
        }
    }
    match pool.auth_type {
        ExternalPoolAuthType::Bearer => {
            let key = pool.api_key.as_deref().unwrap_or_default();
            let value = HeaderValue::from_str(&format!("Bearer {}", key)).map_err(|err| {
                ExternalPoolError {
                    status: None,
                    message: format!("external pool auth header invalid: {}", err),
                    retryable: true,
                    auto_disable_reason: Some("auth_error".to_string()),
                    cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                    response_body: None,
                    response_headers: HeaderMap::new(),
                }
            })?;
            out.insert(header::AUTHORIZATION, value);
        }
        ExternalPoolAuthType::XApiKey => {
            let key = pool.api_key.as_deref().unwrap_or_default();
            let value = HeaderValue::from_str(key).map_err(|err| ExternalPoolError {
                status: None,
                message: format!("external pool x-api-key invalid: {}", err),
                retryable: true,
                auto_disable_reason: Some("auth_error".to_string()),
                cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                response_body: None,
                response_headers: HeaderMap::new(),
            })?;
            out.insert(HeaderName::from_static("x-api-key"), value);
        }
    }
    Ok(out)
}

fn should_forward_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
            | "authorization"
            | "x-api-key"
    )
}

fn should_forward_response_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "connection"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

fn apply_forwarded_response_headers(
    builder: &mut axum::http::response::Builder,
    headers: &HeaderMap,
    request_id: &str,
) {
    let Some(out) = builder.headers_mut() else {
        return;
    };
    for (name, value) in headers {
        if should_forward_response_header(name) {
            out.insert(name.clone(), value.clone());
        }
    }
    envelope::insert_request_id_headers(out, request_id);
}

fn external_pool_error_response(route: &ExternalRouteRequest, err: &ExternalPoolError) -> Response {
    external_pool_error_response_with_request_id(&route.request_id, err)
}

fn external_pool_error_response_with_request_id(
    request_id: &str,
    err: &ExternalPoolError,
) -> Response {
    if let (Some(status), Some(body)) = (err.status, err.response_body.clone()) {
        let mut builder = Response::builder().status(status);
        apply_forwarded_response_headers(&mut builder, &err.response_headers, request_id);
        return builder.body(Body::from(body)).unwrap_or_else(|build_err| {
            envelope::error_response_with_id(
                StatusCode::BAD_GATEWAY,
                "external_pool_error",
                format!("build external pool error response failed: {}", build_err),
                request_id,
            )
        });
    }

    envelope::error_response_with_id(
        err.status.unwrap_or(StatusCode::BAD_GATEWAY),
        "external_pool_error",
        err.message.clone(),
        request_id,
    )
}

fn classify_external_error(
    status: StatusCode,
    body: Bytes,
    headers: HeaderMap,
    config: &ExternalPoolsConfig,
) -> ExternalPoolError {
    let message = String::from_utf8_lossy(&body).to_string();
    let lower = message.to_ascii_lowercase();
    if status == StatusCode::BAD_REQUEST {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            response_body: Some(body),
            response_headers: headers,
        };
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_rate_limit_cooldown_secs.max(1)),
                "rate_limit".to_string(),
            )),
            response_body: Some(body),
            response_headers: headers,
        };
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let reason = if lower.contains("suspended")
            || lower.contains("risk")
            || lower.contains("locked")
            || lower.contains("security precaution")
        {
            "security_lock"
        } else {
            "auth_error"
        };
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some(reason.to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
                reason.to_string(),
            )),
            response_body: Some(body),
            response_headers: headers,
        };
    }
    if status.as_u16() == 402 || lower.contains("quota") || lower.contains("insufficient credits") {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some("quota_exhausted".to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_rate_limit_cooldown_secs.max(1)),
                "quota_exhausted".to_string(),
            )),
            response_body: Some(body),
            response_headers: headers,
        };
    }
    if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_server_error_cooldown_secs.max(1)),
                "server_error".to_string(),
            )),
            response_body: Some(body),
            response_headers: headers,
        };
    }
    ExternalPoolError {
        status: Some(status),
        message,
        retryable: false,
        auto_disable_reason: None,
        cooldown: None,
        response_body: Some(body),
        response_headers: headers,
    }
}

fn auto_disable_reason_enabled(config: &ExternalPoolsConfig, reason: &str) -> bool {
    match reason {
        "auth_error" => config.external_pool_auto_disable_on_auth_error,
        "security_lock" => config.external_pool_auto_disable_on_security_lock,
        "quota_exhausted" => config.external_pool_auto_disable_on_quota_exhausted,
        "misconfigured_endpoint" => config.external_pool_auto_disable_on_misconfigured_endpoint,
        _ => false,
    }
}

fn error_type_for_external_error(err: &ExternalPoolError) -> String {
    if let Some(reason) = err.auto_disable_reason.as_deref() {
        return reason.to_string();
    }
    match err.status {
        Some(StatusCode::TOO_MANY_REQUESTS) => "rate_limit",
        Some(status) if status.is_server_error() => "server_error",
        Some(StatusCode::BAD_REQUEST) => "bad_request",
        _ => "external_pool_error",
    }
    .to_string()
}

fn maybe_project_non_stream_usage(
    bytes: Bytes,
    mode: ExternalPoolUsageProjectionMode,
    reported_usage: &ReportedUsageConfig,
    endpoint: &str,
) -> Bytes {
    if mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return bytes;
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return bytes;
    };
    let Some(usage) = value.get_mut("usage") else {
        return bytes;
    };
    if !project_usage_value(usage, reported_usage, endpoint) {
        return bytes;
    }
    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(bytes)
}

fn maybe_project_sse_event(
    event: &[u8],
    mode: ExternalPoolUsageProjectionMode,
    reported_usage: &ReportedUsageConfig,
    endpoint: &str,
) -> Vec<u8> {
    if mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return event.to_vec();
    }
    let Ok(text) = std::str::from_utf8(event) else {
        return event.to_vec();
    };
    let mut changed = false;
    let mut out = Vec::with_capacity(event.len());
    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        let line_ending = &line[trimmed_line_end.len()..];
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        let leading_ws_len = data.len().saturating_sub(data.trim_start().len());
        let (leading_ws, data_json) = data.split_at(leading_ws_len);
        let data_json = data_json.trim_end();
        if data_json == "[DONE]" {
            out.extend_from_slice(line.as_bytes());
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data_json) else {
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        let Some(usage) = value.get_mut("usage") else {
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        if !project_usage_value(usage, reported_usage, endpoint) {
            out.extend_from_slice(line.as_bytes());
            continue;
        }
        changed = true;
        out.extend_from_slice(b"data:");
        out.extend_from_slice(leading_ws.as_bytes());
        out.extend_from_slice(
            serde_json::to_string(&value)
                .unwrap_or_else(|_| data_json.to_string())
                .as_bytes(),
        );
        out.extend_from_slice(line_ending.as_bytes());
    }
    if changed { out } else { event.to_vec() }
}

fn drain_projected_sse_events(
    buffer: &mut Vec<u8>,
    mode: ExternalPoolUsageProjectionMode,
    reported_usage: &ReportedUsageConfig,
    endpoint: &str,
) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
        let end = idx + delimiter_len;
        let event = buffer.drain(..end).collect::<Vec<u8>>();
        out.extend(maybe_project_sse_event(
            &event,
            mode,
            reported_usage,
            endpoint,
        ));
    }
    out
}

fn find_sse_event_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|idx| (idx, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| (idx, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn project_usage_value(
    usage: &mut serde_json::Value,
    reported_usage: &ReportedUsageConfig,
    endpoint: &str,
) -> bool {
    let Some(cache_usage) = cache_usage_from_value(usage) else {
        return false;
    };
    let raw = RawUsage {
        input_tokens: usage_i32(usage, "input_tokens"),
        output_tokens: usage_i32(usage, "output_tokens"),
        cache_creation_input_tokens: usage_i32(usage, "cache_creation_input_tokens"),
        cache_read_input_tokens: usage_i32(usage, "cache_read_input_tokens"),
        cache_creation_5m_input_tokens: usage_i32(usage, "cache_creation_5m_input_tokens"),
        cache_creation_1h_input_tokens: usage_i32(usage, "cache_creation_1h_input_tokens"),
    };
    let Some(policy) = ReportedCacheUsagePolicy::from_path_policy(
        reported_usage.policy_for_path(endpoint),
        fastrand::u64(..),
    ) else {
        return false;
    };
    let projected = cache_usage.with_reported_cache_usage_policy_and_raw(policy, raw);
    let projected_json = projected.to_json();
    let Some(obj) = usage.as_object_mut() else {
        return false;
    };
    let Some(projected_obj) = projected_json.as_object() else {
        return false;
    };
    for (key, value) in projected_obj {
        obj.insert(key.clone(), value.clone());
    }
    true
}

fn cache_usage_from_value(value: &serde_json::Value) -> Option<CacheUsage> {
    let input_tokens = usage_i32(value, "input_tokens");
    let output_tokens = usage_i32(value, "output_tokens");
    let cache_creation_input_tokens = usage_i32(value, "cache_creation_input_tokens");
    let cache_read_input_tokens = usage_i32(value, "cache_read_input_tokens");
    let cache_creation_5m_input_tokens = usage_i32(value, "cache_creation_5m_input_tokens");
    let cache_creation_1h_input_tokens = usage_i32(value, "cache_creation_1h_input_tokens");
    if input_tokens == 0
        && output_tokens == 0
        && cache_creation_input_tokens == 0
        && cache_read_input_tokens == 0
    {
        return None;
    }
    Some(CacheUsage {
        total_input_tokens: input_tokens
            .saturating_add(cache_creation_input_tokens)
            .saturating_add(cache_read_input_tokens),
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens,
    })
}

fn usage_i32(value: &serde_json::Value, key: &str) -> i32 {
    value
        .get(key)
        .and_then(|value| value.as_i64())
        .unwrap_or(0)
        .clamp(0, i32::MAX as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn external_pool_error_response_passes_through_upstream_error_body() {
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
        let response = external_pool_error_response_with_request_id("req_gateway", &err);

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static("anthropic-request-id"))
                .unwrap(),
            "req_upstream"
        );
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static("request-id"))
                .unwrap(),
            "req_gateway"
        );
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());

        let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read external error body");
        assert_eq!(actual, body);
    }

    #[tokio::test]
    async fn external_pool_retryable_final_error_keeps_upstream_response_shape() {
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

        let response = external_pool_error_response_with_request_id("req_gateway", &err);

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read external final retryable body");
        assert_eq!(actual, body);
    }

    #[test]
    fn usage_projection_pass_through_keeps_body_unchanged() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let projected = maybe_project_non_stream_usage(
            body.clone(),
            ExternalPoolUsageProjectionMode::PassThrough,
            &ReportedUsageConfig::default(),
            "/cc/v1/messages",
        );

        assert_eq!(projected, body);
    }

    #[test]
    fn usage_projection_applies_current_path_policy_to_json_body() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let projected = maybe_project_non_stream_usage(
            body.clone(),
            ExternalPoolUsageProjectionMode::CurrentPathPolicy,
            &ReportedUsageConfig::default(),
            "/cc/v1/messages",
        );

        let value: serde_json::Value = serde_json::from_slice(&projected).expect("projected json");
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
                > 0
        );
    }

    #[test]
    fn sse_usage_projection_preserves_delimiters_and_done_events() {
        let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

data: [DONE]

"#;
        let projected = maybe_project_sse_event(
            event,
            ExternalPoolUsageProjectionMode::CurrentPathPolicy,
            &ReportedUsageConfig::default(),
            "/cc/v1/messages",
        );
        let text = String::from_utf8(projected).expect("utf8");

        assert!(text.contains("data: [DONE]"));
        assert!(text.contains("\n\n"));
        assert!(!text.contains(r#""input_tokens":100000"#));
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
}
