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
use parking_lot::Mutex as SyncMutex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, Notify};
use tokio::time::{Instant, timeout};

use crate::{
    anthropic::{
        cache::{
            CacheAmplification, CacheSimulation, CacheUsage, RawUsage, ReportedCacheUsagePolicy,
        },
        envelope,
        model_capabilities::ModelCapabilitiesCatalog,
        pricing::PricingCatalog,
        prompt_cache::{PromptCacheProfile, PromptCacheScope, PromptCacheTracker},
        prompt_cache_creation_control::PromptCacheCreationController,
        types::MessagesRequest,
        usage::{
            ExternalPoolAttempt, ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageRecord,
            UsageRecordStatus, UsageRouteKind, UsageRouteSubtype, UsageSource,
        },
    },
    model::config::{
        ExternalPoolCapacityMode, ExternalPoolsConfig, PromptCacheCreationControlConfig,
        ReportedUsageConfig,
    },
    storage::{postgres::PostgresStore, redis_cache::RedisStore},
    token,
};

const DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS: u64 = 180;
const EXTERNAL_POOL_LEASE_TOUCH_INTERVAL_SECS: u64 = 30;
const EXTERNAL_POOL_PROMPT_CACHE_CREDENTIAL_ID_OFFSET: u64 = 1_u64 << 63;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
}

#[derive(Clone)]
pub struct ExternalRouteRequest {
    pub raw_body: Bytes,
    pub headers: HeaderMap,
    pub endpoint: &'static str,
    pub payload: MessagesRequest,
    pub upstream_model: Option<String>,
    pub model_resolution_source: Option<String>,
    pub model_resolution_note: Option<String>,
    pub route_subtype: UsageRouteSubtype,
    pub fallback_reason: Option<String>,
    pub direct_policy_reason: Option<String>,
    pub local_attempted: bool,
    pub local_preflight: Option<serde_json::Value>,
    pub local_attempts: Vec<crate::kiro::call_trace::KiroCredentialAttempt>,
    pub reported_usage: ReportedUsageConfig,
    pub prompt_cache: Arc<PromptCacheTracker>,
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    pub pricing_catalog: Arc<PricingCatalog>,
    pub request_id: String,
    pub recorder: Arc<crate::anthropic::usage::UsageRecorder>,
    pub started_at: Instant,
}

struct ExternalForwardResponse {
    response: Response,
    billing: Option<ExternalPoolBilling>,
    stream_usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_usage_projection: Option<ExternalUsageProjectionContext>,
}

pub enum ExternalPoolForwardOutcome {
    Response(Response),
    FinalError(ExternalPoolFinalError),
}

#[derive(Debug, Clone)]
pub struct ExternalPoolFinalError {
    pub status: StatusCode,
    pub response_error_type: String,
    pub route_error_type: String,
    pub message: String,
    pub retryable: bool,
    pub attempts: Vec<ExternalPoolAttempt>,
    pub pool_id: Option<u64>,
    pub pool_name: Option<String>,
}

impl ExternalPoolFinalError {
    pub fn into_response(self, request_id: &str) -> Response {
        envelope::error_response_with_id(
            self.status,
            self.response_error_type,
            self.message,
            request_id,
        )
    }

    pub fn is_rate_limit(&self) -> bool {
        self.status == StatusCode::TOO_MANY_REQUESTS
            || self.route_error_type == "rate_limit"
            || self
                .message
                .to_ascii_lowercase()
                .contains("too many requests")
            || self
                .message
                .to_ascii_lowercase()
                .contains("service_request_rate_exceeded")
    }

    pub fn is_timeout_like(&self) -> bool {
        let lower = self.message.to_ascii_lowercase();
        self.status == StatusCode::REQUEST_TIMEOUT
            || self.status == StatusCode::GATEWAY_TIMEOUT
            || lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("deadline")
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ExternalUsageCapture {
    raw: Option<CacheUsage>,
    shaped: Option<CacheUsage>,
    reported: Option<CacheUsage>,
    projected: bool,
}

#[derive(Debug, Default)]
struct ExternalUsageProjectionState {
    committed_controlled_usage: Option<CacheUsage>,
}

#[derive(Clone)]
struct ExternalUsageProjectionContext {
    mode: ExternalPoolUsageProjectionMode,
    raw_input_tokens: i32,
    simulated_usage: Option<CacheSimulation>,
    reported_policy: Option<ReportedCacheUsagePolicy>,
    scope: Option<PromptCacheScope>,
    prompt_cache: Arc<PromptCacheTracker>,
    prompt_cache_profile: Option<PromptCacheProfile>,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    uplift_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
    state: Arc<SyncMutex<ExternalUsageProjectionState>>,
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
    capacity_notify: Arc<Notify>,
}

struct ExternalPoolLease {
    manager: ExternalPoolManager,
    pool_id: u64,
    lease_id: u64,
}

impl ExternalPoolLease {
    fn touch(&self) {
        let manager = self.manager.clone();
        let pool_id = self.pool_id;
        let lease_id = self.lease_id;
        tokio::spawn(async move {
            manager.touch_pool(pool_id, lease_id).await;
        });
    }
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

struct ExternalPoolQueueGuard {
    manager: ExternalPoolManager,
    released: bool,
}

impl ExternalPoolQueueGuard {
    fn new(manager: ExternalPoolManager) -> Self {
        Self {
            manager,
            released: false,
        }
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let manager = self.manager.clone();
        tokio::spawn(async move {
            manager.leave_external_pool_queue().await;
        });
    }
}

impl Drop for ExternalPoolQueueGuard {
    fn drop(&mut self) {
        self.release();
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
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalPoolCooldownState {
    until: DateTime<Utc>,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolCapacityWaitReason {
    Full,
    Cooldown,
}

#[derive(Debug, Clone)]
struct PoolAcquireUnavailable {
    reason: PoolCapacityWaitReason,
    wait_for: Option<Duration>,
    exclude_pool_for_reselect: bool,
    detail: &'static str,
}

enum PoolAcquireResult {
    Acquired(ExternalPoolLease),
    Unavailable(PoolAcquireUnavailable),
}

#[derive(Debug, Clone, Default)]
struct PoolAvailabilitySnapshot {
    eligible_pools: usize,
    available_pools: usize,
    temporary_unavailable_pools: usize,
    wait_reason: Option<PoolCapacityWaitReason>,
    wait_for: Option<Duration>,
}

impl PoolAvailabilitySnapshot {
    fn has_eligible_pool(&self) -> bool {
        self.eligible_pools > 0
    }

    fn has_temporary_unavailable_pool(&self) -> bool {
        self.temporary_unavailable_pools > 0
    }
}

enum ExternalCapacityDecision {
    Retry,
    Respond(Response),
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
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            local_state: Arc::new(Mutex::new(HashMap::new())),
            capacity_notify: Arc::new(Notify::new()),
        }
    }

    pub async fn status(
        &self,
        config: &ExternalPoolsConfig,
    ) -> anyhow::Result<Vec<ExternalPoolStatus>> {
        let pools = self.postgres.list_external_pools(true).await?;
        let mut statuses = Vec::with_capacity(pools.len());
        for pool in pools {
            let (in_flight, global_in_flight, cooldown_remaining_secs, cooldown_reason) =
                self.pool_runtime_snapshot(pool.id).await;
            let skipped_reason = self.skip_reason(
                &pool,
                in_flight,
                global_in_flight,
                cooldown_remaining_secs,
                config,
            );
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

    pub async fn has_eligible_pool(&self, config: &ExternalPoolsConfig) -> bool {
        self.pool_availability_snapshot(&HashSet::new(), config)
            .await
            .has_eligible_pool()
    }

    pub async fn has_waitable_pool(&self, config: &ExternalPoolsConfig) -> bool {
        let snapshot = self
            .pool_availability_snapshot(&HashSet::new(), config)
            .await;
        snapshot.has_eligible_pool()
            && (snapshot.available_pools > 0 || snapshot.has_temporary_unavailable_pool())
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
        let request_id = route.request_id.clone();
        match self.forward_with_failover_result(config, route).await {
            ExternalPoolForwardOutcome::Response(response) => response,
            ExternalPoolForwardOutcome::FinalError(err) => err.into_response(&request_id),
        }
    }

    pub async fn forward_with_failover_result(
        &self,
        config: ExternalPoolsConfig,
        route: ExternalRouteRequest,
    ) -> ExternalPoolForwardOutcome {
        if !config.external_pools_enabled {
            self.record_external_failure(
                &route,
                None,
                Vec::new(),
                "external_pool_disabled",
                "request route is disabled",
            );
            return ExternalPoolForwardOutcome::FinalError(ExternalPoolFinalError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                response_error_type: "service_unavailable".to_string(),
                route_error_type: "external_pool_unavailable".to_string(),
                message: "request route is disabled".to_string(),
                retryable: false,
                attempts: Vec::new(),
                pool_id: None,
                pool_name: None,
            });
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
        let mut queue_guard: Option<ExternalPoolQueueGuard> = None;
        let mut wait_started_at: Option<Instant> = None;
        let mut attempt_index = 0usize;

        while attempt_index < max_attempts {
            let Some(pool) = self.select_pool(&excluded, &config).await else {
                let snapshot = self.pool_availability_snapshot(&excluded, &config).await;
                if snapshot.has_temporary_unavailable_pool() {
                    match self
                        .handle_capacity_unavailable(
                            &route,
                            attempts.clone(),
                            &config,
                            snapshot.wait_reason.unwrap_or(PoolCapacityWaitReason::Full),
                            snapshot.wait_for,
                            &mut queue_guard,
                            &mut wait_started_at,
                        )
                        .await
                    {
                        ExternalCapacityDecision::Retry => continue,
                        ExternalCapacityDecision::Respond(response) => {
                            return ExternalPoolForwardOutcome::Response(response);
                        }
                    }
                }
                break;
            };
            let pool_id = pool.id;
            let lease = match self.acquire_pool(&pool, &config).await {
                PoolAcquireResult::Acquired(lease) => lease,
                PoolAcquireResult::Unavailable(unavailable) => {
                    tracing::debug!(
                        pool_id,
                        reason = ?unavailable.reason,
                        detail = unavailable.detail,
                        exclude_pool_for_reselect = unavailable.exclude_pool_for_reselect,
                        "外部池选中后并发 lease 未占用，本次请求尝试重选或按配置等待"
                    );
                    if unavailable.exclude_pool_for_reselect {
                        excluded.insert(pool_id);
                        if self.select_pool(&excluded, &config).await.is_some() {
                            continue;
                        }
                        excluded.remove(&pool_id);
                    }
                    match self
                        .handle_capacity_unavailable(
                            &route,
                            attempts.clone(),
                            &config,
                            unavailable.reason,
                            unavailable.wait_for,
                            &mut queue_guard,
                            &mut wait_started_at,
                        )
                        .await
                    {
                        ExternalCapacityDecision::Retry => continue,
                        ExternalCapacityDecision::Respond(response) => {
                            return ExternalPoolForwardOutcome::Response(response);
                        }
                    }
                }
            };
            drop(queue_guard.take());
            let started = std::time::Instant::now();
            let current_attempt = attempt_index.saturating_add(1) as u32;
            attempt_index = attempt_index.saturating_add(1);
            let result = self.forward_once(&pool, &route, lease, &config).await;
            match result {
                Ok(forwarded) => {
                    attempts.push(ExternalPoolAttempt {
                        attempt: current_attempt,
                        pool_id,
                        pool_name: pool.name.clone(),
                        status: Some(forwarded.response.status().as_u16()),
                        action: "success".to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: None,
                        error_message: None,
                    });
                    if route.payload.stream {
                        return ExternalPoolForwardOutcome::Response(
                            self.wrap_external_stream_usage_record(
                                forwarded.response,
                                route.clone(),
                                pool,
                                attempts.clone(),
                                forwarded.stream_usage_capture,
                                forwarded.stream_usage_projection,
                            ),
                        );
                    }
                    if let Some(projection) = forwarded.stream_usage_projection.as_ref() {
                        projection.record_success();
                    }
                    self.record_external_success(
                        &route,
                        &pool,
                        attempts.clone(),
                        forwarded.billing,
                    );
                    return ExternalPoolForwardOutcome::Response(forwarded.response);
                }
                Err(err) => {
                    let action = if err.retryable { "retry_next" } else { "fail" };
                    attempts.push(ExternalPoolAttempt {
                        attempt: current_attempt,
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
                        attempts.clone(),
                        &error_type,
                        &err.message,
                    );
                    return ExternalPoolForwardOutcome::FinalError(
                        external_final_error_from_error(Some(&pool), attempts, &err),
                    );
                }
            }
        }

        if let Some((pool, err)) = last_error {
            let error_type = error_type_for_external_error(&err);
            self.record_external_failure(
                &route,
                Some(&pool),
                attempts.clone(),
                &error_type,
                &err.message,
            );
            return ExternalPoolForwardOutcome::FinalError(external_final_error_from_error(
                Some(&pool),
                attempts,
                &err,
            ));
        }

        self.record_external_failure(
            &route,
            None,
            attempts.clone(),
            "external_pool_unavailable",
            "No available external fallback pools",
        );
        ExternalPoolForwardOutcome::FinalError(ExternalPoolFinalError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            response_error_type: "service_unavailable".to_string(),
            route_error_type: "external_pool_unavailable".to_string(),
            message: "No available external fallback pools".to_string(),
            retryable: false,
            attempts,
            pool_id: None,
            pool_name: None,
        })
    }

    async fn forward_once(
        &self,
        pool: &ExternalPool,
        route: &ExternalRouteRequest,
        lease: ExternalPoolLease,
        config: &ExternalPoolsConfig,
    ) -> Result<ExternalForwardResponse, ExternalPoolError> {
        let url = external_pool_url(pool, route.endpoint, config)?;
        let mut headers = forward_headers(&route.headers, pool)?;
        if !headers.contains_key(header::CONTENT_TYPE) {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        let mut request = self
            .client
            .post(url)
            .headers(headers)
            .body(route.raw_body.clone());
        if route.payload.stream {
            if config.external_pool_stream_request_timeout_secs > 0 {
                request = request.timeout(Duration::from_secs(
                    config.external_pool_stream_request_timeout_secs,
                ));
            }
        } else if config.external_pool_request_timeout_secs > 0 {
            request = request.timeout(Duration::from_secs(
                config.external_pool_request_timeout_secs,
            ));
        }
        let response = request.send().await.map_err(|err| ExternalPoolError {
            status: None,
            message: format!("model endpoint request send failed: {}", err),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_network_error_cooldown_secs.max(1)),
                format!("network_error {}", err),
            )),
            response_body: None,
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
            if success_response_headers_look_like_html(&response_headers) {
                return Err(success_protocol_error(
                    &response_headers,
                    None,
                    config,
                    "model endpoint returned an HTML response for a streaming request",
                ));
            }
            let body_stream = response.bytes_stream();
            let projection_context = build_external_usage_projection_context(
                route,
                pool,
                config.external_pool_usage_projection_uplift_percent,
                config.external_pool_usage_projection_output_uplift_min_tokens,
                config.external_pool_usage_projection_output_uplift_percent,
            );
            let stream_idle_timeout = (config.external_pool_stream_idle_timeout_secs > 0)
                .then(|| Duration::from_secs(config.external_pool_stream_idle_timeout_secs));
            let stream_usage_projection = projection_context.clone();
            let usage_capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
            let stream_usage_capture = usage_capture.clone();
            let stream = futures::stream::unfold(
                (
                    body_stream,
                    Vec::<u8>::new(),
                    Some(lease),
                    Instant::now(),
                    Instant::now(),
                    false,
                ),
                move |(
                    mut body_stream,
                    mut buffer,
                    lease,
                    mut last_touch_at,
                    mut last_chunk_at,
                    finished,
                )| {
                    let projection_context = projection_context.clone();
                    let usage_capture = usage_capture.clone();
                    async move {
                        if finished {
                            return None;
                        }
                        loop {
                            tokio::select! {
                                chunk = body_stream.next() => {
                                    match chunk {
                                        Some(Ok(chunk)) => {
                                            last_chunk_at = Instant::now();
                                            buffer.extend_from_slice(&chunk);
                                            let projected = drain_projected_sse_events(
                                                &mut buffer,
                                                projection_context.as_ref(),
                                                Some(&usage_capture),
                                            );
                                            if !projected.is_empty() {
                                                return Some((
                                                    Ok(Bytes::from(projected)),
                                                    (
                                                        body_stream,
                                                        buffer,
                                                        lease,
                                                        last_touch_at,
                                                        last_chunk_at,
                                                        false,
                                                    ),
                                                ));
                                            }
                                        }
                                        Some(Err(err)) => {
                                            drop(lease);
                                            return Some((
                                                Err(std::io::Error::other(format!(
                                                    "external stream read error: {}",
                                                    err
                                                ))),
                                                (
                                                    body_stream,
                                                    Vec::new(),
                                                    None,
                                                    last_touch_at,
                                                    last_chunk_at,
                                                    true,
                                                ),
                                            ));
                                        }
                                        None => {
                                            let tail = if buffer.is_empty() {
                                                Vec::new()
                                            } else {
                                                maybe_project_sse_event(
                                                    &buffer,
                                                    projection_context.as_ref(),
                                                    Some(&usage_capture),
                                                )
                                            };
                                            drop(lease);
                                            if tail.is_empty() {
                                                return None;
                                            }
                                            return Some((
                                                Ok(Bytes::from(tail)),
                                                (
                                                    body_stream,
                                                    Vec::new(),
                                                    None,
                                                    last_touch_at,
                                                    last_chunk_at,
                                                    true,
                                                ),
                                            ));
                                        }
                                    }
                                }
                                _ = tokio::time::sleep_until(external_pool_lease_touch_deadline(last_touch_at)) => {
                                    if let Some(lease) = lease.as_ref() {
                                        lease.touch();
                                    }
                                    last_touch_at = Instant::now();
                                }
                                _ = external_pool_stream_idle_deadline(last_chunk_at, stream_idle_timeout) => {
                                    drop(lease);
                                    let seconds = stream_idle_timeout
                                        .map(|timeout| timeout.as_secs())
                                        .unwrap_or_default();
                                    return Some((
                                        Err(std::io::Error::other(format!(
                                            "model endpoint stream idle timeout after {} seconds",
                                            seconds
                                        ))),
                                        (
                                            body_stream,
                                            Vec::new(),
                                            None,
                                            last_touch_at,
                                            last_chunk_at,
                                            true,
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                },
            );
            let stream = Body::from_stream(stream);
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            let response = builder.body(stream).map_err(|err| ExternalPoolError {
                status: None,
                message: format!("build external stream response failed: {}", err),
                retryable: false,
                auto_disable_reason: None,
                cooldown: None,
                response_body: None,
            })?;
            Ok(ExternalForwardResponse {
                response,
                billing: None,
                stream_usage_capture: Some(stream_usage_capture),
                stream_usage_projection,
            })
        } else {
            let bytes = response.bytes().await.map_err(|err| ExternalPoolError {
                status: None,
                message: format!("model endpoint response read failed: {}", err),
                retryable: true,
                auto_disable_reason: None,
                cooldown: Some((
                    Duration::from_secs(config.external_pool_network_error_cooldown_secs.max(1)),
                    format!("network_error {}", err),
                )),
                response_body: None,
            })?;
            if success_response_looks_like_html(&response_headers, &bytes) {
                return Err(success_protocol_error(
                    &response_headers,
                    Some(&bytes),
                    config,
                    "model endpoint returned an HTML response for a non-streaming request",
                ));
            }
            drop(lease);
            let projection_context = build_external_usage_projection_context(
                route,
                pool,
                config.external_pool_usage_projection_uplift_percent,
                config.external_pool_usage_projection_output_uplift_min_tokens,
                config.external_pool_usage_projection_output_uplift_percent,
            );
            let projected = maybe_project_non_stream_usage(bytes, projection_context.as_ref());
            let billing = external_pool_billing_from_capture(route, pool, projected.usage_capture);
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            let response =
                builder
                    .body(Body::from(projected.body))
                    .map_err(|err| ExternalPoolError {
                        status: None,
                        message: format!("build external response failed: {}", err),
                        retryable: false,
                        auto_disable_reason: None,
                        cooldown: None,
                        response_body: None,
                    })?;
            Ok(ExternalForwardResponse {
                response,
                billing,
                stream_usage_capture: None,
                stream_usage_projection: projection_context,
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
            let (in_flight, global_in_flight, cooldown_remaining_secs, _) =
                self.pool_runtime_snapshot(pool.id).await;
            if self
                .skip_reason(
                    &pool,
                    in_flight,
                    global_in_flight,
                    cooldown_remaining_secs,
                    config,
                )
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

    async fn pool_availability_snapshot(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
    ) -> PoolAvailabilitySnapshot {
        if !config.external_pools_enabled {
            return PoolAvailabilitySnapshot::default();
        }
        let Ok(pools) = self.postgres.list_external_pools(false).await else {
            return PoolAvailabilitySnapshot::default();
        };
        let mut snapshot = PoolAvailabilitySnapshot::default();
        for pool in pools {
            if excluded.contains(&pool.id) {
                continue;
            }
            if !pool.enabled || pool.is_auto_disabled_now() {
                continue;
            }
            snapshot.eligible_pools += 1;
            let (in_flight, global_in_flight, cooldown_remaining_secs, _) =
                self.pool_runtime_snapshot(pool.id).await;
            match self.skip_reason(
                &pool,
                in_flight,
                global_in_flight,
                cooldown_remaining_secs,
                config,
            ) {
                None => snapshot.available_pools += 1,
                Some(reason)
                    if reason == "pool_concurrency_full" || reason == "global_concurrency_full" =>
                {
                    snapshot.temporary_unavailable_pools += 1;
                    if snapshot.wait_reason.is_none() {
                        snapshot.wait_reason = Some(PoolCapacityWaitReason::Full);
                    }
                }
                Some(reason) if reason == "cooldown" => {
                    snapshot.temporary_unavailable_pools += 1;
                    snapshot
                        .wait_reason
                        .get_or_insert(PoolCapacityWaitReason::Cooldown);
                    let wait_for = Duration::from_secs(cooldown_remaining_secs.max(1));
                    snapshot.wait_for = Some(
                        snapshot
                            .wait_for
                            .map(|existing| existing.min(wait_for))
                            .unwrap_or(wait_for),
                    );
                }
                _ => {}
            }
        }
        snapshot
    }

    async fn handle_capacity_unavailable(
        &self,
        route: &ExternalRouteRequest,
        attempts: Vec<ExternalPoolAttempt>,
        config: &ExternalPoolsConfig,
        reason: PoolCapacityWaitReason,
        wait_for: Option<Duration>,
        queue_guard: &mut Option<ExternalPoolQueueGuard>,
        wait_started_at: &mut Option<Instant>,
    ) -> ExternalCapacityDecision {
        if config.external_pool_capacity_mode != ExternalPoolCapacityMode::Wait {
            let (error_type, message) = external_capacity_error(reason);
            self.record_external_failure(route, None, attempts, error_type, message);
            return ExternalCapacityDecision::Respond(external_pool_scheduler_error_response(
                route,
                StatusCode::SERVICE_UNAVAILABLE,
                error_type,
                message,
            ));
        }

        let started = *wait_started_at.get_or_insert_with(Instant::now);
        let max_wait = if config.external_pool_dispatch_max_wait_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(
                config.external_pool_dispatch_max_wait_secs,
            ))
        };
        if let Some(max_wait) = max_wait {
            if started.elapsed() >= max_wait {
                let message = format!(
                    "Request capacity wait timed out after {} seconds",
                    max_wait.as_secs()
                );
                self.record_external_failure(
                    route,
                    None,
                    attempts,
                    "external_pool_wait_timeout",
                    &message,
                );
                return ExternalCapacityDecision::Respond(external_pool_scheduler_error_response(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_pool_wait_timeout",
                    message,
                ));
            }
        }

        if queue_guard.is_none() {
            match self
                .enter_external_pool_queue(config.external_pool_max_queued_requests)
                .await
            {
                Ok(Some(guard)) => *queue_guard = Some(guard),
                Ok(None) => {
                    let message = "Request dispatch queue is full";
                    self.record_external_failure(
                        route,
                        None,
                        attempts,
                        "external_pool_queue_full",
                        message,
                    );
                    return ExternalCapacityDecision::Respond(
                        external_pool_scheduler_error_response(
                            route,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "external_pool_queue_full",
                            message,
                        ),
                    );
                }
                Err(err) => {
                    let message = format!("Request dispatch queue unavailable: {}", err);
                    self.record_external_failure(
                        route,
                        None,
                        attempts,
                        "external_pool_queue_error",
                        &message,
                    );
                    return ExternalCapacityDecision::Respond(
                        external_pool_scheduler_error_response(
                            route,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "external_pool_queue_error",
                            message,
                        ),
                    );
                }
            }
        }

        let mut wakeup = wait_for
            .unwrap_or_else(|| Duration::from_secs(1))
            .min(Duration::from_secs(1));
        if let Some(max_wait) = max_wait {
            let remaining = max_wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                let message = format!(
                    "Request capacity wait timed out after {} seconds",
                    max_wait.as_secs()
                );
                self.record_external_failure(
                    route,
                    None,
                    attempts,
                    "external_pool_wait_timeout",
                    &message,
                );
                return ExternalCapacityDecision::Respond(external_pool_scheduler_error_response(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_pool_wait_timeout",
                    message,
                ));
            }
            wakeup = wakeup.min(remaining);
        }
        if !wakeup.is_zero() {
            let _ = timeout(wakeup, self.capacity_notify.notified()).await;
        }
        ExternalCapacityDecision::Retry
    }

    fn skip_reason(
        &self,
        pool: &ExternalPool,
        in_flight: u32,
        global_in_flight: u32,
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
        let global_max = config.external_pool_global_max_concurrent_requests;
        if global_max > 0 && global_in_flight >= global_max {
            return Some("global_concurrency_full".to_string());
        }
        None
    }

    async fn acquire_pool(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
    ) -> PoolAcquireResult {
        let (_, _, cooldown_remaining_secs, _) = self.pool_runtime_snapshot(pool.id).await;
        if cooldown_remaining_secs > 0 {
            return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                reason: PoolCapacityWaitReason::Cooldown,
                wait_for: Some(Duration::from_secs(cooldown_remaining_secs.max(1))),
                exclude_pool_for_reselect: true,
                detail: "cooldown_before_acquire",
            });
        }
        let lease_id = match self.redis.next_external_pool_lease_id().await {
            Ok(lease_id) => lease_id,
            Err(err) => {
                tracing::warn!(pool_id = pool.id, "生成外部池 Redis lease ID 失败: {}", err);
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: Some(Duration::from_secs(1)),
                    exclude_pool_for_reselect: false,
                    detail: "lease_id_error",
                });
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
            Ok(Some(_)) => PoolAcquireResult::Acquired(ExternalPoolLease {
                manager: self.clone(),
                pool_id: pool.id,
                lease_id,
            }),
            Ok(None) => {
                let (in_flight, global_in_flight, cooldown_remaining_secs, _) =
                    self.pool_runtime_snapshot(pool.id).await;
                let skip_reason = self.skip_reason(
                    pool,
                    in_flight,
                    global_in_flight,
                    cooldown_remaining_secs,
                    config,
                );
                let unavailable = match skip_reason.as_deref() {
                    Some("cooldown") => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Cooldown,
                        wait_for: Some(Duration::from_secs(cooldown_remaining_secs.max(1))),
                        exclude_pool_for_reselect: true,
                        detail: "cooldown_after_acquire_race",
                    },
                    Some("global_concurrency_full") => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Full,
                        wait_for: None,
                        exclude_pool_for_reselect: false,
                        detail: "global_concurrency_full_after_acquire_race",
                    },
                    Some("pool_concurrency_full") => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Full,
                        wait_for: None,
                        exclude_pool_for_reselect: true,
                        detail: "pool_concurrency_full_after_acquire_race",
                    },
                    _ => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Full,
                        wait_for: Some(Duration::from_secs(1)),
                        exclude_pool_for_reselect: true,
                        detail: "lease_acquire_race",
                    },
                };
                PoolAcquireResult::Unavailable(unavailable)
            }
            Err(err) => {
                tracing::warn!(pool_id = pool.id, "占用外部池 Redis 并发槽失败: {}", err);
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: Some(Duration::from_secs(1)),
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_error",
                })
            }
        }
    }

    async fn enter_external_pool_queue(
        &self,
        max_queued: u32,
    ) -> anyhow::Result<Option<ExternalPoolQueueGuard>> {
        if self
            .redis
            .try_enter_external_pool_dispatch_queue(max_queued)
            .await?
        {
            Ok(Some(ExternalPoolQueueGuard::new(self.clone())))
        } else {
            Ok(None)
        }
    }

    async fn leave_external_pool_queue(&self) {
        if let Err(err) = self.redis.leave_external_pool_dispatch_queue().await {
            tracing::warn!("释放外部池 Redis 调度排队占位失败: {}", err);
        }
        self.capacity_notify.notify_waiters();
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
        self.capacity_notify.notify_waiters();
    }

    async fn touch_pool(&self, pool_id: u64, lease_id: u64) {
        let ttl_secs = DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS
            .saturating_mul(4)
            .max(60) as usize;
        match self
            .redis
            .touch_external_pool_lease(pool_id, lease_id, ttl_secs)
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                pool_id,
                lease_id,
                "外部池 Redis 并发 lease touch 时已不存在"
            ),
            Err(err) => tracing::warn!(
                pool_id,
                lease_id,
                "续期外部池 Redis 并发 lease 失败: {}",
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

    async fn pool_runtime_snapshot(&self, pool_id: u64) -> (u32, u32, u64, Option<String>) {
        let capacity_state = self
            .redis
            .external_pool_capacity_state(
                pool_id,
                Some(Duration::from_secs(
                    DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS.saturating_mul(2),
                )),
            )
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(pool_id, "读取外部池 Redis 并发状态失败: {}", err);
                Default::default()
            });
        let in_flight = capacity_state.pool_in_flight_requests;
        let global_in_flight = capacity_state.global_in_flight_requests;
        if let Ok(Some(cooldown)) = self
            .redis
            .get_json::<ExternalPoolCooldownState>(format!("external_pool:{}:cooldown", pool_id))
            .await
        {
            let now = Utc::now();
            if cooldown.until > now {
                return (
                    in_flight,
                    global_in_flight,
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
        (
            in_flight,
            global_in_flight,
            remaining,
            entry.cooldown_reason.clone(),
        )
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
                .incr_with_ttl(
                    key,
                    config.external_pool_auto_disable_window_secs.max(1) as usize,
                )
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
        billing: Option<ExternalPoolBilling>,
    ) {
        self.record_external(
            route,
            Some(pool),
            attempts,
            UsageRecordStatus::Success,
            None,
            None,
            None,
            billing,
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
            None,
        );
    }

    fn wrap_external_stream_usage_record(
        &self,
        response: Response,
        route: ExternalRouteRequest,
        pool: ExternalPool,
        attempts: Vec<ExternalPoolAttempt>,
        usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
        usage_projection: Option<ExternalUsageProjectionContext>,
    ) -> Response {
        let (parts, body) = response.into_parts();
        let data_stream = body.into_data_stream();
        let guard = ExternalStreamUsageGuard {
            manager: self.clone(),
            route,
            pool,
            attempts,
            usage_capture,
            usage_projection,
            completed: false,
        };
        let stream = futures::stream::unfold(
            (data_stream, Some(guard)),
            |(mut data_stream, mut guard)| async move {
                match data_stream.next().await {
                    Some(Ok(chunk)) => Some((Ok(chunk), (data_stream, guard))),
                    Some(Err(err)) => {
                        if let Some(mut guard) = guard.take() {
                            guard.record_stream_error(&err.to_string());
                        }
                        Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("external stream response failed: {}", err),
                            )),
                            (data_stream, None),
                        ))
                    }
                    None => {
                        if let Some(mut guard) = guard.take() {
                            guard.record_success();
                        }
                        None
                    }
                }
            },
        );
        Response::from_parts(parts, Body::from_stream(stream))
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
        billing: Option<ExternalPoolBilling>,
    ) {
        let request_input_tokens = token::count_all_tokens(
            route.payload.model.clone(),
            route.payload.system.clone(),
            route.payload.messages.clone(),
            route.payload.tools.clone(),
        ) as i32;
        let usage = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .map(|billing| billing.reported_usage);
        let input_tokens = request_input_tokens;
        let compat_input_tokens = usage
            .map(|usage| usage.input_tokens)
            .unwrap_or(request_input_tokens);
        let billable_input_tokens = usage
            .map(|usage| usage.billable_input_tokens)
            .unwrap_or(request_input_tokens);
        let output_tokens = usage.map(|usage| usage.output_tokens).unwrap_or(0);
        let cache_read_input_tokens = usage
            .map(|usage| usage.cache_read_input_tokens)
            .unwrap_or(0);
        let cache_creation_input_tokens = usage
            .map(|usage| usage.cache_creation_input_tokens)
            .unwrap_or(0);
        let cache_creation_5m_input_tokens = usage
            .map(|usage| usage.cache_creation_5m_input_tokens)
            .unwrap_or(0);
        let cache_creation_1h_input_tokens = usage
            .map(|usage| usage.cache_creation_1h_input_tokens)
            .unwrap_or(0);
        let pricing_available = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .is_some_and(|billing| billing.pricing_available);
        let estimated_cost_usd = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .filter(|billing| billing.pricing_available)
            .map(|billing| billing.billable_cost_usd)
            .unwrap_or(0.0);
        let pricing_model = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .and_then(|billing| billing.pricing_model.clone());
        let usage_source = if billing
            .as_ref()
            .is_some_and(|billing| billing.usage_projection_applied)
        {
            UsageSource::LocalPromptCache
        } else if usage.is_some() {
            UsageSource::UpstreamMetadata
        } else {
            UsageSource::RequestEstimate
        };
        route.recorder.record(UsageRecord {
            id: route.request_id.clone(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: route.endpoint.to_string(),
            stream: route.payload.stream,
            model: route.payload.model.clone(),
            upstream_model: route.upstream_model.clone(),
            model_resolution_source: route.model_resolution_source.clone(),
            model_resolution_note: route.model_resolution_note.clone(),
            conversation_id: crate::anthropic::converter::extract_stable_conversation_id(
                &route.payload,
            ),
            credential_id: None,
            credential_label: None,
            status,
            usage_source,
            total_input_tokens: input_tokens,
            compat_input_tokens,
            billable_input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
            estimated_cost_usd,
            pricing_available,
            pricing_model,
            duration_ms: route.started_at.elapsed().as_millis() as u64,
            simulated: usage_source.is_simulated(),
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
            usage_projection_applied: billing
                .as_ref()
                .map(|billing| billing.usage_projection_applied),
            external_pool_billing: billing,
            error_type,
            error_message,
            error_detail,
            payload_breakdown: None,
            payload_guard_report: None,
        });
    }
}

struct ExternalStreamUsageGuard {
    manager: ExternalPoolManager,
    route: ExternalRouteRequest,
    pool: ExternalPool,
    attempts: Vec<ExternalPoolAttempt>,
    usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    usage_projection: Option<ExternalUsageProjectionContext>,
    completed: bool,
}

impl ExternalStreamUsageGuard {
    fn record_success(&mut self) {
        if self.completed {
            return;
        }
        let billing = self.usage_capture.as_ref().and_then(|capture| {
            external_pool_billing_from_capture_ref(&self.route, &self.pool, capture)
        });
        if let Some(projection) = self.usage_projection.as_ref() {
            projection.record_success();
        }
        self.manager.record_external_success(
            &self.route,
            &self.pool,
            self.attempts.clone(),
            billing,
        );
        self.completed = true;
    }

    fn record_stream_error(&mut self, message: &str) {
        if self.completed {
            return;
        }
        self.manager.record_external(
            &self.route,
            Some(&self.pool),
            self.attempts.clone(),
            UsageRecordStatus::StreamError,
            Some("stream_error".to_string()),
            Some(message.to_string()),
            Some(format!("stream_error: {}", message)),
            None,
        );
        self.completed = true;
    }

    fn record_client_dropped(&mut self) {
        if self.completed {
            return;
        }
        let message = "external stream body dropped before completion";
        self.manager.record_external(
            &self.route,
            Some(&self.pool),
            self.attempts.clone(),
            UsageRecordStatus::ClientDropped,
            Some("client_dropped".to_string()),
            Some(message.to_string()),
            Some(format!("client_dropped: {}", message)),
            None,
        );
        self.completed = true;
    }
}

impl Drop for ExternalStreamUsageGuard {
    fn drop(&mut self) {
        self.record_client_dropped();
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

fn external_pool_lease_touch_deadline(last_touch_at: Instant) -> Instant {
    last_touch_at + Duration::from_secs(EXTERNAL_POOL_LEASE_TOUCH_INTERVAL_SECS)
}

async fn external_pool_stream_idle_deadline(
    last_chunk_at: Instant,
    idle_timeout: Option<Duration>,
) {
    if let Some(idle_timeout) = idle_timeout {
        tokio::time::sleep_until(last_chunk_at + idle_timeout).await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(crate) fn external_pool_models_url(base_url: &str) -> Result<Url, url::ParseError> {
    let mut base = base_url.trim().trim_end_matches('/').to_string();
    if base_url_ends_with_v1(&base) {
        base.push_str("/models");
    } else {
        base.push_str("/v1/models");
    }
    Url::parse(&base)
}

pub(crate) fn external_pool_messages_url(base_url: &str) -> Result<Url, url::ParseError> {
    let mut base = base_url.trim().trim_end_matches('/').to_string();
    if base_url_ends_with_v1(&base) {
        base.push_str("/messages");
    } else {
        base.push_str("/v1/messages");
    }
    Url::parse(&base)
}

fn external_pool_url(
    pool: &ExternalPool,
    _endpoint: &str,
    config: &ExternalPoolsConfig,
) -> Result<Url, ExternalPoolError> {
    external_pool_messages_url(&pool.base_url).map_err(|err| ExternalPoolError {
        status: None,
        message: format!("model endpoint URL is invalid: {}", err),
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        response_body: None,
    })
}

fn base_url_ends_with_v1(base: &str) -> bool {
    Url::parse(base)
        .ok()
        .map(|url| {
            url.path()
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .is_some_and(|segment| segment.eq_ignore_ascii_case("v1"))
        })
        .unwrap_or_else(|| {
            base.trim_end_matches('/')
                .rsplit('/')
                .next()
                .is_some_and(|segment| segment.eq_ignore_ascii_case("v1"))
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
                    message: format!("model endpoint auth header invalid: {}", err),
                    retryable: true,
                    auto_disable_reason: Some("auth_error".to_string()),
                    cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                    response_body: None,
                }
            })?;
            out.insert(header::AUTHORIZATION, value);
        }
        ExternalPoolAuthType::XApiKey => {
            let key = pool.api_key.as_deref().unwrap_or_default();
            let value = HeaderValue::from_str(key).map_err(|err| ExternalPoolError {
                status: None,
                message: format!("model endpoint x-api-key invalid: {}", err),
                retryable: true,
                auto_disable_reason: Some("auth_error".to_string()),
                cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                response_body: None,
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

fn success_response_headers_look_like_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false)
}

fn success_response_looks_like_html(headers: &HeaderMap, body: &[u8]) -> bool {
    if success_response_headers_look_like_html(headers) {
        return true;
    }
    let trimmed = body
        .iter()
        .copied()
        .skip_while(|byte| byte.is_ascii_whitespace())
        .take(64)
        .collect::<Vec<_>>();
    let prefix = String::from_utf8_lossy(&trimmed).to_ascii_lowercase();
    prefix.starts_with("<!doctype html") || prefix.starts_with("<html")
}

fn success_protocol_error(
    headers: &HeaderMap,
    body: Option<&Bytes>,
    config: &ExternalPoolsConfig,
    context: &str,
) -> ExternalPoolError {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let body_prefix = body
        .map(|bytes| {
            String::from_utf8_lossy(&bytes[..bytes.len().min(120)])
                .replace('\n', " ")
                .replace('\r', " ")
        })
        .unwrap_or_default();
    let message = if body_prefix.is_empty() {
        format!("{context}; content-type={content_type}")
    } else {
        format!("{context}; content-type={content_type}; body_prefix={body_prefix}")
    };
    ExternalPoolError {
        status: Some(StatusCode::BAD_GATEWAY),
        message,
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        response_body: None,
    }
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

fn external_final_error_from_error(
    pool: Option<&ExternalPool>,
    attempts: Vec<ExternalPoolAttempt>,
    err: &ExternalPoolError,
) -> ExternalPoolFinalError {
    let message = err
        .response_body
        .as_ref()
        .map(|body| String::from_utf8_lossy(body).to_string())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| err.message.clone());
    ExternalPoolFinalError {
        status: err.status.unwrap_or(StatusCode::BAD_GATEWAY),
        response_error_type: anthropic_error_type_for_external_error(err).to_string(),
        route_error_type: error_type_for_external_error(err),
        message,
        retryable: err.retryable,
        attempts,
        pool_id: pool.map(|pool| pool.id),
        pool_name: pool.map(|pool| pool.name.clone()),
    }
}

fn external_capacity_error(reason: PoolCapacityWaitReason) -> (&'static str, &'static str) {
    match reason {
        PoolCapacityWaitReason::Full => ("external_pool_capacity_full", "Request capacity is full"),
        PoolCapacityWaitReason::Cooldown => (
            "external_pool_cooldown",
            "Request capacity is temporarily cooling down",
        ),
    }
}

fn external_pool_scheduler_error_response(
    route: &ExternalRouteRequest,
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
) -> Response {
    envelope::error_response_with_id(status, code, message.into(), &route.request_id)
}

fn classify_external_error(
    status: StatusCode,
    body: Bytes,
    _headers: HeaderMap,
    config: &ExternalPoolsConfig,
) -> ExternalPoolError {
    let message = String::from_utf8_lossy(&body).to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("too many requests")
        || lower.contains("service_request_rate_exceeded")
        || lower.contains("rate limit")
        || lower.contains("ratelimit")
    {
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
        };
    }
    if lower.contains("database is locked") || lower.contains("sqlite_busy") {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_server_error_cooldown_secs.max(1)),
                "database_busy".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if lower.contains("invalid token") {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some("auth_error".to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
                "auth_error".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if lower.contains("channel affinity") && lower.contains("disabled") {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some("channel_disabled".to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
                "channel_disabled".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if lower.contains("model_not_found")
        || lower.contains("failed to get available channel for model")
        || lower.contains("no available channel")
    {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_server_error_cooldown_secs.max(1)),
                "model_unavailable".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if status == StatusCode::BAD_REQUEST {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            response_body: Some(body),
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
        };
    }
    ExternalPoolError {
        status: Some(status),
        message,
        retryable: false,
        auto_disable_reason: None,
        cooldown: None,
        response_body: Some(body),
    }
}

fn auto_disable_reason_enabled(config: &ExternalPoolsConfig, reason: &str) -> bool {
    match reason {
        "auth_error" => config.external_pool_auto_disable_on_auth_error,
        "security_lock" => config.external_pool_auto_disable_on_security_lock,
        "quota_exhausted" => config.external_pool_auto_disable_on_quota_exhausted,
        "misconfigured_endpoint" => config.external_pool_auto_disable_on_misconfigured_endpoint,
        "channel_disabled" => config.external_pool_auto_disable_on_channel_disabled,
        _ => false,
    }
}

fn error_type_for_external_error(err: &ExternalPoolError) -> String {
    if let Some(reason) = err.auto_disable_reason.as_deref() {
        return reason.to_string();
    }
    if let Some((_, reason)) = err.cooldown.as_ref() {
        if reason == "rate_limit"
            || reason == "database_busy"
            || reason == "model_unavailable"
            || reason == "server_error"
            || reason.starts_with("network_error")
        {
            return reason
                .split_whitespace()
                .next()
                .unwrap_or("external_pool_error")
                .to_string();
        }
    }
    match err.status {
        Some(StatusCode::TOO_MANY_REQUESTS) => "rate_limit",
        Some(status) if status.is_server_error() => "server_error",
        Some(StatusCode::BAD_REQUEST) => "bad_request",
        _ => "external_pool_error",
    }
    .to_string()
}

fn anthropic_error_type_for_external_error(err: &ExternalPoolError) -> &'static str {
    match err.status {
        Some(StatusCode::BAD_REQUEST) => "invalid_request_error",
        Some(StatusCode::UNAUTHORIZED) => "authentication_error",
        Some(StatusCode::FORBIDDEN) => "permission_error",
        Some(StatusCode::TOO_MANY_REQUESTS) => "rate_limit_error",
        Some(status) if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT => {
            "api_error"
        }
        _ => match error_type_for_external_error(err).as_str() {
            "rate_limit" => "rate_limit_error",
            "bad_request" => "invalid_request_error",
            "auth_error" => "authentication_error",
            "security_lock" => "permission_error",
            _ => "api_error",
        },
    }
}

struct ProjectedNonStreamBody {
    body: Bytes,
    usage_capture: ExternalUsageCapture,
}

fn maybe_project_non_stream_usage(
    bytes: Bytes,
    projection: Option<&ExternalUsageProjectionContext>,
) -> ProjectedNonStreamBody {
    let mut usage_capture = ExternalUsageCapture::default();
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ProjectedNonStreamBody {
            body: bytes,
            usage_capture,
        };
    };
    let Some(usage) = value.get_mut("usage") else {
        return ProjectedNonStreamBody {
            body: bytes,
            usage_capture,
        };
    };
    let raw_usage = cache_usage_from_value(usage);
    usage_capture.raw = raw_usage;
    usage_capture.reported = raw_usage;

    if let Some(projected) = project_usage_value(usage, projection) {
        usage_capture.shaped = Some(projected.shaped);
        usage_capture.reported = cache_usage_from_value(usage)
            .or(Some(projected.reported))
            .or(raw_usage);
        usage_capture.projected = true;
        let body = serde_json::to_vec(&value).map(Bytes::from).unwrap_or(bytes);
        return ProjectedNonStreamBody {
            body,
            usage_capture,
        };
    }

    ProjectedNonStreamBody {
        body: bytes,
        usage_capture,
    }
}

fn maybe_project_sse_event(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) -> Vec<u8> {
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
        let raw_usage = cache_usage_from_value(usage);
        let Some(projected_usage) = project_usage_value(usage, projection) else {
            update_external_usage_capture(capture, raw_usage, raw_usage, raw_usage, false);
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        update_external_usage_capture(
            capture,
            raw_usage,
            Some(projected_usage.shaped),
            Some(projected_usage.reported),
            true,
        );
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
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
        let end = idx + delimiter_len;
        let event = buffer.drain(..end).collect::<Vec<u8>>();
        out.extend(maybe_project_sse_event(&event, projection, capture));
    }
    out
}

fn update_external_usage_capture(
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    raw: Option<CacheUsage>,
    shaped: Option<CacheUsage>,
    reported: Option<CacheUsage>,
    projected: bool,
) {
    let Some(capture) = capture else {
        return;
    };
    let mut capture = capture.lock();
    if let Some(raw) = raw {
        capture.raw = Some(merge_external_usage(capture.raw, raw));
    }
    if let Some(shaped) = shaped {
        capture.shaped = Some(merge_external_usage(capture.shaped, shaped));
    }
    if let Some(reported) = reported {
        capture.reported = Some(merge_external_usage(capture.reported, reported));
    }
    if projected {
        capture.projected = true;
    }
}

fn merge_external_usage(existing: Option<CacheUsage>, incoming: CacheUsage) -> CacheUsage {
    let Some(existing) = existing else {
        return incoming;
    };
    let input_tokens = existing.input_tokens.max(incoming.input_tokens);
    let output_tokens = existing.output_tokens.max(incoming.output_tokens);
    let cache_read_input_tokens = existing
        .cache_read_input_tokens
        .max(incoming.cache_read_input_tokens);
    let cache_creation_input_tokens = existing
        .cache_creation_input_tokens
        .max(incoming.cache_creation_input_tokens);
    let cache_creation_5m_input_tokens = existing
        .cache_creation_5m_input_tokens
        .max(incoming.cache_creation_5m_input_tokens)
        .min(cache_creation_input_tokens);
    let cache_creation_1h_input_tokens = existing
        .cache_creation_1h_input_tokens
        .max(incoming.cache_creation_1h_input_tokens)
        .min(cache_creation_input_tokens.saturating_sub(cache_creation_5m_input_tokens));
    CacheUsage {
        total_input_tokens: input_tokens
            .saturating_add(cache_read_input_tokens)
            .saturating_add(cache_creation_input_tokens),
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens,
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectedExternalUsage {
    shaped: CacheUsage,
    reported: CacheUsage,
}

fn project_usage_value(
    usage: &mut serde_json::Value,
    projection: Option<&ExternalUsageProjectionContext>,
) -> Option<ProjectedExternalUsage> {
    let projection = projection?;
    if projection.mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }
    let output_tokens = usage_i32(usage, "output_tokens");
    let computed = projection
        .simulated_usage
        .map(|simulation| {
            CacheSimulation::to_usage(simulation, projection.raw_input_tokens, output_tokens)
        })
        .unwrap_or_else(|| CacheUsage {
            total_input_tokens: projection.raw_input_tokens,
            input_tokens: projection.raw_input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        });
    let raw_usage = RawUsage::uncached(projection.raw_input_tokens, output_tokens);
    let reported = projection
        .reported_policy
        .clone()
        .map(|policy| computed.with_reported_cache_usage_policy_and_raw(policy, raw_usage))
        .unwrap_or(computed);
    projection.mark_committed(reported);
    let shaped = projection.prompt_cache_creation_controller.preview_success(
        projection.scope.as_ref(),
        projection.prompt_cache_creation_control,
        reported,
    );
    let projected = shaped
        .with_external_pool_usage_uplift(projection.uplift_percent)
        .with_external_pool_output_uplift(
            projection.output_uplift_min_tokens,
            projection.output_uplift_percent,
        );
    let projected_json = projected.to_json();
    let Some(obj) = usage.as_object_mut() else {
        return None;
    };
    let Some(projected_obj) = projected_json.as_object() else {
        return None;
    };
    for (key, value) in projected_obj {
        obj.insert(key.clone(), value.clone());
    }
    Some(ProjectedExternalUsage {
        shaped,
        reported: projected,
    })
}

trait ExternalPoolUsageUplift {
    fn with_external_pool_usage_uplift(self, percent: u32) -> Self;
    fn with_external_pool_output_uplift(self, min_tokens: i32, percent: u32) -> Self;
}

impl ExternalPoolUsageUplift for CacheUsage {
    fn with_external_pool_usage_uplift(self, percent: u32) -> Self {
        let percent = percent.min(200);
        if percent == 0 {
            return self;
        }

        let cache_read_input_tokens = uplift_tokens(self.cache_read_input_tokens.max(0), percent);
        let cache_creation_input_tokens =
            uplift_tokens(self.cache_creation_input_tokens.max(0), percent);
        let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
            uplift_cache_creation_breakdown(
                self.cache_creation_5m_input_tokens,
                self.cache_creation_1h_input_tokens,
                cache_creation_input_tokens,
            );

        Self {
            total_input_tokens: self
                .input_tokens
                .max(0)
                .saturating_add(cache_read_input_tokens)
                .saturating_add(cache_creation_input_tokens),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        }
    }

    fn with_external_pool_output_uplift(self, min_tokens: i32, percent: u32) -> Self {
        let percent = percent.min(200);
        let min_tokens = min_tokens.max(0);
        if percent == 0 || min_tokens == 0 || self.output_tokens < min_tokens {
            return self;
        }

        Self {
            output_tokens: uplift_tokens(self.output_tokens.max(0), percent),
            ..self
        }
    }
}

fn uplift_tokens(tokens: i32, percent: u32) -> i32 {
    if tokens <= 0 || percent == 0 {
        return tokens.max(0);
    }
    let numerator = tokens as i64 * (100 + percent.min(200)) as i64;
    ((numerator + 99) / 100).clamp(0, i32::MAX as i64) as i32
}

fn uplift_cache_creation_breakdown(
    base_5m: i32,
    base_1h: i32,
    cache_creation_input_tokens: i32,
) -> (i32, i32) {
    let cache_creation_input_tokens = cache_creation_input_tokens.max(0);
    if cache_creation_input_tokens == 0 {
        return (0, 0);
    }
    let base_5m = base_5m.max(0);
    let base_1h = base_1h.max(0);
    let base_total = base_5m.saturating_add(base_1h);
    if base_total == 0 {
        return (cache_creation_input_tokens, 0);
    }
    let five_min = ((cache_creation_input_tokens as i64 * base_5m as i64)
        + (base_total as i64 / 2))
        / base_total as i64;
    let five_min = five_min.clamp(0, cache_creation_input_tokens as i64) as i32;
    let one_hour = cache_creation_input_tokens.saturating_sub(five_min);
    (five_min, one_hour)
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

fn external_pool_billing_from_capture(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    capture: ExternalUsageCapture,
) -> Option<ExternalPoolBilling> {
    let raw = capture.raw?;
    let reported = capture.reported.or(capture.raw)?;
    let shaped = capture.shaped.or(capture.reported).or(capture.raw)?;
    Some(external_pool_billing(
        route,
        pool,
        raw,
        shaped,
        reported,
        capture.projected,
    ))
}

fn external_pool_billing_from_capture_ref(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    capture: &Arc<SyncMutex<ExternalUsageCapture>>,
) -> Option<ExternalPoolBilling> {
    external_pool_billing_from_capture(route, pool, *capture.lock())
}

fn external_pool_billing(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    raw_usage: CacheUsage,
    shaped_usage: CacheUsage,
    reported_usage: CacheUsage,
    usage_projection_applied: bool,
) -> ExternalPoolBilling {
    let pricing_model = route
        .upstream_model
        .as_deref()
        .unwrap_or(&route.payload.model);
    let raw_estimate = route.pricing_catalog.estimate(pricing_model, raw_usage);
    let shaped_estimate = route.pricing_catalog.estimate(pricing_model, shaped_usage);
    let reported_estimate = route
        .pricing_catalog
        .estimate(pricing_model, reported_usage);
    let pricing_available =
        raw_estimate.available && shaped_estimate.available && reported_estimate.available;
    let raw_cost_usd = if pricing_available {
        raw_estimate.cost_usd
    } else {
        0.0
    };
    let shaped_cost_usd = if pricing_available {
        shaped_estimate.cost_usd
    } else {
        0.0
    };
    let reported_cost_usd = if pricing_available {
        reported_estimate.cost_usd
    } else {
        0.0
    };
    let uplifted_cost_usd = reported_cost_usd;
    let profit_usd = uplifted_cost_usd - raw_cost_usd;

    ExternalPoolBilling {
        raw_usage: external_usage_snapshot(raw_usage),
        shaped_usage: external_usage_snapshot(shaped_usage),
        reported_usage: external_usage_snapshot(reported_usage),
        usage_projection_applied,
        raw_cost_usd,
        shaped_cost_usd,
        uplifted_cost_usd,
        profit_usd,
        reported_cost_usd,
        billable_cost_usd: uplifted_cost_usd,
        cost_floor_delta_usd: (raw_cost_usd - uplifted_cost_usd).max(0.0),
        cost_floor_applied: pricing_available && uplifted_cost_usd < raw_cost_usd,
        pricing_available,
        pricing_model: Some(reported_estimate.model),
        usage_projection_mode: pool.usage_projection_mode.as_str().to_string(),
    }
}

fn build_external_usage_projection_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    if pool.usage_projection_mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }

    let model = route
        .upstream_model
        .clone()
        .unwrap_or_else(|| route.payload.model.clone());
    let prompt_cache_supported = route
        .model_capabilities
        .supports_prompt_caching_for(&model)
        .unwrap_or(true);

    let raw_input_tokens = count_external_route_input_tokens(&route.payload);
    let (scope, profile, simulated_usage) = if prompt_cache_supported {
        let profile = route.prompt_cache.build_high_cache_profile_for_model(
            &route.payload,
            raw_input_tokens,
            &model,
        );
        let scope = external_prompt_cache_scope(route, pool, &model);
        let prompt_usage = route.prompt_cache.compute(
            scope.clone(),
            profile.as_ref(),
            route.prompt_cache_target_read_ratio,
        );
        let simulated_usage = profile.as_ref().and_then(|profile| {
            CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
                prompt_usage,
                route.prompt_cache_target_read_ratio,
                external_cache_amplification(route, profile),
            )
        });
        (scope, profile, simulated_usage)
    } else {
        (None, None, None)
    };
    let reported_policy = ReportedCacheUsagePolicy::from_path_policy(
        route.reported_usage.policy_for_path(route.endpoint),
        profile
            .as_ref()
            .map(|profile| profile.cache_jitter_seed())
            .unwrap_or(0)
            ^ fastrand::u64(..),
    );
    Some(ExternalUsageProjectionContext {
        mode: pool.usage_projection_mode,
        raw_input_tokens,
        simulated_usage,
        reported_policy,
        scope,
        prompt_cache: route.prompt_cache.clone(),
        prompt_cache_profile: profile,
        prompt_cache_target_read_ratio: route.prompt_cache_target_read_ratio,
        prompt_cache_creation_controller: route.prompt_cache_creation_controller.clone(),
        prompt_cache_creation_control: route.prompt_cache_creation_control,
        uplift_percent,
        output_uplift_min_tokens: output_uplift_min_tokens.max(0),
        output_uplift_percent: output_uplift_percent.min(200),
        state: Arc::new(SyncMutex::new(ExternalUsageProjectionState::default())),
    })
}

fn count_external_route_input_tokens(payload: &MessagesRequest) -> i32 {
    token::count_all_tokens(
        payload.model.clone(),
        payload.system.clone(),
        payload.messages.clone(),
        payload.tools.clone(),
    ) as i32
}

fn external_prompt_cache_scope(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    model: &str,
) -> Option<PromptCacheScope> {
    Some(PromptCacheScope {
        credential_id: external_pool_prompt_cache_credential_id(pool.id),
        conversation_id: crate::anthropic::converter::extract_stable_conversation_id(
            &route.payload,
        )?,
        model: model.to_string(),
    })
}

fn external_pool_prompt_cache_credential_id(pool_id: u64) -> u64 {
    EXTERNAL_POOL_PROMPT_CACHE_CREDENTIAL_ID_OFFSET.saturating_add(pool_id)
}

fn external_cache_amplification(
    route: &ExternalRouteRequest,
    profile: &PromptCacheProfile,
) -> Option<CacheAmplification> {
    Some(CacheAmplification::new(
        route.prompt_cache_token_scale,
        route.prompt_cache_max_simulated_input_tokens,
        route.prompt_cache_cap_jitter_min_tokens,
        route.prompt_cache_cap_jitter_max_tokens,
        route.prompt_cache_scale_min_input_tokens,
        profile.cache_jitter_seed(),
    ))
}

impl ExternalUsageProjectionContext {
    fn mark_committed(&self, usage: CacheUsage) {
        let mut state = self.state.lock();
        state.committed_controlled_usage = Some(usage);
    }

    fn record_success(&self) {
        let Some(usage) = self.state.lock().committed_controlled_usage else {
            return;
        };
        let _ = self.prompt_cache_creation_controller.apply_success(
            self.scope.as_ref(),
            self.prompt_cache_creation_control,
            usage,
        );
        self.prompt_cache.update(
            self.scope.clone(),
            self.prompt_cache_profile.as_ref(),
            self.prompt_cache_target_read_ratio,
        );
    }
}

fn external_usage_snapshot(usage: CacheUsage) -> ExternalPoolUsageSnapshot {
    ExternalPoolUsageSnapshot {
        total_input_tokens: usage.total_input_tokens,
        input_tokens: usage.input_tokens,
        billable_input_tokens: usage.billable_input_tokens(),
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
    }
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
    use crate::anthropic::types::{Message, Metadata, SystemMessage};

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
    async fn external_pool_error_response_wraps_raw_error_body_as_message() {
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
        let response =
            external_final_error_from_error(None, Vec::new(), &err).into_response("req_gateway");

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
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());

        let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read external error body");
        let value: serde_json::Value = serde_json::from_slice(&actual).expect("json envelope");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(
            value["error"]["message"].as_str(),
            Some(String::from_utf8_lossy(&body).as_ref())
        );
        assert_eq!(value["request_id"], "req_gateway");
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

        let response =
            external_final_error_from_error(None, Vec::new(), &err).into_response("req_gateway");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read external final retryable body");
        let value: serde_json::Value = serde_json::from_slice(&actual).expect("json envelope");
        assert_eq!(value["error"]["type"], "rate_limit_error");
        assert_eq!(
            value["error"]["message"].as_str(),
            Some(String::from_utf8_lossy(&body).as_ref())
        );
        assert_eq!(value["request_id"], "req_gateway");
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
    }

    #[tokio::test]
    async fn external_capacity_scheduler_error_uses_request_id_and_error_type() {
        let route = ExternalRouteRequest {
            raw_body: Bytes::new(),
            headers: HeaderMap::new(),
            endpoint: "/v1/messages",
            payload: MessagesRequest {
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
            },
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
            prompt_cache_target_read_ratio: 0.98,
            prompt_cache_token_scale: 1.6,
            prompt_cache_max_simulated_input_tokens: 300_000,
            prompt_cache_cap_jitter_min_tokens: 12_000,
            prompt_cache_cap_jitter_max_tokens: 24_000,
            prompt_cache_scale_min_input_tokens: 20_000,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_external_capacity".to_string(),
            recorder: Arc::new(crate::anthropic::usage::UsageRecorder::new(1)),
            started_at: Instant::now(),
        };

        let (error_type, message) = external_capacity_error(PoolCapacityWaitReason::Full);
        let response = external_pool_scheduler_error_response(
            &route,
            StatusCode::SERVICE_UNAVAILABLE,
            error_type,
            message,
        );

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get("request-id").unwrap(),
            "req_external_capacity"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read scheduler error body");
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "external_pool_capacity_full");
        assert_eq!(body["error"]["message"], "Request capacity is full");
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
        assert_eq!(err.status, Some(StatusCode::BAD_GATEWAY));
        assert_eq!(
            err.auto_disable_reason.as_deref(),
            Some("misconfigured_endpoint")
        );
        assert_eq!(
            error_type_for_external_error(&err),
            "misconfigured_endpoint"
        );
    }

    fn test_pool(base_url: &str, preserve_path: bool) -> ExternalPool {
        let now = Utc::now();
        ExternalPool {
            id: 1,
            name: "test".to_string(),
            base_url: base_url.to_string(),
            api_key: Some("sk-test".to_string()),
            masked_api_key: None,
            auth_type: ExternalPoolAuthType::Bearer,
            enabled: true,
            priority: 10,
            max_concurrent_requests: 10,
            usage_projection_mode: ExternalPoolUsageProjectionMode::PassThrough,
            auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
            auto_disabled: false,
            auto_disabled_reason: None,
            auto_disabled_at: None,
            auto_disabled_until: None,
            auto_disabled_last_error: None,
            preserve_path,
            notes: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_route(model: &str) -> ExternalRouteRequest {
        ExternalRouteRequest {
            raw_body: Bytes::new(),
            headers: HeaderMap::new(),
            endpoint: "/cc/v1/messages",
            payload: test_payload(model),
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
            prompt_cache_target_read_ratio: 0.98,
            prompt_cache_token_scale: 1.6,
            prompt_cache_max_simulated_input_tokens: 300_000,
            prompt_cache_cap_jitter_min_tokens: 12_000,
            prompt_cache_cap_jitter_max_tokens: 24_000,
            prompt_cache_scale_min_input_tokens: 20_000,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_external_billing".to_string(),
            recorder: Arc::new(crate::anthropic::usage::UsageRecorder::new(1)),
            started_at: Instant::now(),
        }
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
    }

    #[test]
    fn usage_projection_applies_current_path_policy_to_json_body() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let route = test_route("claude-sonnet-4-5");
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&route, &pool, 0);
        let projected = maybe_project_non_stream_usage(body.clone(), projection.as_ref());

        let value: serde_json::Value =
            serde_json::from_slice(&projected.body).expect("projected json");
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
        assert!(
            usage
                .get("cache_creation_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default()
                > 0
        );
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

        let value: serde_json::Value =
            serde_json::from_slice(&projected.body).expect("projected json");
        let usage = value.get("usage").expect("usage object");
        assert_eq!(
            usage
                .get("input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            count_external_route_input_tokens(&route.payload) as i64
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
                .cache_creation_input_tokens,
            0
        );
    }

    #[test]
    fn usage_projection_applies_external_pool_uplift_after_path_policy() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages";
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
        assert_eq!(with_uplift_shaped, no_uplift_usage);
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
    fn usage_projection_output_uplift_only_applies_above_threshold() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":800,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages";
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
        route.endpoint = "/v1/messages";
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let projection =
            projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let value: serde_json::Value =
            serde_json::from_slice(&projected.body).expect("projected json");
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
        route.endpoint = "/v1/messages";
        route.upstream_model = Some("claude-sonnet-4-5".to_string());
        route.model_resolution_source = Some("alias".to_string());
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let projection = projection_context(&route, &pool, 0).expect("projection");
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
            .expect("billing");

        assert_eq!(route.payload.model, "sonnet");
        assert_eq!(billing.pricing_model.as_deref(), Some("claude-sonnet-4-5"));
        assert!(billing.pricing_available);
    }

    #[test]
    fn usage_projection_updates_external_pool_cache_after_success() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages";
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
    fn usage_projection_ignores_external_raw_cache_when_local_policy_reads() {
        let raw_creation_body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":80000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages";
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let first_projection = projection_context(&route, &pool, 0).expect("first projection");
        let first =
            maybe_project_non_stream_usage(raw_creation_body.clone(), Some(&first_projection));
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
        route.endpoint = "/v1/messages";
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
        let projected = maybe_project_sse_event(event, projection.as_ref(), None);
        let text = String::from_utf8(projected).expect("utf8");

        assert!(text.contains("data: [DONE]"));
        assert!(text.contains("\n\n"));
        assert!(!text.contains(r#""input_tokens":100000"#));
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
        let projection = projection_context(&route, &pool, 0);
        let _projected = maybe_project_sse_event(event, projection.as_ref(), Some(&capture));
        let capture = *capture.lock();
        let raw = capture.raw.expect("raw usage");
        let reported = capture.reported.expect("reported usage");

        assert_eq!(raw.input_tokens, 100000);
        assert!(reported.input_tokens <= 96);
        assert!(reported.cache_read_input_tokens > 0);
        assert!(reported.cache_creation_input_tokens > 0);
    }

    #[test]
    fn sse_usage_projection_applies_output_uplift_to_reported_usage() {
        let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1200,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
        let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages";
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection =
            projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
        let projected = maybe_project_sse_event(event, Some(&projection), Some(&capture));
        let text = std::str::from_utf8(&projected).expect("projected sse");
        assert!(text.contains(r#""output_tokens":1800"#));

        let capture = *capture.lock();
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
}
