use super::*;
use crate::anthropic::types::{Message, Metadata, OutputConfig, SystemMessage, Thinking};
use crate::model::config::Config;
use crate::model::config::{ReportedUsageFieldPolicy, ReportedUsagePathPolicy};

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
        preserve_path: true,
        normalize_model_version_dots: false,
        model_mapping_mode: ExternalPoolModelMappingMode::ProcessedMapping,
        model_mapping_require_match: false,
        model_mapping_rules: Vec::new(),
        supported_models: Vec::new(),
        notes: None,
    }
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

    let selected = select_external_pool_candidate(vec![
        (secondary.clone(), 0),
        (tertiary.clone(), 0),
        (primary.clone(), 0),
    ])
    .expect("candidate should be selected");
    assert_eq!(selected.id, primary.id);

    let selected =
        select_external_pool_candidate(vec![(secondary.clone(), 0), (tertiary.clone(), 0)])
            .expect("fallback candidate should be selected when primary is excluded/full");
    assert_eq!(selected.id, secondary.id);

    primary.priority = 1;
    secondary.priority = 1;
    primary.max_concurrent_requests = 2;
    secondary.max_concurrent_requests = 4;
    let selected =
        select_external_pool_candidate(vec![(primary.clone(), 1), (secondary.clone(), 1)])
            .expect("lower same-priority load should be selected");
    assert_eq!(selected.id, secondary.id);
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
    let mut config = ExternalPoolsConfig::default();
    config.external_pools_enabled = true;
    config.external_pool_global_max_concurrent_requests = 0;

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
    let mut config = ExternalPoolsConfig::default();
    config.external_pools_enabled = true;
    config.external_pool_global_max_concurrent_requests = 0;

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
        .scan_pool_availability_uncached(&HashSet::new(), &config, true, None, None)
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

    let before_touch = crate::kiro::token_manager::storage_task::best_effort_storage_task_stats();
    assert!(lease.touch(), "touch task should enter the bounded lane");
    let after_touch_submission =
        crate::kiro::token_manager::storage_task::best_effort_storage_task_stats();
    assert!(
        after_touch_submission.accepted >= before_touch.accepted.saturating_add(1),
        "global counters may include parallel tests, but this touch must add an accepted task"
    );
    let touch_drain = crate::kiro::token_manager::storage_task::drain_best_effort_storage_tasks(
        Duration::from_secs(5),
    )
    .await;
    assert!(touch_drain.drained, "accepted touch task should drain");
    assert!(touch_drain.target >= after_touch_submission.accepted);
    assert!(touch_drain.finished >= touch_drain.target);
    let (pool_in_flight, global_in_flight, _, _) =
        manager.pool_runtime_snapshot(pool.id).await.unwrap();
    assert_eq!(pool_in_flight, 1);
    assert_eq!(global_in_flight, 1);

    let before_drop = crate::kiro::token_manager::storage_task::best_effort_storage_task_stats();
    drop(lease);
    let after_drop_submission =
        crate::kiro::token_manager::storage_task::best_effort_storage_task_stats();
    assert!(
        after_drop_submission.critical_accepted >= before_drop.critical_accepted.saturating_add(1),
        "lease drop must add a task to the critical lane"
    );
    let release_drain = crate::kiro::token_manager::storage_task::drain_best_effort_storage_tasks(
        Duration::from_secs(5),
    )
    .await;
    assert!(release_drain.drained, "accepted release task should drain");
    assert!(release_drain.target >= after_drop_submission.accepted);
    assert!(release_drain.finished >= release_drain.target);
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
async fn external_pool_cancelled_waiter_releases_redis_queue_lease() {
    let Some((manager, postgres)) = test_external_pool_manager().await else {
        return;
    };
    let manager = Arc::new(manager);
    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_pool_capacity_mode: ExternalPoolCapacityMode::Wait,
        external_pool_max_queued_requests: 1,
        external_pool_dispatch_max_wait_secs: 0,
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

    let decision = manager
        .handle_capacity_unavailable(
            &route,
            Vec::new(),
            &config,
            PoolCapacityWaitReason::CoordinatorUnavailable,
            None,
            &mut queue_guard,
            &mut wait_started_at,
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
            .get(HeaderName::from_static("x-kiro-rs-error-id"))
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
            .get(HeaderName::from_static("x-kiro-rs-error-id"))
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
    assert_eq!(metadata["cooldownReason"], "rate_limit");
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

#[test]
fn external_payload_guard_retry_route_trims_and_disables_second_retry() {
    let mut route = test_route("claude-sonnet-4-6");
    let mut messages = Vec::new();
    for idx in 0..32 {
        messages.push(Message {
            role: "user".to_string(),
            content: serde_json::json!(format!("history {} {}", idx, "x".repeat(700))),
        });
        messages.push(Message {
            role: "assistant".to_string(),
            content: serde_json::json!([{
                "type": "text",
                "text": format!("answer {} {}", idx, "y".repeat(500)),
            }]),
        });
    }
    messages.push(Message {
        role: "user".to_string(),
        content: serde_json::json!("current question"),
    });
    route.payload.as_mut().unwrap().messages = messages;
    let body =
        serde_json::to_string(route.payload.as_ref().unwrap()).expect("serialize route payload");
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
        Some(ExternalPoolRequestBodyMode::Normalized)
    );
    assert!(retry_route.raw_body.len() <= 8_000);
    assert!(retry_route.payload_guard_retry_config.is_none());
    assert!(
        retry_route
            .payload_guard_report
            .as_ref()
            .is_some_and(|report| report.trimmed_history_entries > 0)
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
        serde_json::json!("current question")
    );
}

#[tokio::test]
async fn external_capacity_scheduler_error_uses_request_id_and_error_type() {
    let route = ExternalRouteRequest {
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
    };

    let (error_type, message) = external_capacity_error(PoolCapacityWaitReason::Full);
    let err = external_capacity_final_error(
        StatusCode::SERVICE_UNAVAILABLE,
        error_type,
        message,
        &route.error_id,
    );
    assert!(err.is_capacity_like());
    assert_eq!(err.route_error_type, "external_pool_capacity_full");
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
    assert_eq!(err.response_body.as_deref(), Some(body.as_ref()));
    assert!(err.message.contains("success status"));
}

#[test]
fn external_stream_error_event_is_masked_and_raw_event_is_recorded() {
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
        .expect("raw stream error recorded");
    assert!(recorded.contains("raw external promo text"));
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

#[test]
fn external_pool_max_input_preflight_only_rejects_known_oversized_routes() {
    let mut config = ExternalPoolsConfig {
        external_pool_max_input_tokens: 100,
        ..ExternalPoolsConfig::default()
    };
    let mut route = test_route("claude-sonnet-4.5");

    route.request_input_tokens = 100;
    assert_eq!(
        external_pool_max_input_tokens_for_route(&config, &route),
        None
    );

    route.request_input_tokens = 101;
    assert_eq!(
        external_pool_max_input_tokens_for_route(&config, &route),
        Some(100)
    );

    route.request_input_tokens = 0;
    assert_eq!(
        external_pool_max_input_tokens_for_route(&config, &route),
        None
    );

    config.external_pool_max_input_tokens = 0;
    route.request_input_tokens = 1_500_000;
    assert_eq!(
        external_pool_max_input_tokens_for_route(&config, &route),
        None
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

fn test_external_pool_outbound_body(route: &ExternalRouteRequest, pool: &ExternalPool) -> Bytes {
    external_pool_outbound_body(route, pool).expect("build external outbound body")
}

fn payload_ref(route: &ExternalRouteRequest) -> &MessagesRequest {
    route.payload.as_ref().expect("typed test route payload")
}

fn payload_mut(route: &mut ExternalRouteRequest) -> &mut MessagesRequest {
    route.payload.as_mut().expect("typed test route payload")
}

fn test_route(model: &str) -> ExternalRouteRequest {
    let payload = test_payload(model);
    let request_input_tokens = count_external_route_input_tokens(&payload);
    ExternalRouteRequest {
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
    }
}

fn raw_test_route(raw_body: &'static [u8]) -> ExternalRouteRequest {
    let mut route = test_route("raw-placeholder");
    route.raw_body = Bytes::from_static(raw_body);
    let (model_hint, stream_hint) = raw_messages_body_hints(&route.raw_body);
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
        effort: "xhigh".to_string(),
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
        effort: "xhigh".to_string(),
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
    assert!((32..=4_096).contains(&reported.input_tokens));
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
    let mut config = ExternalPoolsConfig::default();
    config.external_pool_stream_response_mode = ExternalPoolStreamResponseMode::EventPassthrough;

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
